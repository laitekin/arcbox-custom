#!/usr/bin/env python3
"""Prove gateway DNS and HTTP traverse a host-only, TCP-only SOCKS5 fixture.

Run against ArcBox Dev with a cached alpine:latest image. Temporarily changes
only the development proxy policy, restores it in finally, and never reads or
writes proxy credentials. No internet connection or privileged helper needed.
"""

import argparse
import asyncio
import ipaddress
import json
import os
from pathlib import Path
import struct
import time


class Fixture:
    def __init__(self, domain, body=b"socks5h-container-dns-ok\n"):
        self.domain = domain
        self.body = body
        self.seen = []

    async def serve(self, reader, writer):
        try:
            await asyncio.wait_for(self.session(reader, writer), 10)
        finally:
            writer.close()
            await writer.wait_closed()

    async def session(self, reader, writer):
        version, length = await reader.readexactly(2)
        methods = await reader.readexactly(length)
        assert version == 5 and 0 in methods
        writer.write(b"\x05\x00")
        version, command, _, atyp = await reader.readexactly(4)
        assert version == 5 and command == 1, "fixture accepts TCP CONNECT only"
        if atyp == 1:
            host = str(ipaddress.ip_address(await reader.readexactly(4)))
        elif atyp == 3:
            host = (await reader.readexactly((await reader.readexactly(1))[0])).decode()
        else:
            raise AssertionError("unexpected SOCKS address type")
        port = struct.unpack("!H", await reader.readexactly(2))[0]
        self.seen.append((host, port))
        writer.write(b"\x05\x00\x00\x01" + bytes(6))
        await writer.drain()
        if port == 53:
            assert host in ("1.1.1.1", "8.8.8.8")
            length = struct.unpack("!H", await reader.readexactly(2))[0]
            query = await reader.readexactly(length)
            offset, labels = 12, []
            while query[offset]:
                size = query[offset]
                labels.append(query[offset + 1:offset + 1 + size].decode())
                offset += size + 1
            assert ".".join(labels) == self.domain
            qtype = struct.unpack("!H", query[offset + 1:offset + 3])[0]
            question = query[12:offset + 5]
            answer = b""
            if qtype == 1:
                answer = b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04" + bytes([203, 0, 113, 10])
            reply = query[:2] + struct.pack("!HHHHH", 0x8180, 1, bool(answer), 0, 0) + question + answer
            writer.write(struct.pack("!H", len(reply)) + reply)
        else:
            assert host == self.domain and port == 80, "HTTP must reach the proxy by name"
            request = await reader.readuntil(b"\r\n\r\n")
            assert request.startswith(b"GET /proof ")
            body = self.body
            writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body)
        await writer.drain()


async def command(*args, check=True):
    process = await asyncio.create_subprocess_exec(*map(str, args), stdin=asyncio.subprocess.DEVNULL,
                                                   stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    try:
        output, error = await asyncio.wait_for(process.communicate(), 30)
    except TimeoutError:
        process.kill()
        await process.wait()
        raise
    if check and process.returncode:
        raise RuntimeError(error.decode(errors="replace"))
    return output.decode().strip()


async def main(app):
    abctl = app / "Contents/MacOS/bin/abctl"
    docker = app / "Contents/MacOS/xbin/docker"
    cli = [abctl, "--profile", "development"]
    dock = [docker, "--host", f"unix://{Path.home()}/.arcbox-dev/run/docker.sock"]
    before = json.loads(await command(*cli, "--format", "json", "network", "proxy", "show"))
    await command(*dock, "image", "inspect", "alpine:latest")
    domain = f"proof-{time.time_ns()}.arcbox-proxy.test"
    fixture = Fixture(domain)
    server = await asyncio.start_server(fixture.serve, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    name = f"arcbox-proxy-dns-{os.getpid()}"
    try:
        await command(*cli, "network", "proxy", "set", f"socks5h://127.0.0.1:{port}", "--no-exclude", "--udp", "block", "--icmp", "block")
        output = await command(*dock, "run", "--rm", "--name", name, "--pull", "never", "--dns", "10.0.2.1", "alpine:latest",
                               "wget", "-T", "8", "-qO-", f"http://{domain}/proof")
        assert output == "socks5h-container-dns-ok", output
        assert ("1.1.1.1", 53) in fixture.seen
        assert (domain, 80) in fixture.seen
        print("PASS: container DNS and HTTP reached the Mac SOCKS5 fixture; UDP was blocked")
    finally:
        # No --clear-password: restoring the policy leaves saved accounts intact.
        restore = before["url"] if before["mode"] == "custom" else before["mode"]
        excludes = [arg for entry in before["exclude"] for arg in ("--exclude", entry)] or ["--no-exclude"]
        await command(*cli, "network", "proxy", "set", restore, *excludes, "--udp", before["udp"], "--icmp", before["icmp"])
        await command(*dock, "rm", "-f", name, check=False)
        server.close()
        await server.wait_closed()
        after = json.loads(await command(*cli, "--format", "json", "network", "proxy", "show"))
        for key in ("mode", "url", "has_password", "exclude", "udp", "icmp"):
            assert after[key] == before[key], f"proxy setting not restored: {key}"
        print("PASS: original development proxy settings restored")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    asyncio.run(main(parser.parse_args().app))
