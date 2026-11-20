# Game project templates

[中文](README.md) | English

`templates/` owns officially maintained, engine-versioned templates for independent game repositories. Templates feed the project-creation command in [`tools/`](../tools/README_en.md); they are not engine examples and never require copying engine source into a game.

## Template contents

- Project manifest: engine version, plugin dependencies, default profile, and build settings.
- Profile declarations: ordered bundles, project plugins, and overridable default patches.
- Lockfile: fixed direct and transitive plugin versions for reproducible installation.
- `assets/`, `scenes/`, `scripts/`, and optional game-specific `plugins/`.
- Asset import metadata, scene entry points, project documentation, and version-control rules that exclude generated caches.
- A minimal runnable scene and verification commands without absolute development-machine paths.

Engine examples remain in [`examples/`](../examples/README_en.md), profile composition semantics live in [`profiles/`](../profiles/README_en.md), and compatibility and migration rely on [`sdk/`](../sdk/README_en.md).

## Repository boundary

A generated game repository has independent commits, branches, and releases. Engine and plugin upgrades use project tools and migrations rather than modifications to the engine installation. Reusable third-party plugins remain independent repositories referenced by the project manifest.

## Performance and stability

- Minimal templates enable few capabilities so new projects do not inherit the cost of a full editor and every game system.
- Verify each template through creation, dependency installation, running, snapshot saving, and at least one real export.
- Cache and build directories can be deleted and regenerated; lockfiles and import metadata belong in version control.
- A failed project upgrade must not overwrite the only usable previous configuration.

## Current limitations

No generatable template, project manifest specification, lockfile, or `moni new` command exists. This directory fixes template ownership only; it does not provide game scaffolding yet.
