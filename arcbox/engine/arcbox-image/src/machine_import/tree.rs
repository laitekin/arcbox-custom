//! A rootfs read from a tar stream, as an in-memory tree of Linux metadata
//! with file contents spilled to disk, and its squashfs rendering.
//!
//! The tree never touches the host filesystem's own metadata: owners,
//! modes, device nodes and links come from the tar headers and go straight
//! into the squashfs, because a macOS directory can hold neither root-owned
//! files nor device nodes.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use backhand::compression::Compressor;
use backhand::{FilesystemCompressor, FilesystemWriter, NodeHeader};

use crate::error::{ImageError, Result};

/// Squashfs block size; matches the published images.
const BLOCK_SIZE: u32 = 1 << 20;

/// What a path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Kind {
    Dir,
    /// Regular file, its contents in the spill file.
    File {
        spill: PathBuf,
        size: u64,
    },
    Symlink(PathBuf),
    CharDevice(u32),
    BlockDevice(u32),
    Fifo,
}

/// One path's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub kind: Kind,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
}

impl Entry {
    fn implicit_dir() -> Self {
        Self {
            kind: Kind::Dir,
            mode: 0o755,
            uid: 0,
            gid: 0,
            mtime: 0,
        }
    }

    fn header(&self) -> NodeHeader {
        NodeHeader::new(self.mode, self.uid, self.gid, self.mtime)
    }
}

/// A rootfs: relative paths (no leading `/`; the root is `""`) to entries,
/// in an order that puts every directory before its contents.
#[derive(Debug, Default)]
pub(super) struct Tree {
    pub entries: BTreeMap<PathBuf, Entry>,
    /// Paths that carried extended attributes the squashfs cannot keep.
    pub dropped_xattrs: Vec<PathBuf>,
    /// Total regular-file bytes.
    pub content_bytes: u64,
}

/// Normalizes a tar path to a relative rootfs path, refusing anything that
/// would escape the root.
pub(super) fn normalize(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(ImageError::image(format!(
                    "archive path {} escapes the root filesystem",
                    path.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Linux `new_encode_dev`, the device number encoding squashfs stores.
fn encode_dev(major: u32, minor: u32) -> u32 {
    (minor & 0xff) | ((major & 0xfff) << 8) | ((minor & !0xff) << 12)
}

impl Tree {
    /// Reads a tar stream, spilling file contents under `spill_dir` and
    /// refusing more than `max_bytes` of content. `on_bytes` receives the
    /// running content total.
    pub fn read_tar(
        reader: impl Read,
        spill_dir: &Path,
        max_bytes: u64,
        mut on_bytes: impl FnMut(u64),
    ) -> Result<Self> {
        std::fs::create_dir_all(spill_dir)?;
        let mut tree = Self::default();
        let mut hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut archive = tar::Archive::new(reader);
        let mut spill_index = 0u64;

        for entry in archive
            .entries()
            .map_err(|e| ImageError::image(format!("read archive: {e}")))?
        {
            let mut entry = entry.map_err(|e| ImageError::image(format!("read archive: {e}")))?;
            let path = normalize(
                &entry
                    .path()
                    .map_err(|e| ImageError::image(format!("archive path: {e}")))?,
            )?;
            let header = entry.header().clone();
            let meta = Entry {
                kind: Kind::Dir,
                mode: (header.mode().unwrap_or(0o644) & 0o7777) as u16,
                uid: header.uid().unwrap_or(0) as u32,
                gid: header.gid().unwrap_or(0) as u32,
                mtime: header.mtime().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
            };
            let entry_type = header.entry_type();
            let link_name = entry
                .link_name()
                .map_err(|e| ImageError::image(format!("archive link: {e}")))?
                .map(|link| link.into_owned());
            let device = (
                header.device_major().ok().flatten().unwrap_or(0),
                header.device_minor().ok().flatten().unwrap_or(0),
            );

            if let Ok(Some(extensions)) = entry.pax_extensions()
                && extensions
                    .flatten()
                    .any(|ext| ext.key().is_ok_and(|key| key.starts_with("SCHILY.xattr.")))
            {
                tree.dropped_xattrs.push(path.clone());
            }

            let kind = match entry_type {
                tar::EntryType::Directory => Kind::Dir,
                tar::EntryType::Regular | tar::EntryType::Continuous => {
                    let size = header.size().unwrap_or(0);
                    tree.content_bytes = tree.content_bytes.saturating_add(size);
                    if tree.content_bytes > max_bytes {
                        return Err(ImageError::image(format!(
                            "the root filesystem exceeds the {max_bytes}-byte import limit"
                        )));
                    }
                    spill_index += 1;
                    let spill = spill_dir.join(spill_index.to_string());
                    let mut file = std::fs::File::create(&spill)?;
                    std::io::copy(&mut entry, &mut file)
                        .map_err(|e| ImageError::image(format!("read {}: {e}", path.display())))?;
                    file.flush()?;
                    on_bytes(tree.content_bytes);
                    Kind::File { spill, size }
                }
                tar::EntryType::Symlink => Kind::Symlink(link_name.ok_or_else(|| {
                    ImageError::image(format!("symlink {} has no target", path.display()))
                })?),
                tar::EntryType::Link => {
                    let target = normalize(&link_name.ok_or_else(|| {
                        ImageError::image(format!("hard link {} has no target", path.display()))
                    })?)?;
                    hard_links.push((path, target));
                    continue;
                }
                tar::EntryType::Char => Kind::CharDevice(encode_dev(device.0, device.1)),
                tar::EntryType::Block => Kind::BlockDevice(encode_dev(device.0, device.1)),
                tar::EntryType::Fifo => Kind::Fifo,
                // PAX/GNU metadata records are consumed by the tar reader;
                // anything else carries no file.
                _ => continue,
            };
            tree.entries.insert(path, Entry { kind, ..meta });
        }

        // A hard link is its target's inode: same contents, same owner and
        // mode. Squashfs deduplicates the data, so it costs nothing to store
        // it as a second file.
        for (path, target) in hard_links {
            let entry = match tree.entries.get(&target) {
                Some(
                    entry @ Entry {
                        kind: Kind::File { .. },
                        ..
                    },
                ) => entry.clone(),
                _ => {
                    return Err(ImageError::image(format!(
                        "hard link {} points at {}, which is not a file in the archive",
                        path.display(),
                        target.display()
                    )));
                }
            };
            tree.entries.insert(path, entry);
        }
        tree.fill_parents();
        Ok(tree)
    }

    /// Adds a default directory for every missing ancestor, root included.
    fn fill_parents(&mut self) {
        let mut missing = Vec::new();
        for path in self.entries.keys() {
            for ancestor in path.ancestors().skip(1) {
                if !self.entries.contains_key(ancestor) {
                    missing.push(ancestor.to_path_buf());
                }
            }
        }
        missing.push(PathBuf::new());
        for path in missing {
            self.entries.entry(path).or_insert_with(Entry::implicit_dir);
        }
    }

    /// Renders the tree as an xz squashfs into `out`.
    pub fn write_squashfs(&self, out: &Path) -> Result<()> {
        let mut fs = FilesystemWriter::default();
        fs.set_compressor(
            FilesystemCompressor::new(Compressor::Xz, None)
                .map_err(|e| ImageError::image(format!("squashfs compressor: {e}")))?,
        );
        fs.set_block_size(BLOCK_SIZE);
        let root = self
            .entries
            .get(Path::new(""))
            .cloned()
            .unwrap_or_else(Entry::implicit_dir);
        fs.set_root_mode(root.mode);
        fs.set_root_uid(root.uid);
        fs.set_root_gid(root.gid);
        fs.set_time(root.mtime);

        let error = |path: &Path, e: backhand::BackhandError| {
            ImageError::image(format!("squashfs {}: {e}", path.display()))
        };
        for (path, entry) in &self.entries {
            if path.as_os_str().is_empty() {
                continue;
            }
            let at = Path::new("/").join(path);
            let header = entry.header();
            match &entry.kind {
                Kind::Dir => fs.push_dir(&at, header),
                Kind::File { spill, .. } => fs.push_file_from_path(spill.clone(), &at, header),
                Kind::Symlink(target) => fs.push_symlink(target.clone(), &at, header),
                Kind::CharDevice(dev) => fs.push_char_device(*dev, &at, header),
                Kind::BlockDevice(dev) => fs.push_block_device(*dev, &at, header),
                Kind::Fifo => fs.push_fifo(&at, header),
            }
            .map_err(|e| error(path, e))?;
        }

        let file = std::fs::File::create(out)?;
        let mut writer = std::io::BufWriter::new(file);
        fs.write(&mut writer)
            .map_err(|e| ImageError::image(format!("write squashfs: {e}")))?;
        writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_normalize_and_refuse_escapes() {
        assert_eq!(
            normalize(Path::new("./etc/passwd")).unwrap(),
            Path::new("etc/passwd")
        );
        assert_eq!(
            normalize(Path::new("/usr/bin")).unwrap(),
            Path::new("usr/bin")
        );
        assert_eq!(normalize(Path::new("./")).unwrap(), Path::new(""));
        assert!(normalize(Path::new("../etc")).is_err());
        assert!(normalize(Path::new("etc/../../x")).is_err());
    }

    #[test]
    fn device_numbers_use_the_linux_encoding() {
        assert_eq!(encode_dev(1, 3), 0x103); // /dev/null
        assert_eq!(
            encode_dev(259, 0x12345),
            (0x45) | (259 << 8) | (0x12300 << 12)
        );
    }
}
