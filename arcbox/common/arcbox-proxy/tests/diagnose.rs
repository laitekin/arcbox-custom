//! The diagnostic walk against a local SOCKS5 fixture that authenticates,
//! tunnels, and (optionally) relays UDP by echoing the datagram back.

use std::sync::Arc;

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::{ProxyConfig, ProxyEnvironment};
use arcbox_proxy::diagnose::{FailureKind, Step, StepKind, diagnose};
use arcbox_proxy::socks5;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

#[derive(Clone, Copy)]
struct Fixture {
    /// Password the fixture accepts.
    password: &'static str,
    /// Whether UDP ASSOCIATE is supported (else REP 0x07).
    udp: bool,
}

async fn serve(fixture: Fixture) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(session(stream, fixture));
        }
    });
    port
}

async fn session(mut stream: TcpStream, fixture: Fixture) {
    let mut greeting = [0; 3];
    stream.read_exact(&mut greeting).await.unwrap();
    stream.write_all(&[5, 2]).await.unwrap();
    let mut header = [0; 2];
    stream.read_exact(&mut header).await.unwrap();
    let mut user = vec![0; header[1] as usize];
    stream.read_exact(&mut user).await.unwrap();
    let size = stream.read_u8().await.unwrap();
    let mut password = vec![0; size as usize];
    stream.read_exact(&mut password).await.unwrap();
    let ok = password == fixture.password.as_bytes();
    stream.write_all(&[1, u8::from(!ok)]).await.unwrap();
    if !ok {
        return;
    }
    let mut request = [0; 3];
    stream.read_exact(&mut request).await.unwrap();
    let _ = socks5::read_addr(&mut stream).await.unwrap();
    match request[1] {
        1 => {
            stream
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut byte = [0];
            let _ = stream.read(&mut byte).await;
        }
        3 if !fixture.udp => {
            stream
                .write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        }
        3 => {
            let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut reply = vec![5, 0, 0];
            socks5::encode_addr(&mut reply, "127.0.0.1", relay.local_addr().unwrap().port());
            stream.write_all(&reply).await.unwrap();
            let mut payload = [0; 512];
            let (n, peer) = relay.recv_from(&mut payload).await.unwrap();
            relay.send_to(&payload[..n], peer).await.unwrap();
            let mut byte = [0];
            let _ = stream.read(&mut byte).await;
        }
        _ => unreachable!(),
    }
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

async fn walk(environment: Option<&ProxyEnvironment>) -> Vec<Step> {
    let mut steps = Vec::new();
    diagnose(environment, ("target.example", 443), |step| {
        steps.push(step);
    })
    .await;
    steps
}

fn kinds(steps: &[Step]) -> Vec<(StepKind, bool)> {
    steps.iter().map(|step| (step.kind, step.ok)).collect()
}

#[tokio::test]
async fn every_step_succeeds_against_a_working_proxy() {
    let port = serve(Fixture {
        password: "s3cret",
        udp: true,
    })
    .await;
    let steps = walk(Some(&environment(port, "s3cret"))).await;
    assert_eq!(
        kinds(&steps),
        [
            (StepKind::Select, true),
            (StepKind::Reach, true),
            (StepKind::Authenticate, true),
            (StepKind::Tunnel, true),
            (StepKind::Udp, true),
        ]
    );
    assert_eq!(steps[0].message, format!("socks5://alice@127.0.0.1:{port}"));
    assert!(steps.iter().all(|step| !step.message.contains("s3cret")));
}

#[tokio::test]
async fn a_wrong_password_stops_at_authentication_with_a_typed_failure() {
    let port = serve(Fixture {
        password: "s3cret",
        udp: true,
    })
    .await;
    let steps = walk(Some(&environment(port, "wrong"))).await;
    let last = steps.last().unwrap();
    assert_eq!(last.kind, StepKind::Authenticate);
    assert_eq!(last.failure, Some(FailureKind::AuthRejected));
    assert_eq!(steps.len(), 3);
}

#[tokio::test]
async fn a_proxy_without_udp_reports_it_after_a_working_tunnel() {
    let port = serve(Fixture {
        password: "s3cret",
        udp: false,
    })
    .await;
    let steps = walk(Some(&environment(port, "s3cret"))).await;
    assert!(steps[3].ok, "{steps:?}");
    let udp = steps.last().unwrap();
    assert_eq!(udp.kind, StepKind::Udp);
    assert_eq!(udp.failure, Some(FailureKind::UdpUnsupported));
}

#[tokio::test]
async fn an_unreachable_proxy_fails_at_reach() {
    // Bind and drop to get a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let steps = walk(Some(&environment(port, "s3cret"))).await;
    let last = steps.last().unwrap();
    assert_eq!(last.kind, StepKind::Reach);
    assert_eq!(last.failure, Some(FailureKind::Unreachable));
}

#[tokio::test]
async fn direct_egress_and_bypassed_targets_are_reported_as_skipped() {
    let steps = walk(None).await;
    assert_eq!(steps.len(), 1);
    assert!(steps[0].skipped);

    let mut env = environment(1, "s3cret");
    env.bypass_domains = vec!["*.example".into()];
    let steps = walk(Some(&env)).await;
    assert_eq!(steps.len(), 1);
    assert!(steps[0].skipped);
}
