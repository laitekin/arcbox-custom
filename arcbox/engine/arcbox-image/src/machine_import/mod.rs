//! Importing a user's own root filesystem as a machine image.
//!
//! Every source becomes the same thing the published images are — an xz
//! squashfs plus a `manifest.json` — registered in the local image
//! registry under the user's name, so creating, booting, exporting and
//! removing a machine never needs to know where its image came from. The
//! machine still boots ArcBox's kernel and shim; the image only supplies
//! the root filesystem.
//!
//! An import is staged under `machine-images/.import-*` and renamed into
//! place only once the squashfs is written and hashed, so a failure or a
//! crash leaves no half-registered image.

mod inspect;
mod squashfs;
mod tree;

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{ImageError, Result};
use crate::machine_image::{
    MachineImage, MachineImageManager, MachineImageManifest, RootfsFile, UpstreamRef,
    host_image_arch,
};
use crate::remote_image::{StagingGuard, validate_name};

use inspect::{Node, RootfsView};

/// Largest root filesystem an import accepts by default (20 GiB).
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 20 << 30;

/// Upstream `server` recorded for imported images.
pub const LOCAL_UPSTREAM: &str = "local";

/// Variant recorded for imported images.
pub const CUSTOM_VARIANT: &str = "custom";

/// Where an import reads its root filesystem from.
pub enum ImportSource {
    /// A file: a tar archive (plain, gzip, xz or zstd) or a squashfs image,
    /// told apart by content, not by extension.
    File(PathBuf),
    /// An uncompressed tar stream (for example `docker export`), with a
    /// description recorded as its provenance.
    TarStream {
        reader: Box<dyn Read + Send>,
        description: String,
    },
}

/// An import request.
pub struct ImportRequest {
    /// The image's name; becomes its stream name in the registry.
    pub name: String,
    /// The root filesystem.
    pub source: ImportSource,
    /// Overrides the distro id read from os-release.
    pub distro: Option<String>,
    /// Overrides the release read from os-release.
    pub release: Option<String>,
    /// Refuses larger root filesystems.
    pub max_bytes: u64,
}

/// Import stages, reported with a byte count where one applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPhase {
    /// Reading the source; bytes read so far.
    Reading,
    /// Checking the root filesystem.
    Checking,
    /// Writing the squashfs.
    Writing,
    /// Hashing and registering; the image's size.
    Registering,
}

/// A finished import.
#[derive(Debug)]
pub struct ImportOutcome {
    /// The registered image.
    pub image: MachineImage,
    /// Non-fatal findings (dropped extended attributes, missing os-release).
    pub warnings: Vec<String>,
}

/// Whether a manifest describes an image imported on this Mac.
#[must_use]
pub fn is_imported(manifest: &MachineImageManifest) -> bool {
    manifest.upstream.server == LOCAL_UPSTREAM
}

/// An imported image's name: what a user can type and a directory can hold
/// without colliding with the registry's own `.`-prefixed staging names.
fn validate_image_name(name: &str) -> Result<()> {
    let valid = (1..=63).contains(&name.len())
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(ImageError::image(format!(
            "invalid image name '{name}': use 1–63 lowercase letters, digits, '.', '-' or '_', \
             starting with a letter or digit"
        )))
    }
}

/// The Dockerfile that installs an init system into `image`, choosing the
/// package manager the image has. Stock Docker images ship no init, and a
/// machine boots by exec'ing `/sbin/init`.
#[must_use]
pub fn init_dockerfile(image: &str) -> String {
    format!(
        r#"FROM {image}
RUN set -e; \
    if command -v apt-get >/dev/null; then \
        export DEBIAN_FRONTEND=noninteractive; \
        apt-get update; \
        apt-get install -y --no-install-recommends systemd systemd-sysv dbus iproute2 ifupdown isc-dhcp-client ca-certificates; \
        rm -rf /var/lib/apt/lists/*; \
    elif command -v apk >/dev/null; then \
        apk add --no-cache openrc busybox-openrc ifupdown-ng; \
        rc-update add networking boot; \
    elif command -v dnf >/dev/null; then \
        dnf install -y systemd iproute NetworkManager; dnf clean all; \
    elif command -v zypper >/dev/null; then \
        zypper --non-interactive install systemd iproute2 wicked; \
    elif command -v pacman >/dev/null; then \
        pacman -Sy --noconfirm systemd iproute2; \
    else \
        echo "no supported package manager to install an init system" >&2; exit 1; \
    fi; \
    test -e /sbin/init
"#
    )
}

/// Squashfs superblock magic (`hsqs`).
const SQUASHFS_MAGIC: &[u8] = b"hsqs";

enum Format {
    Squashfs,
    Tar,
    Gzip,
    Xz,
    Zstd,
}

fn sniff(path: &Path) -> Result<Format> {
    // Enough for the ext superblock magic at 1080.
    let mut head = Vec::with_capacity(1082);
    std::fs::File::open(path)?
        .take(1082)
        .read_to_end(&mut head)?;
    if let Some(kind) = disk_image_kind(&head) {
        return Err(ImageError::image(format!(
            "{} is a {kind}, which cannot be imported yet; import its root filesystem \
             as a tar archive instead (for example `virt-tar-out -a <disk> / rootfs.tar`)",
            path.display()
        )));
    }
    let magic = &head[..head.len().min(6)];
    Ok(if magic.starts_with(SQUASHFS_MAGIC) {
        Format::Squashfs
    } else if magic.starts_with(&[0x1f, 0x8b]) {
        Format::Gzip
    } else if magic.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Format::Xz
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Format::Zstd
    } else {
        Format::Tar
    })
}

/// What kind of disk image `head` (the first bytes of a file) starts, when
/// it is one: VM disk formats, a partitioned raw disk, or a bare ext
/// filesystem.
fn disk_image_kind(head: &[u8]) -> Option<&'static str> {
    let at = |offset: usize, magic: &[u8]| head.get(offset..offset + magic.len()) == Some(magic);
    if at(0, b"QFI\xfb") {
        return Some("qcow2 disk image");
    }
    if at(0, b"KDMV") || at(0, b"# Disk DescriptorFile") {
        return Some("VMDK disk image");
    }
    if at(0, b"vhdxfile") {
        return Some("VHDX disk image");
    }
    if at(64, &[0x7f, 0x10, 0xda, 0xbe]) {
        return Some("VDI disk image");
    }
    // A tar header carries `ustar` at 257 and zero padding at 500..512, so
    // these raw-disk checks do not trip on an archive.
    if at(257, b"ustar") {
        return None;
    }
    if at(512, b"EFI PART") || at(510, &[0x55, 0xaa]) {
        return Some("raw disk image");
    }
    if at(1080, &[0x53, 0xef]) {
        return Some("raw ext filesystem image");
    }
    None
}

fn open_tar(path: &Path, format: &Format) -> Result<Box<dyn Read + Send>> {
    let file = BufReader::new(std::fs::File::open(path)?);
    Ok(match format {
        Format::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Format::Xz => Box::new(liblzma::read::XzDecoder::new_multi_decoder(file)),
        Format::Zstd => Box::new(
            zstd::stream::read::Decoder::with_buffer(file)
                .map_err(|e| ImageError::image(format!("zstd: {e}")))?,
        ),
        Format::Tar | Format::Squashfs => Box::new(file),
    })
}

/// `local-YYYYMMDD_HHMMSS` in UTC.
fn local_version(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400).cast_signed();
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "local-{year:04}{month:02}{day:02}_{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// A sanitized identity field (os-release values are free text).
fn label(value: &str) -> String {
    let label: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if label.is_empty() {
        CUSTOM_VARIANT.to_owned()
    } else {
        label
    }
}

impl RootfsView for tree::Tree {
    fn node(&self, path: &Path) -> Option<Node> {
        self.entries.get(path).map(|entry| match &entry.kind {
            tree::Kind::Dir => Node::Dir,
            tree::Kind::File { .. } => Node::File,
            tree::Kind::Symlink(target) => Node::Symlink(target.clone()),
            _ => Node::Other,
        })
    }

    fn head(&self, path: &Path, len: usize) -> Result<Vec<u8>> {
        let Some(tree::Kind::File { spill, .. }) = self.entries.get(path).map(|e| &e.kind) else {
            return Err(ImageError::image(format!(
                "{} is not a file",
                path.display()
            )));
        };
        let mut head = Vec::with_capacity(len);
        std::fs::File::open(spill)?
            .take(len as u64)
            .read_to_end(&mut head)?;
        Ok(head)
    }
}

impl MachineImageManager {
    /// Imports a root filesystem as a machine image. Blocking: run it off
    /// the async runtime.
    ///
    /// # Errors
    ///
    /// Refuses an invalid name, a name already registered, an archive path
    /// escaping the root, a rootfs without a usable `/sbin/init`, one for
    /// another architecture, and one larger than `max_bytes`; propagates
    /// I/O failures. Nothing is registered on error.
    pub fn import(
        &self,
        request: ImportRequest,
        mut on_progress: impl FnMut(ImportPhase, u64),
    ) -> Result<ImportOutcome> {
        let ImportRequest {
            name,
            source,
            distro,
            release,
            max_bytes,
        } = request;
        validate_image_name(&name)?;
        if self.list().iter().any(|image| image.manifest.name == name) {
            return Err(ImageError::already_exists(format!(
                "machine image {name} (remove it first)"
            )));
        }
        let arch = host_image_arch();
        let version = local_version(std::time::SystemTime::now());
        let images_dir = self.images_dir();
        let staging = images_dir.join(format!(".import-{name}-{version}"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        let mut guard = StagingGuard::new(staging.clone());
        let rootfs_name = "rootfs.squashfs";
        let rootfs_path = staging.join(rootfs_name);

        let (inspection, product, mut warnings) = match source {
            ImportSource::File(path) => {
                let format = sniff(&path)?;
                let product = path
                    .file_name()
                    .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
                if matches!(format, Format::Squashfs) {
                    on_progress(ImportPhase::Checking, 0);
                    let view = squashfs::SquashfsView::open(&path)?;
                    let inspection = inspect::inspect(&view, arch)?;
                    let warnings = view.warnings();
                    drop(view);
                    on_progress(ImportPhase::Writing, 0);
                    std::fs::copy(&path, &rootfs_path)?;
                    (inspection, format!("squashfs:{product}"), warnings)
                } else {
                    let reader = open_tar(&path, &format)?;
                    self.import_tar(
                        reader,
                        &staging,
                        &rootfs_path,
                        arch,
                        max_bytes,
                        &mut on_progress,
                    )
                    .map(|(i, w)| (i, format!("archive:{product}"), w))?
                }
            }
            ImportSource::TarStream {
                reader,
                description,
            } => self
                .import_tar(
                    reader,
                    &staging,
                    &rootfs_path,
                    arch,
                    max_bytes,
                    &mut on_progress,
                )
                .map(|(i, w)| (i, description, w))?,
        };
        warnings.extend(inspection.warnings.iter().cloned());

        let size = std::fs::metadata(&rootfs_path)?.len();
        if size > max_bytes {
            return Err(ImageError::image(format!(
                "the image is {size} bytes, over the {max_bytes}-byte import limit"
            )));
        }
        on_progress(ImportPhase::Registering, size);
        let sha256 = sha256_file(&rootfs_path)?;
        let distro = label(&distro.or(inspection.id).unwrap_or_default());
        let release = label(&release.or(inspection.release).unwrap_or_default());
        let manifest = MachineImageManifest {
            schema_version: 1,
            name: name.clone(),
            version: version.clone(),
            release_title: inspection.pretty_name.unwrap_or_else(|| release.clone()),
            distro,
            release,
            arch: arch.to_owned(),
            variant: CUSTOM_VARIANT.to_owned(),
            upstream: UpstreamRef {
                server: LOCAL_UPSTREAM.to_owned(),
                product,
                version: version.clone(),
            },
            rootfs: RootfsFile {
                path: rootfs_name.to_owned(),
                format: "squashfs".to_owned(),
                size,
                sha256,
            },
        };
        self.land(&staging, &manifest)?;
        guard.disarm();
        Ok(ImportOutcome {
            image: self.get(&name, &version)?,
            warnings,
        })
    }

    fn import_tar(
        &self,
        reader: Box<dyn Read + Send>,
        staging: &Path,
        rootfs_path: &Path,
        arch: &str,
        max_bytes: u64,
        on_progress: &mut impl FnMut(ImportPhase, u64),
    ) -> Result<(inspect::Inspection, Vec<String>)> {
        let spill = staging.join("spill");
        let tree = tree::Tree::read_tar(reader, &spill, max_bytes, |bytes| {
            on_progress(ImportPhase::Reading, bytes);
        })?;
        on_progress(ImportPhase::Checking, tree.content_bytes);
        let inspection = inspect::inspect(&tree, arch)?;
        on_progress(ImportPhase::Writing, tree.content_bytes);
        tree.write_squashfs(rootfs_path)?;
        let mut warnings = Vec::new();
        if !tree.dropped_xattrs.is_empty() {
            let sample: Vec<String> = tree
                .dropped_xattrs
                .iter()
                .take(5)
                .map(|p| format!("/{}", p.display()))
                .collect();
            warnings.push(format!(
                "extended attributes on {} files were not kept (e.g. {}); file \
                 capabilities such as ping's need to be re-applied in the machine",
                tree.dropped_xattrs.len(),
                sample.join(", ")
            ));
        }
        drop(tree);
        std::fs::remove_dir_all(&spill)?;
        Ok((inspection, warnings))
    }

    /// Writes `manifest` into `staging` and renames it into the registry.
    fn land(&self, staging: &Path, manifest: &MachineImageManifest) -> Result<()> {
        let mut json = serde_json::to_string_pretty(manifest)
            .map_err(|e| ImageError::image(format!("serialize manifest: {e}")))?;
        json.push('\n');
        std::fs::write(staging.join("manifest.json"), json)?;
        let final_dir = self
            .images_dir()
            .join(&manifest.name)
            .join(&manifest.version);
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if final_dir.exists() {
            return Err(ImageError::already_exists(format!(
                "machine image {}@{}",
                manifest.name, manifest.version
            )));
        }
        std::fs::rename(staging, &final_dir)?;
        Ok(())
    }

    /// Registers an image directory carried by a machine archive (a
    /// `manifest.json` and its rootfs), verifying the rootfs against the
    /// manifest. A version already present is left as is.
    ///
    /// # Errors
    ///
    /// Refuses a manifest naming a path outside the directory, a rootfs
    /// whose size or digest does not match, or an image for another
    /// architecture.
    pub fn register_carried(&self, source_dir: &Path) -> Result<MachineImage> {
        let manifest: MachineImageManifest =
            serde_json::from_str(&std::fs::read_to_string(source_dir.join("manifest.json"))?)
                .map_err(|e| ImageError::image(format!("carried image manifest: {e}")))?;
        validate_name(&manifest.name)?;
        validate_name(&manifest.version)?;
        validate_name(&manifest.rootfs.path)?;
        if let Ok(existing) = self.get(&manifest.name, &manifest.version) {
            return Ok(existing);
        }
        if manifest.arch != host_image_arch() {
            return Err(ImageError::image(format!(
                "the carried image is for {}; this Mac runs {}",
                manifest.arch,
                host_image_arch()
            )));
        }
        let rootfs = source_dir.join(&manifest.rootfs.path);
        if std::fs::metadata(&rootfs)?.len() != manifest.rootfs.size
            || sha256_file(&rootfs)? != manifest.rootfs.sha256
        {
            return Err(ImageError::image(format!(
                "the carried image {}@{} does not match its manifest",
                manifest.name, manifest.version
            )));
        }
        let staging = self
            .images_dir()
            .join(format!(".carry-{}-{}", manifest.name, manifest.version));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        let mut guard = StagingGuard::new(staging.clone());
        std::fs::copy(&rootfs, staging.join(&manifest.rootfs.path))?;
        self.land(&staging, &manifest)?;
        guard.disarm();
        self.get(&manifest.name, &manifest.version)
    }

    /// Removes a registered image. `in_use` lists the rootfs paths machines
    /// boot from; an image one of them uses is refused.
    ///
    /// # Errors
    ///
    /// Returns not-found for an unknown image and a precondition error for
    /// one in use.
    pub fn remove(&self, name: &str, version: &str, in_use: &[PathBuf]) -> Result<()> {
        let image = self.get(name, version)?;
        let rootfs = image.rootfs_path();
        if in_use.iter().any(|path| path == &rootfs) {
            return Err(ImageError::invalid_state(format!(
                "machine image {name}@{version} is used by a machine; remove the machine first"
            )));
        }
        // Rename first so a half-deleted directory is never listed.
        let doomed = self.images_dir().join(format!(".remove-{name}-{version}"));
        if doomed.exists() {
            std::fs::remove_dir_all(&doomed)?;
        }
        std::fs::rename(&image.dir, &doomed)?;
        std::fs::remove_dir_all(&doomed)?;
        let stream = self.images_dir().join(name);
        if std::fs::read_dir(&stream).is_ok_and(|mut entries| entries.next().is_none()) {
            let _ = std::fs::remove_dir(&stream);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
