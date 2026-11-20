# Asynchronous asset runtime

[中文](README.md) | English

`mod.rs` implements bounded asynchronous asset loading with one `moni-asset-loader` worker and request/response queues capped at 64 entries. The worker reads FBX or cooked meshes without accessing the world or GPU.

## Lifecycle

- `load`: deduplicates and submits nonblocking work.
- `preload`: waits for the same worker during initialization and fails on timeout or error.
- `poll`: receives results on the render thread without waiting for file I/O.
- `set_references`: updates shared asset lifetime from scene references.
- `set_aliases`: installs the `res://<alias>` virtual path table (from scene `extensions.res`).
- `handle`: returns a cached handle with its revision; revisions increment on every successful load.
- `shutdown`: cancels pending work, notifies and joins the worker, then clears caches.

## Virtual asset paths

`res://<alias>` resolves to a workspace-relative physical path inside `load`; the worker still receives physical paths. Cache entries, errors, and reference tables keep the alias (the original string) as their key, so the alias is the asset's stable identity. Undeclared aliases fail with an explicit error instead of silently falling back.

## Hot reload

Hot reload is off by default and enabled explicitly via `client --hot-reload` or `editor --hot-reload`; when off, it costs zero per-frame stat calls. When enabled, `poll` compares mtime+size fingerprints for referenced, cached assets: a changed source requeues through the bounded loader while the renderer keeps the previous revision, and GPU buffers rebuild from the revision mismatch once the new version arrives. A failed reload keeps rendering the previous version with a one-time stderr diagnostic. Fingerprints use mtime+size rather than content hashes, so restoring identical bytes by hand does not trigger a reload.

Each request has a 10-second timeout. Timeout cancels result submission and records an error; the parser cannot be forcibly interrupted, so shutdown waits for the current parser call to return.

Full queues, parse failures, timeouts, and shutdown are explicit. New models enter the GPU cache after loading instead of being parsed synchronously in a render frame. `cook --isolate` provides child-process crash containment for import tasks, but it is not a security sandbox.
