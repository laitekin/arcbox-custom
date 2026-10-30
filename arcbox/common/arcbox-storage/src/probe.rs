use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result, VolumeRole};

const PROVISION_MAGIC: &[u8; 16] = b"ARCBOX-NEW-VOL-1";

/// Host file identity, independent of the image's filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageIdentity {
    /// Host filesystem device (a mount-local fallback for older manifests).
    pub device: u64,
    /// Host inode.
    pub inode: u64,
    /// Persistent host volume identity; macOS device numbers can change on reboot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_uuid: Option<Uuid>,
}

impl ImageIdentity {
    /// Reads the identity of an existing regular image file.
    ///
    /// # Errors
    /// Returns an error for missing files, symlinks, or non-regular files.
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::RecoveryRequired(format!(
                "{} is not a regular image file",
                path.display()
            )));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            #[cfg(target_os = "macos")]
            volume_uuid: Some(host_volume_uuid(path)?),
            #[cfg(not(target_os = "macos"))]
            volume_uuid: None,
        })
    }

    /// Compares persistent volume and inode identities, retaining strict device
    /// checks until an older manifest has been verified and upgraded.
    #[must_use]
    pub fn matches(self, other: Self) -> bool {
        self.inode == other.inode
            && match (self.volume_uuid, other.volume_uuid) {
                (Some(left), Some(right)) => left == right,
                _ => self.device == other.device,
            }
    }
}

#[cfg(target_os = "macos")]
fn host_volume_uuid(path: &Path) -> Result<Uuid> {
    use std::os::fd::AsRawFd;

    let file = File::open(path)?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // getattrlist packs a four-byte length followed by the 16-byte UUID.
    let mut buffer = [0_u8; 20];
    // SAFETY: the fd remains open; attributes and buffer are writable, valid
    // for their declared sizes, and no pointers are retained by fgetattrlist.
    let result = unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            std::ptr::from_mut(&mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let length = u32::from_ne_bytes(buffer[..4].try_into().unwrap());
    let uuid = Uuid::from_bytes(buffer[4..].try_into().unwrap());
    if length != 20 || uuid.is_nil() {
        return Err(Error::RecoveryRequired(format!(
            "{} has no persistent host volume identity",
            path.display()
        )));
    }
    Ok(uuid)
}

/// Reads an existing filesystem UUID without interpreting absence as blankness.
///
/// # Errors
/// Returns all open, seek, and read errors. `None` means only that the expected
/// primary signature is absent; the caller must not format on that evidence.
pub fn filesystem_uuid(path: &Path, role: VolumeRole) -> Result<Option<Uuid>> {
    let mut file = File::open(path)?;
    let (magic_offset, magic, uuid_offset): (u64, &[u8], u64) = match role {
        VolumeRole::Data => (0x10040, b"_BHRfS_M", 0x10020),
        VolumeRole::Metadata => (0x438, &[0x53, 0xef], 0x468),
    };
    file.seek(SeekFrom::Start(magic_offset))?;
    let mut bytes = [0_u8; 8];
    file.read_exact(&mut bytes[..magic.len()])?;
    if &bytes[..magic.len()] != magic {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(uuid_offset))?;
    let mut uuid = [0_u8; 16];
    file.read_exact(&mut uuid)?;
    Ok(Some(Uuid::from_bytes(uuid)))
}

pub fn write_provision_header(file: &mut File, uuid: Uuid) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(PROVISION_MAGIC)?;
    file.write_all(uuid.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Checks a newly created image's one-time provisioning identity.
///
/// # Errors
/// Returns an error when the header is unreadable or does not match the manifest.
pub fn verify_new_volume(path: &Path, uuid: Uuid) -> Result<()> {
    let mut bytes = [0_u8; 32];
    File::open(path)?.read_exact(&mut bytes)?;
    if &bytes[..16] != PROVISION_MAGIC || &bytes[16..] != uuid.as_bytes() {
        return Err(Error::RecoveryRequired(format!(
            "{} lacks its new-volume authorization; formatting is forbidden",
            path.display()
        )));
    }
    Ok(())
}
