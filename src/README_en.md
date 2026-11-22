# src: Composition-root binary

[中文](README.md) | English

`src/` is the composition root: the only CLI entry and the only place allowed to name concrete plugin types. Kernel facts — world data, command dispatch, the plugin host, and profile resolution — live in [`crates/moni-core/`](../crates/moni-core/); concrete game mechanics, editors, file-format importers, and platform packaging do not belong here.

## Current modules

| File/directory | Responsibility |
|---|---|
| `main.rs` | CLI subcommands, editor project detection/profile composition, profile listing, and workspace path restrictions |
| `bootstrap.rs` | Static plugin factory and `serve_session()`: the composition root wires a profile's plugins into a session |
| `client.rs` | `winit` window, redraw, catch-up stepping, and render-benchmark capture on top of `RuntimeSession` |
| `fuzz.rs` | Structured in-process parser fuzzing |
| [`crates/moni-core/src/engine.rs`](../crates/moni-core/README.md) | World, entities, commands, events, revisions, snapshot rollback, and fixed simulation ticks |
| [`crates/moni-core/src/session.rs`](../crates/moni-core/README.md) | `RuntimeSession`: the runtime path shared by client, editor, and agents, plus the idempotent request cache |
| [`crates/moni-core/src/host.rs`](../crates/moni-core/README.md) | Plugin host: dependency-scheduled systems (cached), access enforcement, service registry, plugin commands, and cascade unregistration |
| [`crates/moni-core/src/protocol.rs`](../crates/moni-core/README.md) | NDJSON protocol, atomic batches, dry runs, capability descriptions, and query/snapshot I/O |
| [`plugins/rendering/wgpu/`](../plugins/rendering/wgpu/README_en.md) | `wgpu` device, 2D/3D/shadow pipelines, FBX loading, model matrices, culling, and GPU timestamps |
| [`plugins/editor/scene-viewer/`](../plugins/editor/scene-viewer/README_en.md) | Editable scene editor (project detection, panels, play mode), camera controls through the command API, and performance/hierarchy overlay |
| [`plugins/scripting/quickjs/`](../plugins/scripting/quickjs/README_en.md) | Persistent QuickJS hosts, script contexts, limits, and command collection |
| [`profiles/`](../profiles/README.md) | Named profiles (including `serve`), bundles, and effective trees |

## Core boundary

`serve`, `client`, and `editor` share one `RuntimeSession`: one command dispatch, one plugin lifecycle, one revision and event semantics. GUI input (such as viewer camera moves) goes through the public command API, so human and agent scene edits stay interchangeable. The `serve` profile wires `core.simulation`, giving the agent path script execution and the plugin lifecycle instead of a bare engine that bypasses the host.

The kernel is a plugin host owning only shared entity identities, component/property registration, basic scene hierarchy and prefabs, serialization migrations, system scheduling, threading, time/randomness, and resource lifetimes. Replaceable behavior attaches through service definitions, events, and reversible effects; the core loop provides no privileged branches for concrete plugins.

Plugins are statically registered in Rust. The service registry, plugin command registration, atomic start-failure rollback, and cascade unregistration exist, but dynamic ABI, typed third-party component storage, and external registries are absent; future work must preserve protocol, snapshot, and thread constraints.

## Performance and stability

- World snapshots and events need clear ownership; plugins must not corrupt core state through undeclared access, which the host rejects at runtime.
- Simulation must remain reproducible. Expensive systems must not create unbounded catch-up or task queues.
- GPU resources should be reused and have deterministic release paths. Resizing and device loss must not cause continuing resource growth.
- The wgpu plugin reuses built-in mesh buffers, preloads FBX through an asynchronous worker during initialization, and updates per-entity model matrices and buffers each frame. New assets enter caches through nonblocking `poll`. The QuickJS plugin retains one runtime per scripted entity and cleans hosts on replacement, despawn, world load, and script errors.
- Measure core hot paths with the fixed external FBX scenes in `npm run benchmark`, and exercise orthographic, perspective, and protocol paths. One scene's results must not be extrapolated to other workloads.

## Current limitations

Dynamic registration and a complete asset system do not exist yet. CPU/GPU baselines with about 1.98 million vertices, basic memory sampling via `testing/soak.cjs`, asynchronous loading, and cooked meshes exist, but a multi-scene memory matrix, textures, animation, asset packages, and link-time plugin pruning do not. SVG output in `render.rs` covers only 2D sprites; the protocol `render` command still emits SVG and is not the windowed wgpu pipeline, so captures need the windowed client.
