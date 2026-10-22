use super::*;
use std::sync::Arc;

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::ProxyEnvironment;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn query() -> Vec<u8> {
    let mut packet = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    packet.extend_from_slice(b"\x07example\x04test\0\0\x01\0\x01");
    packet
}

fn response(query: &[u8]) -> Vec<u8> {
    let mut reply = query.to_vec();
    reply[2] |= 0x80;
    reply[3] |= 0x80;
    reply
}

fn remote(port: u16) -> ProxySnapshot {
    ProxySnapshot {
        environment: Some(ProxyEnvironment {
            socks_proxy: Some(ProxyConfig {
                host: "127.0.0.1".into(),
                port,
                auth: Some(Arc::new(
                    ProxyAuth::new("alice".into(), Some("secret".into())).unwrap(),
                )),
            }),
            remote_dns: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// An authenticated TCP-only SOCKS server; it answers DNS in the tunnel
/// without ever contacting the requested public resolver.
async fn socks(accept_password: bool) -> (u16, mpsc::Receiver<(String, u16)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        // forward() tries two resolvers if authentication is rejected.
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 2]);
            stream.write_all(&[5, 2]).await.unwrap();
            let version = stream.read_u8().await.unwrap();
            assert_eq!(version, 1);
            let length = stream.read_u8().await.unwrap();
            let mut user = vec![0; usize::from(length)];
            stream.read_exact(&mut user).await.unwrap();
            assert_eq!(user, b"alice");
            let length = stream.read_u8().await.unwrap();
            let mut password = vec![0; usize::from(length)];
            stream.read_exact(&mut password).await.unwrap();
            assert_eq!(password, b"secret");
            stream
                .write_all(&[1, u8::from(!accept_password)])
                .await
                .unwrap();
            if !accept_password {
                continue;
            }
            let mut command = [0; 3];
            stream.read_exact(&mut command).await.unwrap();
            assert_eq!(
                command,
                [5, 1, 0],
                "DNS must use CONNECT, not UDP ASSOCIATE"
            );
            let target = arcbox_proxy::socks5::read_addr(&mut stream).await.unwrap();
            tx.send(target).await.unwrap();
            stream
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let length = stream.read_u16().await.unwrap();
            let mut data = vec![0; usize::from(length)];
            stream.read_exact(&mut data).await.unwrap();
            assert_eq!(data, query());
            let reply = response(&data);
            stream
                .write_u16(reply.len().try_into().unwrap())
                .await
                .unwrap();
            stream.write_all(&reply).await.unwrap();
            break;
        }
    });
    (port, rx)
}

async fn udp() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut data = [0; 512];
        let (length, peer) = socket.recv_from(&mut data).await.unwrap();
        socket
            .send_to(&response(&data[..length]), peer)
            .await
            .unwrap();
    });
    addr
}

#[tokio::test]
async fn remote_dns_uses_authenticated_tcp_and_skips_the_hosts_resolver() {
    let (port, mut targets) = socks(true).await;
    let local = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let reply = forward(&query(), &[local.local_addr().unwrap()], &remote(port))
        .await
        .unwrap();
    assert_eq!(reply, response(&query()));
    assert_eq!(targets.recv().await.unwrap(), ("1.1.1.1".into(), 53));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), local.recv(&mut [0; 512]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejected_credentials_never_fall_back_to_direct_dns() {
    let (port, _) = socks(false).await;
    let local = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert!(
        forward(&query(), &[local.local_addr().unwrap()], &remote(port))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), local.recv(&mut [0; 512]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bypassed_names_keep_split_dns_and_normal_socks_keeps_local_dns() {
    let mut snapshot = remote(1);
    snapshot.environment.as_mut().unwrap().bypass_domains = vec![".test".into()];
    assert_eq!(
        forward(&query(), &[udp().await], &snapshot).await.unwrap(),
        response(&query())
    );
    let env = snapshot.environment.as_mut().unwrap();
    env.bypass_domains.clear();
    env.remote_dns = false;
    assert_eq!(
        forward(&query(), &[udp().await], &snapshot).await.unwrap(),
        response(&query())
    );
}

#[test]
fn replies_must_match_the_transaction_question_and_response_flag() {
    let query = Message::from_vec(&query()).unwrap();
    let good = response(&query.to_vec().unwrap());
    assert!(validate_response(&query, &good).is_ok());
    for offset in [0, 2, 13] {
        let mut bad = good.clone();
        bad[offset] ^= 0x80;
        assert!(validate_response(&query, &bad).is_err());
    }
    assert!(validate_response(&query, &[0; 2]).is_err());
}

#[tokio::test]
async fn malformed_queries_and_incomplete_remote_policies_fail_closed() {
    assert!(
        forward(&[0; 2], &[], &ProxySnapshot::default())
            .await
            .is_err()
    );
    let snapshot = ProxySnapshot {
        environment: Some(ProxyEnvironment {
            remote_dns: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(forward(&query(), &[udp().await], &snapshot).await.is_err());
}

#[tokio::test]
async fn an_isolated_machine_resolves_through_its_proxy_whatever_it_is_told() {
    // Bypassed name and proxy DNS switched off: still through the tunnel.
    let (port, mut targets) = socks(true).await;
    let mut snapshot = remote(port);
    snapshot.isolated = true;
    let env = snapshot.environment.as_mut().unwrap();
    env.remote_dns = false;
    env.bypass_domains = vec![".test".into()];
    let local = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let reply = forward(&query(), &[local.local_addr().unwrap()], &snapshot)
        .await
        .unwrap();
    assert_eq!(reply, response(&query()));
    assert_eq!(targets.recv().await.unwrap(), ("1.1.1.1".into(), 53));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), local.recv(&mut [0; 512]))
            .await
            .is_err(),
        "the Mac's resolver is never asked"
    );
}

#[tokio::test]
async fn an_isolated_machine_without_a_socks_proxy_gets_no_answer() {
    let local = udp().await;
    let isolated = |environment| ProxySnapshot {
        environment,
        isolated: true,
        ..Default::default()
    };
    assert!(forward(&query(), &[local], &isolated(None)).await.is_err());
    let http_only = ProxyEnvironment {
        http_proxy: Some(ProxyConfig {
            host: "127.0.0.1".into(),
            port: 3128,
            auth: None,
        }),
        ..Default::default()
    };
    assert!(
        forward(&query(), &[local], &isolated(Some(http_only)))
            .await
            .is_err()
    );
}
