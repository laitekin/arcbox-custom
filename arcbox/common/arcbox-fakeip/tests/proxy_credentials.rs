use arcbox_fakeip::proxy_policy::ProxyUrl;

#[test]
fn credential_url_decodes_bytes_but_never_displays_the_password() {
    let url: ProxyUrl = "socks5://alice:p%40ss%3Aword@[::1]:1080".parse().unwrap();
    let auth = url.server.auth.as_ref().unwrap();
    assert_eq!(auth.username(), "alice");
    assert_eq!(auth.password(), Some("p@ss:word"));
    assert_eq!(url.to_string(), "socks5://alice@[::1]:1080");
    assert!(!format!("{url:?}").contains("p@ss:word"));
}

#[test]
fn a_password_free_url_is_a_keychain_reference() {
    let url: ProxyUrl = "socks5://alice@localhost:1080".parse().unwrap();
    assert_eq!(url.server.auth.as_ref().unwrap().username(), "alice");
    assert_eq!(url.server.auth.as_ref().unwrap().password(), None);
}

#[test]
fn malformed_inputs_and_debug_errors_do_not_echo_secrets() {
    for url in [
        "socks5://alice:do-not-leak@proxy:0",
        "socks5://alice:do-not-leak@proxy",
        "socks5://alice:do-not-leak@proxy:not-a-port",
        "alice:do-not-leak@proxy",
        "socks5://alice:do-not-leak@[broken]:1080",
        "socks5://alice:do-not-leak@::1:1080",
    ] {
        let error = url.parse::<ProxyUrl>().unwrap_err();
        assert!(!error.to_string().contains("do-not-leak"));
        assert!(!format!("{error:?}").contains("do-not-leak"));
    }
}

#[test]
fn credentials_validate_decoded_utf8_byte_lengths() {
    for userinfo in [
        ":pass", "user:", "user:%ZZ", "user:%FF", "user:%", "user:%0",
    ] {
        assert!(
            format!("socks5://{userinfo}@localhost:1080")
                .parse::<ProxyUrl>()
                .is_err()
        );
    }
    for size in [1, 255] {
        assert!(
            format!("socks5://u:{}@localhost:1080", "x".repeat(size))
                .parse::<ProxyUrl>()
                .is_ok()
        );
    }
    assert!(
        format!("socks5://u:{}@localhost:1080", "x".repeat(256))
            .parse::<ProxyUrl>()
            .is_err()
    );
    assert!(
        format!("socks5://{}:p@localhost:1080", "中".repeat(86))
            .parse::<ProxyUrl>()
            .is_err()
    );
}
