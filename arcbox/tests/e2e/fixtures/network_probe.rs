//! Dependency-free static Linux probe, cross-compiled by machine_isolation.
use std::io::{self, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::{Duration, Instant};

fn query(name: &str) -> Vec<u8> {
    let mut bytes = vec![0x41, 0x42, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        assert!(label.len() <= 63);
        bytes.push(label.len() as u8);
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.extend_from_slice(&[0, 0, 1, 0, 1]);
    bytes
}

fn run(args: &[String]) -> io::Result<()> {
    match args[1].as_str() {
        "tcp-http" => {
            let address = format!("{}:{}", args[2], args[3])
                .parse()
                .map_err(io::Error::other)?;
            let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.set_write_timeout(Some(Duration::from_secs(3)))?;
            write!(socket, "GET / HTTP/1.0\r\nHost: {}\r\n\r\n", args[2])?;
            let mut header = [0; 5];
            socket.read_exact(&mut header)?;
            if &header != b"HTTP/" {
                return Err(io::Error::other("not an HTTP response"));
            }
            println!("HTTP/");
        }
        "serve-udp" => {
            let socket = UdpSocket::bind(format!("0.0.0.0:{}", args[2]))?;
            socket.set_read_timeout(Some(Duration::from_millis(100)))?;
            println!("READY");
            let end = Instant::now() + Duration::from_secs(480);
            let mut buffer = [0; 4096];
            while Instant::now() < end {
                match socket.recv_from(&mut buffer) {
                    Ok((length, peer)) => {
                        socket.send_to(&buffer[..length], peer)?;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        "dns" | "udp" => {
            let dns = args[1] == "dns";
            let socket = UdpSocket::bind("0.0.0.0:0")?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.connect(format!("{}:{}", args[2], args[3]))?;
            let data = if dns {
                query(&args[4])
            } else {
                args[4].as_bytes().to_vec()
            };
            socket.send(&data)?;
            let mut buffer = [0; 4096];
            let length = socket.recv(&mut buffer)?;
            if dns {
                if length < 12 || buffer[..2] != data[..2] || buffer[2] & 0x80 == 0 {
                    return Err(io::Error::other("invalid DNS reply"));
                }
                println!(
                    "DNS {}",
                    buffer[..length]
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                );
            } else {
                println!("{}", String::from_utf8_lossy(&buffer[..length]));
            }
        }
        _ => return Err(io::Error::other("unknown probe mode")),
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if let Err(error) = run(&args) {
        eprintln!("{error}");
        std::process::exit(
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::ConnectionRefused
            ) {
                2
            } else {
                1
            },
        );
    }
}
