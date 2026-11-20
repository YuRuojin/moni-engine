# Scripting and agent plugins

[中文](README.md) | English

`scripting/` owns script-language runtimes, engine API bindings, agent control interfaces, debugging, and hot reload. Humans and agents should share queryable, verifiable capability descriptions instead of agents receiving a separate rule-bypassing API.

## Responsibility boundary

- Language providers: persistent QuickJS or other runtime instances, isolation, and resource limits.
- API bindings: versioned commands, type declarations, events, and error results.
- Agent interfaces: capability discovery, structured calls, snapshots, and reproducible operations.
- Development tools: breakpoints, call diagnostics, script reload, and state migration.

Game-specific scripts remain in the game repository; profilers belong to [`diagnostics/`](../diagnostics/README_en.md), and editor script panels belong to [`editor/`](../editor/README_en.md). Plugin manifests, service definitions, and stable extension ABIs belong to [`sdk/`](../../sdk/README_en.md); this category owns script execution and game automation interfaces only.

## Initial official capabilities

Keep the current persistent per-entity QuickJS host and extract a language-independent script interface around it. TypeScript declarations, Rust commands, and protocol capability descriptions need one source of truth or consistency checks so agents never read stale APIs.

## Performance and stability

- Do not reconstruct the runtime, recompile unchanged scripts, or copy entity data without bounds for every simulation callback.
- Script execution needs CPU-time, memory, command, log, and result-size limits; violations must identify the responsible entity.
- Hot reload must define state retention, failure rollback, and event disposal without retaining duplicate callbacks.
- Agent commands pass through the same validation and permission rules as local scripts and cannot corrupt the world directly.
- QuickJS hosts and their `Rc` state have thread affinity and remain on their owning simulation thread; other Rust systems exchange command buffers rather than sharing script contexts across threads.
- Measure execution and serialization costs separately for startup, individual ticks, and long runs.

## Current limitations

The QuickJS provider now lives in [`quickjs/`](quickjs/README_en.md), while NDJSON remains in `src/protocol.rs`. Registration is static and has no independent ABI. Each scripted entity has a persistent context, evaluates top-level code once, retains JS state across pause/resume, and recreates the context when source is replaced. World snapshots do not persist the JS heap. Breakpoint debugging, hot-reload state migration, and a separate permission system remain unimplemented.
