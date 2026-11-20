# Cooked mesh format

[中文](README.md) | English

`.mnec` is Moni's current minimal binary cooked mesh. `mod.rs` provides reading and writing, and `moni-engine cook` converts FBX into it.

## Format

- 8-byte magic: `MNECOOK1`.
- Little-endian `u64` vertex count.
- Fixed 44 bytes per vertex: 3×f32 position, 3×f32 normal, 4×f32 color, and a u32 color-presence flag.
- Maximum 10,000,000 vertices.
- Strict file length, finite-value, and color-flag validation; truncation and trailing data are rejected.

## Usage

```bash
cargo run -- cook testing/assets/garden_diorama.fbx --output target/garden_diorama.mnec
```

`core.model.source` in `.mnsc` may reference `.fbx` or `.mnec`; both use the same asset runtime. This format contains static geometry only, with no textured materials, animation, scene hierarchy, or dependency package.
