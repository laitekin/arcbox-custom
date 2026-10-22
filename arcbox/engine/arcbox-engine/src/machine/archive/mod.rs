//! The machine archive `export` writes and `import` reads.
//!
//! A zstd-compressed tar: `manifest.json` first — the machine's settings and
//! the published image it boots, so an archive says what it needs before
//! its bulk is read — then `data.img`, the machine's data disk, as a PAX
//! sparse entry (GNU sparse format 1.0). A data disk is almost all holes;
//! the sparse entry stores only the extents the guest wrote, and every tar
//! that reads the format (GNU tar, bsdtar) restores the holes. The rootfs is
//! normally not in the archive: it is the published image the manifest
//! names, which import requires to be in the local registry. An archive
//! written with the image (`image_included`) also carries the registry
//! directory — `image/manifest.json` and the rootfs — after the data disk,
//! so a machine booting an image no CDN publishes (one imported on the Mac
//! that exported it) can be restored on another.
//!
//! [`write`](write::write) builds an archive, [`read_manifest`],
//! [`extract_data_disk`] and [`extract_image`] read one back.

mod read;
#[cfg(test)]
mod tests;
mod write;

use arcbox_image::machine_image::MachineImageManifest;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::MachineMount;

pub use read::{extract_data_disk, extract_image, read_manifest};
pub use write::write;

/// The archive format this build writes, and the only one it reads.
pub const FORMAT_VERSION: u32 = 1;
const MANIFEST_ENTRY: &str = "manifest.json";
const DATA_DISK_ENTRY: &str = "data.img";
/// Directory holding a carried image's registry files.
const IMAGE_DIR_ENTRY: &str = "image";
const TAR_BLOCK: usize = 512;

/// What the archive says about itself: the first entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifest {
    /// [`FORMAT_VERSION`] of the writer.
    pub format_version: u32,
    /// The ArcBox version that wrote the archive.
    pub arcbox_version: String,
    /// When the archive was written.
    pub exported_at: DateTime<Utc>,
    /// The machine's own settings.
    pub machine: ArchivedMachine,
    /// The image the machine boots: the rootfs under its data disk, which
    /// import must find in the local registry unless the archive carries it.
    pub image: MachineImageManifest,
    /// The archive carries the image under `image/`. Absent in archives
    /// written before the field existed.
    #[serde(default)]
    pub image_included: bool,
}

/// The settings a machine is recreated with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMachine {
    /// Name at export time; import may choose another.
    pub name: String,
    /// vCPUs.
    pub cpus: u32,
    /// Memory in MiB.
    pub memory_mb: u64,
    /// Data disk size in GiB.
    pub disk_gb: u64,
    /// Distro id (`ubuntu`).
    pub distro: String,
    /// Distro release (`noble`).
    #[serde(default)]
    pub distro_version: Option<String>,
    /// Host directories shared into the machine. Host paths, so they are
    /// re-validated on import.
    #[serde(default)]
    pub mounts: Vec<MachineMount>,
    /// The machine is isolated from the Mac; an import keeps it so. Absent
    /// in archives written before the field existed.
    #[serde(default)]
    pub isolated: bool,
}
