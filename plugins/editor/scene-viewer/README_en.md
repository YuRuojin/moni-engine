# Scene editor plugin

[中文](README.md) | English

`mod.rs`, `document.rs`, `ui.rs`, and `ui_egui.rs` implement the scene editor behind the `editor` command. The `editor` profile mounts `core.render`, `core.fbx`, and `core.editor` without registering the QuickJS simulation plugin.

## Current capabilities

- Open and validate `.mnsc`. The scene is opened through the `EditorDocument` in `document.rs`: undo/redo history, selection, and dirty tracking are document state, and every edit goes through the session command API, producing the same revisions and events as agent edits.
- Display FBX/cooked scenes with the same rendering and asset providers.
- Show entity hierarchy, model resources, materials, lights, and performance overlays; overlay quads are rebuilt only when the input signature changes, timings are quantized to 0.1 ms, and golden mode always installs an empty overlay. The character overlay stays the diagnostics display, separate from the egui panels.
- egui editing panels (`ui_egui.rs`, pinned to egui 0.37 + egui-winit 0.37 in this directory's Cargo.toml):
  - Hierarchy panel: entity list (id + name) selection, create, per-entity delete and rename.
  - Inspector: name, Transform (drag values applied on change-commit), attached dynamic components driven by the registered schema (number ranges, enum combo boxes, color/path/string text inputs, object JSON), component add (value pre-filled from schema defaults) and remove; replacing a component re-attaches a new value.
  - Undo/redo (Ctrl+Z / Ctrl+Y and depth-labeled buttons), dirty indicator, save (Ctrl+S), and save-as.
  - Rejected edits (revision conflict, future version, schema validation) surface as a panel error line without clearing selection or history.
- The edited frame reaches the render plugin through the type-erased `RenderPluginApi::set_egui_frame` (payload `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)`: egui 0.37 tessellation must run on the context that produced the shapes, so the editor tessellates before the hand-off; contract documented in `crates/moni-core/src/host.rs`), and the wgpu side composites it with egui-wgpu as the last layer before present.
- Golden mode fully disables egui (no egui context is created and no frame is sent), keeping golden captures byte-deterministic.
- Move the camera with arrows (yielding to egui while it owns the keyboard), zoom with PageUp/PageDown, and reset with Home.

## Limitations

No asset browser, docking, animations, project editing, control registration, or plugin UI SDK. Component field widgets are driven by schema property types; there is no hand-written per-component UI. The core stays egui-free; egui types appear only in this directory and the render plugin.
