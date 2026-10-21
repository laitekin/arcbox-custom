# Host-side proxy authentication

- Type: change
- Area: arcbox-fakeip, arcbox-proxy, splicetcp, arcbox-core, arcbox-api,
  arcbox-cli, arcbox-asset, arcbox-image
- Commits / PRs: laitekin/arcbox-custom#2 (issue)
- Related: none

Status: authentication, live settings, the settings API and CLI, the proxy
test and the daemon's own downloads are implemented and host-tested. The
VM-level coverage scenario exists (`tests/e2e/tests/proxy_coverage.rs`) and
has not been run on a Mac yet. The desktop settings page is separate.

## Problem or trigger

Guest egress could be tunnelled through a SOCKS5 or HTTP proxy, but only an
unauthenticated one: a URL with credentials was refused. The policy was read
once at daemon start, nothing showed which traffic actually used the proxy,
and the daemon's own downloads ignored it.

## What was done

**Authentication.** SOCKS5 credentials are decoded from explicit proxy URLs,
validated against the RFC 1929 byte limits, and carried by shared, redacted
credential objects. Password storage is zeroized on drop. URL Display omits
the password, and parser errors do not include the original URL. TCP CONNECT
and UDP ASSOCIATE share one greeting; configured credentials require method
0x02, and a server choosing no-auth is rejected. HTTP CONNECT supports Basic
authentication and maps 407 to a typed error.

**Live settings.** `arcbox_core::network_proxy::NetworkProxy` owns the one
`ProxyState` every VM's datapath and the daemon's HTTP client read. A change
is validated, the password written to the credential store (login keychain
per profile on macOS; `credentials/proxy.toml`, mode 0600, under the data
dir elsewhere or with `ARCBOX_PROXY_CREDENTIALS=file`), `[network]` rewritten
in `config.toml` in place, and only then the new snapshot published. A failed
step restores the previous password. New flows use the new snapshot; open
flows keep theirs. Under `proxy = "system"` the Mac's settings are re-probed
every 30 s.

**Configuration** (additive):

```toml
[network]
proxy = "socks5://alice@10.0.0.2:1080"   # never contains the password
proxy_exclude = [".corp.example"]
udp = "proxy"    # proxy | direct | block
icmp = "direct"  # direct | block
```

**API and CLI.** `arcbox.v1.EgressProxyService` (Get/Set/TestProxy), served
from the early runtime handle so a proxy the boot needs can be fixed before
the VM is ready. `abctl network proxy show|set|unset|test`; passwords only
from a no-echo prompt or `--password-stdin`.

**Proxy test.** `arcbox_proxy::diagnose` walks Select → Reach → Authenticate
→ Tunnel → UDP from the host and stops at the first failure, with typed
failures (unreachable, timeout, auth rejected, no acceptable method, missing
password, target refused, UDP unsupported, protocol, config).

**Daemon downloads.** `arcbox_asset::http` resolves the proxy per request from
the installed live state (SOCKS5 first with remote resolution, then HTTP
CONNECT; loopback and the bypass list direct).

## Coverage matrix

| Traffic | Leaves from | Through the proxy? | Proven by |
| --- | --- | --- | --- |
| Container TCP | System VM | Yes | `proxy_coverage` e2e (not yet run); `splicetcp` `proxy_rejection`, `proxy_hot_update` |
| Container UDP (DNS to a public resolver, QUIC) | System VM | Yes with SOCKS5; dropped behind an HTTP-only proxy unless `udp = "direct"` | `proxy_coverage` e2e (not yet run); `arcbox-proxy` UDP unit tests |
| `docker pull` / `docker build` | dockerd / BuildKit in the System VM | Yes (same datapath) | `proxy_coverage` e2e pull row (not yet run) |
| Linux machine programs | machine VMs | Yes (same `ProxyState`) | `proxy_coverage` e2e machine row (not yet run) |
| Kubernetes pods | k3s containerd in the System VM | Yes (same datapath) | `proxy_coverage` e2e pod row (not yet run) |
| Sandbox microVMs | nested in the System VM | Expected yes (their egress leaves through the System VM) | not yet verified |
| Built-in DNS (gateway) | host DNS service | Answered on the host; names reach the proxy with the flow | by design |
| ICMP echo | any VM | No proxy can carry it: direct, or dropped with `icmp = "block"` | `arcbox-proxy` egress |
| Machine images, image index, Docker CLI tools, macOS IPSW | daemon (host) | Yes | `arcbox-asset` `http_proxy`; `proxy_coverage` e2e (the machine row's image pull) |
| Boot assets (kernel, rootfs) | daemon via the external `arcbox-boot` crate | No: it builds its own clients (macOS system proxy / environment only) | upstream change needed |
| Desktop distro catalog | ArcBox.app (URLSession) | macOS system proxy, not ArcBox's | moves to the daemon with the image work (#3) |
| Port forwards, `host.docker.internal` | Mac ↔ VM | No, by design | — |

## Evidence

```sh
cargo test -p arcbox-fakeip
cargo test -p arcbox-proxy --test authenticated --test http_auth --test diagnose
cargo test -p splicetcp --test proxy_rejection --test proxy_hot_update
cargo test -p arcbox-asset --features download --test http_proxy
cargo test -p arcbox-core --lib -- network_proxy persist
cargo test -p arcbox-api --lib egress_proxy
cargo test -p arcbox-daemon --bin arcbox-daemon egress_proxy
cargo test -p arcbox-cli network
# on a Mac:
cargo test -p arcbox-e2e --test proxy_coverage -- --ignored
```

The SOCKS5 fixtures verify the authenticated TCP and UDP wire paths, data
relay, rejected passwords and refusal of no-auth downgrade. The TCP egress
regression listens at the real destination and confirms no direct connection
after proxy rejection. The download test shows a wrong password published
live fails closed (no tunnel, no file).

## Follow-ups

- Run `proxy_coverage` (now with machine and pod rows) on the e2e runner.
- Teach `arcbox-boot` to take a client (or proxy) so boot assets follow it.
- Keychain prompts for rebuilt, differently signed daemons need checking on
  a Mac (the file store is the fallback).
