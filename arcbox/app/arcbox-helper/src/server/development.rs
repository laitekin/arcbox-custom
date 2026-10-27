//! Explicit, process-lifetime development grants. The production service
//! continues to require its vendor signature. No grant is read from disk.

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

#[derive(Default)]
pub struct DevelopmentPeers {
    uid: Option<u32>,
    hashes: Vec<String>,
}

impl DevelopmentPeers {
    pub fn add(&mut self, grant: &str) -> io::Result<()> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected UID:40-hex-CDHash, all grants for one non-root UID",
            )
        };
        let (uid, hash) = grant.split_once(':').ok_or_else(invalid)?;
        let uid: u32 = uid.parse().map_err(|_| invalid())?;
        if uid == 0
            || self.uid.is_some_and(|previous| previous != uid)
            || hash.len() != 40
            || !hash.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        self.uid = Some(uid);
        self.hashes.push(hash.to_ascii_lowercase());
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.uid.is_some()
    }

    pub fn verify(&self, stream: &tokio::net::UnixStream) -> bool {
        self.uid
            .is_some_and(|uid| super::peer_auth::verify_development(stream, uid, &self.hashes))
    }

    pub fn validate_socket(&self, socket: &Path) -> io::Result<()> {
        let reject = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "development helper requires an absolute private socket in a directory owned by the granted UID with mode 0700",
            )
        };
        if !socket.is_absolute() || socket == Path::new(arcbox_helper::HELPER_SOCKET) {
            return Err(reject());
        }
        let parent = socket.parent().ok_or_else(reject)?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.uid() != self.uid.ok_or_else(reject)?
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(reject());
        }
        Ok(())
    }

    /// Never remove an existing entry as root. A stale private socket can
    /// be removed by its directory's owner after the old helper exits.
    pub fn bind(&self) -> io::Result<tokio::net::UnixListener> {
        let path = arcbox_helper::socket_path();
        self.validate_socket(Path::new(&path))?;
        let listener = tokio::net::UnixListener::bind(&path)?;
        // Directory traversal is restricted to the granted user. Every
        // connection is additionally checked against UID + live CDHash.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
        Ok(listener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_reject_injection_root_and_mixed_users() {
        let mut peers = DevelopmentPeers::default();
        let hash = "ab".repeat(20);
        for value in [
            "501:any".into(),
            format!("0:{hash}"),
            format!("501:{}\" or always", hash),
        ] {
            assert!(peers.add(&value).is_err());
        }
        peers.add(&format!("501:{hash}")).unwrap();
        assert!(peers.add(&format!("502:{hash}")).is_err());
        assert!(peers.enabled());
    }

    #[test]
    fn socket_must_be_private_and_owned_by_granted_user() {
        let directory = tempfile::tempdir().unwrap();
        let uid = directory.path().metadata().unwrap().uid();
        let mut peers = DevelopmentPeers::default();
        peers.add(&format!("{uid}:{}", "ab".repeat(20))).unwrap();
        let path = directory.path().join("helper.sock");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        peers.validate_socket(&path).unwrap();
        assert!(
            peers
                .validate_socket(Path::new(arcbox_helper::HELPER_SOCKET))
                .is_err()
        );
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(peers.validate_socket(&path).is_err());
    }

    #[tokio::test]
    async fn ungranted_live_peer_is_rejected_even_in_debug() {
        let (stream, _other) = tokio::net::UnixStream::pair().unwrap();
        // SAFETY: geteuid has no arguments or memory preconditions.
        let uid = unsafe { libc::geteuid() };
        assert!(!super::super::peer_auth::verify_development(
            &stream,
            uid,
            &["00".repeat(20)]
        ));
        assert!(!super::super::peer_auth::verify_development(
            &stream,
            uid.wrapping_add(1),
            &["00".repeat(20)]
        ));
    }
}
