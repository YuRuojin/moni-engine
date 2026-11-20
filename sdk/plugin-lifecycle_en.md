# Static plugin host and lifecycle

[中文](plugin-lifecycle.md) | English

`src/host.rs` provides the single-thread reference scheduler, and `src/plugins.rs` provides the current static plugins. This stage has no dynamic-library ABI, threaded task executor, or unbounded background queue.

## System descriptors

Each `SystemDescriptor` declares:

- `id` and `plugin_id`
- `stage`: `startup`, `fixed_update`, `render`, or `shutdown`
- `affinity`: `Logic` or `Render`
- `after`: dependencies within one stage or across stages
- `access`: `Read` or `Write` access to `World`, `Scripts`, `Gpu`, or `Assets`

Dependencies must exist and be acyclic. Conflicting reads/writes of one resource require an explicit dependency order. Cross-stage dependencies are allowed, but a stage executes only its own systems and does not repeat startup work.

## Registration and rollback

`PluginHost::register_plugin` validates the plugin and systems before calling `Plugin::start`, then returns a `PluginHandle`. Startup failure removes the new systems and calls `stop` to clean partial initialization without retaining a partial plugin.

`PluginHandle::dispose` or `PluginHost::unregister` removes systems, cancels queued work, and calls `stop`. Duplicate plugin/system IDs, unknown dependencies, invalid thread affinity, and unordered resource conflicts fail explicitly.

## Stages and shutdown

The single-thread scheduler executes systems in topological order. `shutdown` runs in reverse, then plugins stop in reverse registration order before queues and registrations are cleared. `PluginHost::drop` follows the same path.

`core.simulation` advances fixed ticks in the logic stage and cleans QuickJS hosts on stop. `core.render` creates and uses GPU resources in startup/render and releases them on stop. The client unloads the simulation plugin through a real `PluginHandle`.

## Bounded work and errors

The synchronous per-stage task queue is limited to 1024 entries. Execution failure clears pending work and returns `PluginError`; no queued work continues after unregister. Client catch-up remains capped at 5 simulation ticks per frame, with independent script callback limits.

Future parallel tasks must retain the same dependency, access, thread-affinity, and cancellation rules instead of accessing the shared world or GPU outside the scheduler.
