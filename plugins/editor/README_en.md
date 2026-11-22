# Visual editor plugins

[中文](README.md) | English

`editor/` owns visual development tools and project operations: the editor shell, scene hierarchy, inspectors, viewports, transform controls, asset browsers, script panels, and plugin UI. It operates on game projects through public commands and queries rather than direct engine-state mutation.

## Responsibility boundary

- Editor shell: windows, layouts, workspaces, menus, and project selection.
- Scene tools: hierarchy, selection, inspectors, viewports, cameras, and transform controls.
- Extensible controls: property editors, asset pickers, graphs, and plugin panels.
- Project UI: expose create, open, save, and build operations through [`tools/`](../../tools/README_en.md) and platform services rather than implementing installation, dependency resolution, or exporters directly.

Asset parsing belongs to [`assets/`](../assets/README_en.md), rendered images to [`rendering/`](../rendering/README_en.md), and package generation to [`platforms/`](../platforms/README_en.md).

## Initial official capabilities

Editor controls need stable identities, typed properties, and command mappings so agents can perform operations equivalent to the UI; [`scene-viewer/`](scene-viewer/README_en.md) already makes panel edits produce the same revisions, events, and saved bytes as the NDJSON command path (guaranteed by document-level tests).

## Performance and stability

- The editor is a development tool excluded from player builds by default; the runtime must not carry its dependencies or update costs when the editor is absent.
- Large scene trees, asset lists, and logs need virtualization, pagination, or explicit bounds rather than unbounded memory growth.
- Editing uses undoable commands and validates saved data; failures must not leave partial scene state.
- Imports, builds, and analysis run in the background with progress, cancellation, and clear errors without blocking the viewport.
- Unloading a plugin panel must release subscriptions, timers, and file watchers.

## Current capabilities and limitations

[`scene-viewer/`](scene-viewer/README_en.md) is an editable scene editor: project mode (project detection, profile merge, recents), egui panels (hierarchy, inspector, asset browser, script builds), play mode (simulation ticks with the play-mode edit lock), undo/redo and scene persistence, and in-viewport editing (click-select, the move gizmo with snapping, the renderer grid toggle, and mouse camera operations). Rotate/scale gizmos, control registration, and a plugin UI SDK remain unimplemented.
