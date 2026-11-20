use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::profile::{PluginManifest, Profile, ProfilePatch};

pub const PROJECT_FORMAT: &str = "moni-game-project";
pub const PROJECT_VERSION: u32 = 1;
pub const LOCK_FORMAT: &str = "moni-project-lock";
pub const LOCK_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameProject {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub engine: String,
    pub profile: String,
    pub scene: String,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub patches: Vec<ProfilePatch>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, ProjectDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDependency {
    pub version: String,
}

impl GameProject {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join("game.mngp");
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("could not read project {}", path.display()))?;
        let project: Self = serde_json::from_str(&contents)
            .with_context(|| format!("could not parse project {}", path.display()))?;
        project.validate()?;
        Ok(project)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format != PROJECT_FORMAT {
            bail!("project format must be '{PROJECT_FORMAT}'");
        }
        if self.version != PROJECT_VERSION {
            bail!("unsupported project version {}; expected 1", self.version);
        }
        if self.name.trim().is_empty() {
            bail!("project name must not be empty");
        }
        if self.engine != env!("CARGO_PKG_VERSION") {
            bail!(
                "project requires engine {} but current engine is {}",
                self.engine,
                env!("CARGO_PKG_VERSION")
            );
        }
        validate_relative_path(&self.scene, "scene")?;
        Profile::named(&self.profile)?;
        Ok(())
    }

    pub fn profile(&self) -> anyhow::Result<Profile> {
        let mut profile = Profile::named(&self.profile)?;
        profile.plugins = self.plugins.clone();
        profile.patches = self.patches.clone();
        Ok(profile)
    }

    pub fn scene_path(&self, root: &Path) -> anyhow::Result<PathBuf> {
        validate_relative_path(&self.scene, "scene")?;
        Ok(root.join(&self.scene))
    }

    pub fn lock_value(&self) -> anyhow::Result<Value> {
        let effective = self
            .profile()?
            .resolve(&crate::profile::Target::current())?;
        let plugins: serde_json::Map<String, Value> = effective
            .manifests()
            .map(|manifest| {
                (
                    manifest.id.clone(),
                    json!({
                        "version": manifest.version,
                        "apiVersion": manifest.api_version,
                        "permissions": manifest.permissions,
                        "resources": manifest.resources,
                        "integrity": manifest_integrity(manifest),
                    }),
                )
            })
            .collect();
        Ok(json!({
            "format": LOCK_FORMAT,
            "version": LOCK_VERSION,
            "engine": self.engine,
            "profile": self.profile,
            "plugins": plugins,
            "directDependencies": self.dependencies,
        }))
    }

    pub fn write_lockfile(&self, root: &Path) -> anyhow::Result<()> {
        let value = self.lock_value()?;
        let path = root.join("moni.lock");
        if path.is_file() {
            fs::copy(&path, root.join("moni.lock.backup"))?;
        }
        fs::write(path, serde_json::to_string_pretty(&value)? + "\n")?;
        Ok(())
    }

    pub fn rollback_lockfile(&self, root: &Path) -> anyhow::Result<()> {
        let backup = root.join("moni.lock.backup");
        if !backup.is_file() {
            bail!("no lockfile backup exists");
        }
        fs::copy(&backup, root.join("moni.lock"))?;
        self.check_lockfile(root)
    }

    pub fn permission_diff(&self, root: &Path) -> anyhow::Result<Value> {
        let expected = self.lock_value()?;
        let path = root.join("moni.lock");
        let actual: Value = if path.is_file() {
            serde_json::from_str(&fs::read_to_string(path)?)?
        } else {
            json!({ "plugins": {} })
        };
        let mut added = BTreeMap::<String, BTreeSet<String>>::new();
        let mut removed = BTreeMap::<String, BTreeSet<String>>::new();
        let expected_plugins = expected["plugins"].as_object().cloned().unwrap_or_default();
        let actual_plugins = actual["plugins"].as_object().cloned().unwrap_or_default();
        let ids: BTreeSet<String> = expected_plugins
            .keys()
            .chain(actual_plugins.keys())
            .cloned()
            .collect();
        for id in ids {
            let expected_permissions = plugin_permissions(expected_plugins.get(&id));
            let actual_permissions = plugin_permissions(actual_plugins.get(&id));
            let added_set: BTreeSet<String> = expected_permissions
                .difference(&actual_permissions)
                .cloned()
                .collect();
            let removed_set: BTreeSet<String> = actual_permissions
                .difference(&expected_permissions)
                .cloned()
                .collect();
            if !added_set.is_empty() {
                added.insert(id.clone(), added_set);
            }
            if !removed_set.is_empty() {
                removed.insert(id.clone(), removed_set);
            }
        }
        Ok(json!({ "added": added, "removed": removed }))
    }

    pub fn check_lockfile(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("moni.lock");
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("could not read project lockfile {}", path.display()))?;
        let actual: Value = serde_json::from_str(&contents)?;
        let expected = self.lock_value()?;
        if actual != expected {
            bail!("moni.lock is stale or incompatible; run moni doctor --update-lock");
        }
        Ok(())
    }
}

fn manifest_integrity(manifest: &PluginManifest) -> String {
    let serialized = serde_json::to_vec(manifest).expect("manifest serialization cannot fail");
    let digest = Sha256::digest(&serialized);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn plugin_permissions(plugin: Option<&Value>) -> BTreeSet<String> {
    plugin
        .and_then(|plugin| plugin["permissions"].as_array())
        .map(|permissions| {
            permissions
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn copy_template(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if destination.exists() {
        let mut entries = fs::read_dir(destination)
            .with_context(|| format!("could not inspect {}", destination.display()))?;
        if entries.next().is_some() {
            bail!(
                "project destination must be empty: {}",
                destination.display()
            );
        }
    }
    copy_directory(source, destination)
}

fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn validate_relative_path(value: &str, field: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("project {field} path must not be empty");
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        bail!("project {field} path must stay inside the project");
    }
    Ok(())
}
