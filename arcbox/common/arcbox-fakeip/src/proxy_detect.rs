//! Host VPN/proxy environment detection.
//!
//! Detects whether the host is running a VPN or proxy that intercepts network
//! traffic, and extracts configuration details (system proxy, fake-ip ranges).
//!
//! This information is used by the TCP shim to choose the optimal connection
//! strategy: direct connect, HTTP CONNECT tunnel, or SOCKS5 tunnel. The
//! `scutil`/`ifconfig` probes are macOS-specific and `cfg`-gated; on other
//! targets `detect()` returns the environment-variable-only result.

use std::net::{IpAddr, Ipv4Addr};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;

/// Proxy server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub auth: Option<Arc<crate::proxy_auth::ProxyAuth>>,
}

/// Detected proxy/VPN environment on the host.
#[derive(Debug, Clone, Default)]
pub struct ProxyEnvironment {
    /// Whether a fake-ip DNS proxy is active (Surge, Clash).
    /// Detected by checking for utun interfaces with 198.18.0.0/15 addresses.
    pub fake_ip_active: bool,

    /// System HTTP proxy (from `scutil --proxy`).
    pub http_proxy: Option<ProxyConfig>,

    /// System HTTPS proxy.
    pub https_proxy: Option<ProxyConfig>,

    /// System SOCKS proxy.
    pub socks_proxy: Option<ProxyConfig>,

    /// Forward external gateway DNS through SOCKS5 (`socks5h` policy).
    pub remote_dns: bool,

    /// Domains that bypass the proxy (from ExceptionsList).
    pub bypass_domains: Vec<String>,
}

impl ProxyEnvironment {
    /// Detects the current proxy/VPN environment.
    ///
    /// Checks:
    /// 1. utun interfaces for fake-ip indicators (198.18.0.0/15)
    /// 2. `scutil --proxy` for system proxy settings
    /// 3. Environment variables (`HTTP_PROXY`, `HTTPS_PROXY`)
    #[must_use]
    pub fn detect() -> Self {
        let fake_ip_active = detect_fake_ip_utun();

        let (http_proxy, https_proxy, socks_proxy, bypass_domains) = detect_system_proxy();

        // Also check environment variables as fallback.
        let http_proxy = http_proxy
            .or_else(|| proxy_from_env("HTTP_PROXY").or_else(|| proxy_from_env("http_proxy")));
        let https_proxy = https_proxy
            .or_else(|| proxy_from_env("HTTPS_PROXY").or_else(|| proxy_from_env("https_proxy")));

        let env = Self {
            fake_ip_active,
            http_proxy,
            https_proxy,
            socks_proxy,
            remote_dns: false,
            bypass_domains,
        };

        if env.fake_ip_active
            || env.http_proxy.is_some()
            || env.https_proxy.is_some()
            || env.socks_proxy.is_some()
        {
            tracing::info!(
                fake_ip = env.fake_ip_active,
                http_proxy = env
                    .http_proxy
                    .as_ref()
                    .map(|p| format!("{}:{}", p.host, p.port))
                    .as_deref(),
                https_proxy = env
                    .https_proxy
                    .as_ref()
                    .map(|p| format!("{}:{}", p.host, p.port))
                    .as_deref(),
                socks_proxy = env
                    .socks_proxy
                    .as_ref()
                    .map(|p| format!("{}:{}", p.host, p.port))
                    .as_deref(),
                bypass_count = env.bypass_domains.len(),
                "proxy environment detected"
            );
        } else {
            tracing::debug!("no proxy environment detected");
        }

        env
    }

    /// Whether the given IP falls within the fake-ip range (198.18.0.0/15).
    ///
    /// This is the standard range used by Surge and Clash for DNS fake-ip mode.
    #[must_use]
    pub fn is_fake_ip(&self, ip: Ipv4Addr) -> bool {
        if !self.fake_ip_active {
            return false;
        }
        let octets = ip.octets();
        // 198.18.0.0/15 = 198.18.x.x and 198.19.x.x
        octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
    }

    /// Whether traffic to the given domain should bypass the proxy.
    #[must_use]
    pub fn should_bypass(&self, domain: &str) -> bool {
        let domain_lower = domain.trim_end_matches('.').to_ascii_lowercase();
        let ip = domain_lower.trim_matches(['[', ']']).parse::<IpAddr>().ok();
        for pattern in &self.bypass_domains {
            let pat = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
            if pat == "*" {
                return true;
            }
            if let Some((network, prefix)) = pat.split_once('/') {
                if let (Some(ip), Ok(network), Ok(prefix)) =
                    (ip, network.parse::<IpAddr>(), prefix.parse::<u32>())
                    && ip_in_network(ip, network, prefix)
                {
                    return true;
                }
            } else if let Some(bare) = pat.strip_prefix("*.").or_else(|| pat.strip_prefix('.')) {
                let suffix = format!(".{bare}");
                if domain_lower.ends_with(suffix.as_str()) || domain_lower == bare {
                    return true;
                }
            } else if domain_lower == pat || ip.is_some_and(|ip| pat.parse::<IpAddr>() == Ok(ip)) {
                return true;
            }
        }
        false
    }

    /// Whether a usable proxy tunnel is available.
    #[must_use]
    pub fn has_usable_proxy(&self) -> bool {
        self.http_proxy.is_some() || self.https_proxy.is_some() || self.socks_proxy.is_some()
    }
}

/// Matches CIDRs without resolving names or crossing address families.
fn ip_in_network(ip: IpAddr, network: IpAddr, prefix: u32) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) if prefix <= 32 => {
            let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) if prefix <= 128 => {
            let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
            u128::from(ip) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod bypass_tests {
    use super::*;

    #[test]
    fn no_proxy_supports_suffixes_addresses_networks_and_all() {
        let mut env = ProxyEnvironment {
            bypass_domains: vec![
                ".Corp.Example".into(),
                "10.0.0.0/8".into(),
                "fd00::/8".into(),
            ],
            ..Default::default()
        };
        for host in ["corp.example", "API.CORP.EXAMPLE.", "10.8.1.2", "fd01::1"] {
            assert!(env.should_bypass(host), "{host}");
        }
        for host in [
            "evilcorp.example",
            "corp.example.attacker.test",
            "11.0.0.1",
            "fe80::1",
        ] {
            assert!(!env.should_bypass(host), "{host}");
        }
        env.bypass_domains = vec!["::1".into(), "192.0.2.5/32".into(), "10.0.0.0/33".into()];
        assert!(env.should_bypass("[::1]"));
        assert!(env.should_bypass("192.0.2.5"));
        assert!(!env.should_bypass("192.0.2.6"));
        assert!(!env.should_bypass("10.0.0.1"));
        env.bypass_domains = vec!["0.0.0.0/0".into()];
        assert!(env.should_bypass("203.0.113.1"));
        assert!(!env.should_bypass("::1"));
        env.bypass_domains = vec!["*".into()];
        assert!(env.should_bypass("anything.example"));
    }
}

/// Detects fake-ip VPN by checking for utun interfaces with 198.18.x.x addresses.
///
/// Surge and Clash create a utun with address 198.18.0.1 when in fake-ip mode.
#[cfg(target_os = "macos")]
fn detect_fake_ip_utun() -> bool {
    let Ok(output) = Command::new("ifconfig").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);

    // Look for "inet 198.18." on any utun interface.
    let mut in_utun = false;
    for line in text.lines() {
        if line.starts_with("utun") {
            in_utun = true;
        } else if !line.starts_with('\t') && !line.starts_with(' ') {
            in_utun = false;
        }
        if in_utun && line.contains("inet 198.18.") {
            return true;
        }
    }
    false
}

/// On non-macOS targets there is no `scutil`/`ifconfig` Fake-IP probe; the
/// environment is derived from environment variables only (see `detect`).
#[cfg(not(target_os = "macos"))]
fn detect_fake_ip_utun() -> bool {
    false
}

/// Parses system proxy settings from `scutil --proxy`.
#[cfg(target_os = "macos")]
fn detect_system_proxy() -> (
    Option<ProxyConfig>,
    Option<ProxyConfig>,
    Option<ProxyConfig>,
    Vec<String>,
) {
    let Ok(output) = Command::new("scutil").arg("--proxy").output() else {
        return (None, None, None, Vec::new());
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let http = parse_proxy_block(&text, "HTTPEnable", "HTTPProxy", "HTTPPort");
    let https = parse_proxy_block(&text, "HTTPSEnable", "HTTPSProxy", "HTTPSPort");
    let socks = parse_proxy_block(&text, "SOCKSEnable", "SOCKSProxy", "SOCKSPort");
    let bypass = parse_exceptions_list(&text);

    (http, https, socks, bypass)
}

/// On non-macOS targets there is no `scutil`; the system proxy is unknown and
/// `detect` relies on the environment-variable fallback instead.
#[cfg(not(target_os = "macos"))]
fn detect_system_proxy() -> (
    Option<ProxyConfig>,
    Option<ProxyConfig>,
    Option<ProxyConfig>,
    Vec<String>,
) {
    (None, None, None, Vec::new())
}

/// Parses a proxy block from scutil output.
///
/// Format:
/// ```text
///   HTTPSEnable : 1
///   HTTPSProxy : 127.0.0.1
///   HTTPSPort : 6152
/// ```
#[cfg(target_os = "macos")]
fn parse_proxy_block(
    text: &str,
    enable_key: &str,
    host_key: &str,
    port_key: &str,
) -> Option<ProxyConfig> {
    let enabled = text.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with(enable_key) {
            trimmed.split(':').nth(1).map(|v| v.trim() == "1")
        } else {
            None
        }
    })?;

    if !enabled {
        return None;
    }

    let host = text.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with(host_key) && !trimmed.starts_with(&format!("{host_key}E")) {
            trimmed.split(':').nth(1).map(|v| v.trim().to_string())
        } else {
            None
        }
    })?;

    let port: u16 = text.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with(port_key) {
            trimmed
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
        } else {
            None
        }
    })?;

    if host.is_empty() || port == 0 {
        return None;
    }

    Some(ProxyConfig {
        host,
        port,
        auth: None,
    })
}

/// Parses the ExceptionsList from scutil output.
#[cfg(target_os = "macos")]
fn parse_exceptions_list(text: &str) -> Vec<String> {
    let mut domains = Vec::new();
    let mut in_exceptions = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ExceptionsList") {
            in_exceptions = true;
            continue;
        }
        if in_exceptions {
            if trimmed == "}" {
                break;
            }
            // Lines like: "0 : *.local"
            if let Some(val) = trimmed.split(':').nth(1) {
                let domain = val.trim().to_string();
                if !domain.is_empty() {
                    domains.push(domain);
                }
            }
        }
    }
    domains
}

/// Parses proxy config from an environment variable like `http://host:port`.
fn proxy_from_env(var: &str) -> Option<ProxyConfig> {
    let val = std::env::var(var).ok()?;
    parse_proxy_url(&val)
}

/// Parses a proxy URL like `http://host:port` or `socks5://[::1]:1080`.
fn parse_proxy_url(url: &str) -> Option<ProxyConfig> {
    url.parse::<crate::proxy_policy::ProxyUrl>()
        .ok()
        .map(|value| value.server)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: parse proxy settings from text directly.
    #[cfg(target_os = "macos")]
    fn detect_system_proxy_from_text(
        text: &str,
    ) -> (
        Option<ProxyConfig>,
        Option<ProxyConfig>,
        Option<ProxyConfig>,
        Vec<String>,
    ) {
        let http = parse_proxy_block(text, "HTTPEnable", "HTTPProxy", "HTTPPort");
        let https = parse_proxy_block(text, "HTTPSEnable", "HTTPSProxy", "HTTPSPort");
        let socks = parse_proxy_block(text, "SOCKSEnable", "SOCKSProxy", "SOCKSPort");
        let bypass = parse_exceptions_list(text);
        (http, https, socks, bypass)
    }

    #[test]
    fn is_fake_ip_range() {
        let env = ProxyEnvironment {
            fake_ip_active: true,
            ..Default::default()
        };
        assert!(env.is_fake_ip(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(env.is_fake_ip(Ipv4Addr::new(198, 18, 2, 17)));
        assert!(env.is_fake_ip(Ipv4Addr::new(198, 19, 255, 255)));
        assert!(!env.is_fake_ip(Ipv4Addr::new(198, 20, 0, 1)));
        assert!(!env.is_fake_ip(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn is_fake_ip_returns_false_when_not_active() {
        let env = ProxyEnvironment::default();
        assert!(!env.is_fake_ip(Ipv4Addr::new(198, 18, 0, 1)));
    }

    #[test]
    fn bypass_exact_match() {
        let env = ProxyEnvironment {
            bypass_domains: vec!["localhost".to_string(), "*.local".to_string()],
            ..Default::default()
        };
        assert!(env.should_bypass("localhost"));
        assert!(env.should_bypass("foo.local"));
        assert!(!env.should_bypass("example.com"));
    }

    #[test]
    fn proxy_from_env_parsing() {
        // Test the parsing logic directly instead of using env vars
        // (set_var is unsafe in Rust 2024 edition).
        let p = super::parse_proxy_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 8080);

        let p = super::parse_proxy_url("socks5://[::1]:1080").unwrap();
        assert_eq!(p.host, "::1");
        assert_eq!(p.port, 1080);

        assert!(super::parse_proxy_url("no-port").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_scutil_proxy_block() {
        let text = r"
<dictionary> {
  HTTPSEnable : 1
  HTTPSPort : 6152
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 0
}
";
        let (http, https, socks, _) = detect_system_proxy_from_text(text);
        assert!(http.is_none());
        assert!(socks.is_none());
        let https = https.unwrap();
        assert_eq!(https.host, "127.0.0.1");
        assert_eq!(https.port, 6152);
    }
}
