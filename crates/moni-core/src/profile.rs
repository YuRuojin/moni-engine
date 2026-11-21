use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::host::PluginError;

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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Comparator {
    GreaterEqual,
    LessEqual,
    Greater,
    Less,
    Exact,
    Caret,
}

#[derive(Debug, Clone)]
struct VersionConstraint {
    comparators: Vec<(Comparator, [u32; 3])>,
}

fn parse_version(value: &str) -> Result<[u32; 3], PluginError> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 || value.is_empty() {
        return Err(PluginError::new(format!(
            "version '{value}' must be three dot-separated integers (X.Y.Z)"
        )));
    }
    let mut parsed = [0_u32; 3];
    for (index, part) in parts.iter().enumerate() {
        parsed[index] = part
            .parse()
            .map_err(|_| PluginError::new(format!("version '{value}' has an invalid component")))?;
    }
    Ok(parsed)
}

/// Supported requirement forms: `*` (any), `X.Y.Z` (exact), `^X.Y.Z`
/// (semver caret: same major, or for 0.x the same minor), and
/// comma-separated `>=`/`<=`/`>`/`<`/`=` comparators. Requirements that fail
/// to parse reject every version (fail closed).
fn parse_version_constraint(requirement: &str) -> Result<VersionConstraint, PluginError> {
    let requirement = requirement.trim();
    if requirement.is_empty() || requirement.len() > 64 {
        return Err(PluginError::new(
            "version requirement must be 1-64 characters",
        ));
    }
    if requirement == "*" {
        return Ok(VersionConstraint {
            comparators: Vec::new(),
        });
    }
    let mut comparators = Vec::new();
    for part in requirement.split(',') {
        let part = part.trim();
        let (comparator, version) = if let Some(rest) = part.strip_prefix(">=") {
            (Comparator::GreaterEqual, rest)
        } else if let Some(rest) = part.strip_prefix("<=") {
            (Comparator::LessEqual, rest)
        } else if let Some(rest) = part.strip_prefix('>') {
            (Comparator::Greater, rest)
        } else if let Some(rest) = part.strip_prefix('<') {
            (Comparator::Less, rest)
        } else if let Some(rest) = part.strip_prefix('=') {
            (Comparator::Exact, rest)
        } else if let Some(rest) = part.strip_prefix('^') {
            (Comparator::Caret, rest)
        } else {
            (Comparator::Exact, part)
        };
        comparators.push((comparator, parse_version(version)?));
    }
    Ok(VersionConstraint { comparators })
}

fn caret_upper_bound(version: [u32; 3]) -> [u32; 3] {
    match version {
        [0, 0, patch] => [0, 0, patch + 1],
        [0, minor, _] => [0, minor + 1, 0],
        [major, _, _] => [major + 1, 0, 0],
    }
}

/// Requirement parsing failures reject every version, so a malformed
/// manifest can never satisfy a dependency check.
pub fn version_matches(requirement: &str, version: &str) -> Result<bool, PluginError> {
    let constraint = parse_version_constraint(requirement)?;
    let version = parse_version(version)?;
    for (comparator, bound) in &constraint.comparators {
        let satisfied = match comparator {
            Comparator::GreaterEqual => version >= *bound,
            Comparator::LessEqual => version <= *bound,
            Comparator::Greater => version > *bound,
            Comparator::Less => version < *bound,
            Comparator::Exact => version == *bound,
            Comparator::Caret => version >= *bound && version < caret_upper_bound(*bound),
        };
        if !satisfied {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Validates a version requirement without comparing it to a version.
pub fn validate_version_requirement(requirement: &str) -> Result<(), PluginError> {
    parse_version_constraint(requirement).map(|_| ())
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ProfilePatch {
    pub plugin: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Plugin configuration applied in patch order; later patches override
    /// keys set by earlier ones. Only valid on plugins with instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

fn default_enabled() -> bool {
    true
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
            "serve" => vec!["serve"],
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
            config: None,
        });
        self
    }

    /// Adds an ordered configuration patch; applied after any earlier config
    /// patches for the same plugin, with later keys overriding earlier ones.
    pub fn with_config(mut self, plugin: &str, config: Value) -> Self {
        self.patches.push(ProfilePatch {
            plugin: plugin.to_string(),
            enabled: true,
            config: Some(config),
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
        let mut configs: BTreeMap<String, Value> = BTreeMap::new();
        for patch in &self.patches {
            if !registry.contains_key(&patch.plugin) {
                return Err(PluginError::new(format!(
                    "patch '{}' references an unknown plugin",
                    patch.plugin
                )));
            }
            match (&patch.config, patch.enabled) {
                (Some(_), false) => {
                    return Err(PluginError::new(format!(
                        "patch sets config for '{}' but also disables it",
                        patch.plugin
                    )));
                }
                (Some(config), true) => {
                    let entry = configs
                        .entry(patch.plugin.clone())
                        .or_insert_with(|| json!({}));
                    if let (Some(existing), Some(incoming)) =
                        (entry.as_object_mut(), config.as_object())
                    {
                        for (key, value) in incoming {
                            existing.insert(key.clone(), value.clone());
                        }
                    } else {
                        *entry = config.clone();
                    }
                }
                (None, _) => {}
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
                config: configs.remove(&manifest.id),
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
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectivePlugin {
    manifest: PluginManifest,
    modules: Vec<ModuleDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
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

    /// Effective plugin entries in dependency order; the composition root
    /// iterates these to construct instances.
    pub fn plugins(&self) -> &[EffectivePlugin] {
        &self.plugins
    }

    /// Configuration merged from ordered patches for the given plugin.
    pub fn config_for(&self, id: &str) -> Option<&Value> {
        self.plugins
            .iter()
            .find(|plugin| plugin.manifest.id == id)
            .and_then(|plugin| plugin.config.as_ref())
    }

    pub fn dump(&self) -> Value {
        json!(self)
    }
}

impl EffectivePlugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn permissions(&self) -> &[String] {
        &self.manifest.permissions
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
            let dependency_manifest = self.registry.get(&dependency.id).ok_or_else(|| {
                PluginError::new(format!(
                    "plugin '{id}' requires missing plugin '{}'",
                    dependency.id
                ))
            })?;
            if dependency.optional && !self.requested.contains(&dependency.id) {
                continue;
            }
            if !version_matches(&dependency.version, &dependency_manifest.version)? {
                return Err(PluginError::new(format!(
                    "plugin '{id}' requires '{}' version {} but registry provides '{}'",
                    dependency.id, dependency.version, dependency_manifest.version
                )));
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
        "serve" => Ok(&["core.simulation"]),
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
                version: "^0.1.0".into(),
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
                version: "^0.1.0".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constraints_follow_the_documented_subset() {
        let check = |requirement: &str, version: &str| {
            version_matches(requirement, version).expect("valid requirement")
        };
        assert!(check("*", "0.1.0"));
        assert!(check("*", "9.9.9"));
        assert!(check("0.1.0", "0.1.0"));
        assert!(!check("0.1.0", "0.1.1"));
        assert!(check("^0.1.0", "0.1.0"));
        assert!(check("^0.1.0", "0.1.9"));
        assert!(!check("^0.1.0", "0.2.0"));
        assert!(check("^1.2.3", "1.2.3"));
        assert!(check("^1.2.3", "1.9.0"));
        assert!(!check("^1.2.3", "2.0.0"));
        assert!(check(">=0.2.0, <0.3.0", "0.2.5"));
        assert!(!check(">=0.2.0, <0.3.0", "0.1.9"));
        assert!(!check(">=0.2.0, <0.3.0", "0.3.0"));
        assert!(version_matches("not-a-version", "0.1.0").is_err());
        assert!(version_matches("^abc", "0.1.0").is_err());
        assert!(validate_version_requirement("*").is_ok());
        assert!(validate_version_requirement("").is_err());
    }

    #[test]
    fn resolve_rejects_config_on_disabled_plugins() {
        let mut profile = Profile::named("default").expect("default profile");
        profile.patches.push(ProfilePatch {
            plugin: "core.simulation".to_string(),
            enabled: false,
            config: Some(json!({ "tickRate": 60 })),
        });
        let error = profile
            .resolve(&Target::current())
            .expect_err("config on disabled plugin must fail");
        assert!(error.to_string().contains("also disables"));
    }
}
