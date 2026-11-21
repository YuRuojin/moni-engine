# Profiles and bundles

[中文](README.md) | English

`profiles/` follows the dsh composition model: **a profile is a named composition, a bundle is a reusable layer of plugin declarations, and a patch is an ordered override**. Concrete game capabilities never live here; rendering, physics, editing, and other features remain plugins.

## Composition model

- **Bundle**: identified plugin declarations, default settings, and dependencies reused by multiple profiles.
- **Profile**: ordered bundles, additional plugins, and its own patches forming an editor, player, automation, or server environment.
- **Patch**: replaces settings or inserts plugins by declaration identity without modifying bundle source.
- **Launch adapter**: resolves a named profile and starts the host rather than implementing a separate hardcoded feature composition.

Current static compositions are `minimal`, `default`, `editor`, `headless`, and `serve` (the agent/CLI control path, wiring `core.simulation`); `base`, `player`, and `server` remain future directions. Headless initializes no window, GPU, QuickJS, or FBX provider instead of maintaining another engine. See [`sdk/profiles_en.md`](../sdk/profiles_en.md) for the contract.

## Ordered overrides

The target resolution order is the profile's ordered bundles, project plugin declarations, project patches, user patches, then per-run or agent patches. Later layers may override earlier configuration. The final effective plugin tree must be exportable, inspectable, and reproducible.

Game repositories receive an initial profile through [`templates/`](../templates/README_en.md); [`tools/`](../tools/README_en.md) installs and resolves plugins; [`sdk/`](../sdk/README_en.md) defines declarations and dependency rules.

## Service composition

Profiles compose capabilities without calling concrete providers directly. Every replaceable capability separates service definition, service provider, and consumer, with explicit provider selection. Unloading a plugin reverses its service, event, task, and resource registrations.

## Performance and stability

- Unselected plugins are excluded from the effective tree and instantiation. Cargo features, link-time binary pruning, and package pruning remain unimplemented.
- `player` excludes the editor, Hub, and source-asset importers; `headless` does not initialize windows or GPUs.
- Configuration overrides cannot bypass permission, dependency, or compatibility validation.
- Measure startup, memory, binary size, and shutdown cleanup for every profile, and identify the declaration layer responsible for a patch failure.

## Current limitations

Static manifests, target filtering, dependency resolution, minimal/default/headless/editor bundles, ordered enable/disable patches, effective-tree export, and runtime instantiation pruning exist. External manifest loading, dynamic registries, project profile files, link-time pruning, and server compositions remain unimplemented.
