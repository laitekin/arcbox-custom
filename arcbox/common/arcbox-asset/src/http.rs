//! The daemon's host-side HTTP egress.
//!
//! Guest traffic is sent through the operator's proxy by the datapath; the
//! daemon's own downloads (machine images, image indexes, Docker CLI tools)
//! leave from the host process and would otherwise follow only the
//! environment — empty under launchd — so they bypassed the proxy. The
//! daemon installs its live [`ProxyState`] once with [`install_proxy`]; every
//! client from [`client_builder`] then resolves the proxy per request from
//! the current snapshot, so a settings change applies to the next request
//! without rebuilding clients. A process that installs nothing (the CLI)
//! keeps reqwest's default proxy behavior.

use std::sync::OnceLock;

use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use arcbox_fakeip::proxy_state::{ProxySnapshot, ProxyState};
use reqwest::Url;

/// User-Agent for every host-side request.
pub const USER_AGENT: &str = concat!("arcbox/", env!("CARGO_PKG_VERSION"));

static PROXY: OnceLock<ProxyState> = OnceLock::new();

/// Routes this process's HTTP egress through `state`. The first call wins;
/// returns whether this one did.
pub fn install_proxy(state: ProxyState) -> bool {
    PROXY.set(state).is_ok()
}

/// A client builder whose proxy follows the installed policy.
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().user_agent(USER_AGENT);
    match PROXY.get() {
        None => builder,
        Some(state) => {
            let state = state.clone();
            builder.no_proxy().proxy(reqwest::Proxy::custom(move |url| {
                proxy_for(&state.load(), url)
            }))
        }
    }
}

/// A client from [`client_builder`] with no further options.
///
/// # Errors
///
/// Returns reqwest's error when the TLS backend cannot be initialized.
pub fn client() -> reqwest::Result<reqwest::Client> {
    client_builder().build()
}

/// The proxy a request to `url` goes through under `snapshot`, as a URL
/// carrying its credentials; `None` means direct.
///
/// The choice mirrors the guest TCP egress: a SOCKS5 proxy first (with
/// remote name resolution), then HTTPS/HTTP CONNECT. Loopback targets and
/// the bypass list stay direct.
#[must_use]
pub fn proxy_for(snapshot: &ProxySnapshot, url: &Url) -> Option<Url> {
    let env = snapshot.environment.as_ref()?;
    let host = url.host_str()?;
    if is_loopback(host) || env.should_bypass(host) {
        return None;
    }
    choose(env)
}

fn choose(env: &ProxyEnvironment) -> Option<Url> {
    if let Some(socks) = &env.socks_proxy {
        return proxy_url("socks5h", socks);
    }
    // `https://` in the policy means a CONNECT proxy reached in plain HTTP
    // (see `ProxyScheme::Https`), so both map to an `http://` proxy URL.
    env.https_proxy
        .as_ref()
        .or(env.http_proxy.as_ref())
        .and_then(|proxy| proxy_url("http", proxy))
}

fn proxy_url(scheme: &str, proxy: &ProxyConfig) -> Option<Url> {
    let host = if proxy.host.contains(':') {
        format!("[{}]", proxy.host)
    } else {
        proxy.host.clone()
    };
    let mut url = Url::parse(&format!("{scheme}://{host}:{}", proxy.port)).ok()?;
    if let Some(auth) = &proxy.auth {
        url.set_username(auth.username()).ok()?;
        // A missing password still sends the user, so the proxy rejects the
        // request instead of it leaving directly.
        url.set_password(auth.password()).ok()?;
    }
    Some(url)
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arcbox_fakeip::proxy_auth::ProxyAuth;

    use super::*;

    fn snapshot(env: ProxyEnvironment) -> ProxySnapshot {
        ProxySnapshot {
            environment: Some(env),
            ..Default::default()
        }
    }

    fn server(host: &str, port: u16, auth: Option<(&str, &str)>) -> ProxyConfig {
        ProxyConfig {
            host: host.into(),
            port,
            auth: auth.map(|(user, password)| {
                Arc::new(ProxyAuth::new(user.into(), Some(password.into())).unwrap())
            }),
        }
    }

    fn url(s: &str) -> Url {
        s.parse().unwrap()
    }

    #[test]
    fn socks_wins_and_carries_encoded_credentials() {
        let env = ProxyEnvironment {
            socks_proxy: Some(server("10.0.0.2", 1080, Some(("alice", "p@ss:w")))),
            http_proxy: Some(server("10.0.0.3", 3128, None)),
            ..Default::default()
        };
        let proxy = proxy_for(&snapshot(env), &url("https://image.example/x")).unwrap();
        assert_eq!(proxy.scheme(), "socks5h");
        assert_eq!(proxy.username(), "alice");
        assert_eq!(proxy.password(), Some("p%40ss%3Aw"));
        assert_eq!(proxy.host_str(), Some("10.0.0.2"));
        assert_eq!(proxy.port(), Some(1080));
    }

    #[test]
    fn http_proxies_and_ipv6_hosts_map_to_http_urls() {
        let env = ProxyEnvironment {
            https_proxy: Some(server("::1", 8443, None)),
            ..Default::default()
        };
        let proxy = proxy_for(&snapshot(env), &url("https://image.example/x")).unwrap();
        assert_eq!(proxy.as_str(), "http://[::1]:8443/");
    }

    #[test]
    fn direct_policies_bypass_entries_and_loopback_stay_direct() {
        assert!(proxy_for(&ProxySnapshot::default(), &url("https://image.example/")).is_none());

        let env = ProxyEnvironment {
            socks_proxy: Some(server("10.0.0.2", 1080, None)),
            bypass_domains: vec!["*.corp.example".into()],
            ..Default::default()
        };
        let snap = snapshot(env);
        assert!(proxy_for(&snap, &url("https://mirror.corp.example/")).is_none());
        assert!(proxy_for(&snap, &url("http://127.0.0.1:8080/")).is_none());
        assert!(proxy_for(&snap, &url("http://localhost/")).is_none());
        assert!(proxy_for(&snap, &url("https://image.example/")).is_some());
    }
}
