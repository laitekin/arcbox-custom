//! Step-by-step proxy diagnostics run from the host.
//!
//! [`diagnose`] walks the same path a guest flow takes through the proxy —
//! reach the proxy, authenticate, tunnel to a target, and (SOCKS5 only)
//! relay a UDP DNS query — and reports each step as it finishes, so a
//! client can tell "cannot reach the proxy" from "wrong password" from
//! "the proxy refuses the target" from "no UDP support". It proves the
//! host-side leg only; whether a VM's traffic takes that leg is the egress
//! policy's job and is covered by its own tests.

use std::io;
use std::time::{Duration, Instant};

use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use tokio::net::TcpStream;

use crate::error::ProxyError;
use crate::socks5::{self, Socks5UdpAssociation};

/// Upper bound on each step.
const STEP_TIMEOUT: Duration = Duration::from_secs(8);

/// The public resolver the UDP step queries through the relay.
pub const UDP_PROBE_RESOLVER: (&str, u16) = ("1.1.1.1", 53);

/// One stage of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Which proxy is tested (always succeeds; names it, or says "direct").
    Select,
    /// TCP connection to the proxy.
    Reach,
    /// SOCKS5 method negotiation and RFC 1929 authentication. HTTP proxies
    /// authenticate inside the CONNECT request, reported as [`Self::Tunnel`].
    Authenticate,
    /// CONNECT to the target.
    Tunnel,
    /// UDP ASSOCIATE and one DNS query through the relay (SOCKS5 only).
    Udp,
}

/// Why a step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The proxy's address did not answer or refused the connection.
    Unreachable,
    /// The step did not finish within its deadline.
    Timeout,
    /// The proxy rejected the credentials.
    AuthRejected,
    /// The proxy offered no method the configuration allows (for example,
    /// credentials configured but the proxy only speaks no-auth).
    NoAcceptableMethod,
    /// A user is configured but its password is not available.
    MissingPassword,
    /// The proxy refused or could not reach the target.
    TargetRefused,
    /// The proxy does not support UDP ASSOCIATE, or relayed no answer.
    UdpUnsupported,
    /// The proxy spoke something unexpected.
    Protocol,
}

/// The outcome of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Which stage this is.
    pub kind: StepKind,
    /// Whether it succeeded. A skipped step is neither ok nor failed.
    pub ok: bool,
    /// The step was not applicable (direct egress, HTTP proxy for UDP).
    pub skipped: bool,
    /// Time the step took.
    pub latency: Duration,
    /// Why it failed, when it did.
    pub failure: Option<FailureKind>,
    /// Human-readable detail; never contains a password.
    pub message: String,
}

impl Step {
    fn ok(kind: StepKind, latency: Duration, message: impl Into<String>) -> Self {
        Self {
            kind,
            ok: true,
            skipped: false,
            latency,
            failure: None,
            message: message.into(),
        }
    }

    fn skipped(kind: StepKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            ok: false,
            skipped: true,
            latency: Duration::ZERO,
            failure: None,
            message: message.into(),
        }
    }

    fn failed(kind: StepKind, latency: Duration, failure: FailureKind, message: String) -> Self {
        Self {
            kind,
            ok: false,
            skipped: false,
            latency,
            failure: Some(failure),
            message,
        }
    }
}

/// Which proxy a target's flows would use and its protocol.
enum Chosen<'a> {
    Socks(&'a ProxyConfig),
    Http(&'a ProxyConfig),
}

/// Runs the diagnostic walk for `target` (`host`, `port`) against the
/// proxy `environment` would choose, calling `report` after every step.
/// Stops at the first failed step; later steps are not reported.
pub async fn diagnose(
    environment: Option<&ProxyEnvironment>,
    target: (&str, u16),
    mut report: impl FnMut(Step),
) {
    let (host, port) = target;
    let chosen = environment.and_then(|env| {
        if env.should_bypass(host) {
            None
        } else if let Some(socks) = &env.socks_proxy {
            Some(Chosen::Socks(socks))
        } else {
            env.https_proxy
                .as_ref()
                .or(env.http_proxy.as_ref())
                .map(Chosen::Http)
        }
    });
    let Some(chosen) = chosen else {
        report(Step::skipped(
            StepKind::Select,
            format!("{host}:{port} connects directly (no proxy applies)"),
        ));
        return;
    };
    let (proxy, scheme) = match chosen {
        Chosen::Socks(proxy) => (proxy, "socks5"),
        Chosen::Http(proxy) => (proxy, "http"),
    };
    let user = proxy
        .auth
        .as_ref()
        .map(|auth| format!("{}@", auth.username()))
        .unwrap_or_default();
    report(Step::ok(
        StepKind::Select,
        Duration::ZERO,
        format!("{scheme}://{user}{}", authority(proxy)),
    ));

    // Reach.
    let started = Instant::now();
    let reached = timed(TcpStream::connect((proxy.host.as_str(), proxy.port))).await;
    let mut stream = match reached {
        Ok(stream) => {
            report(Step::ok(
                StepKind::Reach,
                started.elapsed(),
                format!("connected to {}", authority(proxy)),
            ));
            stream
        }
        Err(e) => {
            report(failure(StepKind::Reach, started, &e));
            return;
        }
    };

    match chosen {
        Chosen::Socks(_) => {
            let started = Instant::now();
            match timed(socks5::greet(&mut stream, proxy.auth.as_deref())).await {
                Ok(()) => report(Step::ok(
                    StepKind::Authenticate,
                    started.elapsed(),
                    if proxy.auth.is_some() {
                        "username/password accepted"
                    } else {
                        "no authentication required"
                    },
                )),
                Err(e) => {
                    report(failure(StepKind::Authenticate, started, &e));
                    return;
                }
            }
            let started = Instant::now();
            match timed(socks5::request_connect(&mut stream, host, port)).await {
                Ok(()) => report(Step::ok(
                    StepKind::Tunnel,
                    started.elapsed(),
                    format!("tunnel to {host}:{port} established"),
                )),
                Err(e) => {
                    report(failure(StepKind::Tunnel, started, &e));
                    return;
                }
            }
            drop(stream);
            report(udp_step(proxy, host).await);
        }
        Chosen::Http(_) => {
            drop(stream);
            let started = Instant::now();
            match crate::http_connect::connect_via_http_proxy_config(proxy, host, port).await {
                Ok(_) => report(Step::ok(
                    StepKind::Tunnel,
                    started.elapsed(),
                    format!("tunnel to {host}:{port} established"),
                )),
                Err(e) => {
                    report(failure(StepKind::Tunnel, started, &e));
                    return;
                }
            }
            report(Step::skipped(
                StepKind::Udp,
                "an HTTP proxy cannot carry UDP; see the udp policy",
            ));
        }
    }
}

async fn udp_step(proxy: &ProxyConfig, name: &str) -> Step {
    let started = Instant::now();
    let association = match Socks5UdpAssociation::associate_config(proxy).await {
        Ok(association) => association,
        Err(e) => {
            let mut step = failure(StepKind::Udp, started, &e);
            if matches!(ProxyError::from_io(&e), Some(ProxyError::Refused(0x07))) {
                step.failure = Some(FailureKind::UdpUnsupported);
            }
            return step;
        }
    };
    let query = dns_query(name);
    let (resolver, resolver_port) = UDP_PROBE_RESOLVER;
    let exchange = async {
        association.send_to(&query, resolver, resolver_port).await?;
        let mut buf = vec![0u8; 65_535];
        loop {
            if let Some((len, _, _)) = association.recv_from(&mut buf).await?
                && len >= 2
                && buf[..2] == query[..2]
            {
                return Ok::<_, io::Error>(());
            }
        }
    };
    match timed(exchange).await {
        Ok(()) => Step::ok(
            StepKind::Udp,
            started.elapsed(),
            format!("DNS query to {resolver}:{resolver_port} relayed"),
        ),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Step::failed(
            StepKind::Udp,
            started.elapsed(),
            FailureKind::UdpUnsupported,
            "UDP association opened but no reply was relayed".into(),
        ),
        Err(e) => failure(StepKind::Udp, started, &e),
    }
}

/// A minimal recursive A query for `name` (or `example.com` when `name` is
/// an IP literal), ID 0xAB0C.
fn dns_query(name: &str) -> Vec<u8> {
    let name = if name.parse::<std::net::IpAddr>().is_ok() {
        "example.com"
    } else {
        name
    };
    let mut query = vec![0xAB, 0x0C, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.trim_end_matches('.').split('.') {
        let label = &label.as_bytes()[..label.len().min(63)];
        query.push(label.len() as u8);
        query.extend_from_slice(label);
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    query
}

async fn timed<T>(future: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(STEP_TIMEOUT, future)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))?
}

fn failure(kind: StepKind, started: Instant, error: &io::Error) -> Step {
    let failure = match ProxyError::from_io(error) {
        Some(ProxyError::AuthRejected) => FailureKind::AuthRejected,
        Some(ProxyError::NoAcceptableMethod) => FailureKind::NoAcceptableMethod,
        Some(ProxyError::MissingPassword) => FailureKind::MissingPassword,
        Some(ProxyError::Refused(_)) => FailureKind::TargetRefused,
        Some(ProxyError::Protocol(_)) => FailureKind::Protocol,
        None => match error.kind() {
            io::ErrorKind::TimedOut => FailureKind::Timeout,
            _ if kind == StepKind::Reach => FailureKind::Unreachable,
            io::ErrorKind::ConnectionRefused if kind == StepKind::Tunnel => {
                FailureKind::TargetRefused
            }
            _ if kind == StepKind::Tunnel => FailureKind::TargetRefused,
            _ => FailureKind::Protocol,
        },
    };
    Step::failed(kind, started.elapsed(), failure, error.to_string())
}

fn authority(proxy: &ProxyConfig) -> String {
    if proxy.host.contains(':') {
        format!("[{}]:{}", proxy.host, proxy.port)
    } else {
        format!("{}:{}", proxy.host, proxy.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_query_encodes_labels_and_substitutes_ip_literals() {
        let query = dns_query("www.gstatic.com");
        assert_eq!(
            &query[12..],
            b"\x03www\x07gstatic\x03com\x00\x00\x01\x00\x01"
        );
        let query = dns_query("10.0.0.1");
        assert_eq!(&query[12..], b"\x07example\x03com\x00\x00\x01\x00\x01");
    }
}
