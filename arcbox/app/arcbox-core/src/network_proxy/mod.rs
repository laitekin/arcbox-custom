//! The daemon's live guest egress proxy settings.
//!
//! [`NetworkProxy`] owns the one [`ProxyState`] every VM's datapath reads
//! when a flow opens. Changing the settings validates them, stores a new
//! password (if any) in the [`CredentialStore`], rewrites `[network]` in
//! `config.toml`, and only then publishes the new snapshot — so a failed
//! step leaves both the running policy and the files as they were. Flows
//! already open keep the transport they started with.

pub mod credentials;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use arcbox_fakeip::proxy_policy::{ProxyPolicy, ProxySettings, ProxyUrl};
use arcbox_fakeip::proxy_state::{IcmpPolicy, ProxySnapshot, ProxyState, UdpPolicy};
use zeroize::Zeroizing;

pub use credentials::CredentialStore;

use crate::config::NetworkConfig;
use crate::error::{CoreError, Result};

/// The persisted part of the egress policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyPreferences {
    /// Where guest egress goes. A custom URL never carries a password here.
    pub policy: ProxyPolicy,
    /// Hosts that bypass the proxy, `NO_PROXY` form.
    pub exclude: Vec<String>,
    /// Guest UDP handling.
    pub udp: UdpPolicy,
    /// Guest ICMP echo handling.
    pub icmp: IcmpPolicy,
}

impl ProxyPreferences {
    /// The preferences `config` describes.
    #[must_use]
    pub fn from_config(network: &NetworkConfig) -> Self {
        Self {
            policy: network.proxy.clone(),
            exclude: network.proxy_exclude.clone(),
            udp: network.udp,
            icmp: network.icmp,
        }
    }
}

/// A requested change to the stored password of a custom proxy.
#[derive(Debug, Default)]
pub enum PasswordChange {
    /// Leave whatever is stored for the proxy's account.
    #[default]
    Keep,
    /// Store this password for the proxy's account.
    Set(Zeroizing<String>),
    /// Remove the stored password.
    Clear,
}

/// What clients are shown: everything except the secret itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySettingsView {
    /// The persisted preferences; a custom URL is password-free.
    pub preferences: ProxyPreferences,
    /// Whether a password is stored for the custom proxy's user.
    pub has_password: bool,
    /// The proxies flows are actually sent to right now, password-free
    /// (`socks5://user@host:port`); empty when everything is direct. Under
    /// `system` this is the result of the last probe of the Mac's settings.
    pub effective: Vec<String>,
    /// Which secret store holds the password (`keychain`, `file`, ...).
    pub credential_store: &'static str,
    /// Generation of the published snapshot; bumps on every change.
    pub revision: u64,
}

/// The daemon's egress proxy controller. See the module docs.
#[derive(Debug)]
pub struct NetworkProxy {
    state: ProxyState,
    credentials: Arc<dyn CredentialStore>,
    config_path: PathBuf,
    /// The persisted preferences; the lock also serializes writers so two
    /// concurrent changes cannot interleave their file and store writes.
    current: Mutex<ProxyPreferences>,
}

impl NetworkProxy {
    /// Builds the controller and resolves the initial snapshot. A missing
    /// stored password is not fatal here: the proxy is installed without it,
    /// so flows fail closed with an authentication error instead of leaking
    /// direct.
    #[must_use]
    pub fn new(
        preferences: ProxyPreferences,
        credentials: Arc<dyn CredentialStore>,
        config_path: PathBuf,
    ) -> Self {
        let environment = match resolve(&preferences, &*credentials, Secret::Stored) {
            Ok(environment) => environment,
            Err(e) => {
                tracing::warn!(error = %e, "proxy credentials unavailable; proxied flows will fail");
                ProxySettings {
                    policy: preferences.policy.clone(),
                    exclude: preferences.exclude.clone(),
                }
                .resolve()
            }
        };
        if let ProxyPolicy::Custom(url) = &preferences.policy
            && url
                .server
                .auth
                .as_ref()
                .is_some_and(|a| a.password().is_some())
        {
            tracing::warn!(
                proxy = %url,
                "proxy password given inline (environment or config); prefer the credential store"
            );
        }
        let state = ProxyState::from_snapshot(ProxySnapshot {
            environment,
            udp: preferences.udp,
            icmp: preferences.icmp,
            revision: 0,
        });
        Self {
            state,
            credentials,
            config_path,
            current: Mutex::new(preferences),
        }
    }

    /// The live state shared with every VM's datapath.
    #[must_use]
    pub fn state(&self) -> ProxyState {
        self.state.clone()
    }

    /// The current settings, without the secret.
    #[must_use]
    pub fn settings(&self) -> ProxySettingsView {
        let preferences = self.lock().clone();
        self.view(preferences)
    }

    /// Re-probes the Mac's proxy settings under the `system` policy and
    /// publishes the result when it changed. Returns whether it did.
    pub fn refresh_system(&self) -> bool {
        let preferences = self.lock();
        if preferences.policy != ProxyPolicy::System {
            return false;
        }
        let Ok(environment) = resolve(&preferences, &*self.credentials, Secret::Stored) else {
            return false;
        };
        let current = self.state.load();
        if describe(environment.as_ref()) == describe(current.environment.as_ref())
            && environment.as_ref().map(|e| &e.bypass_domains)
                == current.environment.as_ref().map(|e| &e.bypass_domains)
        {
            return false;
        }
        self.publish(&preferences, environment);
        true
    }

    /// Validates, stores and publishes new settings. On error nothing has
    /// changed: not the credential store, not `config.toml`, not the policy
    /// flows see.
    ///
    /// A password inlined in a custom URL is treated as [`PasswordChange::Set`]
    /// and moved into the credential store; the URL is persisted without it.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for an unusable request (a password
    /// for a proxy without a user, a user with no password anywhere) and
    /// propagates store and file failures.
    pub fn apply(
        &self,
        mut preferences: ProxyPreferences,
        mut password: PasswordChange,
    ) -> Result<ProxySettingsView> {
        let mut current = self.lock();

        // Split an inline password off the URL.
        if let ProxyPolicy::Custom(url) = &mut preferences.policy
            && let Some(auth) = url.server.auth.clone()
            && let Some(inline) = auth.password()
        {
            if matches!(password, PasswordChange::Keep) {
                password = PasswordChange::Set(Zeroizing::new(inline.to_string()));
            }
            url.server.auth = Some(Arc::new(
                ProxyAuth::new(auth.username().to_string(), None)
                    .map_err(|e| CoreError::config(e.to_string()))?,
            ));
        }

        let account = credential_account(&preferences.policy);
        let targets: Vec<String> = match &password {
            PasswordChange::Keep => Vec::new(),
            PasswordChange::Set(secret) => {
                let Some(account) = &account else {
                    return Err(CoreError::config(
                        "a proxy password needs a custom proxy URL with a user, like socks5://user@host:port",
                    ));
                };
                if !(1..=255).contains(&secret.len()) {
                    return Err(CoreError::config("proxy password must contain 1–255 bytes"));
                }
                vec![account.clone()]
            }
            // Clearing forgets the secret of the proxy being replaced as
            // well as the new one's.
            PasswordChange::Clear => {
                let mut targets: Vec<String> = credential_account(&current.policy)
                    .into_iter()
                    .chain(account)
                    .collect();
                targets.dedup();
                targets
            }
        };

        // Resolve against the would-be store contents before touching it.
        let pending = match &password {
            PasswordChange::Set(secret) => Secret::Given(secret.as_str()),
            PasswordChange::Clear => Secret::Cleared,
            PasswordChange::Keep => Secret::Stored,
        };
        let environment = resolve(&preferences, &*self.credentials, pending)?;

        // Change the store, remembering what to restore.
        let mut restore = Vec::with_capacity(targets.len());
        let changed = targets.iter().try_for_each(|target| {
            let previous = self.credentials.get(target)?;
            match &password {
                PasswordChange::Set(secret) => self.credentials.set(target, secret)?,
                _ => self.credentials.delete(target)?,
            }
            restore.push((target.clone(), previous));
            Ok::<_, CoreError>(())
        });
        let persisted = changed.and_then(|()| {
            crate::config::persist::set_network_proxy(&self.config_path, &preferences)
        });
        if let Err(e) = persisted {
            for (target, previous) in restore.into_iter().rev() {
                let restored = match previous {
                    Some(secret) => self.credentials.set(&target, &secret),
                    None => self.credentials.delete(&target),
                };
                if let Err(undo) = restored {
                    tracing::error!(error = %undo, "failed to restore the previous proxy password");
                }
            }
            return Err(e);
        }

        self.publish(&preferences, environment);
        tracing::info!(
            policy = %preferences.policy,
            udp = ?preferences.udp,
            icmp = ?preferences.icmp,
            revision = self.state.load().revision,
            "egress proxy settings applied"
        );
        *current = preferences.clone();
        drop(current);
        Ok(self.view(preferences))
    }

    /// Resolves draft settings without storing or publishing anything, for
    /// testing a configuration before saving it. `password` overrides the
    /// stored one for the draft's account.
    ///
    /// # Errors
    ///
    /// Returns an error when the draft names a user with no password.
    pub fn resolve_draft(
        &self,
        preferences: &ProxyPreferences,
        password: Option<&str>,
    ) -> Result<Option<ProxyEnvironment>> {
        resolve(
            preferences,
            &*self.credentials,
            password.map_or(Secret::Stored, Secret::Given),
        )
    }

    fn publish(&self, preferences: &ProxyPreferences, environment: Option<ProxyEnvironment>) {
        let revision = self.state.load().revision + 1;
        self.state.store(ProxySnapshot {
            environment,
            udp: preferences.udp,
            icmp: preferences.icmp,
            revision,
        });
    }

    fn view(&self, preferences: ProxyPreferences) -> ProxySettingsView {
        let has_password = credential_account(&preferences.policy)
            .is_some_and(|account| matches!(self.credentials.get(&account), Ok(Some(_))));
        let snapshot = self.state.load();
        ProxySettingsView {
            preferences,
            has_password,
            effective: describe(snapshot.environment.as_ref()),
            credential_store: self.credentials.kind(),
            revision: snapshot.revision,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProxyPreferences> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The credential-store account of a custom proxy with a user: its
/// password-free URL, `scheme://user@host:port`.
fn credential_account(policy: &ProxyPolicy) -> Option<String> {
    match policy {
        ProxyPolicy::Custom(url) if url.server.auth.is_some() => Some(password_free(url)),
        _ => None,
    }
}

fn password_free(url: &ProxyUrl) -> String {
    // `ProxyUrl`'s Display never includes the password.
    url.to_string()
}

/// Where [`resolve`] takes a custom proxy's password from.
#[derive(Clone, Copy)]
enum Secret<'a> {
    /// The credential store.
    Stored,
    /// This value, in place of the store's.
    Given(&'a str),
    /// Nowhere: the stored one is about to be removed.
    Cleared,
}

/// Turns preferences into the environment the datapath enforces, filling a
/// custom proxy's password from `pending`.
fn resolve(
    preferences: &ProxyPreferences,
    store: &dyn CredentialStore,
    pending: Secret<'_>,
) -> Result<Option<ProxyEnvironment>> {
    let mut policy = preferences.policy.clone();
    if let ProxyPolicy::Custom(url) = &mut policy
        && let Some(auth) = url.server.auth.clone()
        && auth.password().is_none()
    {
        let account = password_free(url);
        let stored;
        let secret = match pending {
            Secret::Given(value) => Some(value),
            Secret::Cleared => None,
            Secret::Stored => {
                stored = store.get(&account)?;
                stored.as_deref().map(String::as_str)
            }
        };
        let secret = secret.ok_or_else(|| {
            CoreError::config(format!(
                "no password is stored for proxy {account}; set one with \
                 `abctl network proxy set {account} --password-stdin`"
            ))
        })?;
        url.server.auth = Some(Arc::new(
            ProxyAuth::new(auth.username().to_string(), Some(secret.to_string()))
                .map_err(|e| CoreError::config(e.to_string()))?,
        ));
    }
    Ok(ProxySettings {
        policy,
        exclude: preferences.exclude.clone(),
    }
    .resolve())
}

/// Password-free descriptions of the proxies in `environment`, in the order
/// the egress prefers them.
#[must_use]
pub fn describe(environment: Option<&ProxyEnvironment>) -> Vec<String> {
    let Some(env) = environment else {
        return Vec::new();
    };
    let fmt = |scheme: &str, proxy: &ProxyConfig| {
        let user = proxy
            .auth
            .as_ref()
            .map(|a| format!("{}@", a.username()))
            .unwrap_or_default();
        if proxy.host.contains(':') {
            format!("{scheme}://{user}[{}]:{}", proxy.host, proxy.port)
        } else {
            format!("{scheme}://{user}{}:{}", proxy.host, proxy.port)
        }
    };
    let mut out = Vec::new();
    if let Some(p) = &env.socks_proxy {
        out.push(fmt("socks5", p));
    }
    if let Some(p) = &env.https_proxy {
        out.push(fmt("https", p));
    }
    if let Some(p) = &env.http_proxy {
        out.push(fmt("http", p));
    }
    out
}

#[cfg(test)]
mod tests;
