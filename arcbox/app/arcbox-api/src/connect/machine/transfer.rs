//! Export and import: the machine archive's two ends, composed from the
//! engine's archive and the daemon's image registry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arcbox_connect::v1 as pb;
use arcbox_core::machine::MachineConfig;
use arcbox_core::machine::archive::read_manifest;
use arcbox_core::machine_image::{self, MachineImage};
use arcbox_core::{HostCapacity, Runtime};
use connectrpc::ConnectError;

use super::{refusal, rootfs};

/// Writes machine `id` to the archive at `path`; the image manifest it
/// carries is the registry entry the machine's rootfs device belongs to.
/// The image itself rides along when `include_image` says so, or — when it
/// says nothing — when the image was imported on this host, since no image
/// source publishes it for the importing host to pull.
pub(super) async fn export(
    runtime: &Arc<Runtime>,
    id: String,
    path: String,
    include_image: Option<bool>,
) -> Result<pb::ExportMachineResponse, ConnectError> {
    let path = archive_path(&path)?;
    let machine = runtime
        .machine_manager()
        .get(&id)
        .ok_or_else(|| ConnectError::not_found(format!("machine '{id}' not found")))?;
    let image = runtime
        .machine_image_manager()
        .list()
        .into_iter()
        .find(|image| {
            let rootfs = image.rootfs_path();
            machine
                .block_devices
                .iter()
                .any(|device| Path::new(&device.path) == rootfs)
        })
        .ok_or_else(|| {
            ConnectError::failed_precondition(format!(
                "machine '{id}' boots a rootfs that is not in the local image registry, so an \
                 archive could not name the image to restore it under"
            ))
        })?;

    let carry = include_image
        .unwrap_or_else(|| arcbox_image::machine_import::is_imported(&image.manifest))
        .then(|| image.dir.clone());
    let manager = Arc::clone(runtime.machine_manager());
    let archive = path.clone();
    let size = tokio::task::spawn_blocking(move || {
        manager.export(&id, &archive, image.manifest, carry.as_deref())
    })
    .await
    .map_err(|e| ConnectError::internal(format!("export task panicked: {e}")))?
    .map_err(refusal)?;
    Ok(pb::ExportMachineResponse {
        path: path.to_string_lossy().into_owned(),
        size,
        ..Default::default()
    })
}

/// Creates a machine from the archive at `req.path`.
pub(super) async fn import(
    runtime: &Arc<Runtime>,
    req: pb::ImportMachineRequest,
) -> Result<pb::ImportMachineResponse, ConnectError> {
    let path = archive_path(&req.path)?;
    let manifest_path = path.clone();
    let manifest = tokio::task::spawn_blocking(move || read_manifest(&manifest_path))
        .await
        .map_err(|e| ConnectError::internal(format!("import task panicked: {e}")))?
        .map_err(refusal)?;
    let archived = manifest.machine;
    let name = if req.name.is_empty() {
        archived.name.clone()
    } else {
        req.name
    };
    runtime
        .machine_manager()
        .ensure_name_available(&name)
        .map_err(refusal)?;
    if manifest.image_included {
        register_carried_image(runtime, &path, &manifest.image).await?;
    }
    let image = local_image(runtime, &manifest.image)?;

    let cpus = if req.cpus == 0 {
        archived.cpus
    } else {
        req.cpus
    };
    let memory_mb = if req.memory == 0 {
        archived.memory_mb
    } else {
        req.memory / (1024 * 1024)
    };
    HostCapacity::probe()
        .check(cpus, memory_mb)
        .map_err(|e| ConnectError::invalid_argument(format!("{e}; import with --cpus/--memory")))?;

    let config = MachineConfig {
        name,
        cpus,
        memory_mb,
        disk_gb: archived.disk_gb,
        kernel: None,
        cmdline: None,
        block_devices: Vec::new(),
        rootfs: Some(rootfs::rootfs_for(runtime, &image).await?),
        mounts: archived.mounts,
        distro: Some(archived.distro.clone()),
        distro_version: archived.distro_version.clone(),
        backend: arcbox_core::VmBackend::default(),
        enable_rosetta: false,
        nested_virt: false,
        isolated: archived.isolated,
    };
    let manager = Arc::clone(runtime.machine_manager());
    let id = tokio::task::spawn_blocking(move || manager.import(config, &path))
        .await
        .map_err(|e| ConnectError::internal(format!("import task panicked: {e}")))?
        .map_err(refusal)?;
    Ok(pb::ImportMachineResponse {
        id,
        distro: archived.distro,
        distro_version: archived.distro_version.unwrap_or_default(),
        ..Default::default()
    })
}

/// Registers the image an archive carries, unless the registry already
/// has that version. The image is staged in the registry directory and
/// verified against its manifest before it lands.
async fn register_carried_image(
    runtime: &Runtime,
    archive: &Path,
    wanted: &machine_image::MachineImageManifest,
) -> Result<(), ConnectError> {
    let registry = Arc::clone(runtime.machine_image_manager());
    if registry.get(&wanted.name, &wanted.version).is_ok() {
        return Ok(());
    }
    let archive = archive.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let staging = registry
            .images_dir()
            .join(format!(".carried-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&staging)?;
        let _cleanup = arcbox_image::remote_image::StagingGuard::new(staging.clone());
        let carried = arcbox_core::machine::archive::extract_image(&archive, &staging)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if !carried {
            return Err(std::io::Error::other(
                "the archive says it carries its image but does not",
            ));
        }
        registry
            .register_carried(&staging)
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    })
    .await
    .map_err(|e| ConnectError::internal(format!("import task panicked: {e}")))?
    .map_err(|e| ConnectError::failed_precondition(format!("the archive's image: {e}")))
}

/// The registry's copy of the image the archive was made with: same stream
/// and version, same rootfs digest, built for this host.
fn local_image(
    runtime: &Runtime,
    wanted: &machine_image::MachineImageManifest,
) -> Result<MachineImage, ConnectError> {
    let host_arch = machine_image::host_image_arch();
    if wanted.arch != host_arch {
        return Err(ConnectError::failed_precondition(format!(
            "the archive was exported from a {} machine; this host runs {host_arch} machines",
            wanted.arch
        )));
    }
    let registry = runtime.machine_image_manager();
    let image = registry.get(&wanted.name, &wanted.version).map_err(|_| {
        let mut present: Vec<String> = registry
            .list()
            .into_iter()
            .filter(|image| image.manifest.name == wanted.name)
            .map(|image| image.manifest.version)
            .collect();
        present.sort_unstable();
        let present = if present.is_empty() {
            "no version of it is present".to_owned()
        } else {
            format!("present versions: {}", present.join(", "))
        };
        ConnectError::not_found(format!(
            "the archive needs machine image {}@{} ({} {}), which is not in the local image \
             registry ({present}); create a machine from that distro to pull it, then import \
             again",
            wanted.name, wanted.version, wanted.distro, wanted.release
        ))
    })?;
    if image.manifest.rootfs.sha256 != wanted.rootfs.sha256 {
        return Err(ConnectError::failed_precondition(format!(
            "local machine image {}@{} differs from the one the archive was made with \
             (rootfs sha256 {} here, {} in the archive)",
            wanted.name, wanted.version, image.manifest.rootfs.sha256, wanted.rootfs.sha256
        )));
    }
    Ok(image)
}

/// An absolute archive path on the daemon's host.
fn archive_path(path: &str) -> Result<PathBuf, ConnectError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(ConnectError::invalid_argument(
            "the archive path must be absolute: the daemon writes and reads it on its own host",
        ));
    }
    Ok(path.to_path_buf())
}
