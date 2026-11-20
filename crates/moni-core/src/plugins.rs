use std::any::Any;

use serde_json::Value;

use crate::host::{
    Access, CORE_SIMULATION, Plugin, PluginContext, PluginError, PluginOutput, Resource,
    SystemDescriptor, SystemStage, ThreadAffinity,
};

/// Core-owned simulation glue: drives fixed-step ticks and owns the script
/// execution permission boundary. It references only core traits — the
/// scripting implementation is installed by the composition root.
pub struct SimulationPlugin {
    tick_rate_override: Option<u32>,
}

impl SimulationPlugin {
    pub const ID: &'static str = CORE_SIMULATION;

    pub fn new() -> Self {
        Self {
            tick_rate_override: None,
        }
    }
}

impl Default for SimulationPlugin {
    fn default() -> Self {
        Self::new()
    }
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

    fn configure(&mut self, config: &Value) -> Result<(), PluginError> {
        let Some(object) = config.as_object() else {
            return Err(PluginError::new("core.simulation config must be an object"));
        };
        for (key, value) in object {
            match key.as_str() {
                "tickRate" => {
                    let rate = value.as_u64().ok_or_else(|| {
                        PluginError::new("core.simulation config 'tickRate' must be an integer")
                    })?;
                    if !(1..=1000).contains(&rate) {
                        return Err(PluginError::new(
                            "core.simulation config 'tickRate' must be between 1 and 1000",
                        ));
                    }
                    self.tick_rate_override = Some(u32::try_from(rate).expect("range checked"));
                }
                other => {
                    return Err(PluginError::new(format!(
                        "core.simulation config has unknown field '{other}'"
                    )));
                }
            }
        }
        Ok(())
    }

    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
        context.require("script.execute")?;
        if let Some(tick_rate) = self.tick_rate_override {
            context.engine.world_mut().tick_rate = tick_rate;
        }
        Ok(())
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
