# 静态插件宿主与生命周期

中文 | [English](plugin-lifecycle_en.md)

`src/host.rs` 提供单线程参考调度器，`src/plugins.rs` 提供当前静态插件。此阶段没有动态库 ABI、跨线程任务执行器或后台无界队列。

## 系统描述

每个 `SystemDescriptor` 声明：

- `id` 与 `plugin_id`
- `stage`：`startup`、`fixed_update`、`render`、`shutdown`
- `affinity`：`Logic` 或 `Render`
- `after`：允许同阶段或跨阶段的系统依赖
- `access`：对 `World`、`Scripts`、`Gpu`、`Assets` 的 `Read` 或 `Write`

依赖必须存在且无环。同一资源的读写冲突必须有显式依赖顺序；依赖可以跨阶段，但当前阶段只执行属于该阶段的系统，不重复执行启动工作。

## 注册与回滚

`PluginHost::register_plugin` 先验证插件和系统，再调用 `Plugin::start` 并返回 `PluginHandle`。启动失败时移除刚注册的系统并调用 `stop` 清理部分初始化，不保留半完成插件。

`PluginHandle::dispose` 或 `PluginHost::unregister` 会移除系统、取消排队工作并调用 `stop`。重复插件 ID、重复系统 ID、未知依赖、非法线程亲和性和无序资源冲突都会明确失败。

## 阶段执行与关闭

单线程调度器按拓扑顺序执行系统。`shutdown` 阶段反向执行，随后按插件注册的反序调用 `stop`，最后清空队列与注册表。`PluginHost::drop` 也执行同一关闭路径。

`core.simulation` 在逻辑阶段调用固定逻辑帧，并在停止时清理 QuickJS 宿主；`core.render` 在启动和渲染阶段创建/使用 GPU 资源，并在停止时释放它们。客户端通过真实 `PluginHandle` 卸载模拟插件。

## 有界任务与错误传播

每个阶段的同步任务队列上限为 1024。执行失败会清空未完成任务并返回 `PluginError`，不会在卸载后继续运行残留工作。客户端固定追帧仍限制为每次最多 5 个逻辑帧，脚本回调也有独立限制。

后续并行任务必须沿用相同的依赖、访问、线程亲和性和取消规则，不能绕过调度器直接操作共享世界或 GPU。
