//! Compatibility checks on an imported rootfs.
//!
//! A machine boots ArcBox's kernel and shim, which `exec`s the distro's
//! `/sbin/init`; that is the one thing a rootfs must provide, and it must
//! run on this host's architecture. `os-release` only names the image.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::error::{ImageError, Result};

/// Symlink hops followed before giving up (Linux's `MAXSYMLINKS`).
const MAX_HOPS: usize = 40;

/// What the checks need to know about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Node {
    Dir,
    File,
    Symlink(PathBuf),
    Other,
}

/// Read access to a rootfs being imported.
pub(super) trait RootfsView {
    /// The node at a relative path (`""` is the root), not following a
    /// final symlink.
    fn node(&self, path: &Path) -> Option<Node>;
    /// Up to `len` leading bytes of the regular file at `path`.
    fn head(&self, path: &Path, len: usize) -> Result<Vec<u8>>;
}

/// Resolves `path` inside the rootfs, following symlinks the way the guest
/// kernel would but never above the root. `None` when a component is
/// missing or the chain loops.
pub(super) fn resolve(view: &dyn RootfsView, path: &Path) -> Option<PathBuf> {
    let mut pending: Vec<PathBuf> = vec![path.to_path_buf()];
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(next) = pending.pop() {
        let mut components: Vec<_> = next.components().collect();
        components.reverse();
        while let Some(component) = components.pop() {
            match component {
                Component::RootDir => resolved = PathBuf::new(),
                Component::CurDir | Component::Prefix(_) => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(part) => {
                    let candidate = resolved.join(part);
                    match view.node(&candidate)? {
                        Node::Symlink(target) => {
                            hops += 1;
                            if hops > MAX_HOPS {
                                return None;
                            }
                            // Resolve the target, then whatever remained.
                            let rest: PathBuf = components.iter().rev().collect();
                            components.clear();
                            if !rest.as_os_str().is_empty() {
                                pending.push(rest);
                            }
                            if target.is_absolute() {
                                resolved = PathBuf::new();
                            }
                            pending.push(target);
                        }
                        _ => resolved = candidate,
                    }
                }
            }
        }
    }
    Some(resolved)
}

/// The ELF `e_machine` of a published image arch.
fn elf_machine(arch: &str) -> Option<u16> {
    match arch {
        "arm64" => Some(183),
        "amd64" => Some(62),
        _ => None,
    }
}

/// What the checks learned.
#[derive(Debug, Default)]
pub(super) struct Inspection {
    /// `ID` from os-release.
    pub id: Option<String>,
    /// `VERSION_ID`, else `VERSION_CODENAME`.
    pub release: Option<String>,
    /// `PRETTY_NAME`.
    pub pretty_name: Option<String>,
    /// Non-fatal findings for the user.
    pub warnings: Vec<String>,
}

/// Runs the checks for a rootfs that must boot on `arch`.
///
/// # Errors
///
/// Refuses a rootfs without a usable `/sbin/init` or whose init is built
/// for another architecture.
pub(super) fn inspect(view: &dyn RootfsView, arch: &str) -> Result<Inspection> {
    let mut inspection = Inspection::default();

    let init = resolve(view, Path::new("sbin/init")).filter(|p| view.node(p) == Some(Node::File));
    let Some(init) = init else {
        return Err(ImageError::image(
            "the root filesystem has no /sbin/init; install an init system \
             (systemd or openrc), or import with --add-init",
        ));
    };
    let head = view.head(&init, 20)?;
    if head.starts_with(b"#!") {
        inspection
            .warnings
            .push(format!("/sbin/init is a script (/{})", init.display()));
    } else if head.len() >= 20 && head.starts_with(b"\x7fELF") {
        let machine = u16::from_le_bytes([head[18], head[19]]);
        if let Some(expected) = elf_machine(arch)
            && machine != expected
        {
            return Err(ImageError::image(format!(
                "/sbin/init is built for another architecture (ELF machine {machine}); \
                 this Mac runs {arch} images"
            )));
        }
    } else {
        return Err(ImageError::image(format!(
            "/sbin/init (/{}) is neither an ELF binary nor a script",
            init.display()
        )));
    }

    let os_release = ["etc/os-release", "usr/lib/os-release"]
        .into_iter()
        .filter_map(|path| resolve(view, Path::new(path)))
        .find(|path| view.node(path) == Some(Node::File));
    match os_release {
        Some(path) => {
            let fields = parse_os_release(&String::from_utf8_lossy(&view.head(&path, 64 * 1024)?));
            inspection.id = fields.get("ID").cloned();
            inspection.release = fields
                .get("VERSION_ID")
                .or_else(|| fields.get("VERSION_CODENAME"))
                .cloned();
            inspection.pretty_name = fields.get("PRETTY_NAME").cloned();
        }
        None => inspection
            .warnings
            .push("no /etc/os-release; the image is labelled `custom`".to_owned()),
    }
    Ok(inspection)
}

/// Parses `KEY=value` / `KEY="value"` lines.
pub(super) fn parse_os_release(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_owned(), value.replace("\\\"", "\"")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        nodes: HashMap<PathBuf, Node>,
        files: HashMap<PathBuf, Vec<u8>>,
    }

    impl Fake {
        fn dir(mut self, path: &str) -> Self {
            self.nodes.insert(path.into(), Node::Dir);
            self
        }
        fn link(mut self, path: &str, target: &str) -> Self {
            self.nodes.insert(path.into(), Node::Symlink(target.into()));
            self
        }
        fn file(mut self, path: &str, data: &[u8]) -> Self {
            self.nodes.insert(path.into(), Node::File);
            self.files.insert(path.into(), data.to_vec());
            self
        }
    }

    impl RootfsView for Fake {
        fn node(&self, path: &Path) -> Option<Node> {
            if path.as_os_str().is_empty() {
                return Some(Node::Dir);
            }
            self.nodes.get(path).cloned()
        }
        fn head(&self, path: &Path, len: usize) -> Result<Vec<u8>> {
            let data = &self.files[path];
            Ok(data[..data.len().min(len)].to_vec())
        }
    }

    fn elf(machine: u16) -> Vec<u8> {
        let mut head = b"\x7fELF\x02\x01\x01".to_vec();
        head.resize(18, 0);
        head.extend_from_slice(&machine.to_le_bytes());
        head
    }

    #[test]
    fn merged_usr_and_relative_links_resolve_inside_the_root() {
        let fs = Fake::default()
            .link("sbin", "usr/sbin")
            .dir("usr")
            .dir("usr/sbin")
            .dir("usr/lib")
            .dir("usr/lib/systemd")
            .link("usr/sbin/init", "../lib/systemd/systemd")
            .file("usr/lib/systemd/systemd", &elf(183))
            .link("etc/os-release", "../usr/lib/os-release")
            .dir("etc")
            .file(
                "usr/lib/os-release",
                b"PRETTY_NAME=\"Ubuntu 24.04 LTS\"\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
            );
        assert_eq!(
            resolve(&fs, Path::new("sbin/init")),
            Some(PathBuf::from("usr/lib/systemd/systemd"))
        );
        let inspection = inspect(&fs, "arm64").unwrap();
        assert_eq!(inspection.id.as_deref(), Some("ubuntu"));
        assert_eq!(inspection.release.as_deref(), Some("24.04"));
        assert!(inspection.warnings.is_empty());
    }

    #[test]
    fn absolute_links_and_parent_escapes_stay_in_the_root() {
        let fs = Fake::default()
            .dir("bin")
            .file("bin/busybox", &elf(183))
            .dir("sbin")
            .link("sbin/init", "/../../bin/busybox");
        assert_eq!(
            resolve(&fs, Path::new("sbin/init")),
            Some(PathBuf::from("bin/busybox"))
        );
        let inspection = inspect(&fs, "arm64").unwrap();
        assert_eq!(inspection.id, None);
        assert_eq!(inspection.warnings.len(), 1);
    }

    #[test]
    fn missing_foreign_and_looping_inits_are_refused() {
        let missing = Fake::default().dir("sbin");
        assert!(
            inspect(&missing, "arm64")
                .unwrap_err()
                .to_string()
                .contains("--add-init")
        );

        let amd64 = Fake::default().dir("sbin").file("sbin/init", &elf(62));
        assert!(
            inspect(&amd64, "arm64")
                .unwrap_err()
                .to_string()
                .contains("another architecture")
        );

        let looping = Fake::default().dir("sbin").link("sbin/init", "init");
        assert!(resolve(&looping, Path::new("sbin/init")).is_none());
        assert!(inspect(&looping, "arm64").is_err());
    }

    #[test]
    fn os_release_values_unquote() {
        let fields =
            parse_os_release("# c\nID=alpine\nNAME='Alpine'\nPRETTY_NAME=\"A \\\"B\\\"\"\n");
        assert_eq!(fields["ID"], "alpine");
        assert_eq!(fields["NAME"], "Alpine");
        assert_eq!(fields["PRETTY_NAME"], "A \"B\"");
    }
}
