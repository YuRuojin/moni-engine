#[path = "../plugins/assets/runtime/mod.rs"]
mod assets;
mod benchmark;
mod client;
mod components;
#[path = "../plugins/assets/cooked/mod.rs"]
mod cooked;
#[path = "../plugins/editor/scene-viewer/mod.rs"]
mod editor;
#[path = "../plugins/editor/scene-viewer/ui.rs"]
mod editor_ui;
mod engine;
#[path = "../plugins/assets/fbx/mod.rs"]
mod fbx;
#[path = "../plugins/rendering/wgpu/mod.rs"]
mod gpu;
mod host;
mod plugins;
mod profile;
mod project;
mod protocol;
mod render;
mod scene;
mod scene3d;
#[path = "../plugins/scripting/quickjs/mod.rs"]
mod script;

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};

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
        Some("cook") => cook_mesh(args.collect()),
        Some("new") => project_new(args.collect()),
        Some("run") => project_run(args.collect()),
        Some("doctor") => project_doctor(args.collect()),
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
        "moni-engine\n\nCommands:\n  cook INPUT [--output PATH] [--isolate]
                            Convert FBX to a cooked .mnec mesh
  new PATH                  Create an independent game repository\n  run [--frames N]          Build and run the project in the current directory\n  doctor [--update-lock] [--rollback-lock] [--accept-permissions]
                            Validate project, scene, profile, and lockfile\n  capabilities              Print the machine-readable control schema\n  serve                     Serve NDJSON requests on stdin/stdout\n  headless [--profile NAME] [--scene PATH]\n                            Resolve and inspect a headless profile\n  profile [list|dump] [--profile NAME] [--patch ID=on|off]\n                            Inspect profiles and effective plugin trees\n  client [--scene 2d|3d] [--script PATH] [--frames N]\n         [--benchmark --warmup N --output PATH]\n                            Open the winit/wgpu client\n  editor [--scene PATH] [--frames N]\n                            Open the read-only .mnsc scene viewer\n  scene PATH [--output PATH] [--name NAME]\n                            Migrate and round-trip a .mnsc scene\n  benchmark [--scene 2d|3d] [--script PATH] [--frames N]\n            [--warmup N] [--output PATH]\n                            Measure release simulation ticks\n  demo [--ticks N] [--output PATH] [--snapshot PATH] [--script PATH]\n\nScript workflow:\n  npm install\n  npm run build\n  cargo run -- client\n  cargo run -- client --scene 3d\n  npm run benchmark"
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

fn cook_mesh(args: Vec<String>) -> anyhow::Result<()> {
    let mut input = None;
    let mut output = None;
    let mut isolate = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--isolate" => isolate = true,
            "--output" => {
                index += 1;
                output = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?
                        .clone(),
                );
            }
            value if !value.starts_with('-') && input.is_none() => input = Some(value.to_string()),
            unknown => bail!("unknown cook option '{unknown}'"),
        }
        index += 1;
    }
    let input = input.ok_or_else(|| anyhow::anyhow!("cook requires an FBX path"))?;
    let output = output.unwrap_or_else(|| input.replace(".fbx", ".mnec"));
    if isolate {
        return cook_mesh_isolated(&input, &output);
    }
    let mesh = fbx::load_fbx(&workspace_path(&input)?)?;
    cooked::write_cooked_mesh(&workspace_path(&output)?, &mesh)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "input": input,
            "output": output,
            "vertices": mesh.vertices.len(),
        }))?
    );
    Ok(())
}

fn cook_mesh_isolated(input: &str, output: &str) -> anyhow::Result<()> {
    workspace_path(input)?;
    workspace_path(output)?;
    let executable = env::current_exe()?;
    let mut child = ProcessCommand::new(executable)
        .args(["cook", input, "--output", output])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start isolated FBX importer")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("isolated FBX importer failed with status {status}");
            }
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("isolated FBX importer timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "input": input,
            "output": output,
            "isolated": true,
        }))?
    );
    Ok(())
}

fn project_new(args: Vec<String>) -> anyhow::Result<()> {
    if args.len() != 1 || args[0].starts_with('-') {
        bail!("new requires exactly one project path");
    }
    let raw = PathBuf::from(&args[0]);
    let root = if raw.is_absolute() {
        raw
    } else {
        env::current_dir()?.join(raw)
    };
    if let Err(error) = initialize_project(&root) {
        if root.exists() {
            fs::remove_dir_all(&root)
                .with_context(|| format!("could not clean failed project {}", root.display()))?;
        }
        return Err(error);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "project": root.display().to_string(),
            "files": ["game.mngp", "moni.lock", "scenes/main.mnsc", "scripts/main.ts"],
            "gitInitialized": true,
        }))?
    );
    Ok(())
}

fn initialize_project(root: &Path) -> anyhow::Result<()> {
    let template = template_directory()?;
    project::copy_template(&template, root)?;
    let mut project = project::GameProject::load(root)?;
    project.name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("game")
        .to_string();
    fs::write(
        root.join("game.mngp"),
        serde_json::to_string_pretty(&project)? + "\n",
    )?;
    project.write_lockfile(root)?;
    let status = ProcessCommand::new("git")
        .arg("init")
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("git init failed in {}", root.display());
    }
    Ok(())
}

fn template_directory() -> anyhow::Result<PathBuf> {
    if let Ok(executable) = env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let packaged = parent.join("templates/minimal-game");
        if packaged.is_dir() {
            return Ok(packaged);
        }
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates/minimal-game");
    if source.is_dir() {
        Ok(source)
    } else {
        bail!("minimal-game template is not installed")
    }
}

fn project_run(args: Vec<String>) -> anyhow::Result<()> {
    let mut frames = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--frames" => {
                index += 1;
                frames = args.get(index).cloned();
            }
            unknown => bail!("unknown run option '{unknown}'"),
        }
        index += 1;
    }
    let root = env::current_dir()?;
    let project = project::GameProject::load(&root)?;
    project.check_lockfile(&root)?;
    if !root.join("node_modules/typescript").is_dir() {
        run_project_command(&root, "npm", &["ci", "--ignore-scripts"])?;
    }
    run_project_command(&root, "npm", &["run", "build"])?;
    let scene = project.scene_path(&root)?;
    let mut client_args = vec![
        "--world".to_string(),
        scene.to_string_lossy().into_owned(),
        "--profile".to_string(),
        project.profile.clone(),
    ];
    if let Some(frames) = frames {
        client_args.push("--frames".to_string());
        client_args.push(frames);
    }
    client::run_with_profile(client_args, project.profile()?)
}

fn project_doctor(args: Vec<String>) -> anyhow::Result<()> {
    let mut update_lock = false;
    let mut rollback_lock = false;
    let mut accept_permissions = false;
    for arg in args {
        match arg.as_str() {
            "--update-lock" => update_lock = true,
            "--rollback-lock" => rollback_lock = true,
            "--accept-permissions" => accept_permissions = true,
            _ => bail!("unknown doctor option '{arg}'"),
        }
    }
    let root = env::current_dir()?;
    let project = project::GameProject::load(&root)?;
    let permission_changes = project.permission_diff(&root)?;
    if rollback_lock {
        project.rollback_lockfile(&root)?;
    } else if update_lock {
        let has_added_permissions = permission_changes["added"]
            .as_object()
            .is_some_and(|added| !added.is_empty());
        if has_added_permissions && !accept_permissions {
            bail!("permission update requires explicit --accept-permissions");
        }
        project.write_lockfile(&root)?;
    } else {
        project.check_lockfile(&root)?;
    }
    let scene_path = project.scene_path(&root)?;
    let world = scene::load_scene(&scene_path)?;
    let effective = project.profile()?.resolve(&profile::Target::current())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "project": project.name,
            "engine": project.engine,
            "entities": world.entities.len(),
            "plugins": effective.manifests().count(),
            "lockUpdated": update_lock,
            "lockRolledBack": rollback_lock,
            "permissionChanges": permission_changes,
        }))?
    );
    Ok(())
}

fn run_project_command(root: &Path, command: &str, args: &[&str]) -> anyhow::Result<()> {
    let executable = if cfg!(windows) && command == "npm" {
        "npm.cmd"
    } else {
        command
    };
    let status = ProcessCommand::new(executable)
        .args(args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        bail!("{command} {} failed in {}", args.join(" "), root.display());
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
    let mut profile_name = None;
    let mut target = profile::Target::current();
    let mut patches = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "list" | "dump" => action = args[index].as_str(),
            "--profile" => {
                index += 1;
                profile_name = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--profile requires a name"))?
                        .clone(),
                );
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
    let mut profile = if let Some(name) = profile_name {
        profile::Profile::named(&name)?
    } else if env::current_dir()?.join("game.mngp").is_file() {
        project::GameProject::load(&env::current_dir()?)?.profile()?
    } else {
        profile::Profile::named("default")?
    };
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
