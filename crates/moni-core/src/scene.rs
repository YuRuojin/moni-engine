use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::components::{ComponentRegistry, ComponentValue, ScriptComponent};
use crate::engine::{
    Entity, EntityId, SNAPSHOT_VERSION, ScriptState, Sprite, Transform, Viewport, World,
};
use crate::scene3d::{Camera3d, Light};

pub const SCENE_FORMAT: &str = "moni-scene";
pub const SCENE_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneFile {
    format: String,
    version: u32,
    name: String,
    #[serde(default)]
    viewport: Viewport,
    camera: Camera3d,
    #[serde(default)]
    lights: Vec<Light>,
    #[serde(default)]
    extensions: BTreeMap<String, Value>,
    entities: Vec<SceneEntity>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneEntity {
    id: EntityId,
    name: String,
    components: BTreeMap<String, Value>,
}

pub fn load_scene(path: &Path) -> anyhow::Result<World> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read scene {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse scene {}", path.display()))?;
    let value = migrate_scene(value)?;
    let scene: SceneFile = serde_json::from_value(value)
        .with_context(|| format!("invalid scene structure in {}", path.display()))?;
    if scene.name.trim().is_empty() {
        bail!("scene name must not be empty in {}", path.display());
    }

    let registry = ComponentRegistry::new();
    let asset_aliases = parse_asset_aliases(&scene.extensions)?;
    let mut world = World {
        version: SNAPSHOT_VERSION,
        viewport: scene.viewport,
        camera: scene.camera,
        lights: scene.lights,
        extensions: scene.extensions,
        asset_aliases,
        ..World::default()
    };
    let mut entities = BTreeMap::new();
    let mut highest_id = 0;
    for entity in scene.entities {
        if entity.id == 0 {
            bail!("scene entity IDs must be positive in {}", path.display());
        }
        if entity.name.trim().is_empty() {
            bail!("scene entity {} has an empty name", entity.id);
        }
        let mut transform = Transform::default();
        let mut sprite: Option<Sprite> = None;
        let mut model = None;
        let mut script = None;
        let mut extensions = BTreeMap::new();
        for (id, raw) in entity.components {
            let Some(descriptor) = registry.resolve(&id) else {
                extensions.insert(id, raw);
                continue;
            };
            let component = descriptor
                .deserialize(&raw)
                .map_err(|error| anyhow::anyhow!("component '{id}': {error}"))?;
            match component {
                ComponentValue::Transform(value) => transform = value,
                ComponentValue::Sprite(value) => sprite = Some(value),
                ComponentValue::Model(value) => model = Some(value),
                ComponentValue::Script(value) => script = Some(load_script(value)?),
            }
        }
        let id = entity.id;
        let value = Entity {
            id,
            name: entity.name,
            transform,
            sprite,
            model,
            script,
            extensions,
        };
        if entities.insert(id, value).is_some() {
            bail!("duplicate scene entity ID {id} in {}", path.display());
        }
        highest_id = highest_id.max(id);
    }
    world.entities = entities;
    world.next_entity_id = highest_id.saturating_add(1).max(1);
    Ok(world)
}

/// Validates scene structure without reading external script sources, so
/// tools such as `moni doctor` can inspect a freshly created project before
/// `npm run build` produces `build/*.js`. Returns the entity count.
pub fn validate_scene_file(path: &Path) -> anyhow::Result<usize> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read scene {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse scene {}", path.display()))?;
    let value = migrate_scene(value)?;
    let scene: SceneFile = serde_json::from_value(value)
        .with_context(|| format!("invalid scene structure in {}", path.display()))?;
    if scene.name.trim().is_empty() {
        bail!("scene name must not be empty in {}", path.display());
    }
    let registry = ComponentRegistry::new();
    for entity in &scene.entities {
        if entity.id == 0 {
            bail!("scene entity IDs must be positive in {}", path.display());
        }
        if entity.name.trim().is_empty() {
            bail!("scene entity {} has an empty name", entity.id);
        }
        for (id, raw) in &entity.components {
            if let Some(descriptor) = registry.resolve(id) {
                descriptor
                    .deserialize(raw)
                    .map_err(|error| anyhow::anyhow!("component '{id}': {error}"))?;
            }
        }
    }
    Ok(scene.entities.len())
}

pub fn save_scene(world: &World, name: &str, path: &Path) -> anyhow::Result<()> {
    let value = scene_value(world, name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(&value)?)?;
    Ok(())
}

pub fn scene_value(world: &World, name: &str) -> anyhow::Result<Value> {
    if name.trim().is_empty() {
        bail!("scene name must not be empty");
    }
    let registry = ComponentRegistry::new();
    let mut extensions = world.extensions.clone();
    if !world.asset_aliases.is_empty() {
        extensions.insert(
            "res".to_string(),
            serde_json::to_value(&world.asset_aliases)?,
        );
    }
    let mut entities = Vec::new();
    for entity in world.entities.values() {
        let mut components = entity.extensions.clone();
        components.insert(
            "core.transform".to_string(),
            registry
                .resolve("core.transform")
                .expect("core.transform is registered")
                .serialize(&ComponentValue::Transform(entity.transform))
                .map_err(|error| anyhow::anyhow!(error))?,
        );
        if let Some(sprite) = &entity.sprite {
            components.insert(
                "core.sprite".to_string(),
                registry
                    .resolve("core.sprite")
                    .expect("core.sprite is registered")
                    .serialize(&ComponentValue::Sprite(sprite.clone()))
                    .map_err(|error| anyhow::anyhow!(error))?,
            );
        }
        if let Some(model) = &entity.model {
            components.insert(
                "core.model".to_string(),
                registry
                    .resolve("core.model")
                    .expect("core.model is registered")
                    .serialize(&ComponentValue::Model(model.clone()))
                    .map_err(|error| anyhow::anyhow!(error))?,
            );
        }
        if let Some(script) = &entity.script {
            let component = ScriptComponent {
                source: script.source_path.clone(),
                inline: script.source_path.is_none().then(|| script.source.clone()),
            };
            components.insert(
                "core.script".to_string(),
                registry
                    .resolve("core.script")
                    .expect("core.script is registered")
                    .serialize(&ComponentValue::Script(component))
                    .map_err(|error| anyhow::anyhow!(error))?,
            );
        }
        entities.push(SceneEntity {
            id: entity.id,
            name: entity.name.clone(),
            components,
        });
    }
    Ok(serde_json::to_value(SceneFile {
        format: SCENE_FORMAT.to_string(),
        version: SCENE_VERSION,
        name: name.to_string(),
        viewport: world.viewport.clone(),
        camera: world.camera.clone(),
        lights: world.lights.clone(),
        extensions,
        entities,
    })?)
}

pub fn migrate_scene(mut value: Value) -> anyhow::Result<Value> {
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("scene version must be an integer"))?;
    match version {
        1 => migrate_scene_v1_to_v2(&mut value),
        2 => Ok(()),
        _ => bail!("unsupported scene version {version}; expected 1 or 2"),
    }?;
    let format = value
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("scene format must be a string"))?;
    if format != SCENE_FORMAT {
        bail!("unsupported scene format '{format}'");
    }
    Ok(value)
}

fn migrate_scene_v1_to_v2(value: &mut Value) -> anyhow::Result<()> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("scene must be an object"))?;
    if root.get("format").and_then(Value::as_str) != Some(SCENE_FORMAT) {
        bail!("scene format must be '{SCENE_FORMAT}'");
    }
    root.insert("version".to_string(), json!(2));
    root.entry("extensions".to_string())
        .or_insert_with(|| json!({}));
    let entities = root
        .get_mut("entities")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("scene entities must be an array"))?;
    for entity in entities {
        let entity = entity
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("scene entity must be an object"))?;
        let components = entity
            .get_mut("components")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("scene entity components must be an object"))?;
        let old = std::mem::take(components);
        for (id, component) in old {
            let id = match id.as_str() {
                "transform" => "core.transform".to_string(),
                "sprite" => "core.sprite".to_string(),
                "model" => "core.model".to_string(),
                "script" => "core.script".to_string(),
                other => other.to_string(),
            };
            components.insert(id, component);
        }
    }
    Ok(())
}

pub fn load_snapshot(contents: &str) -> anyhow::Result<World> {
    let value: Value = serde_json::from_str(contents)?;
    snapshot_from_value(value)
}

pub fn snapshot_from_value(value: Value) -> anyhow::Result<World> {
    let value = migrate_snapshot(value)?;
    Ok(serde_json::from_value(value)?)
}

pub fn migrate_snapshot(mut value: Value) -> anyhow::Result<Value> {
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("snapshot version must be an integer"))?;
    match version {
        1 => {
            let root = value
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("snapshot must be an object"))?;
            root.insert("version".to_string(), json!(2));
            root.entry("extensions".to_string())
                .or_insert_with(|| json!({}));
            if let Some(entities) = root.get_mut("entities").and_then(Value::as_object_mut) {
                for entity in entities.values_mut() {
                    let entity = entity
                        .as_object_mut()
                        .ok_or_else(|| anyhow::anyhow!("snapshot entity must be an object"))?;
                    entity
                        .entry("extensions".to_string())
                        .or_insert_with(|| json!({}));
                }
            }
        }
        2 => {}
        _ => bail!("unsupported snapshot version {version}; expected 1 or 2"),
    }
    Ok(value)
}

fn parse_asset_aliases(
    extensions: &BTreeMap<String, Value>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let Some(value) = extensions.get("res") else {
        return Ok(BTreeMap::new());
    };
    let Some(map) = value.as_object() else {
        bail!("scene extensions.res must be an object of alias -> workspace-relative path");
    };
    let mut aliases = BTreeMap::new();
    for (alias, target) in map {
        if !is_valid_alias(alias) {
            bail!(
                "scene extensions.res alias '{alias}' is invalid; use 1-64 letters, digits, '-', '_', or '.' without '..'"
            );
        }
        let Some(target) = target.as_str() else {
            bail!("scene extensions.res alias '{alias}' must map to a string path");
        };
        let target_path = std::path::Path::new(target);
        if target.is_empty()
            || target_path.is_absolute()
            || target_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::Prefix(_)
                        | std::path::Component::RootDir
                        | std::path::Component::ParentDir
                )
            })
        {
            bail!("scene extensions.res alias '{alias}' must map to a workspace-relative path");
        }
        aliases.insert(alias.to_string(), target.to_string());
    }
    Ok(aliases)
}

fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 64
        && !alias.contains("..")
        && alias.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn load_script(component: ScriptComponent) -> anyhow::Result<ScriptState> {
    if let Some(raw_path) = &component.source {
        let path = crate::workspace_path(raw_path)?;
        let source = fs::read_to_string(&path).with_context(|| {
            format!(
                "could not read scene script {}; run npm run build first",
                path.display()
            )
        })?;
        Ok(ScriptState {
            source,
            source_path: Some(raw_path.clone()),
            enabled: true,
            last_error: None,
        })
    } else {
        Ok(ScriptState {
            source: component
                .inline
                .ok_or_else(|| anyhow::anyhow!("script requires source or inline"))?,
            source_path: None,
            enabled: true,
            last_error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_reads_scene_asset_aliases() {
        // Test binaries run with the member crate as CWD; anchor on the repo
        // root via the crate manifest location.
        let scene_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testing/assets/benchmark-3d.mnsc");
        let world = load_scene(&scene_path).expect("benchmark scene loads");
        assert_eq!(
            world.asset_aliases.get("terrain").map(String::as_str),
            Some("testing/assets/benchmark.mnec")
        );
    }

    #[test]
    fn scene_save_roundtrips_asset_aliases() {
        let mut world = World::default();
        world
            .asset_aliases
            .insert("hero".to_string(), "assets/hero.mnec".to_string());
        let path = std::env::temp_dir().join(format!("moni-aliases-{}.mnsc", std::process::id()));
        save_scene(&world, "aliases", &path).expect("save scene");
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read saved scene"))
                .expect("parse saved scene");
        assert_eq!(
            value["extensions"]["res"]["hero"],
            json!("assets/hero.mnec")
        );
        let loaded = load_scene(&path).expect("reload scene");
        assert_eq!(
            loaded.asset_aliases.get("hero").map(String::as_str),
            Some("assets/hero.mnec")
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_alias_tables_are_rejected() {
        let mut world = World::default();
        world
            .extensions
            .insert("res".to_string(), json!({ "bad path": "assets/hero.mnec" }));
        let path =
            std::env::temp_dir().join(format!("moni-aliases-bad-{}.mnsc", std::process::id()));
        save_scene(&world, "aliases", &path).expect("save scene");
        let error = load_scene(&path).expect_err("invalid alias table must fail");
        assert!(error.to_string().contains("invalid"));
        fs::remove_file(&path).ok();
    }
}
