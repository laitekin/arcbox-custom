//! A download made after `install_proxy` goes through the installed SOCKS5
//! proxy with its credentials, and follows a change to the live state.
#![cfg(feature = "download")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arcbox_asset::download::download_and_verify;
use arcbox_asset::http;
use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use arcbox_fakeip::proxy_state::{ProxySnapshot, ProxyState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const BODY: &[u8] = b"rootfs-bytes";

/// An HTTP/1.1 server answering every request with `BODY`.
async fn origin() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    BODY.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(BODY).await;
            });
        }
    });
    port
}

/// A SOCKS5 proxy that requires alice/s3cret and connects every CONNECT,
/// whatever name it asks for, to the origin.
async fn socks(origin: u16, tunnels: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let tunnels = Arc::clone(&tunnels);
            tokio::spawn(async move {
                let mut greeting = [0; 2];
                client.read_exact(&mut greeting).await.unwrap();
                let mut methods = vec![0; greeting[1] as usize];
                client.read_exact(&mut methods).await.unwrap();
                if !methods.contains(&2) {
                    client.write_all(&[5, 0xFF]).await.unwrap();
                    return;
                }
                client.write_all(&[5, 2]).await.unwrap();
                let mut header = [0; 2];
                client.read_exact(&mut header).await.unwrap();
                let mut user = vec![0; header[1] as usize];
                client.read_exact(&mut user).await.unwrap();
                let size = client.read_u8().await.unwrap();
                let mut password = vec![0; size as usize];
                client.read_exact(&mut password).await.unwrap();
                let ok = user == b"alice" && password == b"s3cret";
                client.write_all(&[1, u8::from(!ok)]).await.unwrap();
                if !ok {
                    return;
                }
                let mut request = [0; 4];
                client.read_exact(&mut request).await.unwrap();
                assert_eq!(request[3], 3, "the name is resolved by the proxy");
                let len = client.read_u8().await.unwrap();
                let mut name = vec![0; len as usize + 2];
                client.read_exact(&mut name).await.unwrap();
                assert_eq!(&name[..len as usize], b"asset.test");
                client
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                tunnels.fetch_add(1, Ordering::SeqCst);
                let mut upstream = TcpStream::connect(("127.0.0.1", origin)).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    port
}

fn environment(port: u16, password: &str) -> ProxyEnvironment {
    ProxyEnvironment {
        socks_proxy: Some(ProxyConfig {
            host: "127.0.0.1".into(),
            port,
            auth: Some(Arc::new(
                ProxyAuth::new("alice".into(), Some(password.into())).unwrap(),
            )),
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn downloads_follow_the_installed_live_proxy() {
    let origin = origin().await;
    let tunnels = Arc::new(AtomicUsize::new(0));
    let proxy = socks(origin, Arc::clone(&tunnels)).await;
    let state = ProxyState::new(Some(environment(proxy, "s3cret")));
    assert!(http::install_proxy(state.clone()));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("rootfs");
    let sha = {
        use sha2::Digest as _;
        format!("{:x}", sha2::Sha256::digest(BODY))
    };
    download_and_verify("http://asset.test/rootfs", &dest, &sha, "rootfs", |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(tunnels.load(Ordering::SeqCst), 1);

    // A wrong password published live fails closed: no tunnel, no file.
    state.store(ProxySnapshot {
        environment: Some(environment(proxy, "wrong")),
        revision: 1,
        ..Default::default()
    });
    let again = dir.path().join("again");
    assert!(
        download_and_verify(
            "http://asset.test/rootfs",
            &again,
            &sha,
            "rootfs",
            |_, _| {}
        )
        .await
        .is_err()
    );
    assert!(!again.exists());
    assert_eq!(tunnels.load(Ordering::SeqCst), 1);
}
