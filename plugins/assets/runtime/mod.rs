use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::fbx::FbxMesh;

pub const MAX_ASSET_QUEUE: usize = 64;
pub const ASSET_TIMEOUT: Duration = Duration::from_secs(10);

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
    next_id: u64,
    shutdown: bool,
}

impl AssetRuntime {
    pub fn new() -> Self {
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
            next_id: 1,
            shutdown: false,
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
        let absolute = crate::workspace_path(path)?;
        let kind = asset_kind(path)?;
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
                    self.cache.insert(path, mesh);
                }
                Err(error) => {
                    self.errors.insert(path, error);
                }
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

    pub fn get(&self, path: &str) -> Option<Arc<FbxMesh>> {
        self.cache.get(path).cloned()
    }

    pub fn error(&self, path: &str) -> Option<&str> {
        self.errors.get(path).map(String::as_str)
    }

    fn unload(&mut self, path: &str) {
        self.cache.remove(path);
        self.errors.remove(path);
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
