# 2026-10-23 — VZ/HV 的 UDP 与 DNS 功能验收及 HV 修复

- Type: investigation / change
- Area: HV machine exec、VirtioFS read-only、UDP forwarding、DNS e2e
- Base: `20deab896a8f40f4aeb51e4bebeba7b6de41dde9`，加本地未提交修复
- Related: [上一轮 VZ/TCP 验收](2026-10-23-isolated-machines-e2e.md)

## 当前结论

**VZ 与 HV 的完整功能用例通过；外部 DNS 抓包尚未完成，因此 DNS 泄漏的最终验收没有关闭。** 本机 BPF 设备仅 root 可读，`sudo -n` 返回需要密码；没有把空抓包或单纯的功能测试当作无泄漏证明。

| 检查 | VZ | HV |
|---|---|---|
| Mac 文件不可挂载、专用 agent 目录只读 | 通过 | 通过 |
| 单张设备网卡、MAC 与分配的 machine-id | 通过 | 通过 |
| 禁止访问真实监听中的 Mac 服务 | 通过 | 通过 |
| TCP 和 SOCKS5 UDP 出网 | 通过 | 通过 |
| 网关 DNS 经 SOCKS CONNECT；显式解析器 DNS 经 SOCKS UDP | 通过 | 通过 |
| DNS 绕过列表不能绕开隔离；UDP=direct 被阻断 | 通过 | 通过 |
| 代理关闭后的 TCP/UDP 阻断、DNS 无答案或 SERVFAIL | 通过 | 通过 |
| 恢复代理后 TCP、UDP、DNS 无需重启恢复 | 通过 | 通过 |
| 不同宿主/客体端口的 TCP/UDP 转发、删除、恢复、停机释放 | 通过 | 通过 |
| 外部 UDP/53 是否出现重复/回退 DNS 查询 | 待管理员抓包 | 待管理员抓包 |

最终实机用例：VZ 1 passed / 0 failed，34.58 秒；HV 1 passed / 0 failed，48.99 秒。运行环境为 Darwin 25.4.0 arm64、boot assets 0.8.10。

## HV 测试拓扑

系统 VM 保持 VZ，受测隔离机器分别选择 VZ/HV。Create 接口当前默认 VZ；HV 用例在所属测试 daemon 停止后修改该临时机器的持久化 backend，再重启和等待 `WatchSetupStatus`。这是测试配置，不代表桌面已经新增 backend 选择器。

第一次同时启动 HV 系统 VM 与 HV 隔离机器得到 `hv_vm_create failed: resource is busy`。这符合 [Apple Hypervisor.framework 的每进程一台 VM 限制](https://developer.apple.com/documentation/hypervisor?hl=ca)；本次没有实现多 HV 子进程架构。日志明确打印 `system=vz isolated_machine=hv`，metrics 的 `backend` 是受测机器的后端。

## 实机发现并修复的问题

### HV 不能执行流式命令

机器能启动，但 Exec 返回 `streaming RPCs not supported on blocking transport`。HV 的原始 vsock fd 必须保留阻塞式 poll I/O，不能直接切回会在连接重试中卡住的 kqueue 路径。

新增 `agent_client/session_io.rs`：独立阻塞读写 worker 通过有界 channel 与既有 exec 流控连接。两端任意一端释放都会 shutdown socket，唤醒阻塞操作；不会在超时后重用读到一半的 frame。机器 Exec、交互 session 和 machine TCP connect 使用该适配；未宣称全部 sandbox streaming RPC 已支持 HV。

Agent-client 回归：46 passed，覆盖输入/输出、完成、空闲读取消，以及收到 stdin window 前取消。

### HV 忽略共享目录的 read-only 标志

实机在隔离机器的专用共享目录成功创建了 `guest-write-probe`，确认不是单纯显示问题。HV 没有把 `SharedDirConfig.read_only` 传到 `FsServer`。

新增 FsConfig/DispatcherConfig 的 read-only 配置，并在 host dispatcher 拒绝创建、删除、重命名、写入、属性/xattr 修改、空间分配、写入方式 Open 和可写 DAX SetupMapping。Open 判断使用 Linux guest 的标志位，不能用 Darwin libc 常量代替。只读 metadata、读取和只读 DAX 保持可用。

Filesystem 回归：68 passed，1 ignored；包含有效 CREATE 在可写共享上成功、只读共享上 EROFS，以及只读 DAX 可用、可写 DAX 不会触达 mapper。HV 实机随后返回 `Read-only file system`。

### 测试工具修正

- 代理断开时网关返回无答案的 SERVFAIL 是正确拒绝，不能误报为泄漏。
- 公网 TCP 对照使用静态 Linux probe 明确等待响应，避免 `nc` 在 stdin EOF 时提前退出造成不稳定结果。
- 记录代理支持 UDP ASSOCIATE 和专用 DNS 响应；只有代理路径返回 `198.51.100.77`，各阶段使用唯一 `abxleak-*` 查询名。
- 临时 XDG 配置、私有 Helper socket 和独立数据目录继续隔离用户状态。所有测试 VM 的磁盘在归档日志后清理。

## 补齐 DNS 抓包

从 `arcbox/` 在一个终端启动：

```sh
sudo sh tests/e2e/scripts/capture_isolation_dns.sh /tmp/arcbox-dns.pcap
```

此脚本只捕获首个 DNS label 以 `abxl` 开头的 UDP/53 包，打开 BPF 后降权。开始抓包后，分别执行以下两种 backend 的实机用例并保存输出：

```sh
SKIP_BUILD=1 KEEP_TEST_DIR=1 CARGO_BUILD_JOBS=2 \
  ARCBOX_ISOLATION_BACKEND=vz ARCBOX_E2E_PUBLIC_CONTROL_IP=153.3.238.127 \
  cargo test -p arcbox-e2e --test machine_isolation -- --ignored --nocapture --test-threads=1 > /tmp/dns-vz.log 2>&1
SKIP_BUILD=1 KEEP_TEST_DIR=1 CARGO_BUILD_JOBS=2 \
  ARCBOX_ISOLATION_BACKEND=hv ARCBOX_E2E_PUBLIC_CONTROL_IP=153.3.238.127 \
  cargo test -p arcbox-e2e --test machine_isolation -- --ignored --nocapture --test-threads=1 > /tmp/dns-hv.log 2>&1
```

停止抓包后：

```sh
python3 tests/e2e/scripts/check_isolation_dns_capture.py /tmp/arcbox-dns.pcap /tmp/dns-vz.log /tmp/dns-hv.log
```

检查器要求每轮开始与结束的宿主直连 DNS 对照均被捕获，而客体六个阶段的查询名均未在外部 UDP/53 出现。空抓包、开始过晚、功能用例失败均不通过。检查器 4 项单测通过；BPF 过滤器还用人工 pcap 验证过只匹配测试名，**人工 pcap 不是实机抓包证据**。

## 最近新增功能

| 功能 | 代码中的行为 |
|---|---|
| Proxies 主页面 | 保存/导入 SOCKS 代理、连接测试、默认代理切换、逐机器分配 |
| 资源和剪贴板 | CPU/内存上限、重启应用资源设置、由 Mac 发起的 X11 剪贴板传输 |
| 严格隔离机器 | 移除隐式 Mac 文件共享和桥接网卡，强制指定代理，失败时阻断 |
| 独立身份 | MAC、machine-id、克隆后新身份；时区与语言手动设置 |
| 端口转发 | TCP/UDP、运行中更新、VNC 端口预设和入口 |
| 隔离自检 | 文件/网卡/连接等探针及结果展示；不能代替完整泄漏验收 |
| 本轮 HV 补齐 | 流式机器命令、只读共享的 host 端强制执行 |

VNC 图形会话、跨机器互访、所有镜像的时区/tzdata，以及原有系统 Helper/DNS resolver/容器路由问题不在本轮通过范围。

证据位于仓库根 `.local/remaining-acceptance/`：`hv-explicit-tcp-probe.metrics.json`、`vz-final-regression.metrics.json`、`logs/` 和 `runs/`。相关 Clippy（`-D warnings`）已通过。
