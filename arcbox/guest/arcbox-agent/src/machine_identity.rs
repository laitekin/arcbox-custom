//! What a distro machine is told about itself before its own init runs:
//! its name, which NIC is not its to manage, and — when the host sets them
//! — its machine ID, time zone and locale.
//!
//! Both are files the boot shim's `machine-init` writes into the distro's
//! overlay root, so they are in place when the distro's init starts and
//! reads them. The hostname goes three ways because every init in scope
//! reads a different one: the kernel's nodename is what a shell prompt and
//! `hostname` show right away, `/etc/hostname` is what systemd, openrc's
//! `hostname` service, sysvinit's `hostname.sh` and runit's `05-misc.sh`
//! re-apply at boot (and a `hostnamectl` static name), and the
//! `127.0.1.1` line in `/etc/hosts` is the Debian convention that keeps
//! `sudo` and `hostname -f` from stalling on a name no resolver knows.
//!
//! The bridge NIC is ArcBox's: the shim gives it an address by DHCP (no
//! default route — egress stays on the uplink) and the host publishes
//! `<name>.arcbox.local` at that address. A network manager that also
//! configures every Ethernet link would add a second default route through
//! it (arch's `eth.network` and rocky's NetworkManager both did, measured
//! 2026-09-27), so the NIC is declared unmanaged to systemd-networkd and
//! NetworkManager by MAC — the one identifier that holds whatever the
//! interface is called — in the drop-in each of them reads.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

const KERNEL_HOSTNAME: &str = "/proc/sys/kernel/hostname";
const ETC_HOSTNAME: &str = "/etc/hostname";
const ETC_HOSTS: &str = "/etc/hosts";
/// The loopback alias Debian-family images give the hostname; any image
/// gets it, since the line is harmless where the convention is foreign.
const HOSTNAME_ALIAS_ADDRESS: &str = "127.0.1.1";

const ETC_MACHINE_ID: &str = "/etc/machine-id";
/// Where dbus looks first; a plain file here (not the usual symlink) would
/// keep the image's ID alive next to the new one.
const DBUS_MACHINE_ID: &str = "/var/lib/dbus/machine-id";
const ETC_LOCALTIME: &str = "/etc/localtime";
const ETC_TIMEZONE: &str = "/etc/timezone";
const ZONEINFO: &str = "/usr/share/zoneinfo";
/// Read by systemd and most shells' profile scripts.
const ETC_LOCALE_CONF: &str = "/etc/locale.conf";
/// Read by Debian-family PAM sessions.
const ETC_DEFAULT_LOCALE: &str = "/etc/default/locale";

const NETWORKD_BINARIES: &[&str] = &[
    "/usr/lib/systemd/systemd-networkd",
    "/lib/systemd/systemd-networkd",
];
/// Sorts before every distro-shipped `.network` file seen so far
/// (`eth.network`, `10-netplan-*`, `80-*`): networkd applies the first match
/// in lexical order.
const NETWORKD_DROP_IN: &str = "/etc/systemd/network/05-arcbox-bridge.network";

const NETWORK_MANAGER_BINARIES: &[&str] = &["/usr/sbin/NetworkManager", "/usr/bin/NetworkManager"];
const NETWORK_MANAGER_DROP_IN: &str = "/etc/NetworkManager/conf.d/90-arcbox-bridge.conf";

/// The filesystem the files are written into; `/` in the guest, a
/// temporary directory in the tests. See `boot_done::Layout` for the
/// pattern.
struct Layout {
    root: PathBuf,
}

impl Layout {
    #[cfg(target_os = "linux")]
    fn guest() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    /// Makes `name` the guest's hostname everywhere an init or a shell
    /// looks for it. Each write is independent and best-effort: a distro
    /// without `/etc/hosts` still gets its nodename.
    fn set_hostname(&self, name: &str) {
        if let Err(e) = fs::write(self.path(KERNEL_HOSTNAME), name) {
            tracing::warn!(error = %e, "failed to set the kernel hostname");
        }
        if let Err(e) = write_file(&self.path(ETC_HOSTNAME), &format!("{name}\n"), 0o644) {
            tracing::warn!(error = %e, "failed to write /etc/hostname");
        }
        let hosts = self.path(ETC_HOSTS);
        let current = fs::read_to_string(&hosts).unwrap_or_default();
        let wanted = with_hostname_alias(&current, name);
        if wanted != current
            && let Err(e) = write_file(&hosts, &wanted, 0o644)
        {
            tracing::warn!(error = %e, "failed to add the hostname to /etc/hosts");
        }
    }

    /// Makes `id` the guest's machine ID. The host sends 32 hex digits;
    /// anything else is ignored rather than written, since a malformed ID
    /// breaks systemd's boot.
    fn set_machine_id(&self, id: &str) {
        if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            tracing::warn!("ignoring a malformed machine ID from the kernel cmdline");
            return;
        }
        if let Err(e) = write_file(&self.path(ETC_MACHINE_ID), &format!("{id}\n"), 0o444) {
            tracing::warn!(error = %e, "failed to write /etc/machine-id");
        }
        let dbus = self.path(DBUS_MACHINE_ID);
        if fs::symlink_metadata(&dbus).is_ok_and(|meta| meta.file_type().is_file())
            && let Err(e) = write_file(&dbus, &format!("{id}\n"), 0o444)
        {
            tracing::warn!(error = %e, "failed to write the dbus machine ID");
        }
    }

    /// Makes `zone` the guest's time zone, if the image has it: `/etc/localtime`
    /// links to its zoneinfo file and `/etc/timezone` names it. An unknown
    /// zone leaves the image's own, rather than a dangling link.
    fn set_timezone(&self, zone: &str) {
        let valid = zone
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
        let zoneinfo = format!("{ZONEINFO}/{zone}");
        if !valid || !self.path(&zoneinfo).is_file() {
            tracing::warn!(zone, "time zone not in the image; keeping its own");
            return;
        }
        let localtime = self.path(ETC_LOCALTIME);
        if let Some(parent) = localtime.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!(error = %e, "failed to create /etc");
            return;
        }
        let staged = localtime.with_extension("arcbox-tmp");
        let _ = fs::remove_file(&staged);
        let linked = std::os::unix::fs::symlink(format!("..{zoneinfo}"), &staged)
            .and_then(|()| fs::rename(&staged, &localtime));
        if let Err(e) = linked {
            let _ = fs::remove_file(&staged);
            tracing::warn!(error = %e, "failed to link /etc/localtime");
        }
        if let Err(e) = write_file(&self.path(ETC_TIMEZONE), &format!("{zone}\n"), 0o644) {
            tracing::warn!(error = %e, "failed to write /etc/timezone");
        }
    }

    /// Makes `locale` the guest's `LANG`, in the files systemd and Debian's
    /// PAM read. The locale's data must be in the image for programs to use
    /// it; that is the image's business.
    fn set_locale(&self, locale: &str) {
        if locale.is_empty()
            || !locale
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'))
        {
            tracing::warn!("ignoring a malformed locale from the kernel cmdline");
            return;
        }
        let body = format!("LANG={locale}\n");
        if let Err(e) = write_file(&self.path(ETC_LOCALE_CONF), &body, 0o644) {
            tracing::warn!(error = %e, "failed to write /etc/locale.conf");
        }
        if self.path("/etc/default").is_dir()
            && let Err(e) = write_file(&self.path(ETC_DEFAULT_LOCALE), &body, 0o644)
        {
            tracing::warn!(error = %e, "failed to write /etc/default/locale");
        }
    }

    /// Tells every network manager the image ships to leave the NIC with
    /// `mac` alone. Only managers that are installed get a drop-in: the
    /// directories are theirs, and an orphan file there would be noise.
    fn claim_bridge_nic(&self, mac: &str) {
        if NETWORKD_BINARIES.iter().any(|b| self.path(b).exists())
            && let Err(e) = write_file(
                &self.path(NETWORKD_DROP_IN),
                &format!("[Match]\nMACAddress={mac}\n\n[Link]\nUnmanaged=yes\n"),
                0o644,
            )
        {
            tracing::warn!(error = %e, "failed to write the networkd bridge drop-in");
        }
        // `+=` appends to any `unmanaged-devices` the image already sets,
        // where `=` would silently replace it.
        if NETWORK_MANAGER_BINARIES
            .iter()
            .any(|b| self.path(b).exists())
            && let Err(e) = write_file(
                &self.path(NETWORK_MANAGER_DROP_IN),
                &format!("[keyfile]\nunmanaged-devices+=mac:{mac}\n"),
                0o644,
            )
        {
            tracing::warn!(error = %e, "failed to write the NetworkManager bridge drop-in");
        }
    }
}

/// `hosts` with exactly one `127.0.1.1` line, naming `name`.
///
/// An existing `127.0.1.1` line is rewritten rather than kept next to a new
/// one: a machine cloned from another carries the source's line in its
/// overlay, and `hostname -f` would otherwise answer the old name.
fn with_hostname_alias(hosts: &str, name: &str) -> String {
    let alias = format!("{HOSTNAME_ALIAS_ADDRESS}\t{name}");
    let mut lines: Vec<&str> = hosts.lines().collect();
    match lines
        .iter()
        .position(|line| line.split_whitespace().next() == Some(HOSTNAME_ALIAS_ADDRESS))
    {
        Some(i) => lines[i] = &alias,
        None => lines.push(&alias),
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Writes `body` to `path`, staged then renamed, so an init never reads a
/// half-written file.
fn write_file(path: &Path, body: &str, mode: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let staged = path.with_extension("arcbox-tmp");
    let result = (|| {
        let mut file = fs::File::create(&staged)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode))?;
        fs::rename(&staged, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

/// Makes `name` the guest's hostname; see the module docs.
#[cfg(target_os = "linux")]
pub fn set_hostname(name: &str) {
    Layout::guest().set_hostname(name);
}

/// Declares the NIC with `mac` unmanaged to the image's network managers;
/// see the module docs.
#[cfg(target_os = "linux")]
pub fn claim_bridge_nic(mac: &str) {
    Layout::guest().claim_bridge_nic(mac);
}

/// Makes `id` the guest's `/etc/machine-id`; see the module docs.
#[cfg(target_os = "linux")]
pub fn set_machine_id(id: &str) {
    Layout::guest().set_machine_id(id);
}

/// Makes `zone` the guest's time zone; see the module docs.
#[cfg(target_os = "linux")]
pub fn set_timezone(zone: &str) {
    Layout::guest().set_timezone(zone);
}

/// Makes `locale` the guest's `LANG`; see the module docs.
#[cfg(target_os = "linux")]
pub fn set_locale(locale: &str) {
    Layout::guest().set_locale(locale);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            root: dir.path().to_path_buf(),
        };
        (dir, layout)
    }

    fn touch(layout: &Layout, absolute: &str) {
        let path = layout.path(absolute);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "").expect("write");
    }

    fn read(layout: &Layout, absolute: &str) -> String {
        fs::read_to_string(layout.path(absolute)).expect(absolute)
    }

    /// The Debian/Devuan images arrive with `distrobuilder-<uuid>` in
    /// `/etc/hostname` and no `127.0.1.1` line: both must end up naming the
    /// machine, and the rest of `/etc/hosts` must survive.
    #[test]
    fn the_name_lands_in_every_file_an_init_reads() {
        let (_dir, layout) = image();
        touch(&layout, KERNEL_HOSTNAME);
        fs::write(layout.path(KERNEL_HOSTNAME), "arcbox-vm").expect("write");
        touch(&layout, ETC_HOSTNAME);
        fs::write(layout.path(ETC_HOSTNAME), "distrobuilder-4b83\n").expect("write");
        touch(&layout, ETC_HOSTS);
        fs::write(
            layout.path(ETC_HOSTS),
            "127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n",
        )
        .expect("write");

        layout.set_hostname("dev");

        assert_eq!(read(&layout, KERNEL_HOSTNAME), "dev");
        assert_eq!(read(&layout, ETC_HOSTNAME), "dev\n");
        assert_eq!(
            read(&layout, ETC_HOSTS),
            "127.0.0.1\tlocalhost\n::1\t\tlocalhost ip6-localhost\n127.0.1.1\tdev\n"
        );
    }

    /// Fedora and Rocky ship no `/etc/hostname`; it is created rather than
    /// skipped, or `hostnamectl` would keep reporting the static name unset.
    #[test]
    fn a_missing_etc_hostname_is_created() {
        let (_dir, layout) = image();
        touch(&layout, KERNEL_HOSTNAME);
        layout.set_hostname("dev");
        assert_eq!(read(&layout, ETC_HOSTNAME), "dev\n");
        assert_eq!(read(&layout, ETC_HOSTS), "127.0.1.1\tdev\n");
    }

    /// A clone carries the source machine's alias line; it is replaced, not
    /// joined by a second one, and a repeat boot changes nothing.
    #[test]
    fn the_alias_line_is_rewritten_once() {
        let hosts = "127.0.0.1 localhost\n127.0.1.1\told-name\n";
        let once = with_hostname_alias(hosts, "new-name");
        assert_eq!(once, "127.0.0.1 localhost\n127.0.1.1\tnew-name\n");
        assert_eq!(with_hostname_alias(&once, "new-name"), once);
    }

    /// Only an installed manager gets its drop-in, and the networkd one must
    /// match by MAC with the link unmanaged — the file arch's `eth.network`
    /// would otherwise beat to the interface.
    #[test]
    fn drop_ins_go_only_to_the_managers_the_image_ships() {
        let (_dir, layout) = image();
        touch(&layout, "/usr/lib/systemd/systemd-networkd");
        layout.claim_bridge_nic("02:0c:a6:78:f0:a8");

        assert_eq!(
            read(&layout, NETWORKD_DROP_IN),
            "[Match]\nMACAddress=02:0c:a6:78:f0:a8\n\n[Link]\nUnmanaged=yes\n"
        );
        assert!(!layout.path(NETWORK_MANAGER_DROP_IN).exists());

        let (_dir, layout) = image();
        touch(&layout, "/usr/sbin/NetworkManager");
        layout.claim_bridge_nic("02:0c:a6:78:f0:a8");
        assert!(!layout.path(NETWORKD_DROP_IN).exists());
        assert_eq!(
            read(&layout, NETWORK_MANAGER_DROP_IN),
            "[keyfile]\nunmanaged-devices+=mac:02:0c:a6:78:f0:a8\n"
        );
    }

    /// Alpine, Devuan and Void ship neither manager; nothing is written.
    #[test]
    fn an_image_without_a_manager_gets_no_drop_in() {
        let (dir, layout) = image();
        layout.claim_bridge_nic("02:0c:a6:78:f0:a8");
        assert!(
            !layout.path("/etc").exists(),
            "{:?}",
            fs::read_dir(dir.path())
        );
    }

    /// A clone or an isolated machine replaces the image's ID everywhere
    /// it lives as a file; dbus's usual symlink is left to follow it.
    #[test]
    fn the_machine_id_replaces_the_images() {
        let (_dir, layout) = image();
        touch(&layout, ETC_MACHINE_ID);
        fs::write(
            layout.path(ETC_MACHINE_ID),
            "0123456789abcdef0123456789abcdef\n",
        )
        .expect("write");
        touch(&layout, DBUS_MACHINE_ID);
        let id = "fedcba9876543210fedcba9876543210";
        layout.set_machine_id(id);
        assert_eq!(read(&layout, ETC_MACHINE_ID), format!("{id}\n"));
        assert_eq!(read(&layout, DBUS_MACHINE_ID), format!("{id}\n"));

        let (_dir, layout) = image();
        layout.set_machine_id("not-an-id");
        assert!(!layout.path(ETC_MACHINE_ID).exists());
    }

    #[test]
    fn a_known_time_zone_is_linked_and_an_unknown_one_ignored() {
        let (_dir, layout) = image();
        touch(&layout, "/usr/share/zoneinfo/Asia/Tokyo");
        touch(&layout, ETC_LOCALTIME);
        layout.set_timezone("Asia/Tokyo");
        assert_eq!(
            fs::read_link(layout.path(ETC_LOCALTIME)).expect("link"),
            PathBuf::from("../usr/share/zoneinfo/Asia/Tokyo")
        );
        assert_eq!(read(&layout, ETC_TIMEZONE), "Asia/Tokyo\n");

        layout.set_timezone("Mars/Olympus");
        assert_eq!(read(&layout, ETC_TIMEZONE), "Asia/Tokyo\n");
        layout.set_timezone("../../etc/passwd");
        assert_eq!(read(&layout, ETC_TIMEZONE), "Asia/Tokyo\n");
    }

    #[test]
    fn the_locale_goes_where_systemd_and_debian_read_it() {
        let (_dir, layout) = image();
        fs::create_dir_all(layout.path("/etc/default")).expect("mkdir");
        layout.set_locale("de_DE.UTF-8");
        assert_eq!(read(&layout, ETC_LOCALE_CONF), "LANG=de_DE.UTF-8\n");
        assert_eq!(read(&layout, ETC_DEFAULT_LOCALE), "LANG=de_DE.UTF-8\n");

        let (_dir, layout) = image();
        layout.set_locale("en_US.UTF-8");
        assert_eq!(read(&layout, ETC_LOCALE_CONF), "LANG=en_US.UTF-8\n");
        assert!(!layout.path(ETC_DEFAULT_LOCALE).exists());
        layout.set_locale("bad value");
        assert_eq!(read(&layout, ETC_LOCALE_CONF), "LANG=en_US.UTF-8\n");
    }
}
