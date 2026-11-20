# Testing and acceptance

[中文](README.md) | English

`testing/` provides repeatable verification for the engine, plugins, templates, and release artifacts, plus a reusable compatibility kit for third-party plugins. It consumes diagnostics and makes acceptance decisions; runtime metric collection belongs to [`plugins/diagnostics/`](../plugins/diagnostics/README_en.md).

## Verification scope

- Core: protocol, snapshots, fixed simulation ticks, serialization compatibility, and migrations.
- Real composition: boot complete profiles and ordered patches through the loader rather than substituting manual registrations for the product composition.
- Plugin lifecycle: repeated registration, activation, deactivation, failure rollback, hot reload, effect disposal, and leaks.
- Plugin contracts: interface versions, permissions, dependency conflicts, and target filtering.
- Content: importer fuzzing, malformed files, cache consistency, and asset hot reload.
- Rendering: golden images for fixed scenes, GPU validation, and device/window changes.
- Publishing: template creation, dependency locking, actual package startup, and target-platform artifacts.
- Performance: benchmark runs with fixed scenes and input, quantile comparisons, memory growth, and regression reports.

Profiles and bundles belong to [`profiles/`](../profiles/README_en.md), templates to [`templates/`](../templates/README_en.md), and actual exporters to [`plugins/platforms/`](../plugins/platforms/README_en.md).

## Third-party test kit

Once the SDK stabilizes, external plugin repositories should reuse lifecycle, serialization, permission, and target compatibility tests without copying internal engine tests. Failures identify the plugin, execution domain, target, and actionable reason.

## Performance and stability

- Performance tests use release builds, fixed scenes and input, warmup, sufficient samples, and a documented environment.
- Raw results include p50, p95, p99, peaks, sample counts, and memory curves rather than average frame rate alone.
- Rendering comparisons use explicit platform tolerances without hiding real regressions behind broad allowances.
- Tests independently clean up windows, GPUs, processes, temporary directories, and background work with repeatable results.

## Current performance benchmark

```bash
npm run benchmark -- --label baseline --frames 600 --warmup 120
```

The command builds a release binary, loads two FBX files and orthographic/perspective `.mnsc` scenes from `testing/assets/`, runs simulation and real window rendering, and writes raw millisecond samples plus p50/p95/p99 under `target/benchmarks/baseline/`. Simulation measures `Engine::step(1)` and event draining. Total render time includes present/vsync waits; preparation separately covers vertices, uniforms, model matrices, and vertex-buffer operations. When supported, main/shadow-pass GPU timestamps are sampled every 30 frames; synchronous readback is used only for benchmarks.

## Plugin host verification

The real client and viewer start, tick, and render through the static host, and the simulation plugin is disposed through a `PluginHandle`. Shutdown cleans QuickJS and GPU resources. Cross-stage startup dependencies, rendering, and release benchmarks have been exercised.

## Scene and snapshot verification

`cargo run -- scene <INPUT.mnsc> --output <OUTPUT.mnsc>` performs scene migration and round-trip serialization. Current checks cover v1→v2, repeated round-trip consistency, nested unknown-component preservation, script paths, snapshot v1 upgrades, and `capabilities.components` queries.

## Current limitations

There is no formal unit test suite, plugin compatibility kit, golden rendering, or importer fuzzing; `cargo test` currently proves only that the test target compiles. CPU and supported-GPU baselines exist, but memory curves, a scene matrix, and cross-platform benchmarks do not.
