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

## 构建检查

依赖包括完整 Xcode、Rust、Node.js、protobuf、xcodegen、aarch64 Linux musl 交叉编译工具链。

从仓库根目录编译并启动当前本地源码：

```sh
make run
```

`make run` 默认只为本机开发使用 ad-hoc 签名，明确启用本地 runtime 模式以接受本仓库的二开修改。原有固定上游 tag、干净 worktree 的构建模式仍保留在桌面构建脚本中。

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

- 认证代理：`abctl network proxy set socks5://user@host:1080 --password-stdin`，`abctl network proxy test`；桌面版「设置 › 网络」。密码保存在登录钥匙串（或 `ARCBOX_PROXY_CREDENTIALS=file` 时的 0600 文件），`config.toml` 不含密码，修改后对新连接立即生效。
- 自定义镜像：`abctl machine image import ./rootfs.tar.gz --name my-linux`（或 `docker:ubuntu:24.04 --add-init`），`abctl machine create dev --image my-linux`；桌面版「机器 › 镜像」与新建机器中的「My Images」。
- 详细说明与验证记录：`arcbox/docs/logs/2026-10-21-proxy-authentication.md`、`arcbox/docs/logs/2026-10-21-machine-image-import.md`。

## 当前开发环境限制

开发版可以使用 ad-hoc 签名与 `arcbox.dev.entitlements`。官方 release helper 只接受官方团队签名，本机开发需显式启用私有手动 helper；其父目录必须属于当前用户且权限为 0700。helper、DNS 和路由的健康检查分别报告，不能用容器可启动代替这些检查。
ArcBox 二次开发：宿主机 SOCKS5 认证代理、网络设置与诊断、自定义 Linux 镜像。
