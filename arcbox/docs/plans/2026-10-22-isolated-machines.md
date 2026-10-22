# 隔离机器方案设计

状态：已确认采用**严格隔离**；P1–P5 已实现（见 §10），待在 Mac 上验收　　日期：2026-10-22

## 1. 目标

在 ArcBox 中新增「隔离机器」。隔离机器里的程序，即使拿到 root 权限，也做不到下面几件事：

| 编号 | 要求 |
|---|---|
| R1 | 读写 Mac 上的任何文件（用户明确共享的目录除外，默认只读） |
| R2 | 不经过指定代理就连上互联网；代理不可用时直接断网，不退回直连 |
| R3 | 访问 Mac 本机上的服务、局域网设备，以及其他机器 |
| R4 | 和其他机器共用可识别的身份（网卡 MAC、machine-id、主机名） |

同时要保证：Mac 仍然能管理这台机器，包括终端、VNC、剪贴板（只能由 Mac 发起）、资源调整、启动和停止。

### 不做的事

- **伪装物理机、对抗虚拟机检测**。CPU 型号、virtio 设备、虚拟化标志由 Apple 虚拟化框架决定，改不彻底，维护成本也高。
- **防御针对 Apple 虚拟化层本身的攻击**，超出本项目范围。
- **Docker 容器的隔离**。所有容器共用 System VM 的内核，属于容器级隔离。需要隔离的工作负载应放进机器里运行。

## 2. 参考模型

### 2.1 Claude Code 云端会话服务器（实测）

| 层 | 做法 |
|---|---|
| 计算 | 每个会话一台 Firecracker 微型虚拟机（KVM），有独立内核；`nomodule`，关闭 IPv6 |
| 文件 | 不共享宿主机任何目录。只有自己的一块读写盘，工具以只读镜像（`ro,nosuid,nodev`）挂入 |
| 控制 | 宿主机通过 vsock 管理虚拟机；`--block-local-connections` |
| 网络 | 只有一张网卡；所有出网流量被强制送进有策略的代理，绕开代理直连也会被截住；网关不对它开放服务 |
| 凭据 | 真正的密钥不进虚拟机，由外部代理在转发时加上 |

### 2.2 OrbStack 隔离机器（[官方文档](https://docs.orbstack.dev/machines/isolated)）

`orb create --isolated`：不挂载 Mac 文件，不能执行 `mac` 命令，禁止访问 Mac 本机网络，SSH agent 转发和 USB、声音透传都关闭。机器之间默认仍能互访，要再加 `--isolate-network` 才禁止。

OrbStack 的所有机器共用一个内核，官方也说明这不是完整的安全边界。

### 2.3 结论

ArcBox 的底层是每台机器一个独立虚拟机，比 OrbStack 强，和 2.1 的服务器属于同一级别。要补的是文件、网络和身份这三层。做法照 2.1：**所有限制都在 Mac 这一侧强制执行，机器内部的配置一律不算数。**

## 3. 威胁模型

- 假设机器内的 root 不可信：它能改路由、挂载文件系统、改 iptables、改 MAC、自己发原始数据包。
- 所以每项隔离都必须由 Mac 端（daemon、虚拟化层、vmnet）强制执行。机器内的配置（比如桥接网卡不设默认路由）只能算辅助，不算边界。
- Mac 端的 ArcBox daemon、Apple 虚拟化框架和 macOS 视为可信。

## 4. 现状审计（代码证据）

| 编号 | 缺口 | 位置 | 违反 |
|---|---|---|---|
| G1 | 每台机器都通过 VirtioFS 共享了 Mac 的 `/Users`、`/private`，以及 ArcBox 的整个数据目录，全部**可读写**。即使启动脚本不自动挂载，机器里的 root 执行 `mount -t virtiofs users /mnt` 就能读写 | `engine/arcbox-engine/src/machine.rs:327`（`vm_shared_dirs`） | R1 |
| G2 | 第二张网卡（桥接网卡）使用 vmnet **shared（NAT）模式**，失败时退回 `VZNATNetworkDeviceAttachment`，同样是 NAT。「不设默认路由」只是机器内部的配置，root 自己加一条路由，就能用 **Mac 的真实 IP 直接上网，完全绕过代理** | `virt/arcbox-vmm/src/vmm/darwin.rs:120-150`；`guest/arcbox-agent/src/init.rs:490` | R2、R3 |
| G3 | 机器和其他机器、Mac 在同一桥接网段，能互相访问；用户态网关 `192.168.64.1` 后面就是 Mac | 同上；`virt/arcbox-net/src/darwin/datapath_loop/` | R3 |
| G4 | 代理可以选「直连」或「跟随默认」；绕过列表（exclude）可以让流量直连；UDP 可以选直连；ICMP 默认直连 | `common/arcbox-fakeip/src/proxy_state.rs`；`proxy_routes.rs` | R2 |
| G5 | 两个 MAC 地址都是 `02:` 开头的「本地管理地址」；克隆机器会复制整块数据盘，`machine-id` 可能跟着一起被复制（待实机验证）；时区不设置 | `app/arcbox-core/src/macos/machine.rs:52`；`engine/arcbox-engine/src/vm/types.rs:41`；`engine/arcbox-engine/src/machine/clone.rs` | R4 |
| G6 | Mac 会通过 NFS 挂载机器的根目录（`~/ArcBox/machines/…`），并注册 `.arcbox.local` 域名。方向是 Mac → 机器，本身不违反隔离，但依赖桥接网卡 | `app/arcbox-daemon/src/machine_mount/`、`machine_dns.rs` | — |
| G7 | 开发版 daemon 连不上 release 版 Helper（签名校验拒绝），DNS 和路由没配置上 | `app/arcbox-helper/src/server/peer_auth.rs:48` | 与隔离无关，见 §9 |

已经满足的：
- 计算层：每台机器独立虚拟机。
- 控制层：exec、终端、剪贴板都走 vsock，只能由 Mac 发起。
- 凭据层：代理密码存在 Mac 钥匙串里，由 Mac 端完成 SOCKS 认证，机器里看不到。
- 代理认证失败时不会退回直连。

## 5. 设计

### 5.1 数据模型与接口

- `CreateMachineRequest` 增加 `bool isolated`，`MachineSummary`、`MachineInfo` 中回显。
- 新增 `IsolationLevel`：
  - `STANDARD`（标准隔离）：保留一张仅主机可见的桥接网卡，Mac 能用机器 IP 访问它（比如 VNC）。
  - `STRICT`（严格隔离）：没有桥接网卡，Mac 只能通过 ArcBox 的转发通道访问它。
- 写入机器的持久化配置，每次启动时读取。已有机器只能在**停止状态**下切换隔离设置，重启后生效（与 `SetResources` 的处理方式一致）。
- CLI：`abctl machine create --isolated[=strict]`、`abctl machine config <name> --isolated …`。
- 隔离机器的代理只能是**某个已保存的代理**。`SetMachineProxy` 拒绝 `direct` 和「跟随默认」；被隔离机器使用的代理不能删除。

### 5.2 文件层（R1）

- `vm_shared_dirs(isolated)`：隔离机器**不挂** `users`、`private`。
- ArcBox 数据目录（`arcbox` 标签）：先查清机器启动和 agent 实际需要哪些文件（P1 第一步）。之后只共享一个**按机器划分的只读子目录**，或者完全不共享。
- 用户手动添加的共享目录：允许，**默认只读**，界面上明确提示「这会打破文件隔离」。
- 不对隔离机器启用 SSH agent 转发（`app/arcbox-daemon/src/ssh_agent.rs`）。

### 5.3 网络层（R2、R3）

**原则：隔离机器只有一个出口，就是 eth0 的用户态数据通路（ArcBox 自己的代码），并且在这里逐个连接强制执行策略。**

1. **桥接网卡（解决 G2、G3）**
   - 标准隔离：改用 `VmnetConfig::host_only().with_isolation(true)`（`virt/arcbox-vmnet/src/interface.rs:85,133`，接口已经有了）。仅主机模式**没有通往互联网的 NAT**；vmnet 的隔离开关**禁止机器之间互访**；Mac 仍能访问机器 IP。vmnet 启动失败时**不挂桥接网卡**，绝不退回 VZNAT。
   - 严格隔离：不挂桥接网卡。
   - 标准隔离剩下的口子：机器仍能访问 Mac 在这个网段上的 IP，也就能连到 Mac 上「监听所有网卡」的服务。要堵住它，需要通过 Helper 加 pf 防火墙规则，这又依赖 §9 的 Helper 问题。所以「完全隔离」推荐用**严格隔离**。

2. **用户态数据通路的逐机器策略**。在 `ProxySnapshot` 中加入 `isolation: Option<IsolationPolicy>`，下发方式沿用已有的 `ProxyRoutes::for_machine`：
   - **必须有代理**：没有可用的代理环境时，拒绝建立新连接（fail-closed）。
   - 忽略绕过列表，所有目标都走代理。
   - **禁止访问的目标**：网关 `192.168.64.1` 上除 DNS 以外的所有端口；私有网段（`10/8`、`172.16/12`、`192.168/16`）、CGNAT（`100.64/10`）、链路本地地址（`169.254/16`）、环回地址、组播；其他机器的地址。
   - UDP 只能走代理（SOCKS5 UDP ASSOCIATE）或者阻止；ICMP 阻止；IPv6 阻止。
   - DNS：网关上的 DNS 只为隔离机器提供通过代理隧道的解析（socks5h 远程 DNS），不解析 `.arcbox.local` 等本地名字；机器自己往 `8.8.8.8:53` 发的 UDP 查询按上一条的 UDP 规则处理。
   - 执行位置：
     - TCP：`tcp_bridge.handle_outbound_syn`（`datapath_loop/mod.rs:376`）；
     - UDP、ICMP：egress socket proxy；
     - DNS：`intercept.rs:154` 的 `handle_dns`。
   - 被拒绝的连接回 RST 或 ICMP 不可达，并计数，供界面和自检显示。

3. **代理凭据**：保持现状，在 Mac 端完成认证，密码不进机器。

### 5.4 控制层

- 保留 vsock 上的 exec、终端、剪贴板（只能由 Mac 发起）、资源调整。
- 严格隔离下给 Mac 提供访问通道：
  - **端口转发**：daemon 在 Mac 上监听 `127.0.0.1:<端口>`，经入站中继转到机器里的端口。现有的入站中继和 sandbox 端口转发机制（`guest/arcbox-agent/src/agent/linux/port_forward.rs`）可以扩展到机器。界面上预设一项「VNC 5901」，你用 `vnc://localhost:5901` 连接。
  - SSH：现有的 `ssh <machine>@arcbox` 走 loopback 和 exec 通道，不受影响。
- 隔离机器默认关闭 Mac 端的 NFS 根目录挂载和 `.arcbox.local` 注册（G6）。严格隔离下它们本来也用不了。

### 5.5 身份层（R4）

- 两张网卡的 MAC：改用**带真实厂商前缀（OUI）的随机地址**，每台机器不同，创建时生成后固定。
- `machine-id`：创建和克隆时，由 Mac 端通过内核启动参数下发一个新的 ID，agent 每次启动时写入 `/etc/machine-id`。这样克隆机器也不会和源机器重复。
- 主机名：默认仍是机器名，可选改用中性的随机主机名。
- 时区和语言：每台机器可以单独设置。界面上提供「与代理出口所在地一致」的手动选择；可选通过代理查询出口 IP 的所在地，这一步会访问外部服务，默认关闭。
- 不做：改 CPU 信息、隐藏虚拟化标志等对抗虚拟机检测的工作（见 §1）。

### 5.6 界面（主窗口）

- **新建机器**：勾选「隔离机器」→ 选择 标准 / 严格 → 必须选择一个已保存的代理（不提供「直连」和「跟随默认」）→ 可选时区和语言。
- **机器列表**：隔离机器显示盾牌标记。
- **Info 页**新增「隔离」区：显示当前级别和每一层的状态，提供「隔离自检」按钮（§5.7）。机器停止时可以切换级别。
- **Proxies 页**：隔离机器那一行的选项里没有「跟随默认」和「直连」；正被隔离机器使用的代理不能删除。
- 剪贴板区保留，并说明「仅在你点击时，由 Mac 发起传输」。

### 5.7 隔离自检

通过 agent exec 在机器里实际测试，再和 Mac 端的计数对照，每项给出 ✅ 或 ❌：

| 检查 | 方法 | 期望 |
|---|---|---|
| Mac 文件 | 在机器里尝试 `mount -t virtiofs users`、`private` | 失败 |
| 桥接网卡出网 | 临时经桥接网关加一条路由，再访问外网 | 失败（严格隔离：没有这张网卡） |
| 访问 Mac、局域网、其他机器 | TCP 连接 `192.168.64.1` 的常见端口、Mac 的局域网 IP、另一台机器的 IP | 全部被拒，Mac 端拒绝计数增加 |
| 绕过代理 | 直连 `1.1.1.1:443`，以及访问 IP 回显服务 | 出口 IP 等于代理的出口 IP |
| DNS | 直接向 `8.8.8.8:53` 发 UDP 查询 | 被阻止，或经代理转发 |
| ICMP、IPv6 | `ping 1.1.1.1`，IPv6 连接 | 失败 |
| 身份 | 读 MAC 前缀和 `machine-id`，与其他机器比较 | MAC 不是 `02:` 开头；machine-id 各不相同 |

同样的检查写进 e2e 测试（`tests/e2e/tests/machine_isolation.rs`），每次改动都回归验证。

## 6. 实施计划

| 阶段 | 内容 | 验证 |
|---|---|---|
| P1 文件隔离 + 数据模型 | proto 字段、持久化、CLI、`vm_shared_dirs`；先查清数据目录共享的实际用途；新建机器勾选项、列表标记 | 单元测试；在 Mac 上实测机器里挂载 `users` 失败 |
| P2 用户态网络策略 | `IsolationPolicy`、必须有代理、目标黑名单、UDP/ICMP/IPv6、DNS；代理选择限制 | `arcbox-net` 单元测试（伪造 SYN、UDP、ICMP 帧）；e2e |
| P3 桥接网卡 | 标准隔离用 vmnet 仅主机 + 隔离；严格隔离不挂网卡；禁止退回 VZNAT；机器端口转发和界面 | 在 Mac 上实测加路由也出不了网、机器之间互访失败、VNC 经 localhost 能连上 |
| P4 身份 | 带厂商前缀的 MAC、下发 machine-id、时区和语言 | 单元测试；克隆后 machine-id 不同 |
| P5 自检与收尾 | Info 页隔离区、自检、e2e、文档 | 自检全部 ✅；e2e 在 Mac 上运行 |

P1 到 P2 可以在 Linux 上开发和做单元测试；P3 及所有实机验收必须在 Mac 上完成。

## 7. 风险

- vmnet 仅主机模式需要 `com.apple.vm.networking` 权限（已有），而本地开发版是临时签名，可能被系统拒绝。拒绝时按设计不挂桥接网卡，相当于降级成严格隔离。
- HV 和 VZ 两种后端的网卡路径不同，两种都要测试。
- 数据目录共享的用途还没查清，关掉后可能影响 agent 的日志或运行时文件（P1 第一步验证）。
- SOCKS5 代理大多不支持 UDP 转发，开启隔离后 QUIC、自定义 DNS 等 UDP 应用会被阻止，这是预期行为。

## 8. 待确认

1. 默认级别用「严格」还是「标准」？（推荐严格，不依赖 Helper）
2. 你现在 VNC 填的是机器 IP 吗？严格隔离下要改成 `vnc://localhost:端口`。
3. 机器之间是否完全禁止互访？
4. 时区和语言是手动设置，还是需要「按代理出口自动设置」？
5. 已有机器是否需要一键「转为隔离机器」（停机后切换）？

## 9. 与 Helper 签名问题的关系

- Helper 是 root 权限的系统集成服务，负责 Mac 上的 DNS（`.arcbox.local`）和容器路由。release 版 Helper 只接受官方 Team ID 签名的调用方，本地开发版是临时签名，所以被拒绝。
- 隔离机器的设计**不依赖 Helper**：严格隔离不需要域名和路由，策略都在 daemon 自己的数据通路里执行。
- 只有「标准隔离 + pf 防火墙」这条可选路线需要 Helper。届时按项目约定使用开发版 Helper 和私有 socket（`make run-helper`，或 `ARCBOX_HELPER_SOCKET=/tmp/arcbox-helper.sock`，见 `.agents/skills/local-dev.md`）。

## 10. 实施记录

### P1：文件隔离与数据模型（已完成）

| 部分 | 实现 |
|---|---|
| 协议 | `CreateMachineRequest.isolated`（14）、`MachineSummary.isolated`（11）、`MachineInfo.isolated`（11）；新增 `MachineService.SetIsolation(SetMachineIsolationRequest)`，只接受停止状态的机器 |
| 引擎 | `MachineConfig`、`MachineInfo`、持久化记录和导出归档都带 `isolated` 字段，旧记录读出来默认为 `false`。`vm_shared_dirs(isolated)`：隔离机器不挂 `users`、`private`；`arcbox` 标签改为指向机器自己的目录 `machines/<name>/guest-share`（启动脚本不在本仓库，保留这个标签更稳妥）。这个目录建不出来时就不挂，**绝不退回共享整个数据目录**。克隆、导出再导入都保留隔离状态。`set_isolated` 只允许在机器停止时调用，并会重建共享目录列表 |
| CLI | `abctl machine create --isolated`、`abctl machine isolate <name> [--off]`；`status`、`inspect` 显示隔离状态 |
| 桌面 | 新建机器时可勾选「Isolate from my Mac」，勾选后只能选已保存的代理；机器列表显示「Isolated ·」；Info 页新增「Isolation」区，列出哪些已经隔离、哪些还没有（网络一项明确写「尚未隔离」），并提供停机后切换的按钮；Proxies 页和 Info 页的代理菜单对隔离机器不提供「跟随默认」和「直连」 |
| 测试 | 引擎 3 项（共享目录列表、目录建不出来时不退回、创建/克隆/切换/持久化/旧记录兼容）；CLI 参数解析 1 项；Swift 5 项。引擎、API、CLI 全部测试通过；Swift 逻辑在 Linux 上用临时包编译测试通过 |

说明：
- SSH agent 转发在 ArcBox 中只对 System VM 里的 Docker 容器生效，不会进入机器，所以 §5.2 中这一项不需要改。
- 「必须使用已保存的代理」目前只在界面上强制，daemon 端的强制属于 P2。
- 需要在 Mac 上验收：隔离机器能正常启动；机器里执行 `mount -t virtiofs users /mnt` 失败；agent 日志写进 `machines/<name>/guest-share`。

### P2：网络策略（已完成）

| 部分 | 实现 |
|---|---|
| 策略核心 | `arcbox-fakeip::isolation::allows_destination`：拒绝网关（即 Mac 本机）、`0/8`、环回地址、私有网段、`100.64/10`、链路本地地址、组播和 `240/4`；允许公网地址和 Fake-IP 网段 `198.18/15`（由代理按域名解析）。`ProxySnapshot.isolated` 由 `ProxyRoutes::set_isolated` 按机器设置，之后无论默认代理还是机器自己的代理怎么变，这个标记都保留 |
| TCP | `splicetcp::DefaultEgress`：隔离机器忽略绕过列表，必须走代理；没有可用代理，或目标被拒绝时，立即返回空连接（向机器回 RST） |
| UDP / ICMP | `arcbox-proxy`：UDP 只能经 SOCKS5 UDP 转发，策略是 Direct 或 Block、只有 HTTP 代理、目标被拒绝时一律丢弃；ICMP 全部丢弃 |
| DNS | `arcbox-net`：隔离机器的查询一律经 SOCKS5 隧道发往 1.1.1.1 或 8.8.8.8（不管远程 DNS 开关和绕过列表）；没有 SOCKS5 时不返回结果；跳过本地名字解析 |
| IPv6 | 数据通路本来就整体丢弃 IPv6（已有测试覆盖） |
| 接线 | 引擎启动机器时调用 `machine_network`：有逐机器路由时标记隔离；没有逐机器路由时，也给隔离机器单独一份带标记的策略，绝不共用默认策略 |
| 规则 | `SetMachineProxy` 拒绝把隔离机器设成「直连」或「跟随默认」；`SetIsolation(true)` 要求机器已经指定了已保存的代理；CLI 新增 `abctl machine proxy <机器> [default|direct|<代理名或 ID>]`，`create` 新增 `--proxy` |

### P3：严格网络（已完成）

- `VmmConfig.isolated_network`：隔离机器**不挂桥接网卡**（VZ 和 HV 两种后端都改了）。ArcBox 自己的网络栈启动失败时，**机器直接启动失败**，不再退回 Apple NAT。
- 端口转发：机器记录里保存 `port_forwards`；`SetPortForwards` 修改后对运行中的机器立即生效；机器启动时绑定，停止和删除时释放，复用 Docker 端口发布用的入站中继，只监听 `127.0.0.1`。CLI 是 `abctl machine forward <机器> <机器端口> [--host-port N] [--udp] [--remove]`。
- 守护进程：Mac 端的 NFS 挂载显式排除隔离机器（隔离机器没有桥接地址，本来也挂不上）。

### P4：身份（已完成）

- 隔离机器在创建或开启隔离时，生成一个**带真实厂商前缀的 MAC**（Intel 或 Realtek 的 OUI）和一个新的 `machine-id`；克隆出来的机器会重新生成这两项，不复制源机器的转发端口。
- 时区和语言：`CreateMachineRequest.timezone/locale`、`SetIdentity`（只能在停止状态下修改）。引擎把 `arcbox.machine_id=`、`arcbox.timezone=`、`arcbox.locale=` 写进内核启动参数；guest agent 每次启动时写入 `/etc/machine-id`（`/var/lib/dbus/machine-id` 是普通文件时也一并写），把 `/etc/localtime` 链接到对应时区（镜像里没有这个时区就保留原样），并写 `/etc/timezone`、`/etc/locale.conf`、`/etc/default/locale`。CLI 是 `abctl machine identity <机器> [--timezone] [--locale]`，以及创建时的 `--timezone/--locale`。
- 名字校验：拒绝含 `..`、空格、`=` 等字符的时区或语言名，防止路径穿越和篡改启动参数。

### P5：桌面与验证（已完成）

- Info 页的「Isolation」区：清单显示文件、网络、代理和身份都已覆盖；可以设置时区和语言（停机时）并显示网卡地址；机器运行时可点「Run Check」，在机器里实际尝试挂载 Mac 目录、数第二张网卡、连接 Mac 和局域网、ping、查看出口 IP 以及网卡地址，逐项显示通过或失败。
- 新增「Forwarded ports」区：添加或删除转发、一键「Add VNC (5901)」、对 VNC 端口提供「Open VNC」按钮（打开 `vnc://localhost:端口`）。
- e2e：`tests/e2e/tests/machine_isolation.rs`（需在 Mac 上运行，默认 ignored）实测上面每一项，并验证转发端口能连上机器里的 HTTP 服务。

### 验证情况

- Linux 上：引擎、API、CLI、agent、fakeip、splicetcp、arcbox-net、arcbox-proxy 的单元测试全部通过。以下失败与本次改动无关，未改动时同样失败：arcbox-proxy 的 ICMP 回显测试（云环境发不出 ping）；arcbox-core 的 `host_mount`、`storage_recovery::preserve` 两项（macOS 的挂载表格式、`clonefile`）。
- macOS 代码：在 Linux 上用 `--target aarch64-apple-darwin` 对 arcbox-net、arcbox-vmm（含 `vmnet` 特性）、arcbox-engine、arcbox-api、arcbox-core 做了类型检查（C/Swift 构建脚本用空库替代，只检查不链接）。arcbox-daemon 依赖 `libproc` 的绑定生成，在 Linux 上做不了这项检查。
- Swift：逻辑代码在 Linux 上用临时包编译测试通过；SwiftUI 视图未编译。
- 自检脚本已在 Linux 上实际运行，语法和输出格式都正确。

### 需要在 Mac 上验收

1. `make run` 编译运行；新建隔离机器、指定代理、启动，点「Run Check」应全部通过（出口 IP 应是代理的地址）。
2. 运行 e2e：`cargo test -p arcbox-e2e --test machine_isolation -- --ignored`。
3. 机器镜像里的 agent 来自启动资源（boot assets）。身份信息（machine-id、时区、语言）要等使用本仓库重新构建的 agent 才会生效；网络和文件隔离都在 Mac 端执行，不依赖 agent。
4. VNC：机器里的 VNC 服务必须监听所有网卡（TigerVNC 加 `-localhost no`），再在「Forwarded ports」里添加 5901。
