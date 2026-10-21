use std::sync::Arc;

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::ProxyConfig;
use arcbox_proxy::error::ProxyError;
use arcbox_proxy::socks5::{self, Socks5UdpAssociation};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

async fn server(method: u8, status: u8, command: u8) -> (ProxyConfig, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = ProxyConfig {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        auth: Some(Arc::new(
            ProxyAuth::new("alice".into(), Some("s3cret".into())).unwrap(),
        )),
    };
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 2]);
        stream.write_all(&[5, method]).await.unwrap();
        if method != 2 {
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
            return;
        }
        let mut header = [0; 2];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(header, [1, 5]);
        let mut username = [0; 5];
        stream.read_exact(&mut username).await.unwrap();
        assert_eq!(&username, b"alice");
        let size = stream.read_u8().await.unwrap();
        let mut password = vec![0; size as usize];
        stream.read_exact(&mut password).await.unwrap();
        assert_eq!(&password, b"s3cret");
        stream.write_all(&[1, status]).await.unwrap();
        if status != 0 {
            return;
        }
        let mut request = [0; 3];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [5, command, 0]);
        let (host, port) = socks5::read_addr(&mut stream).await.unwrap();
        if command == 1 {
            assert_eq!((host.as_str(), port), ("target.example", 443));
            stream
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut payload = [0; 4];
            stream.read_exact(&mut payload).await.unwrap();
            stream.write_all(&payload).await.unwrap();
        } else {
            assert_eq!((host.as_str(), port), ("0.0.0.0", 0));
            let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut reply = vec![5, 0, 0];
            socks5::encode_addr(&mut reply, "127.0.0.1", relay.local_addr().unwrap().port());
            stream.write_all(&reply).await.unwrap();
            let mut payload = [0; 512];
            let (n, peer) = relay.recv_from(&mut payload).await.unwrap();
            let (host, port, _) = socks5::decode_addr(&payload[3..n]).unwrap();
            assert_eq!((host.as_str(), port), ("dns.example", 53));
            relay.send_to(&payload[..n], peer).await.unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        }
    });
    (proxy, handle)
}

#[tokio::test]
async fn tcp_connect_authenticates_and_relays_payload() {
    let (proxy, server) = server(2, 0, 1).await;
    let mut stream = socks5::connect_via_socks5_config(&proxy, "target.example", 443)
        .await
        .unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0; 4];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"ping");
    server.await.unwrap();
}

#[tokio::test]
async fn udp_associate_authenticates_and_relays_datagrams() {
    let (proxy, server) = server(2, 0, 3).await;
    let assoc = Socks5UdpAssociation::associate_config(&proxy)
        .await
        .unwrap();
    assoc.send_to(b"query", "dns.example", 53).await.unwrap();
    let mut reply = [0; 64];
    let (size, host, port) = assoc.recv_from(&mut reply).await.unwrap().unwrap();
    assert_eq!(&reply[..size], b"query");
    assert_eq!((host.as_str(), port), ("dns.example", 53));
    drop(assoc);
    server.await.unwrap();
}

#[tokio::test]
async fn rejected_credentials_have_a_typed_error_on_both_paths() {
    for command in [1, 3] {
        let (proxy, server) = server(2, 1, command).await;
        let error = if command == 1 {
            socks5::connect_via_socks5_config(&proxy, "target.example", 443)
                .await
                .unwrap_err()
        } else {
            Socks5UdpAssociation::associate_config(&proxy)
                .await
                .unwrap_err()
        };
        assert!(matches!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<ProxyError>()),
            Some(ProxyError::AuthRejected)
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn configured_credentials_never_downgrade_to_no_auth() {
    let (proxy, server) = server(0, 0, 1).await;
    let error = socks5::connect_via_socks5_config(&proxy, "target.example", 443)
        .await
        .unwrap_err();
    assert!(matches!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<ProxyError>()),
        Some(ProxyError::NoAcceptableMethod)
    ));
    server.await.unwrap();
}

