use std::env;
use std::path::{Component, Path, PathBuf};

/// Resolves a workspace-relative path, rejecting absolute paths, drive
/// prefixes, and parent-directory escapes so assets and scenes can never
/// reach outside the current workspace.
pub fn workspace_path(raw_path: &str) -> anyhow::Result<PathBuf> {
    if raw_path.trim().is_empty() {
        anyhow::bail!("path must not be empty");
    }
    let path = Path::new(raw_path);
    let has_forbidden_component = path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    if path.is_absolute() || has_forbidden_component {
        anyhow::bail!("paths must stay inside the current workspace");
    }
    Ok(env::current_dir()?.join(path))
}
