//! Isolated machine e2e: what an isolated machine can and cannot reach,
//! measured from inside it against a live VZ daemon.
//!
//! A saved SOCKS5 proxy (a recording fixture on the Mac) is assigned to an
//! isolated alpine machine with a time zone. Inside it: the Mac's `/Users`
//! cannot be mounted, there is one NIC and its address is the one the
//! daemon reports, a service on the Mac's loopback is out of reach through
//! the gateway, and the internet is reached only through the proxy. From
//! the Mac, a forwarded loopback port reaches a server in the machine.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use arcbox_e2e::daemon::connect_unix;
use arcbox_e2e::net_fixtures::spawn_blob_server;
use arcbox_e2e::scenario::run_vz_scenario_with_env;
use arcbox_grpc::v1::machine_service_client::MachineServiceClient;
use arcbox_grpc::v1::proxy_library_service_client::ProxyLibraryServiceClient;
use arcbox_protocol::v1::{
    CreateMachineRequest, EgressProxyMode, EgressProxySettings, EgressUdpPolicy,
    InspectMachineRequest, MachineExecRequest, MachinePortForward, RemoveMachineRequest,
    SaveProxyProfileRequest, SetMachinePortForwardsRequest, SetMachineProxyRequest,
    StartMachineRequest, StopMachineRequest,
};
use tonic::transport::Channel;

const MACHINE: &str = "e2e-isolated";
/// An address the fixture proxy maps to the blob server on the Mac.
const ORIGIN_IP: &str = "203.0.113.20";
const ORIGIN_PORT: u16 = 8081;
const GUEST_HTTP_PORT: u16 = 8090;
const BUDGET: Duration = Duration::from_secs(240);
const RPC_BUDGET: Duration = Duration::from_secs(30);

/// A SOCKS5 proxy without authentication that records CONNECT targets.
struct RecordingProxy {
    port: u16,
    targets: Arc<Mutex<Vec<String>>>,
}

impl RecordingProxy {
    fn spawn(origin_port: u16) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding SOCKS5 fixture")?;
        let port = listener.local_addr()?.port();
        let targets: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&targets);
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let _ = serve(client, origin_port, &log);
                });
            }
        });
        Ok(Self { port, targets })
    }

    fn saw(&self, target: &str) -> bool {
        self.targets
            .lock()
            .expect("proxy log poisoned")
            .iter()
            .any(|seen| seen == target)
    }
}

fn serve(mut client: TcpStream, origin_port: u16, log: &Mutex<Vec<String>>) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    let mut methods = vec![0u8; usize::from(head[1])];
    client.read_exact(&mut methods)?;
    client.write_all(&[5, 0])?;
    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip)?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len)?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    log.lock()
        .expect("proxy log poisoned")
        .push(format!("{host}:{port}"));
    if request[1] != 1 {
        client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Ok(());
    }
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
    let (mut from_client, mut to_upstream) = (client.try_clone()?, upstream.try_clone()?);
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_upstream);
        let _ = to_upstream.shutdown(std::net::Shutdown::Write);
    });
    let (mut from_upstream, mut to_client) = (upstream, client);
    let _ = std::io::copy(&mut from_upstream, &mut to_client);
    let _ = to_client.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
    Ok(())
}

/// A listener on the Mac's loopback that records whether anyone connected.
fn spawn_mac_service() -> Result<(u16, Arc<AtomicBool>)> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding the Mac service")?;
    let port = listener.local_addr()?.port();
    let reached = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&reached);
    std::thread::spawn(move || {
        for _ in listener.incoming().flatten() {
            flag.store(true, Ordering::SeqCst);
        }
    });
    Ok((port, reached))
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Runs `sh -c <script>` in the machine; returns (exit code, stdout).
async fn exec(machines: &mut MachineServiceClient<Channel>, script: &str) -> Result<(i32, String)> {
    let mut stream = tokio::time::timeout(
        RPC_BUDGET,
        machines.exec(MachineExecRequest {
            id: MACHINE.to_owned(),
            cmd: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            ..Default::default()
        }),
    )
    .await
    .context("exec timed out")?
    .context("exec failed")?
    .into_inner();
    let mut stdout = Vec::new();
    let mut exit_code = -1;
    while let Some(output) = tokio::time::timeout(RPC_BUDGET, stream.message())
        .await
        .context("exec output timed out")?
        .context("exec stream error")?
    {
        if output.stream == "stdout" {
            stdout.extend_from_slice(&output.data);
        }
        if output.done {
            exit_code = output.exit_code;
        }
    }
    Ok((
        exit_code,
        String::from_utf8_lossy(&stdout).trim().to_owned(),
    ))
}

#[test]
#[ignore = "boots a VZ machine through a real daemon; run on the e2e runner"]
fn an_isolated_machine_reaches_only_its_proxy() -> Result<()> {
    let origin = spawn_blob_server(4096)?;
    let proxy = RecordingProxy::spawn(origin.port())?;
    let (mac_port, mac_reached) = spawn_mac_service()?;
    run_vz_scenario_with_env("machine_isolation", "info", &[], |daemon, _, _| {
        let socket = daemon.grpc_socket();
        tokio::runtime::Runtime::new()?.block_on(tokio::time::timeout(BUDGET, async {
            let channel = connect_unix(&socket).await?;
            let mut machines = MachineServiceClient::new(channel.clone());
            let mut library = ProxyLibraryServiceClient::new(channel);

            let profile = library
                .save_profile(SaveProxyProfileRequest {
                    name: "e2e-isolation".to_owned(),
                    settings: Some(EgressProxySettings {
                        mode: EgressProxyMode::Custom.into(),
                        url: format!("socks5://127.0.0.1:{}", proxy.port),
                        udp: EgressUdpPolicy::Proxy.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .await
                .context("saving the proxy")?
                .into_inner();
            machines
                .create(CreateMachineRequest {
                    name: MACHINE.to_owned(),
                    cpus: 1,
                    memory: 1024 * 1024 * 1024,
                    disk_size: 2 * 1024 * 1024 * 1024,
                    distro: "alpine".to_owned(),
                    version: "3.24".to_owned(),
                    isolated: true,
                    timezone: "Asia/Tokyo".to_owned(),
                    ..Default::default()
                })
                .await
                .context("creating the isolated machine")?;
            let refused = library
                .set_machine_proxy(SetMachineProxyRequest {
                    machine: MACHINE.to_owned(),
                    choice: "direct".to_owned(),
                })
                .await;
            if refused.is_ok() {
                bail!("an isolated machine was allowed to go direct");
            }
            library
                .set_machine_proxy(SetMachineProxyRequest {
                    machine: MACHINE.to_owned(),
                    choice: profile.id.clone(),
                })
                .await
                .context("assigning the saved proxy")?;
            let host_port = free_port()?;
            machines
                .set_port_forwards(SetMachinePortForwardsRequest {
                    id: MACHINE.to_owned(),
                    forwards: vec![MachinePortForward {
                        host_port: u32::from(host_port),
                        guest_port: u32::from(GUEST_HTTP_PORT),
                        protocol: "tcp".to_owned(),
                    }],
                })
                .await
                .context("forwarding a port")?;
            machines
                .start(StartMachineRequest {
                    id: MACHINE.to_owned(),
                })
                .await
                .context("starting the isolated machine")?;

            // Files: the Mac's /Users is not shared.
            let (code, _) = exec(&mut machines, "mkdir -p /mnt/u && mount -t virtiofs users /mnt/u").await?;
            if code == 0 {
                bail!("the Mac's /Users could be mounted");
            }

            // One NIC, with the address the daemon reports.
            let (_, nics) = exec(&mut machines, "ls /sys/class/net | grep -v '^lo$'").await?;
            if nics.lines().count() != 1 {
                bail!("expected one NIC, found {nics:?}");
            }
            let info = machines
                .inspect(InspectMachineRequest {
                    id: MACHINE.to_owned(),
                })
                .await?
                .into_inner();
            let mac = info.network.unwrap_or_default().mac_address;
            let (_, guest_mac) = exec(&mut machines, &format!("cat /sys/class/net/{}/address", nics.trim())).await?;
            if mac.is_empty() || guest_mac != mac || mac.starts_with("02:") {
                bail!("NIC address {guest_mac:?}, daemon reports {mac:?}");
            }
            let (_, id) = exec(&mut machines, "cat /etc/machine-id").await?;
            if id.len() != 32 {
                bail!("machine ID {id:?}");
            }
            let (_, zone) = exec(
                &mut machines,
                "[ -e /usr/share/zoneinfo/Asia/Tokyo ] && readlink /etc/localtime || echo no-tzdata",
            )
            .await?;
            if zone != "no-tzdata" && !zone.ends_with("Asia/Tokyo") {
                bail!("/etc/localtime points to {zone:?}");
            }

            // The Mac's loopback service is out of reach through the gateway.
            let (code, _) = exec(
                &mut machines,
                &format!("wget -q -T 5 -O /dev/null http://10.0.2.1:{mac_port}/"),
            )
            .await?;
            if code == 0 || mac_reached.load(Ordering::SeqCst) {
                bail!("the isolated machine reached a service on the Mac");
            }

            // The internet, only through the proxy.
            let (code, _) = exec(
                &mut machines,
                &format!("wget -q -T 10 -O /dev/null http://{ORIGIN_IP}:{ORIGIN_PORT}/blob"),
            )
            .await?;
            if code != 0 || !proxy.saw(&format!("{ORIGIN_IP}:{ORIGIN_PORT}")) {
                bail!("the fetch did not go through the proxy (exit {code})");
            }

            // The Mac reaches a server in the machine through the forward.
            exec(
                &mut machines,
                &format!(
                    "mkdir -p /tmp/www && echo isolated > /tmp/www/index.html && \
                     busybox httpd -p {GUEST_HTTP_PORT} -h /tmp/www"
                ),
            )
            .await?;
            let mut body = String::new();
            let mut stream = TcpStream::connect(("127.0.0.1", host_port)).context("forwarded port")?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.write_all(b"GET /index.html HTTP/1.0\r\n\r\n")?;
            stream.read_to_string(&mut body)?;
            if !body.contains("isolated") {
                bail!("the forwarded port answered {body:?}");
            }

            machines
                .stop(StopMachineRequest {
                    id: MACHINE.to_owned(),
                    force: true,
                })
                .await?;
            machines
                .remove(RemoveMachineRequest {
                    id: MACHINE.to_owned(),
                    force: true,
                    volumes: false,
                })
                .await?;
            Ok(())
        }))
        .context("the isolation scenario timed out")?
    })
}
