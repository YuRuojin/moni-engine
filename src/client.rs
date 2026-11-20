use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use serde_json::json;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::benchmark::{BenchmarkReport, write_report};
use crate::engine::Engine;
use crate::gpu::RenderStatus;
use crate::host::{PluginHandle, PluginHost, SystemStage};
use crate::plugins::{RenderPlugin, SimulationPlugin};
use crate::profile::Profile;

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    run_internal(args, None)
}

pub fn run_with_profile(args: Vec<String>, profile: Profile) -> anyhow::Result<()> {
    run_internal(args, Some(profile))
}

fn run_internal(args: Vec<String>, profile_override: Option<Profile>) -> anyhow::Result<()> {
    let mut script = None;
    let mut scene = "2d";
    let mut world_path = None;
    let mut profile_name = "default".to_string();
    let mut revocations = Vec::new();
    let mut frame_limit = 0_u64;
    let mut benchmark = false;
    let mut warmup_frames = 0_u64;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--script" => {
                index += 1;
                script = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--script requires a path"))?
                        .clone(),
                );
            }
            "--scene" => {
                index += 1;
                scene = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scene requires 2d or 3d"))?;
                if scene != "2d" && scene != "3d" {
                    bail!("--scene must be 2d or 3d");
                }
            }
            "--world" => {
                index += 1;
                world_path = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--world requires a path"))?
                        .clone(),
                );
            }
            "--profile" => {
                index += 1;
                profile_name = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--profile requires a name"))?
                    .clone();
            }
            "--revoke" => {
                index += 1;
                revocations.push(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--revoke requires ID=PERMISSION"))?
                        .clone(),
                );
            }
            "--frames" => {
                index += 1;
                frame_limit = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--frames requires a value"))?
                    .parse()
                    .context("--frames must be a positive integer")?;
                if frame_limit == 0 {
                    bail!("--frames must be at least 1");
                }
            }
            "--benchmark" => benchmark = true,
            "--warmup" => {
                index += 1;
                warmup_frames = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--warmup requires a value"))?
                    .parse()
                    .context("--warmup must be a non-negative integer")?;
            }
            "--output" => {
                index += 1;
                output = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--output requires a path"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown client option '{unknown}'"),
        }
        index += 1;
    }
    if benchmark {
        if cfg!(debug_assertions) {
            bail!("client benchmarks require a release build");
        }
        if frame_limit == 0 {
            bail!("client benchmarks require --frames");
        }
        if warmup_frames >= frame_limit {
            bail!("--warmup must be smaller than --frames");
        }
    } else if warmup_frames != 0 || output.is_some() {
        bail!("--warmup and --output require --benchmark");
    }

    if world_path.is_some() {
        scene = "world";
    }
    let engine = if let Some(world_path) = world_path {
        let path = if Path::new(&world_path).is_absolute() {
            PathBuf::from(&world_path)
        } else {
            crate::workspace_path(&world_path)?
        };
        let world = crate::scene::load_scene(&path)?;
        let mut engine = Engine::new();
        engine.load_world(world)?;
        engine
    } else {
        let script = script.unwrap_or_else(|| "examples/dist/model-orbit.js".to_string());
        let script_path = crate::workspace_path(&script)?;
        let source = std::fs::read_to_string(&script_path).with_context(|| {
            format!(
                "could not read {}; run 'npm install && npm run build' first",
                script_path.display()
            )
        })?;
        if scene == "3d" {
            crate::demo_engine_3d(source)?
        } else {
            crate::demo_engine(source)?
        }
    };
    let event_loop = EventLoop::new().context("could not create the window event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut profile = match profile_override {
        Some(profile) => profile,
        None => Profile::named(&profile_name)?,
    };
    for revocation in revocations {
        let (plugin, permission) = revocation
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--revoke must use ID=PERMISSION"))?;
        profile = profile.with_revoked_permission(plugin, permission);
    }
    let mut app = ClientApp::new(engine, frame_limit, benchmark, warmup_frames, profile)?;
    event_loop
        .run_app(&mut app)
        .context("the window event loop failed")?;
    if let Some(error) = app.failure.take() {
        return Err(error);
    }

    let mut benchmark_result = None;
    if benchmark {
        let output = output.unwrap_or_else(|| format!("target/benchmarks/render-{scene}.json"));
        let report = BenchmarkReport::new(
            "render_cpu",
            "gpu_renderer_render_including_present",
            scene,
            app.rendered_frames,
            warmup_frames,
            std::mem::take(&mut app.frame_samples),
        )?;
        write_report(&report, &output)?;
        let preparation_output = Path::new(&output)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(format!("render-preparation-{scene}.json"))
            .to_string_lossy()
            .into_owned();
        let preparation_report = BenchmarkReport::new(
            "render_preparation_cpu",
            "vertices_uniforms_and_vertex_buffer_creation",
            scene,
            app.rendered_frames,
            warmup_frames,
            std::mem::take(&mut app.preparation_samples),
        )?;
        write_report(&preparation_report, &preparation_output)?;
        let render_plugin = app
            .host
            .plugin_mut::<RenderPlugin>(RenderPlugin::ID)
            .expect("render plugin is registered");
        let gpu_samples = render_plugin.take_gpu_samples();
        let gpu_supported = render_plugin.gpu_timestamps_supported();
        let gpu_result = if gpu_supported {
            let parent = Path::new(&output).parent().unwrap_or_else(|| Path::new(""));
            let shadow_path = parent
                .join(format!("gpu-shadow-{scene}.json"))
                .to_string_lossy()
                .into_owned();
            let main_path = parent
                .join(format!("gpu-main-{scene}.json"))
                .to_string_lossy()
                .into_owned();
            let shadow_samples: Vec<f64> = gpu_samples
                .iter()
                .map(|(shadow, _)| f64::from(*shadow))
                .collect();
            let main_samples: Vec<f64> = gpu_samples
                .iter()
                .map(|(_, main)| f64::from(*main))
                .collect();
            let shadow_report = BenchmarkReport::new(
                "gpu",
                "directional_shadow_pass",
                scene,
                app.rendered_frames,
                warmup_frames,
                shadow_samples,
            )?;
            let main_report = BenchmarkReport::new(
                "gpu",
                "main_render_pass",
                scene,
                app.rendered_frames,
                warmup_frames,
                main_samples,
            )?;
            write_report(&shadow_report, &shadow_path)?;
            write_report(&main_report, &main_path)?;
            json!({
                "supported": true,
                "shadow": { "output": shadow_path, "summary": shadow_report.summary },
                "main": { "output": main_path, "summary": main_report.summary },
            })
        } else {
            json!({ "supported": false })
        };
        benchmark_result = Some(json!({
            "output": output,
            "summary": report.summary,
            "preparation": {
                "output": preparation_output,
                "summary": preparation_report.summary,
            },
            "gpu": gpu_result,
        }));
    }
    let loaded_mesh_vertices = app
        .host
        .plugin_mut::<RenderPlugin>(RenderPlugin::ID)
        .map_or(0, |plugin| plugin.loaded_mesh_vertices());
    let simulation_plugin = app.simulation_handle.as_ref().map(PluginHandle::id);
    if let Some(handle) = app.simulation_handle.take() {
        handle.dispose(&mut app.host)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "scene": scene,
            "simulationPlugin": simulation_plugin,
            "frames": app.rendered_frames,
            "tick": app.host.world().tick,
            "entities": app.host.world().entities.len(),
            "loadedMeshVertices": loaded_mesh_vertices,
            "benchmark": benchmark_result,
        }))?
    );
    Ok(())
}

struct ClientApp {
    host: PluginHost,
    profile: Profile,
    simulation_handle: Option<PluginHandle>,
    window: Option<Arc<Window>>,
    last_update: Instant,
    accumulator_seconds: f64,
    frame_limit: u64,
    rendered_frames: u64,
    collect_benchmark: bool,
    warmup_frames: u64,
    frame_samples: Vec<f64>,
    preparation_samples: Vec<f64>,
    title_state: Option<(u64, usize, usize)>,
    failure: Option<anyhow::Error>,
}

impl ClientApp {
    fn new(
        engine: Engine,
        frame_limit: u64,
        collect_benchmark: bool,
        warmup_frames: u64,
        profile: Profile,
    ) -> anyhow::Result<Self> {
        let mut host = PluginHost::new(engine);
        let simulation_handle = profile
            .clone()
            .with_patch("core.render", false)
            .with_patch("core.fbx", false)
            .instantiate(&mut host, None, false)?
            .into_iter()
            .find(|handle| handle.id() == SimulationPlugin::ID);
        let sample_capacity = if collect_benchmark {
            (frame_limit - warmup_frames) as usize
        } else {
            0
        };
        Ok(Self {
            host,
            profile,
            simulation_handle,
            window: None,
            last_update: Instant::now(),
            accumulator_seconds: 0.0,
            frame_limit,
            rendered_frames: 0,
            collect_benchmark,
            warmup_frames,
            frame_samples: Vec::with_capacity(sample_capacity),
            preparation_samples: Vec::with_capacity(sample_capacity),
            title_state: None,
            failure: None,
        })
    }

    fn advance_simulation(&mut self) -> anyhow::Result<()> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        self.accumulator_seconds += elapsed;

        let tick_seconds = 1.0 / f64::from(self.host.world().tick_rate);
        let steps = (self.accumulator_seconds / tick_seconds).floor() as u64;
        if steps == 0 {
            return Ok(());
        }
        let applied_steps = steps.min(5);
        for _ in 0..applied_steps {
            self.host.run_stage(SystemStage::FixedUpdate)?;
        }
        if steps > applied_steps {
            self.accumulator_seconds = 0.0;
        } else {
            self.accumulator_seconds -= applied_steps as f64 * tick_seconds;
        }
        Ok(())
    }
}

impl ApplicationHandler for ClientApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let viewport = &self.host.world().viewport;
        let attributes = Window::default_attributes()
            .with_title("Moni Engine")
            .with_inner_size(winit::dpi::LogicalSize::new(
                viewport.width as f64,
                viewport.height as f64,
            ));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                let result = self
                    .profile
                    .clone()
                    .with_patch("core.simulation", false)
                    .instantiate(
                        &mut self.host,
                        Some(Arc::clone(&window)),
                        self.collect_benchmark,
                    )
                    .and_then(|_| self.host.run_stage(SystemStage::Startup));
                match result {
                    Ok(()) => {
                        self.window = Some(window);
                        self.last_update = Instant::now();
                    }
                    Err(error) => {
                        self.failure = Some(anyhow::Error::new(error));
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                self.failure = Some(anyhow::anyhow!(error));
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state.is_pressed()
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.host.resize(size.width, size.height);
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.advance_simulation() {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
                let events = self.host.take_events();
                let title_state = (
                    self.host.world().tick,
                    self.host.world().entities.len(),
                    events.len(),
                );
                if self.title_state != Some(title_state) {
                    self.title_state = Some(title_state);
                    window.set_title(&format!(
                        "Moni Engine | tick {} | entities {} | events {}",
                        title_state.0, title_state.1, title_state.2
                    ));
                }
                let started = self.collect_benchmark.then(Instant::now);
                match self.host.run_stage(SystemStage::Render) {
                    Ok(()) => match self.host.last_render_status() {
                        Some(RenderStatus::Rendered) => {
                            self.rendered_frames += 1;
                            if let Some(started) = started
                                && self.rendered_frames > self.warmup_frames
                            {
                                self.frame_samples
                                    .push(started.elapsed().as_secs_f64() * 1_000.0);
                                let preparation = self
                                    .host
                                    .plugin_mut::<RenderPlugin>(RenderPlugin::ID)
                                    .expect("render plugin is registered")
                                    .last_preparation_time();
                                self.preparation_samples
                                    .push(preparation.as_secs_f64() * 1_000.0);
                            }
                            if self.frame_limit != 0 && self.rendered_frames >= self.frame_limit {
                                event_loop.exit();
                            }
                        }
                        Some(RenderStatus::Skip) => {}
                        Some(RenderStatus::Reconfigure) => {
                            let size = window.inner_size();
                            self.host.resize(size.width, size.height);
                        }
                        None => {}
                    },
                    Err(error) => {
                        self.failure = Some(anyhow::Error::new(error));
                        event_loop.exit();
                    }
                }
                window.request_redraw();
            }
            _ => {}
        }
    }
}
