# 外部测试资源

中文 | [English](README_en.md)

此目录保存已入库的基准场景和默认不纳入 Git 的本地测试模型。测试、演示和性能基准从这里读取资源，不把 FBX 或测试场景二进制内置进引擎可执行文件。

## 已入库场景

- `benchmark-2d.mnsc`：正交 2D 精灵网格，用于模拟/渲染基准。
- `benchmark-3d.mnsc`：透视 3D 场景，引用生成的 cooked 网格。

`benchmark-3d.mnsc` 引用的 `benchmark.mnec` 由 [`../generate-fixture.cjs`](../generate-fixture.cjs) 确定性生成（1,975,083 顶点），不入库；`testing/run-benchmarks.cjs` 会在缺失时自动生成。

## 本地用例

- `garden_diorama.fbx`：花园立体模型（约 49 万顶点）。
- `xujiahui_cyber_geo.fbx`：徐家汇赛博场景模型（约 148 万顶点）。
- `garden_diorama.mnsc`：花园立体模型 3D 视图场景（方向光阴影 + 冷色点光）。
- `xujiahui_cyber_geo.mnsc`：赛博城市场景（方向光阴影 + 双色点光，金属质感）。
- `orthographic.mnsc`：正交 2D 视图测试场景（可选）。
- `perspective.mnsc`：透视 3D 视图测试场景（可选）。

场景为 `.mnsc` v2 格式，引用同目录 FBX 并声明相机、灯光和材质，不在 `src/` 中硬编码测试实体。加载器同时保留 v1 读取与 v1→v2 迁移能力。FBX 加载器将模型归一化到 [-1, 1] 盒，场景相机按此取 3/4 视角。

文件由使用者自行提供并放在本目录（不入 Git）。`testing/plugin-compat.cjs` 在这些场景存在时逐个用编辑器打开并渲染，校验顶点数；缺失时跳过并注明原因。缺少文件的演示应明确失败并提示路径，不能静默回退到内置实体。

资源导入架构属于 [`plugins/assets/`](../../plugins/assets/README.md)，基准入口见 [`testing/README.md`](../README.md)。
