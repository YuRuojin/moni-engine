# 插件权限、完整性与隔离

中文 | [English](security_en.md)

`crates/moni-core/src/host.rs` 的 `PermissionPolicy` 按插件 ID 保存清单授予的权限。插件在启动和执行阶段通过 `PluginContext` 检查权限，注销时撤销，关闭时清空。访问声明同样由宿主在运行时强制检查。原生静态插件是受信任代码：权限与访问声明服务于意图表达、审计和依赖检查，不是抵御恶意或有缺陷插件的沙箱；不可信导入器、脚本或工具需要进程或运行时级隔离。

## 权限边界

当前使用的权限：

- `core.simulation`：`script.execute`。
- `core.render`：`gpu.create`。
- `core.fbx`：`asset.read.workspace`。

FBX 权限属于资源提供方，渲染通过 `require_from("core.fbx", "asset.read.workspace")` 检查，不把资源权限并入渲染插件。没有授权时插件明确失败，不会回退执行。

可用 `client --revoke core.render=gpu.create` 或撤销 FBX 权限验证拒绝路径。该功能用于兼容性验证，不是远程权限管理协议。

## 更新差异与接受

`moni doctor` 比较 `moni.lock` 与当前 profile 的权限集合。`doctor --update-lock` 输出新增和移除权限；存在新增权限时必须显式添加 `--accept-permissions`，否则拒绝更新。

`moni run` 不自动接受权限变化，锁文件与 profile 不一致时拒绝启动。

## 完整性与回滚

`moni.lock` 为每个插件清单记录 SHA-256。`doctor` 比较完整锁文件，因此清单、版本、权限或完整性字段被修改都会被发现。

更新前写入 `moni.lock.backup`。`doctor --rollback-lock` 恢复备份并重新校验当前 profile。SHA-256 是完整性哈希，不是发布者签名；当前没有密钥签名、证书链或注册表信任机制。

## 导入进程隔离

`cook --isolate` 使用独立子进程执行 FBX 解析和 cooked 写入。父进程限制等待为 10 秒；超时或崩溃只终止子进程，不影响宿主世界、GPU 或脚本宿主。

这提供崩溃隔离，不是完整安全沙箱：子进程仍拥有当前用户的文件和系统权限。真正的文件系统/网络沙箱与凭据隔离仍需平台插件实现。

## 脚本限制

QuickJS 已有 50 ms 回调时间、32 MiB 内存、256 KiB 栈、命令数量、日志数量和大小限制。脚本上下文固定在逻辑线程，错误后宿主被禁用并清理。
