# Moni 最小游戏工程

中文 | [English](README_en.md)

这是由 `moni new` 复制的独立游戏仓库模板。`game.mngp` 保存引擎版本、默认 profile、场景和依赖；`moni.lock` 由 `moni new` 根据实际插件树生成。

## 开发

```bash
moni run
```

`moni run` 会执行项目的 TypeScript 构建并打开默认客户端。首次运行需要安装项目依赖；也可以手动执行：

```bash
npm install
npm run build
moni run
```

场景位于 `scenes/main.mnsc`，脚本位于 `scripts/main.ts`。构建输出 `build/` 和 `node_modules/` 不纳入 Git。引擎安装和项目源码分离，不需要修改引擎仓库。

## 检查

```bash
moni doctor
moni doctor --update-lock
moni doctor --rollback-lock
moni profile dump
```
