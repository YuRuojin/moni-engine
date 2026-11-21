# Plugin SDK

[中文](README.md) | English

`sdk/` follows the dsh capability-seam model: every capability comprises a **service definition, service provider, and consumer**, and plugins attach through reversible service, event, and effect registrations. It also owns manifests, versions, permissions, data schemas, and migrations rather than concrete rendering, gameplay, or editing features.

## Responsibility boundary

- Service definitions: stable interfaces used by consumers; algorithms, formats, backends, and platform implementations are replaceable providers.
- Lifecycle: typed events, service registration, effect disposal, and resource cleanup without irreversible global registrations.
- Manifests: identity, version, engine/API versions, dependencies, conflicts, permissions, and content declarations.
- Module descriptors: `runtime`, `editor`, `build`, `developer`, and `test` domains plus loading phases, platforms, and build-configuration filters.
- Extension interfaces: public definitions for component/property registration, system scheduling, resource types, editor extensions, and build steps.
- Compatibility and migration: validation and upgrade paths for plugin APIs, scene schemas, and asset formats.
- Agent-readable information: the runtime service tree, interface schemas, capability descriptions, permissions, and diagnostics.

A capability considers definition, provider, and consumer together, splitting packages only when the roles evolve independently. Consumers depend on definitions rather than replaceable providers.

Script-language runtimes belong to [`plugins/scripting/`](../plugins/scripting/README_en.md), dependency installation to [`tools/`](../tools/README_en.md), and ordered plugin composition to [`profiles/`](../profiles/README_en.md).

## Plugin package direction

Once implemented, every plugin package has a manifest, bilingual READMEs, changelog, third-party notices, public interfaces, tests, and optional samples and assets. Code modules are isolated by execution domain; content-only, script-only, native, and hybrid plugins share identity and dependency rules.

The first stage provides version-aligned static Rust interfaces only, with no stable dynamic-library ABI promise. Any future native ABI needs independent versioning, target descriptions, and compatibility tests.

## Performance and stability

- Public interfaces define ownership, calling thread, lifecycle, failure semantics, and resource limits.
- Hot-path interfaces avoid unnecessary strings, JSON, and cross-process conversion. Machine-readable schemas serve discovery, validation, and tools rather than per-frame calls.
- Registrations are reversible; incompatible versions or permissions fail before modifying the world.
- Schema migrations require repeatable verification, and unknown plugin data must not be silently discarded.

## Permissions and isolation

[Plugin permissions, integrity, and isolation](security_en.md) defines host permission enforcement, update acceptance, SHA-256, lockfile rollback, and import process isolation.

## Game projects and lockfiles

[Moni game project and lockfile](project-format_en.md) defines `.mngp`, `moni.lock`, and the basic CLI for independent projects.

## Plugin manifests and composition

[Plugin manifests and profile composition](profiles_en.md) defines static manifests, execution domains, target filtering, dependency resolution, static bundles, ordered patches, and effective-tree export.

## Static plugin lifecycle

[Static plugin host and lifecycle](plugin-lifecycle_en.md) defines stages, dependencies, access constraints, thread affinity, registration rollback, disposal, and reverse cleanup.

## Agent control plane

[MCP adapter](mcp_en.md) defines the `moni mcp` tool surface: MCP is a thin adapter over the control plane — every mutation goes through `RuntimeSession` command dispatch with NDJSON-identical semantics.

## Scene format

[Moni Scene v2](scene-format_en.md) defines declarative cameras, lights, entities, components, and resource references in `.mnsc`, converted by `crates/moni-core/src/scene.rs` into a validated `World`; v1 is read through adjacent migration. [Component and property schemas](component-schema_en.md) unify built-in registration, validation, and agent descriptions.

## Current limitations

`.mnsc` v2, v1 migration, the core component registry, and static plugin manifests exist, but external manifest loading, a public SDK crate, stable ABI, code generation, permission enforcement, and dynamic third-party component registration do not.
