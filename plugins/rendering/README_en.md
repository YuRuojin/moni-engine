# Rendering plugins

[中文](README.md) | English

`rendering/` turns scene data into images through replaceable render pipelines, materials, lighting, shadows, post-processing, and visual extensions for render debugging. The current 2D/3D code still lives in `src/gpu.rs` and the WGSL files; it has not been extracted into independent plugins.

## Responsibility boundary

- Pipelines: explicitly named forward, deferred, or other pipelines and their pass composition.
- Materials: parameters, texture bindings, shader variants, compilation caches, and error fallbacks.
- Lighting and shadows: light types, shadow maps, culling data, and quality levels.
- Post-processing: ordered screen effects such as tone mapping, antialiasing, and bloom.
- Render resources: GPU representations of meshes, textures, and buffers; source file parsing belongs to [`assets/`](../assets/README_en.md).

Material panels belong to [`editor/`](../editor/README_en.md). OS windows, device capabilities, and toolchains belong to [`platforms/`](../platforms/README_en.md); this category owns the GPU abstraction, `wgpu` backend, pipelines, and shaders rather than a concrete render style in the core.

## Initial official capabilities

First extract the built-in 2D/3D paths as default static plugins, then establish basic interfaces for material descriptions, textures, models, and shadows. Visual styles should use data and replaceable pipelines rather than accumulating mutually exclusive conditions in core shaders.

## Performance and stability

- Reuse meshes, textures, samplers, pipelines, and buffers; unbounded per-frame creation is not allowed.
- Measure CPU submission time, GPU pass time, draw calls, triangle counts, graphics memory, and shader compilation stutters.
- Material variants and caches need bounds and invalidation rules. Asynchronous compilation must not block simulation ticks.
- Avoid synchronous GPU readbacks. Resizing, device loss, and plugin deactivation must release or rebuild the correct resources.
- Disabling optional shadows or post-processing must also disable their render passes, textures, and per-frame work.

## Current limitations

Supported features include external FBX meshes, base color/metallic/roughness materials, directional light, up to 8 point lights, a 1024 shadow map with 3x3 PCF, GPU vertex caches, model matrices, and bounding-sphere frustum culling. Full textured materials, cascaded shadows, post-processing, and instancing are absent. CPU and main/shadow-pass GPU baselines exist, but no scene matrix or graphics-memory baseline exists.
