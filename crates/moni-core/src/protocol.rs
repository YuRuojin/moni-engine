use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Command, Engine, EngineError, Entity, EntitySpec, Event, Sprite, Transform};
use crate::host::{CommandDescriptor, SideEffects};
use crate::render::render_svg;
use crate::scene3d::{Camera3d, Model3d, Transform3d};
use crate::session::RuntimeSession;

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Value,
    /// Optimistic concurrency for single commands: fail with
    /// `revision_conflict` before dispatch unless the session revision
    /// matches (same check as a batch's `expectedRevision`).
    #[serde(default, rename = "expectedRevision")]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    pub events: Vec<Event>,
    /// World revision after this request; use as `expectedRevision` for
    /// optimistic concurrency on the next mutation.
    pub revision: u64,
    /// Session-wide event watermark: every event up to this sequence number
    /// has been delivered in a response.
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    /// Optional structured context, e.g. the failing sub-command of a batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
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
struct AttachComponentArgs {
    entity: u64,
    component: String,
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveComponentArgs {
    entity: u64,
    component: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameEntityArgs {
    entity: u64,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreEntityArgs {
    entity: Entity,
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

impl Response {
    pub(crate) fn success(
        id: Value,
        result: Value,
        events: Vec<Event>,
        revision: u64,
        seq: u64,
    ) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
            events,
            revision,
            seq,
        }
    }

    pub(crate) fn error(
        id: Value,
        error: EngineError,
        events: Vec<Event>,
        revision: u64,
        seq: u64,
    ) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: error.code,
                message: error.message,
                details: error.details,
            }),
            events,
            revision,
            seq,
        }
    }
}

/// Engine-level command execution shared by the top-level dispatch, atomic
/// batches, and dry runs. Covers the built-in world commands only.
pub(crate) fn execute_engine_command(
    engine: &mut Engine,
    command: &str,
    args: &Value,
) -> Result<Value, EngineError> {
    match command {
        "spawn" => parse_args::<EntitySpec>(args.clone())
            .and_then(|spec| engine.apply(Command::Spawn { spec })),
        "despawn" => parse_args::<EntityArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::Despawn {
                entity: args.entity,
            })
        }),
        "rename_entity" => parse_args::<RenameEntityArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::RenameEntity {
                entity: args.entity,
                name: args.name,
            })
        }),
        "restore_entity" => parse_args::<RestoreEntityArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::RestoreEntity {
                entity: args.entity,
            })
        }),
        "set_transform" => parse_args::<SetTransformArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::SetTransform {
                entity: args.entity,
                transform: args.transform,
            })
        }),
        "set_sprite" => parse_args::<SetSpriteArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::SetSprite {
                entity: args.entity,
                sprite: args.sprite,
            })
        }),
        "set_model" => parse_args::<SetModelArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::SetModel {
                entity: args.entity,
                model: args.model,
            })
        }),
        "set_model_transform" => {
            parse_args::<SetModelTransformArgs>(args.clone()).and_then(|args| {
                engine.apply(Command::SetModelTransform {
                    entity: args.entity,
                    transform: args.transform,
                })
            })
        }
        "set_camera" => parse_args::<SetCameraArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::SetCamera {
                camera: args.camera,
            })
        }),
        "attach_script" => parse_args::<AttachScriptArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::AttachScript {
                entity: args.entity,
                source: args.source,
            })
        }),
        "set_script_enabled" => parse_args::<SetScriptEnabledArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::SetScriptEnabled {
                entity: args.entity,
                enabled: args.enabled,
            })
        }),
        "attach_component" => parse_args::<AttachComponentArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::AttachComponent {
                entity: args.entity,
                component: args.component,
                value: args.value,
            })
        }),
        "remove_component" => parse_args::<RemoveComponentArgs>(args.clone()).and_then(|args| {
            engine.apply(Command::RemoveComponent {
                entity: args.entity,
                component: args.component,
            })
        }),
        "step" => parse_args::<StepArgs>(args.clone()).and_then(|args| engine.step(args.count)),
        "query" => {
            parse_args::<QueryArgs>(args.clone()).and_then(|args| query(engine, args.entity))
        }
        "snapshot" => serde_json::to_value(engine.world()).map_err(|error| {
            EngineError::new(
                "serialization_error",
                format!("could not serialize world: {error}"),
            )
        }),
        "load_snapshot" => parse_args::<LoadSnapshotArgs>(args.clone()).and_then(|args| {
            let world = crate::scene::snapshot_from_value(args.snapshot)
                .map_err(|error| EngineError::new("invalid_snapshot", error.to_string()))?;
            engine.load_world(world)?;
            Ok(json!({ "loaded": true }))
        }),
        "save" => parse_args::<PathArgs>(args.clone()).and_then(|args| save(engine, &args.path)),
        "load" => parse_args::<PathArgs>(args.clone()).and_then(|args| load(engine, &args.path)),
        "render" => {
            parse_args::<RenderArgs>(args.clone()).and_then(|args| render(engine, args.path))
        }
        "reset" => {
            engine.reset();
            Ok(json!({ "reset": true }))
        }
        _ => Err(EngineError::new(
            "unknown_command",
            format!("unknown command '{command}'"),
        )),
    }
}

/// Builds one built-in descriptor; only the varying fields are spelled out
/// per entry. Built-ins carry no permission and no side effects unless
/// stated.
fn builtin_descriptor(
    mutates_world: bool,
    supports_batch: bool,
    supports_dry_run: bool,
    args_schema: Value,
) -> CommandDescriptor {
    CommandDescriptor {
        owner: "core",
        mutates_world,
        supports_batch,
        supports_dry_run,
        side_effects: SideEffects::NONE,
        required_permission: None,
        args_schema,
        returns_schema: Value::Null,
    }
}

/// Static metadata for every built-in control command. This table is the
/// single source of truth for the reserved plugin-command names, the batch
/// and dry-run transaction checks, and the generated capabilities `commands`
/// section. Flags are honest: `render` is one descriptor flagged `file`
/// because the same command may write an SVG to disk with or without a path;
/// `batch` is flagged world-mutating because its contained requests may
/// mutate the world; `load` mutates the world while `save` only writes a
/// file.
pub(crate) fn builtin_commands() -> &'static BTreeMap<&'static str, CommandDescriptor> {
    static TABLE: OnceLock<BTreeMap<&'static str, CommandDescriptor>> = OnceLock::new();
    TABLE.get_or_init(|| {
        BTreeMap::from([
            (
                "attach_component",
                builtin_descriptor(true, true, true, json!({
                    "entity": "number",
                    "component": "registered dynamic component id",
                    "value": "component value object; schema defaults are applied before validation",
                }))
                .returns_schema(json!(["entity", "component"])),
            ),
            (
                "attach_script",
                builtin_descriptor(
                    true,
                    true,
                    false,
                    json!({ "entity": "number", "source": "compiled JavaScript" }),
                )
                .side_effects(SideEffects::SCRIPT),
            ),
            (
                "batch",
                builtin_descriptor(true, false, false, json!({
                    "requests": "[{ command, args }] — atomic sequence",
                    "expectedRevision": "u64? — optimistic concurrency check",
                    "dryRun": "boolean? — validate against a scratch world copy",
                }))
                .returns_schema(json!(["applied", "results", "revision"])),
            ),
            (
                "capabilities",
                builtin_descriptor(false, false, true, json!({})),
            ),
            (
                "despawn",
                builtin_descriptor(true, true, true, json!({ "entity": "number" })),
            ),
            (
                "load",
                builtin_descriptor(true, false, false, json!({ "path": "workspace-relative path" }))
                    .side_effects(SideEffects::FILE),
            ),
            (
                // Replaces the World and clears script hosts; `EngineSnapshot`
                // does not capture the JS heap, so batches cannot roll this
                // back transactionally.
                "load_snapshot",
                builtin_descriptor(true, false, true, json!({ "snapshot": "World" }))
                    .side_effects(SideEffects::SCRIPT),
            ),
            (
                "query",
                builtin_descriptor(false, true, true, json!({ "entity": "number?" })),
            ),
            (
                "quit",
                builtin_descriptor(false, false, false, json!({})),
            ),
            (
                "render",
                builtin_descriptor(false, false, false, json!({ "path": "workspace-relative path?" }))
                    .side_effects(SideEffects::FILE),
            ),
            (
                "rename_entity",
                builtin_descriptor(true, true, true, json!({
                    "entity": "number",
                    "name": "entity name with 1..128 characters",
                }))
                .returns_schema(json!(["entity"])),
            ),
            (
                // Clears script hosts along with the World; not transactional
                // until script state can be snapshotted.
                "reset",
                builtin_descriptor(true, false, true, json!({})).side_effects(SideEffects::SCRIPT),
            ),
            (
                // Pure data operation: reinserts an entity snapshot under its
                // explicit id (undo/redo stable-ID semantics) and moves
                // next_entity_id past it.
                "restore_entity",
                builtin_descriptor(true, true, true, json!({
                    "entity": "full Entity snapshot; the id must not exist",
                }))
                .returns_schema(json!(["entity"])),
            ),
            (
                "save",
                builtin_descriptor(false, false, false, json!({ "path": "workspace-relative path" }))
                    .side_effects(SideEffects::FILE),
            ),
            (
                "set_camera",
                builtin_descriptor(true, true, true, json!({ "camera": "Camera3d" })),
            ),
            (
                "set_model",
                builtin_descriptor(true, true, true, json!({ "entity": "number", "model": "Model3d|null" })),
            ),
            (
                "set_model_transform",
                builtin_descriptor(true, true, true, json!({ "entity": "number", "transform": "Transform3d" })),
            ),
            (
                "set_script_enabled",
                builtin_descriptor(true, true, false, json!({ "entity": "number", "enabled": "boolean" }))
                    .side_effects(SideEffects::SCRIPT),
            ),
            (
                "set_sprite",
                builtin_descriptor(true, true, true, json!({ "entity": "number", "sprite": "Sprite|null" })),
            ),
            (
                "set_transform",
                builtin_descriptor(true, true, true, json!({ "entity": "number", "transform": "Transform" })),
            ),
            (
                "snapshot",
                builtin_descriptor(false, true, true, json!({})),
            ),
            (
                "spawn",
                builtin_descriptor(true, true, true, json!("EntitySpec"))
                    .returns_schema(json!(["entity"])),
            ),
            (
                // Advances script hosts per tick; `EngineSnapshot` restores
                // the World but not the JS heap, so batches must not include
                // ticks until script state can be snapshotted.
                "step",
                builtin_descriptor(true, false, false, json!({ "count": "1..10000" }))
                    .side_effects(SideEffects::SCRIPT),
            ),
            (
                "remove_component",
                builtin_descriptor(true, true, true, json!({ "entity": "number", "component": "registered dynamic component id" }))
                    .returns_schema(json!(["entity", "component"])),
            ),
        ])
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BatchArgs {
    requests: Vec<BatchRequest>,
    /// Optimistic concurrency: fail without changes unless the current world
    /// revision matches.
    #[serde(default)]
    expected_revision: Option<u64>,
    /// Validate and preview the batch against a scratch copy of the world
    /// without touching session state.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest {
    command: String,
    #[serde(default)]
    args: Value,
}

fn batch(session: &mut RuntimeSession, args: Value) -> Result<Value, EngineError> {
    let batch: BatchArgs = parse_args(args)?;
    if let Some(expected) = batch.expected_revision
        && session.revision() != expected
    {
        return Err(EngineError::new(
            "revision_conflict",
            format!(
                "expected world revision {} but the session is at revision {}",
                expected,
                session.revision()
            ),
        ));
    }
    for request in &batch.requests {
        if let Some(error) = batch_rejection(session, &request.command) {
            return Err(error);
        }
    }

    if batch.dry_run {
        return batch_dry_run(session, &batch);
    }

    // Atomic execution: snapshot first, restore on the first failure so no
    // partial modification, event, or revision change is observable.
    let snapshot = session.engine_mut().snapshot();
    let mut results = Vec::with_capacity(batch.requests.len());
    for (index, request) in batch.requests.iter().enumerate() {
        let outcome = execute_subcommand(session, &request.command, &request.args);
        match outcome {
            Ok(result) => results.push(result),
            Err(error) => {
                session.engine_mut().restore(snapshot);
                return Err(EngineError::new(
                    "batch_failed",
                    format!(
                        "request {index} ('{}') failed: {}: {}; the batch was rolled back",
                        request.command, error.code, error.message
                    ),
                )
                .with_details(json!({
                    "index": index,
                    "command": request.command,
                    "cause": { "code": error.code, "message": error.message },
                    "rolledBack": true,
                })));
            }
        }
    }
    Ok(json!({
        "applied": results.len(),
        "dryRun": false,
        "revision": session.revision(),
        "results": results,
    }))
}

/// Rejects commands whose descriptor declares no batch support: built-ins
/// through the static table, plugin commands through their registered
/// descriptor. Unknown commands pass here and fail later with
/// `unknown_command` during execution.
fn batch_rejection(session: &mut RuntimeSession, command: &str) -> Option<EngineError> {
    let supports_batch = if let Some(descriptor) = builtin_commands().get(command) {
        descriptor.supports_batch
    } else if let Some(descriptor) = session
        .host_mut()
        .plugin_command_descriptor(command)
        .map(|descriptor| descriptor.supports_batch)
    {
        descriptor
    } else {
        return None;
    };
    if supports_batch {
        return None;
    }
    if builtin_commands().contains_key(command) {
        Some(EngineError::new(
            "invalid_batch_command",
            format!(
                "command '{command}' cannot run inside a batch (file I/O and session control are not transactional)"
            ),
        ))
    } else {
        Some(EngineError::new(
            "invalid_batch_command",
            format!(
                "plugin command '{command}' cannot run inside a batch (the command declares no batch support)"
            ),
        ))
    }
}

/// Rejects commands whose descriptor declares no dry-run support: built-ins
/// through the static table, plugin commands through their registered
/// descriptor. Unknown commands pass here and fail later with
/// `unknown_command` during execution.
fn dry_run_rejection(session: &mut RuntimeSession, command: &str) -> Option<EngineError> {
    let supports_dry_run = if let Some(descriptor) = builtin_commands().get(command) {
        descriptor.supports_dry_run
    } else if let Some(supports) = session
        .host_mut()
        .plugin_command_descriptor(command)
        .map(|descriptor| descriptor.supports_dry_run)
    {
        supports
    } else {
        return None;
    };
    if supports_dry_run {
        return None;
    }
    if builtin_commands().contains_key(command) {
        Some(EngineError::new(
            "dry_run_unsupported",
            format!("command '{command}' needs live runtime state and is not supported in dry_run"),
        ))
    } else {
        Some(EngineError::new(
            "dry_run_unsupported",
            format!(
                "plugin command '{command}' needs live runtime state and is not supported in dry_run"
            ),
        ))
    }
}

/// Validates the batch against a scratch copy of the world. Commands that
/// need live runtime state are rejected through their descriptors before any
/// preview work; plugin commands declaring dry-run support execute against
/// the scratch engine.
fn batch_dry_run(session: &mut RuntimeSession, batch: &BatchArgs) -> Result<Value, EngineError> {
    for request in &batch.requests {
        if let Some(error) = dry_run_rejection(session, &request.command) {
            return Err(error);
        }
    }
    let mut scratch = Engine::new();
    scratch.load_world(session.world().clone()).map_err(|_| {
        EngineError::new("serialization_error", "could not clone the session world")
    })?;
    // The scratch engine must validate against the session's component
    // registry, not just the core seed, so dry-run attach_component calls
    // see dynamically registered components.
    *scratch.registry_mut() = session.engine().registry().clone();
    let mut results = Vec::with_capacity(batch.requests.len());
    for (index, request) in batch.requests.iter().enumerate() {
        // Handlers receive only the scratch engine, so world mutations are
        // previewed while host-managed runtime state stays untouched.
        let outcome = if session.host_mut().has_plugin_command(&request.command) {
            session.host_mut().execute_plugin_command_on(
                &request.command,
                &mut scratch,
                &request.args,
            )
        } else {
            execute_engine_command(&mut scratch, &request.command, &request.args)
        };
        match outcome {
            Ok(result) => results.push(result),
            Err(error) => {
                return Err(EngineError::new(
                    "batch_failed",
                    format!(
                        "dry run request {index} ('{}') failed: {}: {}",
                        request.command, error.code, error.message
                    ),
                )
                .with_details(json!({
                    "index": index,
                    "command": request.command,
                    "cause": { "code": error.code, "message": error.message },
                    "dryRun": true,
                })));
            }
        }
    }
    Ok(json!({
        "applied": results.len(),
        "dryRun": true,
        "results": results,
    }))
}

/// Executes one command inside a batch (or any top-level command the dispatch
/// match does not name): built-in world commands, `step`, and
/// plugin-registered commands. Resolution is by name FIRST: a registered
/// plugin command always executes and its structured error is returned
/// unchanged — failures are never masked by falling through to built-in
/// dispatch.
fn execute_subcommand(
    session: &mut RuntimeSession,
    command: &str,
    args: &Value,
) -> Result<Value, EngineError> {
    if command == "step" {
        return parse_args::<StepArgs>(args.clone()).and_then(|args| session.step(args.count));
    }
    if session.host_mut().has_plugin_command(command) {
        return session.host_mut().execute_plugin_command(command, args);
    }
    execute_engine_command(session.engine_mut(), command, args)
}

/// Top-level dispatch for one protocol request.
pub(crate) fn dispatch(
    session: &mut RuntimeSession,
    command: &str,
    args: Value,
) -> (Result<Value, EngineError>, bool) {
    let result = match command {
        "capabilities" => Ok(capabilities_for(session)),
        "batch" => batch(session, args),
        "step" => parse_args::<StepArgs>(args).and_then(|args| session.step(args.count)),
        "quit" => Ok(json!({ "quit": true })),
        _ => execute_subcommand(session, command, &args),
    };
    (result, command == "quit")
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
            "fbxModel": "Workspace-relative or res://<alias> .fbx/.mnec source; decoded mesh data, base color conversions, and GPU vertex buffers are shared by reference across entities with the same source, color, and asset revision — each entity keeps only its model matrix buffer",
            "assets": {
                "formats": ["fbx", "mnec"],
                "worker": "moni-asset-loader",
                "queueCapacity": 64,
                "timeoutSeconds": 10,
                "virtualPaths": "res://<alias> resolves through scene extensions.res so scenes keep stable asset identities",
                "hotReload": "opt-in via client/editor --hot-reload; mtime+size fingerprints requeue changed sources through the bounded loader while the previous revision keeps rendering",
                "runtimeReads": "nonblocking poll after initialization preload"
            },
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
        "commands": builtin_command_capabilities(),
        "transactions": {
            "revision": "Every response carries the world revision; mutations bump it and failed commands leave it unchanged",
            "expectedRevision": "Batch and single requests may pin a revision; mismatches fail with revision_conflict before any change",
            "requestId": "String or number request IDs are idempotent: replays with identical payload return the cached response; reuse with different payload fails with duplicate_request_id",
            "eventSeq": "Session-wide watermark of delivered events",
            "batchRollback": "Any failing request rolls back the whole batch: no partial modification, no leaked events. Commands declare batch and dry-run support through their descriptor metadata (see the per-command flags); non-batchable commands are rejected with invalid_batch_command, non-dry-runnable ones with dry_run_unsupported."
        },
        "components": crate::components::ComponentRegistry::new().agent_schemas(),
        "profiles": {
            "pluginApiVersion": 1,
            "bundles": ["minimal", "default", "headless", "editor"],
            "moduleDomains": ["runtime", "editor", "build", "developer", "test"],
            "patches": "ordered enable/disable by plugin ID",
            "pruning": "effective-tree and runtime instantiation only"
        },
        "host": {
            "scheduler": "single-thread dependency-ordered static plugins",
            "maxSystemTasks": 1024,
            "stages": ["startup", "fixed_update", "render", "shutdown"],
            "threadAffinity": {
                "core.simulation": "logic",
                "core.render": "render"
            }
        },
        "security": {
            "permissions": "manifest-granted and checked at host access points; native plugins are trusted code, permissions are for declaration, auditing, and dependency checks — not a sandbox",
            "updates": "permission diff with explicit acceptance",
            "integrity": "SHA-256 per plugin manifest; does not cover plugin sources, binaries, or asset packages",
            "importIsolation": "time-limited child process; crash containment only",
            "lockRollback": "moni.lock.backup"
        },
        "script": {
            "language": "TypeScript compiled to JavaScript",
            "runtime": "QuickJS",
            "contextLifetime": "persistent per scripted entity; source evaluated once; reset on replacement, world load, or error",
            "snapshot": "script source only; JavaScript heap is not persisted",
            "hook": "function onTick(ctx: ScriptContext, dt: number): void",
            "limits": {
                "timePerHookMilliseconds": 50,
                "budgetPerTickMilliseconds": 10,
                "memoryBytes": 33554432,
                "commandsPerHook": 1024,
                "sourceBytes": 262144
            },
            "definitions": "examples/engine-api.d.ts"
        },
        "events": [
            "entity_spawned",
            "entity_despawned",
            "entity_renamed",
            "transform_changed",
            "sprite_changed",
            "model_changed",
            "component_attached",
            "component_removed",
            "component_registered",
            "component_unregistered",
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

/// Builds one capabilities `commands` entry from a descriptor. The `args` and
/// `returns` keys keep their historical meaning; `returns` and `permission`
/// are omitted when the descriptor documents none.
fn command_capability_entry(descriptor: &CommandDescriptor) -> Value {
    let mut entry = json!({
        "args": descriptor.args_schema,
        "mutatesWorld": descriptor.mutates_world,
        "batch": descriptor.supports_batch,
        "dryRun": descriptor.supports_dry_run,
        "sideEffects": {
            "file": descriptor.side_effects.file,
            "gpu": descriptor.side_effects.gpu,
            "script": descriptor.side_effects.script,
            "network": descriptor.side_effects.network,
        },
        "owner": descriptor.owner,
    });
    if !descriptor.returns_schema.is_null() {
        entry["returns"] = descriptor.returns_schema.clone();
    }
    if let Some(permission) = &descriptor.required_permission {
        entry["permission"] = json!(permission);
    }
    entry
}

/// Generates the capabilities `commands` section from the built-in table.
fn builtin_command_capabilities() -> Value {
    builtin_commands()
        .iter()
        .map(|(name, descriptor)| (name.to_string(), command_capability_entry(descriptor)))
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

/// Session-aware capabilities: the static protocol description plus the
/// plugins, services, plugin-registered commands, component registry, and
/// counters this session actually has, so agents see the running environment
/// instead of a hardcoded maximum. The `commands` section merges the built-in
/// table with the session's plugin-registered commands, each entry carrying
/// its descriptor metadata.
pub fn capabilities_for(session: &mut RuntimeSession) -> Value {
    let mut capabilities = capabilities();
    let services: serde_json::Map<String, Value> = session
        .host_mut()
        .service_descriptions()
        .into_iter()
        .map(|(name, provider)| (name, json!({ "provider": provider })))
        .collect();
    let plugin_commands: serde_json::Map<String, Value> = session
        .host_mut()
        .plugin_command_descriptions()
        .into_iter()
        .map(|(name, descriptor)| (name, command_capability_entry(descriptor)))
        .collect();
    {
        let commands = capabilities["commands"]
            .as_object_mut()
            .expect("generated commands section is an object");
        for (name, entry) in plugin_commands.iter() {
            commands.insert(name.clone(), entry.clone());
        }
    }
    // The session engine owns the registry, so dynamically registered
    // components appear here automatically with their x-component metadata.
    capabilities["components"] = session.engine().registry().agent_schemas();
    capabilities["session"] = json!({
        "profile": session.profile_name(),
        "revision": session.revision(),
        "eventSeq": session.event_seq(),
        "plugins": session.host_mut().plugin_ids(),
        "services": services,
        "pluginCommands": plugin_commands,
    });
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{CORE_SIMULATION, Plugin};
    use crate::plugins::SimulationPlugin;
    use crate::profile::Profile;

    fn session() -> RuntimeSession {
        RuntimeSession::new(
            Engine::new(),
            Profile::named("minimal").expect("minimal profile"),
        )
    }

    fn line(id: &str, command: &str, args: Value) -> String {
        json!({ "id": id, "command": command, "args": args }).to_string()
    }

    fn spawn_args(name: &str) -> Value {
        json!({ "name": name })
    }

    fn batch_line(id: &str, command: &str, args: Value) -> String {
        json!({
            "id": id,
            "command": "batch",
            "args": { "requests": [{ "command": command, "args": args }] },
        })
        .to_string()
    }

    /// Script-host state (tick counters, JS heaps) is not captured by
    /// `EngineSnapshot`, so `step`/`reset`/`load_snapshot` must be refused
    /// up-front inside atomic batches: a later failure could otherwise roll
    /// back the world while the script state stays advanced.
    #[test]
    fn script_backed_commands_never_enter_atomic_batches() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin(
                Box::new(SimulationPlugin::new()),
                &["script.execute".to_string()],
            )
            .expect("simulation registers (not started)");
        let revision = session.revision();
        let tick = session.world().tick;
        let hosts = session.host_mut().script_host_count();

        for (index, command) in ["step", "reset", "load_snapshot"].iter().enumerate() {
            let args = match *command {
                "step" => json!({ "count": 1 }),
                "load_snapshot" => json!({ "snapshot": session.world() }),
                _ => json!({}),
            };
            let (response, quit) =
                session.handle_line(&batch_line(&format!("batch-{index}"), command, args));
            assert!(!quit);
            assert!(!response.ok, "'{command}' must be refused inside a batch");
            assert_eq!(
                response.error.expect("error body").code,
                "invalid_batch_command",
                "command '{command}'"
            );
        }

        assert_eq!(
            session.revision(),
            revision,
            "rejected batch keeps the revision"
        );
        assert_eq!(session.world().tick, tick, "rejected batch does not tick");
        assert_eq!(session.host_mut().script_host_count(), hosts);
        assert!(session.world().entities.is_empty(), "no request executed");
    }

    /// The capabilities entry must describe script side effects honestly so
    /// agents can see why these commands are not transactional.
    #[test]
    fn capabilities_mark_script_side_effects_and_batch_gates() {
        let mut session = session();
        let (response, _) = session.handle_line(
            &json!({ "id": "caps", "command": "capabilities", "args": {} }).to_string(),
        );
        assert!(response.ok);
        let caps = response.result.expect("capabilities");
        for command in ["step", "reset", "load_snapshot"] {
            let entry = &caps["commands"][command];
            assert_eq!(entry["batch"], json!(false), "'{command}' is not batchable");
            assert_eq!(
                entry["sideEffects"]["script"],
                json!(true),
                "'{command}' must declare script side effects"
            );
        }
    }

    /// Registered-but-not-started simulation must not silently produce a
    /// no-op tick result.
    #[test]
    fn step_on_registered_but_unstarted_simulation_fails() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin(
                Box::new(SimulationPlugin::new()) as Box<dyn Plugin>,
                &["script.execute".to_string()],
            )
            .expect("simulation registers");
        assert!(session.host_mut().has_plugin(CORE_SIMULATION));
        assert!(!session.host_mut().is_started(CORE_SIMULATION));

        let (response, _) = session.handle_line(
            &json!({ "id": "1", "command": "step", "args": { "count": 1 } }).to_string(),
        );
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("error body").code,
            "plugin_not_started"
        );
        assert_eq!(session.world().tick, 0, "no tick advanced");
        assert_eq!(session.revision(), 0);
    }

    /// Captures one entity as a full JSON snapshot through `query`, the way
    /// an editor or agent prepares an undo snapshot before despawning.
    fn query_entity(session: &mut RuntimeSession, entity: u64) -> Value {
        let (response, _) = session.handle_line(
            &json!({ "id": "probe", "command": "query", "args": { "entity": entity } }).to_string(),
        );
        assert!(response.ok, "{:?}", response.error);
        response.result.expect("entity json")
    }

    #[test]
    fn rename_entity_validates_and_restore_keeps_explicit_ids() {
        let mut session = session();
        session.handle_line(&line("1", "spawn", spawn_args("a")));
        for (index, name) in [String::new(), "x".repeat(129)].into_iter().enumerate() {
            let (response, _) = session.handle_line(&line(
                &format!("invalid-{index}"),
                "rename_entity",
                json!({ "entity": 1, "name": name }),
            ));
            assert!(!response.ok, "invalid names must fail");
            assert_eq!(response.error.expect("error body").code, "invalid_entity");
        }
        assert_eq!(session.revision(), 1, "failed renames keep the revision");
        assert!(
            session.world().entities[&1].name == "a",
            "no rename applied"
        );

        let (renamed, _) = session.handle_line(&line(
            "3",
            "rename_entity",
            json!({ "entity": 1, "name": "renamed" }),
        ));
        assert!(renamed.ok, "{:?}", renamed.error);
        assert!(renamed.events.iter().any(|event| matches!(
            event,
            crate::engine::Event::EntityRenamed { entity: 1, name } if name == "renamed"
        )));

        // Capture, despawn, restore: the explicit id and the data survive.
        let snapshot = query_entity(&mut session, 1);
        session.handle_line(&line("4", "despawn", json!({ "entity": 1 })));
        let (restored, _) =
            session.handle_line(&line("5", "restore_entity", json!({ "entity": snapshot })));
        assert!(restored.ok, "{:?}", restored.error);
        assert_eq!(
            restored.result.expect("result")["entity"],
            json!(1),
            "the entity comes back under its explicit id"
        );
        assert_eq!(
            query_entity(&mut session, 1),
            snapshot,
            "despawn -> restore round trip is lossless"
        );

        // Restoring onto a live id is refused without side effects.
        let revision = session.revision();
        let (exists, _) =
            session.handle_line(&line("6", "restore_entity", json!({ "entity": snapshot })));
        assert!(!exists.ok);
        assert_eq!(exists.error.expect("error body").code, "entity_exists");
        assert_eq!(session.revision(), revision);
    }

    #[test]
    fn rename_and_restore_are_batchable_pure_data_commands() {
        let mut session = session();
        let (response, _) = session.handle_line(&line("0", "capabilities", json!({})));
        let capabilities = response.result.expect("capabilities");
        for command in ["rename_entity", "restore_entity"] {
            let entry = &capabilities["commands"][command];
            assert_eq!(entry["mutatesWorld"], json!(true), "{command}");
            assert_eq!(entry["batch"], json!(true), "{command}");
            assert_eq!(entry["dryRun"], json!(true), "{command}");
            assert_eq!(entry["owner"], json!("core"), "{command}");
            assert_eq!(entry["sideEffects"]["file"], json!(false), "{command}");
            assert_eq!(entry["sideEffects"]["script"], json!(false), "{command}");
        }
        let events = capabilities["events"].as_array().expect("events");
        assert!(events.contains(&json!("entity_renamed")));

        // restore_entity previews against the scratch world in dry-run
        // batches and participates in atomic batches like any pure data op.
        session.handle_line(&line("1", "spawn", spawn_args("a")));
        let snapshot = query_entity(&mut session, 1);
        session.handle_line(&line("2", "despawn", json!({ "entity": 1 })));
        let (dry, _) = session.handle_line(
            &json!({
                "id": "3",
                "command": "batch",
                "args": {
                    "dryRun": true,
                    "requests": [
                        { "command": "restore_entity", "args": { "entity": snapshot } },
                        { "command": "rename_entity", "args": { "entity": 1, "name": "preview" } },
                    ]
                }
            })
            .to_string(),
        );
        assert!(dry.ok, "dry run validates restore: {:?}", dry.error);
        assert!(
            session.world().entities.is_empty(),
            "the dry run must not mutate"
        );

        let (applied, _) = session.handle_line(&batch_line(
            "4",
            "restore_entity",
            json!({ "entity": snapshot }),
        ));
        assert!(applied.ok, "{:?}", applied.error);
        assert_eq!(session.world().entities[&1].name, "a");
        let (renamed, _) = session.handle_line(&batch_line(
            "5",
            "rename_entity",
            json!({ "entity": 1, "name": "batched" }),
        ));
        assert!(renamed.ok, "{:?}", renamed.error);
        assert_eq!(session.world().entities[&1].name, "batched");
    }
}
