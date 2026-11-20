use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use winit::window::Window;

use crate::host::{PluginError, PluginHandle, PluginHost};
use crate::plugins::{RenderPlugin, SimulationPlugin};

pub const PLUGIN_API_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleDomain {
    Runtime,
    Editor,
    Build,
    Developer,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildConfiguration {
    Debug,
    Release,
    Shipping,
    Test,
}

impl BuildConfiguration {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub os: String,
    pub arch: String,
    pub configuration: BuildConfiguration,
}

impl Target {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            configuration: BuildConfiguration::current(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleDescriptor {
    pub domain: ModuleDomain,
    pub platforms: Vec<String>,
    pub architectures: Vec<String>,
    pub configurations: Vec<BuildConfiguration>,
}

impl ModuleDescriptor {
    fn matches(&self, target: &Target) -> bool {
        (self.platforms.is_empty() || self.platforms.contains(&target.os))
            && (self.architectures.is_empty() || self.architectures.contains(&target.arch))
            && (self.configurations.is_empty()
                || self.configurations.contains(&target.configuration))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginDependency {
    pub id: String,
    pub version: String,
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub api_version: u32,
    pub dependencies: Vec<PluginDependency>,
    pub conflicts: Vec<String>,
    pub permissions: Vec<String>,
    pub resources: Vec<String>,
    pub modules: Vec<ModuleDescriptor>,
}

impl PluginManifest {
    fn validate(&self) -> Result<(), PluginError> {
        if self.id.is_empty() || self.version.is_empty() {
            return Err(PluginError::new("plugin ID and version must not be empty"));
        }
        if self.api_version != PLUGIN_API_VERSION {
            return Err(PluginError::new(format!(
                "plugin '{}' requires API {} but host provides {}",
                self.id, self.api_version, PLUGIN_API_VERSION
            )));
        }
        if self.modules.is_empty() {
            return Err(PluginError::new(format!(
                "plugin '{}' declares no modules",
                self.id
            )));
        }
        let mut dependency_ids = BTreeSet::new();
        for dependency in &self.dependencies {
            if dependency.id == self.id {
                return Err(PluginError::new(format!(
                    "plugin '{}' depends on itself",
                    self.id
                )));
            }
            if !dependency_ids.insert(&dependency.id) {
                return Err(PluginError::new(format!(
                    "plugin '{}' declares dependency '{}' twice",
                    self.id, dependency.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ProfilePatch {
    pub plugin: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub bundles: Vec<String>,
    pub plugins: Vec<String>,
    pub patches: Vec<ProfilePatch>,
    pub revoked_permissions: Vec<(String, String)>,
}

impl Profile {
    pub fn named(name: &str) -> Result<Self, PluginError> {
        let bundles = match name {
            "minimal" => vec!["minimal"],
            "default" => vec!["default"],
            "headless" => vec!["headless"],
            "editor" => vec!["editor"],
            _ => return Err(PluginError::new(format!("unknown profile '{name}'"))),
        };
        Ok(Self {
            name: name.to_string(),
            bundles: bundles.into_iter().map(str::to_string).collect(),
            plugins: Vec::new(),
            patches: Vec::new(),
            revoked_permissions: Vec::new(),
        })
    }

    pub fn with_patch(mut self, plugin: &str, enabled: bool) -> Self {
        self.patches.push(ProfilePatch {
            plugin: plugin.to_string(),
            enabled,
        });
        self
    }

    pub fn with_revoked_permission(mut self, plugin: &str, permission: &str) -> Self {
        self.revoked_permissions
            .push((plugin.to_string(), permission.to_string()));
        self
    }

    pub fn resolve(&self, target: &Target) -> Result<EffectiveProfile, PluginError> {
        let registry = static_registry();
        let mut requested = BTreeSet::new();
        for bundle in &self.bundles {
            for plugin in static_bundle(bundle)? {
                requested.insert(plugin.to_string());
            }
        }
        requested.extend(self.plugins.iter().cloned());
        let mut enabled = requested.clone();
        for patch in &self.patches {
            if !registry.contains_key(&patch.plugin) {
                return Err(PluginError::new(format!(
                    "patch '{}' references an unknown plugin",
                    patch.plugin
                )));
            }
            if patch.enabled {
                enabled.insert(patch.plugin.clone());
            } else {
                enabled.remove(&patch.plugin);
            }
        }

        let mut state = ResolveState {
            registry: &registry,
            requested: &enabled,
            visiting: Vec::new(),
            ordered: Vec::new(),
            visited: BTreeSet::new(),
        };
        for id in &enabled {
            state.include(id)?;
        }
        let mut plugins = Vec::new();
        for id in state.ordered {
            let mut manifest = registry
                .get(&id)
                .expect("dependency resolver only returns registered plugins")
                .clone();
            for (plugin_id, permission) in &self.revoked_permissions {
                if plugin_id == &manifest.id {
                    if !manifest.permissions.contains(permission) {
                        return Err(PluginError::new(format!(
                            "cannot revoke undeclared permission '{permission}' from '{plugin_id}'"
                        )));
                    }
                    manifest.permissions.retain(|value| value != permission);
                }
            }
            let modules: Vec<ModuleDescriptor> = manifest
                .modules
                .iter()
                .filter(|module| module.matches(target))
                .cloned()
                .collect();
            if modules.is_empty() {
                continue;
            }
            plugins.push(EffectivePlugin {
                manifest: manifest.clone(),
                modules,
            });
        }
        let included: BTreeSet<String> = plugins
            .iter()
            .map(|plugin| plugin.manifest.id.clone())
            .collect();
        for plugin in &plugins {
            for conflict in &plugin.manifest.conflicts {
                if included.contains(conflict) {
                    return Err(PluginError::new(format!(
                        "plugin '{}' conflicts with '{conflict}'",
                        plugin.manifest.id
                    )));
                }
            }
        }
        Ok(EffectiveProfile {
            name: self.name.clone(),
            target: target.clone(),
            plugins,
        })
    }

    pub fn instantiate(
        &self,
        host: &mut PluginHost,
        window: Option<Arc<Window>>,
        collect_gpu_timestamps: bool,
    ) -> Result<Vec<PluginHandle>, PluginError> {
        let effective = self.resolve(&Target::current())?;
        let mut handles = Vec::new();
        for plugin in &effective.plugins {
            match plugin.manifest.id.as_str() {
                "core.simulation" => {
                    handles.push(host.register_plugin_with_permissions(
                        Box::new(SimulationPlugin),
                        &plugin.manifest.permissions,
                    )?);
                }
                "core.render" => {
                    let window = window.clone().ok_or_else(|| {
                        PluginError::new("core.render requires a window but headless was requested")
                    })?;
                    handles.push(host.register_plugin_with_permissions(
                        Box::new(RenderPlugin::new(
                            window,
                            collect_gpu_timestamps,
                            effective.contains("core.fbx"),
                        )),
                        &plugin.manifest.permissions,
                    )?);
                }
                "core.fbx" => {
                    host.grant_permissions("core.fbx", &plugin.manifest.permissions);
                }
                "core.editor" => {}
                id => {
                    return Err(PluginError::new(format!(
                        "no static factory registered for plugin '{id}'"
                    )));
                }
            }
        }
        Ok(handles)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectivePlugin {
    manifest: PluginManifest,
    modules: Vec<ModuleDescriptor>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveProfile {
    name: String,
    target: Target,
    plugins: Vec<EffectivePlugin>,
}

impl EffectiveProfile {
    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.plugins.iter().map(|plugin| &plugin.manifest)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.plugins.iter().any(|plugin| plugin.manifest.id == id)
    }

    pub fn dump(&self) -> Value {
        json!(self)
    }
}

struct ResolveState<'a> {
    registry: &'a BTreeMap<String, PluginManifest>,
    requested: &'a BTreeSet<String>,
    visiting: Vec<String>,
    ordered: Vec<String>,
    visited: BTreeSet<String>,
}

impl ResolveState<'_> {
    fn include(&mut self, id: &str) -> Result<(), PluginError> {
        if !self.requested.contains(id) {
            return Err(PluginError::new(format!(
                "required plugin '{id}' was disabled by profile patch"
            )));
        }
        if self.visited.contains(id) {
            return Ok(());
        }
        if self.visiting.iter().any(|item| item == id) {
            let mut cycle = self.visiting.clone();
            cycle.push(id.to_string());
            return Err(PluginError::new(format!(
                "plugin dependency cycle: {}",
                cycle.join(" -> ")
            )));
        }
        let manifest = self
            .registry
            .get(id)
            .ok_or_else(|| PluginError::new(format!("missing required plugin '{id}'")))?;
        manifest.validate()?;
        self.visiting.push(id.to_string());
        for dependency in &manifest.dependencies {
            if !self.registry.contains_key(&dependency.id) {
                return Err(PluginError::new(format!(
                    "plugin '{id}' requires missing plugin '{}'",
                    dependency.id
                )));
            }
            if dependency.optional && !self.requested.contains(&dependency.id) {
                continue;
            }
            self.include(&dependency.id)?;
        }
        self.visiting.pop();
        self.visited.insert(id.to_string());
        self.ordered.push(id.to_string());
        Ok(())
    }
}

fn static_bundle(name: &str) -> Result<&'static [&'static str], PluginError> {
    match name {
        "minimal" | "headless" => Ok(&[]),
        "default" => Ok(&["core.simulation", "core.render", "core.fbx"]),
        "editor" => Ok(&["core.render", "core.fbx", "core.editor"]),
        _ => Err(PluginError::new(format!("unknown bundle '{name}'"))),
    }
}

fn static_registry() -> BTreeMap<String, PluginManifest> {
    let manifests = [
        PluginManifest {
            id: "core.simulation".into(),
            version: "0.1.0".into(),
            api_version: PLUGIN_API_VERSION,
            dependencies: vec![],
            conflicts: vec![],
            permissions: vec!["script.execute".into()],
            resources: vec!["quickjs".into()],
            modules: vec![module(ModuleDomain::Runtime)],
        },
        PluginManifest {
            id: "core.render".into(),
            version: "0.1.0".into(),
            api_version: PLUGIN_API_VERSION,
            dependencies: vec![PluginDependency {
                id: "core.fbx".into(),
                version: "*".into(),
                optional: true,
            }],
            conflicts: vec![],
            permissions: vec!["gpu.create".into()],
            resources: vec!["wgpu".into()],
            modules: vec![module(ModuleDomain::Runtime), module(ModuleDomain::Editor)],
        },
        PluginManifest {
            id: "core.fbx".into(),
            version: "0.1.0".into(),
            api_version: PLUGIN_API_VERSION,
            dependencies: vec![],
            conflicts: vec![],
            permissions: vec!["asset.read.workspace".into()],
            resources: vec!["fbx".into()],
            modules: vec![
                module(ModuleDomain::Runtime),
                module(ModuleDomain::Editor),
                module(ModuleDomain::Build),
                module(ModuleDomain::Developer),
                module(ModuleDomain::Test),
            ],
        },
        PluginManifest {
            id: "core.editor".into(),
            version: "0.1.0".into(),
            api_version: PLUGIN_API_VERSION,
            dependencies: vec![PluginDependency {
                id: "core.render".into(),
                version: "*".into(),
                optional: false,
            }],
            conflicts: vec![],
            permissions: vec!["editor.view".into()],
            resources: vec!["scene-editor".into()],
            modules: vec![module(ModuleDomain::Editor)],
        },
    ];
    manifests
        .into_iter()
        .map(|manifest| (manifest.id.clone(), manifest))
        .collect()
}

fn module(domain: ModuleDomain) -> ModuleDescriptor {
    ModuleDescriptor {
        domain,
        platforms: vec![],
        architectures: vec![],
        configurations: vec![],
    }
}
