# 引擎示例

中文 | [English](README_en.md)

`examples/` 只包含引擎自身的可运行验证脚本，不是游戏工程模板。游戏应使用独立仓库；项目创建器尚未实现。

## 文件

- `model-orbit.ts`：旋转外部 FBX 场景中的花园模型，供 `npm run demo` 和 `npm run demo:3d` 使用。
- `player.ts`：早期 2D 精灵脚本示例。
- `spinning-cube.ts`：早期内置立方体脚本示例。
- `engine-api.d.ts`：当前脚本 API 的 TypeScript 声明。
- `tsconfig.json`：严格模式编译配置；`dist/` 是生成目录，不手工修改。

多个脚本共享全局入口 `onTick`，因此使用相同类型的全局函数变量声明，避免 TypeScript 重复函数实现错误。每次构建会把所有 TypeScript 示例编译到 `examples/dist/`。

## 验证

```bash
npm run check
npm run build
cargo run -- client --frames 5
cargo run -- client --scene 3d --frames 10
```

`--frames` 只用于自动退出验证。正常运行可使用 `npm run demo` 或 `npm run demo:3d`，按 Escape 关闭窗口。
