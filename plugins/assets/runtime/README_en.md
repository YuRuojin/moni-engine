# Asynchronous asset runtime

[中文](README.md) | English

`mod.rs` implements bounded asynchronous asset loading with one `moni-asset-loader` worker and request/response queues capped at 64 entries. The worker reads FBX or cooked meshes without accessing the world or GPU.

## Lifecycle

- `load`: deduplicates and submits nonblocking work.
- `preload`: waits for the same worker during initialization and fails on timeout or error.
- `poll`: receives results on the render thread without waiting for file I/O.
- `set_references`: updates shared asset lifetime from scene references.
- `shutdown`: cancels pending work, notifies and joins the worker, then clears caches.

Each request has a 10-second timeout. Timeout cancels result submission and records an error; the parser cannot be forcibly interrupted, so shutdown waits for the current parser call to return.

Full queues, parse failures, timeouts, and shutdown are explicit. New models enter the GPU cache after loading instead of being parsed synchronously in a render frame. `cook --isolate` provides child-process crash containment for import tasks, but it is not a security sandbox.
