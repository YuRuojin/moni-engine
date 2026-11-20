# Plugin permissions, integrity, and isolation

[中文](security.md) | English

`PermissionPolicy` in `src/host.rs` stores manifest-granted permissions by plugin ID. Plugins check permissions during startup and execution through `PluginContext`; unregister revokes them and shutdown clears them.

## Permission boundaries

Current permissions are:

- `core.simulation`: `script.execute`.
- `core.render`: `gpu.create`.
- `core.fbx`: `asset.read.workspace`.

The FBX permission belongs to the asset provider. Rendering checks `require_from("core.fbx", "asset.read.workspace")` rather than inheriting that permission itself. Missing authorization fails explicitly without fallback execution.

`client --revoke core.render=gpu.create` and FBX revocation exercise denial paths. This is a compatibility verification mechanism, not a remote permission protocol.

## Update differences and acceptance

`moni doctor` compares permission sets in `moni.lock` with current profile resolution. `doctor --update-lock` reports added and removed permissions; any addition requires explicit `--accept-permissions`, otherwise the update is rejected.

`moni run` never automatically accepts permission changes and refuses to start when the lockfile differs from the profile.

## Integrity and rollback

`moni.lock` records a SHA-256 hash for every plugin manifest. Doctor compares the complete lockfile, so changes to manifests, versions, permissions, or integrity fields are detected.

Updates write `moni.lock.backup`. `doctor --rollback-lock` restores it and revalidates the current profile. SHA-256 provides integrity hashing, not publisher signatures; key signing, certificate chains, and registry trust are not implemented.

## Import process isolation

`cook --isolate` runs FBX parsing and cooked writing in a child process. The parent waits at most 10 seconds; timeout or crash terminates only the child and leaves world, GPU, and script hosts intact.

This is crash isolation, not a complete security sandbox: the child still has the current user's filesystem and system permissions. Actual filesystem/network sandboxing and credential isolation remain platform-plugin work.

## Script limits

QuickJS already enforces a 50 ms callback, 32 MiB memory, 256 KiB stack, and command/log count and size limits. Script contexts remain on the logic thread, and hosts are disabled and cleaned after errors.
