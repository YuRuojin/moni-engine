//! The single runtime path shared by the windowed client, the editor, and
//! the NDJSON control server. A `RuntimeSession` owns the plugin host (and
//! through it the engine, world, and script runtime) plus the effective
//! profile name, and routes every control command through one dispatch with
//! revision tracking, event sequencing, and idempotent request IDs.

use std::collections::{BTreeMap, BTreeSet, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use serde_json::{Value, json};

use crate::engine::{Command, Engine, EngineError, EntityId, MAX_STEP_COUNT, World};
use crate::host::{CORE_SIMULATION, PluginHost, SystemStage};
use crate::profile::Profile;
use crate::protocol::{Request, Response};
use crate::scene::COMPONENT_VERSIONS_KEY;

/// Replay responses for this many most recent request IDs.
const REQUEST_CACHE_CAPACITY: usize = 256;

pub struct RuntimeSession {
    host: PluginHost,
    profile_name: String,
    request_cache: RequestCache,
}

struct RequestCache {
    entries: BTreeMap<String, CachedResponse>,
    order: VecDeque<String>,
}

struct CachedResponse {
    fingerprint: u64,
    response: Response,
}

impl RequestCache {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }

    fn fingerprint(command: &str, args: &Value, expected_revision: Option<u64>) -> u64 {
        let mut hasher = DefaultHasher::new();
        command.hash(&mut hasher);
        args.to_string().hash(&mut hasher);
        expected_revision.hash(&mut hasher);
        hasher.finish()
    }

    /// Returns the cached response for `id` when the payload matches, or an
    /// error when the ID was reused for different work.
    fn replay(
        &self,
        id: &Value,
        command: &str,
        args: &Value,
        expected_revision: Option<u64>,
    ) -> Option<Result<Response, EngineError>> {
        let key = id.to_string();
        let Some(cached) = self.entries.get(&key) else {
            return None;
        };
        if cached.fingerprint == Self::fingerprint(command, args, expected_revision) {
            return Some(Ok(cached.response.clone()));
        }
        Some(Err(EngineError::new(
            "duplicate_request_id",
            format!("request id {key} was already used for a different command; use a unique id"),
        )))
    }

    fn remember(
        &mut self,
        id: &Value,
        command: &str,
        args: &Value,
        expected_revision: Option<u64>,
        response: Response,
    ) {
        let key = id.to_string();
        if self.entries.contains_key(&key) {
            return;
        }
        while self.order.len() >= REQUEST_CACHE_CAPACITY {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&evicted);
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedResponse {
                fingerprint: Self::fingerprint(command, args, expected_revision),
                response,
            },
        );
    }
}

impl RuntimeSession {
    /// Builds a session around a prepared engine. The composition root
    /// installs concrete providers (script runtime, renderers) before or
    /// after this call; the session itself stays implementation-agnostic.
    pub fn new(engine: Engine, profile: Profile) -> Self {
        Self {
            host: PluginHost::new(engine),
            profile_name: profile.name.clone(),
            request_cache: RequestCache::new(),
        }
    }

    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    pub fn world(&self) -> &World {
        self.host.world()
    }

    pub fn world_mut(&mut self) -> &mut World {
        self.host.world_mut()
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        self.host.engine_mut()
    }

    /// Read-only engine access, including the session component registry.
    pub fn engine(&self) -> &Engine {
        self.host.engine()
    }

    pub fn host_mut(&mut self) -> &mut PluginHost {
        &mut self.host
    }

    pub fn revision(&self) -> u64 {
        self.host.engine().revision()
    }

    pub fn event_seq(&self) -> u64 {
        self.host.engine().event_seq()
    }

    /// Applies one world command. Every frontend (GUI, CLI, agent) goes
    /// through this so revisions, events, and persistence stay identical.
    pub fn apply(&mut self, command: Command) -> Result<Value, EngineError> {
        self.host.engine_mut().apply(command)
    }

    /// Loads a `.mnsc` scene file into the session. This is the single
    /// scene-loading path shared by the GUI, the CLI, and agent sessions;
    /// `scene_path` is workspace-relative. `scene::load_scene` validates
    /// syntax and core components; dynamic components are then validated
    /// against the session registry BEFORE anything is installed: values
    /// written by older component versions run through the registered
    /// migrator chains, then every value gets schema defaults and schema
    /// validation. A failed load leaves the current world, revision, and
    /// events untouched; success reports migrated and preserved-future
    /// component ids alongside the loaded entity count, and rebuilds the
    /// world-level version table so unregistered and future-version
    /// components keep their scene versions (see
    /// `retain_component_versions`).
    pub fn load_scene(&mut self, scene_path: &str) -> Result<Value, EngineError> {
        let path = crate::workspace_path(scene_path)
            .map_err(|error| EngineError::new("invalid_path", error.to_string()))?;
        let mut world = crate::scene::load_scene(&path)
            .map_err(|error| EngineError::new("invalid_scene", error.to_string()))?;
        let report = {
            let engine = self.host.engine();
            validate_scene_components(engine, &mut world)?
        };
        let entities = world.entities.len();
        self.host.engine_mut().load_world(world)?;
        Ok(json!({
            "loaded": true,
            "entities": entities,
            "migrated": report.migrated,
            "preservedFuture": report.preserved_future,
        }))
    }

    /// Advances the fixed-step simulation. When a simulation provider is
    /// registered the ticks run through the plugin host's FixedUpdate stage
    /// so plugins observe every tick; a registered-but-not-started provider
    /// refuses with `plugin_not_started` instead of silently doing nothing.
    /// Without any simulation provider the engine's built-in loop runs
    /// directly (minimal/headless sessions without simulation plugins).
    pub fn step(&mut self, count: u64) -> Result<Value, EngineError> {
        if !self.host.has_plugin(CORE_SIMULATION) {
            return self.host.engine_mut().step(count);
        }
        if !self.host.is_started(CORE_SIMULATION) {
            return Err(EngineError::new(
                "plugin_not_started",
                format!(
                    "plugin '{CORE_SIMULATION}' is registered but not started; start it before stepping"
                ),
            ));
        }
        if count == 0 || count > MAX_STEP_COUNT {
            return Err(EngineError::new(
                "invalid_step_count",
                format!("step count must be between 1 and {}", MAX_STEP_COUNT),
            ));
        }
        for _ in 0..count {
            self.host
                .run_stage(SystemStage::FixedUpdate)
                .map_err(|error| EngineError::new("plugin_error", format!("{error}")))?;
        }
        Ok(json!({
            "ticks": count,
            "tick": self.host.world().tick,
            "entities": self.host.world().entities.len()
        }))
    }

    /// Handles one NDJSON protocol line: parse, replay-check, dispatch, then
    /// attach the revision and event-sequence watermark to the response.
    pub fn handle_line(&mut self, line: &str) -> (Response, bool) {
        let request = match serde_json::from_str::<Request>(line) {
            Ok(request) => request,
            Err(error) => {
                let response = Response::error(
                    Value::Null,
                    EngineError::new(
                        "invalid_request",
                        format!("invalid NDJSON request: {error}"),
                    ),
                    Vec::new(),
                    self.revision(),
                    self.event_seq(),
                );
                return (response, false);
            }
        };

        let Request {
            id,
            command,
            args,
            expected_revision,
        } = request;
        // Only string and number IDs participate in idempotent replay; null
        // IDs are fire-and-forget and never cached.
        let replayed = if id.is_string() || id.is_i64() || id.is_u64() {
            self.request_cache
                .replay(&id, &command, &args, expected_revision)
        } else {
            None
        };
        if let Some(replay) = replayed {
            let response = match replay {
                Ok(cached) => cached,
                Err(error) => {
                    Response::error(id, error, Vec::new(), self.revision(), self.event_seq())
                }
            };
            return (response, false);
        }

        // Optimistic concurrency for single commands: fail before dispatch so
        // a mismatched revision executes nothing, changes nothing, and emits
        // no events.
        if let Some(expected) = expected_revision
            && self.revision() != expected
        {
            let response = Response::error(
                id.clone(),
                EngineError::new(
                    "revision_conflict",
                    format!(
                        "expected world revision {expected} but the session is at revision {}",
                        self.revision()
                    ),
                ),
                Vec::new(),
                self.revision(),
                self.event_seq(),
            );
            if !id.is_null() {
                self.request_cache.remember(
                    &id,
                    &command,
                    &args,
                    expected_revision,
                    response.clone(),
                );
            }
            return (response, false);
        }

        let (result, quit) = crate::protocol::dispatch(self, &command, args.clone());
        let events = self.host_mut().take_events();
        let response = match result {
            Ok(result) => Response::success(
                id.clone(),
                result,
                events,
                self.revision(),
                self.event_seq(),
            ),
            Err(error) => {
                Response::error(id.clone(), error, events, self.revision(), self.event_seq())
            }
        };
        if !id.is_null() {
            self.request_cache
                .remember(&id, &command, &args, expected_revision, response.clone());
        }
        (response, quit)
    }
}

/// Result of the registry validation pass over a parsed scene world.
struct SceneComponentValidation {
    migrated: Vec<String>,
    preserved_future: Vec<String>,
}

/// Registry validation pass over a freshly parsed scene world, run before the
/// world is installed so a failure never touches the session. For every
/// entity extension key that resolves in `engine`'s registry as a dynamic
/// component:
///
/// - the scene version comes from the world-level `moni.componentVersions`
///   table; a missing entry means the data is assumed to be at the registry
///   version,
/// - values from older versions run through the engine's migrator chain and
///   their ids are reported as migrated,
/// - values written by a newer engine version are preserved verbatim and
///   reported under `preservedFuture` (no error),
/// - every current-version value gets schema defaults and schema validation,
/// - unregistered keys and the reserved version-table key itself are kept as
///   raw data.
///
/// On success the version table is rebuilt for the live world instead of
/// being dropped: entries for unregistered components (any version) and for
/// future-version instances survive so a later registry-aware save
/// round-trips their versions and a later plugin install can still migrate
/// the data, while entries of components migrated/validated to the registry
/// version are consumed (the save recomputes those). Entries whose component
/// no longer appears on any entity are dropped.
fn validate_scene_components(
    engine: &Engine,
    world: &mut World,
) -> Result<SceneComponentValidation, EngineError> {
    let scene_versions = parse_component_versions(world)?;
    let mut migrated = BTreeSet::new();
    let mut preserved_future = BTreeSet::new();
    for entity in world.entities.values_mut() {
        for (component, value) in entity.extensions.iter_mut() {
            if component == COMPONENT_VERSIONS_KEY {
                continue;
            }
            let Some(descriptor) = engine.registry().resolve(component) else {
                continue;
            };
            if !descriptor.is_dynamic() {
                continue;
            }
            let current = descriptor.version;
            let scene_version = scene_versions.get(component).copied().unwrap_or(current);
            let mut upgraded = value.take();
            if scene_version > current {
                preserved_future.insert(component.clone());
                *value = upgraded;
                continue;
            }
            if scene_version < current {
                upgraded = engine
                    .migrate_component_value(component, upgraded, scene_version, current)
                    .map_err(|error| component_load_error(entity.id, component, &error))?;
                migrated.insert(component.clone());
            }
            descriptor
                .schema
                .apply_defaults(&mut upgraded)
                .map_err(|cause| component_schema_error(entity.id, component, &cause))?;
            descriptor
                .schema
                .validate(&upgraded)
                .map_err(|cause| component_schema_error(entity.id, component, &cause))?;
            *value = upgraded;
        }
    }
    retain_component_versions(engine, world, &scene_versions, &preserved_future);
    Ok(SceneComponentValidation {
        migrated: migrated.into_iter().collect(),
        preserved_future: preserved_future.into_iter().collect(),
    })
}

/// Rebuilds the live world's [`COMPONENT_VERSIONS_KEY`] table after a
/// successful session load. Kept entries:
///
/// - future-version instances (`preservedFuture`): the data stayed verbatim,
///   so the scene version must survive until a migrator exists;
/// - unregistered components: the registry does not know their version, so
///   the scene entry is the only record.
///
/// Entries consumed by migration/validation, entries for components that no
/// longer appear on any entity, and entries for core components (whose data
/// always sits at the registry version) are dropped. An empty table is not
/// installed at all.
fn retain_component_versions(
    engine: &Engine,
    world: &mut World,
    scene_versions: &BTreeMap<String, u32>,
    preserved_future: &BTreeSet<String>,
) {
    let mut retained = BTreeMap::new();
    for (component, version) in scene_versions {
        let in_use = world
            .entities
            .values()
            .any(|entity| entity.extensions.contains_key(component));
        if !in_use {
            continue;
        }
        let keep =
            preserved_future.contains(component) || engine.registry().resolve(component).is_none();
        if keep {
            retained.insert(component.clone(), *version);
        }
    }
    if retained.is_empty() {
        world.extensions.remove(COMPONENT_VERSIONS_KEY);
    } else {
        world.extensions.insert(
            COMPONENT_VERSIONS_KEY.to_string(),
            Value::Object(
                retained
                    .into_iter()
                    .map(|(component, version)| (component, json!(version)))
                    .collect(),
            ),
        );
    }
}

/// Reads the world-level `moni.componentVersions` table into a component id
/// -> version map. The table must be an object of non-negative integer
/// versions.
fn parse_component_versions(world: &World) -> Result<BTreeMap<String, u32>, EngineError> {
    let Some(table) = world.extensions.get(COMPONENT_VERSIONS_KEY) else {
        return Ok(BTreeMap::new());
    };
    let Some(map) = table.as_object() else {
        return Err(EngineError::new(
            "invalid_scene",
            format!(
                "scene extension '{COMPONENT_VERSIONS_KEY}' must be an object of component id -> version"
            ),
        ));
    };
    let mut versions = BTreeMap::new();
    for (component, version) in map {
        let Some(raw) = version.as_u64() else {
            return Err(EngineError::new(
                "invalid_scene",
                format!(
                    "scene extension '{COMPONENT_VERSIONS_KEY}' entry '{component}' must be a non-negative integer version"
                ),
            )
            .with_details(json!({ "component": component })));
        };
        if raw > u32::MAX as u64 {
            return Err(EngineError::new(
                "invalid_scene",
                format!(
                    "scene extension '{COMPONENT_VERSIONS_KEY}' entry '{component}' exceeds the u32 version range"
                ),
            )
            .with_details(json!({ "component": component, "version": raw })));
        }
        versions.insert(component.clone(), raw as u32);
    }
    Ok(versions)
}

fn component_load_error(entity: EntityId, component: &str, error: &EngineError) -> EngineError {
    EngineError::new(
        "invalid_component",
        format!("entity {entity} component '{component}': {error}"),
    )
    .with_details(json!({
        "entity": entity,
        "component": component,
        "cause": error.message,
    }))
}

fn component_schema_error(entity: EntityId, component: &str, cause: &str) -> EngineError {
    EngineError::new(
        "invalid_component",
        format!("entity {entity} component '{component}': {cause}"),
    )
    .with_details(json!({
        "entity": entity,
        "component": component,
        "cause": cause,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{
        CommandDescriptor, Plugin, PluginContext, PluginError, PluginOutput, SystemDescriptor,
    };
    use crate::profile::Profile;
    use serde_json::json;
    use std::any::Any;
    use std::fs;

    fn session() -> RuntimeSession {
        RuntimeSession::new(
            Engine::new(),
            Profile::named("minimal").expect("minimal profile"),
        )
    }

    fn line(id: &str, command: &str, args: Value) -> String {
        json!({ "id": id, "command": command, "args": args }).to_string()
    }

    fn line_with_revision(
        id: &str,
        command: &str,
        args: Value,
        expected_revision: Option<u64>,
    ) -> String {
        json!({
            "id": id,
            "command": command,
            "args": args,
            "expectedRevision": expected_revision,
        })
        .to_string()
    }

    fn spawn_args(name: &str) -> Value {
        json!({ "name": name })
    }

    #[test]
    fn responses_carry_revision_and_event_watermark() {
        let mut session = session();
        let (response, quit) = session.handle_line(&line("1", "spawn", spawn_args("a")));
        assert!(!quit);
        assert!(response.ok);
        assert_eq!(response.revision, 1);
        assert_eq!(response.seq, 1);
        let (response, _) = session.handle_line(&line("2", "query", json!({})));
        assert!(response.ok);
        assert_eq!(response.revision, 1, "reads do not change the revision");
        assert_eq!(response.seq, 1);
    }

    #[test]
    fn failed_commands_leave_revision_and_events_untouched() {
        let mut session = session();
        session.handle_line(&line("1", "spawn", spawn_args("a"))).0;
        let (response, _) = session.handle_line(&line("2", "despawn", json!({ "entity": 999 })));
        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().expect("error body").code,
            "entity_not_found"
        );
        assert_eq!(response.revision, 1, "failed command keeps the revision");
        assert!(response.events.is_empty());
    }

    #[test]
    fn batch_applies_all_requests_atomically() {
        let mut session = session();
        session.handle_line(&line("0", "spawn", spawn_args("keep")));
        let revision = session.revision();

        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "requests": [
                    { "command": "spawn", "args": spawn_args("a") },
                    { "command": "spawn", "args": { "name": "b", "sprite": { "type": "circle", "width": 10 } } },
                    { "command": "set_camera", "args": { "camera": {
                        "position": { "x": 2.0, "y": 2.0, "z": 2.0 },
                        "target": { "x": 0.0, "y": 0.0, "z": 0.0 },
                        "projection": "perspective",
                        "fov_y": 1.0,
                    } } },
                ]
            }),
        ));
        assert!(response.ok, "batch should apply: {:?}", response.error);
        let result = response.result.expect("batch result");
        assert_eq!(result["applied"], json!(3));
        assert_eq!(result["results"].as_array().expect("results").len(), 3);
        assert!(result["revision"].as_u64().expect("revision") > revision);
        assert_eq!(session.world().entities.len(), 3);
    }

    #[test]
    fn failing_batch_rolls_back_every_change_and_event() {
        let mut session = session();
        session.handle_line(&line("0", "spawn", spawn_args("keep")));
        let revision = session.revision();
        let seq = session.event_seq();

        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "requests": [
                    { "command": "spawn", "args": spawn_args("ghost") },
                    { "command": "despawn", "args": { "entity": 1 } },
                    { "command": "despawn", "args": { "entity": 999 } },
                ]
            }),
        ));
        assert!(!response.ok);
        let error = response.error.expect("error body");
        assert_eq!(error.code, "batch_failed");
        let details = error.details.expect("details");
        assert_eq!(details["index"], json!(2));
        assert_eq!(details["rolledBack"], json!(true));

        assert_eq!(session.world().entities.len(), 1, "no partial modification");
        assert_eq!(
            session.world().entities.contains_key(&1),
            true,
            "despawned entity is restored"
        );
        assert_eq!(session.revision(), revision, "revision is restored");
        assert_eq!(
            session.event_seq(),
            seq,
            "events from the failed batch are discarded"
        );
        let (probe, _) = session.handle_line(&line("2", "query", json!({})));
        assert!(
            probe.events.is_empty(),
            "rolled-back events must not reappear"
        );
    }

    #[test]
    fn expected_revision_conflicts_fail_before_any_change() {
        let mut session = session();
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "expectedRevision": 999,
                "requests": [{ "command": "spawn", "args": spawn_args("a") }]
            }),
        ));
        assert!(!response.ok);
        assert_eq!(response.error.expect("error").code, "revision_conflict");
        assert_eq!(session.revision(), 0);
        assert!(session.world().entities.is_empty());
    }

    #[test]
    fn expected_revision_match_is_required_before_apply() {
        let mut session = session();
        let (capabilities, _) = session.handle_line(&line("0", "capabilities", json!({})));
        let revision = capabilities.result.expect("capabilities")["session"]["revision"]
            .as_u64()
            .expect("session revision");
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "expectedRevision": revision,
                "requests": [{ "command": "spawn", "args": spawn_args("a") }]
            }),
        ));
        assert!(
            response.ok,
            "matching revision must apply: {:?}",
            response.error
        );
        assert_eq!(session.world().entities.len(), 1);
    }

    #[test]
    fn dry_run_previews_without_touching_the_session() {
        let mut session = session();
        session.handle_line(&line("0", "spawn", spawn_args("keep")));
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "dryRun": true,
                "requests": [
                    { "command": "spawn", "args": spawn_args("preview") },
                    { "command": "query", "args": {} }
                ]
            }),
        ));
        assert!(response.ok, "dry run should validate: {:?}", response.error);
        let result = response.result.expect("result");
        assert_eq!(result["dryRun"], json!(true));
        assert_eq!(result["applied"], json!(2));
        assert!(
            session.world().entities.len() == 1,
            "dry run must not mutate"
        );
        assert!(response.events.is_empty(), "dry run must not emit events");
    }

    #[test]
    fn dry_run_rejects_runtime_backed_commands() {
        let mut session = session();
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "dryRun": true,
                "requests": [{ "command": "step", "args": { "count": 1 } }]
            }),
        ));
        assert!(!response.ok);
        // step is not batchable at all, and the batch gate precedes the
        // dry-run gate, so the rejection reports invalid_batch_command (the
        // dry-run refusal still holds: the command never executes).
        assert_eq!(response.error.expect("error").code, "invalid_batch_command");
        assert_eq!(session.world().tick, 0, "the dry run must not tick");
    }

    #[test]
    fn batches_reject_file_and_session_commands() {
        let mut session = session();
        for (index, command) in ["save", "load", "render", "batch", "quit", "capabilities"]
            .iter()
            .enumerate()
        {
            let args = match *command {
                "save" | "load" => json!({ "path": "target/x.json" }),
                "render" => json!({}),
                _ => json!({ "requests": [] }),
            };
            let (response, quit) = session.handle_line(&line(
                &format!("reject-{index}"),
                "batch",
                json!({
                    "requests": [{ "command": command, "args": args }]
                }),
            ));
            assert!(!quit);
            assert!(
                !response.ok,
                "command '{command}' must be rejected inside a batch"
            );
            assert_eq!(
                response.error.expect("error").code,
                "invalid_batch_command",
                "command '{command}'"
            );
        }
    }

    /// Test double exposing plugin commands in every descriptor shape the
    /// protocol tests need: a conservative default command, a batchable
    /// world-mutating one, a dry-runnable preview one, a permission-guarded
    /// one, and one whose handler always fails with a distinctive code.
    struct ProtocolCommandPlugin;

    impl Plugin for ProtocolCommandPlugin {
        fn id(&self) -> &'static str {
            "test.protocol.commands"
        }

        fn systems(&self) -> Vec<SystemDescriptor> {
            Vec::new()
        }

        fn start(&mut self, context: &mut PluginContext) -> Result<(), PluginError> {
            context.register_command(
                "test.plain",
                json!({ "args": {} }),
                Box::new(|_engine, _args| Ok(json!({ "plain": true }))),
            )?;
            context.register_command_with(
                CommandDescriptor::conservative(json!({}))
                    .batchable(true)
                    .returns_schema(json!(["tick"])),
                "test.mutate",
                Box::new(|engine, _args| {
                    engine.world_mut().tick += 1;
                    Ok(json!({ "tick": engine.world().tick }))
                }),
            )?;
            context.register_command_with(
                CommandDescriptor::conservative(json!({}))
                    .batchable(true)
                    .dry_runnable(true),
                "test.preview",
                Box::new(|engine, _args| {
                    engine.world_mut().tick += 1;
                    Ok(json!({ "tick": engine.world().tick }))
                }),
            )?;
            context.register_command_with(
                CommandDescriptor::conservative(json!({})).required_permission("test.protocol.run"),
                "test.guarded",
                Box::new(|_engine, _args| Ok(json!({ "guarded": true }))),
            )?;
            context.register_command_with(
                CommandDescriptor::conservative(json!({})).batchable(true),
                "test.boom",
                Box::new(|_engine, _args| {
                    Err(EngineError::new("test_boom", "plugin command failed"))
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

    #[test]
    fn plugin_commands_are_conservative_by_default() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("plugin registers");
        let (response, _) = session.handle_line(&line("1", "test.plain", json!({})));
        assert!(response.ok, "{:?}", response.error);

        // Conservative defaults: not batchable, not dry-runnable.
        let (batch, _) = session.handle_line(&line(
            "2",
            "batch",
            json!({ "requests": [{ "command": "test.plain", "args": {} }] }),
        ));
        assert!(!batch.ok);
        assert_eq!(batch.error.expect("error").code, "invalid_batch_command");
        // The batch gate precedes the dry-run gate, so a non-batchable
        // command fails the same way inside a dry-run batch.
        let (dry, _) = session.handle_line(&line(
            "3",
            "batch",
            json!({
                "dryRun": true,
                "requests": [{ "command": "test.plain", "args": {} }]
            }),
        ));
        assert!(!dry.ok);
        assert_eq!(dry.error.expect("error").code, "invalid_batch_command");

        // A batchable command without declared dry-run support is rejected
        // with dry_run_unsupported by the descriptor gate.
        let (dry, _) = session.handle_line(&line(
            "4",
            "batch",
            json!({
                "dryRun": true,
                "requests": [{ "command": "test.mutate", "args": {} }]
            }),
        ));
        assert!(!dry.ok);
        assert_eq!(dry.error.expect("error").code, "dry_run_unsupported");
    }

    #[test]
    fn batchable_plugin_commands_participate_in_batch_transactions() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("plugin registers");
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({ "requests": [{ "command": "test.mutate", "args": {} }] }),
        ));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(session.world().tick, 1, "plugin command mutated the world");

        // A later failing request rolls the plugin mutation back too.
        let (response, _) = session.handle_line(&line(
            "2",
            "batch",
            json!({ "requests": [
                { "command": "test.mutate", "args": {} },
                { "command": "despawn", "args": { "entity": 999 } }
            ] }),
        ));
        assert!(!response.ok);
        assert_eq!(session.world().tick, 1, "plugin mutation was rolled back");
    }

    #[test]
    fn dry_runnable_plugin_commands_preview_against_the_scratch_world() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("plugin registers");
        let (response, _) = session.handle_line(&line(
            "1",
            "batch",
            json!({
                "dryRun": true,
                "requests": [{ "command": "test.preview", "args": {} }]
            }),
        ));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(
            session.world().tick,
            0,
            "dry run must not touch the session world"
        );
        assert!(response.events.is_empty(), "dry run must not emit events");
    }

    #[test]
    fn failing_plugin_commands_surface_their_own_error() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("plugin registers");
        let (response, _) = session.handle_line(&line("1", "test.boom", json!({})));
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("error").code,
            "test_boom",
            "a failing plugin command must not fall through to built-in dispatch"
        );
        assert_eq!(session.revision(), 0);
    }

    #[test]
    fn guarded_plugin_commands_enforce_the_owner_permission() {
        let mut denied = session();
        denied
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("registers without the grant");
        let (response, _) = denied.handle_line(&line("1", "test.guarded", json!({})));
        assert!(!response.ok);
        let error = response.error.expect("error");
        assert_eq!(error.code, "permission_denied");
        assert_eq!(
            error.details.expect("details")["permission"],
            json!("test.protocol.run")
        );

        // Granting the permission at registration lets the command execute.
        let mut granted = session();
        granted
            .host_mut()
            .register_plugin_with_permissions(
                Box::new(ProtocolCommandPlugin),
                &["test.protocol.run".to_string()],
            )
            .expect("registers with the grant");
        let (response, _) = granted.handle_line(&line("2", "test.guarded", json!({})));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(response.result.expect("result")["guarded"], json!(true));
    }

    #[test]
    fn expected_revision_gates_single_commands() {
        let mut session = session();
        let (response, _) = session.handle_line(&line_with_revision(
            "1",
            "spawn",
            spawn_args("a"),
            Some(999),
        ));
        assert!(!response.ok);
        assert_eq!(response.error.expect("error").code, "revision_conflict");
        assert_eq!(session.revision(), 0, "conflict leaves the revision");
        assert!(session.world().entities.is_empty(), "no side effects");
        assert!(response.events.is_empty(), "no events on conflict");

        // Matching the session revision applies the command.
        let (response, _) =
            session.handle_line(&line_with_revision("2", "spawn", spawn_args("a"), Some(0)));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(session.world().entities.len(), 1);
    }

    #[test]
    fn expected_revision_conflicts_are_cached_like_any_response() {
        let mut session = session();
        let raw = line_with_revision("7", "spawn", spawn_args("a"), Some(999));
        let (first, _) = session.handle_line(&raw);
        assert!(!first.ok);
        let (replay, _) = session.handle_line(&raw);
        assert!(
            !replay.ok,
            "the cached conflict response replays for the same payload"
        );
        assert_eq!(replay.error.expect("error").code, "revision_conflict");
        assert_eq!(
            session.world().entities.len(),
            0,
            "replay must not re-execute"
        );

        // The same ID with a different expectedRevision is a payload change.
        let (reuse, _) =
            session.handle_line(&line_with_revision("7", "spawn", spawn_args("a"), Some(0)));
        assert!(!reuse.ok);
        assert_eq!(reuse.error.expect("error").code, "duplicate_request_id");
    }

    #[test]
    fn capabilities_generate_command_metadata_and_registry_events() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin_with_permissions(Box::new(ProtocolCommandPlugin), &[])
            .expect("plugin registers");
        let (response, _) = session.handle_line(&line("1", "capabilities", json!({})));
        let capabilities = response.result.expect("capabilities");

        let spawn = &capabilities["commands"]["spawn"];
        assert_eq!(spawn["mutatesWorld"], json!(true));
        assert_eq!(spawn["batch"], json!(true));
        assert_eq!(spawn["dryRun"], json!(true));
        assert_eq!(spawn["owner"], json!("core"));
        assert_eq!(spawn["sideEffects"]["file"], json!(false));
        let save = &capabilities["commands"]["save"];
        assert_eq!(save["batch"], json!(false));
        assert_eq!(save["sideEffects"]["file"], json!(true));
        let step = &capabilities["commands"]["step"];
        assert_eq!(step["dryRun"], json!(false));

        let plain = &capabilities["commands"]["test.plain"];
        assert_eq!(plain["owner"], json!("test.protocol.commands"));
        assert_eq!(
            plain["batch"],
            json!(false),
            "3-arg registration is conservative"
        );
        assert_eq!(plain["dryRun"], json!(false));

        let events = capabilities["events"].as_array().expect("events");
        assert!(events.contains(&json!("component_registered")));
        assert!(events.contains(&json!("component_unregistered")));

        let plugin_commands = &capabilities["session"]["pluginCommands"];
        assert!(
            plugin_commands.get("test.plain").is_some(),
            "session overlay keeps listing plugin commands: {plugin_commands}"
        );
    }

    #[test]
    fn identical_request_ids_replay_the_cached_response() {
        let mut session = session();
        let first_line = line("42", "spawn", spawn_args("a"));
        let (first, _) = session.handle_line(&first_line);
        assert!(first.ok);

        let (replay, _) = session.handle_line(&first_line);
        assert!(replay.ok);
        assert_eq!(
            serde_json::to_value(&first.result).expect("first result"),
            serde_json::to_value(&replay.result).expect("replayed result"),
            "replayed response matches the original result"
        );
        assert_eq!(
            session.world().entities.len(),
            1,
            "replay must not re-execute"
        );
        assert_eq!(session.revision(), 1, "replay must not bump the revision");
    }

    #[test]
    fn reused_request_ids_with_different_payloads_are_rejected() {
        let mut session = session();
        session.handle_line(&line("42", "spawn", spawn_args("a")));
        let (response, _) = session.handle_line(&line("42", "spawn", spawn_args("different")));
        assert!(!response.ok);
        assert_eq!(response.error.expect("error").code, "duplicate_request_id");
        assert_eq!(
            session.world().entities.len(),
            1,
            "conflicting reuse must not execute"
        );
    }

    #[test]
    fn null_ids_are_never_cached() {
        let mut session = session();
        let raw = json!({ "id": null, "command": "spawn", "args": spawn_args("a") }).to_string();
        let (first, _) = session.handle_line(&raw);
        assert!(first.ok);
        let (second, _) = session.handle_line(&raw);
        assert!(second.ok, "null ids execute every time");
        assert_eq!(session.world().entities.len(), 2);
    }

    #[test]
    fn step_routes_through_the_plugin_host_when_available() {
        let mut session = session();
        // No simulation plugin: step runs the engine loop directly.
        let (response, _) = session.handle_line(&line("1", "step", json!({ "count": 2 })));
        assert!(response.ok);
        assert_eq!(session.world().tick, 2);

        // With the simulation plugin registered, ticks run through the host.
        session
            .host_mut()
            .register_plugin_with_permissions(
                Box::new(crate::plugins::SimulationPlugin::new()),
                &["script.execute".to_string()],
            )
            .expect("simulation registers");
        let (response, _) = session.handle_line(&line("2", "step", json!({ "count": 3 })));
        assert!(response.ok);
        assert_eq!(session.world().tick, 5);
        assert_eq!(
            response
                .events
                .iter()
                .filter(|event| matches!(event, crate::engine::Event::SimulationStepped { .. }))
                .count(),
            3,
            "host-driven ticks emit one simulation_stepped per tick, matching the windowed client"
        );
    }

    #[test]
    fn command_api_and_protocol_path_stay_equivalent() {
        // The GUI path applies commands directly; the agent path sends
        // protocol lines. Both must produce identical worlds and events.
        let mut gui = session();
        let mut cli = session();

        let camera = json!({
            "position": { "x": 4.0, "y": 3.0, "z": 6.0 },
            "target": { "x": 0.0, "y": 0.0, "z": 0.0 },
            "projection": "perspective",
            "fov_y": 1.1,
        });
        let camera: crate::scene3d::Camera3d =
            serde_json::from_value(camera).expect("valid camera");

        gui.apply(Command::Spawn {
            spec: serde_json::from_value(spawn_args("shared")).expect("valid spec"),
        })
        .expect("gui spawn");
        gui.apply(Command::SetCamera {
            camera: camera.clone(),
        })
        .expect("gui camera");
        let gui_events = gui.host_mut().take_events();

        // The protocol path delivers events inside each response; collect
        // them the way a CLI client would.
        let (spawn_response, _) = cli.handle_line(&line("1", "spawn", spawn_args("shared")));
        let (camera_response, _) = cli.handle_line(&line(
            "2",
            "set_camera",
            json!({
                "camera": {
                    "position": { "x": 4.0, "y": 3.0, "z": 6.0 },
                    "target": { "x": 0.0, "y": 0.0, "z": 0.0 },
                    "projection": "perspective",
                    "fov_y": 1.1,
                }
            }),
        ));
        let mut cli_events = spawn_response.events;
        cli_events.extend(camera_response.events);

        assert_eq!(
            serde_json::to_string(gui.world()).expect("gui world"),
            serde_json::to_string(cli.world()).expect("cli world"),
            "GUI and CLI paths must produce identical worlds"
        );
        assert_eq!(
            serde_json::to_value(&gui_events).expect("gui events"),
            serde_json::to_value(&cli_events).expect("cli events"),
            "GUI and CLI paths must produce identical event semantics"
        );
        assert_eq!(gui.revision(), cli.revision());
    }

    fn health_descriptor() -> crate::components::ComponentDescriptor {
        health_descriptor_at(1)
    }

    fn health_descriptor_at(version: u32) -> crate::components::ComponentDescriptor {
        use crate::components::{PropertyField, PropertySchema, PropertyType};
        crate::components::ComponentDescriptor::dynamic(
            "game.health",
            None,
            "game.test",
            version,
            PropertySchema::new(
                "Health",
                "Dynamic test component",
                vec![
                    PropertyField::new("hp", PropertyType::Number, false)
                        .default(json!(100))
                        .range(0.0, 1000.0),
                    PropertyField::new("max_hp", PropertyType::Number, false)
                        .default(json!(100))
                        .range(0.0, 1000.0),
                ],
            ),
        )
    }

    /// Writes a scene into the crate-local `target/` output and returns the
    /// workspace-relative path `RuntimeSession::load_scene` expects.
    fn write_test_scene(name: &str, scene: &Value) -> String {
        let relative = format!("target/moni-session-{name}-{}.mnsc", std::process::id());
        let path = std::env::current_dir().expect("cwd").join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create target directory");
        }
        fs::write(
            &path,
            serde_json::to_string_pretty(scene).expect("scene json"),
        )
        .expect("write scene file");
        relative
    }

    fn remove_test_scene(relative: &str) {
        std::fs::remove_file(std::env::current_dir().expect("cwd").join(relative)).ok();
    }

    #[test]
    fn dynamic_components_round_trip_through_the_protocol() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor())
            .expect("register");
        session.handle_line(&line("1", "spawn", spawn_args("a")));

        let (response, _) = session.handle_line(&line(
            "2",
            "attach_component",
            json!({ "entity": 1, "component": "game.health", "value": { "hp": 55 } }),
        ));
        assert!(response.ok, "{:?}", response.error);
        assert_eq!(
            response.result.expect("result")["component"],
            json!("game.health")
        );
        assert!(
            response.events.iter().any(|event| matches!(event,
                crate::engine::Event::ComponentAttached { component, .. } if component == "game.health"
            )),
            "response carries component_attached: {:?}",
            response.events
        );

        let (query, _) = session.handle_line(&line("3", "query", json!({ "entity": 1 })));
        let entity = query.result.expect("query result");
        assert_eq!(entity["extensions"]["game.health"]["hp"], json!(55));
        assert_eq!(
            entity["extensions"]["game.health"]["max_hp"],
            json!(100),
            "schema defaults are applied"
        );

        let (invalid, _) = session.handle_line(&line(
            "4",
            "attach_component",
            json!({ "entity": 1, "component": "game.health", "value": { "hp": -3 } }),
        ));
        assert!(!invalid.ok);
        assert_eq!(
            invalid.error.expect("error body").code,
            "invalid_component_value"
        );
        assert_eq!(session.revision(), 3, "failed attach keeps the revision");

        let (unknown, _) = session.handle_line(&line(
            "5",
            "attach_component",
            json!({ "entity": 1, "component": "core.sprite", "value": {} }),
        ));
        assert!(!unknown.ok);
        assert_eq!(
            unknown.error.expect("error body").code,
            "component_reserved"
        );

        let (removed, _) = session.handle_line(&line(
            "6",
            "remove_component",
            json!({ "entity": 1, "component": "game.health" }),
        ));
        assert!(removed.ok, "{:?}", removed.error);
        assert!(
            removed.events.iter().any(|event| matches!(event,
                crate::engine::Event::ComponentRemoved { component, .. } if component == "game.health"
            )),
            "response carries component_removed"
        );
        assert!(
            session
                .world()
                .entities
                .get(&1)
                .expect("entity")
                .extensions
                .is_empty(),
            "remove clears the extension entry"
        );
    }

    #[test]
    fn capabilities_project_the_session_component_registry() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor())
            .expect("register");
        let (response, _) = session.handle_line(&line("1", "capabilities", json!({})));
        let capabilities = response.result.expect("capabilities");
        let components = &capabilities["components"];
        assert!(
            components.get("game.health").is_some(),
            "dynamic components appear in capabilities: {components}"
        );
        assert_eq!(
            components["game.health"]["x-component"]["owner"],
            json!("game.test")
        );
        assert_eq!(
            components["game.health"]["x-component"]["version"],
            json!(1)
        );
        assert!(
            components.get("core.sprite").is_some(),
            "core components stay listed"
        );
        let commands = &capabilities["commands"];
        assert!(
            commands.get("attach_component").is_some(),
            "attach_component is documented"
        );
        assert!(
            commands.get("remove_component").is_some(),
            "remove_component is documented"
        );
        let events = capabilities["events"].as_array().expect("events");
        assert!(events.contains(&json!("component_attached")));
        assert!(events.contains(&json!("component_removed")));
    }

    #[test]
    fn attach_component_works_in_batches_and_dry_runs() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor())
            .expect("register");
        session.handle_line(&line("1", "spawn", spawn_args("a")));

        let (dry, _) = session.handle_line(&line(
            "2",
            "batch",
            json!({
                "dryRun": true,
                "requests": [
                    { "command": "attach_component", "args": { "entity": 1, "component": "game.health", "value": {} } }
                ]
            }),
        ));
        assert!(
            dry.ok,
            "dry run validates against the session registry: {:?}",
            dry.error
        );
        assert!(
            session
                .world()
                .entities
                .get(&1)
                .expect("entity")
                .extensions
                .is_empty(),
            "dry run must not mutate"
        );

        let (applied, _) = session.handle_line(&line(
            "3",
            "batch",
            json!({
                "requests": [
                    { "command": "attach_component", "args": { "entity": 1, "component": "game.health", "value": { "hp": 10 } } }
                ]
            }),
        ));
        assert!(applied.ok, "{:?}", applied.error);
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["game.health"]["hp"],
            json!(10)
        );
    }

    fn scene_file(extensions: Value, components: Value) -> Value {
        json!({
            "format": "moni-scene",
            "version": 2,
            "name": "session-load",
            "camera": {},
            "extensions": extensions,
            "entities": [
                {
                    "id": 1,
                    "name": "a",
                    "components": {
                        "core.transform": { "x": 1.0, "y": 2.0 },
                        "game.health": components,
                    }
                }
            ]
        })
    }

    #[test]
    fn load_scene_migrates_component_values_through_the_registry_chain() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(3))
            .expect("register");
        session
            .engine_mut()
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value| {
                    let hp = value
                        .get("hp")
                        .and_then(Value::as_f64)
                        .ok_or("missing 'hp'")?;
                    Ok(json!({ "hp": hp * 2.0 }))
                }),
            )
            .expect("step 1 -> 2");
        session
            .engine_mut()
            .register_component_migrator(
                "game.health",
                2,
                3,
                Box::new(|value| {
                    let hp = value
                        .get("hp")
                        .and_then(Value::as_f64)
                        .ok_or("missing 'hp'")?;
                    Ok(json!({ "hp": hp + 100.0 }))
                }),
            )
            .expect("step 2 -> 3");
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 1 } }),
            json!({ "hp": 40 }),
        );
        let relative = write_test_scene("migrate", &scene);
        let result = session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert_eq!(result["loaded"], json!(true));
        assert_eq!(result["entities"], json!(1));
        assert_eq!(result["migrated"], json!(["game.health"]));
        assert_eq!(result["preservedFuture"], json!([]));
        let stored = &session.world().entities.get(&1).expect("entity").extensions["game.health"];
        assert_eq!(stored["hp"], json!(180.0), "the chain ran 1 -> 2 -> 3");
        assert_eq!(
            stored["max_hp"],
            json!(100),
            "schema defaults apply after migration"
        );
    }

    #[test]
    fn load_scene_without_the_required_migrator_keeps_the_session_untouched() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(3))
            .expect("register");
        session
            .engine_mut()
            .register_component_migrator(
                "game.health",
                2,
                3,
                Box::new(|value| {
                    let hp = value
                        .get("hp")
                        .and_then(Value::as_f64)
                        .ok_or("missing 'hp'")?;
                    Ok(json!({ "hp": hp + 100.0 }))
                }),
            )
            .expect("step 2 -> 3");
        session
            .handle_line(&line("1", "spawn", spawn_args("keep")))
            .0;
        let revision = session.revision();
        let seq = session.event_seq();
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 1 } }),
            json!({ "hp": 40 }),
        );
        let relative = write_test_scene("missing-step", &scene);
        let error = session
            .load_scene(&relative)
            .expect_err("no migration step from version 1");
        remove_test_scene(&relative);
        assert_eq!(error.code, "invalid_component");
        let details = error.details.expect("details");
        assert_eq!(details["entity"], json!(1));
        assert_eq!(details["component"], json!("game.health"));
        assert!(
            details["cause"]
                .as_str()
                .expect("cause")
                .contains("migration"),
            "the cause names the migration problem: {}",
            details["cause"]
        );
        assert_eq!(
            session.world().entities.len(),
            1,
            "the previous world is untouched"
        );
        assert_eq!(session.revision(), revision, "the revision is untouched");
        assert_eq!(session.event_seq(), seq, "no events leak out");
    }

    #[test]
    fn load_scene_rejects_invalid_registered_values_without_replacing_the_world() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(1))
            .expect("register");
        session
            .handle_line(&line("1", "spawn", spawn_args("keep")))
            .0;
        let revision = session.revision();
        let seq = session.event_seq();
        let scene = scene_file(json!({}), json!({ "hp": -5 }));
        let relative = write_test_scene("invalid-value", &scene);
        let error = session
            .load_scene(&relative)
            .expect_err("range violation must fail");
        remove_test_scene(&relative);
        assert_eq!(error.code, "invalid_component");
        let details = error.details.expect("details");
        assert_eq!(details["entity"], json!(1));
        assert_eq!(details["component"], json!("game.health"));
        assert!(
            details["cause"].as_str().expect("cause").contains("hp"),
            "the cause reports the field error: {}",
            details["cause"]
        );
        assert_eq!(
            session.world().entities.len(),
            1,
            "the previous world is untouched"
        );
        assert_eq!(session.revision(), revision);
        assert_eq!(session.event_seq(), seq);
    }

    #[test]
    fn load_scene_assumes_current_version_when_the_table_entry_is_missing() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(1))
            .expect("register");
        let scene = scene_file(json!({}), json!({ "hp": 55 }));
        let relative = write_test_scene("no-entry", &scene);
        let result = session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert_eq!(result["migrated"], json!([]));
        assert_eq!(result["preservedFuture"], json!([]));
        let stored = &session.world().entities.get(&1).expect("entity").extensions["game.health"];
        assert_eq!(stored["hp"], json!(55));
        assert_eq!(
            stored["max_hp"],
            json!(100),
            "missing table entries are validated as the registry version"
        );
        assert!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .is_none(),
            "a scene without a version table leaves none in the live world"
        );

        // A table entry at the registry version is consumed by validation:
        // the live data sits at the current version, so no entry survives
        // and the next save recomputes it.
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 1 } }),
            json!({ "hp": 55 }),
        );
        let relative = write_test_scene("consumed-entry", &scene);
        session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .is_none(),
            "entries validated at the registry version are consumed, not retained"
        );
    }

    #[test]
    fn load_scene_preserves_values_from_future_component_versions() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(1))
            .expect("register");
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 9 } }),
            json!({ "hp": 55, "futureField": true }),
        );
        let relative = write_test_scene("future", &scene);
        let result = session.load_scene(&relative).expect("future values load");
        remove_test_scene(&relative);
        assert_eq!(result["preservedFuture"], json!(["game.health"]));
        assert_eq!(result["migrated"], json!([]));
        let stored = &session.world().entities.get(&1).expect("entity").extensions["game.health"];
        assert_eq!(
            stored,
            &json!({ "hp": 55, "futureField": true }),
            "future-version values are kept verbatim, without defaults or validation"
        );
        let table = session
            .world()
            .extensions
            .get(crate::scene::COMPONENT_VERSIONS_KEY)
            .expect("retained table");
        assert_eq!(
            table["game.health"],
            json!(9),
            "the future entry is retained in the live world for the next save"
        );
    }

    #[test]
    fn v1_scene_survives_pluginless_load_save_then_migrates() {
        let mut session = session();
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 1 } }),
            json!({ "hp": 40 }),
        );
        let relative = write_test_scene("v1-keep", &scene);
        let result = session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert_eq!(result["migrated"], json!([]));
        assert_eq!(result["preservedFuture"], json!([]));
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["game.health"],
            json!({ "hp": 40 }),
            "the unregistered component keeps its data verbatim"
        );
        assert_eq!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .expect("retained table")["game.health"],
            json!(1),
            "the unregistered component keeps its scene version in the live world"
        );

        // Registry-aware save without the plugin: the version entry and the
        // v1 data round-trip unchanged.
        let out_relative = format!("target/moni-session-v1-out-{}.mnsc", std::process::id());
        let out_path = std::env::current_dir().expect("cwd").join(&out_relative);
        crate::scene::save_scene_with_registry(
            session.world(),
            "kept",
            &out_path,
            session.engine().registry(),
        )
        .expect("save scene");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(&out_path).expect("read saved scene"))
                .expect("parse saved scene");
        assert_eq!(
            saved["extensions"][crate::scene::COMPONENT_VERSIONS_KEY]["game.health"],
            json!(1),
            "the retained version is written back"
        );
        assert_eq!(
            saved["entities"][0]["components"]["game.health"],
            json!({ "hp": 40 }),
            "the v1 data is byte-identical"
        );

        // Installing the plugin later: the same saved scene migrates on the
        // next load, and the consumed entry leaves the live world.
        session
            .engine_mut()
            .register_component(health_descriptor_at(2))
            .expect("register");
        session
            .engine_mut()
            .register_component_migrator(
                "game.health",
                1,
                2,
                Box::new(|value| {
                    let hp = value
                        .get("hp")
                        .and_then(Value::as_f64)
                        .ok_or("missing 'hp'")?;
                    Ok(json!({ "hp": hp * 2.0 }))
                }),
            )
            .expect("step 1 -> 2");
        let reloaded = session.load_scene(&out_relative).expect("reload scene");
        remove_test_scene(&out_relative);
        assert_eq!(reloaded["migrated"], json!(["game.health"]));
        assert_eq!(reloaded["preservedFuture"], json!([]));
        let stored = &session.world().entities.get(&1).expect("entity").extensions["game.health"];
        assert_eq!(
            stored,
            &json!({ "hp": 80.0, "max_hp": 100 }),
            "the retained v1 data migrated to v2 with schema defaults"
        );
        assert!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .is_none(),
            "the consumed entry does not linger in the live world"
        );
    }

    #[test]
    fn future_version_data_and_version_survive_round_trip() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(2))
            .expect("register");
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 9 } }),
            json!({ "hp": 55, "futureField": true }),
        );
        let relative = write_test_scene("future-keep", &scene);
        let result = session.load_scene(&relative).expect("future values load");
        remove_test_scene(&relative);
        assert_eq!(result["preservedFuture"], json!(["game.health"]));
        assert_eq!(result["migrated"], json!([]));
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["game.health"],
            json!({ "hp": 55, "futureField": true }),
            "future-version data is kept verbatim"
        );
        assert_eq!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .expect("retained table")["game.health"],
            json!(9),
            "the future version is retained in the live world"
        );

        let out_relative = format!("target/moni-session-future-out-{}.mnsc", std::process::id());
        let out_path = std::env::current_dir().expect("cwd").join(&out_relative);
        crate::scene::save_scene_with_registry(
            session.world(),
            "future",
            &out_path,
            session.engine().registry(),
        )
        .expect("save scene");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(&out_path).expect("read saved scene"))
                .expect("parse saved scene");
        assert_eq!(
            saved["extensions"][crate::scene::COMPONENT_VERSIONS_KEY]["game.health"],
            json!(9),
            "the retained future version survives the save"
        );
        assert_eq!(
            saved["entities"][0]["components"]["game.health"],
            json!({ "hp": 55, "futureField": true }),
            "future-version data is byte-identical after the save"
        );
        remove_test_scene(&out_relative);
    }

    #[test]
    fn current_version_writes_are_rejected_while_future_instances_exist() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(2))
            .expect("register");
        let scene = scene_file(
            json!({ "moni.componentVersions": { "game.health": 9 } }),
            json!({ "hp": 55, "futureField": true }),
        );
        let relative = write_test_scene("future-guard", &scene);
        session.load_scene(&relative).expect("future values load");
        remove_test_scene(&relative);
        session.host_mut().take_events();
        session.handle_line(&line("1", "spawn", spawn_args("b"))).0;
        session.host_mut().take_events();
        let revision = session.revision();

        let (response, _) = session.handle_line(&line(
            "2",
            "attach_component",
            json!({ "entity": 2, "component": "game.health", "value": { "hp": 10 } }),
        ));
        assert!(!response.ok);
        let error = response.error.expect("error body");
        assert_eq!(error.code, "component_future_version");
        let details = error.details.expect("details");
        assert_eq!(details["component"], json!("game.health"));
        assert_eq!(details["sceneVersion"], json!(9));
        assert_eq!(details["registryVersion"], json!(2));
        assert_eq!(details["futureInstances"], json!(1));
        assert_eq!(
            session.revision(),
            revision,
            "failed attach keeps the revision"
        );
        assert_eq!(session.world().entities.len(), 2, "no entity was added");
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["game.health"],
            json!({ "hp": 55, "futureField": true }),
            "the future instance is untouched"
        );
        assert!(
            session.host_mut().take_events().is_empty(),
            "a failed attach emits no events"
        );

        // Removing the future instance is the explicit way out; writes
        // unlock afterwards and the save rebuilds the registry version.
        let (removed, _) = session.handle_line(&line(
            "3",
            "remove_component",
            json!({ "entity": 1, "component": "game.health" }),
        ));
        assert!(removed.ok, "{:?}", removed.error);
        let (attached, _) = session.handle_line(&line(
            "4",
            "attach_component",
            json!({ "entity": 2, "component": "game.health", "value": { "hp": 10 } }),
        ));
        assert!(attached.ok, "{:?}", attached.error);
        let out_relative = format!(
            "target/moni-session-future-guard-out-{}.mnsc",
            std::process::id()
        );
        let out_path = std::env::current_dir().expect("cwd").join(&out_relative);
        crate::scene::save_scene_with_registry(
            session.world(),
            "unlocked",
            &out_path,
            session.engine().registry(),
        )
        .expect("save scene");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(&out_path).expect("read saved scene"))
                .expect("parse saved scene");
        assert_eq!(
            saved["extensions"][crate::scene::COMPONENT_VERSIONS_KEY]["game.health"],
            json!(2),
            "without future instances the save records the registry version"
        );
        remove_test_scene(&out_relative);
    }

    #[test]
    fn version_table_rebuilds_from_actual_content() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor_at(2))
            .expect("register");
        let scene = json!({
            "format": "moni-scene",
            "version": 2,
            "name": "rebuild",
            "camera": {},
            "extensions": {
                "moni.componentVersions": { "game.health": 9, "vendor.custom": 3, "game.gone": 5 }
            },
            "entities": [
                {
                    "id": 1,
                    "name": "a",
                    "components": {
                        "core.transform": { "x": 0.0, "y": 0.0 },
                        "game.health": { "hp": 55, "futureField": true },
                        "vendor.custom": { "kept": true },
                    }
                }
            ]
        });
        let relative = write_test_scene("rebuild", &scene);
        session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert_eq!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .expect("retained table"),
            &json!({ "game.health": 9, "vendor.custom": 3 }),
            "the load keeps entries for live data and drops the entry without instances"
        );

        let out_relative = format!("target/moni-session-rebuild-{}.mnsc", std::process::id());
        let out_path = std::env::current_dir().expect("cwd").join(&out_relative);
        let save = |session: &RuntimeSession| {
            crate::scene::save_scene_with_registry(
                session.world(),
                "rebuild",
                &out_path,
                session.engine().registry(),
            )
            .expect("save scene");
            let saved: Value =
                serde_json::from_str(&fs::read_to_string(&out_path).expect("read saved scene"))
                    .expect("parse saved scene");
            saved
        };
        assert_eq!(
            save(&session)["extensions"][crate::scene::COMPONENT_VERSIONS_KEY],
            json!({ "game.health": 9, "vendor.custom": 3 }),
            "the save mirrors the retained entries of live data"
        );

        // Removing the future instance drops its entry; the unregistered
        // data still carries its retained entry.
        session
            .handle_line(&line(
                "1",
                "remove_component",
                json!({ "entity": 1, "component": "game.health" }),
            ))
            .0;
        assert_eq!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .expect("retained table"),
            &json!({ "vendor.custom": 3 }),
            "the future entry is dropped once no entity carries the component"
        );

        // Despawning the last carrier drops the unregistered entry too; the
        // registered but unused component never gains an entry.
        session
            .handle_line(&line("2", "despawn", json!({ "entity": 1 })))
            .0;
        assert!(
            session
                .world()
                .extensions
                .get(crate::scene::COMPONENT_VERSIONS_KEY)
                .is_none(),
            "no entries and no leaks remain once nothing is used"
        );
        assert_eq!(
            save(&session)["extensions"].get(crate::scene::COMPONENT_VERSIONS_KEY),
            None,
            "the save writes no version table for unused components"
        );
        remove_test_scene(&out_relative);
    }

    #[test]
    fn step_refuses_registered_but_unstarted_simulation() {
        let mut session = session();
        session
            .host_mut()
            .register_plugin(
                Box::new(crate::plugins::SimulationPlugin::new()),
                &["script.execute".to_string()],
            )
            .expect("simulation registers without starting");
        assert!(
            !session.host_mut().is_started(crate::host::CORE_SIMULATION),
            "register_plugin does not start the plugin"
        );
        let (response, _) = session.handle_line(&line("1", "step", json!({ "count": 2 })));
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("error body").code,
            "plugin_not_started"
        );
        assert_eq!(session.world().tick, 0, "no tick advanced");
        assert_eq!(session.revision(), 0, "no revision advanced");
    }

    #[test]
    fn unregistered_component_keys_survive_load_and_save_round_trip() {
        let mut session = session();
        let scene = json!({
            "format": "moni-scene",
            "version": 2,
            "name": "vendor",
            "camera": {},
            "extensions": {},
            "entities": [
                {
                    "id": 1,
                    "name": "a",
                    "components": {
                        "core.transform": { "x": 0.0, "y": 0.0 },
                        "vendor.custom": { "kept": true }
                    }
                }
            ]
        });
        let relative = write_test_scene("vendor", &scene);
        let result = session.load_scene(&relative).expect("load scene");
        remove_test_scene(&relative);
        assert_eq!(result["migrated"], json!([]));
        assert_eq!(result["preservedFuture"], json!([]));
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["vendor.custom"]["kept"],
            json!(true)
        );
        let out_relative = format!("target/moni-session-vendor-out-{}.mnsc", std::process::id());
        let out_path = std::env::current_dir().expect("cwd").join(&out_relative);
        crate::scene::save_scene(session.world(), "vendor", &out_path).expect("save scene");
        let reloaded = session.load_scene(&out_relative).expect("reload scene");
        remove_test_scene(&relative);
        assert_eq!(reloaded["entities"], json!(1));
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["vendor.custom"]["kept"],
            json!(true),
            "the unregistered key survives the load/save/load round trip"
        );
    }

    #[test]
    fn rename_and_restore_round_trip_entity_data_through_the_session() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor())
            .expect("register");
        session.handle_line(&line("1", "spawn", spawn_args("a")));
        session.handle_line(&line(
            "2",
            "attach_component",
            json!({ "entity": 1, "component": "game.health", "value": { "hp": 42 } }),
        ));
        session.handle_line(&line(
            "3",
            "rename_entity",
            json!({ "entity": 1, "name": "renamed" }),
        ));
        let snapshot = session.world().entities.get(&1).expect("entity").clone();
        assert_eq!(snapshot.name, "renamed");

        session.handle_line(&line("4", "despawn", json!({ "entity": 1 })));
        assert!(session.world().entities.is_empty());
        let (restored, _) =
            session.handle_line(&line("5", "restore_entity", json!({ "entity": snapshot })));
        assert!(restored.ok, "{:?}", restored.error);
        assert_eq!(
            restored.result.expect("result")["entity"],
            json!(1),
            "the explicit id survives the restore"
        );
        assert_eq!(
            session.world().entities.get(&1).expect("entity"),
            &snapshot,
            "despawn -> restore round trip is lossless"
        );
        assert_eq!(
            session.world().entities.get(&1).expect("entity").extensions["game.health"],
            json!({ "hp": 42, "max_hp": 100 }),
            "dynamic component data survives the restore"
        );
        assert_eq!(
            session.world().next_entity_id,
            2,
            "restore keeps the allocator past the restored id"
        );
        assert!(
            restored
                .events
                .iter()
                .any(|event| matches!(event, crate::engine::Event::EntitySpawned { entity: 1 }))
        );

        // Restoring onto a live id is refused without side effects.
        let revision = session.revision();
        let (again, _) =
            session.handle_line(&line("6", "restore_entity", json!({ "entity": snapshot })));
        assert!(!again.ok);
        assert_eq!(again.error.expect("error body").code, "entity_exists");
        assert_eq!(session.revision(), revision);
    }
}
