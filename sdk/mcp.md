# MCP 控制面适配器（`moni mcp`）

中文 | [English](mcp_en.md)

`moni mcp` 将引擎控制面暴露为 Model Context Protocol（MCP）stdio 服务器。它**只是**现有控制面之上的薄适配器：每个变更类工具构造与 NDJSON 客户端完全相同的请求，并经 `RuntimeSession::handle_line` 分发，因此 revision、批处理、事件与错误语义与 `moni serve` 一致（见 `moni capabilities` 的协议描述；本文不重复协议细节）。

## 启动

```bash
moni mcp                 # 空会话：Engine + QuickJS + serve profile + 插件启动
moni mcp --scene PATH    # 会话启动后加载 workspace 相对路径的 .mnsc 场景
moni mcp --project       # 加载当前 .mngp 项目的场景（与 --scene 互斥）
```

- 传输：MCP stdio（JSON-RPC 2.0，行分隔帧），由官方 Rust SDK `rmcp` 3.11.0（精确锁定）承载；tokio 仅存在于 `src/mcp.rs`，引擎代码保持单线程同步。
- 会话：整个服务器生命周期一个 `RuntimeSession`（`bootstrap::mcp_session`），在专用引擎线程上构建并运行；工具调用按 MCP stdio 的顺序语义经任务通道逐个进入该线程。服务器在客户端关闭 stdin 时以 EOF 干净退出。

## 工具（第一批）

| 工具 | 副作用 | 说明 |
|---|---|---|
| `capabilities` | 只读 | 静态协议能力 + 本会话 overlay（profile、插件、服务、插件命令、组件注册表） |
| `scene_info` | 只读 | 实体计数与 id/name/组件列表摘要、revision、tick 与 tick rate、相机与灯光 |
| `scene_load` | 变更世界 | `RuntimeSession::load_scene`（与 CLI/编辑器同一校验路径）；成功替换世界、revision 与脚本宿主，失败保持会话不变 |
| `scene_save` | 写文件 | 协议 `save`：写世界快照 JSON（**不是** `.mnsc` 场景格式）；可省略 path 时回退到会话场景路径 |
| `entity_spawn` | 变更世界 | 协议 `spawn`，参数为完整 EntitySpec；支持 `expectedRevision` |
| `entity_despawn` | 变更世界 | 协议 `despawn`；支持 `expectedRevision` |
| `entity_set_transform` | 变更世界 | 协议 `set_transform`；支持 `expectedRevision` |
| `component_attach` | 变更世界 | 协议 `attach_component`，schema 默认值先于校验应用；支持 `expectedRevision` |
| `component_remove` | 变更世界 | 协议 `remove_component`；支持 `expectedRevision` |
| `step` | 变更世界 + 脚本 | 协议 `step`（从不进入批处理）；每个 tick 运行脚本 onTick，脚本状态不可回滚；重复调用会继续推进，不幂等 |
| `render_svg` | 可写文件 | 协议 `render`：诊断用 2D/3D SVG 线框，**不是** GPU 帧；带 path 写文件，不带返回内联 SVG |
| `screenshot` | 写文件、开窗口 | 子进程 `moni editor --scene <s> --frames <n> --golden <out>`（仓库内验证过的字节稳定 golden PNG 捕获）；需要 GPU 与窗口站，无头环境按设计失败 |
| `run_client` | 开窗口 | 子进程 `moni client --scene 2d\|3d` 或 `--world <workspace 路径>` 渲染 `frames` 帧后返回客户端打印的最终 JSON；内置演示场景需要 `npm run build` |
| `diagnostics` | 只读（排空事件） | capabilities 的 session 块、revision、事件水位、脚本宿主数量与最近排空的事件（最多 50 条；排空会消耗缓冲事件） |

工具描述与 MCP 注解（`readOnlyHint`、`destructiveHint`、`idempotentHint`）如实声明副作用：变更类工具标注非只读，`step` 声明脚本副作用，`screenshot`/`run_client` 声明真实窗口，`render_svg` 声明其与 GPU 帧的区别。

## 语义保证

- **薄适配器**：`entity_*`、`component_*`、`step`、`scene_save`、`render_svg`、`capabilities` 全部走 `session.handle_line`；`scene_load` 走 `RuntimeSession::load_scene`（协议 `load` 加载的是世界快照而非 `.mnsc`）。工具结果即完整协议响应 JSON（`ok`/`result`/`error`/`events`/`revision`/`eventSeq`），不重新包装。
- **错误保真**：失败响应整体进入 MCP 工具结果的 `structuredContent` 且 `isError=true`，保留协议结构化错误 `{code, message, details}`，不做有损字符串化。参数形状错误使用同一词汇（`invalid_arguments`）。
- **revision 并发**：变更类工具可传 `expectedRevision`；不匹配时以 `revision_conflict` 失败且不产生任何变更。读取结果中的 `revision` 可直接作为下一次变更的 `expectedRevision`。
- **幂等与重放**：NDJSON 协议的请求 ID 重放缓存由会话维护；MCP 工具调用每次生成全新的内部请求 ID，因此相同参数的重复调用会再次执行而不是重放缓存响应。需要重放/幂等语义的 agent 应直接使用 `moni serve`。

## 当前限制

- 一个服务器生命周期内只有一个会话；没有跨进程共享或持久化。
- 所有路径（场景、输出）必须是 workspace 相对路径，越界被拒绝。
- `screenshot` 与 `run_client` 依赖真实 GPU 窗口，无头环境失败；截图走编辑器 golden 捕获，不是当前会话的实时画面。
- `scene_save` 输出世界快照 JSON；`.mnsc` 场景保存目前仅编辑器路径支持。
- 服务器未实现 MCP resources/prompts；第一批只暴露 tools。
