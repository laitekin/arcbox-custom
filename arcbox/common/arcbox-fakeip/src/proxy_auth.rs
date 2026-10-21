//! Write-only proxy credentials shared by host-side connectors.

use std::fmt;

use zeroize::Zeroizing;

/// RFC 1929 credentials. Cloning the enclosing `Arc` avoids copying secrets.
pub struct ProxyAuth {
    username: String,
    password: Option<Zeroizing<String>>,
}

impl ProxyAuth {
    /// Validates wire lengths in bytes. A missing password is a keychain reference.
    pub fn new(username: String, password: Option<String>) -> Result<Self, &'static str> {
        let password = password.map(Zeroizing::new);
        if !(1..=255).contains(&username.len())
            || password
                .as_ref()
                .is_some_and(|value| !(1..=255).contains(&value.len()))
        {
            return Err("proxy username and password must contain 1–255 bytes");
        }
        Ok(Self { username, password })
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Exposes the secret only to the authentication transport or credential store.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.password.as_ref().map(|value| value.as_str())
    }
}

impl fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl PartialEq for ProxyAuth {
    fn eq(&self, other: &Self) -> bool {
        self.username == other.username && self.password() == other.password()
    }
}

impl Eq for ProxyAuth {}
