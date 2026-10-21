//! Guest egress proxy commands (`abctl network proxy ...`).
//!
//! A password is only ever read from a no-echo prompt or from stdin
//! (`--password-stdin`), never from an argument, so it stays out of shell
//! history and `ps`.

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};

use anyhow::{Context, Result, bail};
use arcbox_connect::v1 as pb;
use arcbox_connect::v1::EgressProxyServiceClient;
use clap::{Args, Subcommand, ValueEnum};
use zeroize::Zeroizing;

use super::OutputFormat;
use crate::connect;

#[derive(Debug, Subcommand)]
pub enum NetworkCommands {
    /// Show, change or test where guest egress is sent
    #[command(subcommand)]
    Proxy(ProxyCommands),
}

#[derive(Debug, Subcommand)]
pub enum ProxyCommands {
    /// Show the proxy settings and the proxies actually in use
    Show,
    /// Set the proxy: `system`, `none`, or a URL like socks5://user@host:1080
    ///
    /// A URL with a user needs a password: give it on stdin with
    /// `--password-stdin`, or interactively when prompted. A stored password
    /// is kept when none is given. Takes effect for new connections at once.
    Set(SetArgs),
    /// Go back to following the Mac's proxy settings and forget the
    /// custom proxy's password
    Unset,
    /// Test the proxy path step by step from the host
    Test(TestArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// `system`, `none`, or `socks5://[user@]host:port`, `http://...`
    pub proxy: String,
    /// Read the password from the first line of stdin
    #[arg(long)]
    pub password_stdin: bool,
    /// Remove the stored password
    #[arg(long, conflicts_with = "password_stdin")]
    pub clear_password: bool,
    /// Hosts that bypass the proxy (repeatable; `.example.com` covers
    /// subdomains). Replaces the current list when given.
    #[arg(long = "exclude", value_name = "HOST")]
    pub exclude: Vec<String>,
    /// Clear the bypass list
    #[arg(long, conflicts_with = "exclude")]
    pub no_exclude: bool,
    /// Guest UDP handling; keeps the current setting when omitted
    #[arg(long, value_enum)]
    pub udp: Option<UdpArg>,
    /// Guest ICMP (ping) handling; keeps the current setting when omitted
    #[arg(long, value_enum)]
    pub icmp: Option<IcmpArg>,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Target to tunnel to (host:port); defaults to www.gstatic.com:443
    #[arg(long)]
    pub target: Option<String>,
    /// Test this proxy instead of the saved settings, without saving
    #[arg(long, value_name = "PROXY")]
    pub with: Option<String>,
    /// Read the draft proxy's password from stdin (with `--with`)
    #[arg(long, requires = "with")]
    pub password_stdin: bool,
}

/// CLI spelling of [`pb::EgressUdpPolicy`].
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UdpArg {
    /// SOCKS5 UDP ASSOCIATE; dropped behind an HTTP-only proxy
    Proxy,
    /// Always direct
    Direct,
    /// Always dropped
    Block,
}

/// CLI spelling of [`pb::EgressIcmpPolicy`].
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IcmpArg {
    /// Sent from the host directly
    Direct,
    /// Dropped
    Block,
}

impl From<UdpArg> for pb::EgressUdpPolicy {
    fn from(arg: UdpArg) -> Self {
        match arg {
            UdpArg::Proxy => Self::EGRESS_UDP_POLICY_PROXY,
            UdpArg::Direct => Self::EGRESS_UDP_POLICY_DIRECT,
            UdpArg::Block => Self::EGRESS_UDP_POLICY_BLOCK,
        }
    }
}

impl From<IcmpArg> for pb::EgressIcmpPolicy {
    fn from(arg: IcmpArg) -> Self {
        match arg {
            IcmpArg::Direct => Self::EGRESS_ICMP_POLICY_DIRECT,
            IcmpArg::Block => Self::EGRESS_ICMP_POLICY_BLOCK,
        }
    }
}

fn client() -> EgressProxyServiceClient<connectrpc::client::SharedHttp2Connection> {
    let (transport, config) = connect::daemon(&super::resolve_grpc_socket_path());
    EgressProxyServiceClient::new(transport, config)
}

pub async fn execute(cmd: NetworkCommands, format: OutputFormat) -> Result<()> {
    let NetworkCommands::Proxy(cmd) = cmd;
    match cmd {
        ProxyCommands::Show => {
            let settings = get(&client()).await?;
            print_settings(&settings, format)
        }
        ProxyCommands::Set(args) => set(args, format).await,
        ProxyCommands::Unset => {
            let client = client();
            let mut settings = get(&client).await?;
            settings.mode = pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM.into();
            settings.url.clear();
            let settings = client
                .set_proxy(pb::SetEgressProxyRequest {
                    settings: settings.into(),
                    password: Some(String::new()),
                    ..Default::default()
                })
                .await
                .context("failed to reset the proxy")?
                .into_owned();
            print_settings(&settings, format)
        }
        ProxyCommands::Test(args) => test(args, format).await,
    }
}

async fn get(
    client: &EgressProxyServiceClient<connectrpc::client::SharedHttp2Connection>,
) -> Result<pb::EgressProxySettings> {
    Ok(client
        .get_proxy(pb::Empty::default())
        .await
        .context("failed to query the proxy settings")?
        .into_owned())
}

/// The mode and URL a `system` / `none` / URL argument stands for.
fn parse_proxy_arg(proxy: &str) -> (pb::EgressProxyMode, String) {
    let proxy = proxy.trim();
    if proxy.eq_ignore_ascii_case("system") || proxy.eq_ignore_ascii_case("auto") {
        (pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM, String::new())
    } else if proxy.eq_ignore_ascii_case("none") || proxy.eq_ignore_ascii_case("direct") {
        (pb::EgressProxyMode::EGRESS_PROXY_MODE_NONE, String::new())
    } else {
        (
            pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM,
            proxy.to_string(),
        )
    }
}

/// Whether `url` names a user (`scheme://user@...`) — such a proxy needs a
/// password.
fn has_user(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(_, rest)| rest.contains('@'))
}

/// Whether `url` carries an inline password (`user:pass@`).
fn has_inline_password(url: &str) -> bool {
    url.split_once("://")
        .and_then(|(_, rest)| rest.rsplit_once('@'))
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

async fn set(args: SetArgs, format: OutputFormat) -> Result<()> {
    let client = client();
    let mut settings = get(&client).await?;
    let (mode, url) = parse_proxy_arg(&args.proxy);
    if has_inline_password(&url) {
        bail!(
            "do not put the password in the URL; use socks5://user@host:port with --password-stdin or the prompt"
        );
    }
    let same_proxy = settings.mode.as_known() == Some(mode) && settings.url == url;
    settings.mode = mode.into();
    settings.url.clone_from(&url);
    if args.no_exclude {
        settings.exclude.clear();
    } else if !args.exclude.is_empty() {
        settings.exclude = args.exclude;
    }
    if let Some(udp) = args.udp {
        settings.udp = pb::EgressUdpPolicy::from(udp).into();
    }
    if let Some(icmp) = args.icmp {
        settings.icmp = pb::EgressIcmpPolicy::from(icmp).into();
    }

    let password = if args.clear_password {
        Some(Zeroizing::new(String::new()))
    } else if args.password_stdin {
        Some(read_stdin_password()?)
    } else if has_user(&url) && !(same_proxy && settings.has_password) && io::stdin().is_terminal()
    {
        // A new user needs a password; an unchanged one keeps the stored one.
        Some(prompt_password(&format!("Password for {url}: "))?)
    } else {
        None
    };

    let settings = client
        .set_proxy(pb::SetEgressProxyRequest {
            settings: settings.into(),
            password: password.as_ref().map(|p| p.to_string()),
            ..Default::default()
        })
        .await
        .context("failed to set the proxy")?
        .into_owned();
    print_settings(&settings, format)
}

async fn test(args: TestArgs, format: OutputFormat) -> Result<()> {
    let client = client();
    let mut request = pb::TestEgressProxyRequest {
        target: args.target.unwrap_or_default(),
        ..Default::default()
    };
    if let Some(proxy) = &args.with {
        let mut draft = get(&client).await?;
        let (mode, url) = parse_proxy_arg(proxy);
        if has_inline_password(&url) {
            bail!("do not put the password in the URL; use --password-stdin");
        }
        draft.mode = mode.into();
        draft.url = url;
        request.settings = draft.into();
        if args.password_stdin {
            request.password = Some(read_stdin_password()?.to_string());
        }
    }

    let mut stream = client
        .test_proxy(request)
        .await
        .context("failed to start the proxy test")?;
    let mut steps = Vec::new();
    let mut failed = false;
    while let Some(item) = stream
        .message::<pb::EgressProxyTestStep>()
        .await
        .context("failed to read the proxy test")?
    {
        let step: pb::EgressProxyTestStep = item.to_owned_message();
        failed |= !step.ok && !step.skipped;
        if matches!(format, OutputFormat::Table) {
            println!("{}", render_step(&step));
        }
        steps.push(step);
    }
    if matches!(format, OutputFormat::Json) {
        let json: Vec<_> = steps.iter().map(step_json).collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
    }
    if failed {
        bail!("the proxy test failed");
    }
    Ok(())
}

fn step_name(step: &pb::EgressProxyTestStep) -> &'static str {
    use pb::EgressProxyTestStepKind as K;
    match step.step.as_known() {
        Some(K::EGRESS_PROXY_TEST_STEP_KIND_SELECT) => "proxy",
        Some(K::EGRESS_PROXY_TEST_STEP_KIND_REACH) => "reach",
        Some(K::EGRESS_PROXY_TEST_STEP_KIND_AUTHENTICATE) => "authenticate",
        Some(K::EGRESS_PROXY_TEST_STEP_KIND_TUNNEL) => "tunnel",
        Some(K::EGRESS_PROXY_TEST_STEP_KIND_UDP) => "udp",
        _ => "unknown",
    }
}

fn failure_name(step: &pb::EgressProxyTestStep) -> Option<&'static str> {
    use pb::EgressProxyTestFailure as F;
    Some(match step.failure.as_known()? {
        F::EGRESS_PROXY_TEST_FAILURE_UNSPECIFIED => return None,
        F::EGRESS_PROXY_TEST_FAILURE_UNREACHABLE => "unreachable",
        F::EGRESS_PROXY_TEST_FAILURE_TIMEOUT => "timeout",
        F::EGRESS_PROXY_TEST_FAILURE_AUTH_REJECTED => "authentication rejected",
        F::EGRESS_PROXY_TEST_FAILURE_NO_ACCEPTABLE_METHOD => "no acceptable auth method",
        F::EGRESS_PROXY_TEST_FAILURE_MISSING_PASSWORD => "password missing",
        F::EGRESS_PROXY_TEST_FAILURE_TARGET_REFUSED => "target refused",
        F::EGRESS_PROXY_TEST_FAILURE_UDP_UNSUPPORTED => "UDP not supported",
        F::EGRESS_PROXY_TEST_FAILURE_PROTOCOL => "protocol error",
        F::EGRESS_PROXY_TEST_FAILURE_CONFIG => "configuration error",
    })
}

fn render_step(step: &pb::EgressProxyTestStep) -> String {
    let mark = if step.skipped {
        "-"
    } else if step.ok {
        "✓"
    } else {
        "✗"
    };
    let detail = match failure_name(step) {
        Some(failure) => format!("{failure}: {}", step.message),
        None => step.message.clone(),
    };
    if step.ok && step.latency_ms > 0 {
        format!(
            "{mark} {:<13}{detail} ({} ms)",
            step_name(step),
            step.latency_ms
        )
    } else {
        format!("{mark} {:<13}{detail}", step_name(step))
    }
}

fn step_json(step: &pb::EgressProxyTestStep) -> serde_json::Value {
    serde_json::json!({
        "step": step_name(step),
        "ok": step.ok,
        "skipped": step.skipped,
        "latency_ms": step.latency_ms,
        "failure": failure_name(step),
        "message": step.message,
    })
}

fn mode_name(settings: &pb::EgressProxySettings) -> &'static str {
    match settings.mode.as_known() {
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM) => "system",
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_NONE) => "none",
        Some(pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM) => "custom",
        _ => "unspecified",
    }
}

fn udp_name(settings: &pb::EgressProxySettings) -> &'static str {
    match settings.udp.as_known() {
        Some(pb::EgressUdpPolicy::EGRESS_UDP_POLICY_DIRECT) => "direct",
        Some(pb::EgressUdpPolicy::EGRESS_UDP_POLICY_BLOCK) => "block",
        _ => "proxy",
    }
}

fn icmp_name(settings: &pb::EgressProxySettings) -> &'static str {
    match settings.icmp.as_known() {
        Some(pb::EgressIcmpPolicy::EGRESS_ICMP_POLICY_BLOCK) => "block",
        _ => "direct",
    }
}

fn print_settings(settings: &pb::EgressProxySettings, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "mode": mode_name(settings),
                "url": settings.url,
                "has_password": settings.has_password,
                "exclude": settings.exclude,
                "udp": udp_name(settings),
                "icmp": icmp_name(settings),
                "effective": settings.effective,
                "credential_store": settings.credential_store,
                "revision": settings.revision,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        OutputFormat::Quiet => {}
        OutputFormat::Table => println!("{}", render_settings(settings)),
    }
    Ok(())
}

fn render_settings(settings: &pb::EgressProxySettings) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "Mode:       {}", mode_name(settings));
    if !settings.url.is_empty() {
        let _ = writeln!(out, "Proxy:      {}", settings.url);
        if has_user(&settings.url) {
            let state = if settings.has_password {
                format!("set ({})", settings.credential_store)
            } else {
                "NOT SET — connections will fail".to_string()
            };
            let _ = writeln!(out, "Password:   {state}");
        }
    }
    let effective = if settings.effective.is_empty() {
        "direct".to_string()
    } else {
        settings.effective.join(", ")
    };
    let _ = writeln!(out, "In use:     {effective}");
    let exclude = if settings.exclude.is_empty() {
        "(none)".to_string()
    } else {
        settings.exclude.join(", ")
    };
    let _ = writeln!(out, "Bypass:     {exclude}");
    let _ = writeln!(out, "UDP:        {}", udp_name(settings));
    let _ = write!(out, "ICMP:       {}", icmp_name(settings));
    out
}

fn read_stdin_password() -> Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(String::new());
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed to read the password from stdin")?;
    let password = Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string());
    if password.is_empty() {
        bail!("no password on stdin");
    }
    Ok(password)
}

/// Reads a password from the terminal without echoing it.
fn prompt_password(prompt: &str) -> Result<Zeroizing<String>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    eprint!("{prompt}");
    io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode().context("failed to read the password")?;
    let mut password = Zeroizing::new(String::new());
    let result = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    password.pop();
                }
                KeyCode::Char(c) => password.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    result?;
    if password.is_empty() {
        bail!("no password entered");
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_arguments_map_to_modes() {
        assert_eq!(
            parse_proxy_arg("System").0,
            pb::EgressProxyMode::EGRESS_PROXY_MODE_SYSTEM
        );
        assert_eq!(
            parse_proxy_arg("direct").0,
            pb::EgressProxyMode::EGRESS_PROXY_MODE_NONE
        );
        let (mode, url) = parse_proxy_arg(" socks5://alice@h:1080 ");
        assert_eq!(mode, pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM);
        assert_eq!(url, "socks5://alice@h:1080");
    }

    #[test]
    fn users_and_inline_passwords_are_detected() {
        assert!(has_user("socks5://alice@h:1080"));
        assert!(!has_user("socks5://h:1080"));
        assert!(has_inline_password("socks5://alice:pw@h:1080"));
        assert!(!has_inline_password("socks5://alice@h:1080"));
        assert!(!has_inline_password("socks5://h:1080"));
    }

    #[test]
    fn a_missing_password_is_called_out() {
        let settings = pb::EgressProxySettings {
            mode: pb::EgressProxyMode::EGRESS_PROXY_MODE_CUSTOM.into(),
            url: "socks5://alice@h:1080".into(),
            ..Default::default()
        };
        assert!(render_settings(&settings).contains("NOT SET"));
    }
}
