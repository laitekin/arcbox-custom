//! Ports forwarded from the Mac's loopback into a machine.
//!
//! They ride the same inbound relay as Docker's published ports, keyed by
//! an owner per machine, and are bound while the machine runs: on start, on
//! a change, and released on stop and removal.

use std::sync::Arc;

use arcbox_connect::v1 as pb;
use arcbox_core::Runtime;
use arcbox_core::machine::MachinePortForward;
use connectrpc::ConnectError;

/// The relay owner of a machine's forwards.
fn owner(machine: &str) -> String {
    format!("machine-forward:{machine}")
}

/// The wire forwards as the engine keeps them.
pub(super) fn from_proto(
    forwards: &[pb::MachinePortForward],
) -> Result<Vec<MachinePortForward>, ConnectError> {
    forwards
        .iter()
        .map(|forward| {
            let port = |value: u32, what: &str| {
                u16::try_from(value)
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| {
                        ConnectError::invalid_argument(format!("{what} {value} is not a port"))
                    })
            };
            let udp = match forward.protocol.to_ascii_lowercase().as_str() {
                "" | "tcp" => false,
                "udp" => true,
                other => {
                    return Err(ConnectError::invalid_argument(format!(
                        "protocol '{other}' is neither tcp nor udp"
                    )));
                }
            };
            Ok(MachinePortForward {
                host_port: port(forward.host_port, "host port")?,
                guest_port: port(forward.guest_port, "guest port")?,
                udp,
            })
        })
        .collect()
}

/// The engine's forwards on the wire.
pub(super) fn to_proto(forwards: &[MachinePortForward]) -> Vec<pb::MachinePortForward> {
    forwards
        .iter()
        .map(|forward| pb::MachinePortForward {
            host_port: u32::from(forward.host_port),
            guest_port: u32::from(forward.guest_port),
            protocol: if forward.udp { "udp" } else { "tcp" }.to_owned(),
            ..Default::default()
        })
        .collect()
}

/// Binds `forwards` for running machine `machine`, replacing what it had.
pub(super) async fn apply(
    runtime: &Arc<Runtime>,
    machine: &str,
    forwards: &[MachinePortForward],
) -> Result<(), ConnectError> {
    let owner = owner(machine);
    runtime.stop_port_forwarding_by_id(&owner).await;
    if forwards.is_empty() {
        return Ok(());
    }
    let bindings: Vec<(String, u16, u16, String)> = forwards
        .iter()
        .map(|forward| {
            (
                "127.0.0.1".to_owned(),
                forward.host_port,
                forward.guest_port,
                if forward.udp { "udp" } else { "tcp" }.to_owned(),
            )
        })
        .collect();
    runtime
        .start_port_forwarding_for(machine, &owner, &bindings)
        .await
        .map_err(|e| ConnectError::unavailable(format!("forwarded ports could not be bound: {e}")))
}

/// Unbinds the forwards of `machine`.
pub(super) async fn release(runtime: &Arc<Runtime>, machine: &str) {
    runtime.stop_port_forwarding_by_id(&owner(machine)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_forwards_round_trip_and_bad_ones_are_refused() {
        let wire = vec![
            pb::MachinePortForward {
                host_port: 5901,
                guest_port: 5901,
                ..Default::default()
            },
            pb::MachinePortForward {
                host_port: 6000,
                guest_port: 53,
                protocol: "UDP".into(),
                ..Default::default()
            },
        ];
        let forwards = from_proto(&wire).unwrap();
        assert_eq!(
            forwards,
            [
                MachinePortForward {
                    host_port: 5901,
                    guest_port: 5901,
                    udp: false
                },
                MachinePortForward {
                    host_port: 6000,
                    guest_port: 53,
                    udp: true
                },
            ]
        );
        let back = to_proto(&forwards);
        assert_eq!(back[0].protocol, "tcp");
        assert_eq!(back[1].protocol, "udp");

        let bad = |host_port, guest_port, protocol: &str| {
            from_proto(&[pb::MachinePortForward {
                host_port,
                guest_port,
                protocol: protocol.into(),
                ..Default::default()
            }])
            .is_err()
        };
        assert!(bad(0, 22, ""));
        assert!(bad(70000, 22, ""));
        assert!(bad(2222, 0, ""));
        assert!(bad(2222, 22, "sctp"));
    }
}
