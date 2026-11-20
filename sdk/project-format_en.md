# Moni game project and lockfile

[中文](project-format.md) | English

`.mngp` is the source configuration of an independent game project, while `moni.lock` records deterministic dependency resolution for its current profile. Engine installation and game repositories are separate; projects store no engine source path.

## `game.mngp`

```json
{
  "format": "moni-game-project",
  "version": 1,
  "name": "my-game",
  "engine": "0.1.0",
  "profile": "default",
  "scene": "scenes/main.mnsc",
  "plugins": [],
  "patches": [],
  "dependencies": {}
}
```

- `format` must be `moni-game-project`, with version 1 currently.
- `engine` must exactly match the current engine version.
- `profile` names a static profile; `plugins` adds explicit plugins and `patches` applies ordered enable/disable overrides.
- `scene` must be project-relative; absolute paths and `..` are rejected.
- `dependencies` stores direct project dependencies and versions; the current template is empty.

## `moni.lock`

The lockfile contains:

- `format: "moni-project-lock"` and lock format version 1.
- Engine version and profile.
- Every effective plugin's exact version, API version, permissions, resources, and manifest SHA-256.
- Direct project dependencies.

`moni new` generates the lockfile after copying the template. `moni run` compares it with current profile resolution before startup and rejects a mismatch. `moni doctor --update-lock` reports permission differences, requires `--accept-permissions` for additions, and saves `moni.lock.backup` before updating. `moni doctor --rollback-lock` restores and revalidates the backup. Hashes provide integrity checks, not publisher signatures.

## Basic commands

```bash
moni new my-game
cd my-game
moni run
moni doctor
moni profile dump
```

`moni new` copies `templates/minimal-game/`, sets the project name, writes the lockfile, and initializes an independent Git repository. `moni run` installs locked npm dependencies with `npm ci` when needed, builds TypeScript, and starts the client with the project's profile and `.mnsc` scene.

`run` currently finds templates in the engine installation or source directory. Release distribution must ship templates with the engine rather than relying on a development-machine source path.
