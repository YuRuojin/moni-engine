use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::engine::Engine;
use crate::gpu::{GpuRenderer, RenderStatus};

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut script = "examples/dist/player.js".to_string();
    let mut frame_limit = 0_u64;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--script" => {
                index += 1;
                script = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--script requires a path"))?
                    .clone();
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
            unknown => bail!("unknown client option '{unknown}'"),
        }
        index += 1;
    }

    let script_path = crate::workspace_path(&script)?;
    let source = std::fs::read_to_string(&script_path).with_context(|| {
        format!(
            "could not read {}; run 'npm install && npm run build' first",
            script_path.display()
        )
    })?;
    let engine = crate::demo_engine(source)?;
    let event_loop = EventLoop::new().context("could not create the window event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = ClientApp::new(engine, frame_limit);
    event_loop
        .run_app(&mut app)
        .context("the window event loop failed")?;
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "frames": app.rendered_frames,
            "tick": app.engine.world().tick,
            "entities": app.engine.world().entities.len()
        })
    );
    Ok(())
}

struct ClientApp {
    engine: Engine,
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    last_update: Instant,
    accumulator_seconds: f64,
    frame_limit: u64,
    rendered_frames: u64,
}

impl ClientApp {
    fn new(engine: Engine, frame_limit: u64) -> Self {
        Self {
            engine,
            window: None,
            renderer: None,
            last_update: Instant::now(),
            accumulator_seconds: 0.0,
            frame_limit,
            rendered_frames: 0,
        }
    }

    fn advance_simulation(&mut self) -> anyhow::Result<()> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        self.accumulator_seconds += elapsed;

        let tick_seconds = 1.0 / f64::from(self.engine.world().tick_rate);
        let steps = (self.accumulator_seconds / tick_seconds).floor() as u64;
        if steps == 0 {
            return Ok(());
        }
        let applied_steps = steps.min(5);
        self.engine.step(applied_steps)?;
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
        let viewport = &self.engine.world().viewport;
        let attributes = Window::default_attributes()
            .with_title("Moni Engine")
            .with_inner_size(winit::dpi::LogicalSize::new(
                viewport.width as f64,
                viewport.height as f64,
            ));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match GpuRenderer::new(Arc::clone(&window)) {
                    Ok(renderer) => {
                        self.window = Some(window);
                        self.renderer = Some(renderer);
                        self.last_update = Instant::now();
                    }
                    Err(error) => {
                        eprintln!("could not initialize wgpu: {error:#}");
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                eprintln!("could not create the client window: {error}");
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.advance_simulation() {
                    eprintln!("simulation failed: {error:#}");
                    event_loop.exit();
                    return;
                }
                let events = self.engine.take_events();
                window.set_title(&format!(
                    "Moni Engine | tick {} | entities {} | events {}",
                    self.engine.world().tick,
                    self.engine.world().entities.len(),
                    events.len()
                ));
                if let Some(renderer) = &mut self.renderer {
                    match renderer.render(self.engine.world()) {
                        Ok(RenderStatus::Rendered) => {
                            self.rendered_frames += 1;
                            if self.frame_limit != 0 && self.rendered_frames >= self.frame_limit {
                                event_loop.exit();
                            }
                        }
                        Ok(RenderStatus::Skip) => {}
                        Ok(RenderStatus::Reconfigure) => {
                            let size = window.inner_size();
                            renderer.resize(size.width, size.height);
                        }
                        Err(error) => {
                            eprintln!("render failed: {error:#}");
                            event_loop.exit();
                        }
                    }
                }
                window.request_redraw();
            }
            _ => {}
        }
    }
}
