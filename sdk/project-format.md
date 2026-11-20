# Moni 游戏工程与锁文件

中文 | [English](project-format_en.md)

`.mngp` 是独立游戏工程的源配置，`moni.lock` 是当前 profile 的确定性依赖解析结果。引擎安装和游戏仓库分离，项目不保存引擎源码路径。

## `game.mngp`

```json
{
  "format": "moni-game-project",
  "version": 1,
  "name": "my-game",
  "engine": "0.1.0",
  "profile": "default",
  "scene": "scenes/main.mnsc",
  "plugins": [],
  "patches": [],
  "dependencies": {}
}
```

- `format` 必须为 `moni-game-project`，`version` 当前为 1。
- `engine` 必须与当前引擎版本精确匹配。
- `profile` 使用静态 profile 名称；`plugins` 添加显式插件，`patches` 应用有序启停覆盖。
- `scene` 必须是项目内相对路径，禁止绝对路径和 `..`。
- `dependencies` 保存项目的直接依赖及版本；当前模板为空。

## `moni.lock`

锁文件包含：

- `format: "moni-project-lock"` 和锁格式版本 1。
- 引擎版本和 profile。
- 每个有效插件的精确版本、API 版本、权限、资源声明和清单 SHA-256。
- 项目的直接依赖。

`moni new` 在复制模板后生成锁文件。`moni run` 在启动前比较锁文件与当前 profile 解析结果；不一致时拒绝运行。`moni doctor --update-lock` 报告权限差异，新增权限需要 `--accept-permissions`，更新前保存 `moni.lock.backup`。`moni doctor --rollback-lock` 恢复备份并重新校验。哈希用于完整性检查，不代表发布者签名。

## 基础命令

```bash
moni new my-game
cd my-game
moni run
moni doctor
moni profile dump
```

`moni new` 复制 `templates/minimal-game/`、设置项目名、生成锁文件并初始化独立 Git 仓库。`moni run` 按需通过 `npm ci` 安装锁定依赖，执行 TypeScript 构建，然后使用项目的 profile 和 `.mnsc` 场景启动客户端。

当前 `run` 从引擎安装或源码模板目录查找模板；正式发布时需要把模板随引擎分发，不能依赖开发机源码路径。
