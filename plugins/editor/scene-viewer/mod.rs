use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use moni_core::engine::{Command, Engine};
use moni_core::host::{CORE_RENDER, RenderStatus, SystemStage};
use moni_core::profile::Profile;
use moni_core::scene3d::{Camera3d, CameraProjection, Vector3};
use moni_core::session::RuntimeSession;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

pub mod document;
pub mod ui;
pub mod ui_egui;

use document::EditorDocument;
use ui::OverlayCache;
use ui_egui::EditorUi;

/// Composition hook supplied by the binary: installs the profile's plugins
/// once the window exists. The editor itself never names a concrete plugin.
pub type Bootstrap = dyn FnOnce(&mut RuntimeSession, &Arc<Window>, bool) -> anyhow::Result<()>;

pub fn run(args: Vec<String>, bootstrap: Box<Bootstrap>) -> anyhow::Result<()> {
    let mut scene_path = "testing/assets/benchmark-3d.mnsc".to_string();
    let mut golden_path = None;
    let mut frame_limit = 0_u64;
    let mut hot_reload = false;
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
            "--hot-reload" => hot_reload = true,
            "--golden" => {
                index += 1;
                golden_path = Some(moni_core::workspace_path(
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

    // The scene itself loads in `resumed` through the EditorDocument after
    // the profile registered, so plugin-contributed component schemas
    // participate in scene validation; the editor surface stays 960x540.
    let engine = Engine::new();
    let profile = Profile::named("editor")?;
    let mut session = RuntimeSession::new(engine, profile);
    session.world_mut().viewport.width = 960;
    session.world_mut().viewport.height = 540;
    let event_loop = EventLoop::new().context("could not create the editor event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = EditorApp::new(
        session,
        scene_path,
        frame_limit,
        golden_path,
        hot_reload,
        bootstrap,
    );
    event_loop
        .run_app(&mut app)
        .context("the editor event loop failed")?;
    if let Some(error) = app.failure.take() {
        return Err(error);
    }
    let document = app
        .document
        .as_mut()
        .context("the editor document is missing after startup")?;
    let loaded_mesh_vertices = document
        .session_mut()
        .host_mut()
        .render_api_mut(CORE_RENDER)
        .map_or(0, |render| render.loaded_mesh_vertices());
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "editor": "editable",
            "frames": app.frames,
            "entities": document.session().world().entities.len(),
            "loadedMeshVertices": loaded_mesh_vertices,
            "goldenCaptured": app.golden_mode,
        })
    );
    Ok(())
}

struct EditorApp {
    /// Session handed to the document once `resumed` opens the scene. `None`
    /// between `start_session` taking it and the document owning it.
    startup_session: Option<RuntimeSession>,
    /// The open scene document; `None` until the window exists and the
    /// bootstrap + scene load finished, and after a startup failure.
    document: Option<EditorDocument>,
    scene_path: String,
    initial_camera: Camera3d,
    window: Option<Arc<Window>>,
    frame_limit: u64,
    frames: u64,
    frame_ms: f64,
    hot_reload: bool,
    bootstrap: Option<Box<Bootstrap>>,
    gpu_samples: Vec<(f32, f32)>,
    /// Overlay rebuild gate: quads are rebuilt and re-installed only when the
    /// overlay inputs change; other frames keep the renderer's cached quads.
    overlay_cache: OverlayCache,
    /// egui panels and input bridge; never created in golden mode so the
    /// renderer never receives a UI frame there.
    ui: Option<EditorUi>,
    golden_mode: bool,
    golden_path: Option<PathBuf>,
    failure: Option<anyhow::Error>,
}

impl EditorApp {
    fn new(
        session: RuntimeSession,
        scene_path: String,
        frame_limit: u64,
        golden_path: Option<PathBuf>,
        hot_reload: bool,
        bootstrap: Box<Bootstrap>,
    ) -> Self {
        Self {
            initial_camera: session.world().camera.clone(),
            startup_session: Some(session),
            document: None,
            scene_path,
            window: None,
            frame_limit,
            frames: 0,
            frame_ms: 0.0,
            hot_reload,
            bootstrap: Some(bootstrap),
            gpu_samples: Vec::new(),
            overlay_cache: OverlayCache::default(),
            ui: None,
            golden_mode: golden_path.is_some(),
            golden_path,
            failure: None,
        }
    }

    /// Registration + scene load + render startup, in the directive order:
    /// register the profile with the window attached, open the scene through
    /// the EditorDocument (the single scene-loading path, which validates
    /// against the effective registry), start the render plugin, then run the
    /// Startup stage.
    fn start_session(&mut self, window: &Arc<Window>) -> anyhow::Result<()> {
        let bootstrap = self.bootstrap.take().expect("editor bootstrap runs once");
        let mut session = self
            .startup_session
            .take()
            .expect("editor session exists before resume");
        bootstrap(&mut session, window, self.hot_reload)?;
        // The document owns the session from here on; scene edits must go
        // through it, not the world directly.
        let mut document = EditorDocument::new(session, &self.scene_path)?;
        // The loaded scene brings its own viewport; the editor keeps its
        // fixed 960x540 surface for camera and overlay math. Composition-root
        // surface setup only — this never counts as an edit (no revision).
        document.session_mut().world_mut().viewport.width = 960;
        document.session_mut().world_mut().viewport.height = 540;
        document
            .session_mut()
            .host_mut()
            .start_plugin(moni_core::host::CORE_RENDER)?;
        document
            .session_mut()
            .host_mut()
            .run_stage(SystemStage::Startup)
            .map_err(anyhow::Error::new)?;
        self.document = Some(document);
        Ok(())
    }

    fn camera_input(&mut self, key: &Key) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        let mut camera = document.session().world().camera.clone();
        let movement = 0.25;
        match key {
            Key::Named(NamedKey::ArrowLeft) => camera.position.x -= movement,
            Key::Named(NamedKey::ArrowRight) => camera.position.x += movement,
            Key::Named(NamedKey::ArrowUp) => camera.position.z -= movement,
            Key::Named(NamedKey::ArrowDown) => camera.position.z += movement,
            Key::Named(NamedKey::PageUp) => zoom_camera(&mut camera, 0.9),
            Key::Named(NamedKey::PageDown) => zoom_camera(&mut camera, 1.1),
            Key::Named(NamedKey::Home) => camera.clone_from(&self.initial_camera),
            _ => return false,
        }
        // Camera edits go through the public command API so editor input
        // produces the same events and revisions as CLI or agent edits.
        if let Err(error) = document.session_mut().apply(Command::SetCamera { camera }) {
            self.failure = Some(anyhow::Error::new(error));
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
        // The window opens at the session's viewport size; the loaded scene
        // may still override it inside the fixed 960x540 surface afterwards.
        let (width, height) = self
            .startup_session
            .as_ref()
            .map(|session| {
                (
                    session.world().viewport.width,
                    session.world().viewport.height,
                )
            })
            .unwrap_or((960, 540));
        let attributes = Window::default_attributes()
            .with_title("Moni Scene Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(
                f64::from(width),
                f64::from(height),
            ));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match self.start_session(&window) {
                    Ok(()) => {
                        if !self.golden_mode {
                            // egui stays fully disabled in golden mode so the
                            // renderer never receives a UI frame there and
                            // golden captures remain byte-deterministic.
                            self.ui = Some(EditorUi::new(Arc::clone(&window)));
                        }
                        self.window = Some(window);
                    }
                    Err(error) => {
                        self.failure = Some(error);
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
        // egui-winit records every event before the app-level handlers run;
        // `consumed` means the UI is using this pointer/keyboard interaction.
        let mut egui_consumed = false;
        if let Some(ui) = &mut self.ui {
            let response = ui.on_window_event(&event);
            egui_consumed = response.consumed;
            if response.repaint {
                window.request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                } else if egui_consumed {
                    // egui owns the keyboard (a text field has focus); the
                    // camera navigation yields.
                } else if self.camera_input(&event.logical_key) {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(document) = self.document.as_mut() {
                    document
                        .session_mut()
                        .host_mut()
                        .resize(size.width, size.height);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let Some(document) = self.document.as_mut() else {
                    return;
                };
                let (preparation_ms, loaded_vertices) = document
                    .session_mut()
                    .host_mut()
                    .render_api_mut(CORE_RENDER)
                    .map(|render| {
                        (
                            render.last_preparation_time().as_secs_f64() * 1_000.0,
                            render.loaded_mesh_vertices(),
                        )
                    })
                    .unwrap_or((0.0, 0));
                if self.golden_mode {
                    // Golden captures must stay deterministic: install an
                    // empty overlay every frame exactly as before, bypassing
                    // the rebuild cache.
                    if let Some(render) = document
                        .session_mut()
                        .host_mut()
                        .render_api_mut(CORE_RENDER)
                    {
                        render.set_overlay_quads(Vec::new());
                    }
                } else {
                    // Rebuild the overlay quads only when their inputs
                    // changed; other frames keep the renderer's cached quads.
                    let world = document.session().world();
                    if let Some(quads) = self.overlay_cache.quads(
                        world,
                        &self.scene_path,
                        preparation_ms,
                        self.frame_ms,
                        &self.gpu_samples,
                        loaded_vertices,
                    ) && let Some(render) = document
                        .session_mut()
                        .host_mut()
                        .render_api_mut(CORE_RENDER)
                    {
                        render.set_overlay_quads(quads);
                    }
                }
                // The egui frame goes to the render plugin through the
                // type-erased hand-off; a renderer that is not started or a
                // payload mismatch degrades to "no UI layer this frame".
                if !self.golden_mode
                    && let Some(ui) = &mut self.ui
                {
                    let mut frame = ui.build_frame(document);
                    if let Some(render) = document
                        .session_mut()
                        .host_mut()
                        .render_api_mut(CORE_RENDER)
                    {
                        render.set_egui_frame(&mut frame);
                    }
                }
                if let Some(path) = self.golden_path.take()
                    && let Some(render) = document
                        .session_mut()
                        .host_mut()
                        .render_api_mut(CORE_RENDER)
                {
                    render.capture_next(path);
                }
                let started = Instant::now();
                match document
                    .session_mut()
                    .host_mut()
                    .run_stage(SystemStage::Render)
                {
                    Ok(()) => match document.session_mut().host_mut().last_render_status() {
                        Some(RenderStatus::Rendered) => {
                            self.frame_ms = started.elapsed().as_secs_f64() * 1_000.0;
                            if let Some(render) = document
                                .session_mut()
                                .host_mut()
                                .render_api_mut(CORE_RENDER)
                            {
                                self.gpu_samples.extend(render.take_gpu_samples());
                            }
                            self.frames += 1;
                            if self.frame_limit != 0 && self.frames >= self.frame_limit {
                                event_loop.exit();
                            }
                        }
                        Some(RenderStatus::Skip) => {}
                        Some(RenderStatus::Reconfigure) => {
                            let size = window.inner_size();
                            document
                                .session_mut()
                                .host_mut()
                                .resize(size.width, size.height);
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
