//! Live macOS signature validation, without root or host mutations.
#![cfg(target_os = "macos")]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn connection_for(hash: &str) -> bool {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let uid = root.path().metadata().unwrap().uid();
    let socket = root.path().join("helper.sock");
    let mut helper = Helper(
        Command::new(env!("CARGO_BIN_EXE_arcbox-helper"))
            .args(["--development-peer", &format!("{uid}:{hash}")])
            .env("ARCBOX_HELPER_SOCKET", &socket)
            .env("ARCBOX_HELPER_TEST_ROOT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !socket.exists() {
        assert!(
            helper.0.try_wait().unwrap().is_none(),
            "helper exited before binding"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "helper never bound socket"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        arcbox_helper::client::Client::connect_to(socket.to_str().unwrap()),
    )
    .await
    .unwrap()
    .is_ok()
}

#[tokio::test]
async fn only_the_explicit_live_code_hash_can_connect() {
    let executable = std::env::current_exe().unwrap();
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "--verbose=4"])
        .arg(executable)
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stderr).unwrap();
    let hash = output
        .lines()
        .find_map(|line| line.strip_prefix("CDHash="))
        .unwrap();
    assert!(
        connection_for(hash).await,
        "the explicitly pinned client was rejected"
    );
    assert!(
        !connection_for(&"00".repeat(20)).await,
        "an unpinned client was accepted"
    );
}
