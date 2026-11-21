//! `moni mcp`: a Model Context Protocol stdio server that is ONLY a thin
//! adapter over the existing control plane. Every mutating tool builds the
//! same NDJSON request a CLI user would send and routes it through
//! `RuntimeSession::handle_line`, so revisions, batches, event semantics,
//! idempotency, and structured errors stay identical to `moni serve`. The
//! first batch exposes high-level operations only: capabilities, scene info,
//! scene load/save, entity spawn/despawn/transform, component attach/remove,
//! step, SVG render, screenshot, client run, and diagnostics.
//!
//! Threading: the engine is single-threaded and `RuntimeSession` is not
//! `Send`, so the session is built and used exclusively on a dedicated
//! engine thread. The rmcp handler (which must be `Send + Sync`) sends one
//! job at a time over an mpsc channel and blocks on the reply — MCP tool
//! calls are sequential over stdio, so this keeps engine access
//! single-threaded without introducing locks into engine code. Tokio and
//! rmcp stay confined to this module; nothing here leaks into engine code.

use std::future::Future;
use std::io::Read as _;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use moni_core::session::RuntimeSession;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer, ServiceExt as _};
use serde_json::{Value, json};

use crate::bootstrap;

/// How long a session tool may run before the MCP layer gives up. Large
/// `step` counts with scripted entities can legitimately take a while.
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Wall-clock budget for the screenshot child process (editor + GPU window).
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// Wall-clock budget for the windowed client child process.
const RUN_CLIENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Default frames for `screenshot` when the caller passes none: the editor
/// captures the golden PNG on its first rendered frame.
const SCREENSHOT_DEFAULT_FRAMES: u64 = 2;
/// Default frames for `run_client` when the caller passes none; the client
/// requires at least one frame to exit on its own.
const RUN_CLIENT_DEFAULT_FRAMES: u64 = 30;
/// Maximum number of drained events `diagnostics` reports.
const DIAGNOSTIC_EVENT_LIMIT: usize = 50;

/// One unit of work executed on the engine thread. The closure receives the
/// session and returns an already-shaped tool payload (see `handle_line_json`).
struct EngineJob {
    run: Box<dyn FnOnce(&mut RuntimeSession) -> Value + Send>,
    reply: mpsc::Sender<Value>,
}

/// State shared between the handler and the engine-thread bookkeeping. All
/// fields are guarded so the handler stays `Sync` as rmcp requires.
struct ServerState {
    /// The workspace-relative scene the session was built with or last
    /// loaded successfully; the default for `scene_save` and `screenshot`.
    scene_path: std::sync::Mutex<Option<String>>,
}

impl ServerState {
    fn scene_default(&self) -> Option<String> {
        self.scene_path.lock().ok().and_then(|guard| guard.clone())
    }

    fn set_scene_default(&self, path: Option<String>) {
        if let Ok(mut guard) = self.scene_path.lock() {
            *guard = path;
        }
    }
}

struct MoniMcpServer {
    jobs: mpsc::Sender<EngineJob>,
    state: Arc<ServerState>,
}

/// Monotonic request IDs for the protocol requests the tools send. Fresh IDs
/// keep the session's idempotent-replay cache out of the way: identical tool
/// calls execute again instead of replaying a cached NDJSON response. Agents
/// that want replay semantics should use `moni serve` directly.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> Value {
    Value::String(format!(
        "mcp-{}",
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Sends one protocol request through `RuntimeSession::handle_line` and
/// serializes the full response. This is the single path every protocol-backed
/// tool uses, so revision, batch, and event semantics stay identical to NDJSON.
fn handle_line_json(
    session: &mut RuntimeSession,
    command: &str,
    args: Value,
    expected_revision: Option<u64>,
) -> Value {
    let id = next_request_id();
    let mut request = json!({ "id": id, "command": command, "args": args });
    if let Some(revision) = expected_revision {
        request["expectedRevision"] = json!(revision);
    }
    let (response, quit) = session.handle_line(&request.to_string());
    let mut payload = serde_json::to_value(response).unwrap_or_else(|error| {
        json!({
            "ok": false,
            "error": {
                "code": "serialization_error",
                "message": format!("could not serialize protocol response: {error}"),
            },
        })
    });
    // The NDJSON `quit` command is not exposed as a tool; if a plugin command
    // ever asked to quit, surface it instead of silently ignoring it.
    payload["quitRequested"] = json!(quit);
    payload
}

/// Shapes a failed engine call the way `handle_line` would have returned it
/// (used by tools that call `RuntimeSession` methods directly, like
/// `scene_load`).
fn engine_error_value(error: moni_core::engine::EngineError, session: &RuntimeSession) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": error.code,
            "message": error.message,
            "details": error.details,
        },
        "events": [],
        "revision": session.revision(),
        "eventSeq": session.event_seq(),
    })
}

/// Argument-shape error, matching the protocol's `invalid_arguments` code so
/// tool callers see the same error vocabulary as NDJSON clients.
fn invalid_arguments_error(message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": "invalid_arguments",
            "message": message.into(),
        },
    })
}

/// Converts an MCP infrastructure error (engine thread gone, reply timeout)
/// into the protocol error envelope so tool callers keep seeing
/// {code, message, details} instead of a JSON-RPC fault.
fn mcp_error_payload(error: McpError) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": "mcp_internal",
            "message": error.message,
            "details": error.data,
        },
    })
}

fn payload_is_ok(payload: &Value) -> bool {
    payload.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

impl MoniMcpServer {
    /// Sends one tool closure to the engine thread and waits for the reply.
    fn session_tool(
        &self,
        tool: impl FnOnce(&mut RuntimeSession) -> Value + Send + 'static,
    ) -> Result<Value, McpError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let job = EngineJob {
            run: Box::new(tool),
            reply: reply_tx,
        };
        self.jobs
            .send(job)
            .map_err(|_| McpError::internal_error("the MCP engine thread is gone", None))?;
        reply_rx.recv_timeout(TOOL_CALL_TIMEOUT).map_err(|_| {
            McpError::internal_error(
                format!(
                    "the engine tool call timed out after {}s; the session may still be busy",
                    TOOL_CALL_TIMEOUT.as_secs()
                ),
                None,
            )
        })
    }

    /// Runs one tool by name and returns its payload in the protocol response
    /// envelope shape (ok / error). Argument errors and infrastructure
    /// failures carry the same envelope as protocol failures.
    fn dispatch_tool_call(&self, name: &str, arguments: &JsonObject) -> Value {
        let outcome: Result<Value, Value> = self.dispatch_tool_call_inner(name, arguments);
        outcome.unwrap_or_else(std::convert::identity)
    }

    fn dispatch_tool_call_inner(&self, name: &str, arguments: &JsonObject) -> Result<Value, Value> {
        match name {
            "capabilities" => Ok(self
                .session_tool(|session| session_tools::capabilities(session))
                .map_err(mcp_error_payload)?),
            "scene_info" => Ok(self
                .session_tool(|session| session_tools::scene_info(session))
                .map_err(mcp_error_payload)?),
            "scene_load" => {
                let path = optional_string(arguments, "path")?.ok_or_else(|| {
                    invalid_arguments_error("scene_load requires a 'path' argument")
                })?;
                let tracked = path.clone();
                let outcome = self
                    .session_tool(move |session| session_tools::scene_load(session, &path))
                    .map_err(mcp_error_payload)?;
                if payload_is_ok(&outcome) {
                    self.state.set_scene_default(Some(tracked));
                }
                Ok(outcome)
            }
            "scene_save" => {
                let path = match optional_string(arguments, "path")? {
                    Some(path) => path,
                    None => self.state.scene_default().ok_or_else(|| {
                        invalid_arguments_error(
                            "scene_save needs a 'path' argument (no session scene is known)",
                        )
                    })?,
                };
                Ok(self
                    .session_tool(move |session| session_tools::scene_save(session, &path))
                    .map_err(mcp_error_payload)?)
            }
            "entity_spawn" => {
                let spec = arguments.get("spec").cloned().ok_or_else(|| {
                    invalid_arguments_error("entity_spawn requires a 'spec' EntitySpec object")
                })?;
                if !spec.is_object() {
                    return Err(invalid_arguments_error(
                        "argument 'spec' must be an EntitySpec object",
                    ));
                }
                let expected_revision = optional_u64(arguments, "expectedRevision")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::entity_spawn(session, spec, expected_revision)
                    })
                    .map_err(mcp_error_payload)?)
            }
            "entity_despawn" => {
                let entity = arguments
                    .get("entity")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_arguments_error("entity_despawn requires an 'entity' id")
                    })?;
                let expected_revision = optional_u64(arguments, "expectedRevision")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::entity_despawn(session, entity, expected_revision)
                    })
                    .map_err(mcp_error_payload)?)
            }
            "entity_set_transform" => {
                let entity = arguments
                    .get("entity")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_arguments_error("entity_set_transform requires an 'entity' id")
                    })?;
                let transform = arguments.get("transform").cloned().ok_or_else(|| {
                    invalid_arguments_error("entity_set_transform requires a 'transform' object")
                })?;
                if !transform.is_object() {
                    return Err(invalid_arguments_error(
                        "argument 'transform' must be an object",
                    ));
                }
                let expected_revision = optional_u64(arguments, "expectedRevision")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::entity_set_transform(
                            session,
                            entity,
                            transform,
                            expected_revision,
                        )
                    })
                    .map_err(mcp_error_payload)?)
            }
            "component_attach" => {
                let entity = arguments
                    .get("entity")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_arguments_error("component_attach requires an 'entity' id")
                    })?;
                let component = arguments
                    .get("component")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_arguments_error("component_attach requires a 'component' id")
                    })?
                    .to_string();
                let value = arguments.get("value").cloned().unwrap_or_else(|| json!({}));
                let expected_revision = optional_u64(arguments, "expectedRevision")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::component_attach(
                            session,
                            entity,
                            &component,
                            value,
                            expected_revision,
                        )
                    })
                    .map_err(mcp_error_payload)?)
            }
            "component_remove" => {
                let entity = arguments
                    .get("entity")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_arguments_error("component_remove requires an 'entity' id")
                    })?;
                let component = arguments
                    .get("component")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_arguments_error("component_remove requires a 'component' id")
                    })?
                    .to_string();
                let expected_revision = optional_u64(arguments, "expectedRevision")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::component_remove(
                            session,
                            entity,
                            &component,
                            expected_revision,
                        )
                    })
                    .map_err(mcp_error_payload)?)
            }
            "step" => {
                let count = arguments
                    .get("count")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        invalid_arguments_error("step requires a 'count' of ticks (1..10000)")
                    })?;
                Ok(self
                    .session_tool(move |session| session_tools::step(session, count))
                    .map_err(mcp_error_payload)?)
            }
            "render_svg" => {
                let path = optional_string(arguments, "path")?;
                Ok(self
                    .session_tool(move |session| {
                        session_tools::render_svg(session, path.as_deref())
                    })
                    .map_err(mcp_error_payload)?)
            }
            "screenshot" => screenshot_tool(self.state.scene_default(), arguments),
            "run_client" => run_client_tool(self.state.scene_default(), arguments),
            "diagnostics" => Ok(self
                .session_tool(session_tools::diagnostics)
                .map_err(mcp_error_payload)?),
            _ => {
                // TOOL_NAMES guards dispatch; unreachable in practice.
                Err(invalid_arguments_error(format!(
                    "tool '{name}' is registered but not dispatched"
                )))
            }
        }
    }
}

/// Entry point for `moni mcp`. Runs the stdio server until the client closes
/// stdin; normal termination is a clean EOF quit.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let (scene, use_project) = parse_mcp_args(args)?;
    let scene = match scene {
        Some(path) => Some(path),
        None if use_project => Some(project_scene_path()?),
        None => None,
    };
    let state = Arc::new(ServerState {
        scene_path: std::sync::Mutex::new(scene.clone()),
    });

    // The engine thread owns the RuntimeSession for the whole server lifetime.
    // The session is neither Send nor Sync, so it is built and used only on
    // this thread; tools reach it one job at a time through the channel.
    let (jobs_tx, jobs_rx) = mpsc::channel::<EngineJob>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let _engine_thread = std::thread::Builder::new()
        .name("moni-mcp-engine".into())
        .spawn({
            let initial_scene = scene.clone();
            move || {
                let mut session = match bootstrap::mcp_session(initial_scene.as_deref()) {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                while let Ok(job) = jobs_rx.recv() {
                    let outcome = (job.run)(&mut session);
                    let _ = job.reply.send(outcome);
                }
                // The job channel closed because the service dropped the
                // handler; the session drops here, running plugin shutdown in
                // the ordinary order.
            }
        })
        .context("could not start the MCP engine thread")?;
    ready_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("the MCP engine thread exited before reporting readiness"))?
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    // rmcp is async-first; a current-thread runtime is enough because tool
    // calls are sequential over stdio. Everything async stays in this function.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the tokio runtime for the MCP server")?;
    let server = MoniMcpServer {
        jobs: jobs_tx,
        state: Arc::clone(&state),
    };
    let result = runtime.block_on(async move {
        match server.serve(rmcp::transport::stdio()).await {
            Ok(service) => match service.waiting().await {
                Ok(reason) => Ok(reason),
                Err(error) => Err(anyhow::anyhow!("MCP service task failed: {error}")),
            },
            Err(error) => Err(anyhow::anyhow!("MCP server failed to initialize: {error}")),
        }
    });
    let reason = result?;
    // Cap the wait for blocking stdin tasks during shutdown so a stubborn
    // console handle cannot hang the process after the client is gone.
    runtime.shutdown_timeout(Duration::from_secs(2));
    // The engine thread exits when the service drops the handler (closing the
    // job channel) or with process exit; rmcp may still hold handler clones in
    // finishing tasks, so joining here is not reliable and not required.
    eprintln!("mcp: server quit ({reason:?})");
    Ok(())
}

fn parse_mcp_args(args: Vec<String>) -> anyhow::Result<(Option<String>, bool)> {
    let mut scene = None;
    let mut project = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--scene" => {
                index += 1;
                scene = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--scene requires a path"))?
                        .clone(),
                );
            }
            "--project" => project = true,
            unknown => bail!("unknown mcp option '{unknown}'"),
        }
        index += 1;
    }
    if scene.is_some() && project {
        bail!("--scene and --project are mutually exclusive");
    }
    Ok((scene, project))
}

/// Resolves the current `.mngp` project's scene as a workspace-relative path
/// for `moni mcp --project`.
fn project_scene_path() -> anyhow::Result<String> {
    let root = std::env::current_dir().context("could not resolve the current directory")?;
    let project = moni_core::project::GameProject::load(&root)?;
    let scene = project.scene_path(&root)?;
    let relative = if scene.is_absolute() {
        scene
            .strip_prefix(&root)
            .with_context(|| {
                format!(
                    "project scene {} is outside the project root",
                    scene.display()
                )
            })?
            .to_path_buf()
    } else {
        scene
    };
    relative
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("project scene path is not valid UTF-8"))
}

// ---------------------------------------------------------------------------
// Tool implementations that run on the engine thread
// ---------------------------------------------------------------------------

mod session_tools {
    use super::*;

    /// Static protocol capabilities plus the session overlay.
    pub fn capabilities(session: &mut RuntimeSession) -> Value {
        handle_line_json(session, "capabilities", json!({}), None)
    }

    /// Compact scene summary derived from the protocol `query` response.
    pub fn scene_info(session: &mut RuntimeSession) -> Value {
        let response = handle_line_json(session, "query", json!({}), None);
        if !payload_is_ok(&response) {
            return response;
        }
        let world = &response["result"];
        let mut entities = Vec::new();
        if let Some(map) = world["entities"].as_object() {
            for (id, entity) in map {
                let mut components = vec![json!("transform")];
                if entity["sprite"].is_object() {
                    components.push(json!("sprite"));
                }
                if entity["model"].is_object() {
                    components.push(json!("model"));
                }
                if entity["script"].is_object() {
                    components.push(json!("script"));
                }
                if let Some(extensions) = entity["extensions"].as_object() {
                    components.extend(extensions.keys().map(|key| json!(key)));
                }
                entities.push(json!({
                    "id": id.parse::<u64>().map(Value::from).unwrap_or_else(|_| json!(id)),
                    "name": entity["name"],
                    "components": components,
                }));
            }
        }
        json!({
            "ok": true,
            "result": {
                "entityCount": entities.len(),
                "entities": entities,
            "tick": world["tick"],
            "tickRate": world["tick_rate"],
                "camera": world["camera"].clone(),
                "lights": world["lights"].clone(),
            },
            "events": response["events"],
            "revision": response["revision"],
            "eventSeq": response["seq"],
        })
    }

    /// Loads a `.mnsc` scene through `RuntimeSession::load_scene` — the same
    /// path the CLI and editor use (protocol `load` loads world snapshots
    /// instead, so scene files go through the session method).
    pub fn scene_load(session: &mut RuntimeSession, path: &str) -> Value {
        let id = next_request_id();
        match session.load_scene(path) {
            Ok(result) => {
                let events = session.host_mut().take_events();
                json!({
                    "id": id,
                    "ok": true,
                    "result": result,
                    "events": serde_json::to_value(&events).unwrap_or_else(|_| json!([])),
                    "revision": session.revision(),
                    "eventSeq": session.event_seq(),
                })
            }
            Err(error) => engine_error_value(error, session),
        }
    }

    /// Protocol `save`: writes the current world as a JSON snapshot (the
    /// world-snapshot format, not the `.mnsc` scene format).
    pub fn scene_save(session: &mut RuntimeSession, path: &str) -> Value {
        handle_line_json(session, "save", json!({ "path": path }), None)
    }

    pub fn entity_spawn(
        session: &mut RuntimeSession,
        spec: Value,
        expected_revision: Option<u64>,
    ) -> Value {
        handle_line_json(session, "spawn", spec, expected_revision)
    }

    pub fn entity_despawn(
        session: &mut RuntimeSession,
        entity: u64,
        expected_revision: Option<u64>,
    ) -> Value {
        handle_line_json(
            session,
            "despawn",
            json!({ "entity": entity }),
            expected_revision,
        )
    }

    pub fn entity_set_transform(
        session: &mut RuntimeSession,
        entity: u64,
        transform: Value,
        expected_revision: Option<u64>,
    ) -> Value {
        handle_line_json(
            session,
            "set_transform",
            json!({ "entity": entity, "transform": transform }),
            expected_revision,
        )
    }

    pub fn component_attach(
        session: &mut RuntimeSession,
        entity: u64,
        component: &str,
        value: Value,
        expected_revision: Option<u64>,
    ) -> Value {
        handle_line_json(
            session,
            "attach_component",
            json!({ "entity": entity, "component": component, "value": value }),
            expected_revision,
        )
    }

    pub fn component_remove(
        session: &mut RuntimeSession,
        entity: u64,
        component: &str,
        expected_revision: Option<u64>,
    ) -> Value {
        handle_line_json(
            session,
            "remove_component",
            json!({ "entity": entity, "component": component }),
            expected_revision,
        )
    }

    /// Protocol `step`, never inside a batch: script hosts run their onTick
    /// hooks every tick and script state cannot be rolled back.
    pub fn step(session: &mut RuntimeSession, count: u64) -> Value {
        handle_line_json(session, "step", json!({ "count": count }), None)
    }

    /// Protocol `render`: the diagnostic SVG wireframe, not the GPU frame.
    pub fn render_svg(session: &mut RuntimeSession, path: Option<&str>) -> Value {
        let args = match path {
            Some(path) => json!({ "path": path }),
            None => json!({}),
        };
        handle_line_json(session, "render", args, None)
    }

    /// Doctor-style diagnostics the session can provide without a project.
    pub fn diagnostics(session: &mut RuntimeSession) -> Value {
        let capabilities = handle_line_json(session, "capabilities", json!({}), None);
        let session_block = capabilities["result"]["session"].clone();
        let events = session.host_mut().take_events();
        let drained = events.len();
        let recent = if drained > DIAGNOSTIC_EVENT_LIMIT {
            &events[drained - DIAGNOSTIC_EVENT_LIMIT..]
        } else {
            &events[..]
        };
        json!({
            "ok": true,
            "result": {
                "session": session_block,
                "revision": session.revision(),
                "eventSeq": session.event_seq(),
                "scriptHosts": session.host_mut().script_host_count(),
                "events": serde_json::to_value(recent).unwrap_or_else(|_| json!([])),
                "drainedEvents": drained,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Argument extraction and child-process tools (screenshot, run_client)
// ---------------------------------------------------------------------------

/// Extracts an optional unsigned-integer argument, reporting shape problems
/// the way the protocol does.
fn optional_u64(arguments: &JsonObject, key: &str) -> Result<Option<u64>, Value> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            invalid_arguments_error(format!("argument '{key}' must be a non-negative integer"))
        }),
    }
}

fn optional_string(arguments: &JsonObject, key: &str) -> Result<Option<String>, Value> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(invalid_arguments_error(format!(
            "argument '{key}' must be a string"
        ))),
    }
}

fn child_process_error(code: &str, message: String, details: Value) -> Value {
    json!({
        "ok": false,
        "error": { "code": code, "message": message, "details": details },
    })
}

/// Validates a workspace-relative path without creating anything.
fn checked_workspace_path(raw: &str, label: &str) -> Result<(), Value> {
    moni_core::workspace_path(raw).map(|_| ()).map_err(|error| {
        invalid_arguments_error(format!(
            "{label} is not a valid workspace-relative path: {error}"
        ))
    })
}

/// `screenshot`: spawns the engine's editor exactly like the isolated FBX
/// importer, using its byte-stable golden capture to write a real PNG. This
/// opens a window on machines with a GPU and fails headless. The session is
/// untouched; the error/success envelope keeps the protocol shape.
fn screenshot_tool(scene_default: Option<String>, arguments: &JsonObject) -> Result<Value, Value> {
    let output = arguments
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid_arguments_error("screenshot requires an 'output' workspace-relative PNG path")
        })?
        .to_string();
    checked_workspace_path(&output, "output")?;
    let scene = match optional_string(arguments, "scene")? {
        Some(scene) => scene,
        None => scene_default.ok_or_else(|| {
            invalid_arguments_error(
                "screenshot requires a 'scene' argument or a session scene (--scene, --project, or scene_load)",
            )
        })?,
    };
    checked_workspace_path(&scene, "scene")?;
    let frames = match optional_u64(arguments, "frames")? {
        Some(0) => {
            return Err(invalid_arguments_error(
                "argument 'frames' must be at least 1",
            ));
        }
        Some(frames) => frames,
        None => SCREENSHOT_DEFAULT_FRAMES,
    };
    let executable = std::env::current_exe().map_err(|error| {
        child_process_error(
            "screenshot_failed",
            format!("could not resolve the engine executable: {error}"),
            json!({}),
        )
    })?;
    let child = ProcessCommand::new(executable)
        .args([
            "editor",
            "--scene",
            &scene,
            "--frames",
            &frames.to_string(),
            "--golden",
            &output,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = child.map_err(|error| {
        child_process_error(
            "screenshot_failed",
            format!("could not start the editor child process: {error}"),
            json!({ "scene": scene, "output": output }),
        )
    })?;
    let deadline = Instant::now() + SCREENSHOT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return Err(child_process_error(
                    "screenshot_failed",
                    format!("could not poll the editor child process: {error}"),
                    json!({ "scene": scene, "output": output }),
                ));
            }
        }
    };
    let stderr = child
        .stderr
        .take()
        .map(|mut pipe| {
            let mut buffer = String::new();
            let _ = pipe.read_to_string(&mut buffer);
            buffer
        })
        .unwrap_or_default();
    match status {
        Some(status) if status.success() => {
            let written = moni_core::workspace_path(&output)
                .map(|path| path.is_file())
                .unwrap_or(false);
            if !written {
                return Err(child_process_error(
                    "screenshot_failed",
                    format!("the editor reported success but did not write {output}"),
                    json!({ "scene": scene, "frames": frames }),
                ));
            }
            Ok(json!({
                "ok": true,
                "result": {
                    "output": output,
                    "scene": scene,
                    "frames": frames,
                    "child": "editor --golden (byte-stable PNG capture)",
                },
            }))
        }
        Some(status) => Err(child_process_error(
            "screenshot_failed",
            format!(
                "the editor child process failed with status {status}; it needs a machine with a \
                 GPU and a window station (headless environments fail by design): {}",
                stderr.trim()
            ),
            json!({
                "scene": scene,
                "output": output,
                "frames": frames,
                "exitStatus": status.code(),
            }),
        )),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(child_process_error(
                "screenshot_timeout",
                format!(
                    "the editor child process timed out after {}s and was killed",
                    SCREENSHOT_TIMEOUT.as_secs()
                ),
                json!({ "scene": scene, "output": output, "frames": frames }),
            ))
        }
    }
}

/// `run_client`: spawns the windowed client as a child process and returns
/// the final JSON it prints. This opens a real window; headless machines
/// fail. The session is untouched.
fn run_client_tool(scene_default: Option<String>, arguments: &JsonObject) -> Result<Value, Value> {
    let scene_arg = optional_string(arguments, "scene")?;
    let frames = match optional_u64(arguments, "frames")? {
        Some(0) => {
            return Err(invalid_arguments_error(
                "argument 'frames' must be at least 1",
            ));
        }
        Some(frames) => frames,
        None => RUN_CLIENT_DEFAULT_FRAMES,
    };
    let executable = std::env::current_exe().map_err(|error| {
        child_process_error(
            "run_client_failed",
            format!("could not resolve the engine executable: {error}"),
            json!({}),
        )
    })?;
    // The client accepts either a bundled demo scene ("2d"/"3d") or a
    // workspace-relative scene path via --world.
    let resolved_scene = scene_arg.or_else(|| scene_default.clone());
    let mut command = ProcessCommand::new(executable);
    command.arg("client");
    let mut scene_label = "2d".to_string();
    match resolved_scene.as_deref() {
        Some(scene @ ("2d" | "3d")) => {
            scene_label = scene.to_string();
            command.args(["--scene", scene]);
        }
        Some(scene) => {
            checked_workspace_path(scene, "scene")?;
            scene_label = scene.to_string();
            command.args(["--world", scene]);
        }
        None => {}
    }
    command.args(["--frames", &frames.to_string()]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        child_process_error(
            "run_client_failed",
            format!("could not start the client child process: {error}"),
            json!({ "scene": scene_label, "frames": frames }),
        )
    })?;
    let deadline = Instant::now() + RUN_CLIENT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return Err(child_process_error(
                    "run_client_failed",
                    format!("could not poll the client child process: {error}"),
                    json!({ "scene": scene_label, "frames": frames }),
                ));
            }
        }
    };
    // The client prints its final JSON right before exiting, so reading the
    // pipes after the status settles cannot deadlock on a full pipe.
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    match status {
        Some(status) if status.success() => {
            let client_json = final_client_json(&stdout).ok_or_else(|| {
                child_process_error(
                    "run_client_failed",
                    "the client child process succeeded but printed no final JSON summary"
                        .to_string(),
                    json!({ "stdoutTail": stdout_tail(&stdout, 2000) }),
                )
            })?;
            Ok(json!({
                "ok": true,
                "result": {
                    "scene": scene_label,
                    "frames": frames,
                    "child": "client (windowed wgpu run)",
                    "client": client_json,
                },
            }))
        }
        Some(status) => Err(child_process_error(
            "run_client_failed",
            format!(
                "the client child process failed with status {status}; it needs a machine with a \
                 GPU and a window station (headless environments fail by design): {}",
                stderr.trim()
            ),
            json!({
                "scene": scene_label,
                "frames": frames,
                "exitStatus": status.code(),
            }),
        )),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(child_process_error(
                "run_client_timeout",
                format!(
                    "the client child process timed out after {}s and was killed",
                    RUN_CLIENT_TIMEOUT.as_secs()
                ),
                json!({ "scene": scene_label, "frames": frames }),
            ))
        }
    }
}

/// Finds the final JSON summary the client prints to stdout (the last JSON
/// object in the stream).
fn final_client_json(stdout: &str) -> Option<Value> {
    let start = stdout.rfind('{')?;
    serde_json::from_str(&stdout[start..]).ok()
}

/// The tail of a string, respecting char boundaries (for error payloads).
fn stdout_tail(stdout: &str, max_chars: usize) -> String {
    if stdout.chars().count() <= max_chars {
        return stdout.to_string();
    }
    stdout
        .chars()
        .skip(stdout.chars().count() - max_chars)
        .collect()
}

// ---------------------------------------------------------------------------
// Tool table
// ---------------------------------------------------------------------------

/// Every tool name in the first batch, in listing order.
const TOOL_NAMES: [&str; 14] = [
    "capabilities",
    "scene_info",
    "scene_load",
    "scene_save",
    "entity_spawn",
    "entity_despawn",
    "entity_set_transform",
    "component_attach",
    "component_remove",
    "step",
    "render_svg",
    "screenshot",
    "run_client",
    "diagnostics",
];

fn readonly_annotations(title: &'static str) -> ToolAnnotations {
    ToolAnnotations::from_raw(
        Some(title.to_string()),
        Some(true),
        Some(false),
        Some(true),
        Some(false),
    )
}

fn mutating_annotations(
    title: &'static str,
    destructive: bool,
    idempotent: bool,
) -> ToolAnnotations {
    ToolAnnotations::from_raw(
        Some(title.to_string()),
        Some(false),
        Some(destructive),
        Some(idempotent),
        Some(false),
    )
}

fn tool(
    name: &'static str,
    description: &'static str,
    schema: Value,
    annotations: ToolAnnotations,
) -> Tool {
    let input_schema = match schema {
        Value::Object(map) => Arc::new(map),
        _ => panic!("tool input schemas must be JSON objects"),
    };
    Tool::new(name, description, input_schema).with_annotations(annotations)
}

/// Builds the first batch of tools. Descriptions state side effects honestly:
/// mutating tools say so, `step` says it advances script state, `screenshot`
/// and `run_client` say they open a real window, and `render_svg` says it is
/// the diagnostic wireframe rather than the GPU frame.
fn tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "capabilities",
            "Read the engine's control-plane capabilities: the static protocol description (world, commands, transactions, components, profiles, host, security, script, events) plus this session's overlay (profile, revision, plugins, services, plugin commands, component registry). Read-only; changes nothing.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            readonly_annotations("Get capabilities"),
        ),
        tool(
            "scene_info",
            "Read a compact scene summary: entity count with per-entity id/name/component list, world revision, tick and tick rate, camera, and lights. Read-only; for full entity or world snapshots use the NDJSON protocol (moni serve) query command.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            readonly_annotations("Get scene info"),
        ),
        tool(
            "scene_load",
            "Load a workspace-relative .mnsc scene file into the session (RuntimeSession::load_scene, the same validation path the CLI and editor use). MUTATES the world: on success it replaces the world, revision, and script hosts; a failed load leaves the session untouched and reports invalid_component or invalid_scene.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "workspace-relative .mnsc scene path" },
                },
                "required": ["path"],
            }),
            mutating_annotations("Load scene", true, false),
        ),
        tool(
            "scene_save",
            "Save the current world through the protocol 'save' command. WRITES A FILE inside the workspace; the payload is the world-snapshot JSON format (the same one 'moni serve' save writes), NOT the .mnsc scene format. Does not mutate the world.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "workspace-relative output path; defaults to the session scene if one is known" },
                },
            }),
            mutating_annotations("Save scene snapshot", false, true),
        ),
        tool(
            "entity_spawn",
            "Spawn an entity from an EntitySpec (same schema as the protocol 'spawn' command: name, transform, sprite, model, script). MUTATES the world, bumps the revision, and emits entity_spawned. Pass expectedRevision for optimistic concurrency; mismatches fail with revision_conflict before any change.",
            json!({
                "type": "object",
                "properties": {
                    "spec": { "type": "object", "description": "EntitySpec exactly as the protocol spawn command takes it" },
                    "expectedRevision": { "type": "integer", "description": "fail unless the world revision matches" },
                },
                "required": ["spec"],
            }),
            mutating_annotations("Spawn entity", false, false),
        ),
        tool(
            "entity_despawn",
            "Remove an entity by id (protocol 'despawn'). MUTATES the world, bumps the revision, and emits entity_despawned. Pass expectedRevision for optimistic concurrency.",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "integer", "description": "entity id" },
                    "expectedRevision": { "type": "integer" },
                },
                "required": ["entity"],
            }),
            mutating_annotations("Despawn entity", true, false),
        ),
        tool(
            "entity_set_transform",
            "Set an entity's transform (protocol 'set_transform'; the transform object mirrors the protocol shape: x, y, rotation, scale_x, scale_y). MUTATES the world and emits transform_changed. Pass expectedRevision for optimistic concurrency.",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "integer" },
                    "transform": { "type": "object", "description": "2D transform object as the protocol set_transform command takes it" },
                    "expectedRevision": { "type": "integer" },
                },
                "required": ["entity", "transform"],
            }),
            mutating_annotations("Set entity transform", false, false),
        ),
        tool(
            "component_attach",
            "Attach a registered dynamic component to an entity (protocol 'attach_component'). Core components are reserved; schema defaults are applied before validation and schema violations fail without touching the session. MUTATES the world. Pass expectedRevision for optimistic concurrency.",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "integer" },
                    "component": { "type": "string", "description": "registered dynamic component id (see capabilities.components)" },
                    "value": { "type": "object", "description": "component value; defaults to {} and receives schema defaults" },
                    "expectedRevision": { "type": "integer" },
                },
                "required": ["entity", "component"],
            }),
            mutating_annotations("Attach component", false, false),
        ),
        tool(
            "component_remove",
            "Remove a component from an entity (protocol 'remove_component'). MUTATES the world and emits component_removed. Pass expectedRevision for optimistic concurrency.",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "integer" },
                    "component": { "type": "string" },
                    "expectedRevision": { "type": "integer" },
                },
                "required": ["entity", "component"],
            }),
            mutating_annotations("Remove component", false, false),
        ),
        tool(
            "step",
            "Advance the fixed-step simulation by 'count' ticks through the protocol 'step' command (never inside a batch). SIDE EFFECTS: script hosts run their onTick hooks every tick and script state cannot be rolled back; revision and events advance. Calling again with the same count ticks again — this tool is not idempotent.",
            json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "description": "ticks to advance, 1..10000" },
                },
                "required": ["count"],
            }),
            mutating_annotations("Step simulation", false, false),
        ),
        tool(
            "render_svg",
            "Render the current world as the diagnostic SVG wireframe (protocol 'render'). This is the 2D/3D diagnostic wireframe output, NOT the GPU frame — use screenshot for the real window capture. With 'path' it WRITES a workspace-relative SVG file; without one it returns the SVG source inline. Does not mutate the world.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "workspace-relative output path; omit to return the SVG inline" },
                },
            }),
            mutating_annotations("Render SVG", false, true),
        ),
        tool(
            "screenshot",
            "Capture a PNG of the real rendered window by spawning the engine's editor as a child process ('moni editor --scene <s> --frames <n> --golden <out>'; byte-stable golden capture). OPENS A REAL WINDOW: it needs a machine with a GPU and a window station and fails headless by design. This session is untouched; the tool WRITES a PNG file. Defaults to the session scene and 2 frames.",
            json!({
                "type": "object",
                "properties": {
                    "scene": { "type": "string", "description": "workspace-relative .mnsc scene; defaults to the session scene" },
                    "frames": { "type": "integer", "description": "frames the editor renders before exiting (capture happens on the first rendered frame)" },
                    "output": { "type": "string", "description": "workspace-relative PNG output path" },
                },
                "required": ["output"],
            }),
            mutating_annotations("Capture screenshot", false, false),
        ),
        tool(
            "run_client",
            "Run the windowed wgpu client as a child process ('moni client --scene 2d|3d' or '--world <workspace-relative path>') for 'frames' frames and return the final JSON the client prints (tick, entities, frames, loaded mesh vertices). OPENS A REAL WINDOW and fails headless; bundled demo scenes ('2d'/'3d') require 'npm run build' for the example scripts. This session is untouched.",
            json!({
                "type": "object",
                "properties": {
                    "scene": { "type": "string", "description": "\"2d\", \"3d\", or a workspace-relative .mnsc scene path" },
                    "frames": { "type": "integer", "description": "frames to render before the client exits (default 30, minimum 1)" },
                },
            }),
            mutating_annotations("Run windowed client", false, false),
        ),
        tool(
            "diagnostics",
            "Return doctor-style session diagnostics without a project: the capabilities session block (profile, plugins, services, plugin commands), current revision and event watermark, script host count, and the last drained events. Read-only; note that draining CONSUMES buffered events — they are reported here instead of the next response.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            readonly_annotations("Get diagnostics"),
        ),
    ]
}

/// Maps a tool payload to an MCP result: ok stays structured content, a
/// failed protocol response becomes an isError result that carries the full
/// structured error {code, message, details} — no lossy stringification.
fn tool_result_from_payload(payload: Value) -> CallToolResult {
    if payload_is_ok(&payload) {
        CallToolResult::structured(payload)
    } else {
        CallToolResult::structured_error(payload)
    }
}

impl ServerHandler for MoniMcpServer {
    fn get_info(&self) -> ServerConfig {
        // ServerConfig is non_exhaustive, so build through the constructor and
        // set the remaining fields afterwards.
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        config.server_info =
            rmcp::model::Implementation::new("moni-engine", env!("CARGO_PKG_VERSION"));
        config.instructions = Some(
            "Moni Engine control-plane adapter. Every mutating tool builds the same NDJSON \
             request a CLI user would send and routes it through RuntimeSession::handle_line, \
             so revisions, batches, events, and errors are identical to 'moni serve'. One \
             session lives for the whole server lifetime; pass expectedRevision on mutating \
             tools for optimistic concurrency and call diagnostics to see the session state."
                .to_string(),
        );
        config
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tool_definitions())))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !TOOL_NAMES.contains(&name.as_str()) {
            return Err(McpError::invalid_params(
                format!("unknown tool '{name}'; see tools/list"),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        let payload = self.dispatch_tool_call(&name, &arguments);
        Ok(CallToolResponse::Complete(tool_result_from_payload(
            payload,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moni_core::components::{ComponentDescriptor, PropertyField, PropertySchema, PropertyType};
    use std::fs;

    fn session() -> RuntimeSession {
        bootstrap::mcp_session(None).expect("mcp session builds")
    }

    fn args(pairs: &[(&str, Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn payload_error(payload: &Value) -> &Value {
        payload.get("error").expect("error envelope")
    }

    fn write_test_scene(name: &str, entities: Value) -> String {
        let relative = format!("target/moni-mcp-{name}-{}.mnsc", std::process::id());
        let path = std::env::current_dir().expect("cwd").join(&relative);
        fs::create_dir_all(path.parent().expect("target parent")).expect("create target dir");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "format": "moni-scene",
                "version": 2,
                "name": name,
                "camera": {},
                "entities": entities,
            }))
            .expect("scene json"),
        )
        .expect("write scene file");
        relative
    }

    fn remove_test_file(relative: &str) {
        fs::remove_file(std::env::current_dir().expect("cwd").join(relative)).ok();
    }

    fn health_descriptor() -> ComponentDescriptor {
        ComponentDescriptor::dynamic(
            "game.health",
            None,
            "game.test",
            1,
            PropertySchema::new(
                "Health",
                "Dynamic test component",
                vec![
                    PropertyField::new("hp", PropertyType::Number, false)
                        .default(json!(100))
                        .range(0.0, 1000.0),
                ],
            ),
        )
    }

    #[test]
    fn spawn_query_save_despawn_round_trip_through_tools() {
        let mut session = session();

        let spawn = session_tools::entity_spawn(&mut session, json!({ "name": "agent-box" }), None);
        assert!(payload_is_ok(&spawn), "{spawn}");
        assert_eq!(spawn["result"]["entity"], json!(1));
        assert_eq!(spawn["revision"], json!(1));

        let info = session_tools::scene_info(&mut session);
        assert!(payload_is_ok(&info), "{info}");
        assert_eq!(info["result"]["entityCount"], json!(1));
        assert_eq!(info["result"]["entities"][0]["name"], json!("agent-box"));
        assert_eq!(info["result"]["tick"], json!(0));
        assert_eq!(info["result"]["tickRate"], json!(60));

        let save_path = format!("target/moni-mcp-save-{}.json", std::process::id());
        let save = session_tools::scene_save(&mut session, &save_path);
        assert!(payload_is_ok(&save), "{save}");
        assert!(
            std::env::current_dir()
                .expect("cwd")
                .join(&save_path)
                .is_file(),
            "the protocol save wrote a snapshot file"
        );
        remove_test_file(&save_path);

        let despawn = session_tools::entity_despawn(&mut session, 1, None);
        assert!(payload_is_ok(&despawn), "{despawn}");
        let info = session_tools::scene_info(&mut session);
        assert_eq!(info["result"]["entityCount"], json!(0));
    }

    #[test]
    fn revision_conflict_surfaces_as_a_structured_error_result() {
        let mut session = session();

        let conflict =
            session_tools::entity_spawn(&mut session, json!({ "name": "late" }), Some(999));
        assert!(!payload_is_ok(&conflict));
        assert_eq!(payload_error(&conflict)["code"], json!("revision_conflict"));
        assert_eq!(session.revision(), 0, "conflict executes nothing");

        let result = tool_result_from_payload(conflict);
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured error");
        assert_eq!(structured["error"]["code"], json!("revision_conflict"));
        assert!(
            structured["error"]["message"]
                .as_str()
                .expect("message stays a string")
                .contains("revision"),
            "the protocol message is preserved verbatim"
        );
    }

    #[test]
    fn successful_payloads_map_to_structured_content_without_iserror() {
        let result = tool_result_from_payload(json!({ "ok": true, "result": {} }));
        assert_eq!(result.is_error, Some(false));
        assert!(result.structured_content.is_some());
    }

    #[test]
    fn entity_set_transform_and_query_round_trip() {
        let mut session = session();
        session_tools::entity_spawn(&mut session, json!({ "name": "moved" }), None);
        let transform = json!({ "x": 42.0, "y": -7.5, "rotation": 0.25 });
        let revision = session.revision();
        let set = session_tools::entity_set_transform(&mut session, 1, transform, Some(revision));
        assert!(payload_is_ok(&set), "{set}");
        let stored = &session.world().entities[&1].transform;
        assert_eq!(stored.x, 42.0);
        assert_eq!(stored.y, -7.5);
        assert_eq!(stored.rotation, 0.25);

        // A stale revision is rejected before any change.
        let stale = session_tools::entity_set_transform(
            &mut session,
            1,
            json!({ "x": 0.0, "y": 0.0 }),
            Some(999),
        );
        assert!(!payload_is_ok(&stale));
        assert_eq!(payload_error(&stale)["code"], json!("revision_conflict"));
        assert_eq!(session.world().entities[&1].transform.x, 42.0);
    }

    #[test]
    fn component_attach_and_remove_round_trip() {
        let mut session = session();
        session
            .engine_mut()
            .register_component(health_descriptor())
            .expect("register test component");
        session_tools::entity_spawn(&mut session, json!({ "name": "with-health" }), None);

        let attach =
            session_tools::component_attach(&mut session, 1, "game.health", json!({}), None);
        assert!(payload_is_ok(&attach), "{attach}");
        assert_eq!(
            session.world().entities[&1].extensions["game.health"]["hp"],
            json!(100),
            "schema defaults are applied to the empty value"
        );

        let overwrite = session_tools::component_attach(
            &mut session,
            1,
            "game.health",
            json!({ "hp": 55 }),
            None,
        );
        assert!(payload_is_ok(&overwrite), "{overwrite}");
        assert_eq!(
            session.world().entities[&1].extensions["game.health"]["hp"],
            json!(55)
        );

        let remove = session_tools::component_remove(&mut session, 1, "game.health", None);
        assert!(payload_is_ok(&remove), "{remove}");
        assert!(session.world().entities[&1].extensions.is_empty());
    }

    #[test]
    fn step_advances_ticks_and_declares_script_side_effects() {
        let mut session = session();
        let stepped = session_tools::step(&mut session, 3);
        assert!(payload_is_ok(&stepped), "{stepped}");
        assert_eq!(stepped["result"]["ticks"], json!(3));
        assert_eq!(session.world().tick, 3);

        let tool = tool_definitions()
            .into_iter()
            .find(|tool| tool.name.as_ref() == "step")
            .expect("step tool is listed");
        let description = tool
            .description
            .expect("step has a description")
            .to_string();
        assert!(
            description.contains("script"),
            "the step description must declare script side effects: {description}"
        );
        let annotations = tool.annotations.expect("step has annotations");
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(false));
    }

    #[test]
    fn scene_load_round_trip_and_outside_workspace_rejection() {
        let mut session = session();
        let relative = write_test_scene(
            "load-ok",
            json!([{ "id": 1, "name": "from-file", "components": {} }]),
        );
        let loaded = session_tools::scene_load(&mut session, &relative);
        assert!(payload_is_ok(&loaded), "{loaded}");
        assert_eq!(loaded["result"]["entities"], json!(1));
        assert_eq!(session.revision(), 1, "loading a scene bumps the revision");
        assert_eq!(session.world().entities[&1].name, "from-file");
        remove_test_file(&relative);

        let outside = session_tools::scene_load(&mut session, "../outside.mnsc");
        assert!(!payload_is_ok(&outside));
        assert_eq!(payload_error(&outside)["code"], json!("invalid_path"));
        assert_eq!(
            session.world().entities.len(),
            1,
            "the session is untouched"
        );
    }

    #[test]
    fn scene_save_writes_a_snapshot_file_through_the_protocol() {
        let mut session = session();
        session_tools::entity_spawn(&mut session, json!({ "name": "saved" }), None);
        let path = format!("target/moni-mcp-save-explicit-{}.json", std::process::id());
        let save = session_tools::scene_save(&mut session, &path);
        assert!(payload_is_ok(&save), "{save}");
        assert_eq!(save["result"]["path"], json!(path));
        assert!(
            std::env::current_dir().expect("cwd").join(&path).is_file(),
            "protocol save wrote the snapshot"
        );
        remove_test_file(&path);
    }

    #[test]
    fn diagnostics_reports_session_drains_events_and_counts_script_hosts() {
        let mut session = session();
        session_tools::entity_spawn(&mut session, json!({ "name": "scripted" }), None);
        // Attach a script through the protocol so the QuickJS host exists.
        let attach = handle_line_json(
            &mut session,
            "attach_script",
            json!({ "entity": 1, "source": "function onTick(ctx, dt) { }" }),
            None,
        );
        assert!(payload_is_ok(&attach), "{attach}");
        // The script runtime rebuilds the host on the next tick, so step once
        // before the count means anything.
        let stepped = session_tools::step(&mut session, 1);
        assert!(payload_is_ok(&stepped), "{stepped}");

        let first = session_tools::diagnostics(&mut session);
        assert!(payload_is_ok(&first), "{first}");
        assert_eq!(first["result"]["session"]["profile"], json!("serve"));
        assert_eq!(first["result"]["revision"], json!(3));
        assert_eq!(first["result"]["scriptHosts"], json!(1));
        let events = first["result"]["events"].as_array().expect("events array");
        assert!(
            events.is_empty(),
            "handle_line drained the events into its own responses already"
        );

        // A second call still reports a healthy session block.
        let second = session_tools::diagnostics(&mut session);
        assert_eq!(second["result"]["session"]["profile"], json!("serve"));
    }

    #[test]
    fn capabilities_tool_returns_static_and_session_overlay() {
        let mut session = session();
        let capabilities = session_tools::capabilities(&mut session);
        assert!(payload_is_ok(&capabilities), "{capabilities}");
        assert_eq!(
            capabilities["result"]["protocol"],
            json!("moni-engine-control/1")
        );
        assert_eq!(capabilities["result"]["session"]["profile"], json!("serve"));
        assert!(
            capabilities["result"]["commands"].get("spawn").is_some(),
            "the merged command table lists built-ins"
        );
    }

    #[test]
    fn render_svg_returns_inline_svg_or_writes_a_file() {
        let mut session = session();
        session_tools::entity_spawn(&mut session, json!({ "name": "drawn" }), None);
        let inline = session_tools::render_svg(&mut session, None);
        assert!(payload_is_ok(&inline), "{inline}");
        assert!(
            inline["result"]["svg"]
                .as_str()
                .expect("inline svg")
                .starts_with("<svg"),
            "the diagnostic wireframe is returned as SVG source"
        );

        let path = format!("target/moni-mcp-render-{}.svg", std::process::id());
        let written = session_tools::render_svg(&mut session, Some(&path));
        assert!(payload_is_ok(&written), "{written}");
        assert_eq!(written["result"]["path"], json!(path));
        remove_test_file(&path);
    }

    #[test]
    fn tool_list_matches_the_first_batch_and_declares_side_effects() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(
            names, TOOL_NAMES,
            "the tool table and dispatch stay in sync"
        );

        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap_or_else(|| panic!("tool '{name}' is listed"))
                .clone()
        };

        // Read-only tools are annotated as such.
        for name in ["capabilities", "scene_info", "diagnostics"] {
            let tool = find(name);
            assert_eq!(
                tool.annotations.expect("annotations").read_only_hint,
                Some(true),
                "'{name}' is read-only"
            );
        }

        // Mutating tools are annotated as non-read-only.
        for name in [
            "scene_load",
            "entity_spawn",
            "entity_despawn",
            "entity_set_transform",
            "component_attach",
            "component_remove",
        ] {
            let tool = find(name);
            assert_eq!(
                tool.annotations.expect("annotations").read_only_hint,
                Some(false),
                "'{name}' mutates the world"
            );
        }

        // Screenshot and run_client honestly describe the window requirement.
        let screenshot = find("screenshot")
            .description
            .expect("description")
            .to_string();
        assert!(screenshot.contains("OPENS A REAL WINDOW"), "{screenshot}");
        assert!(screenshot.contains("golden"), "{screenshot}");
        let run_client = find("run_client")
            .description
            .expect("description")
            .to_string();
        assert!(run_client.contains("OPENS A REAL WINDOW"), "{run_client}");

        // render_svg distinguishes itself from the GPU frame.
        let render_svg = find("render_svg")
            .description
            .expect("description")
            .to_string();
        assert!(render_svg.contains("NOT the GPU frame"), "{render_svg}");

        // scene_save names the snapshot-format caveat.
        let scene_save = find("scene_save")
            .description
            .expect("description")
            .to_string();
        assert!(
            scene_save.contains("NOT the .mnsc scene format"),
            "{scene_save}"
        );
    }

    #[test]
    fn malformed_tool_arguments_report_invalid_arguments() {
        // Missing spec.
        let missing = invalid_arguments_error("entity_spawn requires a 'spec' EntitySpec object");
        assert_eq!(missing["error"]["code"], json!("invalid_arguments"));

        // Wrong types are caught by the optional_* extractors.
        let arguments = args(&[("expectedRevision", json!("soon"))]);
        let error = optional_u64(&arguments, "expectedRevision").expect_err("string is not a u64");
        assert_eq!(error["error"]["code"], json!("invalid_arguments"));

        let arguments = args(&[("entity", json!(1)), ("transform", json!([1, 2]))]);
        assert!(!arguments.get("transform").expect("transform").is_object());

        // Absent arguments stay absent.
        let arguments = args(&[("entity", json!(1))]);
        assert_eq!(
            optional_u64(&arguments, "expectedRevision").expect("absent"),
            None
        );
        assert_eq!(optional_string(&arguments, "path").expect("absent"), None);
    }

    #[test]
    fn child_process_tools_validate_paths_and_defaults_before_spawning() {
        // A path outside the workspace is refused without spawning a child.
        let error = checked_workspace_path("../outside.png", "output").expect_err("rejected");
        assert_eq!(error["error"]["code"], json!("invalid_arguments"));
        let error = checked_workspace_path("../outside.mnsc", "scene").expect_err("rejected");
        assert_eq!(error["error"]["code"], json!("invalid_arguments"));

        // Missing output argument is refused.
        let result = screenshot_tool(None, &args(&[])).expect_err("missing output");
        assert_eq!(payload_error(&result)["code"], json!("invalid_arguments"));

        // Without a session scene the screenshot has no default scene.
        let result = screenshot_tool(None, &args(&[("output", json!("target/shot.png"))]))
            .expect_err("no session scene");
        assert_eq!(payload_error(&result)["code"], json!("invalid_arguments"));

        // frames must be at least 1.
        let result = screenshot_tool(
            Some("testing/assets/benchmark-2d.mnsc".to_string()),
            &args(&[("output", json!("target/shot.png")), ("frames", json!(0))]),
        )
        .expect_err("zero frames");
        assert_eq!(payload_error(&result)["code"], json!("invalid_arguments"));
        let result =
            run_client_tool(None, &args(&[("frames", json!(0))])).expect_err("zero frames");
        assert_eq!(payload_error(&result)["code"], json!("invalid_arguments"));
    }

    #[test]
    fn final_client_json_takes_the_last_object_in_the_stream() {
        let stdout = "noise\n{\n  \"ok\": true,\n  \"frames\": 30\n}\n";
        let parsed = final_client_json(stdout).expect("final json");
        assert_eq!(parsed["frames"], json!(30));
        assert!(final_client_json("no json here").is_none());
    }
}
