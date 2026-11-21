# 场景编辑器插件

中文 | [English](README_en.md)

`mod.rs`、`document.rs`、`ui.rs` 和 `ui_egui.rs` 实现 `editor` 命令的场景编辑器。`editor` profile 挂载 `core.simulation`（注册不启动）、`core.render`、`core.fbx` 和 `core.editor`；组合根在编辑器会话构造时安装 QuickJS 运行时——非播放状态脚本组件保持为数据，进入播放模式后脚本随模拟 tick 执行。

## 当前能力

- 打开并校验 `.mnsc`。场景由 `document.rs` 的 `EditorDocument` 打开：撤销/重做历史、选择和脏标记是文档状态，所有编辑经会话命令 API 产生与 agent 编辑相同的 revision 和事件。
- 使用相同渲染和资源提供方显示 FBX/cooked 场景。
- 显示实体层级、模型资源、材质、灯光和性能 overlay；overlay quad 仅在输入签名变化时重建，时间统计按 0.1 ms 量化，golden 模式始终安装空 overlay。字符 overlay 保持为诊断显示，与 egui 面板分开；overlay 中的场景路径跟随文档（经资产浏览器切换场景后同步更新）。
- egui 编辑面板（`ui_egui.rs`，依赖 egui 0.37 + egui-winit 0.37，版本锁定在本目录 Cargo.toml）：
  - 层级面板：实体列表（id + 名称）选择，创建、逐实体删除与重命名。
  - Inspector：名称、Transform（拖拽值按 change-commit 应用）、附加动态组件按注册 schema 驱动字段（数字范围、枚举下拉、颜色/路径/字符串文本、对象 JSON），组件添加（值按 schema 默认预填）与移除；替换组件即重新附加新值。
  - 撤销/重做（Ctrl+Z / Ctrl+Y 及带深度的按钮）、脏标记、保存（Ctrl+S）与另存为。
  - 播放工具栏：Play / Pause / Resume / Stop、PLAYING/PAUSED 状态指示与当前 tick；空格键在播放/停止间切换。
  - 资产浏览器：扁平分类列出工作区的场景（`.mnsc`）、模型（`.fbx`/`.mnec`）和脚本（`.js`/`.ts`），最多下探 3 层目录、上限 500 条，跳过 `target/`、`node_modules/`、`.git/` 与 `examples/dist/`；列表按会话 revision 缓存（播放期间不重扫），Refresh 按钮强制重扫。场景条目 Open、模型条目 Spawn（生成带 FBX 组件的实体，`.mnec` cooked 源与 `.fbx` 一样通过 `Model3d` 校验）、脚本条目 Attach to selected（读取文件内容附加到选中实体，读取前做大小上限检查，引擎的 `script_too_large` 仍是权威校验）。
  - 被拒绝的编辑（revision 冲突、future-version、schema 校验）显示为面板错误行，不清空选择或历史。
- 播放模式（`document.rs`）：`enter_play` 捕获进入前的完整世界并启动 `core.simulation`，`exit_play` 停止提供方并经 `Engine::load_world` 恢复进入前的世界——revision 向前推进并发出 `WorldLoaded`（不用会回退 revision 的 `EngineSnapshot`），退出后文档按保守策略标记为脏。播放期间 `apply`、全部便捷编辑、撤销/重做与保存/另存为都明确拒绝（经典播放期编辑锁）；编辑帧驱动与窗口客户端一致的固定步进累加器（每帧至多 5 个逻辑帧，经 `RuntimeSession::step` 走插件宿主）；模拟失败退出播放模式并在错误行显示，不崩溃应用。脚本宿主随 `load_world` 清空并在下次播放重建（脚本堆状态从不快照）。播放期间脚本随模拟 tick 执行（QuickJS 运行时由组合根安装）；退出播放经 `load_world` 清空脚本宿主并在下次播放重建。
- 编辑帧经类型擦除的 `RenderPluginApi::set_egui_frame` 交给渲染插件（负载 `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)`：egui 0.37 的分块细分必须在产生形状的上下文上运行，故由编辑器细分后移交；契约见 `crates/moni-core/src/host.rs`），由 wgpu 端的 egui-wgpu 合成器在呈现前作为最后一层绘制。
- golden 模式完全禁用 egui（不创建 egui 上下文、不发送任何帧），保证 golden 捕获字节确定性；golden 模式也不响应播放快捷键。
- 方向键移动相机（egui 正在使用键盘时让位），PageUp/PageDown 缩放，Home 重置。

## 限制

没有停靠布局、动画、项目编辑、控件注册或插件 UI SDK。脚本只能被替换不能被移除（核心命令集没有脚本移除命令）：首次附加脚本撤销后脚本仍在；资产浏览器为扁平列表，不含缩略图、拖放或目录树。组件字段控件由 schema 属性类型驱动，不含按组件 ID 手写的专用界面。核心库保持 egui 无关；egui 类型只出现在本目录与渲染插件。
