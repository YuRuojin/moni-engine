# Asset plugins

[中文](README.md) | English

`assets/` converts external files into reproducible engine assets through format detection, reading, validation, conversion, import metadata, caching, and dependency tracking. Its file-type handlers are processing extensions, not visual editor controls.

## Responsibility boundary

- Format providers: explicit extensions or content formats for models, textures, audio, fonts, scenes, and other assets.
- Importers: convert source files into canonical intermediate data and report located errors.
- Asset database: stable asset identities, source hashes, import settings, and dependencies.
- Caches: converted results invalidated by source content, importer version, settings, and target platform.
- Runtime loading: virtual resource paths, shared references, asynchronous loading, preload, cancellation, unloading, and hot reload.
- Streaming and published reads: chunked large assets plus uniform access to archives and cooked resources after export.
- Build input: an explicit asset and dependency manifest for [`platforms/`](../platforms/README_en.md).

GPU upload and material use belong to [`rendering/`](../rendering/README_en.md); asset browsing and import-setting UI belong to [`editor/`](../editor/README_en.md).

## Initial official capabilities

The current static FBX importer in `src/fbx.rs` supports geometry triangulation, normals, node transforms, material base colors, coordinate/unit conversion, and normalization by maximum extent. Extract it into a formal format provider later, then unify asset identities, import results, textures, and other model formats.

## Performance and stability

- Import belongs to development or build paths, never synchronous file reading or conversion in the render hot path.
- Large files need streaming, explicit size limits, and cancellation rather than unbounded whole-file memory use.
- Cache keys include every content and version input affecting the result. Concurrent imports must not expose partially written caches.
- Untrusted or complex parsers should prefer isolated processes; parse failures must not crash the editor or player runtime.
- Game packages contain validated dependencies only and never depend on absolute development-machine paths.

## Current implementations

- [`fbx/`](fbx/README_en.md): FBX geometry, normals, node transforms, and material base colors.
- [`runtime/`](runtime/README_en.md): bounded asynchronous loading, shared references, cancellation, timeout, and caching.
- [`cooked/`](cooked/README_en.md): `.mnec` static geometry I/O and FBX conversion.

Initialization preloads through the same worker, while new models arrive through nonblocking `poll`. There is no asset database, textures, animation, virtual resource paths, persistent import cache, dependency graph, or complete asset package; cooked files are still read individually.
