# 外部测试资源

中文 | [English](README_en.md)

此目录保存已入库的基准场景和默认不纳入 Git 的本地测试模型。测试、演示和性能基准从这里读取资源，不把 FBX 或测试场景二进制内置进引擎可执行文件。

## 已入库场景

- `benchmark-2d.mnsc`：正交 2D 精灵网格，用于模拟/渲染基准。
- `benchmark-3d.mnsc`：透视 3D 场景，引用生成的 cooked 网格。

`benchmark-3d.mnsc` 引用的 `benchmark.mnec` 由 [`../generate-fixture.cjs`](../generate-fixture.cjs) 确定性生成（1,975,083 顶点），不入库；`testing/run-benchmarks.cjs` 会在缺失时自动生成。

## 本地用例

- `garden_diorama.fbx`：花园立体模型。
- `xujiahui_cyber_geo.fbx`：徐家汇赛博场景模型。
- `orthographic.mnsc`：正交 2D 视图测试场景。
- `perspective.mnsc`：透视 3D 视图测试场景。

两个本地 `.mnsc` v2 场景引用相同的 FBX 实体，并声明相机、灯光、材质和脚本资源路径，不在 `src/` 中硬编码测试实体。加载器同时保留 v1 读取与 v1→v2 迁移能力。

本地文件由使用者自行提供并放在本目录。缺少文件时，相关演示应明确失败并提示路径，不能静默回退到内置实体。模型格式和几何由引擎在运行时解析，二进制测试资源不提交到仓库。

资源导入架构属于 [`plugins/assets/`](../../plugins/assets/README.md)，基准入口见 [`testing/README.md`](../README.md)。
