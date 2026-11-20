# 只读场景查看器插件

中文 | [English](README_en.md)

`mod.rs` 和 `ui.rs` 实现 `editor` 命令的只读场景查看器，通过 `editor` profile 挂载 `core.render`、`core.fbx` 和 `core.editor`，不注册 QuickJS 模拟插件。

## 当前能力

- 打开并校验 `.mnsc`。
- 使用相同渲染和资源提供方显示 FBX/cooked 场景。
- 显示实体层级、模型资源、材质、灯光和性能 overlay。
- 方向键移动相机，PageUp/PageDown 缩放，Home 重置。
- 不执行脚本、不修改或写回场景文件。

## 限制

没有属性编辑、撤销/重做、项目编辑、控件注册或插件 UI SDK。当前 overlay 使用引擎 2D 渲染，不包含完整编辑器框架。
