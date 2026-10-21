use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use splicetcp::egress::{DefaultEgress, EgressResolver as _, FlowMeta};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn rejected_proxy_never_connects_directly_to_the_target() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = ProxyConfig {
        host: "127.0.0.1".into(),
        port: proxy.local_addr().unwrap().port(),
        auth: Some(Arc::new(
            ProxyAuth::new("alice".into(), Some("secret".into())).unwrap(),
        )),
    };
    let reject = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 2]);
        stream.write_all(&[5, 0xff]).await.unwrap();
    });
    let resolver = DefaultEgress::new(
        Ipv4Addr::new(10, 0, 2, 1),
        Some(ProxyEnvironment {
            socks_proxy: Some(server),
            ..Default::default()
        }),
        5,
    );
    let connection = resolver
        .resolve(FlowMeta {
            src_ip: Ipv4Addr::new(10, 0, 2, 2),
            src_port: 49152,
            dst_ip: Ipv4Addr::LOCALHOST,
            dst_port: target.local_addr().unwrap().port(),
            domain: None,
        })
        .await
        .unwrap();
    assert!(connection.is_none());
    reject.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), target.accept())
            .await
            .is_err()
    );
}
