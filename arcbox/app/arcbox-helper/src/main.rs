//! arcbox-helper — privileged helper daemon for host mutations.
//!
//! Runs as a root launchd daemon with socket activation. launchd creates the
//! socket at `/var/run/arcbox-helper.sock` and starts this process on-demand
//! when the ArcBox daemon connects.

mod server;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut idle_exit = false;
    let mut development = server::development::DevelopmentPeers::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                eprintln!(
                    "arcbox-helper {}\nPrivileged helper daemon for host mutations (routes, DNS, sockets)\n\nOptions:\n  --idle-exit  Exit after {}s with no active connections\n  --development-peer UID:CDHash  Allow only pinned development clients on a private socket (repeatable)",
                    env!("CARGO_PKG_VERSION"),
                    server::IDLE_TIMEOUT.as_secs(),
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("arcbox-helper {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--idle-exit" => idle_exit = true,
            "--development-peer" => {
                let grant = args
                    .next()
                    .ok_or("--development-peer requires UID:CDHash")?;
                development.add(&grant)?;
            }
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!("run with --help for usage");
                std::process::exit(1);
            }
        }
    }

    // Helper runs as root — write logs to /var/log/arcbox/helper.log.
    // Debug E2E (`ARCBOX_HELPER_TEST_ROOT`) relocates the log dir under the
    // sandbox so the real binary can start without root.
    // A non-root development instance can test live authentication without
    // writing root's logs. Root instances always retain the privileged log
    // path, never following user-controlled log-file symlinks as root.
    // SAFETY: geteuid has no memory or argument preconditions.
    let log_dir = if development.enabled() && unsafe { libc::geteuid() } != 0 {
        let socket = std::path::PathBuf::from(arcbox_helper::socket_path());
        development.validate_socket(&socket)?;
        socket.parent().ok_or("socket has no parent")?.join("log")
    } else {
        server::fs_root::resolve(arcbox_constants::paths::privileged_log::HELPER_LOG_DIR)
    };
    let _ = std::fs::create_dir_all(&log_dir);
    let log_guard = arcbox_logging::init(arcbox_logging::LogConfig {
        log_dir,
        file_name: arcbox_constants::paths::privileged_log::HELPER_LOG.to_string(),
        default_filter: "arcbox_helper=info".to_string(),
        foreground: true, // Also log to stderr (captured by launchd).
        ..arcbox_logging::LogConfig::default()
    });

    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(server::run(idle_exit, development));

    log_guard.flush();
    result
}
