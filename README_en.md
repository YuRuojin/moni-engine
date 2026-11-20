# Moni Engine

[中文](README.md) | English

Moni Engine is a lightweight 2D/3D engine kernel written in Rust. Its goal is a low-cost, stable game runtime that developers and agents can customize deeply. The architecture moves toward a small core, pluggable capabilities, and independent game repositories. It is at an early stage: the plugin loader, full visual editor, and multi-platform exporters are not implemented yet.

## Currently runnable

- 2D rectangles, circles, transforms, layers, and SVG output.
- Basic 3D cubes, planes, and external FBX meshes with orthographic/perspective cameras, depth testing, directional lighting, and material base colors.
- `.mnsc` v2 with v1 migration, component/property registration, unknown-data preservation, and agent-readable schemas.
- Directional/point lights, basic materials, and directional shadows.
- An NDJSON command protocol, world snapshots, fixed simulation ticks, and persistent QuickJS scripting.
- A single-thread static plugin host with dependency/access validation, rollback, and reverse cleanup.
- A read-only scene viewer, CPU benchmarks, and main/shadow-pass GPU timestamps.
- TypeScript declarations and 2D/3D examples.

```bash
npm install
npm run demo
npm run demo:3d
```

## Reading path for agents

Choose the directory that owns the task and read its README before loading more of the repository.

| Directory | Responsibility | Read it when |
|---|---|---|
| [`src/`](src/README_en.md) | Minimal runtime kernel, protocol, windowing, and the current built-in renderer | Changing the core loop, data model, GPU device, or protocol |
| [`sdk/`](sdk/README_en.md) | Service definitions, plugin lifecycle, manifests, permissions, and migrations | Changing public plugin contracts |
| [`plugins/`](plugins/README_en.md) | Official plugin taxonomy, dependency rules, and capability seams | Adding or changing a pluggable capability |
| [`plugins/rendering/`](plugins/rendering/README_en.md) | Materials, lighting, shadows, post-processing, and render pipelines | Changing visuals or render architecture |
| [`plugins/assets/`](plugins/assets/README_en.md) | File readers, asset import, runtime loading, and caching | Adding model, texture, audio, or other formats |
| [`plugins/gameplay/`](plugins/gameplay/README_en.md) | Physics, animation, audio, input, UI, networking, and other game systems | Adding reusable game mechanics |
| [`plugins/scripting/`](plugins/scripting/README_en.md) | Script languages, agent control, debugging, and hot reload | Changing scripting or automation interfaces |
| [`plugins/editor/`](plugins/editor/README_en.md) | Visual editor, controls, and project operations | Building the editing interface |
| [`plugins/platforms/`](plugins/platforms/README_en.md) | Platform support, builds, exports, and installable packages | Adding Windows, Linux, HarmonyOS, or other targets |
| [`plugins/diagnostics/`](plugins/diagnostics/README_en.md) | Profiling, logging, replay, and diagnostics | Providing runtime measurement and diagnostics |
| [`profiles/`](profiles/README_en.md) | Named profiles, reusable bundles, and ordered patches | Changing plugin composition for a runtime environment |
| [`tools/`](tools/README_en.md) | Engine Hub, project tools, plugin installation, and dependency resolution | Changing installation, projects, or package management |
| [`templates/`](templates/README_en.md) | Independent game repository templates | Changing initial project contents |
| [`testing/`](testing/README_en.md) | Real-composition, compatibility, rendering, and performance acceptance | Adding tests or regression requirements |
| [`examples/`](examples/README_en.md) | Runnable script examples | Exercising the existing client |

## Engine and game repository boundary

This repository contains the engine kernel, SDK, official plugins, compositions, tools, templates, and tests. Creating a game must not require copying or modifying engine source.

The target workflow uses an installed engine version to create an independent game repository:

```text
my-game/
  moni.toml          # Planned engine, plugin, and build configuration
  assets/            # Source assets and import metadata
  scenes/            # Scenes and entity configuration
  scripts/           # Game logic
  plugins/           # Optional game-specific plugins
```

`moni new`, project templates, and version resolution are not implemented yet. Reusable third-party plugins also belong in their own repositories and are installed by compatible version. Only officially maintained plugins live under `plugins/` in this repository.

## Performance and stability requirements

Performance and stability are acceptance requirements for default-enabled features, not cleanup work after feature completion.

- Measure CPU and GPU frame time separately, including p95, p99, and stutters rather than average frame rate alone.
- Track steady-state memory, asset loading time, startup time, package size, and resource growth during long runs.
- Benchmarks use release builds, fixed scenes and simulation input, warmup, and a reproducible environment.
- A disabled plugin must add no per-frame CPU/GPU work. Activation, deactivation, and unloading must not leak resources or background work.
- Hot paths must avoid unbounded allocation, blocking file I/O, synchronous GPU readbacks, and cross-process calls.
- Do not claim better performance or lower cost than Godot, Unity, Unreal Engine, or another implementation without benchmark data.

Release CPU and GPU baselines are available through `npm run benchmark`, with reports under `target/benchmarks/<label>/`. Benchmarks load two FBX files and orthographic/perspective `.mnsc` scenes from `testing/assets/`, totaling about 1.98 million vertices. FBX vertices and model matrices are reused on the GPU, QuickJS persists per scripted entity, and main/shadow passes are sampled every 30 frames when supported. Memory curves and a multi-scene matrix remain uncovered, so these results do not establish overall performance goals.

## Iteration order

| Stage | Deliverables | Exit condition |
|---|---|---|
| A. Current kernel | Existing 2D, basic 3D, protocol, and scripting | Real rendering and protocol calls run successfully |
| B. Plugin foundation | Performance baseline, service definitions/providers/consumers, reversible lifecycle, versioned manifests, profiles/bundles/patches, and project lockfiles/templates | Repeatable measurements, safe real-profile composition and disposal, and no game dependency on engine source paths |
| C. Content and visuals | Asset import, models/textures, materials, lighting, shadows, and replaceable pipelines | Measured large-scene performance and stable imports |
| D. Game production | Game systems, script debugging/hot reload, and the visual editor | The editor adds no cost to player builds |
| E. Publishing | Platform exports, dependency collection, signing, and reproducible packaging | Every target passes testing with its actual installable artifact |

## Documentation convention

Each responsibility directory keeps Chinese in `README.md` and English in `README_en.md`; update both together. A README covers its own responsibilities, interfaces, performance requirements, current limitations, and direct children. Keep implementation detail in the lowest owning directory. Mark planned capabilities explicitly and never present them as supported features.

See [LICENSE](LICENSE).
