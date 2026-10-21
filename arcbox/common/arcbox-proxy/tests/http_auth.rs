use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::ProxyConfig;
use arcbox_proxy::error::ProxyError;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn http_basic_credentials_are_sent_and_407_is_redacted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = ProxyConfig {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        auth: Some(Arc::new(
            ProxyAuth::new("alice".into(), Some("s3cret".into())).unwrap(),
        )),
    };
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .contains("Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 407 s3cret\r\n\r\n")
            .await
            .unwrap();
    });
    let error =
        arcbox_proxy::http_connect::connect_via_http_proxy_config(&proxy, "target.example", 443)
            .await
            .unwrap_err();
    assert!(!error.to_string().contains("s3cret"));
    assert!(matches!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<ProxyError>()),
        Some(ProxyError::AuthRejected)
    ));
    server.await.unwrap();
}
