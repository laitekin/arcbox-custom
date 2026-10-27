//! An actual Xvnc framebuffer through the machine's loopback forward.
use super::*;
use arcbox_e2e::docker::docker_output;
use arcbox_protocol::v1::{MachinePortForward, SetMachinePortForwardsRequest};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

pub fn prepare_image(data_dir: &Path, source: &str) -> Result<String> {
    let tag = "arcbox-e2e-vnc:local";
    let cache_dir = arcbox_e2e::repo_root().join("target/e2e-image-cache");
    std::fs::create_dir_all(&cache_dir).context("creating VNC image cache")?;
    let cache = cache_dir.join(format!("{}-vnc.tar", source.replace(['/', ':'], "_")));
    if cache.exists()
        && docker_output(
            data_dir,
            &["load", "-i", cache.to_str().context("cache path")?],
            Duration::from_secs(120),
        )
        .is_ok()
    {
        return Ok(tag.into());
    }
    let context = data_dir.join("vnc-build");
    std::fs::create_dir(&context)?;
    std::fs::write(
        context.join("Dockerfile"),
        format!(
            "FROM {source}\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends tigervnc-standalone-server x11-utils x11-xserver-utils xfonts-base && rm -rf /var/lib/apt/lists/*\n"
        ),
    )?;
    // Use the Engine build API, the same path used by image import's
    // add-init step; buildx sessions are outside this machine-image test.
    let output = arcbox_e2e::docker::run_with_timeout(
        std::process::Command::new("docker")
            .env("DOCKER_HOST", arcbox_e2e::docker::docker_host(data_dir))
            .env("DOCKER_BUILDKIT", "0")
            .args([
                "build",
                "-t",
                tag,
                context.to_str().context("context path")?,
            ]),
        Duration::from_secs(600),
    )?;
    anyhow::ensure!(
        output.status.success(),
        "VNC fixture build failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let staged = cache.with_extension("tmp");
    docker_output(
        data_dir,
        &["save", "-o", staged.to_str().context("cache path")?, tag],
        Duration::from_secs(120),
    )?;
    std::fs::rename(staged, cache)?;
    Ok(tag.into())
}

pub async fn verify(
    machines: &mut MachineServiceClient<Channel>,
    id: &str,
    data_dir: &Path,
) -> Result<()> {
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    machines
        .set_port_forwards(SetMachinePortForwardsRequest {
            id: id.into(),
            forwards: vec![MachinePortForward {
                host_port: port.into(),
                guest_port: 5901,
                protocol: "tcp".into(),
            }],
        })
        .await?;
    // This guest is isolated, with no bridge. The only host-facing listener
    // is the forward bound to 127.0.0.1, and the server dies with the test VM.
    exec_in(machines, id, "Xtigervnc :1 -noreset -geometry 640x480 -depth 24 -SecurityTypes None -localhost no </dev/null >/tmp/vnc.log 2>&1 & i=0; until DISPLAY=:1 xdpyinfo >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 100 ] || { cat /tmp/vnc.log; exit 1; }; sleep 0.1; done; DISPLAY=:1 xsetroot -solid '#2468ac'; DISPLAY=:1 xmessage -geometry 400x100+100+150 -buttons OK 'ArcBox isolated Ubuntu VNC acceptance' </dev/null >/tmp/xmessage.log 2>&1 &").await?;
    let output = data_dir.join("vnc-frame.ppm");
    let pixels = tokio::task::spawn_blocking(move || framebuffer(port)).await??;
    let colorful = pixels
        .chunks_exact(3)
        .filter(|pixel| pixel[0] != pixel[1] || pixel[1] != pixel[2])
        .count();
    anyhow::ensure!(
        colorful > 10_000,
        "VNC returned no colored desktop: {colorful}"
    );
    let mut file = std::fs::File::create(&output)?;
    file.write_all(b"P6\n640 480\n255\n")?;
    file.write_all(&pixels)?;
    println!(
        "PASS real Xvnc desktop: 640x480 framebuffer via localhost:{port}; capture={}",
        output.display()
    );
    Ok(())
}

fn framebuffer(port: u16) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut version = [0; 12];
    stream.read_exact(&mut version)?;
    anyhow::ensure!(&version == b"RFB 003.008\n", "unexpected VNC protocol");
    stream.write_all(&version)?;
    let mut count = [0];
    stream.read_exact(&mut count)?;
    let mut security = vec![0; count[0] as usize];
    stream.read_exact(&mut security)?;
    anyhow::ensure!(security.contains(&1), "no fixture security type");
    stream.write_all(&[1])?;
    let mut result = [0; 4];
    stream.read_exact(&mut result)?;
    anyhow::ensure!(result == [0; 4], "VNC security negotiation failed");
    stream.write_all(&[1])?;
    let mut init = [0; 24];
    stream.read_exact(&mut init)?;
    let width = u16::from_be_bytes([init[0], init[1]]);
    let height = u16::from_be_bytes([init[2], init[3]]);
    anyhow::ensure!((width, height) == (640, 480), "unexpected framebuffer size");
    let name_len = u32::from_be_bytes(init[20..24].try_into()?) as usize;
    anyhow::ensure!(name_len < 4096, "invalid VNC name length");
    let mut name = vec![0; name_len];
    stream.read_exact(&mut name)?;
    // Request little-endian 32-bit truecolor: B,G,R,pad, raw encoding only.
    stream.write_all(&[
        0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
    ])?;
    stream.write_all(&[2, 0, 0, 1, 0, 0, 0, 0])?;
    stream.write_all(&[3, 0, 0, 0, 0, 0, 2, 128, 1, 224])?;
    let mut update = [0; 4];
    stream.read_exact(&mut update)?;
    anyhow::ensure!(update[0] == 0, "expected framebuffer update");
    let rectangles = u16::from_be_bytes([update[2], update[3]]);
    let mut pixels = vec![0; 640 * 480 * 3];
    let mut covered = 0usize;
    for _ in 0..rectangles {
        let mut rect = [0; 12];
        stream.read_exact(&mut rect)?;
        let value = |n| u16::from_be_bytes([rect[n], rect[n + 1]]) as usize;
        let (x, y, w, h) = (value(0), value(2), value(4), value(6));
        anyhow::ensure!(
            rect[8..] == [0; 4] && x + w <= 640 && y + h <= 480,
            "invalid raw rectangle"
        );
        let mut raw = vec![0; w * h * 4];
        stream.read_exact(&mut raw)?;
        for row in 0..h {
            for col in 0..w {
                let src = (row * w + col) * 4;
                let dst = ((y + row) * 640 + x + col) * 3;
                pixels[dst..dst + 3].copy_from_slice(&[raw[src + 2], raw[src + 1], raw[src]]);
            }
        }
        covered += w * h;
    }
    anyhow::ensure!(covered == 640 * 480, "incomplete framebuffer: {covered}");
    Ok(pixels)
}
