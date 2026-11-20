# Platform and publishing plugins

[中文](README.md) | English

`platforms/` owns target-platform capabilities and conversion of independent game projects into installable, runnable artifacts. Windows, Linux, macOS, HarmonyOS, Android, iOS, and Web are candidate targets, not claims of current support.

## Responsibility boundary

- Platform capabilities: target identity, architecture, OS windows and lifecycle, file paths, device events, and toolchain restrictions.
- Build providers: Rust/script compilation, asset conversion, dependency collection, and target directory generation.
- Exporters: executables, application bundles, installable packages, and symbol files.
- Publishing support: signing, versions, icons, permissions, manifests, and reproducible builds.
- Verification: run the actual artifact in the target environment rather than treating compilation as sufficient.

Asset import belongs to [`assets/`](../assets/README_en.md); platform-specific rendering attaches through [`rendering/`](../rendering/README_en.md); editor export buttons belong only to [`editor/`](../editor/README_en.md).

## Plugin organization

Name target platforms and package formats by their actual responsibilities. A target may reuse a generic builder, but its plugin owns platform-specific behavior. Complete one desktop target exporting an independent project before expanding the target matrix.

## Performance and stability

- Player packages contain only the selected target and enabled plugins, never the editor, development tools, or other platform binaries.
- Builds record engine versions, plugin versions, asset hashes, targets, settings, and dependencies for reproduction and diagnosis.
- Packaging needs path restrictions, size limits, cancellation, and atomic output; failure must not leave an apparently complete package.
- Successful cross-compilation alone does not establish platform support. Test startup, rendering, input, asset paths, upgrades, and long runs separately.
- Signing keys and platform credentials must never enter game repositories, logs, or build artifacts.

## Current limitations

Only a local development client exists. Project exporters, dependency collection, a cross-platform build matrix, signing, installable packages, and HarmonyOS support are not implemented. Every named target is a future plugin direction.
