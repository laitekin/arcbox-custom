use std::sync::Arc;

use arcbox_fakeip::proxy_policy::ProxyPolicy;
use arcbox_fakeip::proxy_state::{IcmpPolicy, UdpPolicy};
use zeroize::Zeroizing;

use super::credentials::MemoryCredentialStore;
use super::*;

const ACCOUNT: &str = "socks5://alice@10.0.0.2:1080";

fn preferences(policy: &str) -> ProxyPreferences {
    ProxyPreferences {
        policy: policy.parse().unwrap(),
        exclude: Vec::new(),
        udp: UdpPolicy::Proxy,
        icmp: IcmpPolicy::Direct,
    }
}

fn controller(dir: &tempfile::TempDir) -> (NetworkProxy, Arc<MemoryCredentialStore>) {
    let store = Arc::new(MemoryCredentialStore::default());
    let proxy = NetworkProxy::new(
        preferences("none"),
        store.clone(),
        dir.path().join("config.toml"),
    );
    (proxy, store)
}

fn socks_password(proxy: &NetworkProxy) -> Option<String> {
    proxy
        .state()
        .load()
        .environment
        .as_ref()?
        .socks_proxy
        .as_ref()?
        .auth
        .as_ref()?
        .password()
        .map(str::to_string)
}

#[test]
fn an_inline_password_moves_to_the_store_and_is_published_live() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, store) = controller(&dir);
    let live = proxy.state();

    let view = proxy
        .apply(
            preferences("socks5://alice:s3cret@10.0.0.2:1080"),
            PasswordChange::Keep,
        )
        .unwrap();

    assert!(view.has_password);
    assert_eq!(view.preferences.policy.to_string(), ACCOUNT);
    assert_eq!(view.effective, vec![ACCOUNT.to_string()]);
    assert_eq!(view.revision, 1);
    assert_eq!(
        store.get(ACCOUNT).unwrap().as_deref().map(String::as_str),
        Some("s3cret")
    );
    // The clone handed to VMs observes the change without being replaced.
    assert_eq!(live.load().revision, 1);
    assert_eq!(socks_password(&proxy).as_deref(), Some("s3cret"));

    let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!written.contains("s3cret"), "{written}");
    assert!(!format!("{view:?}").contains("s3cret"));
}

#[test]
fn a_user_without_any_password_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, _store) = controller(&dir);

    let error = proxy
        .apply(preferences(ACCOUNT), PasswordChange::Keep)
        .unwrap_err();
    assert!(
        error.to_string().contains("no password is stored"),
        "{error}"
    );
    assert_eq!(proxy.state().load().revision, 0);
    assert!(proxy.state().load().environment.is_none());
    assert!(!dir.path().join("config.toml").exists());
}

#[test]
fn keep_reuses_the_stored_password_and_set_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, store) = controller(&dir);
    store.set(ACCOUNT, "first").unwrap();

    proxy
        .apply(preferences(ACCOUNT), PasswordChange::Keep)
        .unwrap();
    assert_eq!(socks_password(&proxy).as_deref(), Some("first"));

    proxy
        .apply(
            preferences(ACCOUNT),
            PasswordChange::Set(Zeroizing::new("second".into())),
        )
        .unwrap();
    assert_eq!(socks_password(&proxy).as_deref(), Some("second"));
    assert_eq!(proxy.state().load().revision, 2);
}

#[test]
fn clear_forgets_the_replaced_proxys_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, store) = controller(&dir);
    proxy
        .apply(
            preferences(ACCOUNT),
            PasswordChange::Set(Zeroizing::new("pw".into())),
        )
        .unwrap();

    let view = proxy
        .apply(preferences("none"), PasswordChange::Clear)
        .unwrap();
    assert!(store.get(ACCOUNT).unwrap().is_none());
    assert!(view.effective.is_empty());
    assert!(!view.has_password);

    // Clearing the password of a proxy that needs one is refused.
    proxy
        .apply(
            preferences(ACCOUNT),
            PasswordChange::Set(Zeroizing::new("pw".into())),
        )
        .unwrap();
    assert!(
        proxy
            .apply(preferences(ACCOUNT), PasswordChange::Clear)
            .is_err()
    );
    assert!(store.get(ACCOUNT).unwrap().is_some());
}

#[test]
fn a_password_for_a_proxy_without_a_user_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, _store) = controller(&dir);
    assert!(
        proxy
            .apply(
                preferences("socks5://10.0.0.2:1080"),
                PasswordChange::Set(Zeroizing::new("pw".into())),
            )
            .is_err()
    );
}

#[test]
fn a_config_write_failure_restores_the_previous_password() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the config file should be makes the write fail.
    std::fs::create_dir(dir.path().join("config.toml")).unwrap();
    let (proxy, store) = controller(&dir);
    store.set(ACCOUNT, "old").unwrap();

    assert!(
        proxy
            .apply(
                preferences(ACCOUNT),
                PasswordChange::Set(Zeroizing::new("new".into())),
            )
            .is_err()
    );
    assert_eq!(
        store.get(ACCOUNT).unwrap().as_deref().map(String::as_str),
        Some("old")
    );
    assert_eq!(proxy.state().load().revision, 0);
}

#[test]
fn policies_and_exclusions_reach_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, _store) = controller(&dir);
    let mut prefs = preferences("http://proxy.corp:3128");
    prefs.exclude = vec![".corp.example".into()];
    prefs.udp = UdpPolicy::Block;
    prefs.icmp = IcmpPolicy::Block;
    proxy.apply(prefs, PasswordChange::Keep).unwrap();

    let snapshot = proxy.state().load();
    let env = snapshot.environment.as_ref().unwrap();
    assert!(env.http_proxy.is_some());
    assert!(env.should_bypass("api.corp.example"));
    assert_eq!(snapshot.udp, UdpPolicy::Block);
    assert_eq!(snapshot.icmp, IcmpPolicy::Block);
    assert_eq!(proxy.settings().effective, vec!["http://proxy.corp:3128"]);
}

#[test]
fn a_missing_password_at_startup_installs_the_proxy_so_flows_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let proxy = NetworkProxy::new(
        preferences(ACCOUNT),
        Arc::new(MemoryCredentialStore::default()),
        dir.path().join("config.toml"),
    );
    let snapshot = proxy.state().load();
    let socks = snapshot
        .environment
        .as_ref()
        .and_then(|env| env.socks_proxy.as_ref())
        .expect("the proxy stays installed");
    assert!(socks.auth.as_ref().unwrap().password().is_none());
    assert!(!proxy.settings().has_password);
    assert_eq!(
        proxy.settings().preferences.policy,
        ProxyPolicy::Custom(ACCOUNT.parse().unwrap())
    );
}

#[test]
fn drafts_resolve_without_storing_or_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let (proxy, store) = controller(&dir);
    let env = proxy
        .resolve_draft(&preferences(ACCOUNT), Some("draft"))
        .unwrap()
        .unwrap();
    assert_eq!(
        env.socks_proxy.unwrap().auth.unwrap().password(),
        Some("draft")
    );
    assert!(store.get(ACCOUNT).unwrap().is_none());
    assert_eq!(proxy.state().load().revision, 0);
}
