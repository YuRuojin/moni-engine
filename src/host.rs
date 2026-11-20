use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::engine::{Engine, Event, World};

pub const MAX_SYSTEM_TASKS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemStage {
    Startup,
    FixedUpdate,
    Render,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadAffinity {
    Logic,
    Render,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resource {
    World,
    Scripts,
    Gpu,
    Assets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub resource: Resource,
    pub mode: AccessMode,
}

impl Access {
    pub const fn read(resource: Resource) -> Self {
        Self {
            resource,
            mode: AccessMode::Read,
        }
    }

    pub const fn write(resource: Resource) -> Self {
        Self {
            resource,
            mode: AccessMode::Write,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemDescriptor {
    pub id: &'static str,
    pub plugin_id: &'static str,
    pub stage: SystemStage,
    pub affinity: ThreadAffinity,
    pub after: Vec<&'static str>,
    pub access: Vec<Access>,
}

#[derive(Debug)]
pub struct PluginError {
    message: String,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for PluginError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PluginError {}

#[derive(Debug, Clone, Copy)]
pub enum PluginOutput {
    None,
    Render(crate::gpu::RenderStatus),
}

#[derive(Default)]
pub struct PermissionPolicy {
    granted: BTreeMap<String, BTreeSet<String>>,
}

impl PermissionPolicy {
    fn grant(&mut self, plugin_id: &str, permissions: &[String]) {
        self.granted
            .insert(plugin_id.to_string(), permissions.iter().cloned().collect());
    }

    fn revoke(&mut self, plugin_id: &str) {
        self.granted.remove(plugin_id);
    }

    pub fn require(&self, plugin_id: &str, permission: &str) -> Result<(), PluginError> {
        if self
            .granted
            .get(plugin_id)
            .is_some_and(|permissions| permissions.contains(permission))
        {
            Ok(())
        } else {
            Err(PluginError::new(format!(
                "plugin '{plugin_id}' lacks required permission '{permission}'"
            )))
        }
    }
}

pub struct PluginContext<'a> {
    pub engine: &'a mut Engine,
    pub permissions: &'a PermissionPolicy,
    pub plugin_id: &'static str,
}

impl PluginContext<'_> {
    pub fn require(&self, permission: &str) -> Result<(), PluginError> {
        self.permissions.require(self.plugin_id, permission)
    }

    pub fn require_from(&self, plugin_id: &str, permission: &str) -> Result<(), PluginError> {
        self.permissions.require(plugin_id, permission)
    }
}

pub trait Plugin: Any {
    fn id(&self) -> &'static str;
    fn systems(&self) -> Vec<SystemDescriptor>;
    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError>;
    fn stop(&mut self, context: &mut PluginContext);
    fn execute(
        &mut self,
        stage: SystemStage,
        context: &mut PluginContext,
    ) -> Result<PluginOutput, PluginError>;
    fn resize(&mut self, _width: u32, _height: u32) {}
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub struct PluginHandle {
    id: &'static str,
}

impl PluginHandle {
    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn dispose(self, host: &mut PluginHost) -> Result<(), PluginError> {
        host.unregister(self.id)
    }
}

pub struct PluginHost {
    engine: Engine,
    plugins: BTreeMap<&'static str, Box<dyn Plugin>>,
    order: Vec<&'static str>,
    systems: Vec<SystemDescriptor>,
    queue: VecDeque<SystemDescriptor>,
    permissions: PermissionPolicy,
    last_render_status: Option<crate::gpu::RenderStatus>,
    shutdown: bool,
}

impl PluginHost {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            plugins: BTreeMap::new(),
            order: Vec::new(),
            systems: Vec::new(),
            queue: VecDeque::with_capacity(MAX_SYSTEM_TASKS),
            permissions: PermissionPolicy::default(),
            last_render_status: None,
            shutdown: false,
        }
    }

    pub fn world(&self) -> &World {
        self.engine.world()
    }

    pub fn world_mut(&mut self) -> &mut World {
        self.engine.world_mut()
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        self.engine.take_events()
    }

    pub fn grant_permissions(&mut self, plugin_id: &str, permissions: &[String]) {
        self.permissions.grant(plugin_id, permissions);
    }

    pub fn register_plugin_with_permissions(
        &mut self,
        mut plugin: Box<dyn Plugin>,
        granted_permissions: &[String],
    ) -> Result<PluginHandle, PluginError> {
        if self.shutdown {
            return Err(PluginError::new("cannot register a plugin after shutdown"));
        }
        let id = plugin.id();
        if self.plugins.contains_key(id) {
            return Err(PluginError::new(format!("duplicate plugin ID '{id}'")));
        }
        let systems = plugin.systems();
        if systems.iter().any(|system| system.plugin_id != id) {
            return Err(PluginError::new(format!(
                "plugin '{id}' declared a system owned by another plugin"
            )));
        }
        self.systems.extend(systems);
        if let Err(error) = validate_schedule(&self.systems) {
            self.systems.retain(|system| system.plugin_id != id);
            return Err(error);
        }
        self.permissions.grant(id, granted_permissions);
        let mut context = PluginContext {
            engine: &mut self.engine,
            permissions: &self.permissions,
            plugin_id: id,
        };
        if let Err(error) = plugin.start(&mut context) {
            self.systems.retain(|system| system.plugin_id != id);
            plugin.stop(&mut context);
            self.permissions.revoke(id);
            return Err(error);
        }
        self.plugins.insert(id, plugin);
        self.order.push(id);
        Ok(PluginHandle { id })
    }

    pub fn unregister(&mut self, id: &'static str) -> Result<(), PluginError> {
        let Some(plugin) = self.plugins.remove(id) else {
            return Err(PluginError::new(format!("plugin '{id}' is not registered")));
        };
        self.order.retain(|plugin_id| *plugin_id != id);
        self.systems.retain(|system| system.plugin_id != id);
        self.queue.retain(|system| system.plugin_id != id);
        let mut plugin = plugin;
        plugin.stop(&mut PluginContext {
            engine: &mut self.engine,
            permissions: &self.permissions,
            plugin_id: id,
        });
        self.permissions.revoke(id);
        Ok(())
    }

    pub fn run_stage(&mut self, stage: SystemStage) -> Result<(), PluginError> {
        if self.shutdown {
            return Err(PluginError::new("cannot run a stage after shutdown"));
        }
        let mut systems = sorted_systems(&self.systems, stage)?;
        if stage == SystemStage::Shutdown {
            systems.reverse();
        }
        self.queue.clear();
        for system in systems {
            if self.queue.len() == MAX_SYSTEM_TASKS {
                self.queue.clear();
                return Err(PluginError::new("system task queue capacity exceeded"));
            }
            self.queue.push_back(system);
        }
        while let Some(system) = self.queue.pop_front() {
            let plugin_id = system.plugin_id;
            let result = self
                .plugins
                .get_mut(plugin_id)
                .expect("registered system owner exists")
                .execute(
                    stage,
                    &mut PluginContext {
                        engine: &mut self.engine,
                        permissions: &self.permissions,
                        plugin_id,
                    },
                );
            match result {
                Ok(PluginOutput::Render(status)) => self.last_render_status = Some(status),
                Ok(PluginOutput::None) => {}
                Err(error) => {
                    self.queue.clear();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub fn script_host_count(&self) -> usize {
        self.engine.script_host_count()
    }

    pub fn has_plugin(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }

    pub fn last_render_status(&self) -> Option<crate::gpu::RenderStatus> {
        self.last_render_status
    }

    pub fn plugin_mut<T: Any>(&mut self, id: &'static str) -> Option<&mut T> {
        self.plugins.get_mut(id)?.as_any_mut().downcast_mut::<T>()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        for plugin in self.plugins.values_mut() {
            plugin.resize(width, height);
        }
    }

    pub fn shutdown(&mut self) -> Result<(), PluginError> {
        if self.shutdown {
            return Ok(());
        }
        let result = self.run_stage(SystemStage::Shutdown);
        for id in self.order.iter().rev() {
            if let Some(plugin) = self.plugins.get_mut(id) {
                plugin.stop(&mut PluginContext {
                    engine: &mut self.engine,
                    permissions: &self.permissions,
                    plugin_id: id,
                });
            }
        }
        self.plugins.clear();
        self.order.clear();
        self.systems.clear();
        self.queue.clear();
        self.permissions = PermissionPolicy::default();
        self.shutdown = true;
        result
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn validate_schedule(systems: &[SystemDescriptor]) -> Result<(), PluginError> {
    let mut ids = BTreeMap::new();
    for system in systems {
        if ids.insert(system.id, true).is_some() {
            return Err(PluginError::new(format!(
                "duplicate system ID '{}'",
                system.id
            )));
        }
        validate_affinity(system)?;
        for after in &system.after {
            let Some(parent) = systems.iter().find(|parent| parent.id == *after) else {
                return Err(PluginError::new(format!(
                    "system '{}' depends on unknown system '{after}'",
                    system.id
                )));
            };
            let _ = parent;
        }
        let mut access = system.access.clone();
        access.sort_by_key(|access| access.resource);
        if access
            .windows(2)
            .any(|pair| pair[0].resource == pair[1].resource)
        {
            return Err(PluginError::new(format!(
                "system '{}' declares duplicate resource access",
                system.id
            )));
        }
    }
    for (index, left) in systems.iter().enumerate() {
        for right in systems.iter().skip(index + 1) {
            if left.stage != right.stage || !accesses_conflict(left, right) {
                continue;
            }
            if !depends_on(systems, left.id, right.id) && !depends_on(systems, right.id, left.id) {
                return Err(PluginError::new(format!(
                    "systems '{}' and '{}' have unordered resource conflicts",
                    left.id, right.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_affinity(system: &SystemDescriptor) -> Result<(), PluginError> {
    let valid = !matches!(
        (system.stage, system.affinity),
        (SystemStage::FixedUpdate, ThreadAffinity::Render)
            | (SystemStage::Render, ThreadAffinity::Logic)
    );
    if valid {
        Ok(())
    } else {
        Err(PluginError::new(format!(
            "system '{}' has invalid thread affinity for its stage",
            system.id
        )))
    }
}

fn accesses_conflict(left: &SystemDescriptor, right: &SystemDescriptor) -> bool {
    left.access.iter().any(|left_access| {
        right.access.iter().any(|right_access| {
            left_access.resource == right_access.resource
                && (left_access.mode == AccessMode::Write || right_access.mode == AccessMode::Write)
        })
    })
}

fn depends_on(systems: &[SystemDescriptor], id: &str, ancestor: &str) -> bool {
    let mut pending = vec![id];
    let mut visited = BTreeMap::new();
    while let Some(current) = pending.pop() {
        if current == ancestor {
            return true;
        }
        if visited.insert(current, true).is_some() {
            continue;
        }
        if let Some(system) = systems.iter().find(|system| system.id == current) {
            pending.extend(system.after.iter().copied());
        }
    }
    false
}

fn sorted_systems(
    systems: &[SystemDescriptor],
    stage: SystemStage,
) -> Result<Vec<SystemDescriptor>, PluginError> {
    validate_schedule(systems)?;
    let mut states = BTreeMap::new();
    let mut output = Vec::new();
    for system in systems.iter().filter(|system| system.stage == stage) {
        visit_system(systems, system.id, stage, &mut states, &mut output)?;
    }
    Ok(output)
}

fn visit_system(
    systems: &[SystemDescriptor],
    id: &'static str,
    stage: SystemStage,
    states: &mut BTreeMap<&'static str, u8>,
    output: &mut Vec<SystemDescriptor>,
) -> Result<(), PluginError> {
    match states.get(id).copied().unwrap_or(0) {
        2 => return Ok(()),
        1 => {
            return Err(PluginError::new(format!(
                "dependency cycle includes '{id}'"
            )));
        }
        _ => {}
    }
    states.insert(id, 1);
    if let Some(system) = systems.iter().find(|system| system.id == id) {
        for after in &system.after {
            visit_system(systems, after, stage, states, output)?;
        }
        if system.stage == stage {
            output.push(system.clone());
        }
    }
    states.insert(id, 2);
    Ok(())
}
