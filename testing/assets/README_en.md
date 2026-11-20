# External test assets

[中文](README.md) | English

This directory holds local test models excluded from Git by default. Demos and performance benchmarks load assets here instead of embedding FBX files or test-scene binaries in the engine executable.

## Current cases

- `garden_diorama.fbx`: garden diorama model.
- `xujiahui_cyber_geo.fbx`: Xujiahui cyber scene model.
- `orthographic.mnsc`: orthographic 2D-view test scene.
- `perspective.mnsc`: perspective 3D-view test scene.

Both `.mnsc` v2 scenes reference the same FBX entities and declare cameras, lights, materials, and script resource paths. Test entities are not hardcoded under `src/`. The loader also accepts v1 and performs v1→v2 migration.

Provide these files locally at the listed paths. Related demos and benchmarks must fail with an actionable path error when a file is missing rather than silently falling back to built-in entities. The engine parses model geometry at runtime; test assets are not committed to the repository.

See [`plugins/assets/`](../../plugins/assets/README_en.md) for the asset architecture and [`testing/README.md`](../README_en.md) for benchmarks.
