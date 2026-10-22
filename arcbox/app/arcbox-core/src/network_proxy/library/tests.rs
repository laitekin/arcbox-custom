use super::super::credentials::MemoryCredentialStore;
use super::*;

fn preferences(url: &str) -> ProxyPreferences {
    ProxyPreferences {
        policy: url.parse().unwrap(),
        exclude: vec![],
        udp: UdpPolicy::Proxy,
        icmp: IcmpPolicy::Block,
    }
}

fn controller(dir: &tempfile::TempDir, store: Arc<MemoryCredentialStore>) -> NetworkProxy {
    NetworkProxy::new(preferences("none"), store, dir.path().join("config.toml"))
}

fn port(state: &ProxyState) -> Option<u16> {
    state
        .load()
        .environment
        .as_ref()?
        .socks_proxy
        .as_ref()
        .map(|proxy| proxy.port)
}

#[test]
fn profiles_isolate_credentials_and_hot_update_only_assigned_machines() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryCredentialStore::default());
    let proxy = controller(&dir, store.clone());
    let first = proxy.routes().for_machine("first");
    let second = proxy.routes().for_machine("second");
    let a = proxy
        .save_proxy(
            "",
            "A",
            preferences("socks5h://alice:first@127.0.0.1:1080"),
            PasswordChange::Keep,
        )
        .unwrap();
    let b = proxy
        .save_proxy(
            "",
            "B",
            preferences("socks5h://alice:second@127.0.0.1:1080"),
            PasswordChange::Keep,
        )
        .unwrap();
    assert_eq!(
        store.get(&account(&a.id)).unwrap().unwrap().as_str(),
        "first"
    );
    assert_eq!(
        store.get(&account(&b.id)).unwrap().unwrap().as_str(),
        "second"
    );
    proxy.set_machine_proxy("first", &a.id).unwrap();
    proxy.set_machine_proxy("second", &b.id).unwrap();
    assert_eq!(port(&first), Some(1080));
    let old = first.load();
    proxy
        .save_proxy(
            &a.id,
            "A changed",
            preferences("socks5h://alice:new@127.0.0.1:1081"),
            PasswordChange::Keep,
        )
        .unwrap();
    assert_eq!(port(&first), Some(1081));
    assert_eq!(port(&second), Some(1080));
    assert_eq!(
        old.environment
            .as_ref()
            .unwrap()
            .socks_proxy
            .as_ref()
            .unwrap()
            .port,
        1080
    );
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!text.contains(":first@") && !text.contains(":second@") && !text.contains(":new@"));
    assert!(!format!("{:?}", proxy.saved_proxies().unwrap()).contains(":new@"));
}

#[test]
fn choices_survive_reload_and_default_changes_preserve_explicit_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryCredentialStore::default());
    let proxy = controller(&dir, store.clone());
    let a = proxy
        .save_proxy(
            "",
            "A",
            preferences("socks5h://127.0.0.1:1080"),
            PasswordChange::Keep,
        )
        .unwrap();
    proxy.select_default_proxy(&a.id).unwrap();
    proxy.set_machine_proxy("direct-machine", "direct").unwrap();
    proxy.set_machine_proxy("pinned", &a.id).unwrap();
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    let config: crate::config::Config = toml::from_str(&text).unwrap();
    let reloaded = controller(&dir, store);
    reloaded.load_library(config.network.proxy_library).unwrap();
    let inherited = reloaded.routes().for_machine("inherited");
    let pinned = reloaded.routes().for_machine("pinned");
    assert_eq!(port(&inherited), Some(1080));
    assert_eq!(port(&pinned), Some(1080));
    assert_eq!(port(&reloaded.routes().for_machine("direct-machine")), None);
    reloaded
        .apply(preferences("none"), PasswordChange::Keep)
        .unwrap();
    assert_eq!(port(&inherited), None);
    assert_eq!(port(&pinned), Some(1080));
    reloaded.set_machine_proxy("pinned", "").unwrap();
    assert_eq!(port(&pinned), None);
}

#[test]
fn deletion_requires_detaching_users_and_removes_only_the_profiles_secret() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryCredentialStore::default());
    let proxy = controller(&dir, store.clone());
    let a = proxy
        .save_proxy(
            "",
            "A",
            preferences("socks5://u:p@127.0.0.1:1080"),
            PasswordChange::Keep,
        )
        .unwrap();
    proxy.select_default_proxy(&a.id).unwrap();
    assert!(proxy.remove_proxy(&a.id).is_err());
    proxy
        .apply(preferences("none"), PasswordChange::Keep)
        .unwrap();
    proxy.set_machine_proxy("first", &a.id).unwrap();
    assert!(proxy.remove_proxy(&a.id).is_err());
    proxy.set_machine_proxy("first", "").unwrap();
    proxy.remove_proxy(&a.id).unwrap();
    assert!(store.get(&account(&a.id)).unwrap().is_none());
    assert!(proxy.saved_proxies().unwrap().is_empty());
}

#[test]
fn failed_persistence_restores_credentials_and_the_running_policy() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryCredentialStore::default());
    let proxy = controller(&dir, store.clone());
    let a = proxy
        .save_proxy(
            "",
            "A",
            preferences("socks5://u:old@127.0.0.1:1080"),
            PasswordChange::Keep,
        )
        .unwrap();
    proxy.set_machine_proxy("first", &a.id).unwrap();
    std::fs::write(dir.path().join("config.toml"), "invalid = [").unwrap();
    assert!(
        proxy
            .save_proxy(
                &a.id,
                "A changed",
                preferences("socks5://u:new@127.0.0.1:1081"),
                PasswordChange::Keep
            )
            .is_err()
    );
    assert_eq!(store.get(&account(&a.id)).unwrap().unwrap().as_str(), "old");
    assert_eq!(port(&proxy.routes().for_machine("first")), Some(1080));
    assert_eq!(proxy.saved_proxies().unwrap()[0].name, "A");
}
