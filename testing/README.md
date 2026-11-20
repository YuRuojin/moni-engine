# 测试与验收

中文 | [English](README_en.md)

`testing/` 负责可重复验证引擎、插件、模板和发布产物，并提供第三方插件可复用的兼容性测试工具。这里消费诊断数据并作出验收判断；运行时指标采集实现属于 [`plugins/diagnostics/`](../plugins/diagnostics/README.md)。

## 验证范围

- 核心：协议、快照、固定逻辑帧、序列化兼容和迁移。
- 真实组合：通过 loader 启动完整 profile 与有序补丁，而非只用手工注册代替产品组合。
- 插件生命周期：重复注册、启停、失败回滚、热重载、效果注销和资源泄漏。
- 插件契约：接口版本、权限、依赖冲突和目标过滤。
- 内容：导入器模糊测试、损坏文件、缓存一致性和资源热重载。
- 渲染：固定场景的 golden 图像、GPU 验证和设备/窗口变化。
- 发布：模板创建、依赖锁定、实际安装包启动和多平台产物验证。
- 性能：固定场景与输入的基准运行、分位数比较、内存增长和回归报告。

Profile 与 bundles 属于 [`profiles/`](../profiles/README.md)，模板属于 [`templates/`](../templates/README.md)，实际导出器属于 [`plugins/platforms/`](../plugins/platforms/README.md)。

## 第三方测试工具包

SDK 稳定后，外部插件仓库应能复用生命周期、序列化、权限和目标兼容测试，而不必复制引擎内部测试代码。测试失败需要给出插件标识、运行域、目标和可定位原因。

## 性能与稳定性

- 性能测试使用 release 构建、固定场景和输入、预热、足够样本及明确环境。
- 原始结果包含 p50、p95、p99、峰值、样本数和内存曲线，不只保留平均帧率。
- 渲染比较允许明确的平台容差，不能用宽泛容差掩盖真实回归。
- 测试独立清理窗口、GPU、进程、临时目录和后台任务，重复运行结果一致。

## 当前性能基准

```bash
npm run benchmark -- --label baseline --frames 600 --warmup 120
```

该命令构建 release 二进制，先确保 `testing/assets/benchmark.mnec` 存在（约 198 万顶点的 cooked 网格，由 [`generate-fixture.cjs`](generate-fixture.cjs) 确定性生成，不入库），然后加载已入库的 [`benchmark-2d.mnsc`](assets/benchmark-2d.mnsc) 与 [`benchmark-3d.mnsc`](assets/benchmark-3d.mnsc) 场景，依次运行逻辑帧和真实窗口渲染，把原始毫秒样本与 p50/p95/p99 写入 `target/benchmarks/baseline/`。逻辑帧指标包含 `Engine::step(1)` 和事件排空；渲染总耗时包含 present/垂直同步等待，准备耗时单独覆盖顶点、uniform、模型矩阵和 vertex buffer 操作。硬件支持时，主/阴影 pass 每 30 帧额外写入 GPU 时间戳；同步读回仅用于基准模式。

## 资源与插件目录验证

已验证异步 worker 预加载、运行时非阻塞接收、cooked 转换与 `.mnec` 场景加载；基准场景加载 1,975,083 顶点的生成 cooked 网格。FBX、wgpu、QuickJS 和 scene-viewer 实现已移入各自官方插件目录。

## 独立项目验证

`node testing/template-e2e.cjs` 自动执行完整验收：`moni new` 创建独立 Git 工程、校验结构与锁文件、确认仓库不含引擎绝对路径、`doctor` 与 `profile dump` 在工程目录内通过、`moni run --frames 3` 完成 `npm ci`、TypeScript 构建并实际运行窗口渲染、场景保存为 v2 `.mnsc`。可用 `MONI_E2E_SKIP_RUN=1` 跳过窗口运行，`--keep` 保留工程目录便于检查。锁文件被修改时 doctor 会拒绝，`doctor --update-lock` 修复后再次检查通过。

## Profile 组合验证

当前已验证 `default`、`editor`、`minimal`、`headless` 的有效树和真实实例化。`minimal/headless` 不初始化 GPU、QuickJS 或 FBX，patch 能移除默认插件，错误的默认 headless 组合会明确失败。release 基准保持原有量级。

## 插件宿主验证

当前真实客户端与查看器通过静态宿主启动、执行固定帧和渲染，并通过 `PluginHandle` 卸载模拟插件。关闭路径会清理 QuickJS 与 GPU 资源；跨阶段启动依赖、渲染和 release 基准均已验证。

## 场景与快照验证

`cargo run -- scene <INPUT.mnsc> --output <OUTPUT.mnsc>` 可执行场景迁移和往返保存。当前验证覆盖 v1→v2、连续往返一致、未知嵌套组件保留、脚本路径保留、快照 v1 升级和 `capabilities.components` 查询。

## 兼容性、Golden 与稳定性工具

```bash
npm run test:compat
npm run test:soak
```

`test:compat` 覆盖清单/API、profile 顺序、headless/minimal、权限拒绝、场景迁移、损坏 FBX 进程隔离和两次 golden 像素一致。`test:soak` 默认运行 1200 帧，并在 Windows 采样进程工作集；原始报告位于 `target/testing/soak-report.json`。

当前 golden 使用 RGBA 原始像素和 JSON 元数据，不包含跨 GPU 容差图。损坏输入测试是固定烟测，不是持续模糊测试。1200 帧测试是稳定性烟测，不代表长时间发布认证。

## 当前限制

`cargo test` 提供针对性单元测试（profile 实例化回滚、权限授予/撤销、锁文件往返与回滚、路径校验、颜色解析缓存、模板初始化与路径隔离），模板端到端由 `template-e2e.cjs` 覆盖；仍无完整第三方兼容认证服务或 CI 接入。CPU/GPU 基准、golden 和内存采样已建立，但没有多场景矩阵或跨平台基准。
