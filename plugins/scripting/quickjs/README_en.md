# QuickJS scripting plugin

[中文](README.md) | English

`mod.rs` provides a persistent QuickJS runtime per entity, turning JavaScript compiled from TypeScript into engine commands validated by Rust. It runs on the logic thread through `core.simulation`.

## Current behavior

- Source compiles once at host creation, and top-level state survives ticks.
- Pause/resume preserves state; replacement, errors, despawn, reset/load, and shutdown clean the host.
- CPU time, memory, stack, commands, logs, and result size are bounded.
- All scripts within one logic frame share a default 10ms wall-time budget (see below); over-budget scripts are skipped, not disabled.
- `Date` and `Math.random` are disabled to preserve deterministic engine data.
- World snapshots store source and resource paths, not the JavaScript heap.

QuickJS contexts and `Rc` state never cross threads; other systems interact through command buffers.

## Per-tick script time budget

All script hosts within one logic frame share a wall-time budget, 10ms by default, adjustable through `QuickJsScriptRuntime::with_tick_budget` / `set_tick_budget`. The budget accumulates the elapsed time of every `run_tick` call within the tick; once exhausted, remaining runnable entities are skipped for that tick rather than disabled: their hosts and `enabled` state stay untouched, they resume on the next tick, and each skipped entity is reported through a `ScriptError` event with code `script_budget_exceeded`.

The 10ms default follows from the roughly 16.7ms whole-frame budget at 60 FPS: scripting takes only a fraction of it, leaving headroom for the rest of the engine. The per-script 50ms `script_timeout` limit is unchanged; the budget only bounds the combined cost of all scripts within one tick. This is a structural guard against script volume overwhelming the frame; the repo holds no measurements for this budget and claims no performance improvement from it.

## Limitations

QuickJS remains a static built-in provider without a dynamic ABI, breakpoint debugging, hot-reload state migration, or an independent permission system. TypeScript must first be built to JavaScript by the project.
