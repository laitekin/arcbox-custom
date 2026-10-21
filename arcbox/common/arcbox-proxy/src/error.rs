//! Typed failures shared by proxy connectors and diagnostics.

use std::io;

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
}

impl From<ProxyError> for io::Error {
    fn from(error: ProxyError) -> Self {
        let kind = match error {
            ProxyError::AuthRejected | ProxyError::NoAcceptableMethod => {
                io::ErrorKind::PermissionDenied
            }
            ProxyError::MissingPassword => io::ErrorKind::InvalidInput,
            ProxyError::Protocol(_) => io::ErrorKind::InvalidData,
        };
        Self::new(kind, error)
    }
}
