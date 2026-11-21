# 场景编辑器插件

中文 | [English](README_en.md)

`mod.rs`、`document.rs`、`ui.rs` 和 `ui_egui.rs` 实现 `editor` 命令的场景编辑器。`editor` profile 挂载 `core.render`、`core.fbx` 和 `core.editor`，不注册 QuickJS 模拟插件。

## 当前能力

- 打开并校验 `.mnsc`。场景由 `document.rs` 的 `EditorDocument` 打开：撤销/重做历史、选择和脏标记是文档状态，所有编辑经会话命令 API 产生与 agent 编辑相同的 revision 和事件。
- 使用相同渲染和资源提供方显示 FBX/cooked 场景。
- 显示实体层级、模型资源、材质、灯光和性能 overlay；overlay quad 仅在输入签名变化时重建，时间统计按 0.1 ms 量化，golden 模式始终安装空 overlay。字符 overlay 保持为诊断显示，与 egui 面板分开。
- egui 编辑面板（`ui_egui.rs`，依赖 egui 0.37 + egui-winit 0.37，版本锁定在本目录 Cargo.toml）：
  - 层级面板：实体列表（id + 名称）选择，创建、逐实体删除与重命名。
  - Inspector：名称、Transform（拖拽值按 change-commit 应用）、附加动态组件按注册 schema 驱动字段（数字范围、枚举下拉、颜色/路径/字符串文本、对象 JSON），组件添加（值按 schema 默认预填）与移除；替换组件即重新附加新值。
  - 撤销/重做（Ctrl+Z / Ctrl+Y 及带深度的按钮）、脏标记、保存（Ctrl+S）与另存为。
  - 被拒绝的编辑（revision 冲突、future-version、schema 校验）显示为面板错误行，不清空选择或历史。
- 编辑帧经类型擦除的 `RenderPluginApi::set_egui_frame` 交给渲染插件（负载 `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)`：egui 0.37 的分块细分必须在产生形状的上下文上运行，故由编辑器细分后移交；契约见 `crates/moni-core/src/host.rs`），由 wgpu 端的 egui-wgpu 合成器在呈现前作为最后一层绘制。
- golden 模式完全禁用 egui（不创建 egui 上下文、不发送任何帧），保证 golden 捕获字节确定性。
- 方向键移动相机（egui 正在使用键盘时让位），PageUp/PageDown 缩放，Home 重置。

## 限制

没有资产浏览器、停靠布局、动画、项目编辑、控件注册或插件 UI SDK。组件字段控件由 schema 属性类型驱动，不含按组件 ID 手写的专用界面。核心库保持 egui 无关；egui 类型只出现在本目录与渲染插件。
