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
                state: ProxyState::from_snapshot((*self.default.load()).clone()),
                inherited: true,
            })
            .state
            .clone()
    }

    /// Publishing a default never overwrites an explicit machine choice.
    pub fn publish_default(&self, snapshot: ProxySnapshot) {
        let machines = self.machines.write().expect("proxy routes lock poisoned");
        self.default.store(snapshot.clone());
        for route in machines.values().filter(|route| route.inherited) {
            store(&route.state, snapshot.clone());
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
        );
    }
}

fn store(state: &ProxyState, mut snapshot: ProxySnapshot) {
    snapshot.revision = state.load().revision + 1;
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
}
