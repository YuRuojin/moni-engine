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

Start with the independent project model and read-only scene preview, then add hierarchy, inspectors, and transform editing. Editor controls need stable identities, typed properties, and command mappings so agents can perform operations equivalent to the UI.

## Performance and stability

- The editor is a development tool excluded from player builds by default; the runtime must not carry its dependencies or update costs when the editor is absent.
- Large scene trees, asset lists, and logs need virtualization, pagination, or explicit bounds rather than unbounded memory growth.
- Editing uses undoable commands and validates saved data; failures must not leave partial scene state.
- Imports, builds, and analysis run in the background with progress, cancellation, and clear errors without blocking the viewport.
- Unloading a plugin panel must release subscriptions, timers, and file watchers.

## Current limitations

The `editor` command provides a read-only scene viewer that loads `.mnsc`, displays entity hierarchy, model resources, lights, and performance overlays, and supports camera movement and zoom. It neither executes scripts nor writes files. Full property editing, project creation, control registration, and undo remain unimplemented.
