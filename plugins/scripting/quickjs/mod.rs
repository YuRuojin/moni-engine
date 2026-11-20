pub mod runtime;

pub use runtime::QuickJsScriptRuntime;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime};

use moni_core::engine::{Entity, Sprite};
use moni_core::scene3d::{Model3d, Transform3d, Vector3};

const SCRIPT_TIME_LIMIT: Duration = Duration::from_millis(50);
const SCRIPT_MEMORY_LIMIT: usize = 32 * 1024 * 1024;
const SCRIPT_STACK_LIMIT: usize = 256 * 1024;
const MAX_COMMANDS_PER_TICK: usize = 1_024;
const MAX_LOGS_PER_TICK: usize = 128;
const MAX_COMMAND_CHARS: usize = 64 * 1024;
const MAX_LOG_CHARS: usize = 1_024;

const PRELUDE: &str = r#"
(function () {
  const unavailable = function () {
    throw new Error("Date and Math.random are disabled; use deterministic engine data");
  };
  try {
    Math.random = unavailable;
    globalThis.Date = undefined;
  } catch (_) {}

  globalThis.__moni_create_context = function (entity, tick) {
    const send = function (command) {
      globalThis.__moni_command(JSON.stringify(command));
    };
    return Object.freeze({
      entity: entity,
      tick: tick,
      setTransform: function (transform) {
        send({ op: "set_transform", entity: entity.id, transform: transform });
      },
      setSprite: function (sprite) {
        send({ op: "set_sprite", entity: entity.id, sprite: sprite });
      },
      setModel: function (model) {
        send({ op: "set_model", entity: entity.id, model: model });
      },
      setModelTransform: function (transform) {
        send({ op: "set_model_transform", entity: entity.id, transform: transform });
      },
      setCamera: function (camera) {
        send({ op: "set_camera", camera: camera });
      },
      spawn: function (spec) {
        send({ op: "spawn", spec: spec || {} });
      },
      despawn: function (entityId) {
        send({ op: "despawn", entity: entityId === undefined ? entity.id : entityId });
      },
      log: function (message) {
        globalThis.__moni_log(String(message));
      }
    });
  };

  globalThis.console = Object.freeze({
    log: function (message) {
      globalThis.__moni_log(String(message));
    }
  });
})();
"#;

#[derive(Debug)]
pub struct ScriptFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct HookOutput {
    pub commands: Vec<String>,
    pub logs: Vec<String>,
}

pub struct ScriptHost {
    context: Context,
    _runtime: Runtime,
    commands: Rc<RefCell<Vec<String>>>,
    logs: Rc<RefCell<Vec<String>>>,
    command_limit_hit: Rc<Cell<bool>>,
    log_limit_hit: Rc<Cell<bool>>,
    deadline: Rc<Cell<Instant>>,
    logic_thread: std::thread::ThreadId,
}

impl std::fmt::Debug for ScriptHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScriptHost")
    }
}

impl ScriptHost {
    pub fn new(source: &str) -> Result<Self, ScriptFailure> {
        let started = Instant::now();
        let deadline = Rc::new(Cell::new(started + SCRIPT_TIME_LIMIT));
        let runtime =
            Runtime::new().map_err(|error| failure("runtime_error", error.to_string()))?;
        runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT);
        runtime.set_max_stack_size(SCRIPT_STACK_LIMIT);
        runtime.set_interrupt_handler({
            let deadline = Rc::clone(&deadline);
            Some(Box::new(move || Instant::now() >= deadline.get()))
        });

        let commands = Rc::new(RefCell::new(Vec::<String>::new()));
        let logs = Rc::new(RefCell::new(Vec::<String>::new()));
        let command_limit_hit = Rc::new(Cell::new(false));
        let log_limit_hit = Rc::new(Cell::new(false));
        let context =
            Context::full(&runtime).map_err(|error| failure("runtime_error", error.to_string()))?;
        let result = context.with(|ctx| {
            let command_sink = {
                let commands = Rc::clone(&commands);
                let command_limit_hit = Rc::clone(&command_limit_hit);
                Function::new(ctx.clone(), move |raw: String| {
                    let mut commands = commands.borrow_mut();
                    if commands.len() >= MAX_COMMANDS_PER_TICK {
                        command_limit_hit.set(true);
                    } else {
                        commands.push(truncate(raw, MAX_COMMAND_CHARS));
                    }
                    rquickjs::Result::Ok(())
                })
            }?;
            let log_sink = {
                let logs = Rc::clone(&logs);
                let log_limit_hit = Rc::clone(&log_limit_hit);
                Function::new(ctx.clone(), move |message: String| {
                    let mut logs = logs.borrow_mut();
                    if logs.len() >= MAX_LOGS_PER_TICK {
                        log_limit_hit.set(true);
                    } else {
                        logs.push(truncate(message, MAX_LOG_CHARS));
                    }
                    rquickjs::Result::Ok(())
                })
            }?;
            ctx.globals().set("__moni_command", command_sink)?;
            ctx.globals().set("__moni_log", log_sink)?;
            ctx.eval::<(), _>(PRELUDE)?;
            ctx.eval::<(), _>(source)?;
            rquickjs::Result::Ok(())
        });
        if let Err(error) = result {
            let code = if started.elapsed() >= SCRIPT_TIME_LIMIT {
                "script_timeout"
            } else {
                "script_error"
            };
            return Err(failure(code, error.to_string()));
        }

        Ok(Self {
            context,
            _runtime: runtime,
            commands,
            logs,
            command_limit_hit,
            log_limit_hit,
            deadline,
            logic_thread: std::thread::current().id(),
        })
    }

    /// QuickJS runtimes are pinned to the logic thread. `Rc` fields already
    /// make `ScriptHost` `!Send`; this assertion keeps that contract explicit
    /// for code paths that stay on one thread by construction.
    pub fn assert_logic_thread(&self) {
        assert_eq!(
            std::thread::current().id(),
            self.logic_thread,
            "ScriptHost must stay on the logic thread that created it"
        );
    }

    pub fn run_tick(
        &mut self,
        entity: &Entity,
        tick: u64,
        dt: f64,
    ) -> Result<HookOutput, ScriptFailure> {
        self.commands.borrow_mut().clear();
        self.logs.borrow_mut().clear();
        self.command_limit_hit.set(false);
        self.log_limit_hit.set(false);
        let started = Instant::now();
        self.deadline.set(started + SCRIPT_TIME_LIMIT);
        let result = self.context.with(|ctx| {
            let entity_view = entity_to_js(&ctx, entity)?;
            let create_context: Function = ctx.globals().get("__moni_create_context")?;
            let script_context: Object = create_context.call((entity_view, tick as f64))?;
            let on_tick: Function = ctx.globals().get("onTick")?;
            on_tick.call::<_, ()>((script_context, dt))?;
            rquickjs::Result::Ok(())
        });

        if let Err(error) = result {
            let code = if started.elapsed() >= SCRIPT_TIME_LIMIT {
                "script_timeout"
            } else {
                "script_error"
            };
            return Err(failure(code, error.to_string()));
        }
        if self.command_limit_hit.get() {
            return Err(failure(
                "script_command_limit",
                format!(
                    "script emitted more than {} commands",
                    MAX_COMMANDS_PER_TICK
                ),
            ));
        }
        if self.log_limit_hit.get() {
            self.logs.borrow_mut().push(format!(
                "log output was truncated after {} entries",
                MAX_LOGS_PER_TICK
            ));
        }
        Ok(HookOutput {
            commands: self.commands.borrow().clone(),
            logs: self.logs.borrow().clone(),
        })
    }
}

fn entity_to_js<'js>(ctx: &rquickjs::Ctx<'js>, entity: &Entity) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set("id", entity.id as f64)?;
    value.set("name", entity.name.as_str())?;

    let transform = Object::new(ctx.clone())?;
    transform.set("x", entity.transform.x)?;
    transform.set("y", entity.transform.y)?;
    transform.set("rotation", entity.transform.rotation)?;
    value.set("transform", transform)?;

    if let Some(sprite) = &entity.sprite {
        value.set("sprite", sprite_to_js(ctx, sprite)?)?;
    }
    if let Some(model) = &entity.model {
        value.set("model", model_to_js(ctx, model)?)?;
    }
    Ok(value)
}

fn model_to_js<'js>(ctx: &rquickjs::Ctx<'js>, model: &Model3d) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set(
        "type",
        match model.kind {
            moni_core::scene3d::MeshKind::Cube => "cube",
            moni_core::scene3d::MeshKind::Plane => "plane",
            moni_core::scene3d::MeshKind::Fbx => "fbx",
        },
    )?;
    if let Some(source) = &model.source {
        value.set("source", source.as_str())?;
    }
    value.set("transform", transform_3d_to_js(ctx, &model.transform)?)?;
    value.set("color", model.color.as_str())?;
    value.set("metallic", model.metallic)?;
    value.set("roughness", model.roughness)?;
    Ok(value)
}

fn transform_3d_to_js<'js>(
    ctx: &rquickjs::Ctx<'js>,
    transform: &Transform3d,
) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set("position", vector3_to_js(ctx, transform.position)?)?;
    value.set("rotation", vector3_to_js(ctx, transform.rotation)?)?;
    value.set("scale", vector3_to_js(ctx, transform.scale)?)?;
    Ok(value)
}

fn vector3_to_js<'js>(ctx: &rquickjs::Ctx<'js>, vector: Vector3) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set("x", vector.x)?;
    value.set("y", vector.y)?;
    value.set("z", vector.z)?;
    Ok(value)
}

fn sprite_to_js<'js>(ctx: &rquickjs::Ctx<'js>, sprite: &Sprite) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set(
        "type",
        match sprite.kind {
            moni_core::engine::SpriteKind::Rectangle => "rectangle",
            moni_core::engine::SpriteKind::Circle => "circle",
        },
    )?;
    value.set("width", sprite.width)?;
    value.set("height", sprite.height)?;
    value.set("color", sprite.color.as_str())?;
    value.set("layer", sprite.layer)?;
    Ok(value)
}

fn truncate(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn failure(code: &str, message: String) -> ScriptFailure {
    ScriptFailure {
        code: code.to_string(),
        message,
    }
}
