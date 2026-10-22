//! Egress rules for a machine isolated from the Mac.
//!
//! An isolated machine reaches the internet only through its proxy: every
//! flow is tunnelled, the bypass list does not apply, and a flow that cannot
//! be tunnelled is refused rather than sent direct. On top of that it may not
//! reach the Mac (the datapath gateway, which the host translates to its own
//! loopback), the Mac's networks, or other machines, whatever the proxy would
//! do with such an address.
//!
//! The rules are the datapath's: the guest's own configuration is never
//! trusted, since root in the guest can change it.

use std::net::Ipv4Addr;

/// Whether an isolated machine may open a flow to `dst`.
///
/// Refused: the gateway (it means the Mac itself), `0.0.0.0/8`, loopback,
/// the private ranges (`10/8`, `172.16/12`, `192.168/16`), shared address
/// space (`100.64/10`), link-local, multicast, and the reserved and
/// broadcast range `240/4`. Allowed: every public address and the Fake-IP
/// range `198.18/15`, whose addresses the proxy resolves by name.
#[must_use]
pub fn allows_destination(dst: Ipv4Addr, gateway: Ipv4Addr) -> bool {
    if dst == gateway {
        return false;
    }
    let [a, b, ..] = dst.octets();
    let shared_address_space = a == 100 && (64..128).contains(&b);
    !(a == 0
        || dst.is_loopback()
        || dst.is_private()
        || shared_address_space
        || dst.is_link_local()
        || dst.is_multicast()
        || a >= 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 1);

    #[test]
    fn the_mac_its_networks_and_other_machines_are_refused() {
        for dst in [
            GATEWAY,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(10, 0, 2, 3),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 31, 255, 254),
            Ipv4Addr::new(192, 168, 64, 1),
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(100, 127, 255, 254),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(224, 0, 0, 251),
            Ipv4Addr::new(239, 255, 255, 250),
            Ipv4Addr::new(240, 0, 0, 1),
            Ipv4Addr::BROADCAST,
        ] {
            assert!(!allows_destination(dst, GATEWAY), "{dst} must be refused");
        }
    }

    #[test]
    fn public_and_fake_ip_destinations_are_allowed() {
        for dst in [
            Ipv4Addr::new(1, 1, 1, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(172, 32, 0, 1),
            Ipv4Addr::new(100, 128, 0, 1),
            Ipv4Addr::new(198, 18, 0, 5),
            Ipv4Addr::new(198, 19, 255, 1),
            Ipv4Addr::new(203, 0, 113, 9),
        ] {
            assert!(allows_destination(dst, GATEWAY), "{dst} must be allowed");
        }
    }
}
