# 插件清单与 Profile 组合

中文 | [English](profiles_en.md)

`src/profile.rs` 提供静态插件清单、依赖解析、目标过滤、静态 bundles、profile patch 和有效插件树导出。此阶段没有动态插件仓库或动态库 ABI。

## 插件清单

每个 `PluginManifest` 包含：

- `id`、`version`、`api_version`
- 直接依赖及其 `optional` 标记
- 冲突插件
- 权限声明
- 资源声明
- 模块列表

`PLUGIN_API_VERSION` 当前为 1。未知插件、缺失必需插件、冲突插件和不兼容 API 版本都会明确失败。

## 模块与目标过滤

模块运行域为 `runtime`、`editor`、`build`、`developer`、`test`。每个模块可以限制操作系统、CPU 架构和 `debug`、`release`、`shipping`、`test` 构建配置；空数组表示不限制。

`Target::current()` 使用当前操作系统、架构和构建配置。没有匹配模块的插件不会进入有效树。

## 依赖解析

Resolver 执行以下步骤：

1. 展开 profile 的有序 bundles 和显式插件声明。
2. 按 patch 顺序启用或禁用插件，未知 patch 目标会报错。
3. 递归加入必需依赖；可选依赖只在显式请求时加入。
4. 检测缺失依赖、禁用必需依赖和依赖环，并输出具体路径。
5. 拓扑排序后应用目标过滤和冲突检查。

被 patch 禁用的必需依赖不会被静默忽略，而是返回错误。

## 当前 bundles

- `minimal`：空组合。
- `default`：`core.simulation`、`core.render`、`core.fbx`。
- `headless`：空组合，不初始化窗口、GPU、QuickJS 或 FBX。
- `editor`：`core.render`、`core.fbx`、`core.editor`，不启动 QuickJS。

## 实例化与裁剪边界

`Profile::instantiate` 只创建有效树中具有静态工厂的插件。`core.render` 没有窗口时会失败，不能被误用于 headless；`core.fbx` 未选择时不会解析或上传 FBX。

当前裁剪发生在**有效树和运行时实例化层**：未选择模块不会初始化。Cargo feature、链接级二进制裁剪和多目标发布包裁剪尚未实现，不能把当前行为描述为已完成二进制体积优化。

## 检查命令

```bash
cargo run -- profile list
cargo run -- profile dump --profile default
cargo run -- profile dump --profile default --patch core.render=off
cargo run -- headless --profile headless
cargo run -- headless --profile minimal
```

`profile dump` 输出目标、依赖顺序、模块、权限和资源声明，便于 agent 和构建工具读取。
