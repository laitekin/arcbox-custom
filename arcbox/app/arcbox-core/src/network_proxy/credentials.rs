//! Where proxy passwords live.
//!
//! A password never enters `config.toml`: the config names the proxy as
//! `scheme://user@host:port`, and that exact string is the account under
//! which the secret is stored. On macOS the store is the login keychain (a
//! generic password whose service is per-profile, so a development daemon
//! never reads production's secret). Elsewhere, and on macOS when the
//! operator opts out of the keychain, it is a 0600 TOML file in the
//! profile's data directory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{CoreError, Result};

/// Keychain service of the production profile's proxy passwords.
pub const KEYCHAIN_SERVICE: &str = "com.arcboxlabs.network-proxy";

/// A write-only secret store keyed by account.
pub trait CredentialStore: Send + Sync + std::fmt::Debug {
    /// The secret stored for `account`, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the store itself cannot be read (as opposed to
    /// holding nothing for `account`).
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>>;

    /// Stores `secret` for `account`, replacing any previous value.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    fn set(&self, account: &str, secret: &str) -> Result<()>;

    /// Removes the secret for `account`; removing an absent one succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    fn delete(&self, account: &str) -> Result<()>;

    /// A short name for diagnostics (`keychain`, `file`, `memory`).
    fn kind(&self) -> &'static str;
}

/// Secrets in a TOML table of `account = "secret"`, replaced atomically.
/// The file is created mode 0600 (the atomic writer's temp file mode
/// survives the rename).
#[derive(Debug)]
pub struct FileCredentialStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileCredentialStore {
    /// A store backed by `path`; the file is created on the first `set`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Entries> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => Zeroizing::new(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Entries::default());
            }
            Err(e) => {
                return Err(CoreError::config(format!(
                    "failed to read {}: {e}",
                    self.path.display()
                )));
            }
        };
        // The parse error would quote the offending line, which may be a
        // secret, so only the location is reported.
        toml::from_str(&text).map_err(|_| {
            CoreError::config(format!(
                "{} is not a valid credential file",
                self.path.display()
            ))
        })
    }

    fn write(&self, entries: &Entries) -> Result<()> {
        let text = Zeroizing::new(
            toml::to_string(entries)
                .map_err(|e| CoreError::config(format!("failed to encode credentials: {e}")))?,
        );
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        arcbox_atomic_file::write(&self.path, text.as_bytes())?;
        Ok(())
    }
}

impl CredentialStore for FileCredentialStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.read()?.0.get(account).cloned().map(Zeroizing::new))
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.read()?;
        if let Some(mut old) = entries.0.insert(account.to_string(), secret.to_string()) {
            old.zeroize();
        }
        self.write(&entries)
    }

    fn delete(&self, account: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.read()?;
        if let Some(mut old) = entries.0.remove(account) {
            old.zeroize();
            self.write(&entries)?;
        }
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "file"
    }
}

/// The credential file's table; values are wiped when dropped.
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
struct Entries(BTreeMap<String, String>);

impl Drop for Entries {
    fn drop(&mut self) {
        self.0.values_mut().for_each(Zeroize::zeroize);
    }
}

/// An in-memory store for tests and for daemons that must not persist.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore(Mutex<BTreeMap<String, Zeroizing<String>>>);

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account.to_string(), Zeroizing::new(secret.to_string()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account);
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "memory"
    }
}

/// Generic passwords in the user's login keychain.
///
/// The daemon is a gui launchd agent in the user's session, so the login
/// keychain is unlocked and reachable. The item's access list is bound to
/// the daemon's code signature: a rebuild signed with a different identity
/// is prompted again, which is why [`FileCredentialStore`] remains available.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct KeychainCredentialStore {
    service: String,
}

#[cfg(target_os = "macos")]
impl KeychainCredentialStore {
    /// A store under keychain service `service`.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for KeychainCredentialStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>> {
        use security_framework::passwords::get_generic_password;
        // errSecItemNotFound
        const NOT_FOUND: i32 = -25300;
        match get_generic_password(&self.service, account) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                String::from_utf8(bytes.to_vec())
                    .map(|s| Some(Zeroizing::new(s)))
                    .map_err(|_| CoreError::config("keychain proxy password is not UTF-8"))
            }
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(CoreError::config(format!(
                "failed to read the proxy password from the keychain: {e}"
            ))),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        security_framework::passwords::set_generic_password(
            &self.service,
            account,
            secret.as_bytes(),
        )
        .map_err(|e| {
            CoreError::config(format!(
                "failed to store the proxy password in the keychain: {e}"
            ))
        })
    }

    fn delete(&self, account: &str) -> Result<()> {
        const NOT_FOUND: i32 = -25300;
        match security_framework::passwords::delete_generic_password(&self.service, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(CoreError::config(format!(
                "failed to remove the proxy password from the keychain: {e}"
            ))),
        }
    }

    fn kind(&self) -> &'static str {
        "keychain"
    }
}

/// The store a daemon of `profile` uses: the login keychain on macOS unless
/// `ARCBOX_PROXY_CREDENTIALS=file`, otherwise `credentials/proxy.toml` under
/// the profile's data directory.
#[must_use]
pub fn default_store(
    profile: arcbox_constants::paths::ArcboxProfile,
    data_dir: &std::path::Path,
) -> std::sync::Arc<dyn CredentialStore> {
    let file = || {
        std::sync::Arc::new(FileCredentialStore::new(
            data_dir.join("credentials").join("proxy.toml"),
        )) as std::sync::Arc<dyn CredentialStore>
    };
    #[cfg(target_os = "macos")]
    {
        if std::env::var("ARCBOX_PROXY_CREDENTIALS").as_deref() != Ok("file") {
            let service = match profile {
                arcbox_constants::paths::ArcboxProfile::Production => KEYCHAIN_SERVICE.to_string(),
                arcbox_constants::paths::ArcboxProfile::Development => {
                    format!("{KEYCHAIN_SERVICE}.dev")
                }
            };
            return std::sync::Arc::new(KeychainCredentialStore::new(service));
        }
    }
    let _ = profile;
    file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials").join("proxy.toml");
        let store = FileCredentialStore::new(path.clone());

        assert!(store.get("socks5://alice@h:1").unwrap().is_none());
        store.set("socks5://alice@h:1", "p@ss:\"word\"").unwrap();
        store.set("socks5://bob@h:1", "other").unwrap();
        assert_eq!(
            store
                .get("socks5://alice@h:1")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("p@ss:\"word\"")
        );

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        store.delete("socks5://alice@h:1").unwrap();
        store.delete("socks5://alice@h:1").unwrap();
        assert!(store.get("socks5://alice@h:1").unwrap().is_none());
        assert!(store.get("socks5://bob@h:1").unwrap().is_some());
    }

    #[test]
    fn a_corrupt_file_reports_its_path_but_not_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.toml");
        std::fs::write(&path, "account = do-not-leak").unwrap();
        let error = FileCredentialStore::new(path).get("account").unwrap_err();
        assert!(!error.to_string().contains("do-not-leak"));
    }
}
