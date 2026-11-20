# wgpu rendering plugin

[中文](README.md) | English

`mod.rs` and three WGSL files implement the current 2D, 3D, and shadow rendering. This directory owns the rendering provider, statically registered as `core.render` with the host.

## Current capabilities

- 2D sprites and editor overlays.
- Built-in meshes and FBX/cooked meshes.
- Orthographic/perspective cameras, depth testing, directional light, and up to 8 point lights.
- Base-color, metallic, and roughness materials.
- 1024 directional shadows with 3x3 PCF.
- FBX GPU vertex caches, per-entity model matrices, and bounding-sphere frustum culling.
- Benchmark-mode GPU timestamps.
- One-shot RGBA golden capture for static scenes with pixel/metadata consistency checks.

Asset parsing belongs to `plugins/assets/`. GPU resources are created, used, and released only on the render thread.

## Limitations

No dynamic ABI, textures, animation, cascaded shadows, post-processing, instancing, or cross-platform golden rendering exists. GPU timestamps require hardware support and are sampled only for benchmarks and the read-only viewer.
