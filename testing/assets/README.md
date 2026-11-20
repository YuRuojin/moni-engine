# 外部测试资源

中文 | [English](README_en.md)

此目录保存本地测试模型，默认不纳入 Git。测试、演示和性能基准从这里读取资源，不把 FBX 或测试场景二进制内置进引擎可执行文件。

## 当前用例

- `garden_diorama.fbx`：花园立体模型。
- `xujiahui_cyber_geo.fbx`：徐家汇赛博场景模型。
- `orthographic.mnsc`：正交 2D 视图测试场景。
- `perspective.mnsc`：透视 3D 视图测试场景。

两个 `.mnsc` v2 场景引用相同的 FBX 实体，并声明相机、灯光、材质和脚本资源路径，不在 `src/` 中硬编码测试实体。加载器同时保留 v1 读取与 v1→v2 迁移能力。

文件由使用者自行提供并放在本目录。缺少文件时，相关演示与基准应明确失败并提示路径，不能静默回退到内置实体。模型格式和几何由引擎在运行时解析，测试资源不提交到仓库。

资源导入架构属于 [`plugins/assets/`](../../plugins/assets/README.md)，基准入口见 [`testing/README.md`](../README.md)。
