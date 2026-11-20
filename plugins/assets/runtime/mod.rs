use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::fbx::FbxMesh;

pub const MAX_ASSET_QUEUE: usize = 64;
pub const ASSET_TIMEOUT: Duration = Duration::from_secs(10);
/// Virtual asset paths use this scheme: `res://<alias>` resolves through the
/// alias table declared in scene `extensions.res`, so scenes keep stable
/// identities even when physical files move.
pub const VIRTUAL_ASSET_SCHEME: &str = "res://";

/// A cached mesh together with the revision it was loaded at. Renderers
/// compare revisions to detect hot reloads without per-frame parsing.
pub struct AssetHandle {
    pub mesh: Arc<FbxMesh>,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Fbx,
    Cooked,
}

struct AssetRequest {
    id: u64,
    path: String,
    kind: AssetKind,
    cancel: Arc<AtomicBool>,
}

enum AssetCommand {
    Load(AssetRequest),
    Shutdown,
}

struct AssetResponse {
    id: u64,
    result: Result<Arc<FbxMesh>, String>,
}

struct PendingAsset {
    path: String,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
}

pub struct AssetRuntime {
    sender: SyncSender<AssetCommand>,
    receiver: Receiver<AssetResponse>,
    worker: Option<JoinHandle<()>>,
    pending: BTreeMap<u64, PendingAsset>,
    ids: BTreeMap<String, u64>,
    cache: BTreeMap<String, Arc<FbxMesh>>,
    references: BTreeMap<String, usize>,
    errors: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    fingerprints: BTreeMap<String, (SystemTime, u64)>,
    revisions: BTreeMap<String, u64>,
    next_id: u64,
    next_revision: u64,
    hot_reload: bool,
    shutdown: bool,
}

impl AssetRuntime {
    pub fn new() -> Self {
        Self::new_with_options(false)
    }

    /// `hot_reload` enables mtime+size fingerprint checks during `poll` for
    /// referenced, cached assets. Players and shipping builds keep it off so
    /// they pay zero per-frame stat cost.
    pub fn new_with_options(hot_reload: bool) -> Self {
        let (sender, request_receiver) = mpsc::sync_channel(MAX_ASSET_QUEUE);
        let (response_sender, receiver) = mpsc::sync_channel(MAX_ASSET_QUEUE);
        let worker = std::thread::Builder::new()
            .name("moni-asset-loader".to_string())
            .spawn(move || worker_loop(request_receiver, response_sender))
            .expect("could not spawn asset worker");
        Self {
            sender,
            receiver,
            worker: Some(worker),
            pending: BTreeMap::new(),
            ids: BTreeMap::new(),
            cache: BTreeMap::new(),
            references: BTreeMap::new(),
            errors: BTreeMap::new(),
            aliases: BTreeMap::new(),
            fingerprints: BTreeMap::new(),
            revisions: BTreeMap::new(),
            next_id: 1,
            next_revision: 1,
            hot_reload,
            shutdown: false,
        }
    }

    /// Installs the virtual alias table. `res://<alias>` asset paths resolve
    /// through it while keeping the alias as their stable cache identity.
    pub fn set_aliases(&mut self, aliases: &BTreeMap<String, String>) {
        if self.aliases != *aliases {
            self.aliases = aliases.clone();
        }
    }

    fn resolve_physical(&self, path: &str) -> anyhow::Result<String> {
        match path.strip_prefix(VIRTUAL_ASSET_SCHEME) {
            Some(alias) => self.aliases.get(alias).map_or_else(
                || {
                    anyhow::bail!(
                        "unknown virtual asset path '{path}'; declare it under scene extensions.res"
                    )
                },
                |physical| Ok(physical.clone()),
            ),
            None => Ok(path.to_string()),
        }
    }

    pub fn load(&mut self, path: &str) -> anyhow::Result<()> {
        if self.shutdown {
            anyhow::bail!("asset runtime is shut down");
        }
        if self.cache.contains_key(path)
            || self.errors.contains_key(path)
            || self.ids.contains_key(path)
        {
            return Ok(());
        }
        let physical = self.resolve_physical(path)?;
        let absolute = moni_core::workspace_path(&physical)?;
        let kind = asset_kind(&physical)?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("asset ID overflow"))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let request = AssetRequest {
            id,
            path: absolute.to_string_lossy().into_owned(),
            kind,
            cancel: Arc::clone(&cancel),
        };
        self.sender
            .try_send(AssetCommand::Load(request))
            .map_err(|error| anyhow::anyhow!("asset queue rejected '{path}': {error}"))?;
        self.ids.insert(path.to_string(), id);
        self.pending.insert(
            id,
            PendingAsset {
                path: path.to_string(),
                deadline: Instant::now() + ASSET_TIMEOUT,
                cancel,
            },
        );
        if self.hot_reload
            && let Ok(fingerprint) = fingerprint(&absolute)
        {
            self.fingerprints.insert(path.to_string(), fingerprint);
        }
        Ok(())
    }

    pub fn preload(&mut self, paths: &[String]) -> anyhow::Result<()> {
        for path in paths {
            self.load(path)?;
        }
        let deadline = Instant::now() + ASSET_TIMEOUT;
        while paths.iter().any(|path| self.ids.contains_key(path)) {
            self.poll()?;
            if let Some(path) = paths.iter().find(|path| self.errors.contains_key(*path)) {
                let error = self.errors[path].clone();
                anyhow::bail!("asset preload failed for '{path}': {error}");
            }
            if Instant::now() >= deadline {
                anyhow::bail!("asset preload timed out");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for path in paths {
            if let Some(error) = self.errors.get(path) {
                anyhow::bail!("asset preload failed for '{path}': {error}");
            }
            if !self.cache.contains_key(path) {
                anyhow::bail!("asset preload did not produce a mesh for '{path}'");
            }
        }
        Ok(())
    }

    pub fn poll(&mut self) -> anyhow::Result<()> {
        while let Ok(response) = self.receiver.try_recv() {
            let Some(pending) = self.pending.remove(&response.id) else {
                continue;
            };
            self.ids.remove(&pending.path);
            let path = pending.path.clone();
            match response.result {
                Ok(mesh) => {
                    let revision = self.next_revision;
                    self.next_revision = self.next_revision.saturating_add(1);
                    self.revisions.insert(path.clone(), revision);
                    self.errors.remove(&path);
                    self.cache.insert(path, mesh);
                }
                Err(error) => {
                    self.errors.insert(path, error);
                }
            }
        }
        if self.hot_reload {
            for (path, fingerprint) in self.detect_source_changes() {
                // Drop the cached entry so `load` can queue a replacement;
                // the renderer keeps serving its previous revision until the
                // fresh one arrives, so no frame ever blocks or blanks.
                self.cache.remove(&path);
                if self.load(&path).is_ok() {
                    self.fingerprints.insert(path, fingerprint);
                }
                // On a bounded-queue rejection the renderer keeps rendering
                // the previous revision; the unchanged fingerprint means the
                // next poll retries the reload.
            }
        }
        let now = Instant::now();
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| now >= pending.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(pending) = self.pending.remove(&id) {
                pending.cancel.store(true, Ordering::Release);
                self.ids.remove(&pending.path);
                self.errors
                    .insert(pending.path, "asset load timed out".to_string());
            }
        }
        Ok(())
    }

    /// Compares mtime+size fingerprints of referenced, cached assets against
    /// the values recorded at load time and reports the paths whose source
    /// file changed on disk.
    fn detect_source_changes(&mut self) -> Vec<(String, (SystemTime, u64))> {
        let mut changed = Vec::new();
        for path in self.references.keys() {
            if !self.cache.contains_key(path) {
                continue; // pending or failed; handled through the normal paths
            }
            let Ok(physical) = self.resolve_physical(path) else {
                continue;
            };
            let Ok(absolute) = moni_core::workspace_path(&physical) else {
                continue;
            };
            let Ok(current) = fingerprint(&absolute) else {
                continue;
            };
            match self.fingerprints.get(path) {
                None => {
                    self.fingerprints.insert(path.clone(), current);
                }
                Some(stored) if *stored != current => {
                    changed.push((path.clone(), current));
                }
                _ => {}
            }
        }
        changed
    }

    pub fn set_references(&mut self, references: BTreeMap<String, usize>) -> anyhow::Result<()> {
        let removed: Vec<String> = self
            .references
            .keys()
            .filter(|path| !references.contains_key(*path))
            .cloned()
            .collect();
        for path in removed {
            self.unload(&path);
        }
        self.references = references;
        Ok(())
    }

    /// Returns the cached mesh with the revision it was loaded at, or `None`
    /// while a (re)load is pending or the source failed.
    pub fn handle(&self, path: &str) -> Option<AssetHandle> {
        self.cache.get(path).map(|mesh| AssetHandle {
            mesh: Arc::clone(mesh),
            revision: self.revisions.get(path).copied().unwrap_or(1),
        })
    }

    pub fn error(&self, path: &str) -> Option<&str> {
        self.errors.get(path).map(String::as_str)
    }

    fn unload(&mut self, path: &str) {
        self.cache.remove(path);
        self.errors.remove(path);
        self.fingerprints.remove(path);
        self.revisions.remove(path);
        if let Some(id) = self.ids.remove(path)
            && let Some(pending) = self.pending.remove(&id)
        {
            pending.cancel.store(true, Ordering::Release);
        }
    }

    pub fn shutdown(&mut self) {
        if self.shutdown {
            return;
        }
        self.shutdown = true;
        for pending in self.pending.values() {
            pending.cancel.store(true, Ordering::Release);
        }
        let _ = self.sender.send(AssetCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.pending.clear();
        self.ids.clear();
        self.cache.clear();
        self.references.clear();
        self.errors.clear();
        self.fingerprints.clear();
        self.revisions.clear();
    }
}

impl Drop for AssetRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(receiver: Receiver<AssetCommand>, sender: SyncSender<AssetResponse>) {
    while let Ok(command) = receiver.recv() {
        match command {
            AssetCommand::Load(request) => {
                if request.cancel.load(Ordering::Acquire) {
                    continue;
                }
                let result = match request.kind {
                    AssetKind::Fbx => crate::fbx::load_fbx(std::path::Path::new(&request.path)),
                    AssetKind::Cooked => {
                        crate::cooked::read_cooked_mesh(std::path::Path::new(&request.path))
                    }
                }
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
                if request.cancel.load(Ordering::Acquire) {
                    continue;
                }
                let _ = sender.try_send(AssetResponse {
                    id: request.id,
                    result,
                });
            }
            AssetCommand::Shutdown => break,
        }
    }
}

fn asset_kind(path: &str) -> anyhow::Result<AssetKind> {
    let lowercase = path.to_ascii_lowercase();
    if lowercase.ends_with(".fbx") {
        Ok(AssetKind::Fbx)
    } else if lowercase.ends_with(".mnec") {
        Ok(AssetKind::Cooked)
    } else {
        anyhow::bail!("unsupported mesh asset '{path}'; expected .fbx or .mnec")
    }
}

/// Cheap content proxy: mtime plus size catches developer edits without
/// hashing whole files on the frame path.
fn fingerprint(path: &std::path::Path) -> anyhow::Result<(SystemTime, u64)> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.modified()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cooked::write_cooked_mesh;
    use crate::fbx::{FbxMesh, FbxVertex};

    fn mesh_with_vertices(count: usize) -> FbxMesh {
        FbxMesh {
            vertices: (0..count)
                .map(|index| FbxVertex {
                    position: [index as f32, 0.0, 0.0],
                    normal: [0.0, 1.0, 0.0],
                    color: Some([1.0, 0.0, 0.0, 1.0]),
                })
                .collect(),
        }
    }

    fn temp_asset_path(label: &str) -> String {
        let directory = format!("target/testing/asset-test-{}", std::process::id());
        fs::create_dir_all(&directory).expect("create temp asset directory");
        format!("{directory}/{label}.mnec")
    }

    fn wait_for<F: FnMut() -> bool>(mut condition: F) {
        for _ in 0..2_000 {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("asset condition never became true");
    }

    #[test]
    fn unknown_virtual_alias_is_rejected() {
        let mut runtime = AssetRuntime::new();
        let error = runtime
            .load("res://missing")
            .expect_err("unknown alias must fail");
        assert!(error.to_string().contains("unknown virtual asset path"));
    }

    #[test]
    fn virtual_alias_loads_and_reports_revision() {
        let path = temp_asset_path("alias-basic");
        write_cooked_mesh(std::path::Path::new(&path), &mesh_with_vertices(4))
            .expect("write cooked fixture");
        let mut runtime = AssetRuntime::new();
        runtime.set_aliases(&BTreeMap::from([("mesh".to_string(), path.clone())]));
        runtime
            .set_references(BTreeMap::from([("res://mesh".to_string(), 1)]))
            .expect("set references");
        runtime.load("res://mesh").expect("queue alias load");
        wait_for(|| {
            let _ = runtime.poll();
            runtime.handle("res://mesh").is_some()
        });
        let handle = runtime.handle("res://mesh").expect("mesh loaded");
        assert_eq!(handle.mesh.vertices.len(), 4);
        assert_eq!(handle.revision, 1);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn hot_reload_swaps_to_a_new_revision() {
        let path = temp_asset_path("alias-reload");
        write_cooked_mesh(std::path::Path::new(&path), &mesh_with_vertices(4))
            .expect("write cooked fixture");
        let mut runtime = AssetRuntime::new_with_options(true);
        runtime.set_aliases(&BTreeMap::from([("mesh".to_string(), path.clone())]));
        runtime
            .set_references(BTreeMap::from([("res://mesh".to_string(), 1)]))
            .expect("set references");
        runtime.load("res://mesh").expect("queue alias load");
        wait_for(|| {
            let _ = runtime.poll();
            runtime.handle("res://mesh").is_some()
        });
        assert_eq!(runtime.handle("res://mesh").unwrap().revision, 1);

        // Rewrite the source with a different vertex count so both mtime and
        // size change; then poll until the replacement revision arrives.
        write_cooked_mesh(std::path::Path::new(&path), &mesh_with_vertices(9))
            .expect("rewrite cooked fixture");
        wait_for(|| {
            runtime.poll().is_ok()
                && runtime
                    .handle("res://mesh")
                    .is_some_and(|handle| handle.revision > 1)
        });
        let handle = runtime.handle("res://mesh").expect("reloaded mesh");
        assert_eq!(handle.mesh.vertices.len(), 9);
        assert_eq!(handle.revision, 2);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn without_hot_reload_source_changes_are_ignored() {
        let path = temp_asset_path("alias-static");
        write_cooked_mesh(std::path::Path::new(&path), &mesh_with_vertices(4))
            .expect("write cooked fixture");
        let mut runtime = AssetRuntime::new();
        runtime.set_aliases(&BTreeMap::from([("mesh".to_string(), path.clone())]));
        runtime
            .set_references(BTreeMap::from([("res://mesh".to_string(), 1)]))
            .expect("set references");
        runtime.load("res://mesh").expect("queue alias load");
        wait_for(|| {
            let _ = runtime.poll();
            runtime.handle("res://mesh").is_some()
        });
        write_cooked_mesh(std::path::Path::new(&path), &mesh_with_vertices(9))
            .expect("rewrite cooked fixture");
        for _ in 0..20 {
            runtime.poll().expect("poll");
        }
        let handle = runtime.handle("res://mesh").expect("mesh still cached");
        assert_eq!(handle.revision, 1);
        assert_eq!(handle.mesh.vertices.len(), 4);
        fs::remove_file(&path).ok();
    }
}
