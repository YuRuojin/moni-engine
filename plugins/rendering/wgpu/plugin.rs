//! The core.render plugin: wraps the wgpu renderer behind the host's plugin
//! and render-API traits so clients and editors never name this type.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use winit::window::Window;

use crate::GpuRenderer;
use moni_core::host::{
    Access, CORE_RENDER, OverlayQuad, Plugin, PluginContext, PluginError, PluginOutput,
    RenderPluginApi, Resource, SystemDescriptor, SystemStage, ThreadAffinity,
};

/// The `core.render` plugin instance.
pub struct RenderPlugin {
    window: Arc<Window>,
    renderer: Option<GpuRenderer>,
    collect_gpu_timestamps: bool,
    load_fbx_assets: bool,
    hot_reload: bool,
}

impl RenderPlugin {
    pub const ID: &'static str = CORE_RENDER;

    pub fn new(
        window: Arc<Window>,
        collect_gpu_timestamps: bool,
        load_fbx_assets: bool,
        hot_reload: bool,
    ) -> Self {
        Self {
            window,
            renderer: None,
            collect_gpu_timestamps,
            load_fbx_assets,
            hot_reload,
        }
    }
}

impl RenderPluginApi for RenderPlugin {
    fn last_preparation_time(&self) -> Duration {
        self.renderer
            .as_ref()
            .map_or(Duration::ZERO, GpuRenderer::last_preparation_time)
    }

    fn loaded_mesh_vertices(&self) -> usize {
        self.renderer
            .as_ref()
            .map_or(0, GpuRenderer::loaded_mesh_vertices)
    }

    fn gpu_timestamps_supported(&self) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(GpuRenderer::gpu_timestamps_supported)
    }

    fn take_gpu_samples(&mut self) -> Vec<(f32, f32)> {
        self.renderer
            .as_mut()
            .map(|renderer| renderer.take_gpu_samples())
            .unwrap_or_default()
    }

    fn set_overlay_quads(&mut self, quads: Vec<OverlayQuad>) {
        if let Some(renderer) = &mut self.renderer {
            renderer.set_overlay_quads(quads);
        }
    }

    fn capture_next(&mut self, path: PathBuf) {
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

    fn configure(&mut self, config: &Value) -> Result<(), PluginError> {
        let Some(object) = config.as_object() else {
            return Err(PluginError::new("core.render config must be an object"));
        };
        for (key, value) in object {
            match key.as_str() {
                "hotReload" => {
                    let enabled = value.as_bool().ok_or_else(|| {
                        PluginError::new("core.render config 'hotReload' must be a boolean")
                    })?;
                    self.hot_reload = enabled;
                }
                other => {
                    return Err(PluginError::new(format!(
                        "core.render config has unknown field '{other}'"
                    )));
                }
            }
        }
        Ok(())
    }

    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
        context.require("gpu.create")?;
        if self.load_fbx_assets {
            context.require_from(moni_core::host::CORE_FBX, "asset.read.workspace")?;
        }
        self.renderer = Some(
            GpuRenderer::new(
                Arc::clone(&self.window),
                context.engine.world(),
                self.collect_gpu_timestamps,
                self.load_fbx_assets,
                self.hot_reload,
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
                    context.require_from(moni_core::host::CORE_FBX, "asset.read.workspace")?;
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

    fn as_render_api(&mut self) -> Option<&mut dyn RenderPluginApi> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
