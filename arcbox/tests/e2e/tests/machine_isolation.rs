//! Isolated machine e2e: what an isolated machine can and cannot reach,
//! measured from inside it against a live VZ daemon.
//!
//! A saved SOCKS5 proxy (a recording fixture on the Mac) is assigned to an
//! isolated alpine machine with a time zone. Inside it: the Mac's `/Users`
//! cannot be mounted, there is one NIC and its address is the one the
//! daemon reports, a service on the Mac's loopback is out of reach through
//! the gateway, and the internet is reached only through the proxy. From
//! the Mac, a forwarded loopback port reaches a server in the machine.

use std::future::Future;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
use arcbox_vmm::VmBackend;
use tonic::transport::Channel;

const MACHINE: &str = "e2e-isolated";
/// An address the fixture proxy maps to the blob server on the Mac.
const ORIGIN_IP: &str = "203.0.113.20";
const ORIGIN_PORT: u16 = 8081;
const GUEST_ECHO_PORT: u16 = 8090;
const GUEST_UDP_PORT: u16 = 8091;
const DEFAULT_PUBLIC_IP: &str = "1.1.1.1";
const BUDGET: Duration = Duration::from_secs(480);
const RPC_BUDGET: Duration = Duration::from_secs(30);

#[path = "machine_isolation/proxy.rs"]
mod proxy;
#[path = "machine_isolation/twins.rs"]
mod twins;
use proxy::RecordingProxy;

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
    exec_in(machines, MACHINE, script).await
}

async fn exec_in(
    machines: &mut MachineServiceClient<Channel>,
    id: &str,
    script: &str,
) -> Result<(i32, String)> {
    let mut stream = tokio::time::timeout(
        RPC_BUDGET,
        machines.exec(MachineExecRequest {
            id: id.to_owned(),
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
        } else if output.stream == "stderr" {
            eprint!("guest stderr: {}", String::from_utf8_lossy(&output.data));
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
    let backend = match std::env::var("ARCBOX_ISOLATION_BACKEND")
        .as_deref()
        .unwrap_or("vz")
    {
        "vz" => VmBackend::Vz,
        "hv" => VmBackend::Hv,
        other => bail!("unsupported isolation backend {other}"),
    };
    let probe = build_guest_probe()?;
    let dns_control: Ipv4Addr = std::env::var("ARCBOX_E2E_DNS_CONTROL_IP")
        .unwrap_or_else(|_| "223.5.5.5".into())
        .parse()?;
    let public_ip: Ipv4Addr = std::env::var("ARCBOX_E2E_PUBLIC_CONTROL_IP")
        .unwrap_or_else(|_| DEFAULT_PUBLIC_IP.to_owned())
        .parse()?;
    let origin = spawn_blob_server(4096)?;
    let mut proxy = RecordingProxy::spawn(origin.port(), public_ip)?;
    let (mac_port, mac_reached) = spawn_mac_service()?;
    // Never let a signed test daemon change host-global routes or resolvers.
    let helper_dir = tempfile::tempdir()?;
    let config_home = helper_dir.path().join("config");
    std::fs::create_dir_all(config_home.join("arcbox"))?;
    std::fs::write(
        config_home.join("arcbox/config.toml"),
        "[vm]\ncpus = 2\nmemory_mb = 2048\n[network.proxy_library]\nprofiles = []\ndefault_profile = ''\nmachines = {}\n",
    )?;
    let env = [
        (
            "ARCBOX_HELPER_SOCKET",
            helper_dir
                .path()
                .join("absent.sock")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "XDG_CONFIG_HOME",
            config_home.to_string_lossy().into_owned(),
        ),
    ];
    run_vz_scenario_with_env(
        "machine_isolation",
        "info",
        &env,
        |daemon, data_dir, metrics| {
            // Hypervisor.framework allows one HV VM per process. Keep the
            // System VM on VZ; backend identifies the isolated machine under test.
            metrics.backend = Some(backend.as_str().to_owned());
            println!("BACKEND system=vz isolated_machine={}", backend.as_str());
            std::fs::write(data_dir.join("host-only-sentinel"), b"must never be shared")?;
            let ready = Instant::now();
            daemon.wait_ready_blocking(Duration::from_secs(180))?;
            metrics.record("daemon_ready", ready.elapsed().as_secs_f64());
            let socket = daemon.grpc_socket();
            run_with_budget(BUDGET, async {
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
                            exclude: vec![".invalid".into()],
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
                let udp_port = UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port();
                machines
                    .set_port_forwards(SetMachinePortForwardsRequest {
                        id: MACHINE.to_owned(),
                        forwards: vec![
                            MachinePortForward {
                                host_port: u32::from(host_port),
                                guest_port: u32::from(GUEST_ECHO_PORT),
                                protocol: "tcp".to_owned(),
                            },
                            MachinePortForward {
                                host_port: u32::from(udp_port),
                                guest_port: u32::from(GUEST_UDP_PORT),
                                protocol: "udp".into(),
                            },
                        ],
                    })
                    .await
                    .context("forwarding a port")?;
                std::fs::copy(
                    &probe,
                    data_dir
                        .join("machines")
                        .join(MACHINE)
                        .join("guest-share/network-probe"),
                )?;
                if backend == VmBackend::Hv {
                    // Create currently defaults to VZ. Change only this stopped
                    // test machine's persisted backend while its daemon is down.
                    tokio::task::block_in_place(|| {
                        daemon.restart_with(|dir| {
                            let path = dir.join("machines").join(MACHINE).join("config.toml");
                            let mut document = std::fs::read_to_string(&path)?
                                .parse::<toml_edit::DocumentMut>()?;
                            document["backend"] = toml_edit::value("hv");
                            std::fs::write(path, document.to_string())?;
                            Ok(())
                        })
                    })?;
                    daemon.wait_ready(Duration::from_secs(180)).await?;
                    let channel = connect_unix(&socket).await?;
                    machines = MachineServiceClient::new(channel.clone());
                    library = ProxyLibraryServiceClient::new(channel);
                }
                machines
                    .start(StartMachineRequest {
                        id: MACHINE.to_owned(),
                    })
                    .await
                    .context("starting the isolated machine")?;

                // A usable mount command is a precondition, not evidence of isolation.
                let (code, _) = exec(&mut machines, "command -v mount && mkdir -p /mnt/u").await?;
                if code != 0 {
                    bail!("cannot run the file-isolation probe");
                }
                for tag in ["users", "private"] {
                    let (code, _) =
                        exec(&mut machines, &format!("mount -t virtiofs {tag} /mnt/u")).await?;
                    if code == 0 {
                        bail!("the Mac's {tag} share could be mounted");
                    }
                }
                let (code, _) = exec(&mut machines,
                "mkdir -p /mnt/own && mount -t virtiofs arcbox /mnt/own && test ! -e /mnt/own/host-only-sentinel && test ! -d /mnt/own/machines && test -x /mnt/own/bin/arcbox-agent && ! touch /mnt/own/guest-write-probe"
            ).await?;
                if code != 0 {
                    bail!("the arcbox share exposed the data root or could not be checked");
                }
                exec(&mut machines, "umount /mnt/own").await?;
                println!(
                    "PASS file isolation: users/private unavailable; private agent share is read-only and excludes daemon data"
                );

                // One NIC, with the address the daemon reports.
                // dummy0 and sit0 are software interfaces, not additional
                // hypervisor network attachments or paths to the Mac.
                let (_, nics) = exec(&mut machines,
                    "for iface in /sys/class/net/*; do if [ -e \"$iface/device\" ]; then basename \"$iface\"; fi; done"
                ).await?;
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
                let (_, guest_mac) = exec(
                    &mut machines,
                    &format!("cat /sys/class/net/{}/address", nics.trim()),
                )
                .await?;
                if mac.is_empty() || guest_mac != mac || mac.starts_with("02:") {
                    bail!("NIC address {guest_mac:?}, daemon reports {mac:?}");
                }
                let (_, id) = exec(&mut machines, "cat /etc/machine-id").await?;
                let record = std::fs::read_to_string(
                    data_dir.join("machines").join(MACHINE).join("config.toml"),
                )?
                .parse::<toml_edit::DocumentMut>()?;
                let expected_id = record
                    .get("machine_id")
                    .and_then(toml_edit::Item::as_str)
                    .context("persisted machine ID is missing")?;
                if id.len() != 32 || id != expected_id {
                    bail!("guest machine ID {id:?} differs from assigned {expected_id:?}");
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

                println!(
                    "PASS strict isolation: one NIC, independent identity, live Mac service unreachable"
                );

                // The internet, only through the proxy.
                let (code, _) = exec(
                    &mut machines,
                    &format!("wget -T 10 -O /dev/null http://{ORIGIN_IP}:{ORIGIN_PORT}/blob"),
                )
                .await?;
                if code != 0 || !proxy.saw(&format!("{ORIGIN_IP}:{ORIGIN_PORT}")) {
                    let (_, routes) = exec(&mut machines, "ip addr; ip route").await?;
                    eprintln!(
                        "guest network: {routes}; proxy targets: {:?}",
                        proxy.targets.lock().unwrap()
                    );
                    bail!("the fetch did not go through the proxy (exit {code})");
                }

                let phase = backend.as_str();
                host_dns_control(dns_control, &dns_name(phase, "capture-control-before"))?;
                let dns_up = dns_name(phase, "gateway-up");
                assert_guest_dns(&mut machines, "10.0.2.1", &dns_up, true).await?;
                anyhow::ensure!(
                    proxy.saw(&format!("dns-tcp:{dns_up}")),
                    "gateway DNS bypassed SOCKS CONNECT"
                );
                let udp_dns_up = dns_name(phase, "udp-up");
                assert_guest_dns(&mut machines, &dns_control.to_string(), &udp_dns_up, true)
                    .await?;
                anyhow::ensure!(
                    proxy.saw(&format!("dns-udp:{udp_dns_up}")),
                    "direct DNS did not use SOCKS UDP"
                );
                assert_guest_udp(&mut machines, true).await?;
                anyhow::ensure!(
                    proxy.saw("udp:203.0.113.30:53535"),
                    "UDP payload bypassed proxy"
                );
                println!(
                    "PASS DNS via SOCKS TCP and explicit resolver DNS via SOCKS UDP; bypass list ignored"
                );
                library
                    .save_profile(SaveProxyProfileRequest {
                        id: profile.id.clone(),
                        name: "e2e-isolation".into(),
                        settings: Some(EgressProxySettings {
                            mode: EgressProxyMode::Custom.into(),
                            url: format!("socks5://127.0.0.1:{}", proxy.port),
                            udp: EgressUdpPolicy::Direct.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .await?;
                assert_guest_dns(
                    &mut machines,
                    &dns_control.to_string(),
                    &dns_name(phase, "udp-direct-blocked"),
                    false,
                )
                .await?;
                println!("PASS isolated UDP=direct policy is blocked instead of going direct");
                library
                    .save_profile(SaveProxyProfileRequest {
                        id: profile.id.clone(),
                        name: "e2e-isolation".into(),
                        settings: Some(EgressProxySettings {
                            mode: EgressProxyMode::Custom.into(),
                            url: format!("socks5://127.0.0.1:{}", proxy.port),
                            udp: EgressUdpPolicy::Proxy.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .await?;

                // Check a routable public target, not only TEST-NET (which could
                // fail even if a buggy implementation fell back to direct).
                let public_probe = format!("/arcbox/network-probe tcp-http {public_ip} 80");
                let (_, response) = exec(&mut machines, &public_probe).await?;
                if !response.contains("HTTP/") || !proxy.saw(&format!("{public_ip}:80")) {
                    bail!(
                        "public-target probe did not pass through the proxy: response={response:?}, targets={:?}",
                        proxy.targets.lock().unwrap()
                    );
                }
                // The host can reach this address directly, so the negative guest
                // result cannot be explained by an unreachable control target.
                direct_http_control(public_ip).context("public direct-egress positive control")?;
                proxy.stop();
                if TcpStream::connect(("127.0.0.1", proxy.port)).is_ok() {
                    bail!("the fixture proxy is still listening");
                }
                let (code, response) = exec(&mut machines, &public_probe).await?;
                if code == 127 || !response.is_empty() {
                    bail!("proxy outage did not fail closed: code={code}, response={response:?}");
                }
                direct_http_control(public_ip)
                    .context("public direct-egress control after outage")?;
                assert_guest_dns(
                    &mut machines,
                    "10.0.2.1",
                    &dns_name(phase, "gateway-down"),
                    false,
                )
                .await?;
                assert_guest_dns(
                    &mut machines,
                    &dns_control.to_string(),
                    &dns_name(phase, "udp-down"),
                    false,
                )
                .await?;
                assert_guest_udp(&mut machines, false).await?;
                host_dns_control(dns_control, &dns_name(phase, "capture-control-after"))?;
                println!(
                    "PASS proxy outage: TCP, UDP and gateway DNS all fail closed while direct host controls remain reachable"
                );

                let recovered = RecordingProxy::spawn(origin.port(), public_ip)?;
                library
                    .save_profile(SaveProxyProfileRequest {
                        id: profile.id.clone(),
                        name: "e2e-isolation".to_owned(),
                        settings: Some(EgressProxySettings {
                            mode: EgressProxyMode::Custom.into(),
                            url: format!("socks5://127.0.0.1:{}", recovered.port),
                            udp: EgressUdpPolicy::Proxy.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .await
                    .context("restoring the proxy")?;
                let (_, response) = exec(&mut machines, &public_probe).await?;
                if !response.contains("HTTP/") || !recovered.saw(&format!("{public_ip}:80")) {
                    bail!("the running machine did not recover after restoring its proxy");
                }
                assert_guest_udp(&mut machines, true).await?;
                let dns_recovered = dns_name(phase, "gateway-recovered");
                assert_guest_dns(&mut machines, "10.0.2.1", &dns_recovered, true).await?;
                anyhow::ensure!(
                    recovered.saw(&format!("dns-tcp:{dns_recovered}")),
                    "recovered DNS missed proxy"
                );
                println!("PASS TCP, UDP and DNS recovery without restarting the machine");

                // Alpine's BusyBox need not include httpd; nc is already
                // required by the outbound probe. Serve a real guest TCP echo.
                let (code, _) = exec(&mut machines, &format!(
                    "nc -h 2>&1 | grep -q -- '-e' || exit 125; (while nc -l -p {GUEST_ECHO_PORT} -e /bin/cat; do :; done) </dev/null >/tmp/arcbox-e2e-echo.log 2>&1 &"
                )).await?;
                if code != 0 {
                    bail!("guest nc cannot serve the TCP echo fixture: {code}");
                }
                let (code, _) = exec(&mut machines,
                    "i=0; until awk '$2 ~ /:1F9A$/ && $4 == \"0A\" { found=1 } END { exit !found }' /proc/net/tcp /proc/net/tcp6; do i=$((i+1)); [ $i -lt 30 ] || { cat /tmp/arcbox-e2e-echo.log; exit 1; }; sleep 0.1; done"
                ).await?;
                if code != 0 {
                    bail!("guest echo listener did not start");
                }
                let (code, _) = exec(&mut machines, &format!(
                    "/arcbox/network-probe serve-udp {GUEST_UDP_PORT} </dev/null >/tmp/arcbox-e2e-udp.log 2>&1 &"
                )).await?;
                anyhow::ensure!(code == 0, "UDP echo could not be launched");
                let (code, _) = exec(&mut machines, "i=0; until grep -q READY /tmp/arcbox-e2e-udp.log; do i=$((i+1)); [ $i -lt 30 ] || exit 1; sleep 0.1; done").await?;
                anyhow::ensure!(code == 0, "UDP echo did not bind");
                // Hold live forwards beyond Docker's 30s reconcile cycle.
                // Machine owners must never be retired as absent containers.
                tokio::time::sleep(Duration::from_secs(35)).await;
                assert_forward_udp(udp_port)?;
                assert_forward_echo(host_port)?;
                println!("PASS loopback TCP and UDP forwarding survives Docker reconciliation");
                twins::verify(
                    &mut machines,
                    &mut library,
                    &profile.id,
                    data_dir,
                    &probe,
                    host_port,
                    udp_port,
                )
                .await?;
                machines
                    .set_port_forwards(SetMachinePortForwardsRequest {
                        id: MACHINE.to_owned(),
                        forwards: vec![],
                    })
                    .await?;
                if TcpStream::connect(("127.0.0.1", host_port)).is_ok() {
                    bail!("removed forwarded port still accepts connections");
                }
                drop(
                    UdpSocket::bind(("127.0.0.1", udp_port))
                        .context("removed UDP port was not released")?,
                );
                machines
                    .set_port_forwards(SetMachinePortForwardsRequest {
                        id: MACHINE.to_owned(),
                        forwards: vec![
                            MachinePortForward {
                                host_port: u32::from(host_port),
                                guest_port: u32::from(GUEST_ECHO_PORT),
                                protocol: "tcp".to_owned(),
                            },
                            MachinePortForward {
                                host_port: u32::from(udp_port),
                                guest_port: u32::from(GUEST_UDP_PORT),
                                protocol: "udp".into(),
                            },
                        ],
                    })
                    .await?;
                assert_forward_echo(host_port).context("restored forward")?;
                assert_forward_udp(udp_port).context("restored UDP forward")?;
                println!("PASS dynamic removal and restoration of forwarded port");
                machines
                    .stop(StopMachineRequest {
                        id: MACHINE.to_owned(),
                        force: true,
                    })
                    .await?;
                if TcpStream::connect(("127.0.0.1", host_port)).is_ok() {
                    bail!("stopped machine still owns its forwarded port");
                }
                drop(
                    UdpSocket::bind(("127.0.0.1", udp_port))
                        .context("stopped UDP port was not released")?,
                );
                println!("PASS stop releases TCP and UDP forwarded ports");
                machines
                    .remove(RemoveMachineRequest {
                        id: MACHINE.to_owned(),
                        force: true,
                        volumes: false,
                    })
                    .await?;
                Ok(())
            })
        },
    )
}

/// Construct the timer only after entering the runtime; constructing it as
/// the argument to block_on panics before any scenario can run.
fn run_with_budget(budget: Duration, scenario: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        tokio::time::timeout(budget, scenario)
            .await
            .context("the isolation scenario timed out")?
    })
}

#[test]
fn scenario_budget_runs_and_reports_timeout_without_a_reactor_panic() -> Result<()> {
    run_with_budget(Duration::from_secs(1), async { Ok(()) })?;
    let error = run_with_budget(Duration::from_millis(10), std::future::pending()).unwrap_err();
    assert!(error.to_string().contains("timed out"));
    Ok(())
}

#[test]
fn recording_proxy_accepts_a_blocking_handshake_and_can_be_stopped() -> Result<()> {
    let origin = spawn_blob_server(16)?;
    let mut proxy = RecordingProxy::spawn(origin.port(), DEFAULT_PUBLIC_IP.parse()?)?;
    let mut client = TcpStream::connect(("127.0.0.1", proxy.port))?;
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    client.write_all(&[5, 1, 0])?;
    let mut greeting = [0; 2];
    client.read_exact(&mut greeting)?;
    assert_eq!(greeting, [5, 0]);
    let mut request = vec![5, 1, 0, 1, 203, 0, 113, 20];
    request.extend_from_slice(&ORIGIN_PORT.to_be_bytes());
    client.write_all(&request)?;
    let mut reply = [0; 10];
    client.read_exact(&mut reply)?;
    assert_eq!(&reply[..2], &[5, 0]);
    assert!(proxy.saw(&format!("{ORIGIN_IP}:{ORIGIN_PORT}")));
    drop(client);
    proxy.stop();
    assert!(TcpStream::connect(("127.0.0.1", proxy.port)).is_err());
    Ok(())
}

fn direct_http_control(ip: Ipv4Addr) -> Result<()> {
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from((ip, 80)),
        Duration::from_secs(5),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(stream, "GET / HTTP/1.0\r\nHost: {ip}\r\n\r\n")?;
    let mut header = [0; 5];
    stream.read_exact(&mut header)?;
    anyhow::ensure!(&header == b"HTTP/", "direct control did not answer HTTP");
    Ok(())
}

fn assert_forward_echo(port: u16) -> Result<()> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).context("forwarded port")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let expected = b"arcbox-isolation-echo";
    stream.write_all(expected)?;
    let mut received = [0; 21];
    stream.read_exact(&mut received[..expected.len()])?;
    anyhow::ensure!(
        &received[..expected.len()] == expected,
        "forwarded echo mismatch"
    );
    Ok(())
}

fn build_guest_probe() -> Result<std::path::PathBuf> {
    let root = arcbox_e2e::repo_root();
    let output = root.join("target/e2e-fixtures/network-probe");
    std::fs::create_dir_all(output.parent().unwrap())?;
    let status = std::process::Command::new("rustc")
        .args([
            "--edition=2024",
            "--target",
            "aarch64-unknown-linux-musl",
            "-C",
            "linker=aarch64-linux-musl-gcc",
        ])
        .arg(root.join("tests/e2e/fixtures/network_probe.rs"))
        .arg("-o")
        .arg(&output)
        .status()?;
    anyhow::ensure!(status.success(), "could not cross-compile network probe");
    Ok(output)
}

fn dns_name(backend: &str, phase: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("abxleak-{backend}-{phase}-{nonce:x}.invalid")
}

async fn assert_guest_dns(
    machines: &mut MachineServiceClient<Channel>,
    server: &str,
    name: &str,
    expected: bool,
) -> Result<()> {
    let (code, output) = exec(
        machines,
        &format!("/arcbox/network-probe dns {server} 53 {name}"),
    )
    .await?;
    println!("DNS PROBE {name} server={server} code={code}");
    if expected {
        anyhow::ensure!(
            code == 0 && output.ends_with("c633644d"),
            "DNS proxy marker absent: {code} {output}"
        );
    } else {
        // A gateway may explicitly refuse resolution with SERVFAIL instead
        // of silently dropping the query. It must not contain an answer.
        let refused = output.strip_prefix("DNS ").is_some_and(|hex| {
            hex.len() >= 24
                && u8::from_str_radix(&hex[6..8], 16).is_ok_and(|flags| flags & 15 == 2)
                && &hex[12..16] == "0000"
        });
        anyhow::ensure!(
            (code == 2 && output.is_empty()) || (code == 0 && refused),
            "DNS leaked or probe failed unexpectedly: {code} {output}"
        );
    }
    Ok(())
}

async fn assert_guest_udp(
    machines: &mut MachineServiceClient<Channel>,
    expected: bool,
) -> Result<()> {
    let (code, output) = exec(
        machines,
        "/arcbox/network-probe udp 203.0.113.30 53535 abx-udp-proxy",
    )
    .await?;
    if expected {
        anyhow::ensure!(
            code == 0 && output == "abx-udp-proxy",
            "UDP proxy echo absent: {code} {output}"
        );
    } else {
        anyhow::ensure!(
            code == 2 && output.is_empty(),
            "UDP did not fail closed: {code} {output}"
        );
    }
    Ok(())
}

fn assert_forward_udp(port: u16) -> Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.connect(("127.0.0.1", port))?;
    socket.send(b"abx-udp-forward")?;
    let mut answer = [0; 128];
    let length = socket.recv(&mut answer)?;
    anyhow::ensure!(
        &answer[..length] == b"abx-udp-forward",
        "UDP forward mismatch"
    );
    Ok(())
}

fn host_dns_control(server: Ipv4Addr, name: &str) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.connect((server, 53))?;
    let mut query = vec![0x41, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    socket.send(&query)?;
    let mut reply = [0; 4096];
    let length = socket.recv(&mut reply)?;
    anyhow::ensure!(
        length >= 12 && reply[..2] == query[..2] && reply[2] & 0x80 != 0,
        "host DNS control invalid"
    );
    println!("DNS CAPTURE CONTROL {name} server={server}:53 answered");
    Ok(())
}
