# QuickJS 脚本插件

中文 | [English](README_en.md)

`mod.rs` 提供每实体持久 QuickJS 运行时，将 TypeScript 编译后的 JavaScript 转换为经过 Rust 校验的引擎命令。它通过 `core.simulation` 在逻辑线程运行。

## 当前行为

- 源码在宿主创建时编译一次，顶层状态跨逻辑帧保留。
- 暂停/恢复保留状态，替换脚本、报错、despawn、reset/load 和关闭会清理宿主。
- CPU 时间、内存、栈、命令、日志和结果大小均有限制。
- `Date` 和 `Math.random` 被禁用，以保留确定性引擎数据。
- 世界快照保存源码和资源路径，不保存 JavaScript 堆。

QuickJS 上下文与 `Rc` 状态不跨线程共享；其他系统通过命令缓冲交互。

## 限制

QuickJS 仍是静态内置提供方，没有动态 ABI、断点调试、热重载状态迁移或独立权限系统。TypeScript 仍需先由项目构建为 JavaScript。
