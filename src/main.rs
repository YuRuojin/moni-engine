mod benchmark;
mod client;
mod components;
mod editor;
mod editor_ui;
mod engine;
mod fbx;
mod gpu;
mod host;
mod plugins;
mod profile;
mod protocol;
mod render;
mod scene;
mod scene3d;
mod script;

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as AnyhowContext, bail};
use engine::Engine;
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
        Some("headless") => headless(args.collect()),
        Some("profile") => profile_command(args.collect()),
        Some("client") => client::run(args.collect()),
        Some("editor") => editor::run(args.collect()),
        Some("scene") => scene_roundtrip(args.collect()),
        Some("benchmark") => benchmark::run(args.collect()),
        Some("demo") => demo(args.collect()),
        Some(command) => bail!("unknown command '{command}'"),
    }
}

fn print_help() -> anyhow::Result<()> {
    println!(
        "moni-engine\n\nCommands:\n  capabilities              Print the machine-readable control schema\n  serve                     Serve NDJSON requests on stdin/stdout\n  headless [--profile NAME] [--scene PATH]\n                            Resolve and inspect a headless profile\n  profile [list|dump] [--profile NAME] [--patch ID=on|off]\n                            Inspect profiles and effective plugin trees\n  client [--scene 2d|3d] [--script PATH] [--frames N]\n         [--benchmark --warmup N --output PATH]\n                            Open the winit/wgpu client\n  editor [--scene PATH] [--frames N]\n                            Open the read-only .mnsc scene viewer\n  scene PATH [--output PATH] [--name NAME]\n                            Migrate and round-trip a .mnsc scene\n  benchmark [--scene 2d|3d] [--script PATH] [--frames N]\n            [--warmup N] [--output PATH]\n                            Measure release simulation ticks\n  demo [--ticks N] [--output PATH] [--snapshot PATH] [--script PATH]\n\nScript workflow:\n  npm install\n  npm run build\n  cargo run -- client\n  cargo run -- client --scene 3d\n  npm run benchmark"
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

fn headless(args: Vec<String>) -> anyhow::Result<()> {
    let mut profile_name = "headless".to_string();
    let mut scene_path = "testing/assets/perspective.mnsc".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                index += 1;
                profile_name = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--profile requires a name"))?
                    .clone();
            }
            "--scene" => {
                index += 1;
                scene_path = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scene requires a path"))?
                    .clone();
            }
            unknown => bail!("unknown headless option '{unknown}'"),
        }
        index += 1;
    }
    let world = scene::load_scene(&workspace_path(&scene_path)?)?;
    let mut engine = Engine::new();
    engine.load_world(world)?;
    let mut host = host::PluginHost::new(engine);
    let profile = profile::Profile::named(&profile_name)?;
    let effective = profile.resolve(&profile::Target::current())?;
    profile.instantiate(&mut host, None, false)?;
    host.run_stage(host::SystemStage::Startup)?;
    let gpu_initialized = host.has_plugin(plugins::RenderPlugin::ID);
    let quickjs_initialized =
        host.has_plugin(plugins::SimulationPlugin::ID) || host.script_host_count() != 0;
    if gpu_initialized || quickjs_initialized {
        bail!("headless profile initialized a forbidden runtime provider");
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "profile": profile_name,
            "entities": host.world().entities.len(),
            "gpuInitialized": gpu_initialized,
            "quickjsInitialized": quickjs_initialized,
            "fbxLoaded": false,
            "loadedMeshVertices": 0,
            "effectiveProfile": effective.dump(),
        }))?
    );
    Ok(())
}

fn profile_command(args: Vec<String>) -> anyhow::Result<()> {
    let mut action = "dump";
    let mut profile_name = "default".to_string();
    let mut target = profile::Target::current();
    let mut patches = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "list" | "dump" => action = args[index].as_str(),
            "--profile" => {
                index += 1;
                profile_name = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--profile requires a name"))?
                    .clone();
            }
            "--patch" => {
                index += 1;
                patches.push(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--patch requires ID=on|off"))?
                        .clone(),
                );
            }
            "--os" => {
                index += 1;
                target.os = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--os requires a value"))?
                    .clone();
            }
            "--arch" => {
                index += 1;
                target.arch = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--arch requires a value"))?
                    .clone();
            }
            "--config" => {
                index += 1;
                target.configuration = match args.get(index).map(String::as_str) {
                    Some("debug") => profile::BuildConfiguration::Debug,
                    Some("release") => profile::BuildConfiguration::Release,
                    Some("shipping") => profile::BuildConfiguration::Shipping,
                    Some("test") => profile::BuildConfiguration::Test,
                    _ => bail!("--config must be debug, release, shipping, or test"),
                };
            }
            unknown => bail!("unknown profile option '{unknown}'"),
        }
        index += 1;
    }
    if action == "list" {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "profiles": ["minimal", "default", "headless", "editor"],
                "bundles": {
                    "minimal": [],
                    "default": ["core.simulation", "core.render", "core.fbx"],
                    "headless": [],
                    "editor": ["core.render", "core.fbx", "core.editor"]
                }
            }))?
        );
        return Ok(());
    }
    let mut profile = profile::Profile::named(&profile_name)?;
    for patch in patches {
        let (id, enabled) = patch
            .rsplit_once('=')
            .ok_or_else(|| anyhow::anyhow!("--patch must use ID=on|off"))?;
        let enabled = match enabled {
            "on" => true,
            "off" => false,
            _ => bail!("--patch must use ID=on|off"),
        };
        profile = profile.with_patch(id, enabled);
    }
    let effective = profile.resolve(&target)?;
    println!("{}", serde_json::to_string_pretty(&effective.dump())?);
    Ok(())
}

fn scene_roundtrip(args: Vec<String>) -> anyhow::Result<()> {
    let mut input = None;
    let mut output = "target/scene-roundtrip.mnsc".to_string();
    let mut name = "roundtrip".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                index += 1;
                output = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?
                    .clone();
            }
            "--name" => {
                index += 1;
                name = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--name requires a value"))?
                    .clone();
            }
            value if !value.starts_with('-') && input.is_none() => input = Some(value.to_string()),
            unknown => bail!("unknown scene option '{unknown}'"),
        }
        index += 1;
    }
    let input = input.ok_or_else(|| anyhow::anyhow!("scene requires a .mnsc path"))?;
    let input_path = workspace_path(&input)?;
    let world = scene::load_scene(&input_path)?;
    let mut engine = Engine::new();
    engine.load_world(world)?;
    let output_path = workspace_path(&output)?;
    scene::save_scene(engine.world(), &name, &output_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "input": input,
            "output": output,
            "sceneVersion": scene::SCENE_VERSION,
            "entities": engine.world().entities.len(),
        }))?
    );
    Ok(())
}

fn demo(args: Vec<String>) -> anyhow::Result<()> {
    let mut ticks = 120_u64;
    let mut output = "target/demo.svg".to_string();
    let mut snapshot = "target/demo-snapshot.json".to_string();
    let mut script = "examples/dist/model-orbit.js".to_string();

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

pub(crate) fn demo_engine(source: String) -> anyhow::Result<Engine> {
    external_model_engine(source, "testing/assets/orthographic.mnsc")
}

pub(crate) fn demo_engine_3d(source: String) -> anyhow::Result<Engine> {
    external_model_engine(source, "testing/assets/perspective.mnsc")
}

fn external_model_engine(_source: String, scene_path: &str) -> anyhow::Result<Engine> {
    let path = workspace_path(scene_path)?;
    let world = scene::load_scene(&path)?;
    let mut engine = Engine::new();
    engine.load_world(world)?;
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
