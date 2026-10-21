//! Machine image service — the local Linux machine image registry.

mod docker_export;

use std::path::PathBuf;
use std::sync::Arc;

use arcbox_connect::v1 as pb;
use arcbox_core::Runtime;
use arcbox_core::machine_image::{self, MachineImage};
use arcbox_image::machine_import::{
    DEFAULT_MAX_IMPORT_BYTES, ImportPhase, ImportRequest, ImportSource, is_imported,
};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::ConnectRuntimeExt as _;
use super::SharedRuntime;

/// Machine image service implementation.
pub struct MachineImageServiceImpl {
    runtime: SharedRuntime,
}

impl MachineImageServiceImpl {
    /// Creates the service over a deferred runtime.
    #[must_use]
    pub fn new(runtime: SharedRuntime) -> Self {
        Self { runtime }
    }
}

/// The rootfs paths machines boot from.
fn rootfs_in_use(runtime: &Runtime) -> Vec<(String, PathBuf)> {
    runtime
        .machine_manager()
        .list()
        .into_iter()
        .flat_map(|machine| {
            machine
                .block_devices
                .into_iter()
                .map(move |device| (machine.name.clone(), PathBuf::from(device.path)))
        })
        .collect()
}

fn image_info(image: &MachineImage, in_use: &[(String, PathBuf)]) -> pb::MachineImageInfo {
    let manifest = &image.manifest;
    let rootfs = image.rootfs_path();
    let mut machines: Vec<String> = in_use
        .iter()
        .filter(|(_, path)| path == &rootfs)
        .map(|(name, _)| name.clone())
        .collect();
    machines.sort_unstable();
    machines.dedup();
    pb::MachineImageInfo {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        distro: manifest.distro.clone(),
        release: manifest.release.clone(),
        release_title: manifest.release_title.clone(),
        arch: manifest.arch.clone(),
        imported: is_imported(manifest),
        source: manifest.upstream.product.clone(),
        size: manifest.rootfs.size,
        sha256: manifest.rootfs.sha256.clone(),
        machines,
        ..Default::default()
    }
}

fn phase(phase: ImportPhase) -> pb::ImportMachineImagePhase {
    use pb::ImportMachineImagePhase as P;
    match phase {
        ImportPhase::Reading => P::IMPORT_MACHINE_IMAGE_PHASE_READING,
        ImportPhase::Checking => P::IMPORT_MACHINE_IMAGE_PHASE_CHECKING,
        ImportPhase::Writing => P::IMPORT_MACHINE_IMAGE_PHASE_WRITING,
        ImportPhase::Registering => P::IMPORT_MACHINE_IMAGE_PHASE_REGISTERING,
    }
}

fn image_error(e: &arcbox_image::ImageError) -> ConnectError {
    use arcbox_error::CommonError;
    match e {
        arcbox_image::ImageError::Common(CommonError::NotFound(_)) => {
            ConnectError::not_found(e.to_string())
        }
        arcbox_image::ImageError::Common(CommonError::AlreadyExists(_)) => {
            ConnectError::already_exists(e.to_string())
        }
        arcbox_image::ImageError::Common(CommonError::InvalidState(_)) => {
            ConnectError::failed_precondition(e.to_string())
        }
        arcbox_image::ImageError::Common(CommonError::Io(_)) => {
            ConnectError::internal(e.to_string())
        }
        _ => ConnectError::invalid_argument(e.to_string()),
    }
}

#[allow(
    refining_impl_trait,
    reason = "the trait returns `impl Encodable<M>`; naming the concrete body \
              type is strictly more informative and these impls are registered on a \
              Router rather than named by callers"
)]
impl pb::MachineImageService for MachineImageServiceImpl {
    async fn list(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListMachineImagesRequest>,
    ) -> ServiceResult<pb::ListMachineImagesResponse> {
        let runtime = self.runtime.ready()?;
        let in_use = rootfs_in_use(runtime);
        let mut images: Vec<_> = runtime
            .machine_image_manager()
            .list()
            .iter()
            .map(|image| image_info(image, &in_use))
            .collect();
        images.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        Response::ok(pb::ListMachineImagesResponse {
            images,
            ..Default::default()
        })
    }

    async fn list_remote(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListRemoteMachineImagesRequest>,
    ) -> ServiceResult<pb::ListRemoteMachineImagesResponse> {
        let req = request.to_owned_message();
        let runtime = self.runtime.ready()?;
        let arch = if req.arch.is_empty() {
            machine_image::host_image_arch().to_owned()
        } else {
            machine_image::image_arch(&req.arch).to_owned()
        };
        let registry = runtime.machine_image_manager();
        let streams = registry
            .list_remote(&arch)
            .await
            .map_err(|e| ConnectError::unavailable(format!("image source: {e}")))?;
        Response::ok(pb::ListRemoteMachineImagesResponse {
            images: streams
                .into_iter()
                .map(|s| pb::RemoteMachineImage {
                    name: s.name,
                    release_title: s.release_title.unwrap_or_else(|| s.release.clone()),
                    distro: s.distro,
                    release: s.release,
                    arch: s.arch,
                    latest: s.latest,
                    ..Default::default()
                })
                .collect(),
            base: registry.base().to_string(),
            ..Default::default()
        })
    }

    async fn import(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ImportMachineImageRequest>,
    ) -> ServiceResult<ServiceStream<pb::ImportMachineImageEvent>> {
        let req = request.to_owned_message();
        let runtime = Arc::clone(self.runtime.ready()?);
        let source = match (req.path.is_empty(), req.docker_image.is_empty()) {
            (false, true) => {
                let path = PathBuf::from(&req.path);
                if !path.is_absolute() {
                    return Err(ConnectError::invalid_argument(
                        "the path must be absolute: the daemon reads it on its own host",
                    ));
                }
                Source::File(path)
            }
            (true, false) => Source::Docker {
                image: req.docker_image.clone(),
                add_init: req.add_init,
            },
            _ => {
                return Err(ConnectError::invalid_argument(
                    "set exactly one of path and docker_image",
                ));
            }
        };
        if req.add_init && !matches!(source, Source::Docker { .. }) {
            return Err(ConnectError::invalid_argument(
                "add_init needs a docker_image; install an init into the archive itself",
            ));
        }
        let optional = |s: String| (!s.is_empty()).then_some(s);
        let (distro, release) = (optional(req.distro), optional(req.release));
        let max_bytes = if req.max_size == 0 {
            DEFAULT_MAX_IMPORT_BYTES
        } else {
            req.max_size
        };
        let name = req.name;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let progress = tx.clone();
        let socket = runtime.config().docker.socket_path.clone();
        let task = tokio::task::spawn_blocking(move || {
            // The derived image (add_init) is bound first so it is dropped
            // last: it must outlive the export of the container made from it.
            let (_derived, source) = match source {
                Source::File(path) => (None, ImportSource::File(path)),
                Source::Docker { image, add_init } => {
                    let (derived, image) = if add_init {
                        let _ = progress.send(Ok(event(
                            pb::ImportMachineImagePhase::IMPORT_MACHINE_IMAGE_PHASE_BUILDING,
                            0,
                        )));
                        let tag = format!("arcbox-machine-import/{name}:latest");
                        let built =
                            docker_export::DerivedImage::build_with_init(&socket, &image, &tag)
                                .map_err(|e| ConnectError::failed_precondition(e.to_string()))?;
                        let tag = built.tag().to_owned();
                        (Some(built), tag)
                    } else {
                        (None, image)
                    };
                    let _ = progress.send(Ok(event(
                        pb::ImportMachineImagePhase::IMPORT_MACHINE_IMAGE_PHASE_EXPORTING,
                        0,
                    )));
                    let export = docker_export::DockerExport::open(&socket, &image)
                        .map_err(|e| ConnectError::failed_precondition(e.to_string()))?;
                    tracing::info!(
                        container = export.container_id(),
                        image,
                        "exporting a Docker image as a machine rootfs"
                    );
                    let source = ImportSource::TarStream {
                        reader: Box::new(export),
                        description: format!("docker:{image}"),
                    };
                    (derived, source)
                }
            };
            let mut last = None;
            runtime
                .machine_image_manager()
                .import(
                    ImportRequest {
                        name,
                        source,
                        distro,
                        release,
                        max_bytes,
                    },
                    |p, bytes| {
                        // Reading reports per file; forward at most one
                        // event per MiB so a large rootfs does not flood.
                        let key = (p, bytes >> 20);
                        if last != Some(key) {
                            last = Some(key);
                            let _ = progress.send(Ok(event(phase(p), bytes)));
                        }
                    },
                )
                .map(|outcome| {
                    let in_use = rootfs_in_use(&runtime);
                    pb::ImportMachineImageEvent {
                        phase: pb::ImportMachineImagePhase::IMPORT_MACHINE_IMAGE_PHASE_DONE.into(),
                        bytes: outcome.image.manifest.rootfs.size,
                        image: image_info(&outcome.image, &in_use).into(),
                        warnings: outcome.warnings,
                        ..Default::default()
                    }
                })
                .map_err(|e| image_error(&e))
        });
        tokio::spawn(async move {
            let last = match task.await {
                Ok(result) => result,
                Err(e) => Err(ConnectError::internal(format!("import task failed: {e}"))),
            };
            let _ = tx.send(last);
        });
        Response::ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }

    async fn remove(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemoveMachineImageRequest>,
    ) -> ServiceResult<pb::Empty> {
        let req = request.to_owned_message();
        let runtime = Arc::clone(self.runtime.ready()?);
        tokio::task::spawn_blocking(move || {
            let registry = runtime.machine_image_manager();
            let version = if req.version.is_empty() {
                let mut versions: Vec<String> = registry
                    .list()
                    .into_iter()
                    .filter(|image| image.manifest.name == req.name)
                    .map(|image| image.manifest.version)
                    .collect();
                match versions.len() {
                    0 => {
                        return Err(ConnectError::not_found(format!(
                            "no local machine image {}",
                            req.name
                        )));
                    }
                    1 => versions.remove(0),
                    _ => {
                        versions.sort_unstable();
                        return Err(ConnectError::invalid_argument(format!(
                            "{} has several versions ({}); name one as {}@<version>",
                            req.name,
                            versions.join(", "),
                            req.name
                        )));
                    }
                }
            } else {
                req.version
            };
            let in_use: Vec<PathBuf> = rootfs_in_use(&runtime)
                .into_iter()
                .map(|(_, path)| path)
                .collect();
            registry
                .remove(&req.name, &version, &in_use)
                .map_err(|e| image_error(&e))
        })
        .await
        .map_err(|e| ConnectError::internal(format!("remove task failed: {e}")))??;
        Response::ok(pb::Empty::default())
    }
}

enum Source {
    File(PathBuf),
    Docker { image: String, add_init: bool },
}

fn event(phase: pb::ImportMachineImagePhase, bytes: u64) -> pb::ImportMachineImageEvent {
    pb::ImportMachineImageEvent {
        phase: phase.into(),
        bytes,
        ..Default::default()
    }
}
