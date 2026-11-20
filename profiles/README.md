# Profiles 与 Bundles

中文 | [English](README_en.md)

`profiles/` 按 dsh 的方式管理引擎组合：**profile 是命名组合，bundle 是可复用的插件声明层，patch 是有序覆盖**。这里不包含具体游戏功能；渲染、物理、编辑器等能力始终由插件提供。

## 组合模型

- **Bundle**：一组带标识的插件声明、默认配置和依赖，可被多个 profile 复用。
- **Profile**：选择有序 bundles、额外插件和自身补丁，形成编辑器、玩家、自动化或服务器运行环境。
- **Patch**：按声明标识替换配置或插入插件，不修改 bundle 源码。
- **启动适配器**：只解析命名 profile 并启动宿主，不实现另一条硬编码功能组合。

官方组合方向包括 `base`、`default`、`editor`、`player`、`headless` 和 `server`。`headless` 与 `server` 通过不挂载窗口和渲染提供方实现，而不是维护另一套引擎。

## 有序覆盖

目标解析顺序为：profile 的有序 bundles、项目插件声明、项目补丁、用户补丁、单次运行或 agent 补丁。后层可以覆盖前层配置；最终有效插件树必须可导出、可检查、可复现。

游戏仓库通过 [`templates/`](../templates/README.md) 获得初始 profile；安装和解析插件由 [`tools/`](../tools/README.md) 负责；插件声明与依赖规则由 [`sdk/`](../sdk/README.md) 定义。

## 服务组合

profile 只组合能力，不直接调用具体提供方。每项可替换能力遵循服务定义、服务提供方、消费者的划分；多个提供方按显式配置选择。插件卸载时，其服务、事件、任务和资源注册随生命周期撤销。

## 性能与稳定性

- 发布构建根据最终有效树裁剪未选择插件及依赖，而非启动后仅跳过更新。
- `player` 不包含编辑器、Hub 和源资源导入器；`headless` 不初始化窗口或 GPU。
- 配置覆盖不得绕过权限、依赖和兼容性校验。
- 每种 profile 分别测量启动、内存、二进制体积和关闭清理；补丁造成失败时报告具体声明层。

## 当前限制

当前只有 `src/main.rs` 中的 `client`、`serve` 和 `demo` 入口，没有 profile 解析器、bundle 格式、有序补丁、有效树导出或按组合裁剪构建。
