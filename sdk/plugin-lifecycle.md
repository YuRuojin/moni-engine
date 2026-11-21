# 静态插件宿主与生命周期

中文 | [English](plugin-lifecycle_en.md)

`crates/moni-core/src/host.rs` 提供单线程参考调度器与服务注册表，`crates/moni-core/src/plugins.rs` 提供 `core.simulation`，`plugins/rendering/wgpu/plugin.rs` 提供 `core.render`。此阶段没有动态库 ABI、跨线程任务执行器或后台无界队列。

## 系统描述

每个 `SystemDescriptor` 声明：

- `id` 与 `plugin_id`
- `stage`：`startup`、`fixed_update`、`render`、`shutdown`
- `affinity`：`Logic` 或 `Render`
- `after`：允许同阶段或跨阶段的系统依赖
- `access`：对 `World`、`Scripts`、`Gpu`、`Assets` 的 `Read` 或 `Write`

依赖必须存在且无环。同一资源的读写冲突必须有显式依赖顺序；依赖可以跨阶段，但当前阶段只执行属于该阶段的系统，不重复执行启动工作。

访问声明在运行时由宿主强制检查：`PluginContext` 不再暴露整个 `&mut Engine`，`world()`、`world_mut()`、`run_ticks()` 和 `shutdown_scripts()` 分别要求对应的声明访问，未声明的访问直接返回 `PluginError`。`start`/`stop` 属于受信任的生命周期调用，拥有完整访问面。原生静态插件仍是受信任代码；权限与访问声明用于表达意图、审计和依赖检查，不是沙箱。

## 两相注册与回滚

插件生命周期分为两相。注册相（`PluginHost::register_plugin`）：校验插件 ID → 授予权限 → 调用 `Plugin::register`。插件通过 `PluginRegistry` 贡献组件 schema、服务、控制命令和迁移器；这些贡献不依赖已加载的世界。宿主按插件记录每笔贡献，注册中途失败会撤销该插件已记录的全部贡献与权限，注册按插件全有或全无。注册后的插件贡献即刻生效（场景因此可以在启动前按有效注册表加载并校验），但其系统不参与调度，shutdown 也不会调用它的 `stop`。

启动相（`PluginHost::start_plugin`）：校验并加入系统声明 → 对引擎做快照 → 以完整访问面调用 `Plugin::start`。启动失败时先调用 `stop` 清理，然后恢复快照：世界内容、事件流和 revision 回到启动前状态，启动期间提供的服务和注册的命令、注册相贡献的组件/服务/命令/迁移器一并移除，再撤销权限并移除系统声明。插件实例保留在宿主中等待注销；重试必须先注销再重新注册（重新运行 `register`）。

`register_plugin_with_permissions` 是文档化的组合路径：注册 → 启动，任一失败都把宿主恢复到调用前状态（注册期间加入的内容按反序注销），简单调用方保持单次调用语义。

## 服务注册表与插件命令

插件可以在注册相通过 `PluginRegistry::register_service`，或在 `start` 中通过 `PluginContext::provide_service` 发布按名字寻址的强类型服务，消费者用 `PluginContext::service::<T>` 获取并自动登记依赖关系；消费者不依赖具体提供方。

- 注销提供方时，宿主先把依赖它的插件按注册反序级联停止，再移除提供方及其贡献，`unregister` 返回实际被移除的插件 ID 列表。
- 注销消费者会释放其依赖声明，之后提供方可直接移除。
- 服务重复注册、未知服务或类型不匹配都会明确失败。

插件还可以通过 `PluginRegistry::register_command`（注册相）或 `PluginContext::register_command`（启动相）向控制协议注册自有命令（含 JSON schema），宿主在插件注销或回滚时移除其命令；`PluginHost::execute_plugin_command` 是命令入口。

每个控制命令都携带统一的 `CommandDescriptor` 元数据（定义在 `host.rs`）：`owner`（内置命令为 `"core"`，插件命令由注册插件 ID 填充）、`mutates_world`、`supports_batch`（`false` ⇒ 在原子批处理内以 `invalid_batch_command` 拒绝）、`supports_dry_run`（`false` ⇒ 在 dry-run 批处理内以 `dry_run_unsupported` 拒绝）、`side_effects`（`file`/`gpu`/`script`/`network` 声明，不是沙箱）、`required_permission`、`args_schema` 和 `returns_schema`。三参数的 `register_command(name, schema, handler)` 使用保守默认：`mutates_world = true`、不支持批处理、不支持 dry-run、无副作用、无权限——插件命令默认保守，显式声明能力；需要完整元数据时使用 `register_command_with(descriptor, name, handler)`（注册相与启动相均提供）。与内置命令同名的插件命令在注册时被明确拒绝，内置命令不可被插件遮蔽。

`required_permission` 在执行时对照 OWNER 插件被授予的权限检查（与 `PluginContext::require` 相同语义），不满足时返回代码为 `permission_denied` 的错误。命令解析按名字先行：已注册的插件命令总是执行并原样返回其结构化错误，失败绝不回退到内置分发而被掩盖成 `unknown_command`。批处理与 dry-run 的准入同样查询描述符（内置命令来自静态表，插件命令来自其注册的描述符）；声明支持 dry-run 的插件命令在 dry-run 中对草稿世界执行，处理器只拿到草稿引擎，宿主管理的运行时状态（脚本、服务）不参与预览。

capabilities 的 `commands` 一节由描述符生成：内置命令来自静态表，加上会话中的插件命令；每个条目携带 `args`、`returns`（有文档时）、`mutatesWorld`、`batch`、`dryRun`、`sideEffects`、`permission`（设置时）和 `owner`。`session.pluginCommands` 使用同一形状。

注册相还可以通过 `PluginRegistry::register_migrator` 为动态组件登记版本迁移步骤（组件必须先由同一 `register` 调用注册）；宿主在插件注销或回滚时移除其迁移器。

## 阶段执行与关闭

单线程调度器按拓扑顺序执行系统；只有已启动插件拥有系统。校验与排序结果按阶段缓存，注册、启动或注销使缓存失效，热阶段不再每帧重新验证。`shutdown` 阶段反向执行，随后按插件注册的反序对已启动插件调用 `stop`——注册未启动的插件直接丢弃，不接收生命周期调用——最后清空贡献记录、服务、命令和插件注册表。`PluginHost::drop` 也执行同一关闭路径。

`core.simulation` 在逻辑阶段调用固定逻辑帧，并在停止时清理 QuickJS 宿主；`core.render` 注册时无需窗口，窗口就绪后经 `set_window` 附加并在启动相创建/使用 GPU 资源，在停止时释放它们。客户端通过真实注销路径卸载模拟插件。

## 会话与控制面

组合根的组装顺序固定为：解析 profile → 构造插件并完成注册相 → 使用有效注册表加载并校验场景 → 启动运行时系统（窗口化的 `core.render` 等）→ 进入共享会话。`register_profile` 只执行注册相；`start_all` 按序启动，`instantiate` 是带完整回滚的注册+启动组合路径。

`crates/moni-core/src/session.rs` 的 `RuntimeSession` 是客户端、编辑器和 NDJSON 服务共用的运行时路径：统一持有插件宿主、世界和 profile，所有命令经同一分发，响应携带 revision 与事件序列号；`RuntimeSession::load_scene` 按有效注册表校验动态组件并应用迁移链。批处理与单条命令的 `expectedRevision` 乐观并发、原子批处理回滚、`dryRun` 预览和幂等请求 ID 的语义见协议 capabilities 的 `transactions` 一节；单条命令的 `expectedRevision` 不匹配在任何执行前返回 `revision_conflict`，不产生副作用、revision 变化或事件，且该响应像其他响应一样参与幂等缓存。

## 有界任务与错误传播

单个阶段的系统数量上限为 1024。执行失败会中止该阶段并返回 `PluginError`，不会在卸载后继续运行残留工作。客户端固定追帧仍限制为每次最多 5 个逻辑帧，脚本回调也有独立限制。

后续并行任务必须沿用相同的依赖、访问、线程亲和性和取消规则，不能绕过调度器直接操作共享世界或 GPU。
