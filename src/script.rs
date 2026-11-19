use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime};

use crate::engine::{Entity, Sprite};

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

pub struct ScriptHost;

impl ScriptHost {
    pub fn run_tick(
        entity: &Entity,
        source: &str,
        tick: u64,
        dt: f64,
    ) -> Result<HookOutput, ScriptFailure> {
        let started = Instant::now();
        let deadline = started + SCRIPT_TIME_LIMIT;
        let runtime =
            Runtime::new().map_err(|error| failure("runtime_error", error.to_string()))?;
        runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT);
        runtime.set_max_stack_size(SCRIPT_STACK_LIMIT);
        runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));

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
        if command_limit_hit.get() {
            return Err(failure(
                "script_command_limit",
                format!(
                    "script emitted more than {} commands",
                    MAX_COMMANDS_PER_TICK
                ),
            ));
        }
        if log_limit_hit.get() {
            logs.borrow_mut().push(format!(
                "log output was truncated after {} entries",
                MAX_LOGS_PER_TICK
            ));
        }
        Ok(HookOutput {
            commands: commands.borrow().clone(),
            logs: logs.borrow().clone(),
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
    Ok(value)
}

fn sprite_to_js<'js>(ctx: &rquickjs::Ctx<'js>, sprite: &Sprite) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set(
        "type",
        match sprite.kind {
            crate::engine::SpriteKind::Rectangle => "rectangle",
            crate::engine::SpriteKind::Circle => "circle",
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
