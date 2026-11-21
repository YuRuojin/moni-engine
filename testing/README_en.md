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

The command builds a release binary, first makes sure `testing/assets/benchmark.mnec` exists (a ~1.98M-vertex cooked mesh generated deterministically by [`generate-fixture.cjs`](generate-fixture.cjs), kept out of Git), then loads the committed [`benchmark-2d.mnsc`](assets/benchmark-2d.mnsc) and [`benchmark-3d.mnsc`](assets/benchmark-3d.mnsc) scenes, runs simulation and real window rendering, and writes raw millisecond samples plus p50/p95/p99 under `target/benchmarks/baseline/`. Simulation measures `Engine::step(1)` and event draining. Total render time includes present/vsync waits; preparation separately covers vertices, uniforms, model matrices, and vertex-buffer operations. When supported, main/shadow-pass GPU timestamps are sampled every 30 frames; synchronous readback is used only for benchmarks.

## CPU baseline

The windowless CPU baseline is collected with the `benchmark` subcommand, which measures simulation ticks only:

```bash
cargo run --release -- benchmark --scene 2d --frames 600 --warmup 60 --output target/benchmarks/baseline-2d-cpu.json
cargo run --release -- benchmark --scene 3d --frames 600 --warmup 60 --output target/benchmarks/baseline-3d-cpu.json
```

This path loads the committed benchmark scenes and runs `Engine::step(1)` plus event draining on a bare `Engine` (report metric `engine_step_and_event_drain`): no window, GPU, plugin host, or mesh import. The 2D scene steps 25 sprite entities and the 3D scene steps 1 model entity; the scenes contain no script entities, so the attached QuickJS runtime executes no scripts, and the 3D scene's terrain alias is data only — `benchmark.mnec` is never read on this path.

The current baseline was recorded on 2026-11-21 on a Windows x86_64 development machine (release build of the working tree at commit d27f6cf), with 600 frames, 60 warmup frames, and 540 samples each:

| Scene | p50 (ms) | p95 (ms) | p99 (ms) | max (ms) |
|---|---|---|---|---|
| 2d | 0.0004 | 0.0004 | 0.0004 | 0.0004 |
| 3d | 0.0002 | 0.0003 | 0.0003 | 0.0003 |

These values are strongly machine- and timer-dependent (Windows QPC granularity is ~100ns and samples are quantized to 0.0001ms steps; current tick work sits below timer resolution). They only indicate the order of magnitude of an idle simulation tick and must not be compared across machines. Regression threshold proposal: rerun the same command on the same machine and flag for review when p95 regresses more than 20% against this baseline. CI does NOT enforce this threshold — GitHub-hosted runners have different hardware, so the baseline is a local regression gate only.

## Asset and plugin-directory verification

Asynchronous worker preload, nonblocking runtime collection, cooked conversion, and `.mnec` scene loading are verified. The benchmark scene loads the generated 1,975,083-vertex cooked mesh. FBX, wgpu, QuickJS, and scene-viewer implementations now live in their official plugin directories.

## Independent project verification

`node testing/template-e2e.cjs` runs the full acceptance flow: `moni new` creates an independent Git project, verifies structure and lockfile, confirms the repository references no absolute engine paths, `doctor` and `profile dump` pass inside the project, `moni run --frames 3` completes `npm ci`, the TypeScript build, and a real windowed run, and the scene saves back as a v2 `.mnsc`. Set `MONI_E2E_SKIP_RUN=1` to skip the windowed run and pass `--keep` to inspect the project afterwards. Doctor rejects a modified lockfile, then `doctor --update-lock` repairs it and verification passes again.

## Profile composition verification

Effective trees and real instantiation are exercised for `default`, `editor`, `minimal`, and `headless`. Minimal/headless initialize no GPU, QuickJS, or FBX; patches remove default plugins, and an invalid default-headless combination fails explicitly. Release benchmarks remain in the existing range.

## Plugin host verification

The real client and viewer start, tick, and render through the static host, and the simulation plugin is disposed through a `PluginHandle`. Shutdown cleans QuickJS and GPU resources. Cross-stage startup dependencies, rendering, and release benchmarks have been exercised.

## Scene and snapshot verification

`cargo run -- scene <INPUT.mnsc> --output <OUTPUT.mnsc>` performs scene migration and round-trip serialization. Current checks cover v1→v2, repeated round-trip consistency, nested unknown-component preservation, script paths, snapshot v1 upgrades, and `capabilities.components` queries.

## Compatibility, golden, and stability tools

```bash
npm run test:compat
npm run test:soak
```

`test:compat` covers manifest/API contracts, profile order, headless/minimal, permission denial, scene migration, corrupt FBX process isolation, and identical pixels across two golden captures. `test:soak` runs 1200 frames by default and samples process working set on Windows; raw output is `target/testing/soak-report.json`.

Golden captures use raw RGBA pixels and JSON metadata, without a cross-GPU tolerance map. Corrupt-input checks are fixed smoke cases rather than continuous fuzzing. The 1200-frame run is a stability smoke test, not long-run release certification.

## CI gate

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs two jobs on push and pull requests to main:

- ubuntu full gate: `cargo fmt --all --check`, `cargo test --workspace`, `npm ci` + `npm run check`, `cargo build`, the display-free subset of the compatibility checks (capabilities contract, default profile dependency order, headless/minimal forbidden-provider guard, scene v2 round-trip, corrupt-input process isolation, fuzz smoke), and a windowless template E2E (`new` → `doctor` → in-project TypeScript build → scene round-trip → `headless`).
- windows test parity: `cargo test --workspace`. The primary development platform is Windows, so CI mirrors it for compile/test parity without duplicating the npm gate.

GitHub-hosted runners have no display or GPU: permission denial (the winit event loop is created before the permission check fires) and golden rendering (opens the editor viewer window) cannot run there, so the full `npm run test:compat` and `test:soak` remain acceptance gates on local GPU machines.

## Current limitations

`cargo test` provides targeted unit tests (profile instantiation rollback, permission grant/revoke, lockfile round-trip and rollback, path validation, the color parse cache, and template initialization with path isolation), and the template flow is covered end to end by `template-e2e.cjs` and CI's windowless template E2E; a full third-party certification service does not exist yet. CPU/GPU benchmarks, golden captures, and memory sampling exist, but a scene matrix and cross-platform benchmarks do not; CI covers only the display-free paths, and display-dependent acceptance stays local.
