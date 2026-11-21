//! Editor-side scene document: a [`RuntimeSession`] plus undo/redo history,
//! selection, and atomic scene persistence. Every edit goes through the
//! session's command API, so GUI edits produce the same revisions, events,
//! and saved bytes as NDJSON agent edits; the document never touches engine
//! state directly.
//!
//! History entries are structured mementos, not whole-world clones: each
//! entry stores a label plus inverse/forward command lists, and create/delete
//! entries carry the full entity snapshot inside a `RestoreEntity` command so
//! undo/redo keeps stable entity IDs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use moni_core::engine::{
    Command, Engine, EngineError, Entity, EntityId, EntitySpec, Transform, World,
};
use moni_core::session::RuntimeSession;
use serde_json::Value;

#[cfg(test)]
#[path = "document_tests.rs"]
mod document_tests;

/// One undo/redo step: a stable label plus the inverse and forward command
/// lists. Create/delete steps embed the exact entity snapshot in a
/// `RestoreEntity` command on the side that brings the entity back.
struct HistoryEntry {
    label: &'static str,
    undo_commands: Vec<Command>,
    redo_commands: Vec<Command>,
}

/// An open scene under edit: owns its session, dirty bookkeeping, selection
/// (document state only — never stored in the world), and undo/redo stacks.
pub struct EditorDocument {
    session: RuntimeSession,
    /// Workspace-relative scene path, as `RuntimeSession::load_scene` and
    /// the editor UI report it.
    scene_path: String,
    /// Scene name written into the `name` field on every save; read from the
    /// opened file and independent of the file path.
    scene_name: String,
    /// Absolute location of `scene_path`, resolved once so saves never depend
    /// on a changed working directory.
    resolved_path: PathBuf,
    /// Revision the scene file last reflected; `is_dirty` compares it with
    /// the live session revision.
    last_saved_revision: u64,
    selection: BTreeSet<EntityId>,
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    /// World captured by `enter_play` and restored by `exit_play`; `Some`
    /// exactly while the document is in play mode.
    pre_play_world: Option<World>,
    /// Play-mode pause flag; only meaningful while playing.
    paused: bool,
}

impl EditorDocument {
    /// Loads `scene_path` through the session's single scene-loading path and
    /// opens it for editing. A failed load returns the error and leaves the
    /// session untouched.
    pub fn new(mut session: RuntimeSession, scene_path: &str) -> anyhow::Result<Self> {
        session.load_scene(scene_path)?;
        let resolved_path = moni_core::workspace_path(scene_path)?;
        let scene_name = read_scene_name(&resolved_path)?;
        Ok(Self {
            last_saved_revision: session.revision(),
            session,
            scene_path: scene_path.to_string(),
            scene_name,
            resolved_path,
            selection: BTreeSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pre_play_world: None,
            paused: false,
        })
    }

    pub fn session(&self) -> &RuntimeSession {
        &self.session
    }

    /// Mutable session access for composition-root work outside the edit
    /// path (plugin registration, render setup); scene edits themselves must
    /// go through the document's apply/undo/redo.
    pub fn session_mut(&mut self) -> &mut RuntimeSession {
        &mut self.session
    }

    pub fn scene_path(&self) -> &str {
        &self.scene_path
    }

    pub fn scene_name(&self) -> &str {
        &self.scene_name
    }

    /// True when the live session revision has moved past the revision the
    /// scene file last reflected (by edits, undo, or redo).
    pub fn is_dirty(&self) -> bool {
        self.last_saved_revision != self.session.revision()
    }

    /// Currently selected entities; selection lives only in the document.
    pub fn selection(&self) -> &BTreeSet<EntityId> {
        &self.selection
    }

    /// Replaces the selection with `ids`.
    pub fn select(&mut self, ids: impl IntoIterator<Item = EntityId>) {
        self.selection = ids.into_iter().collect();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Applies one edit through the session command API. The edit is rejected
    /// with `revision_conflict` before anything changes when
    /// `expected_revision` does not match the session revision. After a
    /// successful apply the inverse commands are built from the post-edit
    /// engine state and pushed as a history entry, clearing the redo stack.
    pub fn apply(
        &mut self,
        label: &'static str,
        expected_revision: u64,
        forward: Command,
        inverse: impl FnOnce(&Engine) -> Vec<Command>,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let redo_commands = vec![forward.clone()];
        self.session.apply(forward)?;
        let undo_commands = inverse(self.session.engine());
        self.push_history(label, undo_commands, redo_commands);
        Ok(())
    }

    /// Undoes the most recent history entry by applying its inverse commands
    /// and moving the entry onto the redo stack. Returns the undone entry's
    /// label, or `None` when the stack is empty. If an inverse command fails
    /// mid-way the entry is put back on the undo stack (so it can be retried)
    /// and the error is surfaced; commands applied before the failure stay
    /// applied.
    pub fn undo(&mut self) -> anyhow::Result<Option<&'static str>> {
        self.check_editable()?;
        let Some(entry) = self.undo_stack.pop() else {
            return Ok(None);
        };
        if let Err(error) = self.run_commands(&entry.undo_commands) {
            self.undo_stack.push(entry);
            return Err(error);
        }
        let label = entry.label;
        self.redo_stack.push(entry);
        Ok(Some(label))
    }

    /// Redoes the most recently undone entry by applying its forward commands
    /// and moving the entry back onto the undo stack, with the same failure
    /// semantics as [`EditorDocument::undo`].
    pub fn redo(&mut self) -> anyhow::Result<Option<&'static str>> {
        self.check_editable()?;
        let Some(entry) = self.redo_stack.pop() else {
            return Ok(None);
        };
        if let Err(error) = self.run_commands(&entry.redo_commands) {
            self.redo_stack.push(entry);
            return Err(error);
        }
        let label = entry.label;
        self.undo_stack.push(entry);
        Ok(Some(label))
    }

    /// True while the document is in play mode. Play mode drives the
    /// session's simulation provider and locks every edit operation.
    pub fn is_playing(&self) -> bool {
        self.pre_play_world.is_some()
    }

    /// True while play mode is paused; always false outside play mode.
    pub fn is_paused(&self) -> bool {
        self.is_playing() && self.paused
    }

    /// Enters play mode: captures the pre-play world and starts the
    /// session's simulation provider (`core.simulation`). The capture is
    /// play-state restore, not undo history: it is a single whole-world
    /// clone kept outside the memento stack on purpose. The no-world-clone
    /// rule governs history entries, which must stay structured commands to
    /// preserve entity ids and event semantics across undo/redo; the play
    /// restore legitimately discards everything that happened during play.
    ///
    /// Already playing, or a session whose profile resolves no `core.simulation`
    /// provider, returns an error and leaves the document unchanged.
    pub fn enter_play(&mut self) -> anyhow::Result<()> {
        if self.is_playing() {
            bail!("the document is already playing; stop play mode first");
        }
        // Capture before the start so the restore point is the exact pre-play
        // state, including any world change the provider's `start` makes.
        self.pre_play_world = Some(self.session.world().clone());
        if let Err(error) = self
            .session
            .host_mut()
            .start_plugin(moni_core::host::CORE_SIMULATION)
        {
            self.pre_play_world = None;
            return Err(anyhow::Error::new(error)
                .context("could not enter play mode: the simulation provider failed to start"));
        }
        Ok(())
    }

    /// Pauses the play-mode simulation; a no-op outside play mode.
    pub fn pause(&mut self) {
        if self.is_playing() {
            self.paused = true;
        }
    }

    /// Resumes a paused play mode; a no-op outside play mode.
    pub fn resume(&mut self) {
        if self.is_playing() {
            self.paused = false;
        }
    }

    /// Exits play mode: stops the simulation provider and restores the
    /// pre-play world through `Engine::load_world`. `load_world` is
    /// deliberate: unlike `Engine::restore` it moves the revision FORWARD
    /// and emits `WorldLoaded`, so revisions stay monotonic and the session
    /// request cache stays coherent; the tick counter returns to its
    /// pre-play value as part of the restored world content. The revision
    /// move also makes `is_dirty` report true after the restore —
    /// conservative, because the restored content equals the pre-play
    /// content but the session revision moved past the saved revision.
    ///
    /// Script hosts are cleared by `load_world` and rebuilt fresh on the
    /// next play: script heap state is never snapshotted, so play-mode
    /// script state outside the world is dropped on exit.
    pub fn exit_play(&mut self) -> anyhow::Result<()> {
        if !self.is_playing() {
            bail!("the document is not playing");
        }
        self.session
            .host_mut()
            .stop_plugin(moni_core::host::CORE_SIMULATION)
            .map_err(anyhow::Error::new)?;
        self.paused = false;
        let pre_play = self
            .pre_play_world
            .take()
            .expect("playing flag checked above");
        self.session.engine_mut().load_world(pre_play)?;
        Ok(())
    }

    /// Switches the document to another scene through the session's
    /// registry-validated load path. Refuses while playing and while the
    /// current scene has unsaved changes (save first); a failed load leaves
    /// the document untouched. On success the document state resets for the
    /// new scene: history and redo stacks are cleared (their commands
    /// describe the previous scene), the selection is cleared, and the dirty
    /// anchor moves to the new revision so the fresh scene reads clean.
    pub fn open_scene(&mut self, scene_path: &str) -> anyhow::Result<()> {
        self.check_editable()?;
        if self.is_dirty() {
            bail!(
                "the current scene has unsaved changes; save before opening {}",
                scene_path
            );
        }
        // Resolve the path and read the scene name first so a bad path fails
        // before the session world changes.
        let resolved_path = moni_core::workspace_path(scene_path)?;
        let scene_name = read_scene_name(&resolved_path)?;
        self.session.load_scene(scene_path)?;
        self.scene_path = scene_path.to_string();
        self.scene_name = scene_name;
        self.resolved_path = resolved_path;
        self.selection.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_saved_revision = self.session.revision();
        Ok(())
    }

    /// Saves the scene to the current path. The registry-aware scene value is
    /// serialized to a sibling temp file first and then renamed over the
    /// target, so a failed save leaves the previous file intact and the
    /// document dirty. Saving never mutates the world: the scene value is
    /// computed from read-only world and registry access.
    pub fn save(&mut self) -> anyhow::Result<()> {
        self.check_editable()?;
        self.save_to(&self.resolved_path.clone())
    }

    /// Saves to `path` (workspace-relative, same rules as scene loading) and
    /// switches the document's path bookkeeping on success. The scene name is
    /// document state and stays unchanged; a failed save keeps the old path.
    pub fn save_as(&mut self, path: &str) -> anyhow::Result<()> {
        self.check_editable()?;
        let resolved_path = moni_core::workspace_path(path)?;
        self.save_to(&resolved_path)?;
        self.scene_path = path.to_string();
        self.resolved_path = resolved_path;
        Ok(())
    }

    /// Creates an entity from `spec`. The undo step despawns it; the redo
    /// step restores the captured entity snapshot, so the entity keeps its
    /// exact id across undo/redo.
    pub fn create_entity(
        &mut self,
        expected_revision: u64,
        spec: EntitySpec,
    ) -> anyhow::Result<EntityId> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let forward = Command::Spawn { spec: spec.clone() };
        let result = self.session.apply(forward)?;
        let entity = result["entity"]
            .as_u64()
            .context("spawn result is missing the entity id")?;
        let snapshot = captured_entity(self.session.engine(), entity)?.clone();
        self.push_history(
            "create_entity",
            vec![Command::Despawn { entity }],
            vec![Command::RestoreEntity { entity: snapshot }],
        );
        Ok(entity)
    }

    /// Deletes an entity. The full entity is captured before the despawn so
    /// the undo step restores it under its explicit id; the deleted entity is
    /// also dropped from the selection.
    pub fn delete_entity(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let snapshot = captured_entity(self.session.engine(), entity)?.clone();
        self.session.apply(Command::Despawn { entity })?;
        self.push_history(
            "delete_entity",
            vec![Command::RestoreEntity { entity: snapshot }],
            vec![Command::Despawn { entity }],
        );
        self.selection.remove(&entity);
        Ok(())
    }

    /// Renames an entity; the inverse restores the previous name.
    pub fn rename_entity(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
        name: &str,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let old_name = captured_entity(self.session.engine(), entity)?.name.clone();
        self.session.apply(Command::RenameEntity {
            entity,
            name: name.to_string(),
        })?;
        self.push_history(
            "rename_entity",
            vec![Command::RenameEntity {
                entity,
                name: old_name,
            }],
            vec![Command::RenameEntity {
                entity,
                name: name.to_string(),
            }],
        );
        Ok(())
    }

    /// Sets an entity transform; the inverse restores the previous transform.
    pub fn set_transform(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
        transform: Transform,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let old_transform = captured_entity(self.session.engine(), entity)?.transform;
        self.session
            .apply(Command::SetTransform { entity, transform })?;
        self.push_history(
            "set_transform",
            vec![Command::SetTransform {
                entity,
                transform: old_transform,
            }],
            vec![Command::SetTransform { entity, transform }],
        );
        Ok(())
    }

    /// Attaches a dynamic component. The engine applies schema defaults
    /// during the attach, so the redo command carries the applied value read
    /// back from the entity extensions; the undo step removes the component.
    pub fn attach_component(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
        component: &str,
        value: Value,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let component = canonical_component(self.session.engine(), component);
        self.session.apply(Command::AttachComponent {
            entity,
            component: component.clone(),
            value,
        })?;
        let applied = captured_entity(self.session.engine(), entity)?
            .extensions
            .get(component.as_str())
            .cloned()
            .context("attached component value missing from the entity extensions")?;
        self.push_history(
            "attach_component",
            vec![Command::RemoveComponent {
                entity,
                component: component.clone(),
            }],
            vec![Command::AttachComponent {
                entity,
                component,
                value: applied,
            }],
        );
        Ok(())
    }

    /// Removes a dynamic component; the undo step re-attaches the captured
    /// previous value, the redo step removes it again.
    pub fn remove_component(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
        component: &str,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let component = canonical_component(self.session.engine(), component);
        let previous = captured_entity(self.session.engine(), entity)?
            .extensions
            .get(component.as_str())
            .cloned();
        self.session.apply(Command::RemoveComponent {
            entity,
            component: component.clone(),
        })?;
        let value = previous.context("removed component value missing before the removal")?;
        self.push_history(
            "remove_component",
            vec![Command::AttachComponent {
                entity,
                component: component.clone(),
                value,
            }],
            vec![Command::RemoveComponent { entity, component }],
        );
        Ok(())
    }

    /// Attaches a script source to an entity through the engine's script
    /// validation. Scripts can only be replaced, never removed — the core
    /// command set has no script-removal command — so the undo step restores
    /// the previously attached source when one existed; when the attach was
    /// the entity's first script the undo step is empty and undoing the
    /// entry is a no-op that still reports its label.
    pub fn attach_script(
        &mut self,
        expected_revision: u64,
        entity: EntityId,
        source: String,
    ) -> anyhow::Result<()> {
        self.check_editable()?;
        self.check_revision(expected_revision)?;
        let previous = captured_entity(self.session.engine(), entity)?
            .script
            .as_ref()
            .map(|script| script.source.clone());
        self.session.apply(Command::AttachScript {
            entity,
            source: source.clone(),
        })?;
        let undo_commands = previous
            .map(|previous| {
                vec![Command::AttachScript {
                    entity,
                    source: previous,
                }]
            })
            .unwrap_or_default();
        self.push_history(
            "attach_script",
            undo_commands,
            vec![Command::AttachScript { entity, source }],
        );
        Ok(())
    }

    /// Play-mode edit lock. Every mutating operation — `apply`, the
    /// convenience ops, undo/redo, save/save_as, and `open_scene` — refuses
    /// while the document is playing: play state is transient (the exit
    /// restore discards it) and unsavable, so edits made during play would
    /// be silently overwritten by the exit restore. The session itself stays
    /// reachable through `session_mut` for the app-driven play ticks.
    fn check_editable(&self) -> anyhow::Result<()> {
        if self.is_playing() {
            bail!("the document is playing; stop play mode before editing or saving");
        }
        Ok(())
    }

    /// Fails with `revision_conflict` before any mutation when the session
    /// revision moved past `expected_revision`.
    fn check_revision(&self, expected_revision: u64) -> anyhow::Result<()> {
        let current = self.session.revision();
        if current != expected_revision {
            return Err(anyhow::Error::new(EngineError::new(
                "revision_conflict",
                format!(
                    "expected world revision {expected_revision} but the session is at revision {current}"
                ),
            )));
        }
        Ok(())
    }

    fn push_history(
        &mut self,
        label: &'static str,
        undo_commands: Vec<Command>,
        redo_commands: Vec<Command>,
    ) {
        self.redo_stack.clear();
        self.undo_stack.push(HistoryEntry {
            label,
            undo_commands,
            redo_commands,
        });
    }

    fn run_commands(&mut self, commands: &[Command]) -> anyhow::Result<()> {
        for command in commands {
            self.session.apply(command.clone())?;
            if let Command::Despawn { entity } = command {
                self.selection.remove(entity);
            }
        }
        Ok(())
    }

    /// Serializes the registry-aware scene value and replaces `path`
    /// atomically. The parent directory is deliberately not created: a save
    /// to a missing directory fails without touching the previous file.
    fn save_to(&mut self, path: &Path) -> anyhow::Result<()> {
        let value = moni_core::scene::scene_value_with_registry(
            self.session.engine().world(),
            &self.scene_name,
            self.session.engine().registry(),
        )?;
        let contents = serde_json::to_string_pretty(&value)?;
        let temp = temp_path(path);
        fs::write(&temp, contents)
            .with_context(|| format!("could not write {}", temp.display()))?;
        if let Err(error) = fs::rename(&temp, path) {
            fs::remove_file(&temp).ok();
            return Err(
                anyhow::Error::new(error).context(format!("could not replace {}", path.display()))
            );
        }
        self.last_saved_revision = self.session.revision();
        Ok(())
    }
}

/// The entity's current state, or `entity_not_found` (the engine's own code)
/// so pre-apply captures fail before anything is mutated.
fn captured_entity(engine: &Engine, entity: EntityId) -> Result<&Entity, EngineError> {
    engine.world().entities.get(&entity).ok_or_else(|| {
        EngineError::new(
            "entity_not_found",
            format!("entity {entity} does not exist"),
        )
    })
}

/// Dynamic components are stored under their canonical registry id; memento
/// commands use the same key the engine writes.
fn canonical_component(engine: &Engine, component: &str) -> String {
    engine
        .registry()
        .resolve(component)
        .map(|descriptor| descriptor.id.to_string())
        .unwrap_or_else(|| component.to_string())
}

/// Sibling temp file for the atomic save: `<path>.moni-tmp`.
fn temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.moni-tmp", path.display()))
}

/// Reads the scene's `name` field; the document keeps it for every save.
fn read_scene_name(path: &Path) -> anyhow::Result<String> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read scene {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse scene {}", path.display()))?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .context(format!("scene {} has no name field", path.display()))?
        .to_string();
    if name.trim().is_empty() {
        bail!("scene name must not be empty");
    }
    Ok(name)
}
