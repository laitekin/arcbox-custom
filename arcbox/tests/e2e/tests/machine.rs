//! Machine lifecycle e2e: the first real end-to-end exercise of the distro
//! machine path through a live VZ daemon.
//!
//! create (pulls the alpine image from the live `image.arcboxcdn.com/linux`
//! mirror) → start (boot shim → overlay → distro init; readiness = agent
//! ping + routable IP) → inspect → system info → piped exec → interactive
//! PTY session → per-machine stats → graceful stop → remove.
//!
//! Requirements beyond the usual daemon e2e setup: boot assets ≥ 0.6.4 (the
//! first bundle carrying `/sbin/arcbox-machine-init` and a squashfs-capable
//! kernel). Until `assets.lock` points there, run with
//! `ARCBOX_BOOT_ASSET_VERSION=0.6.4`.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use arcbox_e2e::boot_assets::{resolve_boot_version, stage_dev_boot_assets};
use arcbox_e2e::daemon::{DaemonConfig, DaemonHandle, connect_unix, host_mounts};
use arcbox_e2e::docker::ensure_image;
use arcbox_e2e::metrics::{BootAssets, RunMetrics};
use arcbox_grpc::v1::machine_image_service_client::MachineImageServiceClient;
use arcbox_grpc::v1::machine_service_client::MachineServiceClient;
use arcbox_grpc::v1::stats_service_client::StatsServiceClient;
use arcbox_protocol::v1::{
    CreateMachineRequest, ExportMachineRequest, ImportMachineImagePhase, ImportMachineImageRequest,
    ImportMachineRequest, InspectMachineRequest, ListMachineImagesRequest, ListMachinesRequest,
    MachineAgentRequest, MachineExecInput, MachineExecRequest, RemoveMachineImageRequest,
    RemoveMachineRequest, StartMachineRequest, StatsWatchRequest, StopMachineRequest, TerminalSize,
    machine_exec_input,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

#[path = "machine/vnc.rs"]
mod vnc;

static TRACING: Once = Once::new();

const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// Budget for create: a cold image pull from the live CDN (~4 MB alpine)
/// plus registry staging.
const CREATE_BUDGET: Duration = Duration::from_secs(120);
/// Budget for start: shim boot, overlay staging, distro init, agent
/// readiness, DHCP.
const START_BUDGET: Duration = Duration::from_secs(120);
/// Budget for one RPC against a running machine.
const RPC_BUDGET: Duration = Duration::from_secs(30);
/// Budget for the daemon to mount the machine's root after `start` returns:
/// the mount loop reacts to `MachineStarted`, asks the agent for the export
/// and runs `mount_nfs`.
const MOUNT_BUDGET: Duration = Duration::from_secs(30);

const MACHINE: &str = "e2e-alpine";

fn init_tracing() {
    TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    });
}

#[test]
#[ignore = "boots a real VZ daemon, pulls a distro image from the live CDN, and drives a machine end to end"]
fn machine_lifecycle_end_to_end() -> Result<()> {
    init_tracing();

    let root = arcbox_e2e::repo_root();
    if !arcbox_e2e::env_flag("SKIP_BUILD") {
        let shell = xshell::Shell::new()?;
        shell.change_dir(&root);
        xshell::cmd!(shell, "cargo build --release -p arcbox-daemon").run()?;
        // The machine agent must speak MachineExec*; build it fresh.
        xshell::cmd!(
            shell,
            "cargo build --release -p arcbox-agent --target aarch64-unknown-linux-musl"
        )
        .run()?;
    }

    let version = resolve_boot_version(&root)?;
    let data_dir = tempfile::Builder::new()
        .prefix("arcbox-machine-e2e-")
        .tempdir()?;
    stage_dev_boot_assets(&root, data_dir.path(), &version)?;

    let mut metrics = RunMetrics::new(
        "machine_lifecycle",
        Some("vz"),
        BootAssets::Bundle {
            version: version.clone(),
        },
    )?;

    let mut daemon = DaemonHandle::spawn(DaemonConfig {
        binary: root.join("target/release/arcbox-daemon"),
        data_dir: data_dir.path().to_owned(),
        args: vec![],
        env: vec![
            ("ARCBOX_BOOT_ASSET_VERSION".to_owned(), version),
            ("ARCBOX_VM_BACKEND".to_owned(), "vz".to_owned()),
            ("ARCBOX_DNS_PORT".to_owned(), "0".to_owned()),
        ],
    })?;

    let result = scenario(&mut daemon, data_dir.path(), &mut metrics);
    metrics.passed = result.is_ok();
    if let Err(error) = metrics.write(Some(data_dir.path())) {
        tracing::warn!("writing run metrics failed: {error:#}");
    }
    if result.is_err() {
        let kept = data_dir.keep();
        tracing::warn!(path = %kept.display(), "preserving test directory");
    }
    result
}

/// Budget for importing a Docker image as a machine image: building the
/// derived image with OpenRC (an `apk add` against the live mirror),
/// exporting it and writing the squashfs.
const IMAGE_IMPORT_BUDGET: Duration = Duration::from_secs(300);
/// Budget for exporting or importing a machine archive carrying its image.
const ARCHIVE_BUDGET: Duration = Duration::from_secs(120);

const IMPORTED_IMAGE: &str = "e2e-imported";
const IMPORTED_MACHINE: &str = "e2e-imported-m";
const RESTORED_MACHINE: &str = "e2e-imported-back";

/// A user-imported image end to end: a Docker image becomes a machine image
/// (the daemon installs OpenRC first), a machine boots from it, an export
/// carries the image, and importing that archive on a host that lost the
/// image brings both back.
#[test]
#[ignore = "boots a real VZ daemon, builds an image in its Docker engine and boots a machine from it"]
fn imported_machine_image_end_to_end() -> Result<()> {
    init_tracing();

    let root = arcbox_e2e::repo_root();
    if !arcbox_e2e::env_flag("SKIP_BUILD") {
        let shell = xshell::Shell::new()?;
        shell.change_dir(&root);
        xshell::cmd!(shell, "cargo build --release -p arcbox-daemon").run()?;
        xshell::cmd!(
            shell,
            "cargo build --release -p arcbox-agent --target aarch64-unknown-linux-musl"
        )
        .run()?;
    }

    let version = resolve_boot_version(&root)?;
    let data_dir = tempfile::Builder::new()
        .prefix("arcbox-machine-image-e2e-")
        .tempdir()?;
    stage_dev_boot_assets(&root, data_dir.path(), &version)?;
    let config_home = data_dir.path().join("xdg-config");
    std::fs::create_dir_all(config_home.join("arcbox"))?;
    std::fs::write(
        config_home.join("arcbox/config.toml"),
        "[vm]\ncpus = 2\nmemory_mb = 2048\n",
    )?;
    let mut metrics = RunMetrics::new(
        "machine_image_import",
        Some("vz"),
        BootAssets::Bundle {
            version: version.clone(),
        },
    )?;
    let mut daemon = DaemonHandle::spawn(DaemonConfig {
        binary: root.join("target/release/arcbox-daemon"),
        data_dir: data_dir.path().to_owned(),
        args: vec![],
        env: vec![
            ("ARCBOX_BOOT_ASSET_VERSION".to_owned(), version),
            ("ARCBOX_VM_BACKEND".to_owned(), "vz".to_owned()),
            ("ARCBOX_DNS_PORT".to_owned(), "0".to_owned()),
            ("XDG_CONFIG_HOME".into(), config_home.display().to_string()),
            (
                "ARCBOX_HELPER_SOCKET".into(),
                data_dir
                    .path()
                    .join("absent-helper.sock")
                    .display()
                    .to_string(),
            ),
        ],
    })?;

    let result = image_scenario(&mut daemon, data_dir.path(), &mut metrics);
    metrics.passed = result.is_ok();
    if let Err(error) = metrics.write(Some(data_dir.path())) {
        tracing::warn!("writing run metrics failed: {error:#}");
    }
    if result.is_err() || arcbox_e2e::env_flag("KEEP_TEST_DIR") {
        let kept = data_dir.keep();
        tracing::warn!(path = %kept.display(), "preserving test directory");
    }
    result
}

fn image_scenario(
    daemon: &mut DaemonHandle,
    data_dir: &Path,
    metrics: &mut RunMetrics,
) -> Result<()> {
    metrics.time("daemon_ready", || daemon.wait_ready_blocking(READY_TIMEOUT))?;
    let mut source = std::env::var("ARCBOX_E2E_MACHINE_SOURCE_IMAGE")
        .unwrap_or_else(|_| "alpine:3.20".to_owned());
    let distro = std::env::var("ARCBOX_E2E_MACHINE_DISTRO").unwrap_or_else(|_| "alpine".into());
    let local_rootfs = std::env::var("ARCBOX_E2E_MACHINE_ROOTFS").ok();
    if local_rootfs.is_none() {
        ensure_image(data_dir, &source)?;
    }
    let check_vnc = arcbox_e2e::env_flag("ARCBOX_E2E_MACHINE_VNC");
    if check_vnc && local_rootfs.is_none() {
        source = vnc::prepare_image(data_dir, &source)?;
    }
    let socket = daemon.grpc_socket();
    let archive = data_dir.join("e2e-imported.arcbox");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    runtime.block_on(async {
        let channel = connect_unix(&socket).await?;
        let mut machines = MachineServiceClient::new(channel.clone());
        let mut images = MachineImageServiceClient::new(channel.clone());
        let mut proxies =
            arcbox_grpc::v1::proxy_library_service_client::ProxyLibraryServiceClient::new(channel);
        // Imported guests must not share host files or expose the VNC
        // fixture through a bridged NIC. No egress is needed after import.
        let proxy = proxies
            .save_profile(arcbox_protocol::v1::SaveProxyProfileRequest {
                name: "e2e-import-no-egress".into(),
                settings: Some(arcbox_protocol::v1::EgressProxySettings {
                    mode: arcbox_protocol::v1::EgressProxyMode::Custom.into(),
                    url: "socks5://127.0.0.1:9".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?
            .into_inner();

        // Docker image → machine image, with OpenRC installed by the daemon.
        let started = Instant::now();
        let image = tokio::time::timeout(IMAGE_IMPORT_BUDGET, async {
            let mut events = images
                .import(ImportMachineImageRequest {
                    name: IMPORTED_IMAGE.to_owned(),
                    docker_image: if local_rootfs.is_some() { String::new() } else { source.clone() },
                    path: local_rootfs.clone().unwrap_or_default(),
                    add_init: local_rootfs.is_none(),
                    ..Default::default()
                })
                .await
                .context("import failed to start")?
                .into_inner();
            let mut phases = Vec::new();
            while let Some(event) = events.message().await.context("import failed")? {
                let phase = event.phase();
                if phases.last() != Some(&phase) {
                    phases.push(phase);
                }
                if phase == ImportMachineImagePhase::Done {
                    if local_rootfs.is_none() && !phases.contains(&ImportMachineImagePhase::Building) {
                        bail!("add_init never reported the building phase: {phases:?}");
                    }
                    return event.image.context("the done event carries no image");
                }
            }
            bail!("the import ended without a done event: {phases:?}")
        })
        .await
        .context("image import timed out")??;
        tracing::info!(elapsed = ?started.elapsed(), size = image.size, "image imported");
        if !image.imported || image.distro != distro {
            bail!("unexpected imported image: {image:?}");
        }

        // A machine boots from it.
        tokio::time::timeout(
            CREATE_BUDGET,
            machines.create(CreateMachineRequest {
                name: IMPORTED_MACHINE.to_owned(),
                cpus: 1,
                memory: 1024 * 1024 * 1024,
                disk_size: 2 * 1024 * 1024 * 1024,
                image: IMPORTED_IMAGE.to_owned(),
                isolated: true,
                timezone: "Asia/Tokyo".into(),
                locale: "C.UTF-8".into(),
                ..Default::default()
            }),
        )
        .await
        .context("create timed out")?
        .context("create --image failed")?;
        proxies
            .set_machine_proxy(arcbox_protocol::v1::SetMachineProxyRequest {
                machine: IMPORTED_MACHINE.into(),
                choice: proxy.id.clone(),
            })
            .await?;
        start_and_check(&mut machines, IMPORTED_MACHINE, &distro).await?;
        if check_vnc {
            vnc::verify(&mut machines, IMPORTED_MACHINE, data_dir).await?;
        }
        // Deliberately do not sync from the test. An orderly stop must
        // preserve both the shim's staged binary and fresh guest writes.
        let checksums = exec_in(&mut machines, IMPORTED_MACHINE, "head -c 1048576 /dev/urandom > /var/tmp/acceptance-write; sha256sum /bin/busybox /var/tmp/acceptance-write").await?;
        let listed = list_images(&mut images).await?;
        let users = listed
            .iter()
            .find(|i| i.name == IMPORTED_IMAGE)
            .map(|i| i.machines.clone());
        if users != Some(vec![IMPORTED_MACHINE.to_owned()]) {
            bail!("the image does not list its machine: {users:?}");
        }

        // An export carries the image; then the host loses both.
        machines
            .stop(StopMachineRequest {
                id: IMPORTED_MACHINE.to_owned(),
                force: false,
            })
            .await
            .context("stop failed")?;
        start_and_check(&mut machines, IMPORTED_MACHINE, &distro).await?;
        anyhow::ensure!(
            exec_in(&mut machines, IMPORTED_MACHINE, "sha256sum /bin/busybox /var/tmp/acceptance-write").await? == checksums,
            "orderly stop lost guest writes"
        );
        machines.stop(StopMachineRequest { id: IMPORTED_MACHINE.into(), force: false }).await?;
        tokio::time::timeout(
            ARCHIVE_BUDGET,
            machines.export(ExportMachineRequest {
                id: IMPORTED_MACHINE.to_owned(),
                path: archive.to_string_lossy().into_owned(),
                include_image: Some(true),
            }),
        )
        .await
        .context("export timed out")?
        .context("export failed")?;
        remove_machine(&mut machines, IMPORTED_MACHINE).await?;
        remove_image(&mut images).await?;

        // Importing the archive restores the image along with the machine.
        tokio::time::timeout(
            ARCHIVE_BUDGET,
            machines.import(ImportMachineRequest {
                path: archive.to_string_lossy().into_owned(),
                name: RESTORED_MACHINE.to_owned(),
                ..Default::default()
            }),
        )
        .await
        .context("machine import timed out")?
        .context("machine import failed")?;
        if !list_images(&mut images)
            .await?
            .iter()
            .any(|i| i.name == IMPORTED_IMAGE)
        {
            bail!("importing the archive did not restore {IMPORTED_IMAGE}");
        }
        proxies
            .set_machine_proxy(arcbox_protocol::v1::SetMachineProxyRequest {
                machine: RESTORED_MACHINE.into(),
                choice: proxy.id.clone(),
            })
            .await?;
        start_and_check(&mut machines, RESTORED_MACHINE, &distro).await?;
        anyhow::ensure!(
            exec_in(&mut machines, RESTORED_MACHINE, "sha256sum /bin/busybox /var/tmp/acceptance-write").await? == checksums,
            "export/import changed guest file contents"
        );
        machines
            .stop(StopMachineRequest {
                id: RESTORED_MACHINE.to_owned(),
                force: false,
            })
            .await
            .context("stop failed")?;
        remove_machine(&mut machines, RESTORED_MACHINE).await?;
        remove_image(&mut images).await
    })
}

/// Starts machine `id` and checks it booted the imported Alpine with an
/// init system.
async fn start_and_check(
    machines: &mut MachineServiceClient<Channel>,
    id: &str,
    distro: &str,
) -> Result<()> {
    tokio::time::timeout(
        START_BUDGET,
        machines.start(StartMachineRequest { id: id.to_owned() }),
    )
    .await
    .with_context(|| format!("starting {id} timed out"))?
    .with_context(|| format!("starting {id} failed"))?;
    let release = exec_in(machines, id, ". /etc/os-release; printf '%s' \"$ID\"").await?;
    if release.trim() != distro {
        bail!("{id} does not run the imported {distro}: {release:?}");
    }
    exec_in(machines, id, "test -e /sbin/init; cat /proc/1/comm").await?;
    let offset = exec_in(machines, id, "date +%z").await?;
    if offset.trim() != "+0900" {
        bail!("{id}: Asia/Tokyo was not applied: {offset:?}");
    }
    tracing::info!(
        id,
        distro,
        offset = offset.trim(),
        "imported machine boot and timezone verified"
    );
    Ok(())
}

async fn list_images(
    images: &mut MachineImageServiceClient<Channel>,
) -> Result<Vec<arcbox_protocol::v1::MachineImageInfo>> {
    Ok(images
        .list(ListMachineImagesRequest {})
        .await
        .context("listing machine images failed")?
        .into_inner()
        .images)
}

async fn remove_image(images: &mut MachineImageServiceClient<Channel>) -> Result<()> {
    images
        .remove(RemoveMachineImageRequest {
            name: IMPORTED_IMAGE.to_owned(),
            ..Default::default()
        })
        .await
        .context("removing the image failed")?;
    if list_images(images)
        .await?
        .iter()
        .any(|i| i.name == IMPORTED_IMAGE)
    {
        bail!("{IMPORTED_IMAGE} still listed after remove");
    }
    Ok(())
}

async fn remove_machine(machines: &mut MachineServiceClient<Channel>, id: &str) -> Result<()> {
    machines
        .remove(RemoveMachineRequest {
            id: id.to_owned(),
            force: false,
            volumes: false,
        })
        .await
        .with_context(|| format!("removing {id} failed"))?;
    Ok(())
}

fn scenario(daemon: &mut DaemonHandle, data_dir: &Path, metrics: &mut RunMetrics) -> Result<()> {
    metrics.time("daemon_ready", || daemon.wait_ready_blocking(READY_TIMEOUT))?;
    let socket = daemon.grpc_socket();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(async {
        let channel = connect_unix(&socket).await?;
        let mut machines = MachineServiceClient::new(channel.clone());

        // Create: pulls alpine from the live mirror and registers the
        // machine with the boot shim contract.
        tokio::time::timeout(
            CREATE_BUDGET,
            machines.create(CreateMachineRequest {
                name: MACHINE.to_owned(),
                cpus: 1,
                memory: 1024 * 1024 * 1024,
                disk_size: 2 * 1024 * 1024 * 1024,
                distro: "alpine".to_owned(),
                // The mirror carries multiple alpine releases; pin one so the
                // resolver is unambiguous.
                version: "3.24".to_owned(),
                ..Default::default()
            }),
        )
        .await
        .context("create timed out (CDN pull)")?
        .context("create failed")?;

        // Start blocks until the agent is reachable and reports a routable
        // IP — the whole shim → overlay → distro-init chain must work.
        tokio::time::timeout(
            START_BUDGET,
            machines.start(StartMachineRequest {
                id: MACHINE.to_owned(),
            }),
        )
        .await
        .context("start timed out")?
        .context("start failed")?;

        // Inspect: running, addressed, timestamped.
        let info = machines
            .inspect(InspectMachineRequest {
                id: MACHINE.to_owned(),
            })
            .await
            .context("inspect failed")?
            .into_inner();
        if info.state != "running" {
            bail!("machine is {:?} after start", info.state);
        }
        let ip = info.network.as_ref().map(|n| n.ip_address.clone());
        let Some(ip) = ip.filter(|ip| !ip.is_empty()) else {
            bail!("machine has no IP after readiness");
        };
        if info.started_at.is_none() {
            bail!("started_at not set after start");
        }
        tracing::info!(%ip, "machine running");

        // System info comes from the agent inside the machine root.
        let sys = tokio::time::timeout(
            RPC_BUDGET,
            machines.get_system_info(MachineAgentRequest {
                id: MACHINE.to_owned(),
            }),
        )
        .await
        .context("system info timed out")?
        .context("system info failed")?
        .into_inner();
        if sys.hostname.is_empty() || sys.total_memory == 0 {
            bail!("implausible system info: {sys:?}");
        }
        if !sys.ip_addresses.contains(&ip) {
            bail!(
                "agent does not report the machine IP {ip}: {:?}",
                sys.ip_addresses
            );
        }

        exec_piped(&mut machines).await?;
        exec_interactive(&mut machines).await?;
        stats_first_frame(channel.clone()).await?;
        let mount = machine_root_mounted_on_the_host(data_dir, &mut machines).await?;

        // Graceful stop, then remove; the registry must forget the machine.
        machines
            .stop(StopMachineRequest {
                id: MACHINE.to_owned(),
                force: false,
            })
            .await
            .context("stop failed")?;
        let info = machines
            .inspect(InspectMachineRequest {
                id: MACHINE.to_owned(),
            })
            .await
            .context("inspect after stop failed")?
            .into_inner();
        if info.state != "stopped" {
            bail!("machine is {:?} after stop", info.state);
        }
        // The mount went with the machine; the daemon removes its mount
        // point too, so a stopped machine has no directory on the host. The
        // unmount runs on the daemon's loop, which the stop RPC does not
        // wait for, so give it a moment.
        let gone = Instant::now() + Duration::from_secs(10);
        while mount.exists() {
            if Instant::now() >= gone {
                bail!("{} still exists after the machine stopped", mount.display());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        machines
            .remove(RemoveMachineRequest {
                id: MACHINE.to_owned(),
                force: false,
                volumes: false,
            })
            .await
            .context("remove failed")?;
        let listed = machines
            .list(ListMachinesRequest { all: true })
            .await
            .context("list failed")?
            .into_inner()
            .machines;
        if listed.iter().any(|m| m.name == MACHINE) {
            bail!("machine still listed after remove");
        }
        Ok(())
    })
}

/// The machine's root is mounted on the host under the harness's host mount
/// root (`ARCBOX_HOST_MOUNT_DIR`, pointed into the data dir by
/// `DaemonHandle::spawn`): `/etc/os-release` reads through it, a file the
/// host writes is what the machine reads, and a file the machine writes is
/// what the host reads. Returns the mount point so the stop path can check
/// it is gone.
async fn machine_root_mounted_on_the_host(
    data_dir: &Path,
    machines: &mut MachineServiceClient<Channel>,
) -> Result<PathBuf> {
    let mount = host_mounts(data_dir).machine(MACHINE);
    let os_release = mount.join("etc/os-release");
    let deadline = Instant::now() + MOUNT_BUDGET;
    let release = loop {
        if let Ok(content) = std::fs::read_to_string(&os_release) {
            break content;
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not appear within {MOUNT_BUDGET:?}; the daemon did not mount the machine's root",
                os_release.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    if !release.contains("ID=alpine") {
        bail!("os-release read through the mount is not alpine's: {release:?}");
    }
    tracing::info!(mount = %mount.display(), "machine root mounted on the host");

    // Host → machine: a file written on the Mac, read inside the machine.
    let marker = format!("from-host-{}", std::process::id());
    std::fs::write(mount.join("root/from-host"), &marker).context("writing through the mount")?;
    let seen = exec_output(machines, "cat /root/from-host").await?;
    if seen.trim() != marker {
        bail!("the machine read {seen:?} from the file the host wrote ({marker:?})");
    }

    // Machine → host: a file written inside the machine, read on the Mac.
    exec_output(machines, "echo from-machine > /root/from-machine").await?;
    let from_machine = std::fs::read_to_string(mount.join("root/from-machine"))
        .context("reading the machine's file through the mount")?;
    if from_machine.trim() != "from-machine" {
        bail!("the host read {from_machine:?} from the file the machine wrote");
    }
    Ok(mount)
}

/// Runs `sh -c <script>` in the machine and returns its stdout; a non-zero
/// exit is an error.
async fn exec_output(machines: &mut MachineServiceClient<Channel>, script: &str) -> Result<String> {
    exec_in(machines, MACHINE, script).await
}

/// Runs `sh -c <script>` in machine `id` and returns its stdout; a non-zero
/// exit is an error.
async fn exec_in(
    machines: &mut MachineServiceClient<Channel>,
    id: &str,
    script: &str,
) -> Result<String> {
    let mut stream = tokio::time::timeout(
        RPC_BUDGET,
        machines.exec(MachineExecRequest {
            id: id.to_owned(),
            cmd: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            ..Default::default()
        }),
    )
    .await
    .context("exec timed out")?
    .context("exec failed")?
    .into_inner();

    let mut stdout = Vec::new();
    let mut exit_code = None;
    while let Some(output) = tokio::time::timeout(RPC_BUDGET, stream.message())
        .await
        .context("exec output timed out")?
        .context("exec stream error")?
    {
        if output.stream == "stdout" {
            stdout.extend_from_slice(&output.data);
        }
        if output.done {
            exit_code = Some(output.exit_code);
        }
    }
    if exit_code != Some(0) {
        bail!("`{script}` exited with {exit_code:?}");
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Piped exec: separate stdout stream, exit code on the final frame.
async fn exec_piped(machines: &mut MachineServiceClient<Channel>) -> Result<()> {
    let mut stream = tokio::time::timeout(
        RPC_BUDGET,
        machines.exec(MachineExecRequest {
            id: MACHINE.to_owned(),
            cmd: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "echo machine-exec-ok".to_owned(),
            ],
            ..Default::default()
        }),
    )
    .await
    .context("exec timed out")?
    .context("exec failed")?
    .into_inner();

    let mut stdout = Vec::new();
    let mut exit_code = None;
    while let Some(output) = tokio::time::timeout(RPC_BUDGET, stream.message())
        .await
        .context("exec output timed out")?
        .context("exec stream error")?
    {
        if output.stream == "stdout" {
            stdout.extend_from_slice(&output.data);
        }
        if output.done {
            exit_code = Some(output.exit_code);
        }
    }
    if exit_code != Some(0) {
        bail!("exec exit code {exit_code:?}");
    }
    if !String::from_utf8_lossy(&stdout).contains("machine-exec-ok") {
        bail!("exec stdout missing marker: {stdout:?}");
    }
    Ok(())
}

/// Interactive session: PTY-merged output, driven over the bidi stream.
async fn exec_interactive(machines: &mut MachineServiceClient<Channel>) -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<MachineExecInput>(4);
    tx.send(MachineExecInput {
        payload: Some(machine_exec_input::Payload::Init(MachineExecRequest {
            id: MACHINE.to_owned(),
            cmd: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "echo interactive-ok".to_owned(),
            ],
            tty: true,
            tty_size: Some(TerminalSize {
                width: 80,
                height: 24,
            }),
            ..Default::default()
        })),
    })
    .await
    .ok();
    // Stdin EOF: the command needs no input.
    tx.send(MachineExecInput {
        payload: Some(machine_exec_input::Payload::Stdin(Vec::new())),
    })
    .await
    .ok();
    drop(tx);

    let mut stream =
        tokio::time::timeout(RPC_BUDGET, machines.exec_session(ReceiverStream::new(rx)))
            .await
            .context("exec session timed out")?
            .context("exec session failed")?
            .into_inner();

    let mut output = Vec::new();
    let mut exit_code = None;
    while let Some(frame) = tokio::time::timeout(RPC_BUDGET, stream.message())
        .await
        .context("session output timed out")?
        .context("session stream error")?
    {
        output.extend_from_slice(&frame.data);
        if frame.done {
            exit_code = Some(frame.exit_code);
        }
    }
    if exit_code != Some(0) {
        bail!("session exit code {exit_code:?}");
    }
    if !String::from_utf8_lossy(&output).contains("interactive-ok") {
        bail!("session output missing marker: {output:?}");
    }
    Ok(())
}

/// Per-machine stats: the first frame must arrive and be sane.
async fn stats_first_frame(channel: Channel) -> Result<()> {
    let mut stats = StatsServiceClient::new(channel);
    let mut stream = stats
        .watch(StatsWatchRequest {
            machine_id: MACHINE.to_owned(),
        })
        .await
        .context("stats watch failed")?
        .into_inner();
    let sample = tokio::time::timeout(RPC_BUDGET, stream.message())
        .await
        .context("no stats frame in budget")?
        .context("stats stream error")?
        .context("stats stream ended")?;
    if sample.memory_total_bytes == 0 || sample.online_cpus == 0 {
        bail!("implausible machine stats: {sample:?}");
    }
    Ok(())
}
