//! HTTP CONNECT proxy client.
//!
//! Used when the host has a system HTTP proxy configured, to tunnel a TCP
//! connection by hostname (from the `DnsResolutionLog`) rather than the raw IP —
//! critical for fake-IP environments where the destination IP only the proxy can
//! resolve. The SOCKS5 equivalent lives in [`crate::socks5`].

use std::io;

use arcbox_fakeip::proxy_auth::ProxyAuth;
use arcbox_fakeip::proxy_detect::ProxyConfig;
use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

use crate::error::ProxyError;

/// Establishes a TCP tunnel via an HTTP CONNECT proxy (RFC 7231 §4.3.6).
///
/// The returned `TcpStream` is an end-to-end tunnel — all subsequent reads
/// and writes go directly to the target host through the proxy.
pub async fn connect_via_http_proxy(proxy: &str, host: &str, port: u16) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(proxy).await?;
    connect_stream(stream, host, port, None).await
}

/// Establishes a CONNECT tunnel with optional Basic authentication.
pub async fn connect_via_http_proxy_config(
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> io::Result<TcpStream> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let stream = TcpStream::connect((proxy.host.as_str(), proxy.port)).await?;
        connect_stream(stream, host, port, proxy.auth.as_deref()).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP proxy connect timed out"))?
}

async fn connect_stream(
    mut stream: TcpStream,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> io::Result<TcpStream> {
    if host.is_empty() || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ProxyError::Protocol("invalid CONNECT target").into());
    }
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = Zeroizing::new(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n"));
    if let Some(auth) = auth {
        if auth.username().contains(':') {
            return Err(ProxyError::Protocol("Basic username contains a colon").into());
        }
        let password = auth.password().ok_or(ProxyError::MissingPassword)?;
        let pair = Zeroizing::new(format!("{}:{password}", auth.username()));
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(pair.as_bytes()));
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&encoded);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    drop(request);

    // Read until we find "\r\n" marking the end of the status line. The
    // response may arrive split across multiple TCP segments, so we loop.
    let mut buf = Vec::with_capacity(256);
    let mut tmp = [0u8; 1];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy closed connection before completing status line",
            ));
        }
        buf.push(tmp[0]);
        if buf.ends_with(b"\r\n") {
            break;
        }
        if buf.len() > 1024 {
            return Err(io::Error::other(
                "HTTP CONNECT status line exceeds 1024 bytes",
            ));
        }
    }

    let status_line = String::from_utf8_lossy(&buf);
    // Accept any 2xx status as success (200, 204, etc).
    let mut fields = status_line.split_whitespace();
    let version = fields.next().unwrap_or_default();
    let status = fields.next().and_then(|field| field.parse::<u16>().ok());
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(ProxyError::Protocol("HTTP status line").into());
    }
    if status == Some(407) {
        return Err(ProxyError::AuthRejected.into());
    }
    let status_ok = status.is_some_and(|status| (200..300).contains(&status));

    if !status_ok {
        return Err(io::Error::other(format!(
            "HTTP CONNECT proxy rejected with status {status:?}"
        )));
    }

    // Consume remaining headers until a blank line. We already read the
    // status line (including its \r\n). The headers end with \r\n\r\n, so
    // we look for two consecutive \r\n sequences in the remaining data.
    let mut header_buf = Vec::with_capacity(512);
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        header_buf.push(tmp[0]);
        // A standalone \r\n (i.e. header_buf == b"\r\n") means the first
        // header line is blank → end of headers. Otherwise \r\n\r\n at the
        // tail means the previous header ended and a blank line followed.
        if header_buf.len() >= 2
            && header_buf.ends_with(b"\r\n")
            && (header_buf.len() == 2 || header_buf[..header_buf.len() - 2].ends_with(b"\r\n"))
        {
            break;
        }
        if header_buf.len() > 8192 {
            return Err(io::Error::other(
                "HTTP CONNECT response headers exceed 8192 bytes",
            ));
        }
    }

    tracing::debug!(
        target = %format!("{host}:{port}"),
        "HTTP CONNECT tunnel established"
    );
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Minimal HTTP CONNECT proxy: validates the request line and replies 200.
    #[tokio::test]
    async fn http_connect_establishes_tunnel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            while !buf.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut byte).await.unwrap();
                buf.push(byte[0]);
            }
            let req = String::from_utf8_lossy(&buf);
            assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1"), "{req}");
            s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut echo = [0u8; 3];
            s.read_exact(&mut echo).await.unwrap();
            s.write_all(&echo).await.unwrap();
        });

        let mut stream = connect_via_http_proxy(&addr.to_string(), "example.com", 443)
            .await
            .expect("HTTP CONNECT should establish");
        stream.write_all(b"abc").await.unwrap();
        let mut got = [0u8; 3];
        stream.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"abc");
        server.await.unwrap();
    }
}
