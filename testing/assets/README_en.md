# External test assets

[中文](README.md) | English

This directory holds committed benchmark scenes plus local test models excluded from Git by default. Demos and performance benchmarks load assets here instead of embedding FBX files or test-scene binaries in the engine executable.

## Committed scenes

- `benchmark-2d.mnsc`: orthographic 2D sprite grid for simulation/render benchmarks.
- `benchmark-3d.mnsc`: perspective 3D scene referencing the generated cooked mesh.

`benchmark-3d.mnsc` references `benchmark.mnec`, which is generated deterministically by [`../generate-fixture.cjs`](../generate-fixture.cjs) (1,975,083 vertices) and stays out of Git. `testing/run-benchmarks.cjs` generates it automatically when missing.

## Local-only cases

- `garden_diorama.fbx`: garden diorama model (~490k vertices).
- `xujiahui_cyber_geo.fbx`: Xujiahui cyber scene model (~1.48M vertices).
- `garden_diorama.mnsc`: 3D-view scene for the diorama (directional shadows + cool fill light).
- `xujiahui_cyber_geo.mnsc`: cyber-city scene (directional shadows + two-tone point lights, metallic material).
- `orthographic.mnsc`: orthographic 2D-view test scene (optional).
- `perspective.mnsc`: perspective 3D-view test scene (optional).

Scenes are `.mnsc` v2 files referencing the sibling FBX models with declared cameras, lights, and materials. Test entities are not hardcoded under `src/`. The loader also accepts v1 and performs v1→v2 migration. The FBX importer normalizes models into the [-1, 1] box; scene cameras use a three-quarter view accordingly.

Provide local files at the listed paths (kept out of Git). `testing/plugin-compat.cjs` opens and renders each scene with the editor when present and asserts the vertex count; it skips with a reason when the files are missing. Related demos must fail with an actionable path error rather than silently falling back to built-in entities. The engine parses model geometry at runtime; binary test assets are not committed to the repository.

See [`plugins/assets/`](../../plugins/assets/README_en.md) for the asset architecture and [`testing/README.md`](../README_en.md) for benchmarks.
