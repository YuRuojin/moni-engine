mod client;
mod engine;
mod gpu;
mod protocol;
mod render;
mod script;

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as AnyhowContext, bail};
use engine::{Command, Engine, EntitySpec, ScriptSpec, Sprite, SpriteKind, Transform};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                json!({
                    "ok": false,
                    "error": {
                        "code": "cli_error",
                        "message": format!("{error:#}")
                    }
                })
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("help") | Some("--help") | Some("-h") => print_help(),
        Some("capabilities") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&protocol::capabilities())?
            );
            Ok(())
        }
        Some("serve") => serve(),
        Some("client") => client::run(args.collect()),
        Some("demo") => demo(args.collect()),
        Some(command) => bail!("unknown command '{command}'"),
    }
}

fn print_help() -> anyhow::Result<()> {
    println!(
        "moni-engine\n\nCommands:\n  capabilities              Print the machine-readable control schema\n  serve                     Serve NDJSON requests on stdin/stdout\n  client [--script PATH] [--frames N]\n                            Open the winit/wgpu client\n  demo [--ticks N] [--output PATH] [--snapshot PATH] [--script PATH]\n\nScript workflow:\n  npm install\n  npm run build\n  cargo run -- client"
    );
    Ok(())
}

fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut engine = Engine::new();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (response, quit) = protocol::handle_line(&mut engine, &line);
        writeln!(output, "{}", serde_json::to_string(&response)?)?;
        output.flush()?;
        if quit {
            break;
        }
    }
    Ok(())
}

fn demo(args: Vec<String>) -> anyhow::Result<()> {
    let mut ticks = 120_u64;
    let mut output = "target/demo.svg".to_string();
    let mut snapshot = "target/demo-snapshot.json".to_string();
    let mut script = "examples/dist/player.js".to_string();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--ticks" => {
                index += 1;
                ticks = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--ticks requires a value"))?
                    .parse()
                    .context("--ticks must be a positive integer")?;
            }
            "--output" => {
                index += 1;
                output = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?
                    .clone();
            }
            "--snapshot" => {
                index += 1;
                snapshot = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--snapshot requires a path"))?
                    .clone();
            }
            "--script" => {
                index += 1;
                script = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--script requires a path"))?
                    .clone();
            }
            unknown => bail!("unknown demo option '{unknown}'"),
        };
        index += 1;
    }
    if ticks == 0 || ticks > engine::MAX_STEP_COUNT {
        bail!("--ticks must be between 1 and {}", engine::MAX_STEP_COUNT);
    }

    let script_path = workspace_path(&script)?;
    let source = fs::read_to_string(&script_path).with_context(|| {
        format!(
            "could not read {}; run 'npm install && npm run build' first",
            script_path.display()
        )
    })?;

    let mut engine = demo_engine(source)?;
    let step_result = engine.step(ticks)?;
    let svg = render::render_svg(engine.world());

    let output_path = workspace_path(&output)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, svg)?;

    let snapshot_path = workspace_path(&snapshot)?;
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &snapshot_path,
        serde_json::to_string_pretty(engine.world())?,
    )?;

    let events = engine.take_events();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "player": { "entity": 1 },
            "simulation": step_result,
            "svg": output,
            "snapshot": snapshot,
            "events": events.len()
        }))?
    );
    Ok(())
}

pub(crate) fn demo_engine(source: String) -> Result<Engine, engine::EngineError> {
    let mut engine = Engine::new();
    engine.apply(Command::Spawn {
        spec: EntitySpec {
            name: Some("player".to_string()),
            transform: Transform {
                x: 120.0,
                y: 180.0,
                rotation: 0.0,
            },
            sprite: Some(Sprite {
                kind: SpriteKind::Rectangle,
                width: 42.0,
                height: 42.0,
                color: "#4f8cff".to_string(),
                layer: 1,
            }),
            script: Some(ScriptSpec { source }),
        },
    })?;
    engine.apply(Command::Spawn {
        spec: EntitySpec {
            name: Some("sun".to_string()),
            transform: Transform {
                x: 500.0,
                y: 90.0,
                rotation: 0.0,
            },
            sprite: Some(Sprite {
                kind: SpriteKind::Circle,
                width: 72.0,
                height: 72.0,
                color: "#ffcf5a".to_string(),
                layer: 0,
            }),
            script: None,
        },
    })?;
    Ok(engine)
}

fn workspace_path(raw_path: &str) -> anyhow::Result<PathBuf> {
    if raw_path.trim().is_empty() {
        bail!("path must not be empty");
    }
    let path = Path::new(raw_path);
    let has_forbidden_component = path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    });
    if path.is_absolute() || has_forbidden_component {
        bail!("paths must stay inside the current workspace");
    }
    Ok(env::current_dir()?.join(path))
}
