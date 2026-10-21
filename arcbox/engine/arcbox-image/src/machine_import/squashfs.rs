//! An existing squashfs image, read for the import checks.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use backhand::traits::types::Compressor;
use backhand::{FilesystemReader, InnerNode};

use super::inspect::{Node, RootfsView};
use crate::error::{ImageError, Result};

/// Compressions the ArcBox kernel is known to mount (the published images
/// are xz).
const KNOWN_GOOD: &[Compressor] = &[Compressor::Xz, Compressor::Gzip];

pub(super) struct SquashfsView {
    fs: FilesystemReader<'static>,
}

impl SquashfsView {
    pub fn open(path: &Path) -> Result<Self> {
        let file = BufReader::new(std::fs::File::open(path)?);
        let fs = FilesystemReader::from_reader(file).map_err(|e| {
            ImageError::image(format!("{} is not a usable squashfs: {e}", path.display()))
        })?;
        Ok(Self { fs })
    }

    pub fn warnings(&self) -> Vec<String> {
        let compressor = self.fs.compressor.unwrap_or_default();
        if KNOWN_GOOD.contains(&compressor) {
            Vec::new()
        } else {
            vec![format!(
                "the squashfs uses {compressor:?} compression; the machine kernel is only known \
                 to mount xz and gzip images"
            )]
        }
    }

    fn find(&self, path: &Path) -> Option<&backhand::Node<backhand::SquashfsFileReader>> {
        let absolute = Path::new("/").join(path);
        self.fs.files().find(|node| node.fullpath == absolute)
    }
}

impl RootfsView for SquashfsView {
    fn node(&self, path: &Path) -> Option<Node> {
        if path.as_os_str().is_empty() {
            return Some(Node::Dir);
        }
        self.find(path).map(|node| match &node.inner {
            InnerNode::Dir(_) => Node::Dir,
            InnerNode::File(_) => Node::File,
            InnerNode::Symlink(link) => Node::Symlink(PathBuf::from(&link.link)),
            _ => Node::Other,
        })
    }

    fn head(&self, path: &Path, len: usize) -> Result<Vec<u8>> {
        let Some(InnerNode::File(file)) = self.find(path).map(|node| &node.inner) else {
            return Err(ImageError::image(format!(
                "{} is not a file",
                path.display()
            )));
        };
        let mut head = Vec::with_capacity(len);
        self.fs
            .file(file)
            .reader()
            .take(len as u64)
            .read_to_end(&mut head)
            .map_err(|e| ImageError::image(format!("read {}: {e}", path.display())))?;
        Ok(head)
    }
}
