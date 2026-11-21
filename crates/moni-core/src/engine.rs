use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::components::{ComponentDescriptor, ComponentRegistry};
use crate::scene::COMPONENT_VERSIONS_KEY;
use crate::scene3d::{Camera3d, Light, Model3d, Transform3d, Vector3};

pub const SNAPSHOT_VERSION: u32 = 2;
pub const DEFAULT_TICK_RATE: u32 = 60;
pub const MAX_STEP_COUNT: u64 = 10_000;
pub const MAX_SCRIPT_BYTES: usize = 256 * 1024;

pub type EntityId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl EngineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attaches structured context (e.g. the failing sub-command of a batch)
    /// without losing the code/message contract.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl Display for EngineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for EngineError {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub rotation: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        }
    }
}

impl Transform {
    fn validate(&self) -> Result<(), EngineError> {
        if self.x.is_finite() && self.y.is_finite() && self.rotation.is_finite() {
            Ok(())
        } else {
            Err(EngineError::new(
                "invalid_transform",
                "transform values must be finite numbers",
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpriteKind {
    Rectangle,
    Circle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sprite {
    #[serde(rename = "type")]
    pub kind: SpriteKind,
    #[serde(default = "default_sprite_size")]
    pub width: f64,
    #[serde(default = "default_sprite_size")]
    pub height: f64,
    #[serde(default = "default_sprite_color")]
    pub color: String,
    #[serde(default)]
    pub layer: i32,
}

fn default_sprite_size() -> f64 {
    32.0
}

fn default_sprite_color() -> String {
    "#ffffff".to_string()
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            kind: SpriteKind::Rectangle,
            width: default_sprite_size(),
            height: default_sprite_size(),
            color: default_sprite_color(),
            layer: 0,
        }
    }
}

impl Sprite {
    fn validate(&self) -> Result<(), EngineError> {
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(EngineError::new(
                "invalid_sprite",
                "sprite width and height must be positive finite numbers",
            ));
        }
        if self.color.is_empty() || self.color.chars().count() > 64 {
            return Err(EngineError::new(
                "invalid_sprite",
                "sprite color must contain between 1 and 64 characters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSpec {
    pub source: String,
}

impl ScriptSpec {
    fn validate(&self) -> Result<(), EngineError> {
        validate_script_source(&self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptState {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default = "script_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub last_error: Option<String>,
}

fn script_enabled() -> bool {
    true
}

impl ScriptState {
    fn new(spec: ScriptSpec) -> Result<Self, EngineError> {
        spec.validate()?;
        Ok(Self {
            source: spec.source,
            source_path: None,
            enabled: true,
            last_error: None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitySpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub sprite: Option<Sprite>,
    #[serde(default)]
    pub model: Option<Model3d>,
    #[serde(default)]
    pub script: Option<ScriptSpec>,
}

impl EntitySpec {
    fn validate(&self) -> Result<(), EngineError> {
        if let Some(name) = &self.name {
            validate_entity_name(name)?;
        }
        self.transform.validate()?;
        if let Some(sprite) = &self.sprite {
            sprite.validate()?;
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        if let Some(script) = &self.script {
            script.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub id: EntityId,
    pub name: String,
    pub transform: Transform,
    #[serde(default)]
    pub sprite: Option<Sprite>,
    #[serde(default)]
    pub model: Option<Model3d>,
    #[serde(default)]
    pub script: Option<ScriptState>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl Entity {
    /// Validates the entity's own parts with the same rules spawn and world
    /// loading use: name length, transform, sprite, model, and script source.
    /// Extension data is dynamic-component JSON that only the session
    /// registry can interpret, so it is not validated here.
    fn validate(&self) -> Result<(), EngineError> {
        validate_entity_name(&self.name)?;
        self.transform.validate()?;
        if let Some(sprite) = &self.sprite {
            sprite.validate()?;
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        if let Some(script) = &self.script {
            validate_script_source(&script.source)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    #[serde(default = "default_viewport_width")]
    pub width: u32,
    #[serde(default = "default_viewport_height")]
    pub height: u32,
    #[serde(default = "default_background")]
    pub background: String,
}

fn default_viewport_width() -> u32 {
    640
}

fn default_viewport_height() -> u32 {
    360
}

fn default_background() -> String {
    "#10141f".to_string()
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: default_viewport_width(),
            height: default_viewport_height(),
            background: default_background(),
        }
    }
}

impl Viewport {
    fn validate(&self) -> Result<(), EngineError> {
        if self.width == 0 || self.height == 0 {
            return Err(EngineError::new(
                "invalid_viewport",
                "viewport width and height must be positive",
            ));
        }
        if self.background.is_empty() || self.background.chars().count() > 64 {
            return Err(EngineError::new(
                "invalid_viewport",
                "viewport background must contain between 1 and 64 characters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    pub version: u32,
    #[serde(default)]
    pub tick: u64,
    #[serde(default = "default_tick_rate")]
    pub tick_rate: u32,
    #[serde(default = "first_entity_id")]
    pub next_entity_id: EntityId,
    #[serde(default)]
    pub viewport: Viewport,
    #[serde(default)]
    pub camera: Camera3d,
    #[serde(default = "default_lights")]
    pub lights: Vec<Light>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
    /// Virtual asset aliases (`res://<alias>` → workspace-relative path),
    /// parsed from scene `extensions.res` at load time so per-frame asset
    /// resolution never touches JSON.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub asset_aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub entities: BTreeMap<EntityId, Entity>,
}

fn default_tick_rate() -> u32 {
    DEFAULT_TICK_RATE
}

fn first_entity_id() -> EntityId {
    1
}

fn default_lights() -> Vec<Light> {
    vec![Light::Directional {
        direction: Vector3 {
            x: -0.45,
            y: -0.8,
            z: -0.35,
        },
        color: [1.0, 1.0, 1.0],
        intensity: 1.0,
        shadows: false,
    }]
}

impl Default for World {
    fn default() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            tick: 0,
            tick_rate: DEFAULT_TICK_RATE,
            next_entity_id: first_entity_id(),
            viewport: Viewport::default(),
            camera: Camera3d::default(),
            lights: default_lights(),
            extensions: BTreeMap::new(),
            asset_aliases: BTreeMap::new(),
            entities: BTreeMap::new(),
        }
    }
}

impl World {
    fn validate(&self) -> Result<(), EngineError> {
        if self.version != SNAPSHOT_VERSION {
            return Err(EngineError::new(
                "unsupported_snapshot_version",
                format!(
                    "snapshot version {} is not supported by version {}",
                    self.version, SNAPSHOT_VERSION
                ),
            ));
        }
        if self.tick_rate == 0 {
            return Err(EngineError::new(
                "invalid_world",
                "tick_rate must be positive",
            ));
        }
        self.viewport.validate()?;
        self.camera.validate()?;
        for light in &self.lights {
            light.validate()?;
        }

        let mut highest_id = 0;
        for (key, entity) in &self.entities {
            if *key != entity.id {
                return Err(EngineError::new(
                    "invalid_world",
                    format!(
                        "entity map key {} does not match entity id {}",
                        key, entity.id
                    ),
                ));
            }
            entity.validate()?;
            highest_id = highest_id.max(entity.id);
        }
        if self.next_entity_id <= highest_id {
            return Err(EngineError::new(
                "invalid_world",
                "next_entity_id must be greater than every existing entity id",
            ));
        }
        Ok(())
    }
}

fn validate_script_source(source: &str) -> Result<(), EngineError> {
    if source.trim().is_empty() {
        return Err(EngineError::new(
            "invalid_script",
            "script source must not be empty",
        ));
    }
    if source.len() > MAX_SCRIPT_BYTES {
        return Err(EngineError::new(
            "script_too_large",
            format!("script source exceeds {} bytes", MAX_SCRIPT_BYTES),
        ));
    }
    Ok(())
}

/// Entity names must contain between 1 and 128 characters; spawn, rename,
/// and restore share this rule.
fn validate_entity_name(name: &str) -> Result<(), EngineError> {
    if name.is_empty() || name.chars().count() > 128 {
        return Err(EngineError::new(
            "invalid_entity",
            "entity name must contain between 1 and 128 characters",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    EntitySpawned {
        entity: EntityId,
    },
    EntityDespawned {
        entity: EntityId,
    },
    EntityRenamed {
        entity: EntityId,
        name: String,
    },
    TransformChanged {
        entity: EntityId,
    },
    SpriteChanged {
        entity: EntityId,
    },
    ModelChanged {
        entity: EntityId,
    },
    ComponentAttached {
        entity: EntityId,
        component: String,
    },
    ComponentRemoved {
        entity: EntityId,
        component: String,
    },
    ComponentRegistered {
        component: String,
    },
    ComponentUnregistered {
        component: String,
    },
    CameraChanged,
    ScriptAttached {
        entity: EntityId,
    },
    ScriptEnabledChanged {
        entity: EntityId,
        enabled: bool,
    },
    ScriptLog {
        entity: EntityId,
        message: String,
    },
    ScriptError {
        entity: EntityId,
        code: String,
        message: String,
    },
    InvalidScriptCommand {
        entity: EntityId,
        message: String,
    },
    ScriptCommandRejected {
        entity: EntityId,
        code: String,
        message: String,
    },
    SimulationStepped {
        ticks: u64,
        current_tick: u64,
    },
    WorldLoaded,
    WorldReset,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Spawn {
        spec: EntitySpec,
    },
    Despawn {
        entity: EntityId,
    },
    SetTransform {
        entity: EntityId,
        transform: Transform,
    },
    SetSprite {
        entity: EntityId,
        sprite: Option<Sprite>,
    },
    SetModel {
        entity: EntityId,
        model: Option<Model3d>,
    },
    SetModelTransform {
        entity: EntityId,
        transform: Transform3d,
    },
    SetCamera {
        camera: Camera3d,
    },
    AttachScript {
        entity: EntityId,
        source: String,
    },
    SetScriptEnabled {
        entity: EntityId,
        enabled: bool,
    },
    AttachComponent {
        entity: EntityId,
        component: String,
        value: Value,
    },
    RemoveComponent {
        entity: EntityId,
        component: String,
    },
    /// Renames an entity; the name follows the spawn validation rules.
    RenameEntity {
        entity: EntityId,
        name: String,
    },
    /// Reinserts a captured entity snapshot under its explicit id (undo/redo
    /// stable-ID semantics). The id must not exist; `next_entity_id` moves
    /// past the restored id so later spawns never collide with it.
    RestoreEntity {
        entity: Entity,
    },
}

/// Result of one script-runtime pass: already-structured core events plus
/// parsed commands for the engine to apply.
#[derive(Debug, Default)]
pub struct ScriptTickOutput {
    pub events: Vec<Event>,
    pub commands: Vec<(EntityId, Command)>,
}

/// Script execution lives behind this trait so the core engine never names a
/// scripting implementation. The runtime reconciles its per-entity state
/// against the world every tick: it spawns hosts for new or changed scripts,
/// drops hosts for despawned entities, and disables scripts that fail. The
/// composition root installs exactly one implementation.
pub trait ScriptRuntime: std::fmt::Debug {
    fn tick(&mut self, world: &mut World, tick: u64, dt: f64) -> ScriptTickOutput;
    fn host_count(&self) -> usize;
    fn clear(&mut self);
}

/// One component schema migration step: converts a component value stored at
/// some schema version into its equivalent at the next version. A `String`
/// error describes why the value cannot be migrated.
pub type ComponentMigrator =
    Box<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send>;

/// Migrator closures are shared behind an `Arc` so snapshots can copy the
/// bookkeeping cheaply; the public API hands over the boxed closure itself.
type SharedMigrator = Arc<dyn Fn(&Value) -> Result<Value, String> + Send>;

/// Per-component migration chains: component id -> from-version ->
/// (to-version, step). Only dynamic components carry chains; core components
/// are migrated by the scene format itself.
#[derive(Clone, Default)]
struct MigratorTable(BTreeMap<&'static str, BTreeMap<u32, (u32, SharedMigrator)>>);

impl std::fmt::Debug for MigratorTable {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        // Closures are opaque; report only the version edges per component.
        let chains: BTreeMap<&str, Vec<String>> = self
            .0
            .iter()
            .map(|(component, steps)| {
                let edges = steps
                    .iter()
                    .map(|(from, (to, _))| format!("{from}->{to}"))
                    .collect();
                (*component, edges)
            })
            .collect();
        formatter
            .debug_tuple("MigratorTable")
            .field(&chains)
            .finish()
    }
}

#[derive(Debug)]
pub struct Engine {
    world: World,
    /// Session-level component registry, seeded with the core components.
    /// Registration is transactional session state: snapshots capture it (and
    /// the migrator chains) and restores revert it, so a rolled-back batch or
    /// failed plugin start leaves no registry trace. Dynamic component DATA
    /// round-trips inside `Entity.extensions`.
    registry: ComponentRegistry,
    /// Migration chains per dynamic component; migrators follow their
    /// component and are removed by `unregister_component`.
    migrators: MigratorTable,
    events: Vec<Event>,
    script_runtime: Option<Box<dyn ScriptRuntime>>,
    /// Monotonic counter bumped by every world mutation (commands, ticks,
    /// world load/reset) and by component registration changes. Clients use
    /// it for optimistic concurrency checks.
    revision: u64,
    /// Session-wide watermark of delivered events: all events with a sequence
    /// number at or below this value have already been taken by a consumer.
    event_seq: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            world: World::default(),
            registry: ComponentRegistry::new(),
            migrators: MigratorTable::default(),
            events: Vec::new(),
            script_runtime: None,
            revision: 0,
            event_seq: 0,
        }
    }
}

/// Restorable engine state used by atomic batches and plugin start rollback.
/// Includes the component registry and migrator chains so a rollback leaves
/// no registration trace. Script runtime state is intentionally not captured:
/// hosts reconcile against the restored world on their next tick.
#[derive(Debug)]
pub struct EngineSnapshot {
    world: World,
    registry: ComponentRegistry,
    migrators: MigratorTable,
    events_len: usize,
    revision: u64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The session component registry, seeded with the four core components.
    pub fn registry(&self) -> &ComponentRegistry {
        &self.registry
    }

    /// Direct registry access for composition roots. Prefer
    /// [`Engine::register_component`] / [`Engine::unregister_component`],
    /// which also bump the revision and enforce world-level revocation rules.
    pub fn registry_mut(&mut self) -> &mut ComponentRegistry {
        &mut self.registry
    }

    /// Registers a component descriptor for the rest of the session. Dynamic
    /// (schema-only) descriptors become attachable immediately. Registration
    /// is transactional session state: snapshots capture it, so restoring an
    /// older snapshot reverts registrations made after it.
    pub fn register_component(
        &mut self,
        descriptor: ComponentDescriptor,
    ) -> Result<Value, EngineError> {
        let id = descriptor.id.to_string();
        self.registry
            .register(descriptor)
            .map_err(|error| EngineError::new("duplicate_component", error))?;
        self.revision = self.revision.wrapping_add(1);
        self.events.push(Event::ComponentRegistered {
            component: id.clone(),
        });
        Ok(json!({ "component": id, "registered": true }))
    }

    /// Removes a component descriptor. Registration is refused while any
    /// entity in the world still carries the component unless `force` is set,
    /// which strips it from every entity (emitting `ComponentRemoved` for
    /// each) before unregistering. Core components are always refused.
    pub fn unregister_component(&mut self, id: &str, force: bool) -> Result<Value, EngineError> {
        let descriptor = self
            .registry
            .resolve(id)
            .ok_or_else(|| unknown_component(id))?;
        if !descriptor.is_dynamic() {
            return Err(EngineError::new(
                "component_reserved",
                format!("core component '{}' cannot be unregistered", descriptor.id),
            ));
        }
        let component = descriptor.id.to_string();
        let users: Vec<EntityId> = self
            .world
            .entities
            .values()
            .filter(|entity| entity.extensions.contains_key(&component))
            .map(|entity| entity.id)
            .collect();
        if !users.is_empty() && !force {
            return Err(EngineError::new(
                "component_in_use",
                format!(
                    "component '{component}' is still attached to {} entities; unregister with force to strip it first",
                    users.len()
                ),
            )
            .with_details(json!({
                "component": component,
                "entities": users,
                "usageCount": users.len(),
            })));
        }
        for entity in &users {
            if let Some(target) = self.world.entities.get_mut(entity) {
                target.extensions.remove(&component);
            }
            self.events.push(Event::ComponentRemoved {
                entity: *entity,
                component: component.clone(),
            });
        }
        // Stripping (or the absence of) instances also drops any retained
        // future-version entry: the data no longer exists to describe.
        prune_component_version_entry(&mut self.world, &component);
        self.registry
            .unregister(&component)
            .map_err(|error| EngineError::new("unknown_component", error))?;
        // Migrators follow their component: revocation drops the chain too.
        self.migrators.0.remove(component.as_str());
        self.revision = self.revision.wrapping_add(1);
        self.events.push(Event::ComponentUnregistered {
            component: component.clone(),
        });
        Ok(json!({
            "component": component,
            "unregistered": true,
            "stripped": users.len(),
        }))
    }

    /// Registers one migration step for a dynamic component: `migrate`
    /// converts a component value stored at `from_version` into its
    /// `to_version` shape. The component must be registered and dynamic, the
    /// step must increase the version without exceeding the registered
    /// component version, and a second step with the same `from_version` is
    /// rejected. Chains are walked by [`Engine::migrate_component_value`].
    pub fn register_component_migrator(
        &mut self,
        component: &'static str,
        from_version: u32,
        to_version: u32,
        migrate: ComponentMigrator,
    ) -> Result<(), EngineError> {
        let descriptor = self
            .registry
            .resolve(component)
            .ok_or_else(|| unknown_component(component))?;
        if !descriptor.is_dynamic() {
            return Err(EngineError::new(
                "component_reserved",
                format!(
                    "core component '{}' is stored in typed entity fields and needs no migrators",
                    descriptor.id
                ),
            ));
        }
        if to_version <= from_version {
            return Err(EngineError::new(
                "invalid_migration_step",
                format!(
                    "component '{}': migration must increase the version ({from_version} -> {to_version})",
                    descriptor.id
                ),
            ));
        }
        if to_version > descriptor.version {
            return Err(EngineError::new(
                "invalid_migration_step",
                format!(
                    "component '{}': migration target {to_version} exceeds the registered component version {}",
                    descriptor.id, descriptor.version
                ),
            ));
        }
        let steps = self.migrators.0.entry(descriptor.id).or_default();
        if let Some((existing_to, _)) = steps.get(&from_version) {
            return Err(EngineError::new(
                "duplicate_migrator",
                format!(
                    "component '{}' already has a migration step {from_version} -> {existing_to}",
                    descriptor.id
                ),
            )
            .with_details(json!({
                "component": descriptor.id,
                "from": from_version,
                "existingTo": existing_to,
            })));
        }
        steps.insert(from_version, (to_version, Arc::from(migrate)));
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Removes every migration step registered for `component`. The component
    /// registration itself is unaffected; [`Engine::unregister_component`]
    /// drops the chain automatically. The revision advances only when a chain
    /// was actually removed.
    pub fn unregister_component_migrators(&mut self, component: &'static str) {
        let key: &str = match self.registry.resolve(component) {
            Some(descriptor) => descriptor.id,
            None => component,
        };
        if self.migrators.0.remove(key).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Converts `value` from schema version `from` to `to` by walking the
    /// component's registered migration chain. `from == to` returns the value
    /// unchanged. Fails with `missing_migrator` (details name the version the
    /// chain stopped at), `migration_failed` when a step rejects the value, or
    /// `invalid_migration_step` for downgrade attempts.
    pub fn migrate_component_value(
        &self,
        component: &str,
        value: Value,
        from: u32,
        to: u32,
    ) -> Result<Value, EngineError> {
        if from > to {
            return Err(EngineError::new(
                "invalid_migration_step",
                format!(
                    "component '{component}': cannot migrate from {from} to {to}; downgrades are not supported"
                ),
            ));
        }
        if from == to {
            return Ok(value);
        }
        let key: &str = match self.registry.resolve(component) {
            Some(descriptor) => descriptor.id,
            None => component,
        };
        let Some(steps) = self.migrators.0.get(key) else {
            return Err(missing_migrator(component, from, to, from));
        };
        let mut current = from;
        let mut value = value;
        let mut remaining = steps.len();
        while current != to {
            let Some((next, migrate)) = steps.get(&current) else {
                return Err(missing_migrator(component, from, to, current));
            };
            if remaining == 0 {
                // The chain never reaches `to` (cycle or overshoot); refuse
                // instead of looping forever.
                return Err(EngineError::new(
                    "missing_migrator",
                    format!(
                        "component '{component}': migration chain from {from} to {to} does not reach version {to} (stopped at {current})"
                    ),
                )
                .with_details(json!({
                    "component": component,
                    "from": from,
                    "to": to,
                    "at": current,
                })));
            }
            remaining -= 1;
            let step_fn: &(dyn Fn(&Value) -> Result<Value, String> + Send) = migrate.as_ref();
            value = step_fn(&value).map_err(|cause| {
                EngineError::new(
                    "migration_failed",
                    format!(
                        "component '{component}': migration {current} -> {next} failed: {cause}"
                    ),
                )
                .with_details(json!({
                    "component": component,
                    "from": current,
                    "to": next,
                    "cause": cause,
                }))
            })?;
            current = *next;
        }
        Ok(value)
    }

    /// Attaches a dynamic component instance to an entity. The value is
    /// schema-normalized (defaults applied, then validated) and stored as
    /// JSON under the component ID in `Entity.extensions`. Attaching again
    /// replaces the stored value. Core components live in typed entity
    /// fields and cannot be attached through this path.
    ///
    /// While any entity still carries a future-version instance of the
    /// component (retained scene version newer than the registry version),
    /// current-version writes are refused with `component_future_version`
    /// and the world is left untouched: a single global version table cannot
    /// describe mixed-version data. Removing the future instances is the way
    /// out. A retained entry without instances describes no live data and is
    /// repaired instead of refusing.
    pub fn attach_component(
        &mut self,
        entity: EntityId,
        component: &str,
        value: Value,
    ) -> Result<Value, EngineError> {
        let result = self.attach_component_inner(entity, component, value);
        if result.is_ok() {
            self.revision = self.revision.wrapping_add(1);
        }
        result
    }

    /// Removes a dynamic component instance from an entity. Core components
    /// and unregistered component IDs are refused.
    pub fn remove_component(
        &mut self,
        entity: EntityId,
        component: &str,
    ) -> Result<Value, EngineError> {
        let result = self.remove_component_inner(entity, component);
        if result.is_ok() {
            self.revision = self.revision.wrapping_add(1);
        }
        result
    }

    fn attach_component_inner(
        &mut self,
        entity: EntityId,
        component: &str,
        mut value: Value,
    ) -> Result<Value, EngineError> {
        let descriptor = self
            .registry
            .resolve(component)
            .ok_or_else(|| unknown_component(component))?;
        if !descriptor.is_dynamic() {
            return Err(EngineError::new(
                "component_reserved",
                format!(
                    "component '{}' is stored in a typed entity field; attach_component only accepts dynamic (schema-only) components",
                    descriptor.id
                ),
            ));
        }
        // The table is keyed by the canonical component ID, matching saves.
        let component_id = descriptor.id;
        if let Some(scene_version) = retained_version(&self.world, component_id) {
            let instances = self
                .world
                .entities
                .values()
                .filter(|entity| entity.extensions.contains_key(component_id))
                .count();
            if instances == 0 {
                // Removal paths drop their entries, so an instance-less
                // entry cannot describe live data; repair it so the attach
                // below is saved at the registry version.
                prune_component_version_entry(&mut self.world, component_id);
            } else if scene_version > u64::from(descriptor.version) {
                return Err(future_version_error(
                    component_id,
                    scene_version,
                    descriptor.version,
                    instances,
                ));
            }
        }
        descriptor
            .schema
            .apply_defaults(&mut value)
            .map_err(|error| invalid_component_value(descriptor.id, &error))?;
        descriptor
            .schema
            .validate(&value)
            .map_err(|error| invalid_component_value(descriptor.id, &error))?;
        let component = descriptor.id.to_string();
        let target = self.entity_mut(entity)?;
        target.extensions.insert(component.clone(), value);
        self.events.push(Event::ComponentAttached {
            entity,
            component: component.clone(),
        });
        Ok(json!({ "entity": entity, "component": component }))
    }

    fn remove_component_inner(
        &mut self,
        entity: EntityId,
        component: &str,
    ) -> Result<Value, EngineError> {
        let descriptor = self
            .registry
            .resolve(component)
            .ok_or_else(|| unknown_component(component))?;
        if !descriptor.is_dynamic() {
            return Err(EngineError::new(
                "component_reserved",
                format!(
                    "component '{}' is stored in a typed entity field; remove_component only accepts dynamic (schema-only) components",
                    descriptor.id
                ),
            ));
        }
        let component = descriptor.id.to_string();
        let target = self.entity_mut(entity)?;
        if target.extensions.remove(&component).is_none() {
            return Err(EngineError::new(
                "component_not_attached",
                format!("entity {entity} does not carry component '{component}'"),
            ));
        }
        // Removing a future-version instance is the explicit way out; once
        // no entity carries the component anymore, drop its retained entry
        // so saves return to the registry version and writes unlock.
        prune_component_version_entry(&mut self.world, &component);
        self.events.push(Event::ComponentRemoved {
            entity,
            component: component.clone(),
        });
        Ok(json!({ "entity": entity, "component": component }))
    }

    /// Current world revision; changes on every successful mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Sequence number of the last event handed out through `take_events`.
    pub fn event_seq(&self) -> u64 {
        self.event_seq
    }

    /// Captures world contents, pending event count, revision, component
    /// registrations, and migrator chains so a caller can roll back to
    /// exactly this state. The registry is small, so the extra clones stay
    /// cheap enough for per-batch snapshots; dynamic component data lives in
    /// `Entity.extensions`, which the world snapshot carries.
    pub fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            world: self.world.clone(),
            registry: self.registry.clone(),
            migrators: self.migrators.clone(),
            events_len: self.events.len(),
            revision: self.revision,
        }
    }

    /// Restores a snapshot: world contents, revision, component
    /// registrations, and migrator chains revert, and events emitted since
    /// the snapshot are discarded so a failed transaction leaves no
    /// observable registry trace.
    pub fn restore(&mut self, snapshot: EngineSnapshot) {
        self.world = snapshot.world;
        self.registry = snapshot.registry;
        self.migrators = snapshot.migrators;
        self.events.truncate(snapshot.events_len);
        self.revision = snapshot.revision;
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        let events = std::mem::take(&mut self.events);
        self.event_seq = self.event_seq.saturating_add(events.len() as u64);
        events
    }

    /// Installs the scripting implementation. Without a runtime, script
    /// components remain inert data and never execute.
    pub fn set_script_runtime(&mut self, runtime: Box<dyn ScriptRuntime>) {
        self.script_runtime = Some(runtime);
    }

    pub fn script_host_count(&self) -> usize {
        self.script_runtime
            .as_ref()
            .map_or(0, |runtime| runtime.host_count())
    }

    pub fn shutdown_runtime(&mut self) {
        if let Some(runtime) = &mut self.script_runtime {
            runtime.clear();
        }
    }

    pub fn reset(&mut self) {
        self.world = World::default();
        if let Some(runtime) = &mut self.script_runtime {
            runtime.clear();
        }
        self.revision = self.revision.wrapping_add(1);
        self.events.push(Event::WorldReset);
    }

    pub fn load_world(&mut self, world: World) -> Result<(), EngineError> {
        world.validate()?;
        self.world = world;
        if let Some(runtime) = &mut self.script_runtime {
            runtime.clear();
        }
        self.revision = self.revision.wrapping_add(1);
        self.events.push(Event::WorldLoaded);
        Ok(())
    }

    pub fn apply(&mut self, command: Command) -> Result<Value, EngineError> {
        let result = match command {
            Command::Spawn { spec } => {
                spec.validate()?;
                let id = self.world.next_entity_id;
                self.world.next_entity_id += 1;
                let script = spec.script.map(ScriptState::new).transpose()?;
                let name = spec.name.unwrap_or_else(|| format!("entity-{id}"));
                self.world.entities.insert(
                    id,
                    Entity {
                        id,
                        name,
                        transform: spec.transform,
                        sprite: spec.sprite,
                        model: spec.model,
                        script,
                        extensions: BTreeMap::new(),
                    },
                );
                self.events.push(Event::EntitySpawned { entity: id });
                Ok(json!({ "entity": id }))
            }
            Command::Despawn { entity } => {
                let removed = self
                    .world
                    .entities
                    .remove(&entity)
                    .ok_or_else(|| entity_not_found(entity))?;
                // Component data left the world with the entity: drop
                // retained version entries that no longer describe any
                // instance.
                for component in removed.extensions.keys() {
                    prune_component_version_entry(&mut self.world, component);
                }
                self.events.push(Event::EntityDespawned { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::RenameEntity { entity, name } => {
                validate_entity_name(&name)?;
                let target = self.entity_mut(entity)?;
                target.name = name.clone();
                self.events.push(Event::EntityRenamed { entity, name });
                Ok(json!({ "entity": entity }))
            }
            Command::RestoreEntity { entity } => {
                if entity.id == 0 {
                    return Err(EngineError::new(
                        "invalid_entity",
                        "entity id must be positive",
                    ));
                }
                if self.world.entities.contains_key(&entity.id) {
                    return Err(EngineError::new(
                        "entity_exists",
                        format!("entity {} already exists", entity.id),
                    ));
                }
                entity.validate()?;
                let id = entity.id;
                // Keyed by entity.id so the world keeps its key == id
                // invariant (see World::validate).
                self.world.entities.insert(id, entity);
                if self.world.next_entity_id <= id {
                    self.world.next_entity_id = id.saturating_add(1);
                }
                self.events.push(Event::EntitySpawned { entity: id });
                Ok(json!({ "entity": id }))
            }
            Command::SetTransform { entity, transform } => {
                transform.validate()?;
                let target = self.entity_mut(entity)?;
                target.transform = transform;
                self.events.push(Event::TransformChanged { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::SetSprite { entity, sprite } => {
                if let Some(sprite) = &sprite {
                    sprite.validate()?;
                }
                let target = self.entity_mut(entity)?;
                target.sprite = sprite;
                self.events.push(Event::SpriteChanged { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::SetModel { entity, model } => {
                if let Some(model) = &model {
                    model.validate()?;
                }
                let target = self.entity_mut(entity)?;
                target.model = model;
                self.events.push(Event::ModelChanged { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::SetModelTransform { entity, transform } => {
                transform.validate()?;
                let target = self.entity_mut(entity)?;
                let model = target.model.as_mut().ok_or_else(|| {
                    EngineError::new(
                        "model_not_found",
                        format!("entity {entity} does not have a 3D model"),
                    )
                })?;
                model.transform = transform;
                self.events.push(Event::ModelChanged { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::SetCamera { camera } => {
                camera.validate()?;
                self.world.camera = camera;
                self.events.push(Event::CameraChanged);
                Ok(json!({ "camera": "updated" }))
            }
            Command::AttachScript { entity, source } => {
                validate_script_source(&source)?;
                let target = self.entity_mut(entity)?;
                target.script = Some(ScriptState {
                    source,
                    source_path: None,
                    enabled: true,
                    last_error: None,
                });
                // The script runtime notices the changed source on its next
                // tick and rebuilds the host for this entity.
                self.events.push(Event::ScriptAttached { entity });
                Ok(json!({ "entity": entity }))
            }
            Command::SetScriptEnabled { entity, enabled } => {
                let target = self.entity_mut(entity)?;
                let script = target.script.as_mut().ok_or_else(|| {
                    EngineError::new(
                        "script_not_found",
                        format!("entity {} does not have an attached script", entity),
                    )
                })?;
                script.enabled = enabled;
                if enabled {
                    script.last_error = None;
                }
                self.events
                    .push(Event::ScriptEnabledChanged { entity, enabled });
                Ok(json!({ "entity": entity, "enabled": enabled }))
            }
            // apply() owns the revision bump for command-driven mutations,
            // so the command arms call the inner helpers directly.
            Command::AttachComponent {
                entity,
                component,
                value,
            } => self.attach_component_inner(entity, &component, value),
            Command::RemoveComponent { entity, component } => {
                self.remove_component_inner(entity, &component)
            }
        };
        if result.is_ok() {
            self.revision = self.revision.wrapping_add(1);
        }
        result
    }

    pub fn step(&mut self, count: u64) -> Result<Value, EngineError> {
        if count == 0 || count > MAX_STEP_COUNT {
            return Err(EngineError::new(
                "invalid_step_count",
                format!("step count must be between 1 and {}", MAX_STEP_COUNT),
            ));
        }
        for _ in 0..count {
            self.step_once();
        }
        self.events.push(Event::SimulationStepped {
            ticks: count,
            current_tick: self.world.tick,
        });
        Ok(json!({
            "ticks": count,
            "tick": self.world.tick,
            "entities": self.world.entities.len()
        }))
    }

    fn step_once(&mut self) {
        let dt = 1.0 / f64::from(self.world.tick_rate);
        let tick = self.world.tick;
        let mut pending_commands = Vec::new();
        if let Some(runtime) = &mut self.script_runtime {
            let output = runtime.tick(&mut self.world, tick, dt);
            self.events.extend(output.events);
            pending_commands = output.commands;
        }

        for (origin, command) in pending_commands {
            if let Err(error) = self.apply(command) {
                self.events.push(Event::ScriptCommandRejected {
                    entity: origin,
                    code: error.code,
                    message: error.message,
                });
            }
        }
        self.world.tick += 1;
        self.revision = self.revision.wrapping_add(1);
    }

    fn entity_mut(&mut self, entity: EntityId) -> Result<&mut Entity, EngineError> {
        self.world
            .entities
            .get_mut(&entity)
            .ok_or_else(|| entity_not_found(entity))
    }
}

fn entity_not_found(entity: EntityId) -> EngineError {
    EngineError::new(
        "entity_not_found",
        format!("entity {} does not exist", entity),
    )
}

/// The world-level retained version-table entry for `component`, when one
/// survives the session load (unregistered-at-load or future-version data).
fn retained_version(world: &World, component: &str) -> Option<u64> {
    world
        .extensions
        .get(COMPONENT_VERSIONS_KEY)
        .and_then(Value::as_object)
        .and_then(|table| table.get(component))
        .and_then(Value::as_u64)
}

/// Drops the retained [`COMPONENT_VERSIONS_KEY`] entry for `component` when
/// no entity carries it anymore, and removes the table itself once empty.
/// Every mutation that removes component data (remove_component, despawn,
/// forced unregister) calls this, so the live table keeps mirroring actual
/// content, saves return to the registry version, and the future-version
/// attach guard never sees stale entries.
fn prune_component_version_entry(world: &mut World, component: &str) {
    let still_present = world
        .entities
        .values()
        .any(|entity| entity.extensions.contains_key(component));
    if still_present {
        return;
    }
    let Some(table) = world
        .extensions
        .get_mut(COMPONENT_VERSIONS_KEY)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    table.remove(component);
    if table.is_empty() {
        world.extensions.remove(COMPONENT_VERSIONS_KEY);
    }
}

fn future_version_error(
    component: &str,
    scene_version: u64,
    registry_version: u32,
    instances: usize,
) -> EngineError {
    EngineError::new(
        "component_future_version",
        format!(
            "component '{component}' has {instances} future-version instance(s) written at scene version {scene_version}, but the registry provides version {registry_version}; a single version table cannot describe mixed-version data, so remove the future-version instances before attaching current-version values"
        ),
    )
    .with_details(json!({
        "component": component,
        "sceneVersion": scene_version,
        "registryVersion": registry_version,
        "futureInstances": instances,
    }))
}

fn unknown_component(component: &str) -> EngineError {
    EngineError::new(
        "unknown_component",
        format!("component '{component}' is not registered"),
    )
}

fn invalid_component_value(component: &str, error: &str) -> EngineError {
    EngineError::new(
        "invalid_component_value",
        format!("component '{component}': {error}"),
    )
}

fn missing_migrator(component: &str, from: u32, to: u32, at: u32) -> EngineError {
    EngineError::new(
        "missing_migrator",
        format!(
            "component '{component}' has no migration step at version {at}; cannot migrate from {from} to {to}"
        ),
    )
    .with_details(json!({
        "component": component,
        "from": from,
        "to": to,
        "at": at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{PropertyField, PropertySchema, PropertyType};
    use serde_json::json;

    fn spec(name: &str) -> EntitySpec {
        serde_json::from_value(json!({ "name": name })).expect("valid entity spec")
    }

    #[test]
    fn revision_tracks_mutations_and_failed_commands() {
        let mut engine = Engine::new();
        assert_eq!(engine.revision(), 0);
        let entity = engine
            .apply(Command::Spawn { spec: spec("a") })
            .expect("spawn");
        let after_spawn = engine.revision();
        assert!(after_spawn > 0);
        let missing = entity["entity"].as_u64().expect("entity id");
        assert!(
            engine
                .apply(Command::SetTransform {
                    entity: missing + 100,
                    transform: Transform::default(),
                })
                .is_err()
        );
        assert_eq!(
            engine.revision(),
            after_spawn,
            "failed command must not bump revision"
        );
        assert!(
            engine
                .apply(Command::SetTransform {
                    entity: missing,
                    transform: Transform::default(),
                })
                .is_ok()
        );
        assert!(engine.revision() > after_spawn);
    }

    #[test]
    fn events_carry_a_session_wide_sequence_watermark() {
        let mut engine = Engine::new();
        assert_eq!(engine.event_seq(), 0);
        engine
            .apply(Command::Spawn { spec: spec("a") })
            .expect("spawn");
        engine
            .apply(Command::Spawn { spec: spec("b") })
            .expect("spawn");
        let first = engine.take_events();
        assert_eq!(first.len(), 2);
        assert_eq!(engine.event_seq(), 2, "watermark counts delivered events");
        let second = engine.take_events();
        assert!(second.is_empty());
        assert_eq!(engine.event_seq(), 2);
    }

    #[test]
    fn snapshot_restore_rolls_back_world_events_and_revision() {
        let mut engine = Engine::new();
        engine
            .apply(Command::Spawn { spec: spec("a") })
            .expect("spawn");
        let snapshot = engine.snapshot();
        let revision = engine.revision();
        engine
            .apply(Command::Spawn { spec: spec("b") })
            .expect("spawn");
        engine
            .apply(Command::Spawn { spec: spec("c") })
            .expect("spawn");
        assert_eq!(engine.world().entities.len(), 3);
        engine.restore(snapshot);
        assert_eq!(engine.world().entities.len(), 1);
        assert_eq!(engine.revision(), revision, "restore reverts the revision");
        let events = engine.take_events();
        assert_eq!(
            events.len(),
            1,
            "events emitted after the snapshot are discarded"
        );
    }

    #[test]
    fn ticks_and_world_loads_bump_revision() {
        let mut engine = Engine::new();
        let before = engine.revision();
        engine.step(1).expect("step");
        assert!(engine.revision() > before);
        let mut world = World::default();
        world.entities.insert(
            1,
            Entity {
                id: 1,
                name: "a".into(),
                transform: Transform::default(),
                sprite: None,
                model: None,
                script: None,
                extensions: BTreeMap::new(),
            },
        );
        world.next_entity_id = 2;
        let after_step = engine.revision();
        engine.load_world(world).expect("load world");
        assert!(engine.revision() > after_step);
    }

    fn health_descriptor() -> ComponentDescriptor {
        health_descriptor_at(1)
    }

    fn health_descriptor_at(version: u32) -> ComponentDescriptor {
        ComponentDescriptor::dynamic(
            "game.health",
            None,
            "game.test",
            version,
            PropertySchema::new(
                "Health",
                "Dynamic test component",
                vec![
                    PropertyField::new("hp", PropertyType::Number, false)
                        .default(json!(100))
                        .range(0.0, 1000.0),
                    PropertyField::new("max_hp", PropertyType::Number, false)
                        .default(json!(100))
                        .range(0.0, 1000.0),
                ],
            ),
        )
    }

    fn spawn_one(engine: &mut Engine, name: &str) -> EntityId {
        let result = engine
            .apply(Command::Spawn { spec: spec(name) })
            .expect("spawn");
        result["entity"].as_u64().expect("entity id")
    }

    fn stored_value(engine: &Engine, entity: EntityId, component: &str) -> Value {
        engine
            .world()
            .entities
            .get(&entity)
            .expect("entity")
            .extensions
            .get(component)
            .expect("component data stored")
            .clone()
    }

    #[test]
    fn dynamic_component_attach_applies_defaults_and_stores_extensions() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        let revision = engine.revision();
        engine
            .attach_component(entity, "game.health", json!({ "hp": 80 }))
            .expect("attach");
        assert_eq!(engine.revision(), revision + 1, "attach bumps the revision");
        let stored = stored_value(&engine, entity, "game.health");
        assert_eq!(stored["hp"], json!(80));
        assert_eq!(stored["max_hp"], json!(100), "defaults are applied");
        let events = engine.take_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::ComponentAttached { component, .. } if component == "game.health"
            )),
            "attach emits component_attached: {events:?}"
        );
    }

    #[test]
    fn dynamic_component_attach_rejects_invalid_values() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        let error = engine
            .attach_component(entity, "game.health", json!({ "hp": -1 }))
            .expect_err("range violation must fail");
        assert_eq!(error.code, "invalid_component_value");
        assert!(
            error.message.contains("field 'hp'"),
            "field error reported: {}",
            error.message
        );
        let unknown_field = engine
            .attach_component(entity, "game.health", json!({ "mana": 5 }))
            .expect_err("unknown field must fail");
        assert_eq!(unknown_field.code, "invalid_component_value");
        assert!(unknown_field.message.contains("unknown field 'mana'"));
        assert!(
            engine
                .world()
                .entities
                .get(&entity)
                .expect("entity")
                .extensions
                .is_empty(),
            "failed attach stores nothing"
        );
    }

    #[test]
    fn dynamic_component_attach_rejects_unknown_and_core_ids() {
        let mut engine = Engine::new();
        let entity = spawn_one(&mut engine, "a");
        let unknown = engine
            .attach_component(entity, "game.missing", json!({}))
            .expect_err("unknown component id");
        assert_eq!(unknown.code, "unknown_component");
        for core_id in ["core.transform", "core.sprite", "core.model", "core.script"] {
            let reserved = engine
                .attach_component(entity, core_id, json!({}))
                .expect_err("core components live in typed fields");
            assert_eq!(reserved.code, "component_reserved", "{core_id}");
        }
        let legacy = engine
            .attach_component(entity, "sprite", json!({}))
            .expect_err("legacy ids resolve to core components");
        assert_eq!(legacy.code, "component_reserved");
    }

    #[test]
    fn dynamic_component_remove_emits_event_and_reports_missing() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        let missing = engine
            .remove_component(entity, "game.health")
            .expect_err("component not attached yet");
        assert_eq!(missing.code, "component_not_attached");
        engine
            .attach_component(entity, "game.health", json!({}))
            .expect("attach");
        engine
            .remove_component(entity, "game.health")
            .expect("remove");
        assert!(
            engine
                .world()
                .entities
                .get(&entity)
                .expect("entity")
                .extensions
                .is_empty(),
            "extension entry is removed"
        );
        let events = engine.take_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::ComponentRemoved { component, .. } if component == "game.health"
            )),
            "remove emits component_removed: {events:?}"
        );
    }

    /// Installs a future-version instance the way a session load would leave
    /// it: verbatim data on the entity plus the retained version entry.
    fn attach_future_instance(engine: &mut Engine, entity: EntityId, value: Value) {
        engine
            .world_mut()
            .entities
            .get_mut(&entity)
            .expect("entity")
            .extensions
            .insert("game.health".to_string(), value);
        engine.world_mut().extensions.insert(
            COMPONENT_VERSIONS_KEY.to_string(),
            json!({ "game.health": 9 }),
        );
    }

    #[test]
    fn attach_component_rejects_current_writes_while_future_instances_exist() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        attach_future_instance(
            &mut engine,
            entity,
            json!({ "hp": 55, "futureField": true }),
        );
        let revision = engine.revision();

        let error = engine
            .attach_component(entity, "game.health", json!({ "hp": 10 }))
            .expect_err("future-version instances block current-version writes");
        assert_eq!(error.code, "component_future_version");
        let details = error.details.expect("details");
        assert_eq!(details["component"], json!("game.health"));
        assert_eq!(details["sceneVersion"], json!(9));
        assert_eq!(details["registryVersion"], json!(2));
        assert_eq!(details["futureInstances"], json!(1));
        assert_eq!(
            engine.revision(),
            revision,
            "refused attach keeps the revision"
        );
        assert_eq!(
            stored_value(&engine, entity, "game.health"),
            json!({ "hp": 55, "futureField": true }),
            "the future instance is untouched"
        );
        let events = engine.take_events();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ComponentAttached { .. })),
            "a refused attach emits no component_attached: {events:?}"
        );

        // The command path is guarded by the same rule.
        let command = engine
            .apply(Command::AttachComponent {
                entity,
                component: "game.health".into(),
                value: json!({}),
            })
            .expect_err("command attach hits the guard too");
        assert_eq!(command.code, "component_future_version");
        assert_eq!(
            engine.revision(),
            revision,
            "the refused command keeps the revision"
        );
    }

    #[test]
    fn removing_future_instances_unlocks_writes_and_drops_the_entry() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        let first = spawn_one(&mut engine, "a");
        let second = spawn_one(&mut engine, "b");
        for entity in [first, second] {
            attach_future_instance(&mut engine, entity, json!({ "hp": 1, "futureField": true }));
        }

        // One future instance left: writes stay forbidden, entry retained.
        engine
            .remove_component(first, "game.health")
            .expect("remove is allowed on a future instance");
        assert_eq!(
            engine
                .world()
                .extensions
                .get(COMPONENT_VERSIONS_KEY)
                .expect("table")["game.health"],
            json!(9),
            "the retained entry stays while instances remain"
        );
        let refused = engine
            .attach_component(second, "game.health", json!({ "hp": 10 }))
            .expect_err("one future instance still blocks writes");
        assert_eq!(refused.code, "component_future_version");

        // Removing the last future instance drops the retained entry, so
        // current-version writes work again and saves record the registry
        // version.
        engine
            .remove_component(second, "game.health")
            .expect("remove last future instance");
        assert!(
            engine
                .world()
                .extensions
                .get(COMPONENT_VERSIONS_KEY)
                .is_none(),
            "the empty table is removed from the live world"
        );
        engine
            .attach_component(second, "game.health", json!({ "hp": 10 }))
            .expect("writes unlock once future instances are gone");
        let value =
            crate::scene::scene_value_with_registry(engine.world(), "rebuild", engine.registry())
                .expect("scene value");
        assert_eq!(
            value["extensions"][COMPONENT_VERSIONS_KEY]["game.health"],
            json!(2),
            "the save rebuilds the table at the registry version"
        );
    }

    #[test]
    fn despawn_drops_retained_entries_without_instances() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        attach_future_instance(
            &mut engine,
            entity,
            json!({ "hp": 55, "futureField": true }),
        );
        engine.apply(Command::Despawn { entity }).expect("despawn");
        assert!(
            engine
                .world()
                .extensions
                .get(COMPONENT_VERSIONS_KEY)
                .is_none(),
            "despawning the last carrier prunes the retained entry"
        );
        let other = spawn_one(&mut engine, "b");
        engine
            .attach_component(other, "game.health", json!({ "hp": 10 }))
            .expect("writes are not blocked by the despawned instance");
    }

    #[test]
    fn unregister_blocked_while_in_use_then_force_strips_entities() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let first = spawn_one(&mut engine, "a");
        let second = spawn_one(&mut engine, "b");
        for entity in [first, second] {
            engine
                .attach_component(entity, "game.health", json!({}))
                .expect("attach");
        }
        let blocked = engine
            .unregister_component("game.health", false)
            .expect_err("component still in use");
        assert_eq!(blocked.code, "component_in_use");
        assert!(
            blocked.message.contains("2 entities"),
            "usage count reported: {}",
            blocked.message
        );
        assert_eq!(
            blocked.details.as_ref().expect("details")["usageCount"],
            json!(2)
        );
        assert!(
            engine.registry().resolve("game.health").is_some(),
            "blocked unregister keeps the component"
        );

        let forced = engine
            .unregister_component("game.health", true)
            .expect("force unregister");
        assert_eq!(forced["stripped"], json!(2));
        assert!(engine.registry().resolve("game.health").is_none());
        for entity in [first, second] {
            assert!(
                engine
                    .world()
                    .entities
                    .get(&entity)
                    .expect("entity")
                    .extensions
                    .is_empty(),
                "force strips the component from entity {entity}"
            );
        }
        let events = engine.take_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::ComponentRemoved { .. }))
                .count(),
            2,
            "each stripped entity emits component_removed"
        );
        let attach = engine
            .attach_component(first, "game.health", json!({}))
            .expect_err("component no longer registered");
        assert_eq!(attach.code, "unknown_component");
    }

    #[test]
    fn core_components_refuse_unregister_and_duplicates_fail() {
        let mut engine = Engine::new();
        for core_id in ["core.transform", "core.sprite", "core.model", "core.script"] {
            let refused = engine
                .unregister_component(core_id, true)
                .expect_err("core components cannot be unregistered");
            assert_eq!(refused.code, "component_reserved", "{core_id}");
        }
        engine
            .register_component(health_descriptor())
            .expect("register");
        let duplicate = engine
            .register_component(health_descriptor())
            .expect_err("duplicate component id");
        assert_eq!(duplicate.code, "duplicate_component");
    }

    #[test]
    fn register_and_unregister_emit_registry_events() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let events = engine.take_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::ComponentRegistered { component } if component == "game.health"
            )),
            "register emits component_registered: {events:?}"
        );
        engine
            .unregister_component("game.health", false)
            .expect("unregister");
        let events = engine.take_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::ComponentUnregistered { component } if component == "game.health"
            )),
            "unregister emits component_unregistered: {events:?}"
        );
    }

    #[test]
    fn migrate_component_value_walks_the_registered_chain() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(3))
            .expect("register");
        engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value| {
                    // v1 stored hit points under 'hp'; v2 renames the field.
                    let object = value.as_object().ok_or("must be an object")?;
                    let hp = object.get("hp").ok_or("missing 'hp'")?;
                    Ok(json!({ "health": hp }))
                }),
            )
            .expect("step 1 -> 2");
        engine
            .register_component_migrator(
                "game.health",
                2,
                3,
                Box::new(|value| {
                    let mut object = value.as_object().ok_or("must be an object")?.clone();
                    object.insert("shield".to_string(), json!(0));
                    Ok(Value::Object(object))
                }),
            )
            .expect("step 2 -> 3");
        let migrated = engine
            .migrate_component_value("game.health", json!({ "hp": 40 }), 1, 3)
            .expect("chain walk");
        assert_eq!(migrated, json!({ "health": 40, "shield": 0 }));
        let same = engine
            .migrate_component_value("game.health", json!({ "health": 1 }), 3, 3)
            .expect("same version is a no-op");
        assert_eq!(same, json!({ "health": 1 }));
        let missing = engine
            .migrate_component_value("game.health", json!({}), 3, 9)
            .expect_err("no step starts at version 3");
        assert_eq!(missing.code, "missing_migrator");
        let details = missing.details.expect("details");
        assert_eq!(details["at"], json!(3));
        assert_eq!(details["from"], json!(3));
        assert_eq!(details["to"], json!(9));
        let downgrade = engine
            .migrate_component_value("game.health", json!({}), 2, 1)
            .expect_err("downgrades are refused");
        assert_eq!(downgrade.code, "invalid_migration_step");
    }

    #[test]
    fn migrator_registration_validates_component_and_steps() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        let unknown = engine
            .register_component_migrator(
                "game.missing",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect_err("unknown component id");
        assert_eq!(unknown.code, "unknown_component");
        let core = engine
            .register_component_migrator(
                "core.sprite",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect_err("core components have no migrators");
        assert_eq!(core.code, "component_reserved");
        let backwards = engine
            .register_component_migrator(
                "game.health",
                2,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect_err("step must increase the version");
        assert_eq!(backwards.code, "invalid_migration_step");
        let beyond = engine
            .register_component_migrator(
                "game.health",
                1,
                5,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect_err("target exceeds the registered version");
        assert_eq!(beyond.code, "invalid_migration_step");
        engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect("first step");
        let duplicate = engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect_err("duplicate from-version");
        assert_eq!(duplicate.code, "duplicate_migrator");
        assert_eq!(duplicate.details.expect("details")["existingTo"], json!(2));
    }

    #[test]
    fn unregister_component_drops_its_migrators() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value| {
                    let object = value.as_object().ok_or("must be an object")?;
                    let hp = object.get("hp").ok_or("missing 'hp'")?;
                    Ok(json!({ "health": hp }))
                }),
            )
            .expect("migrator");
        engine
            .unregister_component("game.health", false)
            .expect("unregister");
        let missing = engine
            .migrate_component_value("game.health", json!({ "hp": 1 }), 1, 2)
            .expect_err("the chain followed the component out");
        assert_eq!(missing.code, "missing_migrator");
        engine
            .register_component(health_descriptor_at(2))
            .expect("re-register");
        let missing = engine
            .migrate_component_value("game.health", json!({ "hp": 1 }), 1, 2)
            .expect_err("re-registering does not resurrect the chain");
        assert_eq!(missing.code, "missing_migrator");
    }

    #[test]
    fn unregister_component_migrators_keeps_the_component_registered() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(2))
            .expect("register");
        engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect("migrator");
        let revision = engine.revision();
        engine.unregister_component_migrators("game.health");
        assert!(
            engine.revision() > revision,
            "removing a chain bumps the revision"
        );
        assert!(
            engine.registry().resolve("game.health").is_some(),
            "the component registration is unaffected"
        );
        let revision = engine.revision();
        engine.unregister_component_migrators("game.health");
        assert_eq!(
            engine.revision(),
            revision,
            "removing an absent chain is a no-op"
        );
    }

    #[test]
    fn snapshot_restore_reverts_registry_and_migrators() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor_at(1))
            .expect("register");
        let snapshot = engine.snapshot();
        let revision = engine.revision();
        engine
            .unregister_component("game.health", false)
            .expect("unregister");
        engine
            .register_component(health_descriptor_at(2))
            .expect("re-register");
        engine
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value: &Value| Ok(value.clone())),
            )
            .expect("migrator");
        engine.restore(snapshot);
        let descriptor = engine
            .registry()
            .resolve("game.health")
            .expect("registration is restored");
        assert_eq!(
            descriptor.version, 1,
            "the snapshot-time version comes back"
        );
        assert_eq!(engine.revision(), revision, "restore reverts the revision");
        let missing = engine
            .migrate_component_value("game.health", json!({}), 1, 2)
            .expect_err("migrator bookkeeping is reverted too");
        assert_eq!(missing.code, "missing_migrator");
        let events = engine.take_events();
        assert_eq!(
            events.len(),
            1,
            "only the pre-snapshot registration event remains: {events:?}"
        );
        assert!(matches!(events[0], Event::ComponentRegistered { .. }));
    }

    #[test]
    fn dynamic_component_data_survives_snapshot_round_trip() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        let snapshot = engine.snapshot();
        engine
            .attach_component(entity, "game.health", json!({ "hp": 42 }))
            .expect("attach");

        let value = serde_json::to_value(engine.world()).expect("serialize world");
        let restored = crate::scene::snapshot_from_value(value).expect("snapshot round trip");
        let stored = restored
            .entities
            .get(&entity)
            .expect("entity")
            .extensions
            .get("game.health")
            .expect("dynamic component preserved")
            .clone();
        assert_eq!(stored["hp"], json!(42));
        assert_eq!(stored["max_hp"], json!(100));

        engine.restore(snapshot);
        assert!(
            engine.registry().resolve("game.health").is_some(),
            "the snapshot carries registrations made before it"
        );
        assert!(
            engine
                .world()
                .entities
                .get(&entity)
                .expect("entity")
                .extensions
                .is_empty(),
            "restore reverts the attach"
        );
    }

    #[test]
    fn dynamic_component_data_round_trips_through_the_scene_format() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        engine
            .attach_component(entity, "game.health", json!({ "hp": 7 }))
            .expect("attach");
        let path = std::env::temp_dir().join(format!(
            "moni-dynamic-components-{}.mnsc",
            std::process::id()
        ));
        crate::scene::save_scene(engine.world(), "dynamic", &path).expect("save scene");
        let loaded = crate::scene::load_scene(&path).expect("load scene");
        let stored = loaded
            .entities
            .get(&entity)
            .expect("entity")
            .extensions
            .get("game.health")
            .expect("dynamic component preserved")
            .clone();
        assert_eq!(stored["hp"], json!(7));
        assert_eq!(stored["max_hp"], json!(100));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn attach_component_command_matches_the_direct_path() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        let revision = engine.revision();
        engine
            .apply(Command::AttachComponent {
                entity,
                component: "game.health".into(),
                value: json!({}),
            })
            .expect("command attach");
        assert_eq!(engine.revision(), revision + 1, "one bump per command");
        let stored = stored_value(&engine, entity, "game.health");
        assert_eq!(stored["hp"], json!(100), "defaults applied via command");
        let invalid = engine
            .apply(Command::AttachComponent {
                entity,
                component: "game.health".into(),
                value: json!({ "hp": 5000 }),
            })
            .expect_err("command path validates too");
        assert_eq!(invalid.code, "invalid_component_value");
    }

    #[test]
    fn rename_entity_validates_and_emits_entity_renamed() {
        let mut engine = Engine::new();
        let entity = spawn_one(&mut engine, "a");
        engine.take_events();
        let revision = engine.revision();
        let bad_names = vec![String::new(), "x".repeat(129)];
        for name in bad_names {
            let error = engine
                .apply(Command::RenameEntity { entity, name })
                .expect_err("name must contain between 1 and 128 characters");
            assert_eq!(error.code, "invalid_entity");
        }
        let missing = engine
            .apply(Command::RenameEntity {
                entity: entity + 10,
                name: "b".into(),
            })
            .expect_err("unknown entity");
        assert_eq!(missing.code, "entity_not_found");
        assert_eq!(
            engine.revision(),
            revision,
            "failed renames keep the revision"
        );
        assert!(engine.take_events().is_empty(), "no events on failure");

        let max_name = "y".repeat(128);
        engine
            .apply(Command::RenameEntity {
                entity,
                name: max_name.clone(),
            })
            .expect("a 128-character name is valid");
        engine
            .apply(Command::RenameEntity {
                entity,
                name: "renamed".into(),
            })
            .expect("rename");
        assert_eq!(
            engine.revision(),
            revision + 2,
            "each successful rename bumps the revision once"
        );
        assert_eq!(engine.world().entities[&entity].name, "renamed");
        let events = engine.take_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::EntityRenamed { entity: renamed, name } if *renamed == entity && name == "renamed"
            )),
            "rename emits entity_renamed: {events:?}"
        );
    }

    #[test]
    fn restore_entity_reinserts_a_snapshot_under_its_explicit_id() {
        let mut engine = Engine::new();
        let first = spawn_one(&mut engine, "a");
        let second = spawn_one(&mut engine, "b");
        let snapshot = engine.world().entities[&second].clone();
        engine
            .apply(Command::Despawn { entity: second })
            .expect("despawn");
        let revision = engine.revision();
        let restored = engine
            .apply(Command::RestoreEntity {
                entity: snapshot.clone(),
            })
            .expect("restore");
        assert_eq!(restored["entity"], json!(second), "the explicit id wins");
        assert_eq!(
            engine.world().entities[&second],
            snapshot,
            "data is reinserted unchanged"
        );
        assert_eq!(
            engine.revision(),
            revision + 1,
            "restore bumps the revision once"
        );
        let events = engine.take_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::EntitySpawned { entity } if *entity == second)),
            "restore reuses entity_spawned: {events:?}"
        );

        // A snapshot with an id beyond the allocator moves next_entity_id
        // past it so later spawns never collide.
        let mut late = snapshot.clone();
        late.id = 100;
        late.name = "late".into();
        engine
            .apply(Command::RestoreEntity { entity: late })
            .expect("restore high id");
        assert!(engine.world().entities.contains_key(&100));
        assert_eq!(engine.world().next_entity_id, 101);
        let spawned = engine
            .apply(Command::Spawn { spec: spec("c") })
            .expect("spawn");
        assert_eq!(spawned["entity"], json!(101));
        let _ = first;
    }

    #[test]
    fn restore_entity_rejects_existing_ids_and_invalid_parts() {
        let mut engine = Engine::new();
        let entity = spawn_one(&mut engine, "a");
        let snapshot = engine.world().entities[&entity].clone();
        let revision = engine.revision();

        let exists = engine
            .apply(Command::RestoreEntity {
                entity: snapshot.clone(),
            })
            .expect_err("the id is taken");
        assert_eq!(exists.code, "entity_exists");
        assert_eq!(
            engine.revision(),
            revision,
            "a refused restore keeps the revision"
        );
        assert_eq!(engine.world().entities.len(), 1);
        assert_eq!(engine.world().next_entity_id, 2);

        let mut zero = snapshot.clone();
        zero.id = 0;
        let zero = engine
            .apply(Command::RestoreEntity { entity: zero })
            .expect_err("entity ids are positive");
        assert_eq!(zero.code, "invalid_entity");

        let mut non_finite = snapshot.clone();
        non_finite.id = 50;
        non_finite.transform = Transform {
            x: f64::NAN,
            y: 0.0,
            rotation: 0.0,
        };
        let non_finite = engine
            .apply(Command::RestoreEntity { entity: non_finite })
            .expect_err("transforms must be finite");
        assert_eq!(non_finite.code, "invalid_transform");

        let mut empty_name = snapshot.clone();
        empty_name.id = 50;
        empty_name.name = String::new();
        let empty_name = engine
            .apply(Command::RestoreEntity { entity: empty_name })
            .expect_err("names must not be empty");
        assert_eq!(empty_name.code, "invalid_entity");

        let mut bad_script = snapshot;
        bad_script.id = 50;
        bad_script.script = Some(ScriptState {
            source: "   ".into(),
            source_path: None,
            enabled: true,
            last_error: None,
        });
        let bad_script = engine
            .apply(Command::RestoreEntity { entity: bad_script })
            .expect_err("script sources must not be empty");
        assert_eq!(bad_script.code, "invalid_script");
        assert!(
            engine.world().next_entity_id == 2 && engine.world().entities.len() == 1,
            "refused restores leave the world untouched"
        );
    }

    #[test]
    fn despawn_restore_round_trip_keeps_entity_data_identical() {
        let mut engine = Engine::new();
        engine
            .register_component(health_descriptor())
            .expect("register");
        let entity = spawn_one(&mut engine, "a");
        engine
            .apply(Command::SetSprite {
                entity,
                sprite: Some(Sprite::default()),
            })
            .expect("sprite");
        engine
            .attach_component(entity, "game.health", json!({ "hp": 42 }))
            .expect("attach");
        let snapshot = engine.world().entities[&entity].clone();
        engine.apply(Command::Despawn { entity }).expect("despawn");
        engine
            .apply(Command::RestoreEntity {
                entity: snapshot.clone(),
            })
            .expect("restore");
        assert_eq!(
            engine.world().entities[&entity],
            snapshot,
            "despawn -> restore round trip is lossless"
        );
        assert_eq!(
            stored_value(&engine, entity, "game.health"),
            json!({ "hp": 42, "max_hp": 100 }),
            "dynamic component data survives the round trip"
        );
    }
}
