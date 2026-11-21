# 组件与属性 Schema

中文 | [English](component-schema_en.md)

`crates/moni-core/src/components.rs` 定义 `ComponentRegistry`、`ComponentDescriptor`、`PropertySchema` 和 `PropertyField`；`Engine`（`crates/moni-core/src/engine.rs`）持有会话级组件注册表和每组件迁移链，并在构造时注册内置组件。动态（schema-only）组件可在运行时通过 Rust API 注册；插件加载器自动注册尚未实现。

## 组件描述

每个 `ComponentDescriptor` 包含：

- `id`：当前版本使用的命名空间 ID，例如 `core.model`。
- `legacy_id`：v1 使用的兼容 ID，例如 `model`。
- `owner`：组件所有者，内置组件均为 `moni.core`。
- `version`：组件 schema 版本。
- `schema`：字段定义、默认值、校验和编辑器信息。

描述符分两类：

- 内置组件携带到 `ComponentValue` 枚举的 `deserialize`/`serialize` 映射；该映射是私有字段，只有核心组件可构造。
- 动态组件由 `ComponentDescriptor::dynamic(...)` 构造，没有值映射；实例以 JSON 存储，完全通过 `PropertySchema` 校验（默认值 + 字段检查）。`is_dynamic()` 区分两类。

重复组件 ID 或 legacy ID 会被拒绝。消费者通过注册表解析组件，不直接依赖某个具体组件提供方。

## 动态组件

`Engine::register_component(descriptor)` 把描述符注册进会话注册表，之后立即可挂载，并发出 `component_registered` 事件。注册与迁移器是事务性会话状态：`Engine::snapshot()` 会捕获它们，`restore()` 回退注册与迁移链，因此回滚的批处理或失败的插件启动不会留下任何注册痕迹。动态组件数据本身随 `Entity.extensions` 进入快照。

动态组件实例存储在 `Entity.extensions` 的组件 ID 键下，协议命令：

- `attach_component`（参数 `entity`、`component`、`value`）：解析描述符 → 应用 schema 默认值 → schema 校验 → 存入 extensions → 发出 `component_attached` 事件。重复挂载替换旧值。
- `remove_component`（参数 `entity`、`component`）：从 extensions 移除并发出 `component_removed` 事件。

错误语义：未注册的组件 ID 返回 `unknown_component`；`core.*` 等内置 ID 存在于类型化实体字段，返回 `component_reserved`，不能用这两条命令挂载或移除；schema 校验失败返回 `invalid_component_value`，消息包含具体字段错误，失败时不写入 extensions、不发事件、不推进 revision。会话加载保留了某组件的未来版本实例时，写入该组件的 `attach_component` 被拒绝并返回 `component_future_version`，details 为 `{ component, sceneVersion, registryVersion, futureInstances }`；移除未来实例不受此限制（保留规则与只读语义见[场景格式](scene-format.md)的"会话加载"）。

`capabilities.components` 投影当前会话引擎的注册表，动态组件自动带上 `x-component`（`owner`/`version`）元数据，无需单独维护组件清单。

## 撤销注册

`Engine::unregister_component(id, force)`：

- 仍有实体携带该组件时拒绝（`component_in_use`），消息与错误详情报告实体数量和实体 ID。
- `force = true` 时先从所有实体剥离组件（每个实体发出 `component_removed`）再撤销注册。
- 内置组件始终拒绝撤销（`component_reserved`）。
- 撤销成功时同时移除该组件的迁移链，并发出 `component_unregistered`。

撤销后组件 ID 立即不可解析，`attach_component` 返回 `unknown_component`；可以随后重新注册同 ID，但不会恢复旧迁移链。

## 版本迁移

动态组件的 schema 版本变化通过迁移链衔接。`ComponentMigrator` 是单个迁移步骤的类型别名：`Box<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send>`，把某个版本的组件值转换成下一个版本的等价值，`String` 错误说明无法迁移的原因。

`Engine::register_component_migrator(component, from_version, to_version, migrate)` 注册一个步骤：

- 组件必须已注册且为动态组件；未注册返回 `unknown_component`，`core.*` 返回 `component_reserved`（core 组件由场景格式自身迁移）。
- `to_version` 必须大于 `from_version` 且不超过组件当前注册版本（`invalid_migration_step`）。
- 同一组件的同一 `from_version` 只能有一个步骤（`duplicate_migrator`）。
- 注册成功推进 revision，不发出事件。

`Engine::migrate_component_value(component, value, from, to)` 沿链迁移：

- `from == to` 原样返回；降级返回 `invalid_migration_step`。
- 链上缺步骤返回 `missing_migrator`，details 为 `{ component, from, to, at }`，`at` 是链停止的版本。
- 步骤自身失败返回 `migration_failed`，details 为 `{ component, from, to, cause }`。

`Engine::unregister_component_migrators(component)` 清除该组件的全部迁移步骤，不影响组件注册本身；清除到链时推进 revision，无链可清除时为无操作。迁移器与注册表一样进入快照，随恢复回退。

## 属性定义

`PropertyField` 描述字段名、类型、是否必需、默认值、数值范围、长度限制、显示标签和说明；`PropertyField::new` 与 `default`/`range`/`max_length`/`label`/`description` 构造器是公开的，`PropertySchema::new` 组装字段集合。`PropertySchema` 负责应用默认值、检查未知字段和字段值，并生成 JSON Schema 风格的 agent 描述，附带 `x-editor` 和 `x-component` 元数据。

`capabilities.components` 返回当前会话注册表中的 schema。Rust 命令、场景解析和 agent 查询共享同一来源。

## 内置组件

- `core.transform`
- `core.sprite`
- `core.model`
- `core.script`

内置组件存储在实体的类型化字段（`transform`/`sprite`/`model`/`script`）而不是 extensions，因此 `attach_component` 与 `remove_component` 会明确拒绝这些 ID，`unregister_component` 始终拒绝。

`.mnsc` v1 使用未加命名空间的 ID；v2 使用上述 `core.*` ID。脚本组件使用 `source` 引用外部 JavaScript，或使用 `inline` 保存内联源码，两者必须且只能存在一个。

## 未知数据

未注册的组件键不会被当作内置组件执行，而是保存在 `Entity.extensions`；场景根扩展保存在 `World.extensions`。场景写回时这些原始 JSON 会重新插入，因此缺少插件不会导致加载再保存后静默删除数据。

动态组件数据存储在同一位置，所以场景保存/加载与快照往返天然保留它。会话加载路径（`RuntimeSession::load_scene`，GUI/CLI/agent 共用的唯一场景加载入口，见 [场景格式](scene-format.md)）会用会话注册表校验动态组件数据：旧版本数据先沿迁移链升级，再应用 schema 默认值并校验。直接调用 `scene::load_scene`（`moni scene` CLI 往返、只读查看器）仍只做 JSON 结构检查。

加载成功后内存世界保留重建的组件版本表：未注册组件与未来版本实例的场景版本条目被保留，已迁移/校验数据的条目被消费；registry 感知保存按实际内容重建该表。条目的取舍规则与未来版本实例的只读语义统一由[场景格式](scene-format.md)定义，此处不重复。

`restore_entity` 把实体快照按其显式 ID 原样重插（undo/redo 的稳定 ID 语义）：extensions 数据——含未注册组件与未来版本实例——随之恢复，恢复本身不做 schema 校验。despawn 清除的版本表条目不随恢复复活；因此 despawn 再恢复一个未来版本或未注册组件的唯一携带者，会在下一次保存时丢失其场景版本记录。

保留数据不代表执行权限。第三方组件注册和权限校验仍需由插件 SDK 与插件管理器完成。

## 当前限制

- 动态组件只是经过 JSON schema 校验的数据：没有类型化热路径存储，每帧逻辑不应每 tick 读取它们；需要逐帧访问的数据应等待类型化组件路径（规划中）或使用内置组件。
- 组件 ID、字段名和 schema 元数据使用 `&'static str`；运行时注册需要静态字符串（例如字面量或 `Box::leak`）。
- 注册与撤销目前只有 Rust API（`Engine::register_component` / `unregister_component`）；协议命令只覆盖挂载与移除，注册不在批处理/协议层开放。`component_registered` / `component_unregistered` 事件由引擎在任何注册入口上发出。
- 迁移只支持动态组件且只支持升级（`from < to`）；没有降级路径。内存世界通过组件版本表保留未注册组件与未来版本实例的场景版本（规则见[场景格式](scene-format.md)）；已迁移/校验的数据在保存时按注册表当前版本写入版本表。
