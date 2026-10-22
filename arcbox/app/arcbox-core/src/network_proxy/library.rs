//! Saved SOCKS5 profiles, credential isolation, and persistent machine choices.

mod mutations;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use arcbox_fakeip::proxy_policy::ProxyScheme;
use serde::{Deserialize, Serialize};

use super::*;

/// Password-free configuration stored inside `[network.proxy_library]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyLibrary {
    pub profiles: Vec<SavedProxy>,
    pub default_profile: String,
    /// Missing means inherit; `direct` is explicit; other values are profile IDs.
    pub machines: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedProxy {
    pub id: String,
    pub name: String,
    pub url: String,
    pub exclude: Vec<String>,
    pub udp: UdpPolicy,
    pub icmp: IcmpPolicy,
}

#[derive(Debug, Clone)]
pub struct SavedProxyView {
    pub id: String,
    pub name: String,
    pub settings: ProxySettingsView,
    pub machines: Vec<String>,
    pub is_default: bool,
}

impl SavedProxy {
    fn preferences(&self) -> Result<ProxyPreferences> {
        let policy = self
            .url
            .parse::<ProxyPolicy>()
            .map_err(|e| CoreError::config(e.to_string()))?;
        match &policy {
            ProxyPolicy::Custom(url)
                if matches!(url.scheme, ProxyScheme::Socks5 | ProxyScheme::Socks5h) => {}
            _ => {
                return Err(CoreError::config(
                    "saved proxies require a SOCKS5 or SOCKS5h URL",
                ));
            }
        }
        Ok(ProxyPreferences {
            policy,
            exclude: self.exclude.clone(),
            udp: self.udp,
            icmp: self.icmp,
        })
    }
}

pub(super) fn account(id: &str) -> String {
    format!("profile:{id}")
}

impl NetworkProxy {
    #[must_use]
    pub fn routes(&self) -> arcbox_fakeip::proxy_routes::ProxyRoutes {
        self.routes.clone()
    }

    /// Loaded before any VM starts. Missing credentials leave the selected
    /// tunnel in place without a password, so its authentication fails closed.
    pub fn load_library(&self, library: ProxyLibrary) -> Result<()> {
        let mut current = self.lock();
        let mut ids = std::collections::HashSet::new();
        for profile in &library.profiles {
            if uuid::Uuid::parse_str(&profile.id).is_err() || !ids.insert(&profile.id) {
                return Err(CoreError::config("invalid or duplicate saved proxy ID"));
            }
            let preferences = profile.preferences()?;
            if let ProxyPolicy::Custom(url) = &preferences.policy
                && url
                    .server
                    .auth
                    .as_ref()
                    .is_some_and(|auth| auth.password().is_some())
            {
                return Err(CoreError::config(
                    "saved proxy passwords must be in the credential store",
                ));
            }
        }
        if !library.default_profile.is_empty() {
            let profile = find(&library, &library.default_profile)?;
            *current = profile.preferences()?;
            let environment = self.resolve_profile(profile, None).unwrap_or_else(|_| {
                ProxySettings {
                    policy: current.policy.clone(),
                    exclude: current.exclude.clone(),
                }
                .resolve()
            });
            self.publish(&current, environment);
        }
        for (machine, choice) in &library.machines {
            let snapshot = if choice == "direct" {
                direct()
            } else {
                let profile = find(&library, choice)?;
                let preferences = profile.preferences()?;
                let environment = self.resolve_profile(profile, None).unwrap_or_else(|_| {
                    ProxySettings {
                        policy: preferences.policy.clone(),
                        exclude: preferences.exclude.clone(),
                    }
                    .resolve()
                });
                snapshot(&preferences, environment)
            };
            self.routes.set_machine(machine, Some(snapshot));
        }
        *self.library.write().expect("proxy library lock poisoned") = library;
        Ok(())
    }

    #[must_use]
    pub fn proxy_library(&self) -> ProxyLibrary {
        self.library
            .read()
            .expect("proxy library lock poisoned")
            .clone()
    }

    pub fn saved_proxies(&self) -> Result<Vec<SavedProxyView>> {
        let library = self.proxy_library();
        library
            .profiles
            .iter()
            .map(|profile| self.profile_view(profile, &library))
            .collect()
    }

    pub fn resolve_profile_draft(
        &self,
        id: &str,
        preferences: &ProxyPreferences,
        password: Option<&str>,
    ) -> Result<Option<ProxyEnvironment>> {
        let library = self.proxy_library();
        let original = find(&library, id)?;
        let mut draft = original.clone();
        draft.url = preferences.policy.to_string();
        draft.exclude.clone_from(&preferences.exclude);
        draft.udp = preferences.udp;
        draft.icmp = preferences.icmp;
        if password.is_none()
            && credential_account(&original.preferences()?.policy)
                != credential_account(&preferences.policy)
        {
            return Err(CoreError::config(
                "enter a password for the changed proxy account",
            ));
        }
        self.resolve_profile(&draft, password)
    }

    fn resolve_profile(
        &self,
        profile: &SavedProxy,
        password: Option<&str>,
    ) -> Result<Option<ProxyEnvironment>> {
        let preferences = profile.preferences()?;
        let stored = if password.is_none() {
            self.credentials.get(&account(&profile.id))?
        } else {
            None
        };
        let secret = password.or_else(|| stored.as_deref().map(String::as_str));
        resolve(
            &preferences,
            &*self.credentials,
            secret.map_or(Secret::Cleared, Secret::Given),
        )
    }

    fn profile_view(&self, profile: &SavedProxy, library: &ProxyLibrary) -> Result<SavedProxyView> {
        let preferences = profile.preferences()?;
        let has_user =
            matches!(&preferences.policy, ProxyPolicy::Custom(url) if url.server.auth.is_some());
        let environment = ProxySettings {
            policy: preferences.policy.clone(),
            exclude: preferences.exclude.clone(),
        }
        .resolve();
        Ok(SavedProxyView {
            id: profile.id.clone(),
            name: profile.name.clone(),
            settings: ProxySettingsView {
                preferences,
                has_password: has_user
                    && matches!(self.credentials.get(&account(&profile.id)), Ok(Some(_))),
                effective: describe(environment.as_ref()),
                credential_store: self.credentials.kind(),
                revision: self.state.load().revision,
                profile_id: profile.id.clone(),
            },
            machines: library
                .machines
                .iter()
                .filter(|(_, id)| **id == profile.id)
                .map(|(name, _)| name.clone())
                .collect(),
            is_default: library.default_profile == profile.id,
        })
    }

    pub fn machine_proxy(&self, name: &str) -> Result<(String, ProxySettingsView)> {
        let library = self.proxy_library();
        let choice = library.machines.get(name).cloned().unwrap_or_default();
        let settings = if choice == "direct" {
            ProxySettingsView {
                preferences: ProxyPreferences {
                    policy: ProxyPolicy::None,
                    exclude: vec![],
                    udp: UdpPolicy::Direct,
                    icmp: IcmpPolicy::Direct,
                },
                has_password: false,
                effective: vec![],
                credential_store: self.credentials.kind(),
                revision: 0,
                profile_id: String::new(),
            }
        } else if choice.is_empty() {
            self.settings()
        } else {
            self.profile_view(find(&library, &choice)?, &library)?
                .settings
        };
        Ok((choice, settings))
    }
}

fn find<'a>(library: &'a ProxyLibrary, id: &str) -> Result<&'a SavedProxy> {
    library
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| CoreError::config("saved proxy not found"))
}

fn snapshot(
    preferences: &ProxyPreferences,
    environment: Option<ProxyEnvironment>,
) -> ProxySnapshot {
    ProxySnapshot {
        environment,
        udp: preferences.udp,
        icmp: preferences.icmp,
        revision: 0,
    }
}

fn direct() -> ProxySnapshot {
    ProxySnapshot {
        udp: UdpPolicy::Direct,
        ..Default::default()
    }
}
