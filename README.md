# ArcBox Custom

在 Apple Silicon Mac 上二次开发 ArcBox：宿主机 SOCKS5 认证代理、原生网络设置与覆盖诊断、自定义 Linux 机器镜像。

## 项目结构

- `arcbox/`：Rust runtime、网络层、CLI、daemon、Linux agents。
- `arcbox-desktop/`：SwiftUI macOS 桌面应用。
- 两个目录通过 Git subtree 导入，保留各自的上游提交历史和许可证。

## 上游基线

| 目录 | 上游 | 导入提交 |
| --- | --- | --- |
| arcbox | https://github.com/arcboxlabs/arcbox | `f55e2c2a07f6ec8309121263e75169723a32e346`（v0.10.4） |
| arcbox-desktop | https://github.com/arcboxlabs/arcbox-desktop | `bb4cd9e686a9e16776a731385dec4a0006fea3ef` |

上游文件中的 MIT / Apache-2.0 许可证和版权声明保留在对应目录。

## 开发进度

功能验收以 Issue 中的测试证据为准；规划项不代表已实现。

- [本机可运行构建与源码导入](https://github.com/laitekin/arcbox-custom/issues/1)
- [认证代理、设置与诊断](https://github.com/laitekin/arcbox-custom/issues/2)
- [Linux 镜像库、导入与迁移](https://github.com/laitekin/arcbox-custom/issues/3)

当前状态（分支 `claude/exciting-brown-jx9f1y`）：

| 模块 | 状态 |
| --- | --- |
| F2 认证代理（守护进程、CLI、热更新、钥匙串/文件凭据、跟随系统的 SCDynamicStore 通知） | 已实现，Linux 上单元/集成测试通过 |
| F2 桌面「设置 › 网络」、菜单栏「代理：开/关」 | 已实现，Linux 上做了编译与逻辑测试，未在 Mac 上运行 |
| F3 代理测试与诊断、守护进程下载走代理 | 已实现并测试 |
| F4 镜像库、导入（归档 / squashfs / Docker，含自动安装 init）、`create --image`、导出携带镜像 | 已实现并测试；磁盘镜像（qcow2 等）明确拒绝 |
| e2e：代理覆盖（容器/UDP/pull/机器/Pod）、镜像导入全流程 | 已编写，需在 Mac e2e 环境运行 |
| R6 网关 DNS | 自定义 SOCKS5 可开启远程 DNS（`socks5h`），认证失败不回退直连；[行为与验证](arcbox/docs/logs/2026-10-22-orbstack-socks-dns.md) |
| 机器详情页：资源编辑与剪贴板 | 主窗口「机器 › Info」可改 CPU/内存上限（重启生效，带「立即重启」）、显示客体内存占用与负载；Mac 与机器 X 桌面（VNC 等）之间收发剪贴板文本（需 xclip 或 xsel）。剪贴板脚本已在 Xvfb 上验证，界面未在 Mac 上运行 |
| 主窗口「代理」页 | 侧边栏 System › Proxies：默认代理一键切换、已保存代理（测试结果摘要、设为默认、编辑/复制/删除）、各机器代理分配，选择即生效；机器 Info 页代理选择即生效并可测试；导入时粘贴 URL 自动填充。逻辑在 Linux 上测试，界面未在 Mac 上运行 |
| 隔离机器（严格隔离） | 不共享 Mac 文件；无桥接网卡，主网卡失败也不退回 Apple NAT；只能经已保存代理出网（TCP、UDP、DNS 都走代理，失败即断，禁止 ping），禁止访问 Mac、局域网和其他机器；独立 MAC、machine-id、时区和语言；Mac 通过 `127.0.0.1` 端口转发访问（VNC）；Info 页可做隔离自检。CLI：`machine create --isolated --proxy`、`isolate`、`proxy`、`forward`、`identity`；[方案与实施记录](arcbox/docs/plans/2026-10-22-isolated-machines.md)。Linux 上测试与 macOS 目标类型检查通过，未在 Mac 上运行 |

逐项清单见 [`TODO-PRD.md`](TODO-PRD.md)。

## 构建检查

依赖包括完整 Xcode、Rust、Node.js、protobuf、xcodegen、aarch64 Linux musl 交叉编译工具链。

从仓库根目录编译并启动当前本地源码：

```sh
make run
```

`make run` 默认只为本机开发使用 ad-hoc 签名，明确启用本地 runtime 模式以接受本仓库的二开修改。原有固定上游 tag、干净 worktree 的构建模式仍保留在桌面构建脚本中。

### 快速开发

在仓库根目录执行：

| 命令 | 用途 |
| --- | --- |
| `make run-fast` | 编译并启动：Swift Debug + Rust 宿主端 `dev-fast` |
| `make run-ui` | 仅重新编译并启动 Swift 界面，复用 App 中已有后端和资源 |
| `make build-fast` / `make build-ui` | 对应模式只构建，不启动 App |
| `make run` | 保留原流程：Swift Debug + Rust Release + 资源准备 |

先运行一次 `make run-fast`（或原来的 `make run`），之后只改 Swift 界面用
`make run-ui`。修改 Rust、protobuf、启动资源或打包配置后，重新用
`make run-fast`；protobuf 仍需执行下文的生成步骤。仅界面模式不会更新这些内容，
也不会自动下载缺失资源；已有 App 不完整时会报错并提示完整构建。

`dev-fast` 使用增量编译、`opt-level=1`、关闭 LTO、16 个 codegen units，
第三方依赖使用 `opt-level=2`；保留 Release 的 panic/断言语义和行号调试信息。
根目录入口把宿主产物放在 `arcbox/target/dev-fast`，与 `target/release` 分开缓存。
Linux guest agents 仍使用原来的 Release 交叉编译，未变化时由 Cargo 复用缓存。
首次快速构建需要建立新缓存；不要在日常修改后执行 `clean`。

两种模式共用 `.build/DerivedData-Runnable`。仅界面模式跳过后端编译、资源复制和
资源重新签名，保留启动配置修复、已有资源校验及 Xcode 的 App 签名。
正式 Release profile、DMG 打包和发布入口保持不变。

需要调试或启动健康检查时，可组合脚本选项：

```sh
cd arcbox-desktop
ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY=- ./script/build_and_run.sh --fast --verify
SIGN_IDENTITY=- ./script/build_and_run.sh --ui-only --debug
```

```sh
cd arcbox-desktop
make test-xtask lint-xtask
```

本机的 `Local.xcconfig`、证书、钥匙串凭据、构建产物与运行数据不进入版本库。

修改 `arcbox/rpc/arcbox-protocol/proto` 后，需要同步更新两处生成代码：

```sh
make -C arcbox refresh-connect-descriptor        # Rust Connect 描述符
make -C arcbox-desktop generate-arcbox-protobuf ARCBOX_DIR=../arcbox   # Swift 客户端
```

`cargo xtask protocol verify` 会按 `arcbox.version` 拉取上游 tag 的 proto，因此在本二开仓库中不适用；以上述本地生成结果为准。

## 二开功能速览

- 网页桌面：机器 Info › Forwarded ports › **Open in Browser**，内置 noVNC；支持 Mac ↔ X11 文字/PNG 剪贴板、⌘C/⌘V、终端快捷键模式和 Mac 拼音组合输入。客体需安装 xclip，支持范围与验证方法见 [网页桌面说明](arcbox-desktop/WebDesktop/README.md)。

- 代理库：设置 › 网络 › Import SOCKS URL，粘贴带可选账号密码的 URL，测试后保存；支持多套代理的编辑、测试和删除。默认代理可选择已保存的代理。
- 每台机器独立代理：新建机器的 Network 或已有机器 Info › Outbound proxy 中选择 Follow default、Direct 或某个已保存代理。运行中可切换，修改代理也会更新引用它的机器；[完整设计与验证](arcbox/docs/architecture/proxy-library-machines.md)。
- SOCKS5 远程 DNS：设置 › 网络 › Custom proxy › SOCKS5，勾选 “Resolve DNS through the proxy”；或 `abctl network proxy set socks5h://host:1080`。网关 DNS 通过宿主机 SOCKS5 TCP 隧道发送，本地域名保留本地解析。绕过规则支持域名、IP、CIDR 和 `*`。

- 认证代理：`abctl network proxy set socks5://user@host:1080 --password-stdin`，`abctl network proxy test`；桌面版「设置 › 网络」。密码保存在登录钥匙串（或 `ARCBOX_PROXY_CREDENTIALS=file` 时的 0600 文件），`config.toml` 不含密码，修改后对新连接立即生效；菜单栏可一键切换代理开/关。
- 自定义镜像：`abctl machine image import ./rootfs.tar.gz --name my-linux`（或 `docker:ubuntu:24.04 --add-init`，由守护进程构建），`abctl machine create dev --image my-linux`；桌面版「机器 › 镜像」与新建机器中的「My Images」。
- 详细说明与验证记录：`arcbox/docs/logs/2026-10-21-proxy-authentication.md`、`arcbox/docs/logs/2026-10-21-machine-image-import.md`。

## 当前开发环境限制

开发版可以使用 ad-hoc 签名与 `arcbox.dev.entitlements`。官方 release helper 只接受官方团队签名，本机开发需显式启用私有手动 helper；其父目录必须属于当前用户且权限为 0700。helper、DNS 和路由的健康检查分别报告，不能用容器可启动代替这些检查。
ArcBox 二次开发：宿主机 SOCKS5 认证代理、网络设置与诊断、自定义 Linux 镜像。
