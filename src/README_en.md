# src: Engine kernel

[中文](README.md) | English

`src/` contains the current executable minimal kernel. It owns world data, simulation ticks, command dispatch, the script host, windowing, and the GPU device. Concrete game mechanics, editors, file-format importers, and platform packaging do not belong here.

## Current modules

| File | Responsibility |
|---|---|
| `main.rs` | CLI entry, external test-scene loading, and workspace path restrictions |
| `engine.rs` | World, entities, commands, events, snapshots, and fixed simulation ticks |
| `scene.rs` | `.mnsc` validation, resource reads, and conversion to `World` |
| `scene3d.rs` | Data definitions for 3D models, materials, lights, and orthographic/perspective cameras |
| [`plugins/assets/fbx/`](../plugins/assets/fbx/README_en.md) | `ufbx`-based geometry, normals, node transforms, and material base-color import |
| [`plugins/rendering/wgpu/`](../plugins/rendering/wgpu/README_en.md) | `wgpu` device, 2D/3D/shadow pipelines, FBX GPU caches, model matrices, culling, and GPU timestamps |
| `host.rs` | Single-thread dependency scheduling, access/thread-affinity validation, bounded queues, and plugin lifecycle |
| `plugins.rs` | `core.simulation` logic plugin and `core.render` GPU plugin |
| `profile.rs` | Manifests, domain/target filters, dependency resolution, bundles, profile patches, and effective-tree instantiation |
| `project.rs` | `.mngp` projects, lockfiles, project validation, and template copying |
| [`plugins/editor/scene-viewer/`](../plugins/editor/scene-viewer/README_en.md) | Read-only scene viewer, camera controls, and performance/hierarchy overlay |
| [`plugins/scripting/quickjs/`](../plugins/scripting/quickjs/README_en.md) | Persistent QuickJS hosts, script contexts, limits, and command collection |
| `shader.wgsl`, `shader3d.wgsl` | Current 2D and 3D shaders |
| `client.rs` | `winit` window, redraw, close input, and simulation advancement |
| `protocol.rs` | NDJSON requests, capability descriptions, queries, and snapshot I/O |
| `render.rs` | 2D SVG output |

## Core boundary

The kernel is a plugin host owning only shared entity identities, component/property registration, basic scene hierarchy and prefabs, serialization migrations, system scheduling, threading, time/randomness, and resource lifetimes. Replaceable behavior attaches through service definitions, events, and reversible effects; the core loop provides no privileged branches for concrete plugins.

Scene and resource schemas are interoperability contracts rather than private data of a rendering or editing plugin. Render styles, concrete asset formats, physics algorithms, editor UI, and export targets attach through [`sdk/`](../sdk/README_en.md) instead of more conditionals in `engine.rs` or `gpu.rs`.

Plugins are statically registered in Rust. Core component registration, scene migration, extension preservation, single-thread lifecycle, and static profile resolution exist, but dynamic ABI, third-party component registration, and external registries are absent; future work must preserve protocol, snapshot, and thread constraints.

## Performance and stability

- World snapshots and events need clear ownership; plugins must not bypass commands and corrupt core state.
- Simulation must remain reproducible. Expensive systems must not create unbounded catch-up or task queues.
- GPU resources should be reused and have deterministic release paths. Resizing and device loss must not cause continuing resource growth.
- The wgpu plugin reuses built-in mesh buffers, preloads FBX through an asynchronous worker during initialization, and updates only per-entity model matrices. New assets enter caches through nonblocking `poll`. The QuickJS plugin retains one runtime per scripted entity and cleans hosts on replacement, despawn, world load, and script errors.
- Measure core hot paths with the fixed external FBX scenes in `npm run benchmark`, and exercise orthographic, perspective, and protocol paths. One scene's results must not be extrapolated to other workloads.

## Current limitations

The plugin API, dynamic registration, and complete asset system do not exist yet. CPU/GPU baselines with about 1.98 million vertices, basic memory sampling via `testing/soak.cjs`, asynchronous loading, and cooked meshes exist, but a multi-scene memory matrix, textures, animation, asset packages, and link-time plugin pruning do not. SVG output in `render.rs` covers only 2D sprites.
