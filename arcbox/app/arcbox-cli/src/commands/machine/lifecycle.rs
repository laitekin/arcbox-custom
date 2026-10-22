//! Machine lifecycle beyond create, start, stop and remove: clone, export,
//! import, resize, isolation and the default machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use arcbox_connect::v1::{
    CloneMachineRequest, ExportMachineRequest, ImportMachineRequest, InspectMachineRequest,
    ListMachinesRequest, MachinePortForward, SetDefaultMachineRequest, SetMachineIdentityRequest,
    SetMachineIsolationRequest, SetMachinePortForwardsRequest, SetMachineResourcesRequest,
};
use clap::Args;

use super::machine_client;
use crate::error::machine_lifecycle;

#[derive(Args)]
pub struct CloneArgs {
    /// Machine to clone; must be stopped
    pub source: String,
    /// Name of the clone
    pub name: String,
}

#[derive(Args)]
pub struct ExportArgs {
    /// Machine to export; must be stopped
    pub name: String,
    /// Where to write the archive (default: ./<name>.tar.zst)
    pub path: Option<PathBuf>,
    /// Carry the machine's image in the archive (default: only for images
    /// imported on this Mac, which no other Mac can download)
    #[arg(long, conflicts_with = "no_include_image")]
    pub include_image: bool,
    /// Never carry the image, even an imported one
    #[arg(long)]
    pub no_include_image: bool,
}

#[derive(Args)]
pub struct ImportArgs {
    /// Archive written by `abctl machine export`
    pub path: PathBuf,
    /// Name for the machine (default: the name in the archive)
    #[arg(long)]
    pub name: Option<String>,
    /// CPUs (default: the archive's)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub cpus: Option<u32>,
    /// Memory in MiB (default: the archive's)
    #[arg(long, value_name = "MIB")]
    pub memory: Option<u64>,
}

#[derive(Args)]
#[command(group = clap::ArgGroup::new("size").multiple(true).required(true))]
pub struct ResizeArgs {
    /// Machine name
    pub name: String,
    /// CPUs, 1 through the host's logical CPU count
    #[arg(long, group = "size", value_parser = clap::value_parser!(u32).range(1..))]
    pub cpus: Option<u32>,
    /// Memory in MiB, 512 through the host's physical memory
    #[arg(long, group = "size", value_name = "MIB")]
    pub memory: Option<u64>,
}

#[derive(Args)]
pub struct IsolateArgs {
    /// Machine name; must be stopped
    pub name: String,
    /// Lift the isolation: share the Mac's /Users and /private again
    #[arg(long)]
    pub off: bool,
}

#[derive(Args)]
pub struct ForwardArgs {
    /// Machine name
    pub name: String,
    /// Port in the machine; omit to list the forwards
    pub guest_port: Option<u16>,
    /// Port on the Mac's 127.0.0.1 (default: the guest port)
    #[arg(long, requires = "guest_port")]
    pub host_port: Option<u16>,
    /// Forward UDP instead of TCP
    #[arg(long, requires = "guest_port")]
    pub udp: bool,
    /// Remove the forward of this guest port instead
    #[arg(long, requires = "guest_port", conflicts_with = "host_port")]
    pub remove: bool,
}

#[derive(Args)]
pub struct IdentityArgs {
    /// Machine name; must be stopped
    pub name: String,
    /// Time zone such as Europe/Berlin; `image` keeps the image's own
    #[arg(long)]
    pub timezone: Option<String>,
    /// Locale such as en_US.UTF-8; `image` keeps the image's own
    #[arg(long)]
    pub locale: Option<String>,
}

#[derive(Args)]
pub struct DefaultArgs {
    /// Machine to make the default; omit to show the current one
    #[arg(conflicts_with = "unset")]
    pub name: Option<String>,
    /// Clear the default machine
    #[arg(long)]
    pub unset: bool,
}

pub async fn execute_clone(args: CloneArgs) -> Result<()> {
    machine_client()
        .clone_machine(CloneMachineRequest {
            id: args.source.clone(),
            name: args.name.clone(),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.source, "clone"))?;
    println!(
        "Cloned machine '{}' into '{}'. Start it with: abctl machine start {}",
        args.source, args.name, args.name
    );
    Ok(())
}

pub async fn execute_export(args: ExportArgs) -> Result<()> {
    let path = args
        .path
        .unwrap_or_else(|| PathBuf::from(format!("{}.tar.zst", args.name)));
    let path = std::path::absolute(&path)
        .with_context(|| format!("Failed to resolve {}", path.display()))?;
    println!("Exporting machine '{}' to {}...", args.name, path.display());
    let exported = machine_client()
        .export(ExportMachineRequest {
            id: args.name.clone(),
            path: path.to_string_lossy().into_owned(),
            include_image: if args.include_image {
                Some(true)
            } else if args.no_include_image {
                Some(false)
            } else {
                None
            },
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "export"))?
        .into_owned();
    println!(
        "Exported machine '{}' to {} ({})",
        args.name,
        exported.path,
        human_size(exported.size)
    );
    Ok(())
}

pub async fn execute_import(args: ImportArgs) -> Result<()> {
    let path = std::fs::canonicalize(&args.path)
        .with_context(|| format!("Failed to read {}", args.path.display()))?;
    let label = args.name.clone().unwrap_or_else(|| archive_label(&path));
    println!("Importing machine from {}...", path.display());
    let imported = machine_client()
        .import(ImportMachineRequest {
            path: path.to_string_lossy().into_owned(),
            name: args.name.unwrap_or_default(),
            cpus: args.cpus.unwrap_or(0),
            memory: args.memory.unwrap_or(0).saturating_mul(1024 * 1024),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &label, "import"))?
        .into_owned();
    let distro = if imported.distro_version.is_empty() {
        imported.distro
    } else {
        format!("{} {}", imported.distro, imported.distro_version)
    };
    println!(
        "Imported machine '{}' ({distro}). Start it with: abctl machine start {}",
        imported.id, imported.id
    );
    Ok(())
}

pub async fn execute_resize(args: ResizeArgs) -> Result<()> {
    let resources = machine_client()
        .set_resources(SetMachineResourcesRequest {
            id: args.name.clone(),
            cpus: args.cpus.unwrap_or(0),
            memory: args.memory.unwrap_or(0).saturating_mul(1024 * 1024),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "resize"))?
        .into_owned();
    let mib = 1024 * 1024;
    println!(
        "Machine '{}' boots with {} CPUs and {} MiB (host: {} CPUs, {} MiB)",
        args.name,
        resources.cpus,
        resources.memory / mib,
        resources.host_cpus,
        resources.host_memory / mib
    );
    if resources.restart_required {
        println!(
            "It is running with its previous size. Restart it to apply the new one:\n  \
             abctl machine stop {} && abctl machine start {}",
            args.name, args.name
        );
    }
    Ok(())
}

pub async fn execute_isolate(args: IsolateArgs) -> Result<()> {
    let isolated = !args.off;
    machine_client()
        .set_isolation(SetMachineIsolationRequest {
            id: args.name.clone(),
            isolated,
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "change the isolation of"))?;
    if isolated {
        println!(
            "Machine '{}' is isolated: from its next start it gets no share of the Mac's \
             files, no network path to the Mac or other machines, and egress only through \
             its saved proxy (mounts you added stay shared)",
            args.name
        );
    } else {
        println!(
            "Machine '{}' is no longer isolated: from its next start it shares the Mac's \
             /Users and /private again",
            args.name
        );
    }
    Ok(())
}

/// `forwards` after adding `guest_port` (as `host_port`) or removing it.
fn edited_forwards(
    mut forwards: Vec<MachinePortForward>,
    guest_port: u16,
    host_port: u16,
    udp: bool,
    remove: bool,
) -> Vec<MachinePortForward> {
    let protocol = if udp { "udp" } else { "tcp" };
    let same_protocol = |f: &MachinePortForward| {
        f.protocol == protocol || (f.protocol.is_empty() && protocol == "tcp")
    };
    if remove {
        forwards.retain(|f| !(f.guest_port == u32::from(guest_port) && same_protocol(f)));
    } else {
        forwards.retain(|f| !(f.host_port == u32::from(host_port) && same_protocol(f)));
        forwards.push(MachinePortForward {
            host_port: u32::from(host_port),
            guest_port: u32::from(guest_port),
            protocol: protocol.to_owned(),
            ..Default::default()
        });
    }
    forwards
}

pub async fn execute_forward(args: ForwardArgs) -> Result<()> {
    let client = machine_client();
    let current = client
        .inspect(InspectMachineRequest {
            id: args.name.clone(),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "inspect"))?
        .into_owned()
        .port_forwards;
    let Some(guest_port) = args.guest_port else {
        if current.is_empty() {
            println!("Machine '{}' has no forwarded ports", args.name);
        }
        for forward in &current {
            let protocol = if forward.protocol.is_empty() {
                "tcp"
            } else {
                forward.protocol.as_str()
            };
            println!(
                "127.0.0.1:{} -> {}:{} ({protocol})",
                forward.host_port, args.name, forward.guest_port
            );
        }
        return Ok(());
    };
    let host_port = args.host_port.unwrap_or(guest_port);
    let forwards = edited_forwards(current, guest_port, host_port, args.udp, args.remove);
    client
        .set_port_forwards(SetMachinePortForwardsRequest {
            id: args.name.clone(),
            forwards,
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "forward ports of"))?;
    if args.remove {
        println!(
            "Removed the forward of port {guest_port} of machine '{}'",
            args.name
        );
    } else {
        println!(
            "127.0.0.1:{host_port} on this Mac now reaches port {guest_port} of machine '{}' \
             while it runs; its service must listen on all interfaces",
            args.name
        );
    }
    Ok(())
}

pub async fn execute_identity(args: IdentityArgs) -> Result<()> {
    let client = machine_client();
    let current = client
        .inspect(InspectMachineRequest {
            id: args.name.clone(),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "inspect"))?
        .into_owned();
    if args.timezone.is_none() && args.locale.is_none() {
        let or_image = |value: &str| {
            if value.is_empty() {
                "the image's".to_owned()
            } else {
                value.to_owned()
            }
        };
        println!("Time zone: {}", or_image(&current.timezone));
        println!("Locale:    {}", or_image(&current.locale));
        return Ok(());
    }
    let pick = |given: Option<String>, current: String| match given.as_deref() {
        Some("image") => String::new(),
        Some(_) => given.unwrap_or_default(),
        None => current,
    };
    client
        .set_identity(SetMachineIdentityRequest {
            id: args.name.clone(),
            timezone: pick(args.timezone, current.timezone),
            locale: pick(args.locale, current.locale),
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, &args.name, "set the identity of"))?;
    println!(
        "Machine '{}' presents the new time zone and locale from its next start",
        args.name
    );
    Ok(())
}

pub async fn execute_default(args: DefaultArgs) -> Result<()> {
    let client = machine_client();
    if args.unset {
        client
            .set_default(SetDefaultMachineRequest::default())
            .await
            .context("Failed to clear the default machine")?;
        println!("Default machine cleared");
        return Ok(());
    }
    if let Some(name) = args.name {
        client
            .set_default(SetDefaultMachineRequest {
                id: name.clone(),
                ..Default::default()
            })
            .await
            .map_err(|error| crate::error::machine_operation(error, &name, "set as default"))?;
        println!("Default machine: {name}");
        println!("`abctl machine exec` and `abctl machine ssh` now use it when given no name.");
        return Ok(());
    }
    let listed = client
        .list(ListMachinesRequest {
            all: true,
            ..Default::default()
        })
        .await
        .context("Failed to list machines")?
        .into_owned();
    if listed.default_machine.is_empty() {
        bail!("No default machine. Set one with `abctl machine default <name>`.");
    }
    println!("{}", listed.default_machine);
    Ok(())
}

/// The machine name an archive implies before its manifest is read: the
/// file name without `.tar.zst`, for error messages only.
fn archive_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| {
            name.to_string_lossy()
                .trim_end_matches(".tar.zst")
                .to_owned()
        },
    )
}

pub(super) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::edited_forwards;
    use arcbox_connect::v1::MachinePortForward;

    fn forward(host_port: u32, guest_port: u32, protocol: &str) -> MachinePortForward {
        MachinePortForward {
            host_port,
            guest_port,
            protocol: protocol.into(),
            ..Default::default()
        }
    }

    #[test]
    fn forwards_are_added_replaced_and_removed_per_protocol() {
        let added = edited_forwards(Vec::new(), 5901, 5901, false, false);
        assert_eq!(added, [forward(5901, 5901, "tcp")]);
        // The same host port again replaces the old target.
        let replaced = edited_forwards(added, 5902, 5901, false, false);
        assert_eq!(replaced, [forward(5901, 5902, "tcp")]);
        // UDP on the same port is a separate forward.
        let both = edited_forwards(replaced, 5902, 5901, true, false);
        assert_eq!(both.len(), 2);
        let tcp_gone = edited_forwards(both, 5902, 0, false, true);
        assert_eq!(tcp_gone, [forward(5901, 5902, "udp")]);
        // A forward from before the protocol field counts as TCP.
        assert!(edited_forwards(vec![forward(22, 22, "")], 22, 0, false, true).is_empty());
    }

    use super::*;

    #[test]
    fn sizes_read_in_the_nearest_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(734_003_200), "700.0 MiB");
        assert_eq!(human_size(5 << 30), "5.0 GiB");
    }

    #[test]
    fn the_archive_label_is_the_file_stem() {
        assert_eq!(archive_label(Path::new("/tmp/dev.tar.zst")), "dev");
        assert_eq!(archive_label(Path::new("backup.tgz")), "backup.tgz");
    }
}
