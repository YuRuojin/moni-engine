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

Built-in components are resolved by the [`ComponentRegistry`](component-schema_en.md). Unregistered components retain their original JSON without execution permission.

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

This loads, validates, and saves a scene to verify migration, alias tables, and unknown-data preservation. Runnable examples are the committed `testing/assets/benchmark-2d.mnsc` and `testing/assets/benchmark-3d.mnsc`.

## Current boundaries

v2 has no prefab inheritance, dynamic third-party component execution, incremental scene editing, or binary encoding. `src/scene.rs` converts it into a `World` validated by the existing engine; runtime snapshots and `.mnsc` remain different formats.
