//! Gateway DNS follows the same live host-side proxy policy as guest traffic.
//!
//! `socks5h` sends DNS-over-TCP through an authenticated SOCKS5 CONNECT tunnel.
//! Public resolvers are reached inside that tunnel, never through the Mac's
//! LAN/VPN resolver. Explicitly bypassed names keep the host's split DNS.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use arcbox_fakeip::proxy_detect::ProxyConfig;
use arcbox_fakeip::proxy_state::ProxySnapshot;
use hickory_proto::op::{Message, MessageType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const REMOTE_RESOLVERS: [SocketAddr; 2] = [
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 53),
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 53),
];

/// Resolves one query under a snapshot captured when the query arrived.
/// A failed remote-DNS tunnel never falls back to a direct socket.
pub async fn forward(
    data: &[u8],
    upstream: &[SocketAddr],
    snapshot: &ProxySnapshot,
) -> io::Result<Vec<u8>> {
    let query = Message::from_vec(data).map_err(io::Error::other)?;
    if query.metadata.message_type != MessageType::Query || query.queries.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one DNS question",
        ));
    }
    let host = query.queries[0].name().to_ascii();
    let remote = snapshot
        .environment
        .as_ref()
        .filter(|env| env.remote_dns && !env.should_bypass(&host));
    let proxy = remote
        .map(|env| {
            env.socks_proxy.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    "remote DNS requires a SOCKS5 proxy",
                )
            })
        })
        .transpose()?;
    let servers = if proxy.is_some() {
        &REMOTE_RESOLVERS[..]
    } else {
        upstream
    };
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, "no DNS upstream available");
    for addr in servers {
        let exchange = async {
            let response = match proxy {
                Some(proxy) => exchange_socks(data, *addr, proxy).await?,
                None => exchange_udp(data, *addr).await?,
            };
            validate_response(&query, &response)?;
            Ok(response)
        };
        match tokio::time::timeout(QUERY_TIMEOUT, exchange).await {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(error)) => last_error = error,
            Err(_) => {
                last_error = io::Error::new(io::ErrorKind::TimedOut, "DNS upstream timed out");
            }
        }
    }
    Err(last_error)
}

async fn exchange_socks(
    data: &[u8],
    upstream: SocketAddr,
    proxy: &ProxyConfig,
) -> io::Result<Vec<u8>> {
    let length = u16::try_from(data.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "DNS query exceeds TCP frame limit",
        )
    })?;
    let mut tunnel = arcbox_proxy::socks5::connect_via_socks5_config(
        proxy,
        &upstream.ip().to_string(),
        upstream.port(),
    )
    .await?;
    tunnel.write_u16(length).await?;
    tunnel.write_all(data).await?;
    let length = usize::from(tunnel.read_u16().await?);
    let mut response = vec![0; length];
    tunnel.read_exact(&mut response).await?;
    Ok(response)
}

async fn exchange_udp(data: &[u8], upstream: SocketAddr) -> io::Result<Vec<u8>> {
    let bind = match upstream.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let socket = UdpSocket::bind(SocketAddr::new(bind, 0)).await?;
    // The kernel accepts replies from this upstream only.
    socket.connect(upstream).await?;
    socket.send(data).await?;
    let mut response = vec![0; u16::MAX as usize];
    let length = socket.recv(&mut response).await?;
    response.truncate(length);
    Ok(response)
}

fn validate_response(query: &Message, data: &[u8]) -> io::Result<()> {
    let response = Message::from_vec(data).map_err(io::Error::other)?;
    if response.metadata.message_type != MessageType::Response
        || response.metadata.id != query.metadata.id
        || response.metadata.op_code != query.metadata.op_code
        || response.queries != query.queries
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DNS response does not match the question",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
