use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::editor_ui::build_overlay;
use crate::engine::Engine;
use crate::gpu::RenderStatus;
use crate::host::{PluginHost, SystemStage};
use crate::plugins::RenderPlugin;
use crate::profile::Profile;
use crate::scene3d::{Camera3d, CameraProjection, Vector3};

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut scene_path = "testing/assets/benchmark-3d.mnsc".to_string();
    let mut golden_path = None;
    let mut frame_limit = 0_u64;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--scene" => {
                index += 1;
                scene_path = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scene requires a path"))?
                    .clone();
            }
            "--golden" => {
                index += 1;
                golden_path = Some(crate::workspace_path(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--golden requires a path"))?,
                )?);
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
            unknown => bail!("unknown editor option '{unknown}'"),
        }
        index += 1;
    }

    let path = crate::workspace_path(&scene_path)?;
    let world = crate::scene::load_scene(&path)?;
    let mut engine = Engine::new();
    engine.load_world(world)?;
    let mut host = PluginHost::new(engine);
    host.world_mut().viewport.width = 960;
    host.world_mut().viewport.height = 540;
    let event_loop = EventLoop::new().context("could not create the editor event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = EditorApp::new(host, scene_path, frame_limit, golden_path);
    event_loop
        .run_app(&mut app)
        .context("the editor event loop failed")?;
    if let Some(error) = app.failure.take() {
        return Err(error);
    }
    let loaded_mesh_vertices = app
        .host
        .plugin_mut::<RenderPlugin>(RenderPlugin::ID)
        .map_or(0, |plugin| plugin.loaded_mesh_vertices());
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "editor": "read_only",
            "frames": app.frames,
            "entities": app.host.world().entities.len(),
            "loadedMeshVertices": loaded_mesh_vertices,
            "goldenCaptured": app.golden_mode,
        })
    );
    Ok(())
}

struct EditorApp {
    host: PluginHost,
    scene_path: String,
    initial_camera: Camera3d,
    window: Option<Arc<Window>>,
    frame_limit: u64,
    frames: u64,
    frame_ms: f64,
    gpu_samples: Vec<(f32, f32)>,
    golden_mode: bool,
    golden_path: Option<PathBuf>,
    failure: Option<anyhow::Error>,
}

impl EditorApp {
    fn new(
        host: PluginHost,
        scene_path: String,
        frame_limit: u64,
        golden_path: Option<PathBuf>,
    ) -> Self {
        Self {
            initial_camera: host.world().camera.clone(),
            host,
            scene_path,
            window: None,
            frame_limit,
            frames: 0,
            frame_ms: 0.0,
            gpu_samples: Vec::new(),
            golden_mode: golden_path.is_some(),
            golden_path,
            failure: None,
        }
    }

    fn camera_input(&mut self, key: &Key) -> bool {
        let camera = &mut self.host.world_mut().camera;
        let movement = 0.25;
        match key {
            Key::Named(NamedKey::ArrowLeft) => camera.position.x -= movement,
            Key::Named(NamedKey::ArrowRight) => camera.position.x += movement,
            Key::Named(NamedKey::ArrowUp) => camera.position.z -= movement,
            Key::Named(NamedKey::ArrowDown) => camera.position.z += movement,
            Key::Named(NamedKey::PageUp) => zoom_camera(camera, 0.9),
            Key::Named(NamedKey::PageDown) => zoom_camera(camera, 1.1),
            Key::Named(NamedKey::Home) => camera.clone_from(&self.initial_camera),
            _ => return false,
        }
        true
    }
}

fn zoom_camera(camera: &mut Camera3d, factor: f64) {
    let target = camera.target;
    camera.position = Vector3 {
        x: target.x + (camera.position.x - target.x) * factor,
        y: target.y + (camera.position.y - target.y) * factor,
        z: target.z + (camera.position.z - target.z) * factor,
    };
    if camera.projection == CameraProjection::Orthographic {
        camera.orthographic_height = (camera.orthographic_height * factor).clamp(1.0, 50.0);
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let viewport = &self.host.world().viewport;
        let attributes = Window::default_attributes()
            .with_title("Moni Scene Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(
                f64::from(viewport.width),
                f64::from(viewport.height),
            ));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                let result = Profile::named("editor")
                    .and_then(|profile| {
                        profile.instantiate(&mut self.host, Some(Arc::clone(&window)), true)
                    })
                    .and_then(|_| self.host.run_stage(SystemStage::Startup));
                match result {
                    Ok(()) => self.window = Some(window),
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
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                } else if self.camera_input(&event.logical_key) {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                self.host.resize(size.width, size.height);
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let (preparation_ms, loaded_vertices) = self
                    .host
                    .plugin_mut::<RenderPlugin>(RenderPlugin::ID)
                    .map(|plugin| {
                        (
                            plugin.last_preparation_time().as_secs_f64() * 1_000.0,
                            plugin.loaded_mesh_vertices(),
                        )
                    })
                    .unwrap_or((0.0, 0));
                let overlay = if self.golden_mode {
                    Vec::new()
                } else {
                    build_overlay(
                        self.host.world(),
                        &self.scene_path,
                        preparation_ms,
                        self.frame_ms,
                        &self.gpu_samples,
                        loaded_vertices,
                    )
                };
                if let Some(render_plugin) = self.host.plugin_mut::<RenderPlugin>(RenderPlugin::ID)
                {
                    render_plugin.set_overlay_quads(overlay);
                }
                if let Some(path) = self.golden_path.take()
                    && let Some(plugin) = self.host.plugin_mut::<RenderPlugin>(RenderPlugin::ID)
                {
                    plugin.capture_next(path);
                }
                let started = Instant::now();
                match self.host.run_stage(SystemStage::Render) {
                    Ok(()) => match self.host.last_render_status() {
                        Some(RenderStatus::Rendered) => {
                            self.frame_ms = started.elapsed().as_secs_f64() * 1_000.0;
                            if let Some(plugin) =
                                self.host.plugin_mut::<RenderPlugin>(RenderPlugin::ID)
                            {
                                self.gpu_samples.extend(plugin.take_gpu_samples());
                            }
                            self.frames += 1;
                            if self.frame_limit != 0 && self.frames >= self.frame_limit {
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
