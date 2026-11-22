use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::components::ComponentDescriptor;
use crate::engine::{ComponentMigrator, Engine, EngineError, Event, World};

pub const MAX_SYSTEM_TASKS: usize = 1_024;

/// Well-known plugin IDs. Core and the composition root share these string
/// contracts without sharing concrete types.
pub const CORE_SIMULATION: &str = "core.simulation";
pub const CORE_RENDER: &str = "core.render";
pub const CORE_FBX: &str = "core.fbx";
pub const CORE_EDITOR: &str = "core.editor";

/// Outcome of a render pass, reported back to the host without exposing any
/// renderer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStatus {
    Rendered,
    Reconfigure,
    Skip,
}

/// Screen-space quad rendered above the scene by the render plugin. Core
/// owns the data so editors can compose overlays without naming a renderer.
#[derive(Debug, Clone, Copy)]
pub struct OverlayQuad {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: [f32; 4],
}

/// Renderer capabilities the host and tools consume through the plugin trait,
/// so clients and editors never downcast to a concrete renderer type.
///
/// Editor UI frames cross this trait type-erased: the editor hands over
/// `&mut dyn Any` and the render plugin downcasts to the UI frame payload it
/// was built against. For an egui editor the payload is
/// `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)` — the frame's
/// tessellated primitives, its texture deltas, and the pixels-per-point scale
/// — because egui tessellation must run on the context that produced the
/// shapes (its font atlas normalizes glyph UVs). The payload type must match
/// the egui version the render plugin was compiled with; a mismatch — or a
/// renderer that is not started — degrades to `false`, meaning "no UI layer
/// this frame", and never panics. Core itself stays egui-free; only plugins
/// that opt into the hand-off name the concrete types.
pub trait RenderPluginApi {
    fn last_preparation_time(&self) -> Duration;
    fn loaded_mesh_vertices(&self) -> usize;
    fn gpu_timestamps_supported(&self) -> bool;
    fn take_gpu_samples(&mut self) -> Vec<(f32, f32)>;
    fn set_overlay_quads(&mut self, quads: Vec<OverlayQuad>);
    fn capture_next(&mut self, path: PathBuf);
    /// Hands one editor UI frame to the render plugin, which composites it as
    /// the last pass before present. Returns `true` when the frame was
    /// accepted and consumed; `false` when the payload is not the UI frame
    /// type this plugin understands or no renderer is running. Plugins that
    /// do not support UI compositing keep the `false` default.
    fn set_egui_frame(&mut self, frame: &mut dyn Any) -> bool {
        let _ = frame;
        false
    }
    /// Toggles the editor's viewport ground grid (an XZ line lattice at
    /// y = 0). Off by default; golden captures never enable it. This is the
    /// same opt-in tool-surface pattern as [`RenderPluginApi::set_egui_frame`]:
    /// plugins without a grid keep the `false` default, and a not-started
    /// renderer also reports `false`.
    fn set_grid_visible(&mut self, visible: bool) -> bool {
        let _ = visible;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    details: Option<Value>,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }

    /// Attaches structured JSON details (for example component IDs and usage
    /// counts) alongside the human-readable message.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
}

impl Display for PluginError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PluginError {}

impl From<EngineError> for PluginError {
    fn from(error: EngineError) -> Self {
        Self {
            message: error.message,
            details: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PluginOutput {
    None,
    Render(RenderStatus),
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

/// Access surface a plugin context is allowed to touch. Derived from the
/// executing system's declared accesses; `start`/`stop` lifecycle calls get
/// the full surface because registration itself is trusted code.
#[derive(Debug, Clone, Copy)]
pub struct ContextAccess {
    world_read: bool,
    world_write: bool,
    scripts_write: bool,
}

impl ContextAccess {
    pub fn from_accesses(accesses: &[Access]) -> Self {
        let mut resolved = Self {
            world_read: false,
            world_write: false,
            scripts_write: false,
        };
        for access in accesses {
            match (access.resource, access.mode) {
                (Resource::World, AccessMode::Read) => resolved.world_read = true,
                (Resource::World, AccessMode::Write) => {
                    resolved.world_read = true;
                    resolved.world_write = true;
                }
                (Resource::Scripts, AccessMode::Write) => resolved.scripts_write = true,
                _ => {}
            }
        }
        resolved
    }

    pub const fn full() -> Self {
        Self {
            world_read: true,
            world_write: true,
            scripts_write: true,
        }
    }
}

/// Name-addressable provider/consumer registry. Providers own entries;
/// consumers look values up by name and type and never depend on a concrete
/// providing plugin. The host refuses to remove a provider while consumers
/// still exist, or cascades the removal to those consumers first.
#[derive(Default)]
pub struct ServiceRegistry {
    entries: BTreeMap<String, ServiceEntry>,
}

struct ServiceEntry {
    provider: &'static str,
    value: Box<dyn Any>,
    consumers: BTreeSet<&'static str>,
}

impl ServiceRegistry {
    pub fn provide(
        &mut self,
        provider: &'static str,
        name: &str,
        value: Box<dyn Any>,
    ) -> Result<(), PluginError> {
        if self.entries.contains_key(name) {
            return Err(PluginError::new(format!(
                "service '{name}' is already provided by '{}'",
                self.entries[name].provider
            )));
        }
        self.entries.insert(
            name.to_string(),
            ServiceEntry {
                provider,
                value,
                consumers: BTreeSet::new(),
            },
        );
        Ok(())
    }

    /// Returns the service value for `consumer`, recording the dependency so
    /// provider removal can check (or cascade) consumers.
    pub fn query_mut<T: Any>(
        &mut self,
        consumer: &'static str,
        name: &str,
    ) -> Result<&mut T, PluginError> {
        let Some(entry) = self.entries.get_mut(name) else {
            return Err(PluginError::new(format!("unknown service '{name}'")));
        };
        let type_ok = entry.value.downcast_mut::<T>().is_some();
        if !type_ok {
            return Err(PluginError::new(format!(
                "service '{name}' does not provide the requested type"
            )));
        }
        entry.consumers.insert(consumer);
        Ok(entry.value.downcast_mut::<T>().expect("type checked above"))
    }

    fn remove_provider(&mut self, provider: &'static str) {
        self.entries.retain(|_, entry| entry.provider != provider);
    }

    /// Removes the single entry `name` only when `provider` still owns it;
    /// used for precise register-phase rollback.
    fn remove_provider_entry(&mut self, provider: &'static str, name: &str) {
        if self
            .entries
            .get(name)
            .is_some_and(|entry| entry.provider == provider)
        {
            self.entries.remove(name);
        }
    }

    fn remove_consumer(&mut self, consumer: &'static str) {
        for entry in self.entries.values_mut() {
            entry.consumers.remove(consumer);
        }
    }

    /// Plugin IDs consuming any service provided by the given provider.
    fn consumers_of_provider(&self, provider: &'static str) -> BTreeSet<&'static str> {
        self.entries
            .values()
            .filter(|entry| entry.provider == provider)
            .flat_map(|entry| entry.consumers.iter().copied())
            .collect()
    }

    /// (service name, provider ID) pairs, for capability reporting.
    pub fn descriptions(&self) -> Vec<(String, &'static str)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.provider))
            .collect()
    }
}

/// Side-effect classes one control command may touch. Surfaced in the
/// capabilities `commands` section and used to explain transaction limits;
/// native plugins are trusted code, so these are declarations, not a sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideEffects {
    pub file: bool,
    pub gpu: bool,
    pub script: bool,
    pub network: bool,
}

impl SideEffects {
    pub const NONE: Self = Self {
        file: false,
        gpu: false,
        script: false,
        network: false,
    };
    pub const FILE: Self = Self {
        file: true,
        gpu: false,
        script: false,
        network: false,
    };
    pub const SCRIPT: Self = Self {
        file: false,
        gpu: false,
        script: true,
        network: false,
    };
}

/// Unified metadata for one control-protocol command, shared by built-in
/// commands (static table in `protocol.rs`) and plugin-registered commands.
/// It drives transaction checks (batches, dry runs), permission enforcement,
/// and the generated capabilities `commands` section.
///
/// Plugin commands are conservative by default: the plain `register_command`
/// path yields `mutates_world = true`, `supports_batch = false`,
/// `supports_dry_run = false`, no side effects, and no permission. Declare
/// capabilities explicitly through `register_command_with`.
#[derive(Debug, Clone)]
pub struct CommandDescriptor {
    /// `"core"` for built-in commands, the plugin ID for plugin commands.
    /// Filled from the registering plugin's ID; the value set at construction
    /// is a placeholder.
    pub owner: &'static str,
    pub mutates_world: bool,
    /// `false` ⇒ rejected inside atomic batches (`invalid_batch_command`).
    pub supports_batch: bool,
    /// `false` ⇒ rejected inside dry-run batches (`dry_run_unsupported`).
    pub supports_dry_run: bool,
    pub side_effects: SideEffects,
    /// Enforced against the OWNER plugin's granted permissions at execute
    /// time; failure returns `permission_denied`.
    pub required_permission: Option<String>,
    pub args_schema: Value,
    /// `Value::Null` means "no documented returns" and is omitted from
    /// capabilities.
    pub returns_schema: Value,
}

impl CommandDescriptor {
    /// Conservative baseline for plugin-registered commands; see the struct
    /// documentation for the default flags.
    pub fn conservative(args_schema: Value) -> Self {
        Self {
            owner: "core",
            mutates_world: true,
            supports_batch: false,
            supports_dry_run: false,
            side_effects: SideEffects::NONE,
            required_permission: None,
            args_schema,
            returns_schema: Value::Null,
        }
    }

    pub fn mutates_world(mut self, mutates_world: bool) -> Self {
        self.mutates_world = mutates_world;
        self
    }

    pub fn batchable(mut self, supports_batch: bool) -> Self {
        self.supports_batch = supports_batch;
        self
    }

    pub fn dry_runnable(mut self, supports_dry_run: bool) -> Self {
        self.supports_dry_run = supports_dry_run;
        self
    }

    pub fn side_effects(mut self, side_effects: SideEffects) -> Self {
        self.side_effects = side_effects;
        self
    }

    pub fn required_permission(mut self, permission: impl Into<String>) -> Self {
        self.required_permission = Some(permission.into());
        self
    }

    pub fn returns_schema(mut self, returns_schema: Value) -> Self {
        self.returns_schema = returns_schema;
        self
    }
}

/// Commands a plugin contributes to the control protocol. Handlers receive
/// the engine at call time; registration names the owning plugin (in the
/// descriptor's `owner`) so unregistering removes its commands, and the
/// descriptor metadata surfaces in capabilities.
pub struct PluginCommand {
    pub descriptor: CommandDescriptor,
    handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
}

impl PluginCommand {
    pub fn new(
        descriptor: CommandDescriptor,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Self {
        Self {
            descriptor,
            handler,
        }
    }
}

#[derive(Default)]
pub struct PluginCommandRegistry {
    commands: BTreeMap<String, PluginCommand>,
}

impl PluginCommandRegistry {
    /// Registers a command with conservative descriptor defaults
    /// ([`CommandDescriptor::conservative`]); see `register_descriptor` for
    /// the shared rules.
    pub fn register(
        &mut self,
        owner: &'static str,
        name: &str,
        schema: Value,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        self.register_descriptor(
            owner,
            CommandDescriptor::conservative(schema),
            name,
            handler,
        )
    }

    /// Registers a command with full descriptor metadata. The descriptor's
    /// `owner` is overwritten with `owner`, so callers may pass any
    /// placeholder. Duplicate names and names that collide with built-in
    /// commands are refused with a clear error.
    pub fn register_descriptor(
        &mut self,
        owner: &'static str,
        descriptor: CommandDescriptor,
        name: &str,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        if self.commands.contains_key(name) {
            return Err(PluginError::new(format!(
                "command '{name}' is already registered"
            )));
        }
        if crate::protocol::builtin_commands().contains_key(name) {
            return Err(PluginError::new(format!(
                "command '{name}' collides with a built-in command; plugin commands must use a different name"
            )));
        }
        let mut descriptor = descriptor;
        descriptor.owner = owner;
        self.commands
            .insert(name.to_string(), PluginCommand::new(descriptor, handler));
        Ok(())
    }

    /// Whether a plugin command with this name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// The descriptor for `name`, if a plugin command is registered.
    pub fn descriptor(&self, name: &str) -> Option<&CommandDescriptor> {
        self.commands.get(name).map(|command| &command.descriptor)
    }

    fn remove_owner(&mut self, owner: &'static str) {
        self.commands
            .retain(|_, command| command.descriptor.owner != owner);
    }

    /// Removes the single command `name` only when `owner` still owns it;
    /// used for precise register-phase rollback.
    fn remove_owner_entry(&mut self, owner: &'static str, name: &str) {
        if self
            .commands
            .get(name)
            .is_some_and(|command| command.descriptor.owner == owner)
        {
            self.commands.remove(name);
        }
    }

    pub fn execute(
        &mut self,
        name: &str,
        engine: &mut Engine,
        args: &Value,
    ) -> Result<Value, EngineError> {
        let Some(command) = self.commands.get_mut(name) else {
            return Err(EngineError::new(
                "unknown_command",
                format!("unknown command '{name}'"),
            ));
        };
        (command.handler)(engine, args)
    }

    /// (name, descriptor) pairs for capability reporting.
    pub fn descriptions(&self) -> Vec<(String, &CommandDescriptor)> {
        self.commands
            .iter()
            .map(|(name, command)| (name.clone(), &command.descriptor))
            .collect()
    }
}

pub struct PluginContext<'a> {
    engine: &'a mut Engine,
    permissions: &'a PermissionPolicy,
    services: &'a mut ServiceRegistry,
    commands: &'a mut PluginCommandRegistry,
    plugin_id: &'static str,
    access: ContextAccess,
}

impl PluginContext<'_> {
    pub fn require(&self, permission: &str) -> Result<(), PluginError> {
        self.permissions.require(self.plugin_id, permission)
    }

    pub fn require_from(&self, plugin_id: &str, permission: &str) -> Result<(), PluginError> {
        self.permissions.require(plugin_id, permission)
    }

    /// Read-only world access; requires a declared World read or write.
    pub fn world(&self) -> Result<&World, PluginError> {
        if !self.access.world_read {
            return Err(access_denied(self.plugin_id, "read(world)"));
        }
        Ok(self.engine.world())
    }

    /// Mutable world access; requires a declared World write.
    pub fn world_mut(&mut self) -> Result<&mut World, PluginError> {
        if !self.access.world_write {
            return Err(access_denied(self.plugin_id, "write(world)"));
        }
        Ok(self.engine.world_mut())
    }

    /// Drives `count` fixed simulation ticks, which advances scripts and
    /// applies their commands. Requires declared write access to World and
    /// Scripts.
    pub fn run_ticks(&mut self, count: u64) -> Result<Value, PluginError> {
        if !self.access.world_write {
            return Err(access_denied(self.plugin_id, "write(world)"));
        }
        if !self.access.scripts_write {
            return Err(access_denied(self.plugin_id, "write(scripts)"));
        }
        self.engine
            .step(count)
            .map_err(|error| PluginError::new(format!("could not advance simulation: {error}")))
    }

    /// Clears script runtime state. Requires a declared Scripts write.
    pub fn shutdown_scripts(&mut self) {
        if self.access.scripts_write {
            self.engine.shutdown_runtime();
        }
    }

    /// Publishes a typed service under `name`. Fails if another provider
    /// already owns the name.
    pub fn provide_service<T: Any>(&mut self, name: &str, value: T) -> Result<(), PluginError> {
        self.services.provide(self.plugin_id, name, Box::new(value))
    }

    /// Borrows a service by name and type, recording this plugin as a
    /// consumer for provider-removal checks.
    pub fn service<T: Any>(&mut self, name: &str) -> Result<&mut T, PluginError> {
        self.services.query_mut::<T>(self.plugin_id, name)
    }

    /// Registers a control-plane command owned by this plugin.
    pub fn register_command(
        &mut self,
        name: &str,
        schema: Value,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        self.commands
            .register(self.plugin_id, name, schema, handler)
    }

    /// Registers a control-plane command with full descriptor metadata. The
    /// owner is filled from this plugin's ID; duplicate names and built-in
    /// command names are refused.
    pub fn register_command_with(
        &mut self,
        descriptor: CommandDescriptor,
        name: &str,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        self.commands
            .register_descriptor(self.plugin_id, descriptor, name, handler)
    }
}

/// Registration-phase contributions owned by one plugin. The host keeps one
/// record per registered plugin so a failed registration, a failed start, or
/// unregistration can revoke exactly what the plugin contributed. Services
/// and commands published later through `PluginContext` during `start` are
/// NOT recorded here; they are revoked wholesale through the provider/owner
/// sweep on the existing failure paths.
#[derive(Default)]
struct PluginContributions {
    components: Vec<&'static str>,
    services: Vec<String>,
    commands: Vec<String>,
    /// (component, from_version, to_version) triples for migrator cleanup.
    migrators: Vec<(&'static str, u32, u32)>,
}

/// Registration-phase surface handed to `Plugin::register`. It bundles the
/// engine, the service and command registries, the owning plugin ID, and the
/// plugin's contributions record; every successful call is recorded so a
/// later failure or unregistration revokes exactly what this plugin added.
pub struct PluginRegistry<'a> {
    engine: &'a mut Engine,
    services: &'a mut ServiceRegistry,
    commands: &'a mut PluginCommandRegistry,
    plugin_id: &'static str,
    contributions: &'a mut PluginContributions,
}

impl PluginRegistry<'_> {
    /// Registers a component descriptor in the engine registry and records it
    /// under this plugin's contributions. Duplicate component IDs fail; if a
    /// later call in the same `register` fails, the host rolls back every
    /// contribution recorded so far for this plugin.
    pub fn register_component(
        &mut self,
        descriptor: ComponentDescriptor,
    ) -> Result<(), PluginError> {
        let id = descriptor.id;
        self.engine.register_component(descriptor)?;
        self.contributions.components.push(id);
        Ok(())
    }

    /// Publishes a typed service under `name`, same semantics as
    /// [`PluginContext::provide_service`], and records it under this plugin.
    pub fn register_service<T: Any>(&mut self, name: &str, value: T) -> Result<(), PluginError> {
        self.services
            .provide(self.plugin_id, name, Box::new(value))?;
        self.contributions.services.push(name.to_string());
        Ok(())
    }

    /// Registers a control-plane command owned by this plugin, same
    /// semantics as [`PluginContext::register_command`], and records it.
    pub fn register_command(
        &mut self,
        name: &str,
        schema: Value,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        self.commands
            .register(self.plugin_id, name, schema, handler)?;
        self.contributions.commands.push(name.to_string());
        Ok(())
    }

    /// Registers a control-plane command with full descriptor metadata, same
    /// semantics as [`PluginContext::register_command_with`], and records it.
    /// The owner is filled from this plugin's ID; duplicate names and names
    /// colliding with built-in commands are refused.
    pub fn register_command_with(
        &mut self,
        descriptor: CommandDescriptor,
        name: &str,
        handler: Box<dyn FnMut(&mut Engine, &Value) -> Result<Value, EngineError>>,
    ) -> Result<(), PluginError> {
        self.commands
            .register_descriptor(self.plugin_id, descriptor, name, handler)?;
        self.contributions.commands.push(name.to_string());
        Ok(())
    }

    /// Registers a component data migrator with the engine and records it
    /// under this plugin's contributions.
    pub fn register_migrator(
        &mut self,
        component: &'static str,
        from_version: u32,
        to_version: u32,
        migrate: ComponentMigrator,
    ) -> Result<(), PluginError> {
        self.engine
            .register_component_migrator(component, from_version, to_version, migrate)?;
        self.contributions
            .migrators
            .push((component, from_version, to_version));
        Ok(())
    }
}

fn access_denied(plugin_id: &str, access: &str) -> PluginError {
    PluginError::new(format!(
        "plugin '{plugin_id}' lacks required access '{access}'; declare it on the owning system"
    ))
}

pub trait Plugin: Any {
    fn id(&self) -> &'static str;
    /// Registration phase: contribute component schemas, services, commands,
    /// and migrators. Called once per plugin before any start; must not
    /// depend on a loaded World. A failed call rolls back every contribution
    /// recorded during it, so `register` is all-or-nothing per plugin.
    fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
        let _ = registry;
        Ok(())
    }
    fn systems(&self) -> Vec<SystemDescriptor>;
    fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError>;
    fn stop(&mut self, context: &mut PluginContext);
    fn execute(
        &mut self,
        stage: SystemStage,
        context: &mut PluginContext,
    ) -> Result<PluginOutput, PluginError>;
    /// Applies configuration merged from ordered profile patches before
    /// `start`. Plugins without configuration support reject every input so
    /// a misdirected patch fails loudly.
    fn configure(&mut self, _config: &serde_json::Value) -> Result<(), PluginError> {
        Err(PluginError::new(format!(
            "plugin '{}' does not accept configuration",
            self.id()
        )))
    }
    fn resize(&mut self, _width: u32, _height: u32) {}
    /// Render plugins expose their tool surface here; everything else returns
    /// `None` so callers never downcast to concrete plugin types.
    fn as_render_api(&mut self) -> Option<&mut dyn RenderPluginApi> {
        None
    }
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Debug)]
pub struct PluginHandle {
    id: &'static str,
}

impl PluginHandle {
    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn dispose(self, host: &mut PluginHost) -> Result<Vec<&'static str>, PluginError> {
        host.unregister(self.id)
    }
}

pub struct PluginHost {
    engine: Engine,
    plugins: BTreeMap<&'static str, Box<dyn Plugin>>,
    order: Vec<&'static str>,
    /// Plugins that completed the start phase; only these run systems and
    /// receive `stop` on shutdown or removal.
    started: BTreeSet<&'static str>,
    systems: Vec<SystemDescriptor>,
    permissions: PermissionPolicy,
    services: ServiceRegistry,
    commands: PluginCommandRegistry,
    /// Registration-phase contributions per plugin, keyed by plugin ID.
    contributions: BTreeMap<&'static str, PluginContributions>,
    /// Validated, dependency-sorted systems per stage; rebuilt lazily after
    /// any registration change instead of on every stage run.
    schedule_cache: BTreeMap<SystemStage, Vec<SystemDescriptor>>,
    schedule_valid: bool,
    last_render_status: Option<RenderStatus>,
    shutdown: bool,
}

impl PluginHost {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            plugins: BTreeMap::new(),
            order: Vec::new(),
            started: BTreeSet::new(),
            systems: Vec::new(),
            permissions: PermissionPolicy::default(),
            services: ServiceRegistry::default(),
            commands: PluginCommandRegistry::default(),
            contributions: BTreeMap::new(),
            schedule_cache: BTreeMap::new(),
            schedule_valid: false,
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

    pub(crate) fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        self.engine.take_events()
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn grant_permissions(&mut self, plugin_id: &str, permissions: &[String]) {
        self.permissions.grant(plugin_id, permissions);
    }

    pub fn revoke_permissions(&mut self, plugin_id: &str) {
        self.permissions.revoke(plugin_id);
    }

    /// Registration phase: checks the plugin ID, grants permissions, and runs
    /// `Plugin::register` so the plugin can contribute component schemas,
    /// services, commands, and migrators. The plugin instance is stored but
    /// NOT started: its systems are not scheduled, it executes nothing, and
    /// shutdown will not call `stop` on it until [`PluginHost::start_plugin`]
    /// succeeds. On failure every contribution recorded during this call is
    /// revoked and permissions are removed, so the host keeps no trace of the
    /// plugin.
    pub fn register_plugin(
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
        self.permissions.grant(id, granted_permissions);
        self.contributions
            .insert(id, PluginContributions::default());
        let register_result = {
            let mut registry = PluginRegistry {
                engine: &mut self.engine,
                services: &mut self.services,
                commands: &mut self.commands,
                plugin_id: id,
                contributions: self
                    .contributions
                    .get_mut(&id)
                    .expect("contributions record was just inserted"),
            };
            plugin.register(&mut registry)
        };
        if let Err(error) = register_result {
            self.revoke_contributions(id);
            self.contributions.remove(id);
            self.permissions.revoke(id);
            return Err(error);
        }
        self.plugins.insert(id, plugin);
        self.order.push(id);
        Ok(PluginHandle { id })
    }

    /// Startup phase: validates and schedules the plugin's systems, snapshots
    /// the engine, then runs `Plugin::start` with full context access. On
    /// failure the plugin is stopped, the engine snapshot is restored, and the
    /// plugin's services, commands, components, migrators, systems, and
    /// permissions are removed, so the world and every registry end the call
    /// with the same content they started it (revoking components and
    /// migrators advances the revision, as any registration change does). The
    /// plugin instance itself stays registered so the caller can unregister
    /// it; to try again it must be unregistered and re-registered (which
    /// re-runs `register`).
    pub fn start_plugin(&mut self, id: &'static str) -> Result<(), PluginError> {
        if self.shutdown {
            return Err(PluginError::new("cannot start a plugin after shutdown"));
        }
        if !self.plugins.contains_key(id) {
            return Err(PluginError::new(format!("plugin '{id}' is not registered")));
        }
        if self.started.contains(id) {
            return Err(PluginError::new(format!(
                "plugin '{id}' is already started"
            )));
        }
        let systems = {
            let plugin = self.plugins.get_mut(id).expect("checked above");
            let systems = plugin.systems();
            if systems.iter().any(|system| system.plugin_id != id) {
                return Err(PluginError::new(format!(
                    "plugin '{id}' declared a system owned by another plugin"
                )));
            }
            systems
        };
        // Validate against the combined system set before touching shared
        // state so a rejected declaration cannot leave partial schedules.
        let mut combined = self.systems.clone();
        combined.extend(systems);
        if let Err(error) = validate_schedule(&combined) {
            return Err(error);
        }
        self.systems = combined;
        // Snapshot the engine so a failed start cannot leave world
        // modifications, events, or a moved revision behind.
        let snapshot = self.engine.snapshot();
        let start_result = {
            let plugin = self.plugins.get_mut(id).expect("checked above");
            let mut context = PluginContext {
                engine: &mut self.engine,
                permissions: &self.permissions,
                services: &mut self.services,
                commands: &mut self.commands,
                plugin_id: id,
                access: ContextAccess::full(),
            };
            plugin.start(&mut context)
        };
        if let Err(error) = start_result {
            {
                let plugin = self.plugins.get_mut(id).expect("checked above");
                let mut context = PluginContext {
                    engine: &mut self.engine,
                    permissions: &self.permissions,
                    services: &mut self.services,
                    commands: &mut self.commands,
                    plugin_id: id,
                    access: ContextAccess::full(),
                };
                plugin.stop(&mut context);
            }
            self.engine.restore(snapshot);
            self.services.remove_provider(id);
            self.commands.remove_owner(id);
            self.revoke_contributions(id);
            self.contributions.remove(id);
            self.systems.retain(|system| system.plugin_id != id);
            self.permissions.revoke(id);
            self.invalidate_schedule();
            return Err(error);
        }
        self.started.insert(id);
        self.invalidate_schedule();
        Ok(())
    }

    /// Stops a started plugin: the inverse of [`PluginHost::start_plugin`].
    /// Runs `Plugin::stop` with full context access, removes the plugin's
    /// systems from the schedule (invalidating the schedule cache), and marks
    /// it not-started. Registration-phase contributions (components,
    /// services, commands, migrators) and granted permissions remain: the
    /// plugin stays registered and can be started again with
    /// [`PluginHost::start_plugin`], which re-schedules its systems. An
    /// unknown ID fails with the `plugin_not_registered` semantics of
    /// [`PluginHost::start_plugin`], a registered-but-unstarted plugin with
    /// `plugin_not_started`; neither failure mutates the host.
    pub fn stop_plugin(&mut self, id: &'static str) -> Result<(), PluginError> {
        if !self.plugins.contains_key(id) {
            return Err(PluginError::new(format!("plugin '{id}' is not registered")));
        }
        if !self.started.remove(id) {
            return Err(PluginError::new(format!("plugin '{id}' is not started")));
        }
        self.systems.retain(|system| system.plugin_id != id);
        self.invalidate_schedule();
        let plugin = self.plugins.get_mut(id).expect("checked above");
        let mut context = PluginContext {
            engine: &mut self.engine,
            permissions: &self.permissions,
            services: &mut self.services,
            commands: &mut self.commands,
            plugin_id: id,
            access: ContextAccess::full(),
        };
        plugin.stop(&mut context);
        Ok(())
    }

    /// Combined convenience path: register, then start. On ANY failure the
    /// host is restored to its pre-call state — a failed registration already
    /// revokes its own contributions, and a failed start is followed by
    /// unregistering the registered-but-unstarted plugin — so existing tests
    /// and simple callers keep single-call semantics. Prefer the two-phase
    /// API when composition must load or validate state in between.
    pub fn register_plugin_with_permissions(
        &mut self,
        plugin: Box<dyn Plugin>,
        granted_permissions: &[String],
    ) -> Result<PluginHandle, PluginError> {
        let handle = self.register_plugin(plugin, granted_permissions)?;
        if let Err(error) = self.start_plugin(handle.id()) {
            let _ = self.unregister(handle.id());
            return Err(error);
        }
        Ok(handle)
    }

    /// Removal set for `id` plus every plugin that consumed a service it
    /// provided, directly or through the removed chain.
    fn cascade_removal_set(&self, id: &'static str) -> Result<Vec<&'static str>, PluginError> {
        if !self.plugins.contains_key(id) {
            return Err(PluginError::new(format!("plugin '{id}' is not registered")));
        }
        let mut removal = vec![id];
        let mut index = 0;
        while index < removal.len() {
            let provider = removal[index];
            index += 1;
            for consumer in self.services.consumers_of_provider(provider) {
                if self.plugins.contains_key(consumer) && !removal.contains(&consumer) {
                    removal.push(consumer);
                }
            }
        }
        Ok(removal)
    }

    /// (component ID, entity count) pairs for components contributed by the
    /// removal set that are still attached to at least one entity. Read-only,
    /// so callers can refuse removal before mutating anything.
    fn component_usage_conflicts(&self, removal: &[&'static str]) -> Vec<(&'static str, usize)> {
        let mut conflicts = Vec::new();
        for plugin_id in removal {
            let Some(record) = self.contributions.get(plugin_id) else {
                continue;
            };
            for component in &record.components {
                let users = self
                    .engine
                    .world()
                    .entities
                    .values()
                    .filter(|entity| entity.extensions.contains_key(*component))
                    .count();
                if users > 0 {
                    conflicts.push((*component, users));
                }
            }
        }
        conflicts
    }

    /// Removes a plugin and, before it, every plugin that consumed a service
    /// it provided (directly or through the removed chain). Returns the IDs
    /// actually removed, dependents first, `id` last.
    ///
    /// Registration-phase contributions travel with their owner: if any
    /// contributed component of the removal set is still attached to an
    /// entity, the whole unregister is refused with a structured error naming
    /// each component and its usage count, and nothing is removed. Unstarted
    /// plugins unregister without calling `stop`.
    pub fn unregister(&mut self, id: &'static str) -> Result<Vec<&'static str>, PluginError> {
        let removal = self.cascade_removal_set(id)?;
        let conflicts = self.component_usage_conflicts(&removal);
        if !conflicts.is_empty() {
            let listing = conflicts
                .iter()
                .map(|(component, count)| format!("{component} ({count} entities)"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(PluginError::new(format!(
                "cannot unregister plugins [{}]: contributed components are still attached: {listing}",
                removal.join(", ")
            ))
            .with_details(json!({
                "plugins": removal,
                "components": conflicts.iter().map(|(component, count)| json!({
                    "component": component,
                    "usageCount": count,
                })).collect::<Vec<_>>(),
            })));
        }
        for dependent in &removal[1..] {
            self.remove_one(dependent);
        }
        self.remove_one(id);
        Ok(removal)
    }

    /// Like [`PluginHost::unregister`], but force-strips every contributed
    /// component of the removal set from its entities first (emitting
    /// `ComponentRemoved` events through the engine registry path) so the
    /// unregister proceeds. Use this for teardown paths that must succeed
    /// even with live attachments.
    pub fn unregister_forcing(
        &mut self,
        id: &'static str,
    ) -> Result<Vec<&'static str>, PluginError> {
        let removal = self.cascade_removal_set(id)?;
        for plugin_id in &removal {
            let components = self
                .contributions
                .get(plugin_id)
                .map(|record| record.components.clone())
                .unwrap_or_default();
            for component in components {
                let _ = self.engine.unregister_component(component, true);
            }
        }
        for dependent in &removal[1..] {
            self.remove_one(dependent);
        }
        self.remove_one(id);
        Ok(removal)
    }

    /// Whether the plugin completed the start phase. Registered-but-unstarted
    /// plugins contribute schemas and services but run no systems.
    pub fn is_started(&self, id: &str) -> bool {
        self.started.contains(id)
    }

    /// Removes every contribution recorded for `id`: recorded migrators, then
    /// dynamic components via the force path (the plugin owns them, so any
    /// straggling attachment is stripped instead of leaving half-removed
    /// state), then the precisely recorded service and command names. Used
    /// for register-phase rollback, start rollback, and unregistration.
    fn revoke_contributions(&mut self, id: &'static str) {
        let Some(record) = self.contributions.get(id) else {
            return;
        };
        let migrator_components: Vec<&'static str> = record
            .migrators
            .iter()
            .map(|(component, _, _)| *component)
            .collect();
        let components = record.components.clone();
        let services = record.services.clone();
        let commands = record.commands.clone();
        for component in &migrator_components {
            self.engine.unregister_component_migrators(component);
        }
        for component in &components {
            let _ = self.engine.unregister_component(component, true);
        }
        for name in &services {
            self.services.remove_provider_entry(id, name);
        }
        for name in &commands {
            self.commands.remove_owner_entry(id, name);
        }
    }

    fn remove_one(&mut self, id: &'static str) {
        let Some(mut plugin) = self.plugins.remove(id) else {
            return;
        };
        // Only started plugins ever ran, so only they receive `stop`.
        let started = self.started.remove(id);
        self.order.retain(|plugin_id| *plugin_id != id);
        self.systems.retain(|system| system.plugin_id != id);
        self.invalidate_schedule();
        if started {
            let mut context = PluginContext {
                engine: &mut self.engine,
                permissions: &self.permissions,
                services: &mut self.services,
                commands: &mut self.commands,
                plugin_id: id,
                access: ContextAccess::full(),
            };
            plugin.stop(&mut context);
        }
        self.commands.remove_owner(id);
        self.services.remove_provider(id);
        self.services.remove_consumer(id);
        self.permissions.revoke(id);
        self.revoke_contributions(id);
        self.contributions.remove(id);
    }

    /// Services published through plugin contexts as (name, provider) pairs.
    pub fn service_descriptions(&self) -> Vec<(String, &'static str)> {
        self.services.descriptions()
    }

    /// Plugin-registered commands as (name, descriptor) pairs.
    pub fn plugin_command_descriptions(&self) -> Vec<(String, &CommandDescriptor)> {
        self.commands.descriptions()
    }

    /// Whether a plugin-registered command with this name exists.
    pub fn has_plugin_command(&self, name: &str) -> bool {
        self.commands.contains(name)
    }

    /// The descriptor for a plugin-registered command, if any.
    pub fn plugin_command_descriptor(&self, name: &str) -> Option<&CommandDescriptor> {
        self.commands.descriptor(name)
    }

    /// Enforces a command descriptor's `required_permission` against the
    /// owner plugin's granted permissions. Commands without a declared
    /// permission always pass.
    fn enforce_command_permission(&self, name: &str) -> Result<(), EngineError> {
        let Some(descriptor) = self.commands.descriptor(name) else {
            return Ok(());
        };
        let Some(permission) = &descriptor.required_permission else {
            return Ok(());
        };
        self.permissions
            .require(descriptor.owner, permission)
            .map_err(|error| {
                EngineError::new("permission_denied", error.to_string()).with_details(json!({
                    "plugin": descriptor.owner,
                    "permission": permission,
                }))
            })
    }

    /// Executes a plugin-registered control command by name. A declared
    /// `required_permission` is checked against the owner plugin's grants
    /// before the handler runs.
    pub fn execute_plugin_command(
        &mut self,
        name: &str,
        args: &Value,
    ) -> Result<Value, EngineError> {
        self.enforce_command_permission(name)?;
        self.commands.execute(name, &mut self.engine, args)
    }

    /// Executes a plugin-registered control command against an arbitrary
    /// engine (the dry-run scratch world), with the same permission
    /// enforcement as [`PluginHost::execute_plugin_command`]. Handlers only
    /// receive the engine, so world mutations land on the scratch copy while
    /// host-managed runtime state stays untouched.
    pub fn execute_plugin_command_on(
        &mut self,
        name: &str,
        engine: &mut Engine,
        args: &Value,
    ) -> Result<Value, EngineError> {
        self.enforce_command_permission(name)?;
        self.commands.execute(name, engine, args)
    }

    pub fn run_stage(&mut self, stage: SystemStage) -> Result<(), PluginError> {
        if self.shutdown {
            return Err(PluginError::new("cannot run a stage after shutdown"));
        }
        if !self.schedule_valid {
            self.schedule_cache.clear();
            self.schedule_valid = true;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.schedule_cache.entry(stage)
        {
            let sorted = sorted_systems(&self.systems, stage)?;
            entry.insert(sorted);
        }
        // Shutdown runs once, so copying and reversing is fine; hot stages
        // iterate the cached schedule directly without rebuilding it.
        let mut reversed;
        let systems: &[SystemDescriptor] = if stage == SystemStage::Shutdown {
            reversed = self
                .schedule_cache
                .get(&stage)
                .expect("stage schedule was just ensured")
                .clone();
            reversed.reverse();
            &reversed
        } else {
            self.schedule_cache
                .get(&stage)
                .expect("stage schedule was just ensured")
                .as_slice()
        };
        if systems.len() > MAX_SYSTEM_TASKS {
            return Err(PluginError::new("system task queue capacity exceeded"));
        }
        for system in systems {
            let plugin_id = system.plugin_id;
            let access = ContextAccess::from_accesses(&system.access);
            let result = self
                .plugins
                .get_mut(plugin_id)
                .expect("registered system owner exists")
                .execute(
                    stage,
                    &mut PluginContext {
                        engine: &mut self.engine,
                        permissions: &self.permissions,
                        services: &mut self.services,
                        commands: &mut self.commands,
                        plugin_id,
                        access,
                    },
                );
            match result {
                Ok(PluginOutput::Render(status)) => self.last_render_status = Some(status),
                Ok(PluginOutput::None) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn invalidate_schedule(&mut self) {
        self.schedule_valid = false;
    }

    pub fn script_host_count(&self) -> usize {
        self.engine.script_host_count()
    }

    pub fn has_plugin(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }

    /// Registered plugin IDs in registration order.
    pub fn plugin_ids(&self) -> Vec<&'static str> {
        self.order.clone()
    }

    pub fn last_render_status(&self) -> Option<RenderStatus> {
        self.last_render_status
    }

    pub fn plugin_mut<T: Any>(&mut self, id: &'static str) -> Option<&mut T> {
        self.plugins.get_mut(id)?.as_any_mut().downcast_mut::<T>()
    }

    /// Renderer tool surface for the plugin with the given ID, without
    /// exposing the concrete plugin type to callers.
    pub fn render_api_mut(&mut self, id: &str) -> Option<&mut dyn RenderPluginApi> {
        self.plugins.get_mut(id)?.as_render_api()
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
        // Only started plugins ever ran systems or acquired runtime
        // resources, so only they receive `stop`; registered-but-unstarted
        // plugins are dropped without lifecycle calls.
        for id in self.order.iter().rev() {
            if !self.started.contains(id) {
                continue;
            }
            if let Some(plugin) = self.plugins.get_mut(id) {
                plugin.stop(&mut PluginContext {
                    engine: &mut self.engine,
                    permissions: &self.permissions,
                    services: &mut self.services,
                    commands: &mut self.commands,
                    plugin_id: id,
                    access: ContextAccess::full(),
                });
            }
        }
        self.plugins.clear();
        self.order.clear();
        self.started.clear();
        self.systems.clear();
        self.services = ServiceRegistry::default();
        self.commands = PluginCommandRegistry::default();
        self.contributions.clear();
        self.schedule_cache.clear();
        self.schedule_valid = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ComponentDescriptor, PropertyField, PropertySchema, PropertyType};
    use crate::engine::{Command, Engine};
    use crate::plugins::SimulationPlugin;
    use serde_json::json;

    #[test]
    fn duplicate_plugin_registration_is_rejected() {
        let mut host = PluginHost::new(Engine::new());
        let permissions = ["script.execute".to_string()];
        let first =
            host.register_plugin_with_permissions(Box::new(SimulationPlugin::new()), &permissions);
        assert!(first.is_ok());
        let second =
            host.register_plugin_with_permissions(Box::new(SimulationPlugin::new()), &permissions);
        assert!(second.is_err(), "duplicate plugin ID must be rejected");
        let mut handles = vec![first.expect("first registration")];
        while let Some(handle) = handles.pop() {
            let removed = handle.dispose(&mut host).expect("dispose plugin");
            assert_eq!(removed, vec!["core.simulation"]);
        }
        assert!(!host.has_plugin("core.simulation"));
    }

    #[test]
    fn permission_grants_can_be_revoked() {
        let mut host = PluginHost::new(Engine::new());
        host.grant_permissions("core.fbx", &["asset.read.workspace".to_string()]);
        assert!(
            host.permissions
                .require("core.fbx", "asset.read.workspace")
                .is_ok()
        );
        host.revoke_permissions("core.fbx");
        assert!(
            host.permissions
                .require("core.fbx", "asset.read.workspace")
                .is_err()
        );
    }

    /// Test double: publishes a service and optionally mutates the world
    /// before failing its start.
    struct ProviderPlugin {
        fail_start: bool,
    }

    #[derive(Debug, Default)]
    struct ProviderState {
        value: u64,
    }

    impl Plugin for ProviderPlugin {
        fn id(&self) -> &'static str {
            "test.provider"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            context.provide_service("test.state", ProviderState { value: 7 })?;
            if self.fail_start {
                context.world_mut()?.tick_rate = 11;
                return Err(PluginError::new("provider start failure"));
            }
            Ok(())
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            _context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Test double: consumes the provider's service in start and execute.
    /// `service_name` defaults to `test.state`; cascade tests point it at the
    /// contribution plugin's registered service instead.
    struct ConsumerPlugin {
        consumed_in_start: Option<u64>,
        service_name: &'static str,
    }

    impl Plugin for ConsumerPlugin {
        fn id(&self) -> &'static str {
            "test.consumer"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            vec![SystemDescriptor {
                id: "test.consumer.fixed_update",
                plugin_id: "test.consumer",
                stage: SystemStage::FixedUpdate,
                affinity: ThreadAffinity::Logic,
                after: vec![],
                access: vec![Access::write(Resource::World)],
            }]
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            let state = context.service::<ProviderState>(self.service_name)?;
            self.consumed_in_start = Some(state.value);
            Ok(())
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            stage: SystemStage,
            context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            if stage == SystemStage::FixedUpdate {
                let state = context.service::<ProviderState>(self.service_name)?;
                state.value += 1;
                context.world_mut()?.tick += 1;
            }
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Test double: registers a control command during start.
    struct CommandPlugin;

    impl Plugin for CommandPlugin {
        fn id(&self) -> &'static str {
            "test.commands"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            context.register_command(
                "test.ping",
                json!({ "args": {} }),
                Box::new(|engine, _args| {
                    Ok(json!({ "tick": engine.world().tick, "entities": engine.world().entities.len() }))
                }),
            )
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            _context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Test double: declares no world access but writes the world anyway.
    struct UndeclaredAccessPlugin;

    impl Plugin for UndeclaredAccessPlugin {
        fn id(&self) -> &'static str {
            "test.undeclared"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            vec![SystemDescriptor {
                id: "test.undeclared.fixed_update",
                plugin_id: "test.undeclared",
                stage: SystemStage::FixedUpdate,
                affinity: ThreadAffinity::Logic,
                after: vec![],
                access: vec![],
            }]
        }

        fn start(&mut self, _context: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            context.world_mut()?.tick += 1;
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn services_connect_provider_and_consumer_without_type_leaks() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: false }), &[])
            .expect("provider registers");
        host.register_plugin_with_permissions(
            Box::new(ConsumerPlugin {
                consumed_in_start: None,
                service_name: "test.state",
            }),
            &[],
        )
        .expect("consumer registers");

        host.run_stage(SystemStage::FixedUpdate)
            .expect("fixed update");
        let descriptions = host.service_descriptions();
        assert_eq!(
            descriptions,
            vec![("test.state".to_string(), "test.provider" as &'static str)]
        );
    }

    #[test]
    fn removing_a_provider_cascades_to_consumers() {
        let mut host = PluginHost::new(Engine::new());
        let provider = host
            .register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: false }), &[])
            .expect("provider registers");
        let consumer = host
            .register_plugin_with_permissions(
                Box::new(ConsumerPlugin {
                    consumed_in_start: None,
                    service_name: "test.state",
                }),
                &[],
            )
            .expect("consumer registers");
        assert!(host.has_plugin("test.provider"));
        assert!(host.has_plugin("test.consumer"));

        let removed = host
            .unregister("test.provider")
            .expect("cascade unregister");
        assert_eq!(removed, vec!["test.provider", "test.consumer"]);
        assert!(!host.has_plugin("test.provider"));
        assert!(!host.has_plugin("test.consumer"));
        assert!(host.service_descriptions().is_empty());

        // After the cascade, both can be re-registered cleanly.
        host.register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: false }), &[])
            .expect("provider re-registers");
        let _ = (provider, consumer);
    }

    #[test]
    fn consumer_claims_release_when_the_consumer_is_removed() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: false }), &[])
            .expect("provider registers");
        host.register_plugin_with_permissions(
            Box::new(ConsumerPlugin {
                consumed_in_start: None,
                service_name: "test.state",
            }),
            &[],
        )
        .expect("consumer registers");
        let removed = host.unregister("test.consumer").expect("consumer removal");
        assert_eq!(removed, vec!["test.consumer"]);
        // The provider now has no consumers and can be removed directly.
        let removed = host.unregister("test.provider").expect("provider removal");
        assert_eq!(removed, vec!["test.provider"]);
    }

    #[test]
    fn failed_start_leaves_world_services_and_permissions_untouched() {
        let mut host = PluginHost::new(Engine::new());
        let revision_before = host.engine().revision();
        let events_before = host.take_events().len();
        let error = host
            .register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: true }), &[])
            .expect_err("failing start must reject registration");
        assert!(error.message.contains("provider start failure"));
        assert!(!host.has_plugin("test.provider"));
        assert!(host.service_descriptions().is_empty());
        assert_eq!(
            host.world().tick_rate,
            60,
            "world mutation during failed start is reverted"
        );
        assert_eq!(host.engine().revision(), revision_before);
        assert_eq!(
            host.take_events().len(),
            events_before,
            "no events leak from failed start"
        );

        // The same plugin registers cleanly once it stops failing.
        host.register_plugin_with_permissions(Box::new(ProviderPlugin { fail_start: false }), &[])
            .expect("provider registers after rollback");
    }

    #[test]
    fn undeclared_world_access_is_rejected_at_runtime() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(Box::new(UndeclaredAccessPlugin), &[])
            .expect("plugin registers");
        let error = host
            .run_stage(SystemStage::FixedUpdate)
            .expect_err("undeclared world write must be rejected");
        assert!(error.message.contains("lacks required access"));
        assert_eq!(
            host.world().tick,
            0,
            "rejected system must not mutate the world"
        );
    }

    #[test]
    fn plugin_commands_register_execute_and_unregister() {
        let mut host = PluginHost::new(Engine::new());
        let handle = host
            .register_plugin_with_permissions(Box::new(CommandPlugin), &[])
            .expect("command plugin registers");
        let result = host
            .execute_plugin_command("test.ping", &json!({}))
            .expect("plugin command executes");
        assert_eq!(result["entities"], json!(0));
        let descriptions = host.plugin_command_descriptions();
        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0].0, "test.ping");
        assert_eq!(descriptions[0].1.owner, "test.commands");
        // The plain 3-arg path yields the conservative descriptor defaults.
        assert!(descriptions[0].1.mutates_world);
        assert!(!descriptions[0].1.supports_batch);
        assert!(!descriptions[0].1.supports_dry_run);
        assert_eq!(descriptions[0].1.side_effects, SideEffects::NONE);
        handle.dispose(&mut host).expect("dispose");
        assert!(
            host.execute_plugin_command("test.ping", &json!({}))
                .is_err(),
            "unregistering the owner must remove its commands"
        );
    }

    /// Test double: registers a command whose name collides with a built-in
    /// command, which registration must refuse.
    struct BuiltinCollisionPlugin;

    impl Plugin for BuiltinCollisionPlugin {
        fn id(&self) -> &'static str {
            "test.collision"
        }

        fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
            registry.register_command(
                "spawn",
                json!({ "args": {} }),
                Box::new(|_engine, _args| Ok(json!({}))),
            )
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, _context: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            _context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn plugin_commands_cannot_shadow_builtin_names() {
        let mut host = PluginHost::new(Engine::new());
        let error = host
            .register_plugin_with_permissions(Box::new(BuiltinCollisionPlugin), &[])
            .expect_err("built-in name collisions must be refused");
        assert!(
            error.message.contains("built-in"),
            "unexpected error: {error}"
        );
        assert!(
            !host.has_plugin("test.collision"),
            "a refused registration must leave no trace"
        );
    }

    /// Test double: registers a fully described batchable command and a
    /// permission-guarded command during start.
    struct DescriptorPlugin;

    impl Plugin for DescriptorPlugin {
        fn id(&self) -> &'static str {
            "test.descriptor"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            context.register_command_with(
                CommandDescriptor::conservative(json!({ "delta": "number" }))
                    .batchable(true)
                    .returns_schema(json!(["tick"])),
                "test.descriptor.bump",
                Box::new(|engine, _args| {
                    engine.world_mut().tick += 1;
                    Ok(json!({ "tick": engine.world().tick }))
                }),
            )?;
            context.register_command_with(
                CommandDescriptor::conservative(json!({})).required_permission("test.command.run"),
                "test.descriptor.guarded",
                Box::new(|_engine, _args| Ok(json!({ "pong": true }))),
            )
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            _context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn command_descriptors_carry_owner_and_declared_metadata() {
        let mut host = PluginHost::new(Engine::new());
        let handle = host
            .register_plugin_with_permissions(Box::new(DescriptorPlugin), &[])
            .expect("descriptor plugin registers");
        let descriptions = host.plugin_command_descriptions();
        assert_eq!(descriptions.len(), 2);
        let bump = descriptions
            .iter()
            .find(|(name, _)| name == "test.descriptor.bump")
            .expect("batchable command registered");
        assert_eq!(
            bump.1.owner, "test.descriptor",
            "owner is filled at registration"
        );
        assert!(bump.1.supports_batch);
        assert_eq!(bump.1.returns_schema, json!(["tick"]));
        handle.dispose(&mut host).expect("dispose");
        assert!(host.plugin_command_descriptions().is_empty());
    }

    #[test]
    fn guarded_commands_enforce_the_owner_permission() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(Box::new(DescriptorPlugin), &[])
            .expect("registers without the grant");
        let error = host
            .execute_plugin_command("test.descriptor.guarded", &json!({}))
            .expect_err("missing permission must be denied");
        assert_eq!(error.code, "permission_denied");
        assert!(
            error.message.contains("test.command.run"),
            "the error names the missing permission: {error}"
        );
        assert_eq!(
            error.details.expect("details")["permission"],
            json!("test.command.run")
        );

        // With the grant in place the command executes normally.
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(
            Box::new(DescriptorPlugin),
            &["test.command.run".to_string()],
        )
        .expect("registers with the grant");
        let result = host
            .execute_plugin_command("test.descriptor.guarded", &json!({}))
            .expect("granted command executes");
        assert_eq!(result["pong"], json!(true));
    }

    #[test]
    fn schedule_cache_follows_registration_changes() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin_with_permissions(
            Box::new(SimulationPlugin::new()),
            &["script.execute".to_string()],
        )
        .expect("simulation registers");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("first run builds cache");
        let tick_after_first = host.world().tick;
        assert_eq!(tick_after_first, 1);

        // Registering again invalidates the cache but keeps scheduling correct.
        host.run_stage(SystemStage::FixedUpdate)
            .expect("second run uses cache");
        assert_eq!(host.world().tick, 2);

        let handle = host
            .register_plugin_with_permissions(Box::new(CommandPlugin), &[])
            .expect("registration invalidates cache");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("run after invalidation");
        assert_eq!(host.world().tick, 3);
        handle.dispose(&mut host).expect("dispose");
    }

    /// Test descriptor for registration-phase contributions.
    fn test_descriptor(id: &'static str, version: u32) -> ComponentDescriptor {
        ComponentDescriptor::dynamic(
            id,
            None,
            "test.owner",
            version,
            PropertySchema::new(
                "Test",
                "Dynamic test component",
                vec![PropertyField::new("hp", PropertyType::Number, false).default(json!(10))],
            ),
        )
    }

    /// Test double: contributes a component, service, command, and migrator
    /// during the registration phase, runs a world-writing system once
    /// started, and marks the world in `stop` so tests can observe whether
    /// stop ran.
    struct ContributionPlugin {
        fail_start: bool,
    }

    impl Plugin for ContributionPlugin {
        fn id(&self) -> &'static str {
            "test.contrib"
        }

        fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
            registry.register_component(test_descriptor("test.contrib.health", 2))?;
            registry.register_service("test.contrib.state", ProviderState { value: 3 })?;
            registry.register_command(
                "test.contrib.ping",
                json!({ "args": {} }),
                Box::new(|_engine, _args| Ok(json!({ "pong": true }))),
            )?;
            registry.register_migrator(
                "test.contrib.health",
                1,
                2,
                Box::new(|value| {
                    let mut value = value.clone();
                    value["hp"] = json!(value["hp"].as_i64().unwrap_or(0) + 1);
                    Ok(value)
                }),
            )?;
            Ok(())
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            vec![SystemDescriptor {
                id: "test.contrib.fixed_update",
                plugin_id: "test.contrib",
                stage: SystemStage::FixedUpdate,
                affinity: ThreadAffinity::Logic,
                after: vec![],
                access: vec![Access::write(Resource::World)],
            }]
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            if self.fail_start {
                context.provide_service("test.contrib.runtime", ProviderState { value: 9 })?;
                context.world_mut()?.tick_rate = 11;
                return Err(PluginError::new("contrib start failure"));
            }
            Ok(())
        }

        fn stop(&mut self, context: &mut PluginContext) {
            if let Ok(world) = context.world_mut() {
                world.tick_rate = 1;
            }
        }

        fn execute(
            &mut self,
            stage: SystemStage,
            context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            if stage == SystemStage::FixedUpdate {
                context.world_mut()?.tick += 1;
            }
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Test double: succeeds registering a component and service, then fails
    /// on a duplicate component ID to verify all-or-nothing register
    /// rollback.
    struct DuplicateRegisterPlugin;

    impl Plugin for DuplicateRegisterPlugin {
        fn id(&self) -> &'static str {
            "test.duplicate"
        }

        fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
            registry.register_component(test_descriptor("test.duplicate.health", 1))?;
            registry.register_service("test.duplicate.state", ProviderState { value: 1 })?;
            registry.register_component(test_descriptor("test.duplicate.health", 1))
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, _context: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn stop(&mut self, _context: &mut PluginContext) {}

        fn execute(
            &mut self,
            _stage: SystemStage,
            _context: &mut PluginContext,
        ) -> Result<PluginOutput, PluginError> {
            Ok(PluginOutput::None)
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn two_phase_contributions_are_visible_before_start() {
        let mut host = PluginHost::new(Engine::new());
        let handle = host
            .register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        assert!(
            !host.is_started("test.contrib"),
            "register must not start the plugin"
        );
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_some(),
            "component schema is visible before start"
        );
        assert_eq!(
            host.service_descriptions(),
            vec![(
                "test.contrib.state".to_string(),
                "test.contrib" as &'static str
            )]
        );
        assert_eq!(host.plugin_command_descriptions().len(), 1);

        host.start_plugin("test.contrib")
            .expect("start phase succeeds");
        assert!(host.is_started("test.contrib"));
        host.run_stage(SystemStage::FixedUpdate)
            .expect("started systems execute");
        assert_eq!(host.world().tick, 1);

        handle.dispose(&mut host).expect("dispose");
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_none(),
            "unregister removes the contributed component"
        );
        assert!(host.service_descriptions().is_empty());
    }

    #[test]
    fn run_stage_executes_only_started_plugins() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("stages run without started systems");
        assert_eq!(host.world().tick, 0, "unstarted plugin systems never run");

        host.start_plugin("test.contrib").expect("start");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("started systems execute");
        assert_eq!(host.world().tick, 1);
    }

    /// Stopping a started plugin keeps it registered with every
    /// registration-phase contribution and permission, leaves it out of the
    /// schedule, and a following start re-executes its FixedUpdate systems.
    #[test]
    fn stop_plugin_round_trip_restarts_fixed_update() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin(
            Box::new(ContributionPlugin { fail_start: false }),
            &["test.permission".to_string()],
        )
        .expect("register phase succeeds");
        host.start_plugin("test.contrib").expect("start");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("started systems execute");
        assert_eq!(host.world().tick, 1);

        host.stop_plugin("test.contrib").expect("stop");
        assert!(host.has_plugin("test.contrib"));
        assert!(!host.is_started("test.contrib"));
        // Contributions and permissions remain after the stop.
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_some()
        );
        assert_eq!(
            host.service_descriptions(),
            vec![(
                "test.contrib.state".to_string(),
                "test.contrib" as &'static str
            )]
        );
        assert_eq!(host.plugin_command_descriptions().len(), 1);
        assert!(
            host.permissions
                .require("test.contrib", "test.permission")
                .is_ok()
        );
        // The plugin's own stop hook ran (ContributionPlugin marks the world).
        assert_eq!(host.world().tick_rate, 1);

        // The stopped systems leave the schedule: the stage runs empty.
        host.run_stage(SystemStage::FixedUpdate)
            .expect("stage runs without the stopped systems");
        assert_eq!(host.world().tick, 1, "stopped plugin systems never run");

        // Restarting re-executes FixedUpdate without re-registration.
        host.start_plugin("test.contrib").expect("restart");
        host.run_stage(SystemStage::FixedUpdate)
            .expect("restarted systems execute");
        assert_eq!(host.world().tick, 2);
    }

    #[test]
    fn stop_plugin_rejects_unknown_unstarted_and_double_stop() {
        let mut host = PluginHost::new(Engine::new());
        let error = host
            .stop_plugin("test.contrib")
            .expect_err("an unknown ID must fail");
        assert!(error.message.contains("not registered"), "{error}");

        host.register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        let error = host
            .stop_plugin("test.contrib")
            .expect_err("a registered-but-unstarted plugin must fail");
        assert!(error.message.contains("not started"), "{error}");
        assert!(!host.is_started("test.contrib"));

        host.start_plugin("test.contrib").expect("start");
        host.stop_plugin("test.contrib").expect("first stop");
        let error = host
            .stop_plugin("test.contrib")
            .expect_err("a second stop must fail");
        assert!(error.message.contains("not started"), "{error}");
        assert!(host.has_plugin("test.contrib"));
    }

    /// Unregistering after a stop revokes the contributions exactly like the
    /// direct path: the stopped plugin is still fully registered state.
    #[test]
    fn unregister_after_stop_removes_contributions() {
        let mut host = PluginHost::new(Engine::new());
        let handle = host
            .register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        host.start_plugin("test.contrib").expect("start");
        host.stop_plugin("test.contrib").expect("stop");

        let removed = handle.dispose(&mut host).expect("unregister after stop");
        assert_eq!(removed, vec!["test.contrib"]);
        assert!(!host.has_plugin("test.contrib"));
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_none()
        );
        assert!(host.service_descriptions().is_empty());
        assert!(host.plugin_command_descriptions().is_empty());
    }

    #[test]
    fn shutdown_skips_unstarted_plugins() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        host.shutdown().expect("shutdown");
        assert_eq!(
            host.world().tick_rate,
            60,
            "stop must not run for unstarted plugins"
        );
    }

    #[test]
    fn failed_start_rolls_back_world_and_all_contributions() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin(
            Box::new(ContributionPlugin { fail_start: true }),
            &["test.permission".to_string()],
        )
        .expect("register phase succeeds");
        let error = host
            .start_plugin("test.contrib")
            .expect_err("failing start must reject startup");
        assert!(error.message.contains("contrib start failure"));

        // The instance stays registered for cleanup but is not started.
        assert!(host.has_plugin("test.contrib"));
        assert!(!host.is_started("test.contrib"));

        // World modifications from the failed start are reverted.
        assert_eq!(host.world().tick_rate, 60);
        assert_eq!(host.world().tick, 0);

        // Registration-phase contributions are revoked: component, service,
        // command, migrator, plus the start-phase runtime service.
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_none(),
            "contributed component must be removed after failed start"
        );
        assert!(host.service_descriptions().is_empty());
        assert!(host.plugin_command_descriptions().is_empty());
        assert!(
            host.engine()
                .migrate_component_value("test.contrib.health", json!({ "hp": 5 }), 1, 2,)
                .is_err(),
            "contributed migrator must be removed after failed start"
        );

        // Permissions revoked and systems unscheduled.
        assert!(
            host.permissions
                .require("test.contrib", "test.permission")
                .is_err()
        );
        host.run_stage(SystemStage::FixedUpdate)
            .expect("stage runs with no systems left");
        assert_eq!(host.world().tick, 0);

        // The registered instance unregisters cleanly and can try again.
        let removed = host
            .unregister("test.contrib")
            .expect("unregister after failed start");
        assert_eq!(removed, vec!["test.contrib"]);
        host.register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("re-register");
        host.start_plugin("test.contrib").expect("re-start");
    }

    #[test]
    fn register_phase_rollback_is_all_or_nothing() {
        let mut host = PluginHost::new(Engine::new());
        let error = host
            .register_plugin_with_permissions(
                Box::new(DuplicateRegisterPlugin),
                &["test.permission".to_string()],
            )
            .expect_err("duplicate component ID must fail the whole register");
        assert!(
            error.message.contains("duplicate component ID"),
            "unexpected error: {error}"
        );
        assert!(!host.has_plugin("test.duplicate"));
        assert!(
            host.engine()
                .registry()
                .resolve("test.duplicate.health")
                .is_none(),
            "earlier component contribution must be rolled back"
        );
        assert!(
            host.service_descriptions().is_empty(),
            "earlier service contribution must be rolled back"
        );
        assert!(
            host.permissions
                .require("test.duplicate", "test.permission")
                .is_err(),
            "permissions granted for a failed register must be revoked"
        );
    }

    #[test]
    fn unregister_refuses_attached_components_and_forcing_strips_them() {
        let mut host = PluginHost::new(Engine::new());
        let handle = host
            .register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("register phase succeeds");
        host.start_plugin("test.contrib").expect("start");
        let spawned = host
            .engine_mut()
            .apply(Command::Spawn {
                spec: serde_json::from_value(json!({ "name": "carrier" }))
                    .expect("valid entity spec"),
            })
            .expect("spawn");
        let entity = spawned["entity"].as_u64().expect("entity id");
        host.engine_mut()
            .attach_component(entity, "test.contrib.health", json!({ "hp": 5 }))
            .expect("attach contributed component");

        let error = host
            .unregister("test.contrib")
            .expect_err("attached component must refuse the unregister");
        assert!(
            error.message.contains("test.contrib.health") && error.message.contains("1 entities"),
            "structured refusal must name the component and usage count: {error}"
        );
        assert!(host.has_plugin("test.contrib"), "nothing may be removed");
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_some(),
            "the component stays registered after refusal"
        );
        assert!(
            host.world()
                .entities
                .get(&entity)
                .expect("entity still there")
                .extensions
                .contains_key("test.contrib.health"),
            "the attachment survives the refusal"
        );

        // Forcing strips the attachment (emitting removal events), then
        // unregisters the plugin and its contributions.
        let removed = host
            .unregister_forcing("test.contrib")
            .expect("force unregister");
        assert_eq!(removed, vec!["test.contrib"]);
        assert!(!host.has_plugin("test.contrib"));
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_none()
        );
        let events = host.take_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ComponentRemoved { .. })),
            "force strip must emit component_removed events"
        );
        let _ = handle;
    }

    #[test]
    fn unregister_cascades_remove_each_owner_contributions() {
        let mut host = PluginHost::new(Engine::new());
        host.register_plugin(Box::new(ContributionPlugin { fail_start: false }), &[])
            .expect("contrib registers");
        host.register_plugin_with_permissions(
            Box::new(ConsumerPlugin {
                consumed_in_start: None,
                service_name: "test.contrib.state",
            }),
            &[],
        )
        .expect("consumer registers and consumes the registered service");
        let removed = host.unregister("test.contrib").expect("cascade unregister");
        assert_eq!(removed, vec!["test.contrib", "test.consumer"]);
        assert!(
            host.engine()
                .registry()
                .resolve("test.contrib.health")
                .is_none(),
            "cascade unregister removes the provider's component"
        );
        assert!(host.service_descriptions().is_empty());
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
