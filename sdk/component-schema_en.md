# Component and property schemas

[中文](component-schema.md) | English

`crates/moni-core/src/components.rs` defines `ComponentRegistry`, `ComponentDescriptor`, `PropertySchema`, and `PropertyField`; the `Engine` (`crates/moni-core/src/engine.rs`) owns the session-level component registry and the per-component migrator chains, and registers the built-ins at construction. Dynamic (schema-only) components can be registered at runtime through the Rust API; automatic registration via the plugin loader is not implemented.

## Component descriptors

Each `ComponentDescriptor` contains:

- `id`: the namespaced ID used by the current version, such as `core.model`.
- `legacy_id`: the compatibility ID used by v1, such as `model`.
- `owner`: the component owner, `moni.core` for built-ins.
- `version`: the component schema version.
- `schema`: field definitions, defaults, validation, and editor metadata.

Descriptors come in two kinds:

- Built-in components carry a `deserialize`/`serialize` mapping into the `ComponentValue` enum; that mapping is a private field and only core components can construct it.
- Dynamic components are constructed with `ComponentDescriptor::dynamic(...)`, have no value mapping; their instances are stored as JSON and validated purely through `PropertySchema` (defaults plus field checks). `is_dynamic()` distinguishes the kinds.

Duplicate component or legacy IDs are rejected. Consumers resolve components through the registry instead of depending on concrete providers.

## Dynamic components

`Engine::register_component(descriptor)` registers a descriptor in the session registry; it becomes attachable immediately and emits `component_registered`. Registration and migrators are transactional session state: `Engine::snapshot()` captures them and `restore()` reverts registrations and migrator chains, so a rolled-back batch or failed plugin start leaves no registration trace. Dynamic component data itself travels inside `Entity.extensions`.

Dynamic component instances are stored under the component ID key in `Entity.extensions`. Protocol commands:

- `attach_component` (args `entity`, `component`, `value`): resolve the descriptor, apply schema defaults, validate through the schema, store into extensions, and emit `component_attached`. Attaching again replaces the previous value.
- `remove_component` (args `entity`, `component`): removes the extensions entry and emits `component_removed`.

Error semantics: unknown component IDs return `unknown_component`; built-in IDs such as `core.*` live in typed entity fields and return `component_reserved`, so they cannot be attached or removed through these commands; a failed schema validation returns `invalid_component_value` with the concrete field error in the message, and nothing is written to extensions, no event is emitted, and the revision is left unchanged. When the session load preserved future-version instances of a component, `attach_component` writes for it are refused with `component_future_version` (details `{ component, sceneVersion, registryVersion, futureInstances }`); removing a future instance is not restricted (the retention rules and read-only semantics are defined by "Session loading" in the [scene format](scene-format_en.md)).

`capabilities.components` projects the session engine's registry, so dynamic components appear automatically with their `x-component` (`owner`/`version`) metadata instead of a separately maintained catalog.

## Revocation

`Engine::unregister_component(id, force)`:

- Refuses while any entity still carries the component (`component_in_use`), reporting the entity count and entity IDs in the message and error details.
- With `force = true` it strips the component from every entity first (emitting `component_removed` per entity) and then unregisters.
- Built-in components are always refused (`component_reserved`).
- On success it also drops the component's migrator chain and emits `component_unregistered`.

After revocation the component ID stops resolving immediately and `attach_component` returns `unknown_component`; the same ID can be registered again afterwards, but the old migrator chain is not restored.

## Version migration

Dynamic component schema versions are bridged by migrator chains. `ComponentMigrator` is a type alias for a single migration step: `Box<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send>`, converting a component value from one schema version into its equivalent at the next version; the `String` error explains why the value cannot be migrated.

`Engine::register_component_migrator(component, from_version, to_version, migrate)` registers one step:

- The component must be registered and dynamic; unregistered IDs return `unknown_component`, `core.*` returns `component_reserved` (core components are migrated by the scene format itself).
- `to_version` must be greater than `from_version` and must not exceed the registered component version (`invalid_migration_step`).
- One component can have only one step per `from_version` (`duplicate_migrator`).
- A successful registration bumps the revision and emits no event.

`Engine::migrate_component_value(component, value, from, to)` walks the chain:

- `from == to` returns the value unchanged; downgrades return `invalid_migration_step`.
- A missing chain step returns `missing_migrator` with details `{ component, from, to, at }`, where `at` is the version the chain stopped at.
- A failing step returns `migration_failed` with details `{ component, from, to, cause }`.

`Engine::unregister_component_migrators(component)` removes every migration step of the component without touching its registration; it bumps the revision when a chain was actually removed and is a no-op otherwise. Migrators, like the registry, are captured by snapshots and reverted by restores.

## Property definitions

A `PropertyField` describes its name, type, required status, default, numeric range, length limit, display label, and description; the `PropertyField::new` constructor and the `default`/`range`/`max_length`/`label`/`description` builders are public, and `PropertySchema::new` assembles a field set. `PropertySchema` applies defaults, validates fields and unknown keys, and produces JSON Schema-style agent output with `x-editor` and `x-component` metadata.

`capabilities.components` exposes the current session registry. Rust commands, scene parsing, and agent queries therefore share one source of component metadata.

## Built-in components

- `core.transform`
- `core.sprite`
- `core.model`
- `core.script`

Built-in components are stored in typed entity fields (`transform`/`sprite`/`model`/`script`) rather than extensions, so `attach_component` and `remove_component` clearly refuse these IDs, and `unregister_component` always refuses them.

`.mnsc` v1 uses IDs without namespaces; v2 uses the `core.*` IDs above. A script component uses `source` for an external JavaScript path or `inline` for inline source, with exactly one present.

## Unknown data

Unregistered component keys are preserved in `Entity.extensions` instead of being executed as built-ins. Root scene extensions are stored in `World.extensions`. Scene serialization reinserts the original JSON, so a missing plugin cannot silently delete data during load/save round trips.

Dynamic component data lives in the same place, so scene save/load and snapshot round trips preserve it naturally. The session load path (`RuntimeSession::load_scene`, the single scene-loading entry shared by GUI/CLI/agent, see the [scene format](scene-format_en.md)) validates dynamic component data against the session registry: values from older versions run through the migrator chain first, then schema defaults and validation apply. Calling `scene::load_scene` directly (the `moni scene` CLI round trip, the read-only viewer) still performs JSON structure checks only.

After a successful load the in-memory world keeps a rebuilt component version table: scene version entries for unregistered components and future-version instances are retained, entries of migrated/validated data are consumed, and the registry-aware save rebuilds the table from actual content. The entry rules and the future-version read-only semantics are defined once by the [scene format](scene-format_en.md) and are not repeated here.

`restore_entity` reinserts an entity snapshot under its explicit id (the undo/redo stable-ID path): its extensions data — including unregistered components and future-version instances — comes back with it, and the restore itself performs no schema validation. Version-table entries dropped by despawn do not come back with the restore; despawning and restoring the only carrier of a future-version or unregistered component therefore loses its scene version record on the next save.

Preservation does not grant execution permission. Third-party registration and permission checks belong to the plugin SDK and manager.

## Current limitations

- Dynamic components are JSON-schema-validated data only: there is no typed hot-path storage, and per-frame game logic must not read them every tick; data that needs per-frame access should wait for a typed component path (planned) or use the built-in components.
- Component IDs, field names, and schema metadata use `&'static str`; runtime registration therefore needs static strings (literals or `Box::leak`).
- Registration and revocation are currently Rust API only (`Engine::register_component` / `unregister_component`); protocol commands cover attach and remove only, and registration is not exposed through batches or the protocol. The engine emits `component_registered` / `component_unregistered` on every registration entry path.
- Migration supports dynamic components and upgrades only (`from < to`); there is no downgrade path. The in-memory world retains the scene versions of unregistered components and future-version instances through the component version table (rules in the [scene format](scene-format_en.md)); migrated/validated data is saved at the registry's current versions.
