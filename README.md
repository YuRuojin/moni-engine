# Moni Engine

中文 | [English](README_en.md)

Moni Engine 是一个以 Rust 编写的轻量 2D/3D 引擎内核，目标是提供低成本、稳定、可被开发者和 agent 深入定制的游戏运行时。项目采用“精简核心、能力插件化、游戏工程独立”的方向；当前仍处于早期开发阶段，插件加载器、完整可视化编辑器和多平台导出尚未实现。

## 当前可运行能力

- 2D 矩形、圆形、变换、图层和 SVG 输出。
- 基础 3D 立方体、平面和外部 FBX 网格，支持正交/透视相机、深度测试、方向光和材质基础色。
- `.mnsc` v2 与 v1 迁移、组件/属性注册、未知数据保留和 agent 可读 schema。
- 方向光/点光源、基础材质和方向光阴影。
- NDJSON 命令协议、世界快照、固定逻辑帧和持久 QuickJS 脚本。
- 单线程静态插件宿主、依赖/访问校验、注册回滚和反向清理。
- 只读场景查看器、CPU 性能基线和主/阴影 pass GPU 时间戳。
- TypeScript 类型定义与 2D/3D 示例。

```bash
npm install
npm run demo
npm run demo:3d
```

## Agent 阅读路径

先按任务选择目录，再阅读该目录的 README；不要一开始加载整个仓库。

| 目录 | 职责 | 何时阅读 |
|---|---|---|
| [`src/`](src/README.md) | 最小运行时内核、协议、窗口和当前内置渲染器 | 修改核心循环、数据模型、GPU 设备或协议 |
| [`sdk/`](sdk/README.md) | 服务定义、插件生命周期、清单、权限和版本迁移 | 修改插件公共契约 |
| [`plugins/`](plugins/README.md) | 官方插件的分类、依赖规则和能力接缝 | 新增或修改可插拔能力 |
| [`plugins/rendering/`](plugins/rendering/README.md) | 材质、光照、阴影、后处理和渲染管线 | 修改画面或渲染架构 |
| [`plugins/assets/`](plugins/assets/README.md) | 文件格式读取、资源导入、运行时加载和缓存 | 添加模型、纹理、音频等格式 |
| [`plugins/gameplay/`](plugins/gameplay/README.md) | 物理、动画、音频、输入、UI、网络等游戏系统 | 添加通用游戏机制 |
| [`plugins/scripting/`](plugins/scripting/README.md) | 脚本语言、agent 控制、调试和热重载 | 修改脚本或自动化接口 |
| [`plugins/editor/`](plugins/editor/README.md) | 可视化编辑器、控件和项目操作 | 开发编辑界面 |
| [`plugins/platforms/`](plugins/platforms/README.md) | 平台适配、构建、导出和安装包 | 添加 Windows、Linux、HarmonyOS 等目标 |
| [`plugins/diagnostics/`](plugins/diagnostics/README.md) | 性能分析、日志、回放和诊断 | 提供运行时测量与诊断 |
| [`profiles/`](profiles/README.md) | 命名 profile、可复用 bundles 和有序补丁 | 修改运行环境的插件组合 |
| [`tools/`](tools/README.md) | 引擎 Hub、项目工具、插件安装和依赖解析 | 修改安装、项目或包管理 |
| [`templates/`](templates/README.md) | 独立游戏仓库模板 | 修改新项目初始内容 |
| [`testing/`](testing/README.md) | 真实组合、兼容性、渲染和性能验收 | 添加测试或回归门槛 |
| [`examples/`](examples/README.md) | 当前可运行的脚本示例 | 验证现有客户端 |

## 仓库与游戏工程边界

本仓库维护引擎内核、SDK、官方插件、组合、工具、模板和测试。游戏不应通过复制或修改引擎源码来制作。

目标使用方式是由已安装的引擎版本创建独立游戏仓库：

```text
my-game/
  moni.toml          # 规划中的引擎版本、插件和构建配置
  assets/            # 原始资源与导入元数据
  scenes/            # 场景和实体配置
  scripts/           # 游戏逻辑
  plugins/           # 可选的游戏专属插件
```

`moni new`、项目模板和版本解析目前尚未实现。第三方通用插件也应拥有独立仓库，由游戏工程按兼容版本安装；只有官方维护的插件放入本仓库的 `plugins/`。

## 性能与稳定性约束

性能和稳定性是功能进入默认配置前的验收条件，而不是完成后的优化项。

- 分别记录 CPU 与 GPU 帧时间，关注 p95、p99 和卡顿，不只看平均帧率。
- 记录稳态内存、资源加载时间、启动时间、包体积和长时间运行后的资源增长。
- 基准使用 release 构建、固定场景、固定逻辑输入、预热阶段和可复现环境。
- 插件未启用时不应产生每帧 CPU/GPU 开销；启停和卸载不得遗留资源或后台任务。
- 高频路径避免无界分配、阻塞文件 I/O、GPU 同步回读和跨进程调用。
- 没有基准数据时，不宣称比 Godot、Unity、UE 或其他实现更快、更省。

已通过 `npm run benchmark` 建立 release CPU 与 GPU 基线，报告保存在 `target/benchmarks/<标签>/`。当前基准从 `testing/assets/` 加载两个 FBX 和正交/透视 `.mnsc` 场景，合计约 198 万顶点；FBX 顶点与模型矩阵在 GPU 中复用，QuickJS 按脚本实体持久运行，主/阴影 pass 在硬件支持时每 30 帧采样。内存曲线和多场景矩阵仍未覆盖，不能据此宣称已达到整体性能目标。

## 迭代顺序

| 阶段 | 交付重点 | 进入下一阶段的条件 |
|---|---|---|
| A. 当前内核 | 现有 2D、基础 3D、协议和脚本 | 真实渲染与协议调用可运行 |
| B. 插件地基 | 性能基线、服务定义/提供方/消费者、可撤销生命周期、版本化清单、profile/bundle/patch、项目锁文件与模板 | 可重复测量，真实 profile 可安全组合和卸载，游戏仓库不依赖引擎源码路径 |
| C. 内容与画面 | 资源导入、模型/纹理、材质、光照、阴影和可替换管线 | 大场景性能和导入稳定性有数据 |
| D. 游戏生产 | 游戏系统、脚本调试与热重载、可视化编辑器 | 编辑器不损害玩家运行时成本 |
| E. 发布 | 各平台导出、依赖收集、签名和可复现打包 | 每个目标分别通过实际安装包验证 |

## 文档约定

每个职责目录使用 `README.md` 维护中文、`README_en.md` 维护英文，两份同步更新。README 只描述本层职责、接口、性能要求、当前限制和直接子目录；实现细节放在拥有它的最底层目录。规划能力必须明确标注，不能写成已支持功能。

许可证见 [LICENSE](LICENSE)。
