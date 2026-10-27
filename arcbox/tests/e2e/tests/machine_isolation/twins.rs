//! Both targets must answer through host forwards before peer access is
//! denied; an absent service must never count as isolation evidence.
use super::*;
use std::path::Path;

const SECOND: &str = "e2e-isolated-peer";
const TCP: u16 = 8092;
const UDP: u16 = 8093;

pub async fn verify(
    machines: &mut MachineServiceClient<Channel>,
    library: &mut ProxyLibraryServiceClient<Channel>,
    profile: &str,
    data_dir: &Path,
    probe: &Path,
    first_tcp: u16,
    first_udp: u16,
) -> Result<()> {
    machines
        .create(CreateMachineRequest {
            name: SECOND.into(),
            cpus: 1,
            memory: 512 * 1024 * 1024,
            disk_size: 2 * 1024 * 1024 * 1024,
            distro: "alpine".into(),
            version: "3.24".into(),
            isolated: true,
            ..Default::default()
        })
        .await?;
    library
        .set_machine_proxy(SetMachineProxyRequest {
            machine: SECOND.into(),
            choice: profile.into(),
        })
        .await?;
    std::fs::copy(
        probe,
        data_dir
            .join("machines")
            .join(SECOND)
            .join("guest-share/network-probe"),
    )?;
    let tcp = free_port()?;
    let udp = UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port();
    machines
        .set_port_forwards(SetMachinePortForwardsRequest {
            id: SECOND.into(),
            forwards: vec![
                MachinePortForward {
                    host_port: tcp.into(),
                    guest_port: TCP.into(),
                    protocol: "tcp".into(),
                },
                MachinePortForward {
                    host_port: udp.into(),
                    guest_port: UDP.into(),
                    protocol: "udp".into(),
                },
            ],
        })
        .await?;
    machines
        .start(StartMachineRequest { id: SECOND.into() })
        .await?;
    let (code, _) = exec_in(machines, SECOND, &format!(
        "(while nc -l -p {TCP} -e /bin/cat; do :; done) </dev/null >/tmp/tcp.log 2>&1 & /arcbox/network-probe serve-udp {UDP} </dev/null >/tmp/udp.log 2>&1 &"
    )).await?;
    anyhow::ensure!(code == 0, "second machine's listeners did not launch");
    let (code, _) = exec_in(machines, SECOND, "i=0; until grep -q READY /tmp/udp.log && awk '$2 ~ /:1F9C$/ && $4 == \"0A\" { f=1 } END { exit !f }' /proc/net/tcp /proc/net/tcp6; do i=$((i+1)); [ $i -lt 50 ] || exit 1; sleep 0.1; done").await?;
    anyhow::ensure!(code == 0, "second machine's listeners did not bind");
    assert_forward_echo(tcp)?;
    assert_forward_udp(udp)?;
    assert_forward_echo(first_tcp)?;
    assert_forward_udp(first_udp)?;
    let first = machines
        .inspect(InspectMachineRequest { id: MACHINE.into() })
        .await?
        .into_inner();
    let second = machines
        .inspect(InspectMachineRequest { id: SECOND.into() })
        .await?
        .into_inner();
    let first_net = first.network.context("first network missing")?;
    let second_net = second.network.context("second network missing")?;
    anyhow::ensure!(
        first_net.mac_address != second_net.mac_address,
        "machines share MAC identity"
    );
    let (_, first_id) = exec_in(machines, MACHINE, "cat /etc/machine-id").await?;
    let (_, second_id) = exec_in(machines, SECOND, "cat /etc/machine-id").await?;
    anyhow::ensure!(
        first_id.len() == 32 && second_id.len() == 32 && first_id != second_id,
        "machine IDs are not independent"
    );
    for (from, target, port, udp_port, host_tcp, host_udp) in [
        (MACHINE, second_net.ip_address, TCP, UDP, tcp, udp),
        (
            SECOND,
            first_net.ip_address,
            GUEST_ECHO_PORT,
            GUEST_UDP_PORT,
            first_tcp,
            first_udp,
        ),
    ] {
        for (address, tcp_port, udp_port) in [
            (target.as_str(), port, udp_port),
            ("10.0.2.1", host_tcp, host_udp),
        ] {
            let (code, output) = exec_in(
                machines,
                from,
                &format!("printf peer-check | nc -w 2 {address} {tcp_port}"),
            )
            .await?;
            anyhow::ensure!(
                code != 0 && output.is_empty(),
                "{from} reached peer TCP at {address}:{tcp_port}"
            );
            let (code, output) = exec_in(
                machines,
                from,
                &format!("/arcbox/network-probe udp {address} {udp_port} peer-check"),
            )
            .await?;
            anyhow::ensure!(
                code == 2 && output.is_empty(),
                "{from}: unexpected peer UDP result {code}: {output}"
            );
        }
    }
    // Re-check the positive controls after the negative probes.
    assert_forward_echo(tcp)?;
    assert_forward_udp(udp)?;
    assert_forward_echo(first_tcp)?;
    assert_forward_udp(first_udp)?;
    machines
        .stop(StopMachineRequest {
            id: SECOND.into(),
            force: true,
        })
        .await?;
    machines
        .remove(RemoveMachineRequest {
            id: SECOND.into(),
            force: true,
            volumes: false,
        })
        .await?;
    println!(
        "PASS two running isolated machines: distinct identities, bidirectional TCP/UDP denial, live host-forward controls"
    );
    Ok(())
}
