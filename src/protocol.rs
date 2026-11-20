use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Command, Engine, EngineError, EntitySpec, Event, Sprite, Transform};
use crate::render::render_svg;
use crate::scene3d::{Camera3d, Model3d, Transform3d};

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    pub events: Vec<Event>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityArgs {
    entity: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetTransformArgs {
    entity: u64,
    transform: Transform,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetSpriteArgs {
    entity: u64,
    sprite: Option<Sprite>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetModelArgs {
    entity: u64,
    model: Option<Model3d>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetModelTransformArgs {
    entity: u64,
    transform: Transform3d,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetCameraArgs {
    camera: Camera3d,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachScriptArgs {
    entity: u64,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetScriptEnabledArgs {
    entity: u64,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepArgs {
    count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    entity: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderArgs {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadSnapshotArgs {
    snapshot: Value,
}

pub fn handle_line(engine: &mut Engine, line: &str) -> (Response, bool) {
    let request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(error) => {
            return (
                Response::error(
                    Value::Null,
                    EngineError::new(
                        "invalid_request",
                        format!("invalid NDJSON request: {error}"),
                    ),
                    Vec::new(),
                ),
                false,
            );
        }
    };

    let id = request.id.clone();
    let (result, quit) = dispatch(engine, &request.command, request.args);
    let events = engine.take_events();
    let response = match result {
        Ok(result) => Response::success(id, result, events),
        Err(error) => Response::error(id, error, events),
    };
    (response, quit)
}

impl Response {
    fn success(id: Value, result: Value, events: Vec<Event>) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
            events,
        }
    }

    fn error(id: Value, error: EngineError, events: Vec<Event>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: error.code,
                message: error.message,
            }),
            events,
        }
    }
}

fn dispatch(engine: &mut Engine, command: &str, args: Value) -> (Result<Value, EngineError>, bool) {
    let result = match command {
        "capabilities" => Ok(capabilities()),
        "spawn" => {
            parse_args::<EntitySpec>(args).and_then(|spec| engine.apply(Command::Spawn { spec }))
        }
        "despawn" => parse_args::<EntityArgs>(args).and_then(|args| {
            engine.apply(Command::Despawn {
                entity: args.entity,
            })
        }),
        "set_transform" => parse_args::<SetTransformArgs>(args).and_then(|args| {
            engine.apply(Command::SetTransform {
                entity: args.entity,
                transform: args.transform,
            })
        }),
        "set_sprite" => parse_args::<SetSpriteArgs>(args).and_then(|args| {
            engine.apply(Command::SetSprite {
                entity: args.entity,
                sprite: args.sprite,
            })
        }),
        "set_model" => parse_args::<SetModelArgs>(args).and_then(|args| {
            engine.apply(Command::SetModel {
                entity: args.entity,
                model: args.model,
            })
        }),
        "set_model_transform" => parse_args::<SetModelTransformArgs>(args).and_then(|args| {
            engine.apply(Command::SetModelTransform {
                entity: args.entity,
                transform: args.transform,
            })
        }),
        "set_camera" => parse_args::<SetCameraArgs>(args).and_then(|args| {
            engine.apply(Command::SetCamera {
                camera: args.camera,
            })
        }),
        "attach_script" => parse_args::<AttachScriptArgs>(args).and_then(|args| {
            engine.apply(Command::AttachScript {
                entity: args.entity,
                source: args.source,
            })
        }),
        "set_script_enabled" => parse_args::<SetScriptEnabledArgs>(args).and_then(|args| {
            engine.apply(Command::SetScriptEnabled {
                entity: args.entity,
                enabled: args.enabled,
            })
        }),
        "step" => parse_args::<StepArgs>(args).and_then(|args| engine.step(args.count)),
        "query" => parse_args::<QueryArgs>(args).and_then(|args| query(engine, args.entity)),
        "snapshot" => serde_json::to_value(engine.world()).map_err(|error| {
            EngineError::new(
                "serialization_error",
                format!("could not serialize world: {error}"),
            )
        }),
        "load_snapshot" => parse_args::<LoadSnapshotArgs>(args).and_then(|args| {
            let world = crate::scene::snapshot_from_value(args.snapshot)
                .map_err(|error| EngineError::new("invalid_snapshot", error.to_string()))?;
            engine.load_world(world)?;
            Ok(json!({ "loaded": true }))
        }),
        "save" => parse_args::<PathArgs>(args).and_then(|args| save(engine, &args.path)),
        "load" => parse_args::<PathArgs>(args).and_then(|args| load(engine, &args.path)),
        "render" => parse_args::<RenderArgs>(args).and_then(|args| render(engine, args.path)),
        "reset" => {
            engine.reset();
            Ok(json!({ "reset": true }))
        }
        "quit" => return (Ok(json!({ "quit": true })), true),
        _ => Err(EngineError::new(
            "unknown_command",
            format!("unknown command '{command}'"),
        )),
    };
    (result, false)
}

fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, EngineError> {
    serde_json::from_value(args).map_err(|error| {
        EngineError::new(
            "invalid_arguments",
            format!("invalid command arguments: {error}"),
        )
    })
}

fn query(engine: &Engine, entity: Option<u64>) -> Result<Value, EngineError> {
    match entity {
        Some(id) => {
            let entity = engine.world().entities.get(&id).ok_or_else(|| {
                EngineError::new("entity_not_found", format!("entity {id} does not exist"))
            })?;
            serde_json::to_value(entity).map_err(|error| {
                EngineError::new(
                    "serialization_error",
                    format!("could not serialize entity: {error}"),
                )
            })
        }
        None => serde_json::to_value(engine.world()).map_err(|error| {
            EngineError::new(
                "serialization_error",
                format!("could not serialize world: {error}"),
            )
        }),
    }
}

fn save(engine: &Engine, raw_path: &str) -> Result<Value, EngineError> {
    let path = workspace_path(raw_path, true)?;
    let contents = serde_json::to_string_pretty(engine.world()).map_err(|error| {
        EngineError::new(
            "serialization_error",
            format!("could not serialize world: {error}"),
        )
    })?;
    fs::write(&path, contents).map_err(|error| io_error("could not save snapshot", error))?;
    Ok(json!({ "path": raw_path }))
}

fn load(engine: &mut Engine, raw_path: &str) -> Result<Value, EngineError> {
    let path = workspace_path(raw_path, false)?;
    let contents =
        fs::read_to_string(&path).map_err(|error| io_error("could not read snapshot", error))?;
    let world = crate::scene::load_snapshot(&contents)
        .map_err(|error| EngineError::new("invalid_snapshot", error.to_string()))?;
    engine.load_world(world)?;
    Ok(json!({ "path": raw_path }))
}

fn render(engine: &Engine, raw_path: Option<String>) -> Result<Value, EngineError> {
    let svg = render_svg(engine.world());
    if let Some(raw_path) = raw_path {
        let path = workspace_path(&raw_path, true)?;
        fs::write(&path, &svg).map_err(|error| io_error("could not write SVG", error))?;
        Ok(json!({ "path": raw_path, "bytes": svg.len() }))
    } else {
        Ok(json!({ "svg": svg }))
    }
}

fn workspace_path(raw_path: &str, create_parent: bool) -> Result<PathBuf, EngineError> {
    if raw_path.trim().is_empty() {
        return Err(EngineError::new("invalid_path", "path must not be empty"));
    }
    let path = Path::new(raw_path);
    let has_forbidden_component = path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    if path.is_absolute() || has_forbidden_component {
        return Err(EngineError::new(
            "path_outside_workspace",
            "snapshot and render paths must stay inside the current workspace",
        ));
    }
    let root =
        env::current_dir().map_err(|error| io_error("could not resolve workspace", error))?;
    let path = root.join(path);
    if create_parent && let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("could not create output directory", error))?;
    }
    Ok(path)
}

fn io_error(context: &str, error: std::io::Error) -> EngineError {
    EngineError::new("io_error", format!("{context}: {error}"))
}

pub fn capabilities() -> Value {
    json!({
        "protocol": "moni-engine-control/1",
        "transport": "NDJSON over stdin/stdout",
        "world": {
            "snapshotVersion": 2,
            "tickRate": 60,
            "coordinateSystem": "2D pixels with y down; right-handed 3D with y up; rotations in radians",
            "camera3d": "Perspective or orthographic camera with position, target, fov_y, orthographic_height, near and far",
            "meshes3d": ["cube", "plane", "fbx"],
            "fbxModel": "Workspace-relative source path; geometry and base colors loaded once per renderer",
            "scene": {
                "format": "moni-scene",
                "version": 2,
                "readVersions": [1, 2],
                "extension": "mnsc",
                "source": "Declarative camera, lights, entities and workspace-relative component resources"
            },
            "rendering": {
                "material": "Base color, metallic and roughness",
                "lights": "One directional light plus up to 8 point lights",
                "shadows": "1024x1024 directional shadow map with 3x3 PCF",
                "gpuTimestamps": "Optional benchmark-only main and shadow pass timings"
            }
        },
        "commands": {
            "capabilities": { "args": {} },
            "spawn": { "args": { "spec": "EntitySpec" }, "returns": ["entity"] },
            "despawn": { "args": { "entity": "number" } },
            "set_transform": { "args": { "entity": "number", "transform": "Transform" } },
            "set_sprite": { "args": { "entity": "number", "sprite": "Sprite|null" } },
            "set_model": { "args": { "entity": "number", "model": "Model3d|null" } },
            "set_model_transform": { "args": { "entity": "number", "transform": "Transform3d" } },
            "set_camera": { "args": { "camera": "Camera3d" } },
            "attach_script": { "args": { "entity": "number", "source": "compiled JavaScript" } },
            "set_script_enabled": { "args": { "entity": "number", "enabled": "boolean" } },
            "step": { "args": { "count": "1..10000" } },
            "query": { "args": { "entity": "number?" } },
            "snapshot": { "args": {} },
            "load_snapshot": { "args": { "snapshot": "World" } },
            "save": { "args": { "path": "workspace-relative path" } },
            "load": { "args": { "path": "workspace-relative path" } },
            "render": { "args": { "path": "workspace-relative path?" } },
            "reset": { "args": {} },
            "quit": { "args": {} }
        },
        "components": crate::components::ComponentRegistry::new().agent_schemas(),
        "host": {
            "scheduler": "single-thread dependency-ordered static plugins",
            "maxSystemTasks": 1024,
            "stages": ["startup", "fixed_update", "render", "shutdown"],
            "threadAffinity": {
                "core.simulation": "logic",
                "core.render": "render"
            }
        },
        "script": {
            "language": "TypeScript compiled to JavaScript",
            "runtime": "QuickJS",
            "contextLifetime": "persistent per scripted entity; source evaluated once; reset on replacement, world load, or error",
            "snapshot": "script source only; JavaScript heap is not persisted",
            "hook": "function onTick(ctx: ScriptContext, dt: number): void",
            "limits": {
                "timePerHookMilliseconds": 50,
                "memoryBytes": 33554432,
                "commandsPerHook": 1024,
                "sourceBytes": 262144
            },
            "definitions": "examples/engine-api.d.ts"
        },
        "events": [
            "entity_spawned",
            "entity_despawned",
            "transform_changed",
            "sprite_changed",
            "model_changed",
            "camera_changed",
            "script_attached",
            "script_enabled_changed",
            "script_log",
            "script_error",
            "invalid_script_command",
            "script_command_rejected",
            "simulation_stepped",
            "world_loaded",
            "world_reset"
        ]
    })
}
