# 2026-10-22 — SOCKS5 远程 DNS 与绕过规则

- Type: change
- Area: arcbox-fakeip, splicetcp, arcbox-proxy, arcbox-net, arcbox-core, ArcBoxClient, desktop
- Commits / PRs: laitekin/arcbox-custom#2
- Related: [此前的代理覆盖范围](2026-10-21-proxy-authentication.md)

## 使用

ArcBox Dev → Settings → Network → Custom proxy → SOCKS5，勾选
“Resolve DNS through the proxy”，填写服务器、端口和可选的账号密码，测试后 Apply。
新增自定义 SOCKS5 默认勾选；已有普通 `socks5://` 配置保持原行为。
也可以运行 `abctl --profile development network proxy set socks5h://服务器:端口`。
带认证时使用 `socks5h://用户名@服务器:端口 --password-stdin`，不要把密码写进命令行。

## 依据与实现

[OrbStack 的公开代理文档](https://docs.orbstack.dev/docker/network#proxies) 支持
自动跟随 macOS、独立代理、SOCKS5 认证、`socks5h` 远程解析和 NO_PROXY 风格的
域名/IP/CIDR 绕过。本次以这些公开行为为参照；没有使用其闭源实现。

此前解析器把 `socks5h` 归一化成 `socks5`，网关 DNS 始终通过宿主机 UDP
访问系统解析器，设置远程解析并不能避免这条直连。

- `socks5h` 现在会保留在配置、RPC、CLI 和界面中。切换 DNS 方式不改变
  钥匙串账号，已有 SOCKS5 密码继续可用；菜单栏开关也保留 DNS 选项。
- 容器和 Linux 机器使用同一宿主机数据通路。发给网关的外部 DNS 查询通过
  SOCKS5 CONNECT 内的 DNS-over-TCP 访问 `1.1.1.1:53`，失败时只重试代理内的
  `8.8.8.8:53`，不回退到宿主机直连。整个单次查询尝试有 3 秒期限。
- 这不依赖 SOCKS5 UDP ASSOCIATE，也不要求 helper、网桥权限或虚拟机内
  配置代理。普通 UDP 的 proxy/direct/block 规则保持独立。
- 本地域名在进程内回答，包括 HTTPS/SVCB 类型。被明确绕过的域名仍访问
  Mac 的 DNS，保留公司/VPN 的内部解析能力。
- 绕过规则支持精确域名、`.example.com` / `*.example.com`、IP、IPv4/IPv6
  CIDR 和 `*`。TCP/UDP 即使已从 DNS 日志恢复域名，也会检查目的 IP 的 CIDR。
- 校验 DNS 回复的事务 ID、响应标志、opcode 和问题，避免接收不匹配的答复。

## 证据

- `cargo test -p arcbox-fakeip -p arcbox-proxy -p splicetcp`：193 项通过。
- `cargo test -p arcbox-core network_proxy`：17 项通过，包含密码复用与重载。
- `cargo test -p arcbox-net dns::`：35 项通过，包含 TCP-only 认证代理、本机
  UDP 未收到请求、认证失败不回退、split DNS 和本地域名 HTTPS/SVCB。
- 桌面 `make test`，仅选择 `EgressProxySettingsTests` 和
  `EgressProxyQuickToggleTests`：11 项通过。
- 相关 Rust crate 的 `cargo clippy --all-targets -- -D warnings` 通过。
- `cargo check -p arcbox-net --target aarch64-unknown-linux-musl` 通过。
- 本机 `make run` 完整构建、App 签名验证和网络设置界面检查通过。
- 真实容器验证通过：普通 UDP 被禁用时，网关 DNS 和 HTTP 请求均到达
  Mac 上的 TCP-only SOCKS5 测试服务；原开发版代理设置已恢复，测试容器已删除。
- 真机复现脚本：[proxy-dns/smoke.py](../../tests/bench/proxy-dns/smoke.py)。
  它需要运行中的开发版和已缓存的 `alpine:latest`；临时设置本地 TCP-only
  SOCKS5 并禁用 UDP，验证容器 DNS 与 HTTP，最后恢复原代理设置并删除测试容器。

## 范围与限制

- 系统代理模式和普通 `socks5://` 的网关 DNS 仍跟随 Mac；远程 DNS 是显式选项。
- 公共 DNS 经代理访问，代理端必须允许到公共解析器 TCP 53 的连接。服务器域名
  自身的引导解析仍由 Mac 完成；需要避免它时可填写代理 IP。
- bypass 是显式直连例外；ICMP 仍由独立规则决定，SOCKS5 不能承载 ICMP。
- 外部 `arcbox-boot` 的下载、桌面 URLSession 请求仍有此前覆盖矩阵记录的限制；
  本次没有把整个应用或 Mac 的所有流量切入 SOCKS5。
- helper 的 DNS/路由安装和官方团队签名策略不属于此项修改。
