//! TypeScript build integration for the Scripts panel: spawns `npm run
//! build` in the project/workspace root on worker threads (the panel never
//! blocks the render loop), parses tsc diagnostics from the captured output,
//! and maps a diagnostic's file path back to entities whose script source
//! references that file — selection only, files are never attached.
//!
//! The engine reads scene scripts by path at load time
//! (`RuntimeSession::load_scene` -> `scene::load_script`), so a failed build
//! can also block scene loading with "could not read scene script"; the
//! panel's build status explains that connection.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::Context;
use moni_core::engine::{EntityId, ScriptState, World};

/// One parsed tsc diagnostic: `path(line,col): error TSxxxx: message`.
#[derive(Debug, Clone, PartialEq)]
pub struct TsDiagnostic {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub code: String,
    pub message: String,
}

/// Leniently parses tsc diagnostics of the form
/// `relative/path.ts(line,col): error TSxxxx: message`. Lines that do not
/// match (summaries, npm noise, pretty-printed continuations) are ignored.
pub fn parse_ts_diagnostics(output: &str) -> Vec<TsDiagnostic> {
    output.lines().filter_map(parse_diagnostic_line).collect()
}

fn parse_diagnostic_line(line: &str) -> Option<TsDiagnostic> {
    let open = line.find('(')?;
    let close = line[open..].find("):")? + open;
    if close <= open + 1 {
        return None;
    }
    let file = &line[..open];
    // Continuation lines of wrapped messages are indented; only unindented
    // file positions are diagnostics.
    if file.is_empty() || file.starts_with([' ', '\t']) {
        return None;
    }
    let (line_number, column) = line[open + 1..close].split_once(',')?;
    let line_number: u32 = line_number.trim().parse().ok()?;
    let column: u32 = column.trim().parse().ok()?;
    let rest = line[close + 2..].strip_prefix(' ')?;
    let (severity_code, message) = rest.split_once(": ")?;
    let (severity, code) = severity_code.split_once(' ')?;
    if severity != "error" && severity != "warning" {
        return None;
    }
    let digits = code.strip_prefix("TS")?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(TsDiagnostic {
        file: file.to_string(),
        line: line_number,
        col: column,
        code: code.to_string(),
        message: message.to_string(),
    })
}

/// The file path an entity's script references: path-loaded scenes carry the
/// raw `source` path in `source_path`; an attached source counts as a path
/// when it looks like one (single line, no whitespace, script extension)
/// rather than inline code.
fn entity_script_path(script: &ScriptState) -> Option<&str> {
    script
        .source_path
        .as_deref()
        .or_else(|| path_like_source(&script.source))
}

fn path_like_source(source: &str) -> Option<&str> {
    let trimmed = source.trim();
    if trimmed.is_empty() || trimmed.contains([' ', '\t', '\n', '\r']) {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.ends_with(".ts")
        || lowered.ends_with(".js")
        || lowered.ends_with(".mjs")
        || lowered.ends_with(".cjs")
    {
        Some(trimmed)
    } else {
        None
    }
}

/// True when a script source path references the diagnostic's file, matching
/// by path suffix in both directions so `scripts/main.ts` and
/// `project/scripts/main.ts` agree regardless of how deep the scene sits.
fn references_file(source_path: &str, file: &str) -> bool {
    let source = source_path.replace('\\', "/");
    let file = file.replace('\\', "/");
    if source.is_empty() || file.is_empty() {
        return false;
    }
    source == file || source.ends_with(&format!("/{file}")) || file.ends_with(&format!("/{source}"))
}

/// Finds the entity whose script source references `file` (lowest id wins).
/// `None` means no entity can own this error and the panel renders no
/// Locate control for it — files are never attached to entities here.
pub fn find_entity_by_script_source(world: &World, file: &str) -> Option<EntityId> {
    let mut matched = None;
    for (id, entity) in &world.entities {
        let Some(source_path) = entity.script.as_ref().and_then(entity_script_path) else {
            continue;
        };
        if references_file(source_path, file) && matched.is_none_or(|current| *id < current) {
            matched = Some(*id);
        }
    }
    matched
}

/// Characters of raw build output kept for the panel display; older output
/// is dropped and the remainder starts at a line boundary.
pub const OUTPUT_TAIL_CHARS: usize = 4000;

/// Upper bound for one build before the editor kills it; tsc builds of a
/// game project finish in seconds, so a longer hang is an error.
const BUILD_TIMEOUT_SECS: u64 = 120;

struct BuildOutcome {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Scripts-panel build state: the running build's completion channel, the
/// raw output tail, the parsed diagnostics, and the one-shot auto-build
/// request raised when a project opens.
pub struct ScriptBuild {
    running: bool,
    receiver: Option<Receiver<BuildOutcome>>,
    /// Set when a project opens (or at startup in project mode); the panel
    /// consumes it on the next frame by starting one build.
    pub auto_requested: bool,
    /// Last build status line ("build running", "build finished", ...).
    pub status: Option<String>,
    /// Bounded raw output tail of the last build.
    pub output_tail: String,
    pub diagnostics: Vec<TsDiagnostic>,
}

impl ScriptBuild {
    pub fn new(auto_requested: bool) -> Self {
        Self {
            running: false,
            receiver: None,
            auto_requested,
            status: None,
            output_tail: String::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Starts one `npm run build` in `root` unless one is already running.
    /// Spawn failures surface in the status; the build itself runs on worker
    /// threads and completes through [`ScriptBuild::poll`].
    pub fn request(&mut self, root: &Path) {
        if self.running {
            return;
        }
        match spawn_build(root) {
            Ok(receiver) => {
                self.running = true;
                self.receiver = Some(receiver);
                self.status = Some("build running".to_string());
            }
            Err(error) => {
                self.status = Some(format!("build failed to start: {error:#}"));
            }
        }
    }

    /// Drains the completion channel without blocking. A finished build
    /// updates the raw output tail, re-parses the diagnostics, and records
    /// the status; build failures are never fatal to the editor.
    pub fn poll(&mut self) {
        if !self.running {
            return;
        }
        let Some(receiver) = &self.receiver else {
            self.running = false;
            return;
        };
        match receiver.try_recv() {
            Ok(outcome) => {
                self.running = false;
                self.receiver = None;
                let combined = combine_output(&outcome.stdout, &outcome.stderr);
                self.diagnostics = parse_ts_diagnostics(&combined);
                self.output_tail = output_tail(&combined, OUTPUT_TAIL_CHARS);
                self.status = Some(if outcome.success {
                    "build finished".to_string()
                } else {
                    "build failed".to_string()
                });
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.running = false;
                self.receiver = None;
                self.status = Some("build worker lost".to_string());
            }
        }
    }
}

/// `npm.cmd` on Windows (npm is a batch wrapper there), plain `npm`
/// elsewhere — mirroring the composition root's `run_project_command`.
fn npm_executable() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

/// Spawns the build with piped output. Two reader threads drain stdout and
/// stderr; the worker thread waits for the child with a deadline (killing a
/// hung build), joins the readers, and sends one outcome.
fn spawn_build(root: &Path) -> anyhow::Result<Receiver<BuildOutcome>> {
    let mut child = Command::new(npm_executable())
        .args(["run", "build", "--", "--pretty", "false"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("could not start {} in {}", npm_executable(), root.display()))?;
    let stdout = child.stdout.take().context("npm stdout is not piped")?;
    let stderr = child.stderr.take().context("npm stderr is not piped")?;
    let (sender, receiver) = channel();
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr));
    std::thread::spawn(move || {
        let success = wait_with_deadline(&mut child, Duration::from_secs(BUILD_TIMEOUT_SECS));
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        let _ = sender.send(BuildOutcome {
            stdout,
            stderr,
            success,
        });
    });
    Ok(receiver)
}

/// The cook-importer timeout pattern: poll the child, kill it at the
/// deadline, and report failure either way.
fn wait_with_deadline(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return false,
        }
    }
}

fn read_pipe(mut pipe: impl Read) -> String {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn combine_output(stdout: &str, stderr: &str) -> String {
    let mut combined = stdout.to_string();
    if !stdout.is_empty() && !stdout.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(stderr);
    combined
}

/// The last `max_chars` characters of `text`, aligned to a line boundary so
/// the panel never shows a torn first line.
fn output_tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let mut tail: String = text.chars().skip(count - max_chars).collect();
    if let Some(position) = tail.find('\n') {
        tail.drain(..=position);
    }
    tail
}

#[cfg(test)]
#[path = "scripts_tests.rs"]
mod scripts_tests;
