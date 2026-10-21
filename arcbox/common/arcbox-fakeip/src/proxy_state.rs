//! Shared host egress policy. Existing flows keep their own resolved transport.

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::proxy_detect::ProxyEnvironment;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UdpPolicy {
    #[default]
    Proxy,
    Direct,
    Block,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IcmpPolicy {
    #[default]
    Direct,
    Block,
}

/// One consistent generation of the policy, loaded only when a flow opens.
#[derive(Debug, Clone, Default)]
pub struct ProxySnapshot {
    pub environment: Option<ProxyEnvironment>,
    pub udp: UdpPolicy,
    pub icmp: IcmpPolicy,
    pub revision: u64,
}

/// Lock-free reads shared by all currently running and future VMs.
#[derive(Debug, Clone)]
pub struct ProxyState(Arc<ArcSwap<ProxySnapshot>>);

impl Default for ProxyState {
    fn default() -> Self {
        Self::from_snapshot(ProxySnapshot::default())
    }
}

impl ProxyState {
    #[must_use]
    pub fn new(environment: Option<ProxyEnvironment>) -> Self {
        Self::from_snapshot(ProxySnapshot {
            environment,
            ..Default::default()
        })
    }

    #[must_use]
    pub fn from_snapshot(snapshot: ProxySnapshot) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(snapshot)))
    }

    #[must_use]
    pub fn load(&self) -> Arc<ProxySnapshot> {
        self.0.load_full()
    }

    /// Writers serialize persistence before publishing the new generation.
    pub fn store(&self, snapshot: ProxySnapshot) {
        self.0.store(Arc::new(snapshot));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumers_observe_new_generations_without_mutating_old_snapshots() {
        let state = ProxyState::default();
        let vm = state.clone();
        let old = vm.load();
        state.store(ProxySnapshot {
            revision: 1,
            udp: UdpPolicy::Block,
            ..Default::default()
        });
        assert_eq!(old.revision, 0);
        assert_eq!(old.udp, UdpPolicy::Proxy);
        assert_eq!(vm.load().revision, 1);
        assert_eq!(vm.load().udp, UdpPolicy::Block);
    }
}
