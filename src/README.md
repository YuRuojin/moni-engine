# src：引擎内核

中文 | [English](README_en.md)

`src/` 包含当前可执行的最小内核。这里负责世界数据、逻辑帧、命令分发、脚本宿主、窗口和 GPU 设备；具体游戏机制、编辑器、文件格式导入和平台打包不属于这一层。

## 当前模块

| 文件 | 职责 |
|---|---|
| `main.rs` | CLI 入口、外部测试场景加载和工作区路径限制 |
| `engine.rs` | 世界、实体、命令、事件、快照和固定逻辑帧 |
| `components.rs` | 组件 ID、所有者、版本、PropertySchema、序列化和 agent schema |
| `scene.rs` | `.mnsc` v1→v2 迁移、组件读写、扩展数据保留和快照迁移 |
| `scene3d.rs` | 3D 模型、材质、灯光和正交/透视相机的数据定义 |
| [`plugins/assets/fbx/`](../plugins/assets/fbx/README.md) | 基于 `ufbx` 的几何、法线、节点变换和材质基础色导入 |
| [`plugins/rendering/wgpu/`](../plugins/rendering/wgpu/README.md) | `wgpu` 设备、2D/3D/阴影管线、FBX GPU 缓存、模型矩阵、剔除和 GPU 时间戳 |
| `host.rs` | 单线程依赖调度、访问/线程亲和性校验、有界队列和插件生命周期 |
| `plugins.rs` | `core.simulation` 逻辑插件和 `core.render` GPU 插件 |
| `profile.rs` | 插件清单、运行域/目标过滤、依赖解析、bundles、profile patch 和有效树实例化 |
| `project.rs` | `.mngp` 工程、锁文件、项目校验和模板复制 |
| [`plugins/editor/scene-viewer/`](../plugins/editor/scene-viewer/README.md) | 只读场景查看器、相机控制和性能/层级 overlay |
| [`plugins/scripting/quickjs/`](../plugins/scripting/quickjs/README.md) | 持久 QuickJS 宿主、脚本上下文、限制和命令收集 |
| `client.rs` | `winit` 窗口、重绘、输入关闭和模拟推进 |
| `protocol.rs` | NDJSON 请求、能力描述、查询和快照读写 |
| `render.rs` | 2D SVG 输出 |

## 核心边界

内核作为插件宿主，只拥有共同依赖的实体标识、组件/属性注册、场景层级与预制体基础、序列化迁移、系统调度、线程模型、时间/随机和资源生命周期。可替换行为通过服务定义、事件和可撤销效果注册接入；核心循环不提供按具体插件分支的特权路径。

场景和资源 schema 是插件间互操作契约，不应被某个渲染或编辑插件私有占有。渲染风格、具体资源格式、物理算法、编辑界面和导出目标应通过 [`sdk/`](../sdk/README.md) 的接口接入，而不是继续加入 `engine.rs` 或 `gpu.rs` 的条件分支。

当前插件由 Rust 静态注册，核心组件注册、场景迁移、扩展数据保留、单线程生命周期和静态 profile 解析已实现，但动态 ABI、第三方组件注册和外部插件仓库尚未实现；后续扩展必须保留现有协议、快照和线程约束。

## 性能与稳定性

- 世界快照和事件必须有明确所有权，插件不能绕过命令直接破坏核心状态。
- 逻辑帧需要保持可复现；耗时系统不得制造无界追帧或无界任务队列。
- GPU 资源应复用并具有确定释放路径，窗口缩放和设备丢失不能导致资源持续增长。
- wgpu 插件持久复用内置网格缓冲区，FBX 在初始化时通过异步 worker 预加载并上传一次，每帧只更新每实体模型矩阵；新增资源通过非阻塞 `poll` 进入缓存。QuickJS 插件为每个脚本实体保留运行时，替换脚本、实体销毁、世界加载和脚本错误会清理对应宿主。
- 修改核心热路径时使用 `npm run benchmark` 的外部 FBX 固定场景测量，并同时验证正交、透视与协议路径；单一场景结果不能外推到其他负载。

## 当前限制

插件 API、动态注册和完整资源系统尚未建立。已有约 198 万顶点的 CPU 与 GPU 基准、`testing/soak.cjs` 的基础内存采样、异步加载和 cooked mesh，但没有多场景内存矩阵、纹理、动画、资源包或链接级插件裁剪。`render.rs` 的 SVG 输出只覆盖 2D 精灵。
