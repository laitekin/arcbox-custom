//! A Docker image's filesystem as a tar stream, read from the daemon's own
//! Docker API socket: create a container that never starts, export it,
//! remove it — optionally after building a derived image with an init
//! system installed. Blocking: run it on the blocking pool.
//!
//! The System VM's dockerd does the work, so the image never has to be
//! pulled or unpacked on the Mac, and its owners and device nodes survive
//! in the tar headers.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// A Docker image's flattened filesystem. Dropping it removes the
/// container it was exported from.
pub struct DockerExport {
    body: Body,
    cleanup: ContainerCleanup,
}

impl Read for DockerExport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.body.read(buf)
    }
}

impl DockerExport {
    /// Starts exporting `image` through the Docker socket at `socket`.
    ///
    /// # Errors
    ///
    /// Returns the Docker API's message when the image is unknown or the
    /// container cannot be created or exported.
    pub fn open(socket: &Path, image: &str) -> io::Result<Self> {
        // The command is never run; it only has to be present for images
        // that declare none.
        let create = serde_json::json!({
            "Image": image,
            "Entrypoint": ["/sbin/init"],
            "Cmd": [],
            "Labels": { "com.arcboxlabs.purpose": "machine-image-export" },
        });
        let (status, body) = request(
            socket,
            "POST",
            "/containers/create",
            Some(Payload::json(&create)),
        )?;
        let reply: serde_json::Value = serde_json::from_reader(body).unwrap_or_default();
        if status != 201 {
            return Err(api_error("create a container from", image, &reply));
        }
        let id = reply["Id"]
            .as_str()
            .ok_or_else(|| io::Error::other("Docker returned no container id"))?
            .to_owned();
        let cleanup = ContainerCleanup {
            socket: socket.to_path_buf(),
            id: id.clone(),
        };
        let (status, body) = request(socket, "GET", &format!("/containers/{id}/export"), None)?;
        if status != 200 {
            let reply: serde_json::Value = serde_json::from_reader(body).unwrap_or_default();
            return Err(api_error("export", image, &reply));
        }
        Ok(Self { body, cleanup })
    }

    /// The container being exported.
    #[must_use]
    pub fn container_id(&self) -> &str {
        &self.cleanup.id
    }
}

struct ContainerCleanup {
    socket: PathBuf,
    id: String,
}

impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        let path = format!("/containers/{}?force=1&v=1", self.id);
        if let Err(e) = request(&self.socket, "DELETE", &path, None) {
            tracing::warn!(container = %self.id, error = %e, "could not remove the export container");
        }
    }
}

fn api_error(action: &str, image: &str, reply: &serde_json::Value) -> io::Error {
    let message = reply["message"].as_str().unwrap_or("unknown error");
    io::Error::other(format!("Docker could not {action} {image}: {message}"))
}

/// A request body and its content type.
struct Payload {
    content_type: &'static str,
    bytes: Vec<u8>,
}

impl Payload {
    fn json(value: &serde_json::Value) -> Self {
        Self {
            content_type: "application/json",
            bytes: value.to_string().into_bytes(),
        }
    }
}

/// One HTTP/1.1 request on a fresh connection; returns the status and the
/// body (de-chunked when the server chunks it).
fn request(
    socket: &Path,
    method: &str,
    path: &str,
    payload: Option<Payload>,
) -> io::Result<(u16, Body)> {
    let mut stream = UnixStream::connect(socket)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n");
    if let Some(payload) = &payload {
        use std::fmt::Write as _;
        let _ = write!(
            head,
            "Content-Type: {}\r\nContent-Length: {}\r\n",
            payload.content_type,
            payload.bytes.len()
        );
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(payload) = payload {
        stream.write_all(&payload.bytes)?;
    }
    read_response(BufReader::new(stream))
}

/// A Docker image the daemon built for an import. Dropping it removes the
/// image.
pub struct DerivedImage {
    socket: PathBuf,
    tag: String,
}

impl DerivedImage {
    /// Builds `tag` from `base` with an init system installed, through the
    /// Docker socket at `socket`. Blocks until the build ends.
    ///
    /// # Errors
    ///
    /// Returns the build's own error message when it fails.
    pub fn build_with_init(socket: &Path, base: &str, tag: &str) -> io::Result<Self> {
        let dockerfile = arcbox_image::machine_import::init_dockerfile(base);
        let mut context = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(dockerfile.len() as u64);
        header.set_cksum();
        context.append_data(&mut header, "Dockerfile", dockerfile.as_bytes())?;
        let context = context.into_inner()?;

        let path = format!("/build?t={}&rm=1&forcerm=1", percent_encode(tag));
        let (status, body) = request(
            socket,
            "POST",
            &path,
            Some(Payload {
                content_type: "application/x-tar",
                bytes: context,
            }),
        )?;
        if status != 200 {
            let reply: serde_json::Value = serde_json::from_reader(body).unwrap_or_default();
            return Err(api_error("build an init image from", base, &reply));
        }
        // The build streams one JSON object per line; a failed step is an
        // object with `error`, even though the status was 200.
        let image = Self {
            socket: socket.to_path_buf(),
            tag: tag.to_owned(),
        };
        for line in BufReader::new(body).lines() {
            let line = line?;
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(error) = event["error"].as_str() {
                return Err(io::Error::other(format!(
                    "installing an init system into {base} failed: {}",
                    error.trim()
                )));
            }
            if let Some(output) = event["stream"].as_str() {
                tracing::debug!(tag, "{}", output.trim_end());
            }
        }
        Ok(image)
    }

    /// The built image's tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl Drop for DerivedImage {
    fn drop(&mut self) {
        let path = format!("/images/{}?force=1", percent_encode(&self.tag));
        if let Err(e) = request(&self.socket, "DELETE", &path, None) {
            tracing::warn!(tag = %self.tag, error = %e, "could not remove the derived image");
        }
    }
}

/// Percent-encodes a query or path value.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn read_response<R: BufRead + Send + 'static>(mut reader: R) -> io::Result<(u16, Body)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other(format!("bad Docker API response: {line:?}")))?;
    let mut chunked = false;
    let mut length = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("transfer-encoding") {
                chunked = value.eq_ignore_ascii_case("chunked");
            } else if name.eq_ignore_ascii_case("content-length") {
                length = value.parse::<u64>().ok();
            }
        }
    }
    let reader: Box<dyn BufRead + Send> = Box::new(reader);
    let body = if chunked {
        Body::Chunked {
            reader,
            remaining: 0,
            done: false,
        }
    } else {
        Body::Plain(match length {
            Some(length) => Box::new(reader.take(length)),
            None => reader,
        })
    };
    Ok((status, body))
}

/// A response body.
enum Body {
    Plain(Box<dyn BufRead + Send>),
    Chunked {
        reader: Box<dyn BufRead + Send>,
        remaining: u64,
        done: bool,
    },
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(reader) => reader.read(buf),
            Self::Chunked {
                reader,
                remaining,
                done,
            } => {
                if *done || buf.is_empty() {
                    return Ok(0);
                }
                if *remaining == 0 {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    let size = line.trim().split(';').next().unwrap_or_default();
                    *remaining = u64::from_str_radix(size, 16).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad chunk size")
                    })?;
                    if *remaining == 0 {
                        *done = true;
                        return Ok(0);
                    }
                }
                let want = usize::try_from(*remaining).map_or(buf.len(), |r| r.min(buf.len()));
                let n = reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *remaining -= n as u64;
                if *remaining == 0 {
                    let mut crlf = [0u8; 2];
                    reader.read_exact(&mut crlf)?;
                }
                Ok(n)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(raw: &'static [u8]) -> (u16, String) {
        let (status, mut body) = read_response(io::Cursor::new(raw)).unwrap();
        let mut out = String::new();
        body.read_to_string(&mut out).unwrap();
        (status, out)
    }

    #[test]
    fn chunked_and_sized_bodies_decode() {
        assert_eq!(
            body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;x=y\r\n world\r\n0\r\n\r\n"),
            (200, "hello world".to_owned())
        );
        assert_eq!(
            body(b"HTTP/1.1 201 Created\r\nContent-Length: 4\r\n\r\n{}\r\nignored"),
            (201, "{}\r\n".to_owned())
        );
    }

    /// Serves one canned reply per connection, recording each request line
    /// and body size.
    fn fake_docker(
        replies: &'static [&'static str],
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::thread::JoinHandle<Vec<(String, usize)>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("docker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                request.read_line(&mut line).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    request.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.strip_prefix("Content-Length: ") {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                request.read_exact(&mut body).unwrap();
                seen.push((line.trim().to_owned(), length));
                stream.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (dir, socket, server)
    }

    #[test]
    fn an_init_build_sends_a_dockerfile_context_and_removes_the_image() {
        let (_dir, socket, server) = fake_docker(&[
            "HTTP/1.1 200 OK\r\nContent-Length: 29\r\n\r\n{\"stream\":\"Step 1/2 : FROM\"}\n",
            "HTTP/1.1 200 OK\r\n\r\n",
        ]);
        let image = DerivedImage::build_with_init(
            &socket,
            "ubuntu:24.04",
            "arcbox-machine-import/u:latest",
        )
        .unwrap();
        assert_eq!(image.tag(), "arcbox-machine-import/u:latest");
        drop(image);
        let seen = server.join().unwrap();
        assert_eq!(
            seen[0].0,
            "POST /build?t=arcbox-machine-import%2Fu%3Alatest&rm=1&forcerm=1 HTTP/1.1"
        );
        assert!(seen[0].1 >= 1024, "a tar context, not an empty body");
        assert_eq!(
            seen[1].0,
            "DELETE /images/arcbox-machine-import%2Fu%3Alatest?force=1 HTTP/1.1"
        );
    }

    #[test]
    fn a_failed_build_step_is_an_error_even_with_status_200() {
        let (_dir, socket, server) = fake_docker(&[
            "HTTP/1.1 200 OK\r\nContent-Length: 41\r\n\r\n{\"error\":\"no supported package manager\"}\n",
            "HTTP/1.1 200 OK\r\n\r\n",
        ]);
        let error = DerivedImage::build_with_init(&socket, "scratch", "t:1")
            .err()
            .expect("the build failed");
        assert!(
            error.to_string().contains("no supported package manager"),
            "{error}"
        );
        // The half-built image is still removed.
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn a_daemon_socket_round_trip_exports_and_cleans_up() {
        // A fake Docker API: create, export (chunked), delete.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("docker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in [
                "HTTP/1.1 201 Created\r\nContent-Length: 13\r\n\r\n{\"Id\":\"abc1\"}",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\ntar\r\n0\r\n\r\n",
                "HTTP/1.1 204 No Content\r\n\r\n",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                request.read_line(&mut line).unwrap();
                seen.push(line.trim().to_owned());
                stream.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });

        let mut export = DockerExport::open(&socket, "alpine:latest").unwrap();
        assert_eq!(export.container_id(), "abc1");
        let mut tar = String::new();
        export.read_to_string(&mut tar).unwrap();
        assert_eq!(tar, "tar");
        drop(export);

        let seen = server.join().unwrap();
        assert_eq!(seen[0], "POST /containers/create HTTP/1.1");
        assert_eq!(seen[1], "GET /containers/abc1/export HTTP/1.1");
        assert_eq!(seen[2], "DELETE /containers/abc1?force=1&v=1 HTTP/1.1");
    }
}
