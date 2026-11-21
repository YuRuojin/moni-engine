# wgpu rendering plugin

[中文](README.md) | English

`mod.rs` and three WGSL files implement the current 2D, 3D, and shadow rendering. This directory owns the rendering provider, statically registered as `core.render` with the host.

## Current capabilities

- 2D sprites and editor overlays.
- Built-in meshes and FBX/cooked meshes.
- Orthographic/perspective cameras, depth testing, directional light, and up to 8 point lights.
- Base-color, metallic, and roughness materials.
- 1024 directional shadows with 3x3 PCF.
- FBX GPU vertex caches shared across entities by (source, color, asset revision), reference-counted and released when no entity references a key; per-entity state keeps only the model matrix, bind group, and a shared handle.
- Built-in mesh and sprite vertices are cached per entity by geometry signature and rebuilt only when inputs or layer order change; editor overlay vertices are rebuilt only when the quad list changes.
- Per-entity model matrices and bounding-sphere frustum culling.
- Benchmark-mode GPU timestamps.
- One-shot RGBA golden capture for static scenes with pixel/metadata consistency checks.
- Editor egui UI compositing: `RenderPluginApi::set_egui_frame` accepts a type-erased egui frame payload `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)` (handed over by the editor after tessellating on the context that produced the shapes; the payload must match the egui version this plugin links, and a mismatch or a not-started renderer returns `false` and produces no UI layer). The renderer owns an egui-wgpu compositor (surface format) and a pending-frame slot; after the main/shadow/overlay passes it applies texture deltas and paints the UI onto the same surface view as the last pass before present, then frees the frame immediately. The golden capture path is unaffected (golden mode never sends a frame).

Dependencies: egui-wgpu 0.37 and egui 0.37 (egui-wgpu 0.37 does not re-export egui itself). The core crate `moni-core` stays egui-free; the UI frame contract crosses the plugin boundary only as `&mut dyn Any`.

Asset parsing belongs to `plugins/assets/`. GPU resources are created, used, and released only on the render thread.

## Limitations

No dynamic ABI, textures, animation, cascaded shadows, post-processing, instancing, or cross-platform golden rendering exists. GPU timestamps require hardware support and are sampled only for benchmarks and the editor. egui compositing supports the single root viewport only.
