//! Project support for the editor: the `game.mngp` project the editor works
//! against, the recent-projects list persisted under the user config
//! directory, and in-app project creation. The composition root resolves the
//! startup project and passes it to the UI; the panels here handle opening,
//! creating, and remembering projects at runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};

/// The project the editor is working against. `None` everywhere means
/// workspace mode (no `game.mngp` involved).
#[derive(Debug, Clone)]
pub struct EditorProject {
    /// Absolute project root. In project mode the process working directory
    /// points here, so every workspace-relative path (documents, assets,
    /// scripts, saves) resolves inside the project.
    pub root: PathBuf,
    /// Project name from `game.mngp`.
    pub name: String,
    /// Project-relative main scene (`game.mngp` `scene` field); the default
    /// document scene in project mode.
    pub main_scene: String,
    /// Static profile name the project requests.
    pub profile_name: String,
}

impl EditorProject {
    /// Loads and validates the project at `root` before the caller changes
    /// the working directory: `game.mngp` must exist, parse, and validate,
    /// and the main scene file must exist under the project root. Loading
    /// never changes the working directory itself; the caller chdirs only
    /// after this succeeded so a failed open leaves the process untouched.
    pub fn load_at(root: &Path) -> anyhow::Result<Self> {
        if !root.join("game.mngp").is_file() {
            bail!(
                "{} is not a moni project: game.mngp is missing",
                root.display()
            );
        }
        let project = moni_core::project::GameProject::load(root)?;
        let scene_path = project.scene_path(root)?;
        if !scene_path.is_file() {
            bail!("project main scene {} is missing", scene_path.display());
        }
        Ok(Self {
            root: root.to_path_buf(),
            name: project.name,
            main_scene: project.scene,
            profile_name: project.profile,
        })
    }
}

/// Bounded recent-projects list length; older entries are dropped.
pub const RECENT_LIMIT: usize = 10;

/// One recent project: absolute root path and project name.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
}

/// The user config directory: `%APPDATA%` on Windows, `$HOME` elsewhere.
/// `None` disables recents (the panel simply shows an empty list).
pub fn user_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// `<config dir>/.moni/editor-recent.json`.
fn recent_file(config_dir: &Path) -> PathBuf {
    config_dir.join(".moni").join("editor-recent.json")
}

/// Loads the recent-projects list. The file is user data, so any missing,
/// unreadable, or malformed content degrades to an empty list instead of
/// failing the editor.
pub fn load_recent_projects(config_dir: &Path) -> Vec<RecentProject> {
    let Ok(contents) = fs::read_to_string(recent_file(config_dir)) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&contents) else {
        return Vec::new();
    };
    value
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let path = entry.get("path")?.as_str()?.to_string();
                    let name = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(RecentProject { path, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Records a successfully opened project: existing entries for the same path
/// move to the front, the list stays bounded at [`RECENT_LIMIT`], and the
/// updated list is returned for the panel's cache. Callers only record
/// opens that fully succeeded (project verified, working directory moved,
/// scene opened); failed opens change nothing.
pub fn record_recent_project(
    config_dir: &Path,
    path: &str,
    name: &str,
) -> anyhow::Result<Vec<RecentProject>> {
    // Normalize separators so the same project recorded through different
    // Windows path shapes (`C:\x` vs `C:/x`) deduplicates to one entry.
    let normalized = path.replace('\\', "/");
    let mut entries: Vec<RecentProject> = load_recent_projects(config_dir)
        .into_iter()
        .filter(|entry| entry.path != normalized)
        .collect();
    entries.insert(
        0,
        RecentProject {
            path: normalized,
            name: name.to_string(),
        },
    );
    entries.truncate(RECENT_LIMIT);
    let file = recent_file(config_dir);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let value = json!({
        "entries": entries
            .iter()
            .map(|entry| json!({ "path": entry.path, "name": entry.name }))
            .collect::<Vec<_>>(),
    });
    fs::write(&file, serde_json::to_string_pretty(&value)? + "\n")
        .with_context(|| format!("could not write {}", file.display()))?;
    Ok(entries)
}

/// How long `moni new` may run before the editor kills it: template copying
/// and `git init` are normally sub-second, so a hung child is an error.
const NEW_PROJECT_TIMEOUT_SECS: u64 = 30;

/// Creates a project by spawning `moni new <path>` as a child process (the
/// same timeout/status pattern as the isolated FBX importer in the
/// composition root), so template copying and `git init` cannot hang or
/// crash the editor process. The caller opens the project afterwards.
pub fn spawn_new_project(path: &Path) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("could not locate the editor executable")?;
    let mut child = ProcessCommand::new(executable)
        .arg("new")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start project creation")?;
    let deadline = Instant::now() + Duration::from_secs(NEW_PROJECT_TIMEOUT_SECS);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("project creation failed with status {status}");
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("project creation timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod project_tests;
