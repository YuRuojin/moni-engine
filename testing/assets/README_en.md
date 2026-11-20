# External test assets

[中文](README.md) | English

This directory holds committed benchmark scenes plus local test models excluded from Git by default. Demos and performance benchmarks load assets here instead of embedding FBX files or test-scene binaries in the engine executable.

## Committed scenes

- `benchmark-2d.mnsc`: orthographic 2D sprite grid for simulation/render benchmarks.
- `benchmark-3d.mnsc`: perspective 3D scene referencing the generated cooked mesh.

`benchmark-3d.mnsc` references `benchmark.mnec`, which is generated deterministically by [`../generate-fixture.cjs`](../generate-fixture.cjs) (1,975,083 vertices) and stays out of Git. `testing/run-benchmarks.cjs` generates it automatically when missing.

## Local-only cases

- `garden_diorama.fbx`: garden diorama model.
- `xujiahui_cyber_geo.fbx`: Xujiahui cyber scene model.
- `orthographic.mnsc`: orthographic 2D-view test scene.
- `perspective.mnsc`: perspective 3D-view test scene.

Both local `.mnsc` v2 scenes reference the same FBX entities and declare cameras, lights, materials, and script resource paths. Test entities are not hardcoded under `src/`. The loader also accepts v1 and performs v1→v2 migration.

Provide local files at the listed paths. Related demos must fail with an actionable path error when a file is missing rather than silently falling back to built-in entities. The engine parses model geometry at runtime; binary test assets are not committed to the repository.

See [`plugins/assets/`](../../plugins/assets/README_en.md) for the asset architecture and [`testing/README.md`](../README_en.md) for benchmarks.
