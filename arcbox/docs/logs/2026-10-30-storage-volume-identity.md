# 2026-10-30 — macOS 设备编号变化不再误判 Docker 磁盘被替换

- Type: incident
- Area: `arcbox-storage`, System VM storage admission
- Commits / PRs: none
- Related: [配对磁盘恢复](2026-10-20-storage-recovery-moved-pair.md)

## Problem or trigger

开发实例在 10 月 29 日 22:50 启动时报告 `docker.img was replaced; restore the paired images`，进入持久存储保护。Docker API 无法启动 System VM，桌面容器列表显示加载失败。

证据来自开发实例的存储 manifest、文件 stat、两块镜像的文件系统超级块和 daemon 日志。原记录的宿主设备编号为 `16777234`，当前为 `16777231`；数据盘 inode `22322083`、元数据盘 inode `22322084` 均未变化，两块镜像的文件系统 UUID 也与 manifest 一致。

发现：原来的身份比较要求 `st_dev` 和 inode 同时相等，把挂载期间分配的设备编号作为了持久身份。此次保护由设备编号变化触发；这些身份读数本身不能证明文件系统完整性。

处理路径：先执行正常的保留副本、离线检查和读写恢复，再将宿主身份改为持久卷 UUID 加 inode，保留真正替换镜像时的保护。

## What was done

- 通过开发实例的 `abctl disk recover` 恢复，保留两块磁盘和原 manifest，没有清空 Docker 数据或手动删除保护标记。
- macOS 使用 `fgetattrlist(ATTR_VOL_UUID)` 读取宿主卷 UUID。相同卷 UUID 和 inode 可以跨设备编号变化通过检查；卷或 inode 不匹配仍拒绝。
- 老 manifest 没有宿主卷 UUID 时，继续要求设备编号和 inode 一致。只有两块磁盘身份及文件系统状态全部验证通过后，才原子保存新身份。旧记录已经不匹配时仍必须执行显式恢复。
- 保持 JSON 旧字段可读；更新后的 Linux guest 保留新增身份字段。部署必须同时更新宿主和 guest agent，旧 guest 会拒绝新增字段。

## Evidence

- `cargo test -p arcbox-storage`：19 个测试通过，覆盖设备编号变化、不同卷、不同 inode、旧记录升级、旧记录不匹配拒绝、缺失成员、格式化权限和复制恢复。
- `cargo clippy -p arcbox-storage --all-targets -- -D warnings` 通过。
- `cargo fmt --check` 通过。
- 本地实际恢复报告确认两块磁盘的写入、fsync、读回、删除和目录 fsync 通过；Docker 镜像导入、容器创建/启动/写入/读取/等待/移除及测试镜像移除通过。
- 恢复状态为 `Complete`、`storage_protected=false`，Docker `/_ping` 返回 `OK`，刷新桌面后容器列表正常显示。
- `make build-fast` 完整构建成功，宿主和 Linux guest agent 一并部署到现有开发应用；应用通过 `codesign --verify --deep --strict`。
- 实际启动验证旧 manifest 自动补充宿主卷 UUID。随后正常退出实例，备份 manifest，只将两块镜像记录中的设备编号异或 `1`，保持 inode、卷 UUID、文件系统 UUID 和磁盘内容不变。再次启动后 `GetSetupStatus` 返回 `READY`、`vmRunning=true`，两块磁盘均为 `MOUNTED_READ_WRITE`；设备编号观察值自动更新，未生成保护标记，也未再次执行恢复。这是设备编号变化的模拟验证，没有重启宿主 macOS。
- 修复版实例执行 `docker --host unix://$HOME/.arcbox-dev/run/docker.sock run --rm --network none --pull never alpine:latest sh -ec 'printf storage-ok > /tmp/storage-probe; sync; test "$(cat /tmp/storage-probe)" = storage-ok; echo storage-ok'`，退出码为 0，输出 `storage-ok`。测试容器自动移除，原有 3 个镜像保留。
- 最终界面确认容器列表正常，没有存储保护横幅和加载错误；修复后截图与旧应用保存在本地 `.build` 回滚目录。

## Follow-ups

`abctl doctor` 的 daemon、Docker API/CLI 和 shell 检查通过；系统 DNS resolver、容器宿主路由和管理员 Helper RPC 仍失败，与此次存储身份问题独立。当前会话无法无密码使用 `sudo`，没有更改管理员 Helper 或系统 DNS/路由配置。
