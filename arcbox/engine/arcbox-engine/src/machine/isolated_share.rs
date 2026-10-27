//! Minimal, read-only boot payload for machines without the host data share.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{EngineError, Result};

/// Do not follow a directory symlink left by an older writable guest share.
pub(super) fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other(format!(
            "{} must be a directory, not a symlink or file",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

/// Refresh before each start, so an agent upgrade also reaches stopped machines.
/// Copy bytes rather than hard-linking a file from the shared host runtime.
pub(super) fn prepare(data_dir: &Path, machine_dir: &Path) -> Result<()> {
    let own = machine_dir.join(super::ISOLATED_SHARE_DIR);
    directory(&own)?;
    let bin = own.join("bin");
    directory(&bin)?;
    let source = data_dir.join("bin/arcbox-agent");
    let bytes = fs::read(&source).map_err(|error| {
        EngineError::Machine(format!(
            "isolated machine requires its boot agent at {}: {error}",
            source.display()
        ))
    })?;
    let destination = bin.join("arcbox-agent");
    // Both atomic-write failure variants abort startup: never boot an
    // unconfirmed payload. Rename replaces a file symlink without following it.
    arcbox_atomic_file::write(&destination, &bytes)
        .map_err(|error| EngineError::Machine(format!("staging isolated boot agent: {error}")))?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o555))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn agent_is_private_executable_and_refreshed_without_exposing_host_data() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path();
        let machine = data.join("machines/isolated");
        fs::create_dir_all(&machine).unwrap();
        fs::create_dir(data.join("bin")).unwrap();
        fs::write(data.join("bin/arcbox-agent"), b"old agent").unwrap();
        fs::write(data.join("credentials.toml"), b"host only").unwrap();
        prepare(data, &machine).unwrap();
        let own = machine.join(super::super::ISOLATED_SHARE_DIR);
        let agent = own.join("bin/arcbox-agent");
        assert_eq!(fs::read(&agent).unwrap(), b"old agent");
        assert_eq!(
            fs::metadata(&agent).unwrap().permissions().mode() & 0o777,
            0o555
        );
        assert_ne!(
            fs::metadata(&agent).unwrap().ino(),
            fs::metadata(data.join("bin/arcbox-agent")).unwrap().ino()
        );
        assert!(!own.join("credentials.toml").exists());
        fs::write(data.join("bin/arcbox-agent"), b"new agent").unwrap();
        prepare(data, &machine).unwrap();
        assert_eq!(fs::read(&agent).unwrap(), b"new agent");
    }

    #[test]
    fn missing_agent_and_symlinked_directories_abort_preparation() {
        let root = tempfile::tempdir().unwrap();
        let machine = root.path().join("machine");
        fs::create_dir(&machine).unwrap();
        assert!(prepare(root.path(), &machine).is_err());
        let own = machine.join(super::super::ISOLATED_SHARE_DIR);
        fs::remove_dir(own.join("bin")).unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, own.join("bin")).unwrap();
        assert!(prepare(root.path(), &machine).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(own.join("bin")).unwrap();
        fs::remove_dir(&own).unwrap();
        symlink(&outside, &own).unwrap();
        assert!(prepare(root.path(), &machine).is_err());
    }
}
