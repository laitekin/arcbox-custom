# PRD 剩余工作清单

由定时任务逐项推进；每完成一项就勾选并随同代码一起提交、推送到
`claude/exciting-brown-jx9f1y`。全部勾选后停用定时任务。

## 环境准备（每次运行先做）

- `git fetch origin && git merge --ff-only origin/claude/exciting-brown-jx9f1y`（若 main 有新提交，合并 main，不要 rebase 已推送历史）。
- Rust 构建用 `CARGO_INCREMENTAL=0`；磁盘配额有限，`df -h` 低于 8 GB 时删除 `arcbox/target`。
- 需要 protoc：`apt-get install -y protobuf-compiler squashfs-tools`。
- Swift（生成客户端 / 编译检查）：`/opt/swift` 不存在时下载
  `https://download.swift.org/swift-6.1.2-release/ubuntu2404/swift-6.1.2-RELEASE/swift-6.1.2-RELEASE-ubuntu24.04.tar.gz`
  解压到 `/opt/swift`；protoc 插件与 XcodeGen 2.45.4 按 `Packages/ArcBoxClient/generate.sh` 的步骤构建。
- 提交信息遵循 `arcbox/AGENTS.md`：`type(scope): summary`，不加 AI 署名行；提交前对改动 crate 跑 fmt、clippy（`-- --cap-lints warn` 过滤到改动文件）和测试。

## 待办

- [x] F4 界面「自动安装 init」：在 `ImportMachineImageRequest` 增加 `add_init`，守护进程经 Docker API `/build` 用与 CLI 相同的 Dockerfile 构建派生镜像后导入；CLI `--add-init` 改走该字段；桌面导入表单加复选框；测试用伪 Docker socket 覆盖。
- [x] F2 菜单栏快速切换：菜单栏增加「代理：开/关」项（关=direct，开=恢复上次的 custom/system 设置，记录在 `UserDefaults`）；view model 单元测试。
- [x] 桌面单元测试：`NetworkSettingsModel`（isDirty、needsPassword、validationError）与 `MachineImagesSheet.suggestedName` 放入 `ArcBoxTests`，Linux 上用 scratch 包至少编译通过。
- [ ] e2e：`tests/e2e/tests/proxy_coverage.rs` 增加 Linux 机器行（创建 alpine 机器，机器内 wget TEST-NET 目标）与 k3s Pod 行；`tests/e2e/tests/machine.rs` 增加镜像导入场景（导入 rootfs → create --image → start → exec → export 含镜像 → 删除镜像后 import）。
- [ ] R6 宿主机 DNS：阅读 `arcbox-dns`/`splicetcp` 代码，确认代理模式下宿主机侧是否发出真实 DNS 查询；结论写入 `docs/logs/2026-10-21-proxy-authentication.md` 覆盖矩阵；若会泄露，代理模式下改为仅返回 fake IP（带测试）。
- [ ] sandbox 覆盖：确认 `computer/` sandbox microVM 出站经过 System VM 数据通路，结论写入覆盖矩阵。
- [ ] F2.4 跟随系统：macOS 上用 `SCDynamicStore` 通知替代 30 秒轮询（保留轮询作为非 macOS 回退），`cfg(target_os = "macos")` 隔离。
- [ ] F4.8（P2，仅接口）：`ImportMachineImageRequest` 文档注明 qcow2/raw 暂不支持，导入时识别 qcow2 魔数并返回明确错误（带测试）。
- [ ] 收尾：README「开发进度」更新；在 issue #2、#3 评论实现与验证摘要（若 GitHub 工具可用）；询问用户是否开 PR。

## 需要 Mac 才能做（只记录，不由定时任务完成）

- 桌面 App 编译运行、`make test`。
- `cargo test -p arcbox-e2e --test proxy_coverage -- --ignored` 等 e2e。
- 导入镜像（systemd / openrc）实机启动、钥匙串授权行为、吞吐回归。
