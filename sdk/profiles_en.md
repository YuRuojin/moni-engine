# Plugin manifests and profile composition

[中文](profiles.md) | English

`src/profile.rs` provides static plugin manifests, dependency resolution, target filtering, static bundles, profile patches, and effective plugin-tree export. No dynamic registry or dynamic-library ABI is involved.

## Plugin manifests

A `PluginManifest` contains:

- `id`, `version`, and `api_version`
- direct dependencies with `optional` markers
- conflicting plugins
- permission declarations
- resource declarations
- module descriptors

`PLUGIN_API_VERSION` is currently 1. Unknown plugins, missing required dependencies, conflicts, and incompatible API versions fail explicitly.

## Modules and target filtering

Module domains are `runtime`, `editor`, `build`, `developer`, and `test`. Modules may restrict OS, CPU architecture, and `debug`, `release`, `shipping`, or `test` build configuration; empty lists mean unrestricted.

`Target::current()` uses the current OS, architecture, and configuration. Plugins without a matching module are excluded from the effective tree.

## Dependency resolution

The resolver:

1. Expands ordered bundles and explicit plugin declarations.
2. Applies ordered enable/disable patches and rejects unknown targets.
3. Recursively includes required dependencies; optional dependencies are included only when explicitly requested.
4. Reports missing dependencies, disabled required dependencies, and dependency cycles with concrete paths.
5. Applies target filtering and conflict checks after topological sorting.

A required dependency disabled by a patch is an error rather than a silent skip.

## Current bundles

- `minimal`: empty composition.
- `default`: `core.simulation`, `core.render`, and `core.fbx`.
- `headless`: empty composition with no window, GPU, QuickJS, or FBX initialization.
- `editor`: `core.render`, `core.fbx`, and `core.editor`, without QuickJS.

## Instantiation and pruning boundary

`Profile::instantiate` creates only plugins in the effective tree that have static factories. `core.render` fails without a window and cannot be used for headless execution. Without `core.fbx`, no FBX parsing or upload occurs.

Current pruning happens in the **effective tree and runtime instantiation layer**: unselected modules are not initialized. Cargo features, link-time binary pruning, and target-package pruning are not implemented, so binary-size optimization must not be claimed.

## Inspection commands

```bash
cargo run -- profile list
cargo run -- profile dump --profile default
cargo run -- profile dump --profile default --patch core.render=off
cargo run -- headless --profile headless
cargo run -- headless --profile minimal
```

`profile dump` exposes the target, dependency order, modules, permissions, and resources for agents and build tools.
