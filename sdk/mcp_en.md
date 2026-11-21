# MCP Control-Plane Adapter (`moni mcp`)

English | [中文](mcp.md)

`moni mcp` exposes the engine control plane as a Model Context Protocol (MCP) stdio server. It is ONLY a thin adapter over the existing control plane: every mutating tool builds the same request an NDJSON client would send and routes it through `RuntimeSession::handle_line`, so revisions, batches, events, and error semantics stay identical to `moni serve` (see the protocol description in `moni capabilities`; this document does not repeat protocol details).

## Startup

```bash
moni mcp                 # Empty session: Engine + QuickJS + serve profile + plugin start
moni mcp --scene PATH    # Load a workspace-relative .mnsc scene after session build
moni mcp --project       # Load the current .mngp project's scene (mutually exclusive with --scene)
```

- Transport: MCP stdio (JSON-RPC 2.0, line-delimited framing) carried by the official Rust SDK `rmcp` 3.11.0 (pinned exactly); tokio exists only inside `src/mcp.rs`, and engine code stays single-threaded and synchronous.
- Session: one `RuntimeSession` per server lifetime (`bootstrap::mcp_session`), built and used on a dedicated engine thread; tool calls enter that thread one at a time through a job channel, matching MCP's sequential stdio semantics. The server quits cleanly on stdin EOF.

## Tools (first batch)

| Tool | Side effects | Description |
|---|---|---|
| `capabilities` | Read-only | Static protocol capabilities plus this session's overlay (profile, plugins, services, plugin commands, component registry) |
| `scene_info` | Read-only | Entity count with an id/name/component list summary, revision, tick and tick rate, camera and lights |
| `scene_load` | Mutates world | `RuntimeSession::load_scene` (the same validation path as the CLI and editor); success replaces the world, revision, and script hosts, failure leaves the session untouched |
| `scene_save` | Writes a file | Protocol `save`: writes a world-snapshot JSON (**not** the `.mnsc` scene format); `path` may be omitted to fall back to the session scene |
| `entity_spawn` | Mutates world | Protocol `spawn` with a full EntitySpec; supports `expectedRevision` |
| `entity_despawn` | Mutates world | Protocol `despawn`; supports `expectedRevision` |
| `entity_set_transform` | Mutates world | Protocol `set_transform`; supports `expectedRevision` |
| `component_attach` | Mutates world | Protocol `attach_component`; schema defaults are applied before validation; supports `expectedRevision` |
| `component_remove` | Mutates world | Protocol `remove_component`; supports `expectedRevision` |
| `step` | Mutates world + scripts | Protocol `step` (never inside a batch); script hosts run onTick every tick and script state cannot be rolled back; repeated calls tick again — not idempotent |
| `render_svg` | May write a file | Protocol `render`: the diagnostic 2D/3D SVG wireframe, **not** the GPU frame; with `path` writes a file, without one returns the SVG inline |
| `screenshot` | Writes a file, opens a window | Child process `moni editor --scene <s> --frames <n> --golden <out>` (the byte-stable golden PNG capture verified in-repo); needs a GPU and a window station and fails headless by design |
| `run_client` | Opens a window | Child process `moni client --scene 2d\|3d` or `--world <workspace path>` renders `frames` frames and returns the final JSON the client prints; bundled demo scenes require `npm run build` |
| `diagnostics` | Read-only (drains events) | The capabilities session block, revision, event watermark, script host count, and the last drained events (up to 50; draining consumes buffered events) |

Tool descriptions and MCP annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`) state side effects honestly: mutating tools are non-read-only, `step` declares script effects, `screenshot`/`run_client` declare a real window, and `render_svg` declares how it differs from the GPU frame.

## Semantic guarantees

- **Thin adapter**: `entity_*`, `component_*`, `step`, `scene_save`, `render_svg`, and `capabilities` all go through `session.handle_line`; `scene_load` goes through `RuntimeSession::load_scene` (protocol `load` loads world snapshots instead of `.mnsc` scenes). The tool result is the full protocol response JSON (`ok`/`result`/`error`/`events`/`revision`/`eventSeq`), not a rewrapped shape.
- **Error fidelity**: a failed response lands whole in the MCP tool result's `structuredContent` with `isError=true`, preserving the protocol's structured error `{code, message, details}` with no lossy stringification. Malformed tool arguments use the same vocabulary (`invalid_arguments`).
- **Revision concurrency**: mutating tools accept `expectedRevision`; mismatches fail with `revision_conflict` before any change. The `revision` in a read result feeds directly into the next mutating call's `expectedRevision`.
- **Idempotency and replay**: the NDJSON protocol's request-ID replay cache stays session-owned; MCP tool calls generate a fresh internal request ID each time, so identical repeated calls execute again instead of replaying a cached response. Agents that need replay/idempotency semantics should use `moni serve` directly.

## Current limitations

- One session per server lifetime; no cross-process sharing or persistence.
- All paths (scenes, outputs) must be workspace-relative; escapes are rejected.
- `screenshot` and `run_client` depend on a real GPU window and fail headless; screenshots use the editor's golden capture, not the live frame of this session.
- `scene_save` writes a world-snapshot JSON; `.mnsc` scene saving is currently only available through the editor path.
- The server implements no MCP resources/prompts; the first batch exposes tools only.
