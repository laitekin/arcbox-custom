//! Proxy coverage e2e — each row of the egress coverage matrix
//! (`docs/logs/2026-10-21-proxy-authentication.md`) proven against a real
//! daemon whose guest egress is pinned to an authenticating SOCKS5 proxy.
//!
//! The proxy runs on the Mac and records every request it serves (user,
//! command, target). It forwards to the real destination, except
//! [`ORIGIN_IP`], a TEST-NET address it splices to a local origin so the
//! container's request needs no internet. Each scenario then asserts the
//! proxy saw the traffic *as the configured user* — a flow that left
//! directly would never appear.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use arcbox_e2e::docker::{docker_output, ensure_image};
use arcbox_e2e::net_fixtures::spawn_blob_server;
use arcbox_e2e::scenario::run_vz_scenario_with_env;

const READY_TIMEOUT: Duration = Duration::from_secs(180);
const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);
const USER: &str = "alice";
const PASSWORD: &str = "s3cret";
/// TEST-NET-3: unroutable, so reaching it at all proves the proxy carried it.
const ORIGIN_IP: &str = "203.0.113.10";

/// One request the proxy served.
#[derive(Debug, Clone)]
struct Seen {
    user: String,
    command: u8,
    target: String,
}

/// An authenticating SOCKS5 proxy (RFC 1928/1929) with CONNECT and UDP
/// ASSOCIATE, recording what it serves.
struct RecordingProxy {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl RecordingProxy {
    fn spawn(origin_port: u16) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding SOCKS5 fixture")?;
        let port = listener.local_addr()?.port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let _ = serve(client, origin_port, &log);
                });
            }
        });
        Ok(Self { port, seen })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("proxy log poisoned").clone()
    }

    fn expect(&self, what: &str, matches: impl Fn(&Seen) -> bool) -> Result<()> {
        let seen = self.seen();
        let Some(hit) = seen.iter().find(|s| matches(s)) else {
            bail!("the proxy never saw {what}; it saw {seen:?}");
        };
        if hit.user != USER {
            bail!("{what} reached the proxy as {:?}, not {USER:?}", hit.user);
        }
        Ok(())
    }
}

fn serve(mut client: TcpStream, origin_port: u16, log: &Mutex<Vec<Seen>>) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods)?;
    if !methods.contains(&2) {
        // A guest flow offering no credentials must not get through.
        client.write_all(&[5, 0xFF])?;
        return Ok(());
    }
    client.write_all(&[5, 2])?;
    let mut auth = [0u8; 2];
    client.read_exact(&mut auth)?;
    let mut user = vec![0u8; auth[1] as usize];
    client.read_exact(&mut user)?;
    let mut size = [0u8; 1];
    client.read_exact(&mut size)?;
    let mut password = vec![0u8; size[0] as usize];
    client.read_exact(&mut password)?;
    let user = String::from_utf8_lossy(&user).into_owned();
    if user != USER || password != PASSWORD.as_bytes() {
        client.write_all(&[1, 1])?;
        return Ok(());
    }
    client.write_all(&[1, 0])?;

    let mut request = [0u8; 3];
    client.read_exact(&mut request)?;
    let (host, port) = read_addr(&mut client)?;
    log.lock().expect("proxy log poisoned").push(Seen {
        user,
        command: request[1],
        target: format!("{host}:{port}"),
    });
    match request[1] {
        1 => {
            let upstream = if host == ORIGIN_IP {
                TcpStream::connect(("127.0.0.1", origin_port))
            } else {
                TcpStream::connect((host.as_str(), port))
            };
            let Ok(upstream) = upstream else {
                client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])?;
                return Ok(());
            };
            client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
            client.set_read_timeout(None)?;
            splice(client, upstream);
        }
        3 => {
            let relay = UdpSocket::bind("127.0.0.1:0")?;
            let relay_port = relay.local_addr()?.port();
            let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
            reply.extend_from_slice(&relay_port.to_be_bytes());
            client.write_all(&reply)?;
            relay_udp(&relay, log);
        }
        _ => client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?,
    }
    Ok(())
}

fn read_addr(stream: &mut TcpStream) -> std::io::Result<(String, u16)> {
    let mut atyp = [0u8; 1];
    stream.read_exact(&mut atyp)?;
    let host = match atyp[0] {
        1 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip)?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        4 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip)?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port)?;
    Ok((host, u16::from_be_bytes(port)))
}

fn splice(client: TcpStream, upstream: TcpStream) {
    let (mut client_read, mut upstream_write) =
        (client.try_clone().unwrap(), upstream.try_clone().unwrap());
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let (mut upstream_read, mut client_write) = (upstream, client);
    let _ = std::io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
}

/// Relays SOCKS5-framed datagrams to their real targets and frames the
/// replies back, logging each target.
fn relay_udp(relay: &UdpSocket, log: &Mutex<Vec<Seen>>) {
    let _ = relay.set_read_timeout(Some(Duration::from_secs(30)));
    let Ok(upstream) = UdpSocket::bind("0.0.0.0:0") else {
        return;
    };
    let _ = upstream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = vec![0u8; 65_535];
    while let Ok((n, peer)) = relay.recv_from(&mut buf) {
        // RSV RSV FRAG ATYP=1 ADDR(4) PORT(2) payload; the datapath sends
        // IP literals here.
        if n < 10 || buf[2] != 0 || buf[3] != 1 {
            continue;
        }
        let target = SocketAddr::from((
            [buf[4], buf[5], buf[6], buf[7]],
            u16::from_be_bytes([buf[8], buf[9]]),
        ));
        log.lock().expect("proxy log poisoned").push(Seen {
            user: USER.to_owned(),
            command: 0xFF,
            target: target.to_string(),
        });
        let header = buf[..10].to_vec();
        if upstream.send_to(&buf[10..n], target).is_err() {
            continue;
        }
        let mut answer = vec![0u8; 65_535];
        if let Ok((m, _)) = upstream.recv_from(&mut answer) {
            let mut framed = header;
            framed.extend_from_slice(&answer[..m]);
            let _ = relay.send_to(&framed, peer);
        }
    }
}

/// Every matrix row the System VM covers, against one daemon.
#[test]
#[ignore = "boots a VZ System VM through a real daemon; run on the e2e runner"]
fn system_vm_egress_goes_through_the_authenticated_proxy() -> Result<()> {
    let origin = spawn_blob_server(4096)?;
    let proxy = RecordingProxy::spawn(origin.port())?;
    let env = [
        (
            "ARCBOX_NETWORK_PROXY",
            format!("socks5://{USER}:{PASSWORD}@127.0.0.1:{}", proxy.port),
        ),
        ("ARCBOX_PROXY_CREDENTIALS", "file".to_owned()),
    ];
    run_vz_scenario_with_env(
        "proxy_coverage",
        "info",
        &env,
        |daemon, data_dir, metrics| {
            metrics.time("daemon_ready", || daemon.wait_ready_blocking(READY_TIMEOUT))?;
            let image =
                std::env::var("ARCBOX_E2E_IMAGE").unwrap_or_else(|_| "alpine:latest".to_owned());
            ensure_image(data_dir, &image)?;

            // Row: container TCP.
            let url = format!("http://{ORIGIN_IP}/blob");
            metrics.time("container_tcp", || {
                docker_output(
                    data_dir,
                    &["run", "--rm", &image, "wget", "-q", "-O", "/dev/null", &url],
                    DOCKER_TIMEOUT,
                )
            })?;
            proxy.expect("the container's TCP flow", |s| {
                s.command == 1 && s.target == format!("{ORIGIN_IP}:80")
            })?;

            // Row: container UDP (DNS to a public resolver, not the gateway).
            metrics.time("container_udp", || {
                docker_output(
                    data_dir,
                    &["run", "--rm", &image, "nslookup", "example.com", "1.1.1.1"],
                    DOCKER_TIMEOUT,
                )
            })?;
            proxy.expect("the container's UDP association", |s| s.command == 3)?;
            proxy.expect("the container's DNS datagram", |s| {
                s.command == 0xFF && s.target == "1.1.1.1:53"
            })?;

            // Row: docker pull (dockerd in the System VM).
            let pull = std::env::var("ARCBOX_E2E_PULL_IMAGE")
                .unwrap_or_else(|_| "busybox:latest".to_owned());
            metrics.time("docker_pull", || {
                docker_output(data_dir, &["pull", &pull], DOCKER_TIMEOUT)
            })?;
            proxy.expect("dockerd's registry connection", |s| {
                s.command == 1 && s.target.ends_with(":443")
            })?;
            Ok(())
        },
    )
}
