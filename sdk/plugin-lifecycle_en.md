# Static plugin host and lifecycle

[中文](plugin-lifecycle.md) | English

`crates/moni-core/src/host.rs` provides the single-thread reference scheduler and the service registry, `crates/moni-core/src/plugins.rs` provides `core.simulation`, and `plugins/rendering/wgpu/plugin.rs` provides `core.render`. This stage has no dynamic-library ABI, threaded task executor, or unbounded background queue.

## System descriptors

Each `SystemDescriptor` declares:

- `id` and `plugin_id`
- `stage`: `startup`, `fixed_update`, `render`, or `shutdown`
- `affinity`: `Logic` or `Render`
- `after`: dependencies within one stage or across stages
- `access`: `Read` or `Write` access to `World`, `Scripts`, `Gpu`, or `Assets`

Dependencies must exist and be acyclic. Conflicting reads/writes of one resource require an explicit dependency order. Cross-stage dependencies are allowed, but a stage executes only its own systems and does not repeat startup work.

Access declarations are enforced by the host at runtime: `PluginContext` no longer exposes the whole `&mut Engine`; `world()`, `world_mut()`, `run_ticks()`, and `shutdown_scripts()` each require the corresponding declared access and fail with `PluginError` otherwise. `start`/`stop` are trusted lifecycle calls and receive full access. Native static plugins remain trusted code; permissions and access declarations express intent, auditing, and dependency checks — they are not a sandbox.

## Two-phase registration and rollback

The plugin lifecycle has two phases. Registration (`PluginHost::register_plugin`): validate the plugin ID, grant permissions, then call `Plugin::register`. The plugin contributes component schemas, services, control commands, and migrators through `PluginRegistry`; contributions must not depend on a loaded World. The host records every contribution per plugin, and a failure mid-registration revokes everything the plugin recorded plus its permissions, so registration is all-or-nothing per plugin. Contributions take effect immediately after registration — scenes can therefore be loaded and validated against the effective registry before startup — but the plugin's systems are not scheduled, and shutdown will not call its `stop`.

Startup (`PluginHost::start_plugin`): validate and add the system declarations, snapshot the engine, then call `Plugin::start` with full access. On startup failure the host calls `stop` first, then restores the snapshot: world contents, the event stream, and the revision return to their pre-start state, services provided and commands registered during the failed start are removed together with the registration-phase contributions (components, services, commands, migrators), and permissions plus system declarations are revoked. The plugin instance stays registered for disposal; retrying requires unregistering and re-registering, which re-runs `register`.

`register_plugin_with_permissions` is the documented combined path: register, then start. Any failure restores the host to its pre-call state (everything added during the call is unregistered in reverse order), so simple callers keep single-call semantics.

## Service registry and plugin commands

Plugins publish name-addressed, strongly typed services through `PluginRegistry::register_service` in the registration phase or `PluginContext::provide_service` in `start`; consumers fetch them with `PluginContext::service::<T>`, which records the dependency automatically. Consumers never depend on a concrete providing plugin.

- Removing a provider first cascade-stops the plugins consuming its services (in reverse registration order), then removes the provider and its contributions; `unregister` returns the list of plugin IDs actually removed.
- Removing a consumer releases its claims, after which the provider can be removed directly.
- Duplicate service names, unknown services, and type mismatches fail explicitly.

Plugins can also contribute control-protocol commands (with JSON schemas) via `PluginRegistry::register_command` (registration phase) or `PluginContext::register_command` (startup phase); the host removes a plugin's commands when it unregisters or rolls back, `PluginHost::execute_plugin_command` is the entry point, and schemas surface through capabilities.

Every control command carries unified `CommandDescriptor` metadata (defined in `host.rs`): `owner` (`"core"` for built-ins, filled with the registering plugin's ID for plugin commands), `mutates_world`, `supports_batch` (`false` ⇒ rejected inside atomic batches with `invalid_batch_command`), `supports_dry_run` (`false` ⇒ rejected inside dry-run batches with `dry_run_unsupported`), `side_effects` (a `file`/`gpu`/`script`/`network` declaration, not a sandbox), `required_permission`, `args_schema`, and `returns_schema`. The three-argument `register_command(name, schema, handler)` uses conservative defaults: `mutates_world = true`, no batch support, no dry-run support, no side effects, no permission — plugin commands are conservative by default; declare capabilities explicitly. Use `register_command_with(descriptor, name, handler)` (available in both phases) for full metadata. Plugin commands whose name collides with a built-in command are refused at registration with a clear error; built-ins cannot be shadowed.

A declared `required_permission` is enforced at execute time against the OWNER plugin's granted permissions (same semantics as `PluginContext::require`); failure returns an error with code `permission_denied`. Command resolution is by name FIRST: a registered plugin command always executes and its structured error is returned unchanged — failures are never masked as `unknown_command` by falling through to built-in dispatch. Batch and dry-run admission consult the descriptors too (built-ins via the static table, plugin commands via their registered descriptor); a plugin command declaring dry-run support executes against the scratch world during dry runs — its handler receives only the scratch engine, so host-managed runtime state (scripts, services) does not participate in the preview.

The capabilities `commands` section is generated from descriptors: built-ins come from the static table plus the session's plugin commands; each entry carries `args`, `returns` (when documented), `mutatesWorld`, `batch`, `dryRun`, `sideEffects`, `permission` (when set), and `owner`. `session.pluginCommands` uses the same shape.

The registration phase can record per-component version migration steps through `PluginRegistry::register_migrator` (the component must be registered by the same `register` call first); the host removes a plugin's migrators on unregister or rollback.

## Stages and shutdown

The single-thread scheduler executes systems in topological order; only started plugins own systems. Validation and ordering results are cached per stage; registration, startup, or removal invalidates them lazily, so hot stages stop re-validating every frame. `shutdown` runs in reverse, then started plugins stop in reverse registration order — registered-but-unstarted plugins are dropped without lifecycle calls — before contributions, services, commands, and plugin registrations are cleared. `PluginHost::drop` follows the same path.

`core.simulation` advances fixed ticks in the logic stage and cleans QuickJS hosts on stop. `core.render` registers without a window, attaches one via `set_window` once the surface exists, creates and uses GPU resources in the startup phase, and releases them on stop. The client unloads the simulation plugin through the real unregister path.

## Session and control plane

The composition root assembles in a fixed order: resolve the profile, construct plugins and run the registration phase, load and validate the scene against the effective registry, start runtime systems (the windowed `core.render` and friends), then enter the shared session. `register_profile` performs only the registration phase; `start_all` starts handles in order, and `instantiate` is the combined register+start path with full rollback.

`RuntimeSession` in `crates/moni-core/src/session.rs` is the single runtime path shared by the windowed client, the editor, and the NDJSON server: one owner of the plugin host, world, and profile, with all commands routed through one dispatch whose responses carry the revision and event-sequence watermark; `RuntimeSession::load_scene` validates dynamic components against the effective registry and applies migration chains. Batch and single-command `expectedRevision` optimistic concurrency, atomic batch rollback, `dryRun` previews, and idempotent request IDs are documented in the protocol capabilities' `transactions` section; a single command whose `expectedRevision` mismatches fails with `revision_conflict` before any execution, with no side effects, no revision change, and no events, and that response participates in the idempotency cache like any other.

## Bounded work and errors

A single stage is limited to 1024 systems. Execution failure aborts the stage and returns `PluginError`; no work continues after unregister. Client catch-up remains capped at 5 simulation ticks per frame, with independent script callback limits.

Future parallel tasks must retain the same dependency, access, thread-affinity, and cancellation rules instead of accessing the shared world or GPU outside the scheduler.
