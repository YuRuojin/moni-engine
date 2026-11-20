# Minimal Moni game project

[中文](README.md) | English

This independent game repository template is copied by `moni new`. `game.mngp` stores the engine version, default profile, scene, and dependencies; `moni.lock` is generated from the effective plugin tree by `moni new`.

## Develop

```bash
moni run
```

`moni run` builds the project's TypeScript and opens the default client. The first run needs project dependencies; the equivalent manual steps are:

```bash
npm install
npm run build
moni run
```

The scene is `scenes/main.mnsc`, and scripts live in `scripts/main.ts`. Generated `build/` and `node_modules/` stay out of Git. Engine installation and project source are separate; do not modify the engine repository for game development.

## Verify

```bash
moni doctor
moni doctor --update-lock
moni doctor --rollback-lock
moni profile dump
```
