//! Per-machine live policies without coupling the datapath to VM types.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::proxy_state::{ProxySnapshot, ProxyState};

#[derive(Debug, Clone)]
pub struct ProxyRoutes {
    default: ProxyState,
    machines: Arc<RwLock<HashMap<String, Route>>>,
}

#[derive(Debug)]
struct Route {
    state: ProxyState,
    inherited: bool,
    /// Stamped on every snapshot this machine gets, whichever source it
    /// comes from.
    isolated: bool,
}

impl ProxyRoutes {
    #[must_use]
    pub fn new(default: ProxyState) -> Self {
        Self {
            default,
            machines: Arc::default(),
        }
    }

    /// Called at VM start; the returned cell stays live for its lifetime.
    #[must_use]
    pub fn for_machine(&self, name: &str) -> ProxyState {
        let mut machines = self.machines.write().expect("proxy routes lock poisoned");
        machines
            .entry(name.to_string())
            .or_insert_with(|| Route {
                state: ProxyState::from_snapshot(ProxySnapshot {
                    isolated: false,
                    ..(*self.default.load()).clone()
                }),
                inherited: true,
                isolated: false,
            })
            .state
            .clone()
    }

    /// Publishing a default never overwrites an explicit machine choice.
    pub fn publish_default(&self, snapshot: ProxySnapshot) {
        let machines = self.machines.write().expect("proxy routes lock poisoned");
        self.default.store(snapshot.clone());
        for route in machines.values().filter(|route| route.inherited) {
            store(&route.state, snapshot.clone(), route.isolated);
        }
    }

    /// `None` follows the default; a snapshot overrides only this machine.
    pub fn set_machine(&self, name: &str, snapshot: Option<ProxySnapshot>) {
        let state = self.for_machine(name);
        let mut machines = self.machines.write().expect("proxy routes lock poisoned");
        let route = machines.get_mut(name).expect("machine route created above");
        route.inherited = snapshot.is_none();
        store(
            &state,
            snapshot.unwrap_or_else(|| (*self.default.load()).clone()),
            route.isolated,
        );
    }

    /// Marks machine `name` isolated or not. Its current policy is
    /// republished with the flag, and every later one carries it, so the
    /// rules hold from the next flow on.
    pub fn set_isolated(&self, name: &str, isolated: bool) {
        let state = self.for_machine(name);
        let mut machines = self.machines.write().expect("proxy routes lock poisoned");
        let route = machines.get_mut(name).expect("machine route created above");
        route.isolated = isolated;
        store(&state, (*state.load()).clone(), isolated);
    }
}

fn store(state: &ProxyState, mut snapshot: ProxySnapshot, isolated: bool) {
    snapshot.revision = state.load().revision + 1;
    snapshot.isolated = isolated;
    state.store(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_state::UdpPolicy;

    #[test]
    fn machine_choices_are_isolated_and_follow_default_again_when_reset() {
        let routes = ProxyRoutes::new(ProxyState::default());
        let first = routes.for_machine("first");
        let second = routes.for_machine("second");
        routes.set_machine(
            "first",
            Some(ProxySnapshot {
                udp: UdpPolicy::Block,
                ..Default::default()
            }),
        );
        routes.publish_default(ProxySnapshot {
            udp: UdpPolicy::Direct,
            ..Default::default()
        });
        assert_eq!(first.load().udp, UdpPolicy::Block);
        assert_eq!(second.load().udp, UdpPolicy::Direct);
        routes.set_machine("first", None);
        assert_eq!(first.load().udp, UdpPolicy::Direct);
        routes.publish_default(ProxySnapshot::default());
        assert_eq!(first.load().udp, UdpPolicy::Proxy);
        assert_eq!(second.load().udp, UdpPolicy::Proxy);
    }

    #[test]
    fn isolation_survives_every_later_policy_and_only_for_its_machine() {
        let routes = ProxyRoutes::new(ProxyState::default());
        let sealed = routes.for_machine("sealed");
        let open = routes.for_machine("open");

        routes.set_isolated("sealed", true);
        assert!(sealed.load().isolated);
        assert!(!open.load().isolated);

        // A new default, then an explicit choice, keep the flag.
        routes.publish_default(ProxySnapshot {
            udp: UdpPolicy::Block,
            ..Default::default()
        });
        assert!(sealed.load().isolated);
        assert_eq!(sealed.load().udp, UdpPolicy::Block);
        routes.set_machine("sealed", Some(ProxySnapshot::default()));
        assert!(sealed.load().isolated);
        routes.set_machine("sealed", None);
        assert!(sealed.load().isolated);
        assert!(!open.load().isolated);

        // A default snapshot that claims isolation does not leak to others.
        routes.publish_default(ProxySnapshot {
            isolated: true,
            ..Default::default()
        });
        assert!(!open.load().isolated);
        assert!(!routes.for_machine("late").load().isolated);

        routes.set_isolated("sealed", false);
        assert!(!sealed.load().isolated);
    }
}
