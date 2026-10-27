use super::ORIGIN_IP;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
/// A SOCKS5 proxy without authentication that records CONNECT targets.
pub struct RecordingProxy {
    pub port: u16,
    pub targets: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    listener_thread: Option<std::thread::JoinHandle<()>>,
    controls: Arc<Mutex<Vec<TcpStream>>>,
    workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl RecordingProxy {
    pub fn spawn(origin_port: u16, public_ip: Ipv4Addr) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding SOCKS5 fixture")?;
        let port = listener.local_addr()?.port();
        let targets: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&targets);
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let controls = Arc::new(Mutex::new(Vec::new()));
        let worker_controls = Arc::clone(&controls);
        let workers = Arc::new(Mutex::new(Vec::new()));
        let worker_handles = Arc::clone(&workers);
        let listener_thread = std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let log = Arc::clone(&log);
                        if let Ok(copy) = client.try_clone() {
                            worker_controls.lock().unwrap().push(copy);
                        }
                        let handler_stop = Arc::clone(&stop);
                        let handle = std::thread::spawn(move || {
                            if let Err(error) =
                                serve(client, origin_port, public_ip, &log, &handler_stop)
                            {
                                eprintln!("fixture SOCKS connection: {error}");
                            }
                        });
                        worker_handles.lock().unwrap().push(handle);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            port,
            targets,
            stopped,
            listener_thread: Some(listener_thread),
            controls,
            workers,
        })
    }

    pub fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(thread) = self.listener_thread.take() {
            thread.join().expect("proxy listener panicked");
        }
        for stream in self.controls.lock().unwrap().drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        for worker in self.workers.lock().unwrap().drain(..) {
            worker.join().expect("proxy worker panicked");
        }
    }

    pub fn saw(&self, target: &str) -> bool {
        self.targets
            .lock()
            .expect("proxy log poisoned")
            .iter()
            .any(|seen| seen == target)
    }
}

impl Drop for RecordingProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(
    mut client: TcpStream,
    origin_port: u16,
    public_ip: Ipv4Addr,
    log: &Mutex<Vec<String>>,
    stopped: &AtomicBool,
) -> std::io::Result<()> {
    // macOS accepted sockets inherit O_NONBLOCK from the stoppable listener.
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    let mut methods = vec![0u8; usize::from(head[1])];
    client.read_exact(&mut methods)?;
    client.write_all(&[5, 0])?;
    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip)?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len)?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    log.lock()
        .expect("proxy log poisoned")
        .push(format!("{host}:{port}"));
    if request[1] == 3 {
        return serve_udp(client, log, stopped);
    }
    if request[1] != 1 {
        client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Ok(());
    }
    if port == 53 {
        client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
        let mut length = [0; 2];
        client.read_exact(&mut length)?;
        let mut query = vec![0; usize::from(u16::from_be_bytes(length))];
        client.read_exact(&mut query)?;
        let (name, response) = dns_response(&query)?;
        log.lock().unwrap().push(format!("dns-tcp:{name}"));
        client.write_all(&(response.len() as u16).to_be_bytes())?;
        client.write_all(&response)?;
        return Ok(());
    }
    let upstream = if host == ORIGIN_IP || host == public_ip.to_string() {
        TcpStream::connect(("127.0.0.1", origin_port))
    } else {
        TcpStream::connect((host.as_str(), port))
    };
    let Ok(upstream) = upstream else {
        client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Ok(());
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
    client.set_read_timeout(None)?;
    let (mut from_client, mut to_upstream) = (client.try_clone()?, upstream.try_clone()?);
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_upstream);
        let _ = to_upstream.shutdown(std::net::Shutdown::Write);
    });
    let (mut from_upstream, mut to_client) = (upstream, client);
    let _ = std::io::copy(&mut from_upstream, &mut to_client);
    let _ = to_client.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
    Ok(())
}

/// Deterministic A answer available only from this fixture's proxy paths.
fn dns_response(query: &[u8]) -> std::io::Result<(String, Vec<u8>)> {
    let invalid = || std::io::Error::other("invalid fixture DNS query");
    if query.len() < 17 || query[4..6] != [0, 1] {
        return Err(invalid());
    }
    let mut offset = 12;
    let mut labels = Vec::new();
    loop {
        let length = usize::from(*query.get(offset).ok_or_else(invalid)?);
        offset += 1;
        if length == 0 {
            break;
        }
        if length > 63 || offset + length > query.len() {
            return Err(invalid());
        }
        labels.push(String::from_utf8_lossy(&query[offset..offset + length]).into_owned());
        offset += length;
    }
    if offset + 4 > query.len() {
        return Err(invalid());
    }
    let is_a = query[offset..offset + 2] == [0, 1];
    let mut reply = query[..offset + 4].to_vec();
    reply[2..4].copy_from_slice(&[0x81, 0x80]);
    reply[6..12].copy_from_slice(&[0, u8::from(is_a), 0, 0, 0, 0]);
    if is_a {
        reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4, 198, 51, 100, 77]);
    }
    Ok((labels.join("."), reply))
}

fn serve_udp(
    mut client: TcpStream,
    log: &Mutex<Vec<String>>,
    stopped: &AtomicBool,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
    reply.extend_from_slice(&socket.local_addr()?.port().to_be_bytes());
    client.write_all(&reply)?;
    let mut bytes = [0; 4096];
    while !stopped.load(Ordering::SeqCst) {
        match socket.recv_from(&mut bytes) {
            Ok((length, peer)) => {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if length < 10 || bytes[2] != 0 || bytes[3] != 1 {
                    continue;
                }
                let host = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
                let port = u16::from_be_bytes([bytes[8], bytes[9]]);
                log.lock().unwrap().push(format!("udp:{host}:{port}"));
                let mut response = bytes[..10].to_vec();
                if port == 53 {
                    let (name, dns) = dns_response(&bytes[10..length])?;
                    log.lock().unwrap().push(format!("dns-udp:{name}"));
                    response.extend_from_slice(&dns);
                } else {
                    response.extend_from_slice(&bytes[10..length]);
                }
                socket.send_to(&response, peer)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
