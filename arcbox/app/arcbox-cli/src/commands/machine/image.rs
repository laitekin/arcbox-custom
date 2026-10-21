//! `abctl machine image` — the local machine image registry.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use arcbox_connect::v1 as pb;
use arcbox_connect::v1::MachineImageServiceClient;
use clap::{Args, Subcommand};

use super::lifecycle::human_size;
use crate::commands::OutputFormat;
use crate::connect;

#[derive(Debug, Subcommand)]
pub enum ImageCommands {
    /// List local machine images
    #[command(alias = "list")]
    Ls,
    /// List the images the image source publishes
    Remote,
    /// Import a root filesystem as a machine image
    ///
    /// SOURCE is a rootfs archive (.tar, .tar.gz, .tar.xz, .tar.zst), a
    /// squashfs image, or `docker:<image>` for an image in ArcBox's Docker
    /// engine. The rootfs must contain /sbin/init for this Mac's
    /// architecture.
    Import(ImportArgs),
    /// Remove a local machine image (refused while a machine uses it)
    #[command(alias = "remove")]
    Rm(RmArgs),
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Archive or squashfs path, or docker:<image>
    pub source: String,
    /// Name for the image (lowercase letters, digits, '.', '-', '_')
    #[arg(long)]
    pub name: String,
    /// Distro id to record instead of os-release's ID
    #[arg(long)]
    pub distro: Option<String>,
    /// Release to record instead of os-release's VERSION_ID
    #[arg(long)]
    pub release: Option<String>,
    /// For docker: sources, first install an init system (systemd, or
    /// openrc on Alpine) into a derived image
    #[arg(long)]
    pub add_init: bool,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    /// Image name, or name@version
    pub image: String,
}

fn client() -> MachineImageServiceClient<connectrpc::client::SharedHttp2Connection> {
    let (transport, config) = connect::daemon(&crate::commands::resolve_grpc_socket_path());
    MachineImageServiceClient::new(transport, config)
}

pub async fn execute(cmd: ImageCommands, format: OutputFormat) -> Result<()> {
    match cmd {
        ImageCommands::Ls => list(format).await,
        ImageCommands::Remote => remote(format).await,
        ImageCommands::Import(args) => import(args).await,
        ImageCommands::Rm(args) => remove(&args.image).await,
    }
}

async fn list(format: OutputFormat) -> Result<()> {
    let images = client()
        .list(pb::ListMachineImagesRequest::default())
        .await
        .context("failed to list machine images")?
        .into_owned()
        .images;
    if matches!(format, OutputFormat::Json) {
        let json: Vec<_> = images
            .iter()
            .map(|i| {
                serde_json::json!({
                    "name": i.name, "version": i.version, "distro": i.distro,
                    "release": i.release, "release_title": i.release_title,
                    "arch": i.arch, "imported": i.imported, "source": i.source,
                    "size": i.size, "sha256": i.sha256, "machines": i.machines,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }
    if images.is_empty() {
        println!(
            "No machine images. Create a machine to pull one, or `abctl machine image import`."
        );
        return Ok(());
    }
    println!(
        "{:<24} {:<22} {:<18} {:>9}  {:<8}  MACHINES",
        "NAME", "VERSION", "DISTRO", "SIZE", "ORIGIN"
    );
    for image in &images {
        println!(
            "{:<24} {:<22} {:<18} {:>9}  {:<8}  {}",
            image.name,
            image.version,
            format!("{} {}", image.distro, image.release),
            human_size(image.size),
            if image.imported { "imported" } else { "pulled" },
            if image.machines.is_empty() {
                "-".to_owned()
            } else {
                image.machines.join(", ")
            }
        );
    }
    Ok(())
}

async fn remote(format: OutputFormat) -> Result<()> {
    let listing = client()
        .list_remote(pb::ListRemoteMachineImagesRequest::default())
        .await
        .context("failed to list published machine images")?
        .into_owned();
    if matches!(format, OutputFormat::Json) {
        let json: Vec<_> = listing
            .images
            .iter()
            .map(|i| {
                serde_json::json!({
                    "name": i.name, "distro": i.distro, "release": i.release,
                    "release_title": i.release_title, "arch": i.arch, "latest": i.latest,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }
    println!("Source: {}", listing.base);
    println!("{:<14} {:<12} {:<26} LATEST", "DISTRO", "RELEASE", "TITLE");
    for image in &listing.images {
        println!(
            "{:<14} {:<12} {:<26} {}",
            image.distro, image.release, image.release_title, image.latest
        );
    }
    Ok(())
}

fn phase_label(phase: pb::ImportMachineImagePhase) -> &'static str {
    use pb::ImportMachineImagePhase as P;
    match phase {
        P::IMPORT_MACHINE_IMAGE_PHASE_EXPORTING => "Exporting the Docker image",
        P::IMPORT_MACHINE_IMAGE_PHASE_READING => "Reading the root filesystem",
        P::IMPORT_MACHINE_IMAGE_PHASE_CHECKING => "Checking init and architecture",
        P::IMPORT_MACHINE_IMAGE_PHASE_WRITING => "Writing the squashfs",
        P::IMPORT_MACHINE_IMAGE_PHASE_REGISTERING => "Registering",
        P::IMPORT_MACHINE_IMAGE_PHASE_DONE => "Done",
        P::IMPORT_MACHINE_IMAGE_PHASE_UNSPECIFIED => "Working",
    }
}

async fn import(args: ImportArgs) -> Result<()> {
    let mut request = pb::ImportMachineImageRequest {
        name: args.name.clone(),
        distro: args.distro.clone().unwrap_or_default(),
        release: args.release.clone().unwrap_or_default(),
        ..Default::default()
    };
    let mut derived = None;
    if let Some(image) = args.source.strip_prefix("docker:") {
        request.docker_image = if args.add_init {
            let tag = format!("arcbox-machine-import/{}:latest", args.name);
            build_with_init(image, &tag)?;
            derived = Some(tag.clone());
            tag
        } else {
            image.to_owned()
        };
    } else {
        if args.add_init {
            bail!(
                "--add-init needs a docker:<image> source; install an init into the archive itself"
            );
        }
        let path = PathBuf::from(&args.source);
        let path = std::fs::canonicalize(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        request.path = path.to_string_lossy().into_owned();
    }

    let result = stream_import(request).await;
    if let Some(tag) = derived {
        let _ = docker().args(["image", "rm", &tag]).output();
    }
    result
}

async fn stream_import(request: pb::ImportMachineImageRequest) -> Result<()> {
    let mut stream = client()
        .import(request)
        .await
        .context("failed to start the import")?;
    let mut shown = None;
    while let Some(item) = stream
        .message::<pb::ImportMachineImageEvent>()
        .await
        .context("import failed")?
    {
        let event: pb::ImportMachineImageEvent = item.to_owned_message();
        let phase = event.phase.as_known().unwrap_or_default();
        if phase == pb::ImportMachineImagePhase::IMPORT_MACHINE_IMAGE_PHASE_DONE {
            for warning in &event.warnings {
                eprintln!("warning: {warning}");
            }
            if let Some(image) = event.image.as_option() {
                println!(
                    "Imported {}@{} ({} {}, {})",
                    image.name,
                    image.version,
                    image.distro,
                    image.release,
                    human_size(image.size)
                );
                println!("Create a machine from it with:");
                println!("  abctl machine create <name> --image {}", image.name);
            }
            return Ok(());
        }
        if shown != Some(phase) {
            shown = Some(phase);
            println!("{}...", phase_label(phase));
        }
    }
    bail!("the daemon ended the import without a result")
}

async fn remove(image: &str) -> Result<()> {
    let (name, version) = image.split_once('@').unwrap_or((image, ""));
    client()
        .remove(pb::RemoveMachineImageRequest {
            name: name.to_owned(),
            version: version.to_owned(),
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to remove {image}"))?;
    println!("Removed {image}");
    Ok(())
}

fn docker() -> std::process::Command {
    let mut command = std::process::Command::new("docker");
    command.env(
        "DOCKER_HOST",
        format!(
            "unix://{}",
            crate::commands::resolve_docker_socket_path().display()
        ),
    );
    command
}

/// The Dockerfile that installs an init system into `image`, choosing the
/// package manager the image has.
fn init_dockerfile(image: &str) -> String {
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

/// Builds `tag` from `image` with an init system installed.
fn build_with_init(image: &str, tag: &str) -> Result<()> {
    use std::io::Write as _;
    println!("Building {tag} with an init system...");
    let mut child = docker()
        .args(["build", "--pull=false", "-t", tag, "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("failed to run `docker build` (is the docker CLI installed?)")?;
    child
        .stdin
        .take()
        .context("docker build has no stdin")?
        .write_all(init_dockerfile(image).as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("docker build of {tag} failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_dockerfile_derives_from_the_source_and_requires_an_init() {
        let dockerfile = init_dockerfile("ubuntu:24.04");
        assert!(dockerfile.starts_with("FROM ubuntu:24.04\n"));
        assert!(dockerfile.contains("systemd-sysv"));
        assert!(dockerfile.contains("openrc"));
        assert!(dockerfile.trim_end().ends_with("test -e /sbin/init"));
    }
}
