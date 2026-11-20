# Read-only scene viewer plugin

[中文](README.md) | English

`mod.rs` and `ui.rs` implement the read-only viewer for the `editor` command. The `editor` profile mounts `core.render`, `core.fbx`, and `core.editor` without registering the QuickJS simulation plugin.

## Current capabilities

- Open and validate `.mnsc`.
- Display FBX/cooked scenes with the same rendering and asset providers.
- Show entity hierarchy, model resources, materials, lights, and performance overlays.
- Move the camera with arrows, zoom with PageUp/PageDown, and reset with Home.
- Execute no scripts and never modify or write back scene files.

## Limitations

Property editing, undo/redo, project editing, control registration, and a plugin UI SDK are absent. The overlay uses the engine's 2D renderer rather than a complete editor framework.
