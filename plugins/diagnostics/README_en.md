# Diagnostics plugins

[中文](README.md) | English

`diagnostics/` observes the engine without changing game semantics through profiling, CPU/GPU metrics, structured logs, memory and resource counts, deterministic replay capture, and debug views. It provides runtime measurement providers; fixed-scene benchmark execution and regression acceptance belong to [`testing/`](../../testing/README_en.md).

## Responsibility boundary

- Performance capture: simulation ticks, render submission, GPU passes, scripts, and asset import timing.
- Stability data: allocations, memory, handles, task queues, errors, and resource disposal.
- Replay and comparison: repeated measurements with fixed input, scene, version, and environment.
- Debug presentation: overlays, traces, and exported reports, with panels attached through [`editor/`](../editor/README_en.md).

Diagnostics must not change physics results, asset contents, or packaging rules; those behaviors belong to their owning categories.

## Initial official capabilities

Provide CPU frame-time, memory, and script-timing capture for fixed scenes, while [`testing/`](../../testing/README_en.md) runs benchmarks and decides regressions; add GPU timestamps afterward. Diagnostics retain raw samples rather than only averages.

## Performance and stability

- With diagnostics disabled by default, do not sample, format logs, or allocate per-frame records.
- Samplers need bounded buffers and low-cost writes so observer overhead does not hide real stutters.
- Reports include p50, p95, p99, peaks, sample counts, warmup, and environment; never report only the fastest single run.
- Replay input and snapshot versions must be explicit, with scene and configuration equality checked before comparison.
- Long tasks, file export, and remote transfer must not block simulation or rendering threads.

## Current limitations

`testing/` provides CPU benchmarks with fixed external FBX scenes and main/shadow-pass GPU timestamps sampled every 30 frames when supported. GPU sampling is enabled only for benchmarks. Pluggable metric registration, replay files, memory curves, and long-run stability tests remain absent, so current data does not establish overall performance goals.
