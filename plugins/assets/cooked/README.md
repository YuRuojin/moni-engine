# Cooked mesh 格式

中文 | [English](README_en.md)

`.mnec` 是 Moni 当前的最小二进制 cooked mesh。`mod.rs` 提供读取和写入，`moni-engine cook` 将 FBX 转换为该格式。

## 格式

- 8 字节 magic：`MNECOOK1`。
- 小端 `u64` 顶点数量。
- 每顶点固定 44 字节：位置 3×f32、法线 3×f32、颜色 4×f32、颜色存在标记 u32。
- 最大 10,000,000 顶点。
- 严格检查文件长度、有限数值和颜色标记，禁止截断或尾随数据。

## 使用

```bash
cargo run -- cook testing/assets/garden_diorama.fbx --output target/garden_diorama.mnec
```

`.mnsc` 的 `core.model.source` 可以引用 `.fbx` 或 `.mnec`，两者经过同一资源运行时。该格式只包含静态几何，不包含材质纹理、动画、场景层级或依赖包。
