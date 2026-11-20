use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
}

impl EngineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
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
        if let Some(name) = &self.name
            && (name.is_empty() || name.chars().count() > 128)
        {
            return Err(EngineError::new(
                "invalid_entity",
                "entity name must contain between 1 and 128 characters",
            ));
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
            entity.transform.validate()?;
            if let Some(sprite) = &entity.sprite {
                sprite.validate()?;
            }
            if let Some(model) = &entity.model {
                model.validate()?;
            }
            if let Some(script) = &entity.script {
                validate_script_source(&script.source)?;
            }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    EntitySpawned {
        entity: EntityId,
    },
    EntityDespawned {
        entity: EntityId,
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

#[derive(Debug, Default)]
pub struct Engine {
    world: World,
    events: Vec<Event>,
    script_runtime: Option<Box<dyn ScriptRuntime>>,
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

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
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
        self.events.push(Event::WorldReset);
    }

    pub fn load_world(&mut self, world: World) -> Result<(), EngineError> {
        world.validate()?;
        self.world = world;
        if let Some(runtime) = &mut self.script_runtime {
            runtime.clear();
        }
        self.events.push(Event::WorldLoaded);
        Ok(())
    }

    pub fn apply(&mut self, command: Command) -> Result<Value, EngineError> {
        match command {
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
                if self.world.entities.remove(&entity).is_none() {
                    return Err(entity_not_found(entity));
                }
                self.events.push(Event::EntityDespawned { entity });
                Ok(json!({ "entity": entity }))
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
        }
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
