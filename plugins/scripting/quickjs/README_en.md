# QuickJS scripting plugin

[中文](README.md) | English

`mod.rs` provides a persistent QuickJS runtime per entity, turning JavaScript compiled from TypeScript into engine commands validated by Rust. It runs on the logic thread through `core.simulation`.

## Current behavior

- Source compiles once at host creation, and top-level state survives ticks.
- Pause/resume preserves state; replacement, errors, despawn, reset/load, and shutdown clean the host.
- CPU time, memory, stack, commands, logs, and result size are bounded.
- `Date` and `Math.random` are disabled to preserve deterministic engine data.
- World snapshots store source and resource paths, not the JavaScript heap.

QuickJS contexts and `Rc` state never cross threads; other systems interact through command buffers.

## Limitations

QuickJS remains a static built-in provider without a dynamic ABI, breakpoint debugging, hot-reload state migration, or an independent permission system. TypeScript must first be built to JavaScript by the project.
