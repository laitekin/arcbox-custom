# 2026-10-23 — 自导入镜像、VNC、双机隔离验收与开发 Helper 修复

- Type: investigation / change
- Area: guest shutdown、machine import、port forwarding、Helper、desktop packaging
- Commits / PRs: 未提交；基线 `20deab896a8f40f4aeb51e4bebeba7b6de41dde9` 加本地修改
- Related: [上一轮 HV/UDP/DNS 验收](2026-10-23-hv-udp-dns-acceptance.md)、[Helper 使用说明](../helper.md)

**仍未闭环的两项：管理员 Helper 的真实系统 DNS/路由变更、宿主外部 DNS 抓包。**
本机没有 sudo 缓存授权，用户此前选择暂不管理员抓包。本记录不把沙箱测试或
代理功能测试当作这两项的通过证据。
新版开发应用已打开，但其 daemon 正等待 macOS 钥匙串授权，尚未进入可用状态；
用户需自行处理系统提示，密码不应发送到对话。

## 范围和结果

环境：Apple Silicon，Darwin 25.4.0 arm64，boot assets 0.8.10。构建并发限制为 2。
实机 E2E 使用独立数据目录、临时代理配置和不可用的私有 Helper socket；不修改
系统 resolver、路由或用户代理配置。VZ/HV 最终隔离用例执行前关闭了桌面实例。

| 项目 | 本轮结果 | 证据 |
|---|---|---|
| Ubuntu 24.04 Docker 导入、启动、重启、导出恢复 | 通过，203.01 秒 | E-001 |
| Alpine 3.20 同一生命周期 | 通过，26.83 秒 | E-002 |
| Ubuntu/Alpine 时区 | `Asia/Tokyo` 三次启动均为 `+0900` | E-001、E-002 |
| Ubuntu VNC | 真正的 Xvnc，640×480 彩色 RFB 帧经过 localhost 转发 | E-001 |
| VZ 隔离、UDP、DNS、双机互访阻断 | 通过，77.30 秒 | E-003 |
| HV 隔离、UDP、DNS、双机互访阻断 | 通过，92.18 秒 | E-004 |
| Helper 验签与 DNS 文件操作沙箱 | 24 单测 + 1 活进程验签 + 13 文件系统 E2E 通过；Release 验签通过 | E-005 |
| Desktop 打包工具 | 24 单测 + 2 集成测试通过，Clippy 无告警，完整开发包构建成功 | E-006 |
| 实际系统 DNS / 容器路由 | 等待用户启动 root Helper | 未验收 |
| 外部 DNS 泄漏抓包 | 未执行，遵守此前暂不抓包选择 | 未验收 |

HV 场景是 VZ 系统 VM、HV 第一台隔离机器、VZ 第二台隔离机器；不是同进程
同时运行两台 HV。两台隔离机器的 TCP/UDP 服务先后都通过宿主转发正向验证，
再检查双向直连和经网关访问对方宿主转发端口均被阻断，避免把服务未启动误判为隔离。

VNC 验证覆盖 X server 和图形传输，不表示已经验收完整 GNOME 桌面或所有图形应用。
发行版支持结论限定于实测的 Ubuntu 24.04 和 Alpine 3.20 ARM64；不包含安装 ISO。

## 问题、修复与调用路径

### F-001：Ubuntu 关机后持久文件变成零长度

证据 E-001 及 `ubuntu-reproduce.log`、`ubuntu-before-after.log`：原机器在导出前
已经出现零长度 BusyBox；恢复后报 `Exec format error`，随后 PID 1 panic。
因此不是归档解包造成的数据损坏。boot shim 懒卸载 overlay 背后的 Btrfs 挂载，
发行版关机只看到 overlay；在请求 init 关机前执行全局 `sync()`，确保隐藏 backing
superblock 的数据写回。位置：`guest/arcbox-agent/src/shutdown.rs`。

P-001：机器 Stop RPC → agent 的 `DistroInit` → `sync()` → 发行版 poweroff →
重启/导出恢复 → 比较 BusyBox 和新写入 1 MiB 文件的 SHA-256。测试自身不调用 sync。
修复后的 Ubuntu、Alpine 路径均通过（E-001、E-002）。此结论针对正常关机，
不承诺强制断电或宿主崩溃时未落盘数据不丢失。

### F-002：机器端口转发被 Docker 定时清理

证据 E-003、E-004 及 `forward-owner-test.log`：机器监听器此前使用容器所有权
命名，被 Docker reconciliation 当成不存在的容器清除。新增统一的
`Runtime::machine_forward_owner`，并在容器所有权判断中排除 `machine-forward:`。
位置：`app/arcbox-core/src/runtime.rs`、`app/arcbox-api/src/connect/machine/forwards.rs`。

P-002：机器设置转发 → 独立机器 owner → 等待超过 Docker reconciliation 周期 →
TCP/UDP echo 成功 → 动态移除/恢复 → 停机后端口释放（E-003、E-004）。

### F-003：开发签名与 Helper 不匹配，打包校验也未接受私有配置

证据 E-005：生产 Helper 拒绝 ad-hoc 是原本的安全策略。新增显式、进程生命周期内
的 `UID:CDHash` 授权，仅允许同 UID 且审计 token 对应实时签名匹配的客户进程。
私有 socket 必须位于该用户拥有的 0700 目录；无 debug 绕过或 PID 降级。
正式版 Team ID 验证保持原策略。

P-003：`scripts/start-development-helper.sh` 核验签名、读取已打包 daemon/CLI 的
CDHash → root 启动独立 Helper → 私有 socket 验证 UID 和 live CDHash → 既有
参数校验和宿主变更实现。每次重编译后需重新启动 Helper 以更新授权。
真实系统变更仍等待管理员步骤，E-005 只能证明认证及沙箱操作。

首次完整打包发现 LaunchAgent 写入了开发 DNS 参数，而 bundle verifier 仍只允许
旧参数数组。现在生成器和校验器共用 `launch_agent_arguments`，从 plist 中验证
Helper 配置，拒绝生产 profile 使用开发 override，并测试参数与 socket 不一致的
情况。修复后完整构建通过（E-006）。

### F-004：自导入精简镜像缺少时区数据

`add_init` 的发行版包安装路径增加 `tzdata`（zypper 路径为 `timezone`）。
Ubuntu/Alpine 导入后在首次启动、重启及恢复后均报告 `+0900`（E-001、E-002）。
位置：`engine/arcbox-image/src/machine_import/mod.rs`。已有的本地 rootfs 导入不
自动联网补包；用户需准备好 init 和时区数据。

## 证据索引

观察日期均为 2026-10-23；源类型为本机命令日志。原始日志、metrics、画面及
SHA-256 清单在仓库根目录 `.local/final-acceptance/`，该目录不提交。
下表 hash 为对应日志完整 SHA-256 的前 12 位；完整值见 `sha256.json`。

| ID | source_ref | hash 前缀 | 原始结果摘录 |
|---|---|---|---|
| E-001 | `ubuntu-docker-final.log`、`ubuntu-evidence/` | `d4920c218f7d` | `1 passed`；`PASS real Xvnc desktop: 640x480` |
| E-002 | `alpine-final.log`、`alpine-evidence/` | `cdfb04fc4408` | `1 passed`；三次 `offset="+0900"` |
| E-003 | `isolation-vz-final.log`、`vz-evidence/` | `ea8acbb6c3aa` | `1 passed`；`PASS two running isolated machines` |
| E-004 | `isolation-hv-final.log`、`hv-evidence/` | `afb55c491a6a` | `1 passed`；`PASS stop releases TCP and UDP forwarded ports` |
| E-005 | `helper-final-tests.log`、`helper-release-auth-fixed.log` | `a83e9669eb48` / `c1a3f8c15b73` | `24 passed`、`1 passed`、`13 passed`；Release `1 passed` |
| E-006 | `desktop-final-build-fixed.log`、`desktop-xtask-final-tests.log` | `28a469dbee96` | `BUILD SUCCEEDED`；24 + 2 测试通过 |

复现 E-001 至 E-004：参见 [E2E commands](../../tests/e2e/README.md#commands)，
两个后端分别设置 `ARCBOX_ISOLATION_BACKEND=vz` / `hv`，本机公网 HTTP 对照使用
`ARCBOX_E2E_PUBLIC_CONTROL_IP=153.3.238.127`。网络环境不同需提供真实可达对照，
不能跳过正向对照。Ubuntu VNC fixture 首次下载包需要能访问上游镜像源。

复现 E-005，从 `arcbox/` 执行：

```sh
CARGO_BUILD_JOBS=2 cargo test -p arcbox-helper --bin arcbox-helper --test development_peer_test --test e2e_fs_test
CARGO_BUILD_JOBS=2 cargo test --release -p arcbox-helper --test development_peer_test
```

E-006 的构建选项与完整输出保存在上述日志；开发包使用本地 runtime、ad-hoc
签名、`ARCBOX_HELPER_SOCKET` 私有路径和 Xcode `-jobs 2`。
Host 与 Linux guest Clippy 通过；打包的 guest agent 与本次构建 SHA-256 一致：
`50a07a432dbb76339cc7779b62581e0543a3331af34ca80ffbcb7c3cc569af8e`。
完整 app 的 `codesign --verify --deep --strict` 通过。
四个成功场景的日志、metrics、VNC 帧已归档，确认无挂载和打开文件后清理了各自的
临时测试目录，回收约 2.7 GiB；失败复现的磁盘证据仍保留。

### 桌面启动现场

12:51（Asia/Shanghai）的 `desktop-startup-sample.txt` 显示新 daemon 主线程阻塞
在 `SecItemCopyMatching → SecKeychainItemCopyContent → SecurityServer::decrypt`，
系统 SecurityAgent 同时运行；UI 停在 Boot assets ready。调用路径是
`Runtime::new → NetworkProxy::load_library` 读取保存的代理凭据。进程 CPU 约 0%，
没有继续运行编译或实机测试。计算机控制工具明确禁止操作 SecurityAgent，已请用户
在本机处理提示。未导出凭据、修改钥匙串 ACL 或关闭代理隔离规则来跳过授权。

当前 `doctor` 记录不通过，包含启动未完成导致的 Docker socket 缺失，以及尚未
启动私有 Helper 导致的宿主网络项；不能把构建成功写成应用已完全就绪。
用户配置 SHA-256 仍为
`24ae5207f29da27921bc09149cebe097d06631179d303f50abbc3e31c18947dc`。

## 剩余验收入口

1. 用户在终端以 sudo 启动已签名的私有 Helper；具体命令已在任务中提供，
   入口为 `scripts/start-development-helper.sh`。不覆盖系统已安装的正式 Helper。
2. 重启开发 daemon，设置相同 `ARCBOX_HELPER_SOCKET` 运行
   `abctl --profile development doctor`，并实测系统解析和容器 IP 可达性。
3. 用户明确恢复管理员抓包后，按上一轮记录启动限定测试域名的 capture，再运行
   VZ/HV 场景及 pcap auditor。功能用例成功、空抓包和人工 pcap 都不能替代此步骤。
