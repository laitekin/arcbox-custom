//! Host-only proxy profile management. Passwords are write-only.

use std::sync::Arc;

use arcbox_connect::v1 as pb;
use arcbox_core::network_proxy::library::SavedProxyView;
use arcbox_core::network_proxy::{NetworkProxy, PasswordChange};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use zeroize::Zeroizing;

use super::egress_proxy::{preferences_from_proto, settings_to_proto};
use super::{ConnectRuntimeExt as _, SharedRuntime};

pub struct ProxyLibraryServiceImpl {
    runtime: SharedRuntime,
}

impl ProxyLibraryServiceImpl {
    pub fn new(runtime: SharedRuntime) -> Self {
        Self { runtime }
    }
    fn controller(&self) -> Result<Arc<NetworkProxy>, ConnectError> {
        Ok(Arc::clone(self.runtime.ready()?.network_proxy()))
    }
}

#[allow(
    refining_impl_trait,
    reason = "concrete protobuf responses implement Encodable"
)]
impl pb::ProxyLibraryService for ProxyLibraryServiceImpl {
    async fn list_profiles(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, pb::Empty>,
    ) -> ServiceResult<pb::ProxyLibrary> {
        let controller = self.controller()?;
        let library = blocking(move || {
            let profiles = controller
                .saved_proxies()?
                .into_iter()
                .map(profile)
                .collect();
            Ok(pb::ProxyLibrary {
                profiles,
                default_profile_id: controller.proxy_library().default_profile,
                ..Default::default()
            })
        })
        .await?;
        Response::ok(library)
    }

    async fn save_profile(
        &self,
        _: RequestContext,
        request: ServiceRequest<'_, pb::SaveProxyProfileRequest>,
    ) -> ServiceResult<pb::ProxyProfile> {
        let req = request.to_owned_message();
        let settings = req
            .settings
            .as_option()
            .ok_or_else(|| ConnectError::invalid_argument("settings are required"))?;
        let preferences = preferences_from_proto(settings)?;
        let password = match req.password {
            None => PasswordChange::Keep,
            Some(value) if value.is_empty() => PasswordChange::Clear,
            Some(value) => PasswordChange::Set(Zeroizing::new(value)),
        };
        let controller = self.controller()?;
        let saved =
            blocking(move || controller.save_proxy(&req.id, &req.name, preferences, password))
                .await?;
        Response::ok(profile(saved))
    }

    async fn remove_profile(
        &self,
        _: RequestContext,
        request: ServiceRequest<'_, pb::ProxyProfileRequest>,
    ) -> ServiceResult<pb::Empty> {
        let id = request.to_owned_message().id;
        let controller = self.controller()?;
        blocking(move || controller.remove_proxy(&id)).await?;
        Response::ok(pb::Empty::default())
    }

    async fn get_machine_proxy(
        &self,
        _: RequestContext,
        request: ServiceRequest<'_, pb::MachineProxyRequest>,
    ) -> ServiceResult<pb::MachineProxyAssignment> {
        let name = request.to_owned_message().machine;
        let runtime = self.runtime.ready()?;
        if name == "default" || runtime.machine_manager().get(&name).is_none() {
            return Err(ConnectError::not_found("Linux machine not found"));
        }
        let controller = self.controller()?;
        let result = blocking(move || assignment(&controller, &name)).await?;
        Response::ok(result)
    }

    async fn set_machine_proxy(
        &self,
        _: RequestContext,
        request: ServiceRequest<'_, pb::SetMachineProxyRequest>,
    ) -> ServiceResult<pb::MachineProxyAssignment> {
        let req = request.to_owned_message();
        let runtime = self.runtime.ready_for_write(&req.machine)?;
        if !req.choice.is_empty() && runtime.machine_manager().get(&req.machine).is_none() {
            return Err(ConnectError::not_found("Linux machine not found"));
        }
        let controller = self.controller()?;
        let result = blocking(move || {
            controller.set_machine_proxy(&req.machine, &req.choice)?;
            assignment(&controller, &req.machine)
        })
        .await?;
        Response::ok(result)
    }
}

fn profile(view: SavedProxyView) -> pb::ProxyProfile {
    pb::ProxyProfile {
        id: view.id,
        name: view.name,
        settings: settings_to_proto(view.settings).into(),
        machines: view.machines,
        is_default: view.is_default,
        ..Default::default()
    }
}

fn assignment(
    controller: &NetworkProxy,
    name: &str,
) -> arcbox_core::Result<pb::MachineProxyAssignment> {
    let (choice, settings) = controller.machine_proxy(name)?;
    Ok(pb::MachineProxyAssignment {
        machine: name.to_string(),
        choice,
        effective: settings_to_proto(settings).into(),
        ..Default::default()
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> arcbox_core::Result<T> + Send + 'static,
) -> Result<T, ConnectError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ConnectError::internal(format!("proxy library task failed: {e}")))?
        .map_err(|e| ConnectError::invalid_argument(e.to_string()))
}
