# Official plugins

[中文](README.md) | English

`plugins/` is the taxonomy for officially maintained capabilities. These directories define ownership and dependency direction; **plugin manifests, a dynamic loader, a stable ABI, and automatic discovery are not implemented yet**. The first implementation stage uses static Rust composition. Create a concrete plugin directory only for a real implementation, not an empty placeholder.

## Categories

| Directory | Owns | Does not own |
|---|---|---|
| [`rendering/`](rendering/README_en.md) | Render pipelines, materials, lighting, shadows, and post-processing | Model parsing, editor controls, or platform packages |
| [`assets/`](assets/README_en.md) | File readers, import, conversion, asset metadata, and caches | Per-frame render decisions or game logic |
| [`gameplay/`](gameplay/README_en.md) | Runtime systems such as physics, animation, audio, input, UI, and networking | Core scheduling or platform export |
| [`scripting/`](scripting/README_en.md) | Script runtimes, language bindings, agent interfaces, debugging, and hot reload | Game-specific scripts or visual asset import |
| [`editor/`](editor/README_en.md) | Editor UI, hierarchy/inspectors, scene controls, and project operations | Runtime required by player builds |
| [`platforms/`](platforms/README_en.md) | Platform capabilities, dependency collection, builds, export, signing, and packages | Materials, game logic, or general editor controls |
| [`diagnostics/`](diagnostics/README_en.md) | Profiling, metrics, logs, replay, and debug views | Normal game mechanics or asset import |

A plugin has one primary category. Cross-category capabilities collaborate through public interfaces rather than copied implementations.

## DSH-style capability seams

A replaceable capability may define three roles:

- **Service definition**: Rust data types and traits used by consumers.
- **Service provider**: an algorithm, format, backend, or platform implementation.
- **Consumer**: the runtime, editor, or build flow using the capability.

A capability considers all three roles together, splitting packages only when they evolve independently; keep a single-purpose plugin simple. Consumers depend on definitions, not replaceable providers. New behavior attaches through services, typed events, and reversible effects rather than core-loop patches or global singletons.

Beyond functional categories, one plugin may contain `runtime`, `editor`, `build`, `developer`, and `test` modules to isolate player, editor, and build costs. Module metadata belongs to [`sdk/`](../sdk/README_en.md). Runtime environments use ordered bundles and patches from [`profiles/`](../profiles/README_en.md) instead of hardcoded entry-point plugin lists.

## Lifecycle direction

The [static host](../sdk/plugin-lifecycle_en.md) implements registration validation, disposal handles, startup rollback, reverse cleanup, and bounded synchronous work. Deactivation must not leave callbacks, GPU resources, threads, file watchers, or background work. Profile discovery and dynamic loading are absent, and required-capability failures must remain explicit.

Static registration comes first to avoid an unstable Rust dynamic-library ABI and cross-process hot-path costs. Future native dynamic plugins require explicit trust and version checks. Untrusted importers or tools should prefer process isolation so a crash cannot terminate the editor or player runtime.

## Official, third-party, and game-specific plugins

- This directory contains only officially maintained plugins.
- Reusable third-party plugins use independent repositories and versioned releases rather than copied engine source.
- Game-specific plugins belong to the game repository and are referenced by project configuration.
- Compatibility depends on engine version, interface version, and target platform, not the plugin name alone.

## Performance and stability requirements

- Disabled plugins must perform no per-frame CPU/GPU work and must not be included in player builds.
- Hot-path plugins must declare allocation, CPU/GPU, memory, and loading costs with representative measurements.
- Asynchronous work needs an owner, cancellation path, and bounded results; unbounded queues are not allowed.
- Plugin failures must not corrupt world snapshots or leave partial registrations.
- With an optional feature disabled, core behavior, snapshots, and performance must remain unchanged.

## Documentation for a new plugin

Place a concrete plugin at `plugins/<category>/<plugin-name>/` with both `README.md` and `README_en.md`. Once implemented, its README covers purpose, public interfaces, dependencies, configuration, lifecycle, performance costs, platform limitations, and verification. Category READMEs index responsibilities and direct children; detailed interfaces belong to the concrete plugin.
