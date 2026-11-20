# Engine and project tools

[中文](README.md) | English

`tools/` owns engine installation, the Hub/launcher, project creation and upgrades, plugin package management, and developer CLIs. It manages installations and repositories outside the engine and is not part of the player runtime.

## Responsibility boundary

- Engine management: download, install, verify, select, uninstall, and retain multiple engine versions.
- Hub and project tools: create, open, and migrate projects, selecting engine versions and named profiles.
- Plugin and composition management: registry queries, dependency resolution, lockfiles, installation, updates, rollback, and removal.
- Profile tools: select bundles, apply ordered patches, and export or inspect the final effective plugin tree.
- Distribution metadata: origins, signatures, hashes, third-party notices, and compatible targets.
- Automation CLI: structured plugin, service, and profile operations equivalent to UI operations for developers and agents.

Named profiles and bundles belong to [`profiles/`](../profiles/README_en.md), game export to [`plugins/platforms/`](../plugins/platforms/README_en.md), plugin interfaces and permissions to [`sdk/`](../sdk/README_en.md), and editor UI to [`plugins/editor/`](../plugins/editor/README_en.md).

## Dependencies and trust

Direct project dependencies and transitive plugin dependencies form a deterministic version graph. Lockfiles ensure repeatable installation; updates expose version and permission changes. Downloads require origin and integrity verification, and transitive dependencies must not silently expand native-code or script privileges.

## Performance and stability

- The Hub and package manager are excluded from player builds and must not continuously consume game runtime resources.
- Installation and updates use temporary directories and atomic transitions with rollback, preserving existing projects and engine versions on failure.
- Operations support cancellation, structured errors, and reproducible logs without exposing credentials in repositories, output, or logs.
- Installed versions and lockfiles remain usable offline when a registry is unavailable.

## Current limitations

There is no Hub, engine installer, registry client, dependency resolver, lockfile implementation, project creator, or upgrade tool. Current commands run engine examples from source only.
