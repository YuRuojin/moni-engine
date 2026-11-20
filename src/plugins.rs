use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use winit::window::Window;

use crate::gpu::{GpuRenderer, OverlayQuad};
use crate::host::{
    Access, Plugin, PluginContext, PluginError, PluginOutput, Resource, SystemDescriptor,
    SystemStage, ThreadAffinity,
};

pub struct SimulationPlugin;

impl SimulationPlugin {
    pub const ID: &'static str = "core.simulation";
}

impl Plugin for SimulationPlugin {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn systems(&self) -> Vec<SystemDescriptor> {
        vec![
            SystemDescriptor {
                id: "core.simulation.fixed_update",
                plugin_id: Self::ID,
                stage: SystemStage::FixedUpdate,
                affinity: ThreadAffinity::Logic,
                after: vec![],
                access: vec![
                    Access::write(Resource::World),
                    Access::write(Resource::Scripts),
                ],
            },
            SystemDescriptor {
                id: "core.simulation.shutdown",
                plugin_id: Self::ID,
                stage: SystemStage::Shutdown,
                affinity: ThreadAffinity::Logic,
                after: vec![],
                access: vec![Access::write(Resource::Scripts)],
            },
        ]
    }

    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
        context.require("script.execute")
    }

    fn stop(&mut self, context: &mut PluginContext) {
        context.engine.shutdown_runtime();
    }

    fn execute(
        &mut self,
        stage: SystemStage,
        context: &mut PluginContext,
    ) -> Result<PluginOutput, PluginError> {
        match stage {
            SystemStage::FixedUpdate => {
                context.require("script.execute")?;
                context
                    .engine
                    .step(1)
                    .map_err(|error| PluginError::new(error.message))?;
                Ok(PluginOutput::None)
            }
            SystemStage::Shutdown => {
                context.engine.shutdown_runtime();
                Ok(PluginOutput::None)
            }
            _ => Ok(PluginOutput::None),
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct RenderPlugin {
    window: Arc<Window>,
    renderer: Option<GpuRenderer>,
    collect_gpu_timestamps: bool,
    load_fbx_assets: bool,
}

impl RenderPlugin {
    pub const ID: &'static str = "core.render";

    pub fn new(window: Arc<Window>, collect_gpu_timestamps: bool, load_fbx_assets: bool) -> Self {
        Self {
            window,
            renderer: None,
            collect_gpu_timestamps,
            load_fbx_assets,
        }
    }

    pub fn last_preparation_time(&self) -> std::time::Duration {
        self.renderer
            .as_ref()
            .map_or(std::time::Duration::ZERO, |renderer| {
                renderer.last_preparation_time()
            })
    }

    pub fn loaded_mesh_vertices(&self) -> usize {
        self.renderer
            .as_ref()
            .map_or(0, GpuRenderer::loaded_mesh_vertices)
    }

    pub fn gpu_timestamps_supported(&self) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(GpuRenderer::gpu_timestamps_supported)
    }

    pub fn take_gpu_samples(&mut self) -> Vec<(f32, f32)> {
        self.renderer
            .as_mut()
            .map(|renderer| renderer.take_gpu_samples())
            .unwrap_or_default()
    }

    pub fn set_overlay_quads(&mut self, quads: Vec<OverlayQuad>) {
        if let Some(renderer) = &mut self.renderer {
            renderer.set_overlay_quads(quads);
        }
    }

    pub fn capture_next(&mut self, path: PathBuf) {
        if let Some(renderer) = &mut self.renderer {
            renderer.capture_next(path);
        }
    }
}

impl Plugin for RenderPlugin {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn systems(&self) -> Vec<SystemDescriptor> {
        vec![
            SystemDescriptor {
                id: "core.render.startup",
                plugin_id: Self::ID,
                stage: SystemStage::Startup,
                affinity: ThreadAffinity::Render,
                after: vec![],
                access: vec![
                    Access::read(Resource::World),
                    Access::read(Resource::Assets),
                    Access::write(Resource::Gpu),
                ],
            },
            SystemDescriptor {
                id: "core.render.frame",
                plugin_id: Self::ID,
                stage: SystemStage::Render,
                affinity: ThreadAffinity::Render,
                after: vec!["core.render.startup"],
                access: vec![
                    Access::read(Resource::World),
                    Access::read(Resource::Assets),
                    Access::write(Resource::Gpu),
                ],
            },
            SystemDescriptor {
                id: "core.render.shutdown",
                plugin_id: Self::ID,
                stage: SystemStage::Shutdown,
                affinity: ThreadAffinity::Render,
                after: vec![],
                access: vec![Access::write(Resource::Gpu)],
            },
        ]
    }

    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
        context.require("gpu.create")?;
        if self.load_fbx_assets {
            context.require_from("core.fbx", "asset.read.workspace")?;
        }
        self.renderer = Some(
            GpuRenderer::new(
                Arc::clone(&self.window),
                context.engine.world(),
                self.collect_gpu_timestamps,
                self.load_fbx_assets,
            )
            .map_err(|error| PluginError::new(format!("{error:#}")))?,
        );
        Ok(())
    }

    fn stop(&mut self, _context: &mut PluginContext) {
        self.renderer = None;
    }

    fn execute(
        &mut self,
        stage: SystemStage,
        context: &mut PluginContext,
    ) -> Result<PluginOutput, PluginError> {
        match stage {
            SystemStage::Render => {
                context.require("gpu.create")?;
                if self.load_fbx_assets {
                    context.require_from("core.fbx", "asset.read.workspace")?;
                }
                let status = self
                    .renderer
                    .as_mut()
                    .ok_or_else(|| PluginError::new("render plugin is not started"))?
                    .render(context.engine.world())
                    .map_err(|error| PluginError::new(format!("{error:#}")))?;
                Ok(PluginOutput::Render(status))
            }
            SystemStage::Shutdown => {
                self.renderer = None;
                Ok(PluginOutput::None)
            }
            _ => Ok(PluginOutput::None),
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Some(renderer) = &mut self.renderer {
            renderer.resize(width, height);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
