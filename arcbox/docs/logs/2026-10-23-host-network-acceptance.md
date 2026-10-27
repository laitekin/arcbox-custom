# 2026-10-23 — 私有 Helper 启动后系统 DNS 与容器路由验收通过

- Type: investigation / acceptance
- Area: development Helper、macOS DNS、container routing、desktop startup
- Commits / PRs: 未提交；沿用本地已编译的修复版本
- Related: [上一轮验收记录](2026-10-23-final-acceptance.md)

本记录更新上一轮的两个待确认状态：系统 DNS/容器路由已通过真实宿主访问，
钥匙串等待已解除，新版桌面应用正常进入 Containers 页面。
**外部 DNS 泄漏抓包仍未执行**，继续遵守用户此前的选择。

## 授权与环境

用户在自己的终端执行 `scripts/start-development-helper.sh`，截图显示
2026-10-23 13:03:48（Asia/Shanghai）私有 Helper 已监听。socket 为
`~/.arcbox-dev/run/private-helper/helper.sock`，授权当前用户及当前打包的
daemon/CLI CDHash。未替换正式 Helper，未运行管理员抓包，未读取密码。

## Evidence：实际结果

源类型均为本机命令日志，观察日期为 2026-10-23。证据保存在仓库根目录
`.local/final-acceptance/`，完整哈希也写入该目录 `sha256.json`。

| ID | source_ref | 观察 | SHA-256 |
|---|---|---|---|
| E-007 | `after-helper-doctor.log` | `10 passed, 0 failed`，包含 DNS resolver、Container route、ArcBoxHelper | `8243e2ef953819e8f7caa3587fa6ae815f0f6e117b68ee2a2bff0678f3ffec37` |
| E-008 | `host-network-live.log` | localhost 转发、容器 IP 直连、系统 getaddrinfo、域名 HTTP、daemon DNS 全部通过 | `2ab18e5dee6f3ced574d74eefb4361180d5ebf958d0ada2ff7650c1f2ed5ef29` |

E-008 使用本机已有 `node:22-bookworm-slim`，在临时容器启动返回随机 token 的
HTTP server。只发布 `127.0.0.1` 的动态端口，不修改 Docker context。具体结果：

- 容器 `arcbox-host-accept-c9133915`：`172.17.0.2:8080`。
- 宿主映射 `127.0.0.1:32769` 返回正确 token。
- 宿主直接访问 `172.17.0.2:8080` 返回同一 token，路由接口为 `bridge100`。
- macOS `socket.getaddrinfo` 将 `arcbox-host-accept-c9133915.arcbox-dev.local`
  解析为 `172.17.0.2`；按该域名访问 HTTP 也返回正确 token。
- `dig @127.0.0.1 -p 5554` 返回相同容器 IP。
- `/etc/resolver/arcbox-dev.local` 包含 Helper 所有权标记及 `127.0.0.1:5554`。
- 测试容器已经删除，随后再次 doctor 仍为 10/10 通过。

首轮夹具尝试本地 Alpine 中未提供的 `httpd` applet，未计为网络失败；已清理
并使用本地 Node HTTP server 重新执行完整断言。没有下载新镜像或安装软件。

## Finding 与 Path

F-005（validated，E-007、E-008）：开发 Helper 的授权修复已经经过真实系统
DNS 配置、路由及应用访问验证，不再仅限验签和文件操作沙箱。

P-004：用户以 sudo 启动显式 CDHash 授权的私有 Helper → daemon 连接该 socket
并完成宿主网络配置 → Docker 启动临时服务 → macOS 系统解析取得容器地址 →
`bridge100` 路由到容器 → HTTP 返回 token（E-007、E-008）。

桌面 AX 状态已显示正常 Containers 页面、`0 running`，New container 按钮可用。
不再停在 Boot assets ready。验证结束时 GUI、daemon、Helper 单次 CPU 采样均为
0.0%；这只是现场快照，不是长期空闲性能基准。用户 config.toml SHA-256 仍为
`24ae5207f29da27921bc09149cebe097d06631179d303f50abbc3e31c18947dc`。

## 复核命令与剩余事项

从仓库根目录复核当前应用：

```sh
ARCBOX_HELPER_SOCKET="$HOME/.arcbox-dev/run/private-helper/helper.sock" \
  arcbox-desktop/.build/DerivedData-Runnable/Build/Products/Debug/ArcBox.app/Contents/MacOS/bin/abctl \
  --profile development doctor
cat /etc/resolver/arcbox-dev.local
```

要复现 E-008，使用临时 Node HTTP 容器、随机返回值、loopback 动态发布端口，
分别通过 localhost、inspect 返回的 IP 和 `<container>.arcbox-dev.local` 请求并
比较响应；使用系统 `getaddrinfo` 验证解析，不能只调用 dig；最后删除测试容器。

私有 Helper 当前在用户终端前台运行，终端需保持打开。重编译 daemon/CLI 后
CDHash 变化，需要重新启动脚本授权新构建。这是开发启动方式，不是已安装的
永久后台服务。

仅剩外部 DNS 抓包验收未完成；代理路径、断代理阻断功能测试已通过，但不能据此
声称外部线路绝无重复 DNS 查询。后续用户恢复抓包授权时，按
[原抓包步骤](2026-10-23-hv-udp-dns-acceptance.md#补齐-dns-抓包)执行。
