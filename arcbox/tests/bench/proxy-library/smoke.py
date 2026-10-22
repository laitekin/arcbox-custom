#!/usr/bin/env python3
"""Prove two real Linux VMs have independent, hot-switchable proxy profiles.

Needs a running ArcBox Dev and a stopped Alpine base machine. Creates two
temporary clones and two password-free local SOCKS fixtures; leaves the base
machine intact and restores the global/default proxy. Deletes its own resources.
"""

import argparse
import asyncio
import json
from pathlib import Path
import runpy
import time

Fixture = runpy.run_path(Path(__file__).parents[1] / "proxy-dns/smoke.py")["Fixture"]


async def command(*args, check=True):
    process = await asyncio.create_subprocess_exec(*map(str, args), stdin=asyncio.subprocess.DEVNULL,
                                                   stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    try:
        output, error = await asyncio.wait_for(process.communicate(), 180)
    except TimeoutError:
        process.kill()
        await process.wait()
        raise
    if check and process.returncode:
        raise RuntimeError((output + error).decode(errors="replace"))
    return output.decode().strip()


async def rpc(method, payload, check=True, service="ProxyLibraryService"):
    output = await command("/usr/bin/curl", "--silent", "--show-error", "--max-time", "20",
                           "--unix-socket", Path.home() / ".arcbox-dev/run/arcbox.sock",
                           "-H", "Content-Type: application/json", "-H", "Connect-Protocol-Version: 1",
                           "-d", json.dumps(payload), f"http://localhost/arcbox.v1.{service}/{method}")
    result = json.loads(output)
    if check and "code" in result:
        raise RuntimeError(result)
    return result


async def main(app, base):
    cli = [app / "Contents/MacOS/bin/abctl", "--profile", "development"]
    before = json.loads(await command(*cli, "--format", "json", "network", "proxy", "show"))
    before_wire = await rpc("GetProxy", {}, service="EgressProxyService")
    token = str(time.time_ns())
    domain = f"machine-{token}.arcbox-proxy.test"
    names = [f"proxy-proof-a-{token}", f"proxy-proof-b-{token}"]
    fixtures = [Fixture(domain, b"proxy-A\n"), Fixture(domain, b"proxy-B\n")]
    servers, profiles, created = [], [], []
    try:
        for index, fixture in enumerate(fixtures):
            server = await asyncio.start_server(fixture.serve, "127.0.0.1", 0)
            servers.append(server)
            port = server.sockets[0].getsockname()[1]
            profile = await rpc("SaveProfile", {"name": f"VM proof {index} {token}", "settings": {
                "mode": "EGRESS_PROXY_MODE_CUSTOM", "url": f"socks5h://127.0.0.1:{port}",
                "udp": "EGRESS_UDP_POLICY_BLOCK", "icmp": "EGRESS_ICMP_POLICY_BLOCK"}})
            profiles.append(profile)
            await command(*cli, "machine", "clone", base, names[index])
            created.append(names[index])
            await command(*cli, "machine", "resize", names[index], "--cpus", "1", "--memory", "512")
            await rpc("SetMachineProxy", {"machine": names[index], "choice": profile["id"]})
            await command(*cli, "machine", "start", names[index])
        for index, name in enumerate(names):
            output = await command(*cli, "machine", "exec", name, "--", "wget", "-T", "8", "-qO-", f"http://{domain}/proof")
            assert output == f"proxy-{'AB'[index]}", output
        print("PASS: two running Linux machines used different host SOCKS5 profiles")
        await rpc("SetMachineProxy", {"machine": names[0], "choice": profiles[1]["id"]})
        output = await command(*cli, "machine", "exec", names[0], "--", "wget", "-T", "8", "-qO-", f"http://{domain}/proof")
        assert output == "proxy-B", output
        print("PASS: switching a running machine changed its next connection without a restart")
        rejected = await rpc("RemoveProfile", {"id": profiles[1]["id"]}, check=False)
        assert "code" in rejected and "in use" in rejected.get("message", "")
        changed = profiles[1]["settings"]
        changed["url"] = profiles[0]["settings"]["url"]
        await rpc("SaveProfile", {"id": profiles[1]["id"], "name": profiles[1]["name"], "settings": changed})
        for name in names:
            output = await command(*cli, "machine", "exec", name, "--", "wget", "-T", "8", "-qO-", f"http://{domain}/proof")
            assert output == "proxy-A", output
        print("PASS: editing an assigned profile updated both live machines; deleting an in-use profile was refused")
        selected = await rpc("SetProxy", {"settings": {"mode": "EGRESS_PROXY_MODE_CUSTOM", "profileId": profiles[0]["id"]}}, service="EgressProxyService")
        assert selected.get("profileId") == profiles[0]["id"]
        assert "code" in await rpc("RemoveProfile", {"id": profiles[0]["id"]}, check=False)
        print("PASS: a saved profile can be selected as the default by ID without copying its password")
    finally:
        await rpc("SetProxy", {"settings": before_wire}, service="EgressProxyService")
        for name in reversed(created):
            await command(*cli, "machine", "remove", name, "--force", check=False)
        for profile in profiles:
            await rpc("RemoveProfile", {"id": profile["id"]})
        remaining = await command(*cli, "--format", "json", "machine", "ls", "--all")
        assert all(name not in remaining for name in created), "temporary machine cleanup failed"
        for server in servers:
            server.close()
            await server.wait_closed()
        after = json.loads(await command(*cli, "--format", "json", "network", "proxy", "show"))
        for key in ("mode", "url", "has_password", "exclude", "udp", "icmp", "effective"):
            assert after[key] == before[key], f"default proxy changed: {key}"
        print("PASS: temporary machines/proxies removed; the base machine was untouched and the original default proxy was restored")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--base-machine", default="arcbox-check-alpine")
    options = parser.parse_args()
    asyncio.run(main(options.app, options.base_machine))
