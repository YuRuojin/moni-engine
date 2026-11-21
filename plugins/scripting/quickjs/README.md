# QuickJS 脚本插件

中文 | [English](README_en.md)

`mod.rs` 提供每实体持久 QuickJS 运行时，将 TypeScript 编译后的 JavaScript 转换为经过 Rust 校验的引擎命令。它通过 `core.simulation` 在逻辑线程运行。

## 当前行为

- 源码在宿主创建时编译一次，顶层状态跨逻辑帧保留。
- 暂停/恢复保留状态，替换脚本、报错、despawn、reset/load 和关闭会清理宿主。
- CPU 时间、内存、栈、命令、日志和结果大小均有限制。
- 单个逻辑帧内所有脚本共享默认 10ms 的墙钟预算（见下文），超限时跳过剩余脚本而不是禁用。
- `Date` 和 `Math.random` 被禁用，以保留确定性引擎数据。
- 世界快照保存源码和资源路径，不保存 JavaScript 堆。

QuickJS 上下文与 `Rc` 状态不跨线程共享；其他系统通过命令缓冲交互。

## 每 tick 脚本时间预算

单个逻辑帧内所有脚本宿主共享一个墙钟时间预算，默认 10ms，可用 `QuickJsScriptRuntime::with_tick_budget` / `set_tick_budget` 调整。预算按该 tick 内各 `run_tick` 调用的累计耗时计算；耗尽后，剩余可运行实体在本 tick 被跳过而不是禁用：宿主与 `enabled` 状态保持不变，下一 tick 自动恢复，每个被跳过的实体通过 `code` 为 `script_budget_exceeded` 的 `ScriptError` 事件上报。

默认 10ms 的依据是 60 FPS 下约 16.7ms 的整帧预算：脚本只占其中一部分，为引擎其余系统留出余量。单脚本 50ms 的 `script_timeout` 限制保持不变，预算只约束同一 tick 内所有脚本的累计成本。这是一个结构性护栏，防止脚本总量拖垮帧率；仓库中没有针对该预算的测量数据，也不据此声称任何性能提升。

## 限制

QuickJS 仍是静态内置提供方，没有动态 ABI、断点调试、热重载状态迁移或独立权限系统。TypeScript 仍需先由项目构建为 JavaScript。
