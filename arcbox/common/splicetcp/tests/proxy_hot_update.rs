use std::{net::Ipv4Addr, sync::Arc};

use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use arcbox_fakeip::proxy_state::{ProxySnapshot, ProxyState};
use splicetcp::egress::{DefaultEgress, EgressConn, EgressResolver as _, FlowMeta};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn echo_proxy(label: u8) -> (ProxyEnvironment, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = ProxyConfig {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        auth: None,
    };
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
        let mut command = [0; 3];
        stream.read_exact(&mut command).await.unwrap();
        arcbox_proxy::socks5::read_addr(&mut stream).await.unwrap();
        stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut byte = [0];
        while stream.read(&mut byte).await.unwrap() != 0 {
            stream.write_all(&[label]).await.unwrap();
        }
    });
    (
        ProxyEnvironment {
            socks_proxy: Some(server),
            ..Default::default()
        },
        task,
    )
}

async fn roundtrip(stream: &mut TcpStream) -> u8 {
    stream.write_all(b"x").await.unwrap();
    stream.read_u8().await.unwrap()
}

#[tokio::test]
async fn newly_enabled_and_changed_proxies_apply_to_new_flows_only() {
    let state = ProxyState::default();
    let resolver = Arc::new(DefaultEgress::with_proxy_state(
        Ipv4Addr::new(10, 0, 2, 1),
        state.clone(),
        5,
    ));
    let (first, server_a) = echo_proxy(b'A').await;
    let (second, server_b) = echo_proxy(b'B').await;
    let flow = FlowMeta {
        src_ip: Ipv4Addr::new(10, 0, 2, 2),
        src_port: 49152,
        dst_ip: Ipv4Addr::new(203, 0, 113, 1),
        dst_port: 443,
        domain: Some("target.example".into()),
    };
    state.store(ProxySnapshot {
        environment: Some(first),
        revision: 1,
        ..Default::default()
    });
    let EgressConn::Tcp(mut old) = resolver.resolve(flow.clone()).await.unwrap().unwrap();
    assert_eq!(roundtrip(&mut old).await, b'A');
    state.store(ProxySnapshot {
        environment: Some(second),
        revision: 2,
        ..Default::default()
    });
    let EgressConn::Tcp(mut new) = resolver.resolve(flow).await.unwrap().unwrap();
    assert_eq!(roundtrip(&mut new).await, b'B');
    assert_eq!(roundtrip(&mut old).await, b'A');
    drop((old, new));
    server_a.await.unwrap();
    server_b.await.unwrap();
}
