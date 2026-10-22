Status: Active

# 代理库与机器出站代理

目标：在 Mac 管理多套 SOCKS5 代理，让 Linux 机器各自选择出站代理；虚拟机不保存代理密码，也不需要安装代理软件。

## 界面与流程

| 入口 | 操作 | 生效时机 |
| --- | --- | --- |
| Settings → Network → Import SOCKS URL | 粘贴 `socks5://` 或 `socks5h://` URL，自动拆分地址、端口、账号、密码 | 仅修改编辑草稿 |
| 导入/编辑弹窗 → Test without saving | 测试连接、认证、目标隧道和 UDP 能力 | 不保存、不切换当前代理 |
| 导入/编辑弹窗 → Save | 保存名称、连接信息、DNS、UDP、ICMP 和绕过规则 | 已使用该代理的机器在新连接上生效 |
| Settings → Network → Default proxy | 选择跟随 macOS、直连、手工配置或某个已保存代理，再 Apply | Docker、宿主机下载、选择“跟随默认”的机器使用该默认值 |
| New Machine → Network | 选择 Follow default、Direct 或已保存代理 | 创建后先绑定代理，再启动；绑定失败则保留停止状态并显示错误 |
| Machines → 机器 → Info → Outbound proxy | 为现有机器选择代理，再 Apply | 运行中也可切换，新连接立即生效；现有连接保留原路由 |
| Saved SOCKS proxies | 测试、编辑、查看默认/机器使用情况、删除 | 仍被使用的代理禁止删除，需先修改使用方的选择 |

导入支持用户名密码的百分号转义、IPv6、默认端口 1080；不支持的协议、非法端口、错误转义会就地报错。
URL 输入框隐藏原始内容；导入后清空原 URL，密码只显示在密码框，列表和返回的 URL 均不含密码。
`socks5h` 开启远程 DNS；普通 `socks5` 保留本机 DNS。测试 UDP 不支持时会显示单独失败步骤，仍可选择阻止普通 UDP 后使用 TCP。

## 数据与规则

- 代理有不可变 UUID 和可编辑名称；名称不区分大小写去重，最多 128 套。
- `config.toml` 的 `[network.proxy_library]` 保存脱敏 URL、流量策略、默认代理 ID 和机器引用。
- 密码使用现有 dev/prod 隔离的凭据存储，按 `profile:<UUID>` 保存。同地址、同用户名的两套代理仍可保存不同密码。
- 重命名或切换 DNS 模式复用密码；修改账号或服务器需要明确提供新密码，避免向新地址发送旧密码。
- 写凭据后原子保存配置；配置写入失败恢复旧凭据，运行中的路由不变。
- 删除会检查默认引用和机器引用；成功后删除该代理的凭据。删除机器会释放它的代理引用。
- 没有机器引用表示继承默认，`direct` 表示显式直连，其余值为代理 UUID。System VM 不接受单独分配。
- 从磁盘恢复配置时保留每台机器的选择；凭据不可用时代理认证失败，不静默改成直连。

## 实现

`NetworkProxy` 管理代理库、凭据和配置事务；`ProxyRoutes` 为每台机器持有独立的 `ProxyState`。
`MachineManager` 在启动 VM 前按机器名称选择该状态，HV/VZ 都沿用现有宿主机数据通路。
新建连接、网关 DNS 查询读取机器自己的快照；发布默认配置只更新继承它的机器，发布某个代理只更新引用它的机器。
数据通路仍通过 ArcSwap 读取，不在网络热路径访问配置文件或钥匙串。

主 API 是 Unix socket 上的 `ProxyLibraryService`：ListProfiles、SaveProfile、RemoveProfile、GetMachineProxy、SetMachineProxy。
`EgressProxyService.SetProxy` 支持按 `profile_id` 选择默认代理，TestProxy 支持按代理 ID 读取凭据测试编辑草稿。
所有返回结构只包含 `has_password`；不返回密码。新增字段和服务均为协议的向后兼容扩展。

## 验证

- Rust：代理库、配置、API、daemon 与 Fake-IP 相关测试共 313 项通过，1 项环境相关测试保持忽略。
- Swift：URL 导入、脱敏、默认端口、错误输入、菜单栏代理引用恢复，共 15 项通过。
- [真实机器验证脚本](../../tests/bench/proxy-library/smoke.py)：两台临时 Alpine 分别走 A/B、运行中切换、编辑代理热更新、使用中禁止删除；退出时清理自己的机器与代理并恢复默认配置。
- 设计关联：[SOCKS5h 网关 DNS](../logs/2026-10-22-orbstack-socks-dns.md)、仓库 Issue #2。

代理服务器连接仍在 Mac 发起；helper 的本地域名与直连路由安装权限沿用原实现。
