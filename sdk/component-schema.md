# 组件与属性 Schema

中文 | [English](component-schema_en.md)

`src/components.rs` 定义 `ComponentRegistry`、`ComponentDescriptor`、`PropertySchema` 和 `PropertyField`。它目前只注册 `moni.core` 拥有的内置组件，不代表第三方动态插件加载已经实现。

## 组件描述

每个 `ComponentDescriptor` 包含：

- `id`：当前版本使用的命名空间 ID，例如 `core.model`。
- `legacy_id`：v1 使用的兼容 ID，例如 `model`。
- `owner`：组件所有者，内置组件均为 `moni.core`。
- `version`：组件 schema 版本。
- `schema`：字段定义、默认值、校验和编辑器信息。
- `deserialize` / `serialize`：注册组件的 JSON 转换函数。

重复组件 ID 或 legacy ID 会被拒绝。消费者通过注册表解析组件，不直接依赖某个具体组件提供方。

## 属性定义

`PropertyField` 描述字段名、类型、是否必需、默认值、数值范围、长度限制、显示标签和说明。`PropertySchema` 负责应用默认值、检查未知字段和字段值，并生成 JSON Schema 风格的 agent 描述，附带 `x-editor` 和 `x-component` 元数据。

`capabilities.components` 返回当前注册表中的 schema。Rust 命令、场景解析和 agent 查询共享同一来源，不需要单独维护组件清单。

## 内置组件

- `core.transform`
- `core.sprite`
- `core.model`
- `core.script`

`.mnsc` v1 使用未加命名空间的 ID；v2 使用上述 `core.*` ID。脚本组件使用 `source` 引用外部 JavaScript，或使用 `inline` 保存内联源码，两者必须且只能存在一个。

## 未知数据

未注册的组件键不会被当作内置组件执行，而是保存在 `Entity.extensions`；场景根扩展保存在 `World.extensions`。场景写回时这些原始 JSON 会重新插入，因此缺少插件不会导致加载再保存后静默删除数据。

保留数据不代表执行权限。第三方组件注册、版本兼容和权限校验仍需由插件 SDK 与插件管理器完成。
