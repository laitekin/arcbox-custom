use std::io::Write as _;
use std::path::Path;

use super::*;
use crate::remote_image::RemoteLocation;

/// An ELF header for this host's image architecture.
fn elf() -> Vec<u8> {
    let machine: u16 = if host_image_arch() == "arm64" {
        183
    } else {
        62
    };
    let mut bytes = b"\x7fELF\x02\x01\x01".to_vec();
    bytes.resize(18, 0);
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&[0; 44]);
    bytes
}

struct Archive(tar::Builder<Vec<u8>>);

impl Archive {
    fn new() -> Self {
        Self(tar::Builder::new(Vec::new()))
    }

    fn header(kind: tar::EntryType, mode: u32, size: u64) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(1_700_000_000);
        header.set_size(size);
        header
    }

    fn dir(mut self, path: &str) -> Self {
        let mut h = Self::header(tar::EntryType::Directory, 0o755, 0);
        self.0.append_data(&mut h, path, std::io::empty()).unwrap();
        self
    }

    fn file(mut self, path: &str, mode: u32, data: &[u8]) -> Self {
        let mut h = Self::header(tar::EntryType::Regular, mode, data.len() as u64);
        self.0.append_data(&mut h, path, data).unwrap();
        self
    }

    fn link(mut self, kind: tar::EntryType, path: &str, target: &str) -> Self {
        let mut h = Self::header(kind, 0o777, 0);
        self.0.append_link(&mut h, path, target).unwrap();
        self
    }

    fn device(mut self, kind: tar::EntryType, path: &str, major: u32, minor: u32) -> Self {
        let mut h = Self::header(kind, 0o666, 0);
        h.set_device_major(major).unwrap();
        h.set_device_minor(minor).unwrap();
        self.0.append_data(&mut h, path, std::io::empty()).unwrap();
        self
    }

    /// Attaches an xattr record to the next entry.
    fn with_xattr(mut self) -> Self {
        self.0
            .append_pax_extensions([("SCHILY.xattr.security.capability", &b"\x01"[..])])
            .unwrap();
        self
    }

    /// A merged-usr rootfs with systemd-style init and os-release links.
    fn rootfs() -> Self {
        Self::new()
            .dir("./")
            .dir("./usr/")
            .dir("./usr/lib/")
            .dir("./usr/lib/systemd/")
            .dir("./usr/sbin/")
            .dir("./usr/bin/")
            .dir("./etc/")
            .dir("./dev/")
            .link(tar::EntryType::Symlink, "./sbin", "usr/sbin")
            .file("./usr/lib/systemd/systemd", 0o755, &elf())
            .link(
                tar::EntryType::Symlink,
                "./usr/sbin/init",
                "../lib/systemd/systemd",
            )
            .file(
                "./usr/lib/os-release",
                0o644,
                b"PRETTY_NAME=\"Test Linux 1.0\"\nID=testlinux\nVERSION_ID=1.0\n",
            )
            .link(
                tar::EntryType::Symlink,
                "./etc/os-release",
                "../usr/lib/os-release",
            )
            .with_xattr()
            .file("./usr/bin/su", 0o4755, b"setuid")
            .file("./usr/bin/ping", 0o755, b"ping")
            .link(tar::EntryType::Link, "./usr/bin/ping6", "./usr/bin/ping")
            .device(tar::EntryType::Char, "./dev/null", 1, 3)
            .device(tar::EntryType::Fifo, "./dev/initctl", 0, 0)
    }

    fn finish(self) -> Vec<u8> {
        self.0.into_inner().unwrap()
    }

    fn write_gz(self, path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        gz.write_all(&self.finish()).unwrap();
        gz.finish().unwrap();
    }
}

fn manager(dir: &Path) -> MachineImageManager {
    MachineImageManager::with_base(dir, RemoteLocation::parse("/nonexistent"))
}

fn request(name: &str, source: ImportSource) -> ImportRequest {
    ImportRequest {
        name: name.to_owned(),
        source,
        distro: None,
        release: None,
        max_bytes: DEFAULT_MAX_IMPORT_BYTES,
    }
}

fn leftovers(manager: &MachineImageManager) -> Vec<String> {
    std::fs::read_dir(manager.images_dir())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_compressed_rootfs_tarball_imports_with_linux_metadata_intact() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("rootfs.tar.gz");
    Archive::rootfs().write_gz(&archive);
    let manager = manager(dir.path());

    let mut phases = Vec::new();
    let outcome = manager
        .import(
            request("my-linux", ImportSource::File(archive)),
            |phase, _| {
                if phases.last() != Some(&phase) {
                    phases.push(phase);
                }
            },
        )
        .unwrap();
    assert_eq!(
        phases,
        [
            ImportPhase::Reading,
            ImportPhase::Checking,
            ImportPhase::Writing,
            ImportPhase::Registering
        ]
    );

    let manifest = &outcome.image.manifest;
    assert_eq!(manifest.name, "my-linux");
    assert!(manifest.version.starts_with("local-"));
    assert_eq!(manifest.distro, "testlinux");
    assert_eq!(manifest.release, "1.0");
    assert_eq!(manifest.release_title, "Test Linux 1.0");
    assert_eq!(manifest.arch, host_image_arch());
    assert_eq!(manifest.rootfs.format, "squashfs");
    assert!(is_imported(manifest));
    assert_eq!(
        sha256_file(&outcome.image.rootfs_path()).unwrap(),
        manifest.rootfs.sha256
    );
    assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
    assert!(outcome.warnings[0].contains("/usr/bin/su"));
    assert_eq!(manager.list().len(), 1);
    assert!(leftovers(&manager).is_empty());

    // The squashfs reads back through the import's own checks.
    let view = squashfs::SquashfsView::open(&outcome.image.rootfs_path()).unwrap();
    assert!(view.warnings().is_empty());
    assert_eq!(
        inspect::inspect(&view, host_image_arch())
            .unwrap()
            .id
            .as_deref(),
        Some("testlinux")
    );

    // And through the reference implementation, when it is installed.
    let Ok(listing) = std::process::Command::new("unsquashfs")
        .args(["-lln", "-n"])
        .arg(outcome.image.rootfs_path())
        .output()
    else {
        return;
    };
    let listing = String::from_utf8_lossy(&listing.stdout);
    let line = |suffix: &str| {
        listing
            .lines()
            .find(|l| l.ends_with(suffix))
            .unwrap_or_else(|| panic!("{suffix} missing from:\n{listing}"))
            .to_owned()
    };
    assert!(
        line("/usr/bin/su").starts_with("-rwsr-xr-x 0/0"),
        "{listing}"
    );
    assert!(line("/dev/null").starts_with("crw-rw-rw- 0/0"), "{listing}");
    assert!(
        line("/dev/null").contains("1,  3") || line("/dev/null").contains("1, 3"),
        "{listing}"
    );
    assert!(line("/dev/initctl").starts_with('p'), "{listing}");
    assert!(line("/sbin -> usr/sbin").starts_with('l'), "{listing}");
    assert!(
        line("/usr/bin/ping6").starts_with("-rwxr-xr-x"),
        "{listing}"
    );
}

#[test]
fn an_existing_squashfs_imports_as_is_under_a_new_name() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("rootfs.tar.gz");
    Archive::rootfs().write_gz(&archive);
    let manager = manager(dir.path());
    let first = manager
        .import(request("first", ImportSource::File(archive)), |_, _| {})
        .unwrap();

    let copy = dir.path().join("image.squashfs");
    std::fs::copy(first.image.rootfs_path(), &copy).unwrap();
    let second = manager
        .import(request("second", ImportSource::File(copy)), |_, _| {})
        .unwrap();
    assert_eq!(
        second.image.manifest.rootfs.sha256,
        first.image.manifest.rootfs.sha256
    );
    assert!(
        second
            .image
            .manifest
            .upstream
            .product
            .starts_with("squashfs:")
    );
}

#[test]
fn unsafe_or_unbootable_archives_are_refused_without_leftovers() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let cases: [(&str, Vec<u8>, &str); 3] = [
        (
            "escape",
            {
                // tar::Builder refuses `..` itself, so write the name raw.
                let mut header = Archive::header(tar::EntryType::Regular, 0o644, 1);
                header.as_old_mut().name[..9].copy_from_slice(b"../evil\0\0");
                header.set_cksum();
                let mut builder = tar::Builder::new(Vec::new());
                builder.append(&header, &b"x"[..]).unwrap();
                builder.into_inner().unwrap()
            },
            "escapes the root",
        ),
        (
            "noinit",
            Archive::new()
                .dir("./etc/")
                .file("./etc/hostname", 0o644, b"x")
                .finish(),
            "no /sbin/init",
        ),
        (
            "dangling",
            Archive::new()
                .dir("./sbin/")
                .link(tar::EntryType::Link, "./sbin/init", "./bin/missing")
                .finish(),
            "not a file in the archive",
        ),
    ];
    for (name, bytes, expected) in cases {
        let path = dir.path().join(format!("{name}.tar"));
        std::fs::write(&path, bytes).unwrap();
        let error = manager
            .import(request(name, ImportSource::File(path)), |_, _| {})
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{name}: {error}");
    }
    assert!(manager.list().is_empty());
    assert!(leftovers(&manager).is_empty(), "{:?}", leftovers(&manager));
}

#[test]
fn names_are_validated_and_unique() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let tar = Archive::rootfs().finish();
    for bad in [".hidden", "Upper", "a/b", "", "with space"] {
        let stream = ImportSource::TarStream {
            reader: Box::new(std::io::Cursor::new(tar.clone())),
            description: "test".into(),
        };
        assert!(
            manager.import(request(bad, stream), |_, _| {}).is_err(),
            "{bad:?}"
        );
    }
    let stream = || ImportSource::TarStream {
        reader: Box::new(std::io::Cursor::new(tar.clone())),
        description: "docker:test".into(),
    };
    let image = manager.import(request("dup", stream()), |_, _| {}).unwrap();
    assert_eq!(image.image.manifest.upstream.product, "docker:test");
    let error = manager
        .import(request("dup", stream()), |_, _| {})
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn the_size_limit_is_enforced_while_reading() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let mut req = request(
        "big",
        ImportSource::TarStream {
            reader: Box::new(std::io::Cursor::new(Archive::rootfs().finish())),
            description: "test".into(),
        },
    );
    req.max_bytes = 16;
    assert!(
        manager
            .import(req, |_, _| {})
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    assert!(leftovers(&manager).is_empty());
}

#[test]
fn removal_refuses_images_in_use_and_carried_images_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let stream = ImportSource::TarStream {
        reader: Box::new(std::io::Cursor::new(Archive::rootfs().finish())),
        description: "test".into(),
    };
    let image = manager
        .import(request("carry", stream), |_, _| {})
        .unwrap()
        .image;
    let (name, version) = (image.manifest.name.clone(), image.manifest.version.clone());

    let error = manager
        .remove(&name, &version, &[image.rootfs_path()])
        .unwrap_err();
    assert!(error.to_string().contains("used by a machine"), "{error}");

    // Carry it out, remove it, and register it again from the copy.
    let carried = dir.path().join("carried");
    std::fs::create_dir(&carried).unwrap();
    for file in ["manifest.json", "rootfs.squashfs"] {
        std::fs::copy(image.dir.join(file), carried.join(file)).unwrap();
    }
    manager.remove(&name, &version, &[]).unwrap();
    assert!(manager.list().is_empty());
    assert!(!manager.images_dir().join(&name).exists());

    let back = manager.register_carried(&carried).unwrap();
    assert_eq!(back.manifest.rootfs.sha256, image.manifest.rootfs.sha256);
    // Registering again is a no-op.
    manager.register_carried(&carried).unwrap();

    // A tampered rootfs is refused.
    manager.remove(&name, &version, &[]).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(carried.join("rootfs.squashfs"))
        .unwrap()
        .write_all(b"x")
        .unwrap();
    assert!(manager.register_carried(&carried).is_err());
    assert!(leftovers(&manager).is_empty());
}

#[test]
fn local_versions_are_utc_timestamps() {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_603_845);
    assert_eq!(local_version(at), "local-20261021_173045");
    assert_eq!(
        local_version(std::time::UNIX_EPOCH),
        "local-19700101_000000"
    );
}

#[test]
fn the_init_dockerfile_derives_from_the_source_and_requires_an_init() {
    let dockerfile = init_dockerfile("ubuntu:24.04");
    assert!(dockerfile.starts_with("FROM ubuntu:24.04\n"));
    assert!(dockerfile.contains("systemd-sysv"));
    assert!(dockerfile.contains("openrc"));
    assert!(dockerfile.trim_end().ends_with("test -e /sbin/init"));
}
