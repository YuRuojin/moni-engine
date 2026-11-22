# src：组合根二进制

中文 | [English](README_en.md)

`src/` 是组合根：唯一的 CLI 入口和唯一允许命名具体插件类型的位置。世界数据、命令分发、插件宿主、profile 解析等内核事实位于 [`crates/moni-core/`](../crates/moni-core/)；具体游戏机制、编辑器、文件格式导入和平台打包不属于这一层。

## 当前模块

| 文件/目录 | 职责 |
|---|---|
| `main.rs` | CLI 子命令、编辑器工程检测/profile 合成、profile 列表和 workspace 路径限制 |
| `bootstrap.rs` | 静态插件工厂与 `serve_session()`：组合根把 profile 的插件装配进会话 |
| `client.rs` | `winit` 窗口、重绘、追帧推进和渲染基准采集，基于 `RuntimeSession` |
| `fuzz.rs` | 结构化进程内解析器模糊测试 |
| [`crates/moni-core/src/engine.rs`](../crates/moni-core/README.md) | 世界、实体、命令、事件、revision、快照回滚和固定逻辑帧 |
| [`crates/moni-core/src/session.rs`](../crates/moni-core/README.md) | `RuntimeSession`：客户端/编辑器/agent 共用的运行时路径与幂等请求缓存 |
| [`crates/moni-core/src/host.rs`](../crates/moni-core/README.md) | 插件宿主：依赖调度（含缓存）、访问强制、服务注册表、插件命令和级联卸载 |
| [`crates/moni-core/src/protocol.rs`](../crates/moni-core/README.md) | NDJSON 协议、原子批处理、dry-run、能力描述和查询/快照读写 |
| [`plugins/rendering/wgpu/`](../plugins/rendering/wgpu/README.md) | `wgpu` 设备、2D/3D/阴影管线、FBX 加载、模型矩阵、剔除和 GPU 时间戳 |
| [`plugins/editor/scene-viewer/`](../plugins/editor/scene-viewer/README.md) | 可编辑场景编辑器（工程检测、面板、播放模式）、经命令 API 的相机控制和性能/层级 overlay |
| [`plugins/scripting/quickjs/`](../plugins/scripting/quickjs/README.md) | 持久 QuickJS 宿主、脚本上下文、限制和命令收集 |
| [`profiles/`](../profiles/README.md) | 命名 profile（含 `serve`）、bundles 和有效树 |

## 核心边界

`serve`、`client` 和 `editor` 共用 `RuntimeSession`：同一条命令分发、同一套插件生命周期、同一套 revision 与事件语义。GUI 输入（如查看器相机）走公开命令 API，保证人与 agent 修改场景的结果可互换。`serve` profile 装配 `core.simulation`，agent 路径具备脚本执行与插件生命周期，而非绕过宿主的裸引擎。

内核作为插件宿主，只拥有共同依赖的实体标识、组件/属性注册、场景层级与预制体基础、序列化迁移、系统调度、线程模型、时间/随机和资源生命周期。可替换行为通过服务定义、事件和可撤销效果注册接入；核心循环不提供按具体插件分支的特权路径。

当前插件由 Rust 静态注册，服务注册表、插件命令注册、启动失败原子回滚和级联卸载已实现，但动态 ABI、第三方组件类型化存储和外部插件仓库尚未实现；后续扩展必须保留现有协议、快照和线程约束。

## 性能与稳定性

- 世界快照和事件必须有明确所有权，插件不能绕过声明访问直接破坏核心状态；宿主在运行时拒绝未声明的访问。
- 逻辑帧需要保持可复现；耗时系统不得制造无界追帧或无界任务队列。
- GPU 资源应复用并具有确定释放路径，窗口缩放和设备丢失不能导致资源持续增长。
- wgpu 插件持久复用内置网格缓冲区，FBX 在初始化时通过异步 worker 预加载并上传，每帧只更新每实体模型矩阵与缓冲；新增资源通过非阻塞 `poll` 进入缓存。QuickJS 插件为每个脚本实体保留运行时，替换脚本、实体销毁、世界加载和脚本错误会清理对应宿主。
- 修改核心热路径时使用 `npm run benchmark` 的外部 FBX 固定场景测量，并同时验证正交、透视与协议路径；单一场景结果不能外推到其他负载。

## 当前限制

动态注册和完整资源系统尚未建立。已有约 198 万顶点的 CPU 与 GPU 基准、`testing/soak.cjs` 的基础内存采样、异步加载和 cooked mesh，但没有多场景内存矩阵、纹理、动画、资源包或链接级插件裁剪。`render.rs` 的 SVG 输出只覆盖 2D 精灵；协议 `render` 命令仍输出 SVG，与窗口 wgpu 渲染不是同一管线，截图需使用带窗口的客户端。
