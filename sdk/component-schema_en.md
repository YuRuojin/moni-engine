# Component and property schemas

[中文](component-schema.md) | English

`src/components.rs` defines `ComponentRegistry`, `ComponentDescriptor`, `PropertySchema`, and `PropertyField`. Only built-in components owned by `moni.core` are registered; third-party dynamic plugin loading is not implemented.

## Component descriptors

Each `ComponentDescriptor` contains:

- `id`: the namespaced ID used by the current version, such as `core.model`.
- `legacy_id`: the compatibility ID used by v1, such as `model`.
- `owner`: the component owner, `moni.core` for built-ins.
- `version`: the component schema version.
- `schema`: field definitions, defaults, validation, and editor metadata.
- `deserialize` / `serialize`: JSON conversion functions for the registered component.

Duplicate component or legacy IDs are rejected. Consumers resolve components through the registry instead of depending on concrete providers.

## Property definitions

A `PropertyField` describes its name, type, required status, default, numeric range, length limit, display label, and description. `PropertySchema` applies defaults, validates fields and unknown keys, and produces JSON Schema-style agent output with `x-editor` and `x-component` metadata.

`capabilities.components` exposes the current registry. Rust commands, scene parsing, and agent queries therefore share one source of component metadata instead of separate catalogs.

## Built-in components

- `core.transform`
- `core.sprite`
- `core.model`
- `core.script`

`.mnsc` v1 uses IDs without namespaces; v2 uses the `core.*` IDs above. A script component uses `source` for an external JavaScript path or `inline` for inline source, with exactly one present.

## Unknown data

Unregistered component keys are preserved in `Entity.extensions` instead of being executed as built-ins. Root scene extensions are stored in `World.extensions`. Scene serialization reinserts the original JSON, so a missing plugin cannot silently delete data during load/save round trips.

Preservation does not grant execution permission. Third-party registration, version compatibility, and permission checks belong to the plugin SDK and manager.
