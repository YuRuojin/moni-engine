mod benchmark;
mod bootstrap;
mod client;
mod fuzz;

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::sync::Arc;

use anyhow::{Context as AnyhowContext, bail};
use moni_assets::{cooked, fbx};
use moni_core::engine::Engine;
use moni_core::host::{CORE_RENDER, CORE_SIMULATION};
use moni_core::profile::{self, Profile};
use moni_core::{project, protocol, scene, workspace_path};
use moni_script::QuickJsScriptRuntime;
use serde_json::{Value, json};

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
        Some("fuzz") => fuzz::run(args.collect()),
        Some("profile") => profile_command(args.collect()),
        Some("client") => client::run(args.collect()),
        Some("editor") => editor_command(),
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
                            Validate project, scene, profile, and lockfile\n  fuzz [--target cooked|scene|fbx] [--iterations N] [--seed S]
                            Structured in-process parser fuzzing
  capabilities              Print the machine-readable control schema\n  serve                     Serve NDJSON requests on stdin/stdout\n  headless [--profile NAME] [--scene PATH]\n                            Resolve and inspect a headless profile\n  profile [list|dump] [--profile NAME] [--patch ID=on|off] [--plugin-config ID=JSON]\n                            Inspect profiles and effective plugin trees\n  client [--scene 2d|3d] [--script PATH] [--frames N]\n         [--benchmark --warmup N --output PATH] [--hot-reload]\n                            Open the winit/wgpu client\n  editor [--scene PATH] [--frames N] [--hot-reload]\n                            Open the editable .mnsc scene editor (hierarchy + inspector)\n  scene PATH [--output PATH] [--name NAME]\n                            Migrate and round-trip a .mnsc scene\n  benchmark [--scene 2d|3d] [--script PATH] [--frames N]\n            [--warmup N] [--output PATH]\n                            Measure release simulation ticks\n  demo [--ticks N] [--output PATH] [--snapshot PATH] [--script PATH]\n\nScript workflow:\n  npm install\n  npm run build\n  cargo run -- client\n  cargo run -- client --scene 3d\n  npm run benchmark"
    );
    Ok(())
}

fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    // The agent path runs through the same RuntimeSession as the windowed
    // client and editor: profile resolution, plugin lifecycle, script
    // runtime, revisions, and event sequencing all apply.
    let mut session = bootstrap::serve_session()?;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (response, quit) = session.handle_line(&line);
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
    let entities = scene::validate_scene_file(&scene_path)?;
    let effective = project.profile()?.resolve(&profile::Target::current())?;
    project.check_direct_dependencies(&effective)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "project": project.name,
            "engine": project.engine,
            "entities": entities,
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
    let mut scene_path = "testing/assets/benchmark-3d.mnsc".to_string();
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
    // Registration phase only: even a non-empty profile never starts runtime
    // providers here. The headless profile is empty, so has_plugin checks
    // below keep their original meaning.
    let mut session = bootstrap::headless_session(&profile_name, &scene_path)?;
    let effective =
        Profile::named(&profile_name)?.resolve(&moni_core::profile::Target::current())?;
    let gpu_initialized = session.host_mut().has_plugin(CORE_RENDER);
    let quickjs_initialized = session.host_mut().has_plugin(CORE_SIMULATION)
        || session.host_mut().script_host_count() != 0;
    if gpu_initialized || quickjs_initialized {
        bail!("headless profile initialized a forbidden runtime provider");
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "profile": profile_name,
            "entities": session.world().entities.len(),
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
    let mut patches: Vec<(String, bool)> = Vec::new();
    let mut configs: Vec<(String, Value)> = Vec::new();
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
                let raw = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--patch requires ID=on|off"))?;
                let (id, enabled) = raw
                    .rsplit_once('=')
                    .ok_or_else(|| anyhow::anyhow!("--patch must use ID=on|off"))?;
                let enabled = match enabled {
                    "on" => true,
                    "off" => false,
                    _ => bail!("--patch must use ID=on|off"),
                };
                patches.push((id.to_string(), enabled));
            }
            "--plugin-config" => {
                index += 1;
                let raw = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--plugin-config requires ID=JSON"))?;
                let (id, json) = raw
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("--plugin-config must use ID=JSON"))?;
                let value: Value = serde_json::from_str(json)
                    .with_context(|| format!("--plugin-config '{id}' has invalid JSON"))?;
                configs.push((id.to_string(), value));
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
                "profiles": ["minimal", "default", "headless", "editor", "serve"],
                "bundles": {
                    "minimal": [],
                    "default": ["core.simulation", "core.render", "core.fbx"],
                    "headless": [],
                    "editor": ["core.render", "core.fbx", "core.editor"],
                    "serve": ["core.simulation"]
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
    for (id, enabled) in patches {
        profile = profile.with_patch(&id, enabled);
    }
    for (id, value) in configs {
        profile = profile.with_config(&id, value);
    }
    let effective = profile.resolve(&target)?;
    println!("{}", serde_json::to_string_pretty(&effective.dump())?);
    Ok(())
}

fn editor_command() -> anyhow::Result<()> {
    moni_editor::run(
        env::args().skip(2).collect(),
        Box::new(|session, window, hot_reload| {
            // Registration phase only, with the window attached to the render
            // plugin: scene loading and render startup stay inside the editor
            // so they run after its event loop has a surface.
            let profile = Profile::named("editor")?.with_config(
                "core.render",
                json!({ "collectGpuTimestamps": true, "hotReload": hot_reload }),
            );
            bootstrap::register_profile(&profile, session.host_mut(), Some(Arc::clone(window)))?;
            Ok(())
        }),
    )
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
    if ticks == 0 || ticks > moni_core::engine::MAX_STEP_COUNT {
        bail!(
            "--ticks must be between 1 and {}",
            moni_core::engine::MAX_STEP_COUNT
        );
    }

    let script_path = workspace_path(&script)?;
    let source = fs::read_to_string(&script_path).with_context(|| {
        format!(
            "could not read {}; run 'npm install && npm run build' first",
            script_path.display()
        )
    })?;

    let mut engine = demo_engine(source)?;
    engine.set_script_runtime(Box::new(QuickJsScriptRuntime::new()));
    let step_result = engine.step(ticks)?;
    let svg = moni_core::render::render_svg(engine.world());

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
    external_model_engine(source, "testing/assets/benchmark-2d.mnsc")
}

pub(crate) fn demo_engine_3d(source: String) -> anyhow::Result<Engine> {
    external_model_engine(source, "testing/assets/benchmark-3d.mnsc")
}

fn external_model_engine(_source: String, scene_path: &str) -> anyhow::Result<Engine> {
    let path = workspace_path(scene_path)?;
    let world = scene::load_scene(&path)?;
    let mut engine = Engine::new();
    engine.load_world(world)?;
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("moni-template-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(root).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                collect_files(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// Detects `E:\path` / `E:/path` drive paths while ignoring URL schemes
    /// such as `https://registry.npmjs.org` in lockfiles.
    fn contains_drive_path(value: &str) -> bool {
        let bytes = value.as_bytes();
        for index in 0..bytes.len().saturating_sub(2) {
            if bytes[index].is_ascii_alphabetic()
                && bytes[index + 1] == b':'
                && (bytes[index + 2] == b'/' || bytes[index + 2] == b'\\')
                && bytes.get(index + 3) != Some(&b'/')
                && bytes.get(index + 3) != Some(&b'\\')
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn new_creates_independent_project_without_engine_paths() {
        let root = temp_root("independent");
        initialize_project(&root).expect("initialize project");
        assert!(root.join("game.mngp").is_file());
        assert!(root.join("moni.lock").is_file());
        assert!(root.join("scenes/main.mnsc").is_file());
        assert!(root.join("scripts/main.ts").is_file());
        assert!(root.join(".git").is_dir());

        let expected_name = root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("temp root has a name")
            .to_string();
        let project = project::GameProject::load(&root).expect("load generated project");
        assert_eq!(project.name, expected_name);
        project.check_lockfile(&root).expect("fresh lockfile");

        let mut files = Vec::new();
        collect_files(&root, &mut files);
        assert!(!files.is_empty());
        for file in files {
            let extension = file.extension().and_then(|value| value.to_str());
            if matches!(
                extension,
                Some("exe") | Some("dll") | Some("png") | Some("ico")
            ) {
                continue;
            }
            let contents = std::fs::read_to_string(&file).expect("template files are text");
            for needle in [
                "CARGO_MANIFEST_DIR",
                "moni-engine",
                "workspace\\code",
                "workspace/code",
            ] {
                assert!(
                    !contents.contains(needle),
                    "{} references engine path '{needle}'",
                    file.display()
                );
            }
            assert!(
                !contains_drive_path(&contents),
                "{} contains an absolute Windows drive path",
                file.display()
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn fuzz_smoke_completes_without_panics() {
        fuzz::run(vec![
            "--iterations".to_string(),
            "60".to_string(),
            "--seed".to_string(),
            "7".to_string(),
        ])
        .expect("fuzz smoke run should not catch parser panics");
    }
}
