//! Typed failures shared by proxy connectors and diagnostics.

use std::io;

/// Why a proxy connector failed, carried inside its `io::Error`.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("proxy authentication failed")]
    AuthRejected,
    #[error("proxy did not accept the required authentication method")]
    NoAcceptableMethod,
    #[error("proxy password is missing or unavailable in the credential store")]
    MissingPassword,
    #[error("invalid proxy protocol response: {0}")]
    Protocol(&'static str),
    /// The SOCKS5 server answered a request with a non-zero REP code.
    #[error("SOCKS5: request failed: {reason} (code {code})", code = .0, reason = socks_reply_reason(*.0))]
    Refused(u8),
}

/// RFC 1928 §6 reply-field meanings.
const fn socks_reply_reason(code: u8) -> &'static str {
    match code {
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown error",
    }
}

impl ProxyError {
    /// The typed failure behind `error`, when a connector produced one.
    #[must_use]
    pub fn from_io(error: &io::Error) -> Option<&Self> {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Self>())
    }
}

impl From<ProxyError> for io::Error {
    fn from(error: ProxyError) -> Self {
        let kind = match error {
            ProxyError::AuthRejected | ProxyError::NoAcceptableMethod => {
                io::ErrorKind::PermissionDenied
            }
            ProxyError::MissingPassword => io::ErrorKind::InvalidInput,
            ProxyError::Protocol(_) => io::ErrorKind::InvalidData,
            ProxyError::Refused(0x05) => io::ErrorKind::ConnectionRefused,
            ProxyError::Refused(_) => io::ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}
