//! Egress proxy service — the guest egress proxy settings and their test.
//!
//! Served from the early runtime handle, not the ready one: a proxy the
//! daemon needs while booting (asset downloads, a stuck VM) has to be
//! fixable before boot finishes. Every call reaches the keychain or probes
//! the Mac's settings, so the work runs on the blocking pool.

use std::sync::Arc;
use std::time::Duration;

use arcbox_connect::v1 as pb;
use arcbox_core::network_proxy::{PasswordChange, ProxyPreferences, ProxySettingsView};
use arcbox_fakeip::proxy_policy::{ProxyPolicy, ProxyUrl};
use arcbox_fakeip::proxy_state::{IcmpPolicy, UdpPolicy};
use arcbox_proxy::diagnose::{self, FailureKind, Step, StepKind};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;
use zeroize::Zeroizing;

use super::ConnectRuntimeExt as _;
use super::SharedRuntime;

/// The target tunnelled to when a test names none.
const DEFAULT_TARGET: (&str, u16) = ("www.gstatic.com", 443);

/// Egress proxy service implementation.
pub struct EgressProxyServiceImpl {
    /// The runtime as soon as it is constructed, before the VM boots.
    early_runtime: SharedRuntime,
}

impl EgressProxyServiceImpl {
    /// Creates the service over the early runtime handle.
    #[must_use]
    pub fn new(early_runtime: SharedRuntime) -> Self {
        Self { early_runtime }
    }

    fn controller(&self) -> Result<Arc<arcbox_core::network_proxy::NetworkProxy>, ConnectError> {
        Ok(Arc::clone(self.early_runtime.ready()?.network_proxy()))
    }
}

#[allow(
    refining_impl_trait,
    reason = "the trait returns `impl Encodable<M>`; naming the concrete body \
              type is strictly more informative and these impls are registered on a \
              Router rather than named by callers"
)]
impl pb::EgressProxyService for EgressProxyServiceImpl {
    async fn get_proxy(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::Empty>,
    ) -> ServiceResult<pb::EgressProxySettings> {
        let controller = self.controller()?;
        let view = blocking(move || Ok(controller.settings())).await?;
        Response::ok(settings_to_proto(view))
    }

    async fn set_proxy(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetEgressProxyRequest>,
    ) -> ServiceResult<pb::EgressProxySettings> {
        let req = request.to_owned_message();
        let settings = req
            .settings
            .as_option()
            .ok_or_else(|| ConnectError::invalid_argument("settings are required"))?;
        let preferences = preferences_from_proto(settings)?;
        let password = match req.password {
            None => PasswordChange::Keep,
            Some(password) if password.is_empty() => PasswordChange::Clear,
            Some(password) => PasswordChange::Set(Zeroizing::new(password)),
        };
        let controller = self.controller()?;
        let view = blocking(move || {
            controller
                .apply(preferences, password)
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))
        })
        .await?;
        Response::ok(settings_to_proto(view))
    }

    async fn test_proxy(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::TestEgressProxyRequest>,
    ) -> ServiceResult<ServiceStream<pb::EgressProxyTestStep>> {
        let req = request.to_owned_message();
        let target = parse_target(&req.target)?;
        let draft = req
            .settings
            .as_option()
            .map(preferences_from_proto)
            .transpose()?;
        let password = req.password.map(Zeroizing::new);
        let controller = self.controller()?;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let environment = match draft {
                None => Ok(controller.state().load().environment.clone()),
                Some(draft) => tokio::task::spawn_blocking(move || {
                    controller
                        .resolve_draft(&draft, password.as_deref().map(String::as_str))
                        .map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(format!("proxy test task failed: {e}"))),
            };
            match environment {
                Ok(environment) => {
                    diagnose::diagnose(
                        environment.as_ref(),
                        (target.0.as_str(), target.1),
                        |step| {
                            let _ = tx.send(step_to_proto(&step));
                        },
                    )
                    .await;
                }
                Err(message) => {
                    let _ = tx.send(pb::EgressProxyTestStep {
                        step: pb::EgressProxyTestStepKind::EGRESS_PROXY_TEST_STEP_KIND_SELECT
                            .into(),
                        failure: pb::EgressProxyTestFailure::EGRESS_PROXY_TEST_FAILURE_CONFIG
                            .into(),
                        message,
                        ..Default::default()
                    });
                }
            }
        });

        let stream = UnboundedReceiverStream::new(rx).map(Ok::<_, ConnectError>);
        Response::ok(Box::pin(stream))
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ConnectError> + Send + 'static,
) -> Result<T, ConnectError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ConnectError::internal(format!("egress proxy task failed: {e}")))?
}

fn parse_target(target: &str) -> Result<(String, u16), ConnectError> {
    let target = target.trim();
    if target.is_empty() {
        return Ok((DEFAULT_TARGET.0.to_string(), DEFAULT_TARGET.1));
    }
    let invalid = || ConnectError::invalid_argument("target must be host:port");
    let (host, port) = target.rsplit_once(':').ok_or_else(invalid)?;
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(invalid)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    Ok((host.to_string(), port))
}

fn preferences_from_proto(
    settings: &pb::EgressProxySettings,
) -> Result<ProxyPreferences, ConnectError> {
    let policy = match settings.mode.as_known() {
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM) => ProxyPolicy::System,
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_NONE) => ProxyPolicy::None,
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM) => ProxyPolicy::Custom(
            settings
                .url
                .parse::<ProxyUrl>()
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))?,
        ),
        _ => {
            return Err(ConnectError::invalid_argument(
                "mode must be SYSTEM, NONE or CUSTOM",
            ));
        }
    };
    let udp = match settings.udp.as_known() {
        Some(pb::EgressUdpPolicy::EGRESS_UDP_POLICY_DIRECT) => UdpPolicy::Direct,
        Some(pb::EgressUdpPolicy::EGRESS_UDP_POLICY_BLOCK) => UdpPolicy::Block,
        _ => UdpPolicy::Proxy,
    };
    let icmp = match settings.icmp.as_known() {
        Some(pb::EgressIcmpPolicy::EGRESS_ICMP_POLICY_BLOCK) => IcmpPolicy::Block,
        _ => IcmpPolicy::Direct,
    };
    Ok(ProxyPreferences {
        policy,
        exclude: settings
            .exclude
            .iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect(),
        udp,
        icmp,
    })
}

fn settings_to_proto(view: ProxySettingsView) -> pb::EgressProxySettings {
    let preferences = view.preferences;
    let (mode, url) = match &preferences.policy {
        ProxyPolicy::System => (pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM, String::new()),
        ProxyPolicy::None => (pb::EgressProxyMode::EGRESS_PROXY_MODE_NONE, String::new()),
        // `ProxyUrl`'s Display never includes a password.
        ProxyPolicy::Custom(url) => (
            pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM,
            url.to_string(),
        ),
    };
    pb::EgressProxySettings {
        mode: mode.into(),
        url,
        has_password: view.has_password,
        exclude: preferences.exclude,
        udp: match preferences.udp {
            UdpPolicy::Proxy => pb::EgressUdpPolicy::EGRESS_UDP_POLICY_PROXY,
            UdpPolicy::Direct => pb::EgressUdpPolicy::EGRESS_UDP_POLICY_DIRECT,
            UdpPolicy::Block => pb::EgressUdpPolicy::EGRESS_UDP_POLICY_BLOCK,
        }
        .into(),
        icmp: match preferences.icmp {
            IcmpPolicy::Direct => pb::EgressIcmpPolicy::EGRESS_ICMP_POLICY_DIRECT,
            IcmpPolicy::Block => pb::EgressIcmpPolicy::EGRESS_ICMP_POLICY_BLOCK,
        }
        .into(),
        effective: view.effective,
        credential_store: view.credential_store.to_string(),
        revision: view.revision,
        ..Default::default()
    }
}

fn step_to_proto(step: &Step) -> pb::EgressProxyTestStep {
    use pb::EgressProxyTestFailure as F;
    use pb::EgressProxyTestStepKind as K;
    let kind = match step.kind {
        StepKind::Select => K::EGRESS_PROXY_TEST_STEP_KIND_SELECT,
        StepKind::Reach => K::EGRESS_PROXY_TEST_STEP_KIND_REACH,
        StepKind::Authenticate => K::EGRESS_PROXY_TEST_STEP_KIND_AUTHENTICATE,
        StepKind::Tunnel => K::EGRESS_PROXY_TEST_STEP_KIND_TUNNEL,
        StepKind::Udp => K::EGRESS_PROXY_TEST_STEP_KIND_UDP,
    };
    let failure = match step.failure {
        None => F::EGRESS_PROXY_TEST_FAILURE_UNSPECIFIED,
        Some(FailureKind::Unreachable) => F::EGRESS_PROXY_TEST_FAILURE_UNREACHABLE,
        Some(FailureKind::Timeout) => F::EGRESS_PROXY_TEST_FAILURE_TIMEOUT,
        Some(FailureKind::AuthRejected) => F::EGRESS_PROXY_TEST_FAILURE_AUTH_REJECTED,
        Some(FailureKind::NoAcceptableMethod) => F::EGRESS_PROXY_TEST_FAILURE_NO_ACCEPTABLE_METHOD,
        Some(FailureKind::MissingPassword) => F::EGRESS_PROXY_TEST_FAILURE_MISSING_PASSWORD,
        Some(FailureKind::TargetRefused) => F::EGRESS_PROXY_TEST_FAILURE_TARGET_REFUSED,
        Some(FailureKind::UdpUnsupported) => F::EGRESS_PROXY_TEST_FAILURE_UDP_UNSUPPORTED,
        Some(FailureKind::Protocol) => F::EGRESS_PROXY_TEST_FAILURE_PROTOCOL,
    };
    pb::EgressProxyTestStep {
        step: kind.into(),
        ok: step.ok,
        skipped: step.skipped,
        latency_ms: u64::try_from(step.latency.as_millis()).unwrap_or(u64::MAX),
        failure: failure.into(),
        message: step.message.clone(),
        ..Default::default()
    }
}

/// How often to re-probe anyway while change notifications are watched: a
/// safety net for a missed notification.
const WATCHED_POLL: Duration = Duration::from_secs(300);

/// Notifications arrive in bursts (one per service as macOS re-applies its
/// settings); settle before re-probing once.
const SETTLE: Duration = Duration::from_millis(500);

/// Follows changes to the Mac's proxy settings under the `system` policy.
///
/// A change in System Settings reaches new flows without a daemon restart.
/// On macOS a dynamic-store notification triggers the re-probe and polling
/// drops to a slow safety net; elsewhere it re-probes every `interval`.
/// Runs until the task is dropped.
pub async fn follow_system_proxy(early_runtime: SharedRuntime, interval: Duration) {
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::channel::<()>(1);
    let interval = match arcbox_core::network_proxy::watch_system_proxy(move || {
        let _ = wake_tx.try_send(());
    }) {
        Ok(()) => {
            tracing::debug!("watching the Mac's proxy settings for changes");
            interval.max(WATCHED_POLL)
        }
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => interval,
        Err(e) => {
            tracing::warn!(error = %e, "cannot watch the Mac's proxy settings; polling instead");
            interval
        }
    };
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            Some(()) = wake_rx.recv() => {
                tokio::time::sleep(SETTLE).await;
                while wake_rx.try_recv().is_ok() {}
            }
        }
        let Ok(runtime) = early_runtime.ready() else {
            continue;
        };
        let controller = Arc::clone(runtime.network_proxy());
        let changed = tokio::task::spawn_blocking(move || controller.refresh_system()).await;
        if matches!(changed, Ok(true)) {
            tracing::info!("the Mac's proxy settings changed; egress follows");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse_with_defaults_and_ipv6() {
        assert_eq!(
            parse_target("").unwrap(),
            ("www.gstatic.com".to_string(), 443)
        );
        assert_eq!(
            parse_target("example.com:80").unwrap(),
            ("example.com".to_string(), 80)
        );
        assert_eq!(
            parse_target("[::1]:8443").unwrap(),
            ("::1".to_string(), 8443)
        );
        for bad in ["example.com", "example.com:0", ":80", "a b:1"] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn settings_round_trip_without_the_password() {
        let settings = pb::EgressProxySettings {
            mode: pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM.into(),
            url: "socks5://alice:do-not-leak@10.0.0.2:1080".into(),
            exclude: vec![" .corp.example ".into(), String::new()],
            udp: pb::EgressUdpPolicy::EGRESS_UDP_POLICY_BLOCK.into(),
            ..Default::default()
        };
        let preferences = preferences_from_proto(&settings).unwrap();
        assert_eq!(preferences.exclude, vec![".corp.example"]);
        assert_eq!(preferences.udp, UdpPolicy::Block);
        assert_eq!(preferences.icmp, IcmpPolicy::Direct);

        let back = settings_to_proto(ProxySettingsView {
            preferences,
            has_password: true,
            effective: vec![],
            credential_store: "memory",
            revision: 3,
            profile_id: String::new(),
        });
        assert_eq!(back.url, "socks5://alice@10.0.0.2:1080");
        assert_eq!(back.revision, 3);
    }

    #[test]
    fn an_unspecified_mode_is_rejected() {
        assert!(preferences_from_proto(&pb::EgressProxySettings::default()).is_err());
    }
}
