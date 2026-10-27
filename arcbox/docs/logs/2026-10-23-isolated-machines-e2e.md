# 2026-10-23 — 隔离机器在 Mac 上完成文件、断代理和 TCP 转发验收

- Type: investigation / change
- Area: machine engine、入站转发、desktop isolation check、e2e
- Base: `20deab896a8f40f4aeb51e4bebeba7b6de41dde9`，加本地未提交修复
- Related: [隔离机器方案](../plans/2026-10-22-isolated-machines.md)

## 结论和范围

在 Apple Silicon Mac（Darwin 25.4.0）使用 **VZ 后端、boot assets 0.8.10、重新编译的 daemon 和 Linux agent**，完整 `machine_isolation` 实机测试通过：1 passed，0 failed，25.27 秒。测试不使用用户的机器目录或代理凭据。

| 实机检查 | 结果 |
|---|---|
| 客体 root 尝试挂载 `users`、`private` | 均失败 |
| `arcbox` 共享目录 | 可执行专用 agent；看不到 daemon 数据目录和宿主哨兵文件；写入被拒绝 |
| 网络设备与身份 | 只有一张设备网卡；MAC 与宿主记录一致；`machine-id` 与持久化分配值一致 |
| 客体访问 Mac 网关上的真实监听服务 | 连接被拒，监听端未收到连接 |
| 正常出网 | 本地 SOCKS fixture 记录到目标 CONNECT，客体收到响应 |
| 关闭代理 | 真正关闭 SOCKS 监听端口；Mac 直连公网 HTTP 对照仍成功，客体收不到响应 |
| 恢复代理 | 更新同一代理记录后，运行中的机器恢复联网，无需重启 |
| TCP 转发 | 随机宿主端口映射到客体 8090，收到客体回显 |
| 动态删除、恢复和停机 | 删除后拒绝新连接，恢复后再次收到回显，停机后端口释放 |

这不是所有隔离场景的完整证明：本轮未跑 HV、UDP 实机转发、DNS 泄漏抓包、两台机器互访或 VNC 图形会话。时区缺少 tzdata 时原有测试允许跳过，不能据此宣称所有镜像的时区设置已验证。系统 Helper、DNS resolver 和容器路由问题不在本次修复范围；测试显式禁用 Helper 接入。

## 实机发现与修复

1. **e2e 在 runtime 外创建 Tokio 计时器。** 改为进入 runtime 后创建超时 future，并先通过 `WatchSetupStatus` 等待 daemon ready；增加不启动 VM 的超时回归测试。
2. **文件隔离让启动脚本找不到 agent。** 第一轮客体日志明确报 `chroot: can't execute '/arcbox/bin/arcbox-agent': No such file or directory`。引擎现在每次启动前复制 agent 到该机器独立的 `guest-share/bin/`，使用原子替换，不使用硬链接，整个内置共享只读。目录符号链接或 agent 缺失时拒绝启动。日志回退到串口。
3. **代理配置没有随测试数据目录隔离。** 原测试写入用户 XDG 配置，重跑出现同名代理冲突。现在使用临时 `XDG_CONFIG_HOME` 和不存在的私有 Helper socket。本次早期测试生成的代理 ID 和机器分配已精确移除；后续实机运行前后用户配置 SHA-256 相同。
4. **网卡计数误报。** Alpine 的 `dummy0`、`sit0` 不是额外的虚拟硬件连接；e2e 和桌面自检改为统计存在 `/sys/class/net/<name>/device` 的网卡，并要求恰好一张。
5. **宿主端口覆盖了客体目标端口。** TCP/UDP 入站路径过去使用 `host_port`。现在使用显式客体目标端口；Docker 的启动与恢复调用通过 `guest_relay_binding()` 传入客体已发布端口，再由 dockerd DNAT 到容器端口，保留原有语义。
6. **测试 fixture 的可移植性。** macOS accepted socket 继承非阻塞模式，SOCKS handler 现在显式切回阻塞模式并有握手回归测试。镜像没有 BusyBox httpd，改用已有 `nc` 的 TCP 回显服务，检查服务监听就绪后才测试转发。

## 复现

先停止其他 ArcBox daemon；不要与日常开发实例同时启动测试 VM。从 `arcbox/` 执行：

```sh
CARGO_BUILD_JOBS=2 cargo build --release -p arcbox-daemon
CARGO_BUILD_JOBS=2 cargo build -p arcbox-agent --target aarch64-unknown-linux-musl --release
SKIP_BUILD=1 KEEP_TEST_DIR=1 CARGO_BUILD_JOBS=2 \
  ARCBOX_E2E_PUBLIC_CONTROL_IP=153.3.238.127 \
  cargo test -p arcbox-e2e --test machine_isolation -- --ignored --nocapture --test-threads=1
```

`153.3.238.127:80` 是本次实测可直接访问的公网对照；默认 `1.1.1.1:80` 在本机超时。换网络时需选择可直接返回 HTTP 的 IPv4 地址。代理断开前后均验证对照可达，对照失败会令测试失败，不能当成隔离成功。

## 自动化检查和证据

- 引擎 machine 测试：68 passed。
- 入站转发测试：15 passed；包含宿主端口与客体 UDP 目标端口不同的回归。
- Docker 端口绑定：5 passed；覆盖 TCP/UDP 的客体发布端口映射。
- e2e 框架：25 passed；入口超时与 SOCKS fixture 回归：2 passed。
- Mac `MachineIsolationTests`：7 passed，Swift 编译通过。
- 相关 Rust crates 的 `Clippy -D warnings` 通过。

本机原始记录保存在仓库根的 `.local/isolation-e2e/`：`guest-port-fixed.metrics.json` 标记 `backend=vz, passed=true`；`logs/arcbox-isolation-live-port-fixed.log` 包含逐项 PASS；`runs/` 保存失败和成功运行的 daemon 日志。临时 VM 磁盘已在归档日志后清理。
