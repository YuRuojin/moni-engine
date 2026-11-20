# FBX importer plugin

[中文](README.md) | English

`mod.rs` uses `ufbx` to read external FBX files into normalized geometry without animation or texture loading.

## Current capabilities

- Binary/ASCII detection through `ufbx`.
- Node and geometry transforms, coordinate axes, and unit conversion.
- Face triangulation, normals, and missing-normal fallback.
- Material base colors.
- Centering and normalization by maximum extent.
- Explicit failures for non-finite vertices, empty models, and missing files.

This implementation only produces geometry. GPU upload belongs to `plugins/rendering/wgpu/`, asynchronous scheduling to `plugins/assets/runtime/`, and cooked encoding to `plugins/assets/cooked/`.

## Limitations

Textures, skeletons, animation, skinning, texture materials, and external-resource packaging are unsupported. The importer is statically linked and has no independent ABI.
