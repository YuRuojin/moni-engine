# Moni Scene v2 (`.mnsc`)

[中文](scene-format.md) | English

`.mnsc` is Moni Engine's declarative scene source format, currently encoded as JSON. It is not a runtime snapshot and stores no tick, events, script errors, JavaScript heap, or GPU state. The loader accepts v1 and v2 and always saves v2.

## Header

```json
{
  "format": "moni-scene",
  "version": 2,
  "name": "example",
  "extensions": {}
}
```

The loader rejects other formats, unknown versions, empty scene names, duplicate or non-positive entity IDs, empty entity names, and unknown root fields. The extension is only a hint; `format + version` determines interpretation.

## Root fields

| Field | Required | Meaning |
|---|---|---|
| `format`, `version`, `name` | Yes | Format identity, scene schema version, and display name |
| `viewport` | No | Logical viewport size and background |
| `camera` | Yes | `perspective` or `orthographic` camera, position, target, and clipping planes |
| `lights` | No | Directional and point lights; empty by default |
| `extensions` | No | Raw extension data outside the core scene model |
| `entities` | Yes | Scene entities with `id`, `name`, and `components` |

Camera fields are `projection`, `position`, `target`, `fov_y`, `near`, and `far`; orthographic cameras also use `orthographic_height`.

## Components

v2 uses namespaced component IDs:

- `core.transform`: 2D `x`, `y`, and `rotation`, defaulting to zero values.
- `core.sprite`: a 2D rectangle or circle with dimensions, color, and layer.
- `core.model`: `cube`, `plane`, or `fbx`, with model transform, color, `metallic`, and `roughness`.
- `core.script`: `source` for an external JavaScript path or `inline` for inline source, with exactly one present.

```json
{
  "core.model": {
    "type": "fbx",
    "source": "testing/assets/garden_diorama.fbx",
    "transform": {
      "position": { "x": -1.6, "y": 0.0, "z": 0.0 },
      "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
      "scale": { "x": 1.4, "y": 1.4, "z": 1.4 }
    },
    "color": "#ffffff",
    "metallic": 0.0,
    "roughness": 0.55
  },
  "core.script": {
    "source": "examples/dist/model-orbit.js"
  },
  "vendor.custom": {
    "preservedWithoutExecution": true
  }
}
```

Built-in components are resolved by the [`ComponentRegistry`](component-schema_en.md). Unregistered components retain their original JSON without execution permission. Version declaration and load-time validation for dynamic (schema-only) components are covered in "Component version table" and "Session loading" below.

## Component version table

The root-level `extensions["moni.componentVersions"]` declares the world-level component version table: dynamic component id -> the schema version the scene data was written at.

```json
{
  "extensions": {
    "moni.componentVersions": {
      "game.health": 1
    }
  }
}
```

Save behavior (the registry-aware path rebuilds the table from actual content and never mutates the in-memory world):

- `save_scene_with_registry` / `scene_value_with_registry` write a version for every dynamic (non-core) component still carried by an entity: when the in-memory world retains an entry for it (an unregistered component, or "future-version" data whose scene version is newer than the registry's, see "Session loading"), the retained version is kept; otherwise the registry's current version is written.
- Entries whose component is no longer carried by any entity are dropped, unused components gain no entry (including registered dynamic components that are not in use), and the key is omitted when no entries remain.
- The registry-less `save_scene` / `scene_value` neither injects nor removes the table; extension data is preserved verbatim.

`moni.componentVersions` is a reserved key: it only exists at the scene root, and per-entity component logic must skip it.

## Session loading (the single scene-loading path)

`RuntimeSession::load_scene` (the single loading entry shared by GUI, CLI, and agent sessions; the path is workspace-relative) validates dynamic components against the session registry after `scene::load_scene` syntax and core validation, and before the world is installed:

- The scene version comes from the component version table; a missing entry means the data is treated as being at the registry's current version, with no migration.
- Scene version < registry version: the value runs through the registered migrator chain first, then schema defaults and schema validation apply; a migration or validation failure returns `invalid_component` with details `{ entity, component, cause }`, and the current world, revision, and events stay untouched.
- Scene version > registry version: the data is preserved verbatim (no defaults, no validation), the component id is listed under `preservedFuture` in the result, and no error is raised.
- Unregistered component keys are preserved verbatim.
- On success the session runs `engine.load_world` and returns `{ "loaded": true, "entities": n, "migrated": [component ids...], "preservedFuture": [component ids...] }`.
- A successful load rebuilds the version table in the in-memory world: entries for unregistered components (any version) and for future-version instances are preserved verbatim, entries of components migrated/validated to the registry version are consumed (the save recomputes those), and entries without instances are dropped; an empty table is not installed.
- Future-version instances are read-only: while any entity still carries a future-version instance of a component, `attach_component` writes for it fail with `component_future_version` (details `{ component, sceneVersion, registryVersion, futureInstances }`) and the world stays untouched; `remove_component` on a future instance is allowed (explicitly discarding the data is the way out), and removing the last future instance also drops the retained entry. Later registry-aware saves keep writing the retained version, and reloading after the plugin is installed migrates the data through the migrator chain.

## Lights

```json
{
  "type": "directional",
  "direction": { "x": -0.45, "y": -0.8, "z": -0.35 },
  "color": [1.0, 0.96, 0.9],
  "intensity": 1.0,
  "shadows": true
}
```

Point lights use `position`, `color`, `intensity`, and `range`. Directional light direction follows the light rays, and only directional lights produce shadows.

## v1 → v2 migration

v1 `transform`, `sprite`, `model`, and `script` migrate to `core.transform`, `core.sprite`, `core.model`, and `core.script`. Other component keys remain unchanged, while root and entity extension maps default to empty objects. The migration chain handles only adjacent v1 → v2 and invents no later versions.

Runtime snapshots also accept v1 and v2: migration adds root and entity `extensions`, with current `SNAPSHOT_VERSION` 2. External script `source` paths survive scene round trips unchanged.

## Virtual asset paths

Root-level `extensions.res` declares the virtual asset alias table, mapping aliases to workspace-relative paths:

```json
{
  "extensions": {
    "res": {
      "terrain": "testing/assets/benchmark.mnec"
    }
  }
}
```

`core.model` `source` fields can then use `res://terrain`. The alias is the asset's stable identity: moving the physical file or switching to the cooked format only updates the alias table, while scenes and cache keys stay unchanged. Aliases are 1-64 letters, digits, `-`, `_`, or `.` with no `..`; targets must be workspace-relative paths. The loader parses the table once at scene load and caches it on the world, so render frames never touch JSON. `core.script` `source` still uses physical relative paths and does not participate in virtual addressing.

## Round-trip command

```bash
cargo run -- scene testing/assets/benchmark-3d.mnsc --output target/roundtrip.mnsc --name example
```

This loads, validates, and saves a scene to verify migration, alias tables, and unknown-data preservation (including verbatim preservation of the component version table). Runnable examples are the committed `testing/assets/benchmark-2d.mnsc` and `testing/assets/benchmark-3d.mnsc`.

## Current boundaries

v2 has no prefab inheritance, dynamic third-party component execution, incremental scene editing, or binary encoding. The loader lives in `crates/moni-core/src/scene.rs` and converts the file into a `World` validated by the existing engine; the registry-validated load entry is `RuntimeSession::load_scene` (see "Session loading"). Runtime snapshots and `.mnsc` remain different formats.
