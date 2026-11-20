# Profiles and bundles

[中文](README.md) | English

`profiles/` follows the dsh composition model: **a profile is a named composition, a bundle is a reusable layer of plugin declarations, and a patch is an ordered override**. Concrete game capabilities never live here; rendering, physics, editing, and other features remain plugins.

## Composition model

- **Bundle**: identified plugin declarations, default settings, and dependencies reused by multiple profiles.
- **Profile**: ordered bundles, additional plugins, and its own patches forming an editor, player, automation, or server environment.
- **Patch**: replaces settings or inserts plugins by declaration identity without modifying bundle source.
- **Launch adapter**: resolves a named profile and starts the host rather than implementing a separate hardcoded feature composition.

Official composition directions include `base`, `default`, `editor`, `player`, `headless`, and `server`. Headless and server profiles omit window and rendering providers instead of maintaining another engine.

## Ordered overrides

The target resolution order is the profile's ordered bundles, project plugin declarations, project patches, user patches, then per-run or agent patches. Later layers may override earlier configuration. The final effective plugin tree must be exportable, inspectable, and reproducible.

Game repositories receive an initial profile through [`templates/`](../templates/README_en.md); [`tools/`](../tools/README_en.md) installs and resolves plugins; [`sdk/`](../sdk/README_en.md) defines declarations and dependency rules.

## Service composition

Profiles compose capabilities without calling concrete providers directly. Every replaceable capability separates service definition, service provider, and consumer, with explicit provider selection. Unloading a plugin reverses its service, event, task, and resource registrations.

## Performance and stability

- Release builds prune plugins and dependencies absent from the final effective tree rather than merely skipping their updates after startup.
- `player` excludes the editor, Hub, and source-asset importers; `headless` does not initialize windows or GPUs.
- Configuration overrides cannot bypass permission, dependency, or compatibility validation.
- Measure startup, memory, binary size, and shutdown cleanup for every profile, and identify the declaration layer responsible for a patch failure.

## Current limitations

Only the `client`, `serve`, and `demo` entries in `src/main.rs` exist. Profile resolution, bundle formats, ordered patches, effective-tree export, and composition-based build pruning are not implemented.
