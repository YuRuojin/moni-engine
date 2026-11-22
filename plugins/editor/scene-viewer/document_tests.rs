//! Cross-cutting acceptance tests for [`EditorDocument`] (no GUI): the full
//! edit/undo/redo/save loop with a third-party dynamic component, byte
//! equivalence with the NDJSON protocol path, stale-revision rejection,
//! atomic save failure, retention of unregistered and future-version
//! component data, and stable-ID undo/redo of create/delete.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use moni_core::components::{ComponentDescriptor, PropertyField, PropertySchema, PropertyType};
use moni_core::engine::{
    Command, Engine, EngineError, Entity, EntitySpec, MAX_SCRIPT_BYTES, Transform, World,
};
use moni_core::host::{
    Plugin, PluginContext, PluginError, PluginOutput, PluginRegistry, SystemDescriptor, SystemStage,
};
use moni_core::profile::Profile;
use moni_core::scene;
use moni_core::session::RuntimeSession;

use super::EditorDocument;

const COMPONENT: &str = "game.health";
const OWNER: &str = "third.game";
const HEALTH_VERSION: u32 = 2;

/// Third-party test double registering `game.health` at `version` during the
/// registration phase — the HealthPlugin pattern from the moni-core
/// component-lifecycle acceptance test, written against public APIs only.
struct HealthPlugin {
    version: u32,
}

impl Plugin for HealthPlugin {
    fn id(&self) -> &'static str {
        OWNER
    }

    fn systems(&self) -> Vec<SystemDescriptor> {
        Vec::new()
    }

    fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
        registry.register_component(ComponentDescriptor::dynamic(
            COMPONENT,
            None,
            OWNER,
            self.version,
            PropertySchema::new(
                "Health",
                "Third-party health component",
                vec![
                    PropertyField::new("current", PropertyType::Integer, true),
                    PropertyField::new("maximum", PropertyType::Integer, false)
                        .default(json!(100))
                        .range(1.0, f64::MAX),
                ],
            ),
        ))?;
        Ok(())
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

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn session_with_health(version: u32) -> RuntimeSession {
    let mut session =
        RuntimeSession::new(Engine::new(), Profile::named("minimal").expect("minimal"));
    session
        .host_mut()
        .register_plugin(Box::new(HealthPlugin { version }), &[])
        .expect("third-party plugin registers");
    session
}

/// Editor-profile session with the simulation plugin registered but not
/// started, mirroring the composition root: play mode starts it on demand.
fn session_for_play() -> RuntimeSession {
    let mut session = RuntimeSession::new(
        Engine::new(),
        Profile::named("editor").expect("editor profile"),
    );
    session
        .host_mut()
        .register_plugin(
            Box::new(moni_core::plugins::SimulationPlugin::new()),
            &["script.execute".to_string()],
        )
        .expect("simulation registers");
    session
}

/// Scene paths are workspace-relative by contract (the same rule the
/// document applies to saves), so test scenes live under the crate-local
/// target directory, which is the test binary's working directory.
fn temp_scene_path(label: &str) -> PathBuf {
    PathBuf::from(format!(
        "target/document-{label}-{}.mnsc",
        std::process::id()
    ))
}

/// Writes a one-entity scene file and returns its workspace-relative path.
/// `with_component` attaches the dynamic component at registry-current
/// values; `scene_name` becomes the file's `name` field.
fn seed_scene(label: &str, scene_name: &str, with_component: bool) -> String {
    let path = temp_scene_path(label);
    let mut world = World::default();
    let mut extensions = BTreeMap::new();
    if with_component {
        extensions.insert(
            COMPONENT.to_string(),
            json!({ "current": 80, "maximum": 100 }),
        );
    }
    world.entities.insert(
        1,
        Entity {
            id: 1,
            name: "hero".into(),
            transform: Transform::default(),
            sprite: None,
            model: None,
            script: None,
            extensions,
        },
    );
    world.next_entity_id = 2;
    let session = session_with_health(HEALTH_VERSION);
    scene::save_scene_with_registry(&world, scene_name, &path, session.engine().registry())
        .expect("seed scene saves");
    path.to_str().expect("utf8 path").to_string()
}

/// The big loop: create, rename, transform, attach a dynamic component,
/// undo, redo, save, and reload the saved file in a fresh windowless
/// session with the same plugin — the world, entity ids, and schema version
/// table must reproduce the pre-save state.
#[test]
fn document_edit_undo_redo_save_round_trip_with_dynamic_components() {
    let scene_path = seed_scene("loop", "document-loop", true);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");
    let spec: EntitySpec = serde_json::from_value(json!({
        "name": "created",
        "sprite": { "type": "circle", "width": 12.0, "height": 12.0 },
    }))
    .expect("valid spec");

    let created = document
        .create_entity(document.session().revision(), spec)
        .expect("create");
    assert_eq!(created, 2, "ids continue after the loaded scene");
    document
        .rename_entity(document.session().revision(), created, "hero-jr")
        .expect("rename");
    document
        .set_transform(
            document.session().revision(),
            created,
            Transform {
                x: 3.0,
                y: 4.0,
                rotation: 0.5,
            },
        )
        .expect("transform");
    document
        .attach_component(
            document.session().revision(),
            created,
            COMPONENT,
            json!({ "current": 50 }),
        )
        .expect("attach");
    assert_eq!(
        document.session().world().entities[&created].extensions[COMPONENT],
        json!({ "current": 50, "maximum": 100 }),
        "schema defaults are applied by the engine"
    );
    assert!(document.is_dirty());
    assert_eq!(document.undo_depth(), 4);
    assert_eq!(document.redo_depth(), 0);

    // Undo two steps and redo them; each step reports its label.
    assert_eq!(document.undo().expect("undo"), Some("attach_component"));
    assert_eq!(document.undo().expect("undo"), Some("set_transform"));
    assert_eq!(document.redo_depth(), 2);
    assert!(
        document.session().world().entities[&created]
            .extensions
            .get(COMPONENT)
            .is_none(),
        "the attach undo removes the component"
    );
    assert_eq!(document.redo().expect("redo"), Some("set_transform"));
    assert_eq!(document.redo().expect("redo"), Some("attach_component"));
    assert_eq!(
        document.redo().expect("redo"),
        None,
        "redo stack is empty again"
    );
    assert_eq!(document.undo_depth(), 4);
    assert_eq!(
        document.session().world().entities[&created].extensions[COMPONENT],
        json!({ "current": 50, "maximum": 100 }),
        "the redo re-attaches the applied value"
    );

    let before_save = serde_json::to_value(document.session().world()).expect("world json");
    document.save().expect("save");
    assert!(!document.is_dirty(), "saving clears the dirty state");

    let mut fresh = session_with_health(HEALTH_VERSION);
    fresh.load_scene(&scene_path).expect("saved scene reloads");
    assert_eq!(
        serde_json::to_value(fresh.world()).expect("world json"),
        before_save,
        "the reload reproduces the pre-save world exactly"
    );
    assert_eq!(fresh.world().entities[&2].name, "hero-jr");
    assert_eq!(fresh.world().entities[&2].transform.x, 3.0);
    assert_eq!(
        fresh.world().entities[&2].extensions[COMPONENT],
        json!({ "current": 50, "maximum": 100 })
    );
    assert_eq!(
        fresh.world().entities[&1].extensions[COMPONENT],
        json!({ "current": 80, "maximum": 100 })
    );
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(&scene_path).expect("saved scene readable"))
            .expect("saved scene parses");
    assert_eq!(
        saved["extensions"][scene::COMPONENT_VERSIONS_KEY][COMPONENT],
        json!(HEALTH_VERSION),
        "the saved version table reflects the registry version"
    );

    let _ = fs::remove_file(&scene_path);
}

/// The same edits applied through `EditorDocument` and through NDJSON
/// `handle_line` requests on two identical sessions save byte-identical
/// scene files.
#[test]
fn document_edits_match_the_ndjson_path_byte_for_byte() {
    let seed = seed_scene("equiv-seed", "document-equiv", true);
    let mut cli = session_with_health(HEALTH_VERSION);
    cli.load_scene(&seed).expect("cli session loads the seed");
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &seed).expect("open");

    let spec: EntitySpec = serde_json::from_value(json!({
        "name": "added",
        "transform": { "x": 1.0, "y": 2.0 },
    }))
    .expect("valid spec");
    let created = document
        .create_entity(document.session().revision(), spec.clone())
        .expect("create");
    document
        .rename_entity(document.session().revision(), created, "renamed")
        .expect("rename");
    document
        .set_transform(
            document.session().revision(),
            created,
            Transform {
                x: 5.0,
                y: 6.0,
                rotation: 0.25,
            },
        )
        .expect("transform");
    document
        .attach_component(
            document.session().revision(),
            created,
            COMPONENT,
            json!({ "current": 42 }),
        )
        .expect("attach");

    let line = |id: &str, command: &str, args: Value| {
        json!({ "id": id, "command": command, "args": args }).to_string()
    };
    let (spawn, _) = cli.handle_line(&line(
        "1",
        "spawn",
        serde_json::to_value(&spec).expect("spec json"),
    ));
    assert!(spawn.ok, "{:?}", spawn.error);
    cli.handle_line(&line(
        "2",
        "rename_entity",
        json!({ "entity": 2, "name": "renamed" }),
    ));
    cli.handle_line(&line(
        "3",
        "set_transform",
        json!({ "entity": 2, "transform": { "x": 5.0, "y": 6.0, "rotation": 0.25 } }),
    ));
    cli.handle_line(&line(
        "4",
        "attach_component",
        json!({ "entity": 2, "component": COMPONENT, "value": { "current": 42 } }),
    ));

    let document_out = temp_scene_path("equiv-gui");
    document
        .save_as(document_out.to_str().expect("utf8 path"))
        .expect("document save");
    let cli_out = temp_scene_path("equiv-cli");
    scene::save_scene_with_registry(
        cli.world(),
        "document-equiv",
        &cli_out,
        cli.engine().registry(),
    )
    .expect("ndjson save");

    let document_bytes = fs::read_to_string(&document_out).expect("document file readable");
    let cli_bytes = fs::read_to_string(&cli_out).expect("cli file readable");
    assert_eq!(
        document_bytes, cli_bytes,
        "document and NDJSON edits must save byte-identical scenes"
    );
    let _ = fs::remove_file(&seed);
    let _ = fs::remove_file(&document_out);
    let _ = fs::remove_file(&cli_out);
}

/// A stale `expected_revision` is rejected before any mutation: dirty state,
/// history, selection, and the world stay untouched; the correct revision
/// then applies.
#[test]
fn stale_expected_revision_is_rejected_without_side_effects() {
    let scene_path = seed_scene("stale", "document-stale", false);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");
    let revision = document.session().revision();
    document.select([1]);

    let stale = document.rename_entity(revision + 7, 1, "stale-name");
    let error = stale.expect_err("a stale revision must be rejected");
    assert!(
        error.to_string().contains("revision"),
        "the error explains the conflict: {error}"
    );
    assert_eq!(
        document.session().revision(),
        revision,
        "the world revision is untouched"
    );
    assert!(!document.is_dirty(), "dirty state is untouched");
    assert_eq!(document.undo_depth(), 0, "history is untouched");
    assert_eq!(document.redo_depth(), 0);
    assert_eq!(
        document.selection(),
        &BTreeSet::from([1]),
        "selection is untouched"
    );
    assert_eq!(
        document.session().world().entities[&1].name,
        "hero",
        "world content is untouched"
    );

    document
        .rename_entity(revision, 1, "renamed")
        .expect("rename");
    assert_eq!(document.session().world().entities[&1].name, "renamed");
    assert_eq!(document.undo_depth(), 1);
    assert!(document.is_dirty());
    let _ = fs::remove_file(&scene_path);
}

/// A failed save (target parent directory missing) leaves the previous file
/// intact, keeps the document dirty, and keeps the old path; saving to the
/// valid path still works afterwards.
#[test]
fn failed_save_leaves_the_previous_file_and_dirty_state_untouched() {
    let scene_path = seed_scene("save-failure", "document-save", false);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");
    document
        .rename_entity(document.session().revision(), 1, "first-save")
        .expect("edit");
    document.save().expect("the first save succeeds");
    assert!(!document.is_dirty());
    let original = fs::read_to_string(&scene_path).expect("saved file readable");

    document
        .rename_entity(document.session().revision(), 1, "second-save")
        .expect("edit");
    assert!(document.is_dirty());
    let missing_parent = PathBuf::from(format!(
        "target/moni-missing-dir-{}/nested/document-save.mnsc",
        std::process::id()
    ));
    let error = document
        .save_as(missing_parent.to_str().expect("utf8 path"))
        .expect_err("the missing parent directory must fail the save");
    assert!(
        error.to_string().contains("could not"),
        "the error explains the failure: {error}"
    );
    assert_eq!(
        fs::read_to_string(&scene_path).expect("old file readable"),
        original,
        "the previous file is intact"
    );
    assert!(
        document.is_dirty(),
        "a failed save keeps the document dirty"
    );
    assert_eq!(
        document.scene_path(),
        scene_path.as_str(),
        "a failed save_as keeps the old path"
    );
    assert!(!missing_parent.exists(), "no partial output is written");

    document.save().expect("save to the original path");
    assert!(!document.is_dirty());
    let _ = fs::remove_file(&scene_path);
}

/// Unregistered components and future-version instances keep their data and
/// version-table entries byte-identically through an unrelated edit, an
/// undo, and a save (the wave-3 retention semantics).
#[test]
fn unregistered_and_future_version_component_data_survive_document_edits() {
    let path = temp_scene_path("retention");
    // Registry at v1 so the scene's game.health entry (9) is a future
    // version; vendor.custom stays unregistered the whole time.
    let session = session_with_health(1);
    let mut world = World::default();
    world.entities.insert(
        1,
        Entity {
            id: 1,
            name: "carrier".into(),
            transform: Transform::default(),
            sprite: None,
            model: None,
            script: None,
            extensions: BTreeMap::from([
                (
                    COMPONENT.to_string(),
                    json!({ "current": 55, "futureField": true }),
                ),
                ("vendor.custom".to_string(), json!({ "kept": true })),
            ]),
        },
    );
    world.next_entity_id = 2;
    world.extensions.insert(
        scene::COMPONENT_VERSIONS_KEY.to_string(),
        json!({ COMPONENT: 9, "vendor.custom": 3 }),
    );
    scene::save_scene_with_registry(
        &world,
        "document-retention",
        &path,
        session.engine().registry(),
    )
    .expect("seed scene saves");
    let scene_path = path.to_str().expect("utf8 path").to_string();
    let original = fs::read_to_string(&path).expect("seed file readable");

    let mut document = EditorDocument::new(session, &scene_path).expect("open");

    // An unrelated edit (create + undo) touches neither the carried data nor
    // the retained version table.
    let spec: EntitySpec =
        serde_json::from_value(json!({ "name": "unrelated" })).expect("valid spec");
    let created = document
        .create_entity(document.session().revision(), spec)
        .expect("create");
    document.undo().expect("undo the unrelated edit");
    assert!(
        !document.session().world().entities.contains_key(&created),
        "the unrelated entity is gone again"
    );
    document.save().expect("save");

    let saved = fs::read_to_string(&scene_path).expect("saved file readable");
    assert_eq!(
        saved, original,
        "unregistered and future-version entries and data survive byte-identically"
    );
    let _ = fs::remove_file(&path);
}

/// Undo/redo of create and delete restores the exact entity id (the
/// `RestoreEntity` path) with identical data.
#[test]
fn undo_redo_of_create_and_delete_keeps_exact_entity_ids() {
    let scene_path = seed_scene("stable-ids", "document-stable", true);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");

    let spec: EntitySpec = serde_json::from_value(json!({ "name": "temp" })).expect("valid spec");
    let created = document
        .create_entity(document.session().revision(), spec)
        .expect("create");
    assert_eq!(created, 2);
    document.undo().expect("undo create");
    assert!(!document.session().world().entities.contains_key(&2));
    document.redo().expect("redo create");
    assert_eq!(
        document.session().world().entities[&2].name,
        "temp",
        "the redo restore brings back the exact id"
    );

    document.select([1]);
    let before_delete = document.session().world().entities[&1].clone();
    document
        .delete_entity(document.session().revision(), 1)
        .expect("delete");
    assert!(
        document.selection().is_empty(),
        "deleting drops the entity from the selection"
    );
    assert!(!document.session().world().entities.contains_key(&1));
    document.undo().expect("undo delete");
    assert_eq!(
        document.session().world().entities[&1],
        before_delete,
        "the undo restores identical data under the same id"
    );
    document.redo().expect("redo delete");
    assert!(!document.session().world().entities.contains_key(&1));
    document.undo().expect("undo again");
    assert_eq!(document.session().world().entities[&1], before_delete);
    let _ = fs::remove_file(&scene_path);
}

/// Play mode drives the session simulation (ticks advance through the
/// started provider), and exit restores the exact pre-play world: play-time
/// entities disappear, the tick returns to its pre-play value, the revision
/// moves forward, and the document is conservatively dirty.
#[test]
fn play_mode_restores_the_pre_play_world_on_exit() {
    let scene_path = seed_scene("play", "document-play", false);
    let mut document = EditorDocument::new(session_for_play(), &scene_path).expect("open");
    let revision_before = document.session().revision();

    // World content attached before play (a script, here) survives as part
    // of the restored pre-play world.
    document
        .attach_script(
            document.session().revision(),
            1,
            "function tick() {}".to_string(),
        )
        .expect("attach before play");
    document.enter_play().expect("enter play");
    assert!(document.is_playing());
    assert!(!document.is_paused());
    document.pause();
    assert!(document.is_paused());
    document.resume();
    assert!(!document.is_paused());

    document
        .session_mut()
        .step(3)
        .expect("ticks run through the started simulation provider");
    assert_eq!(
        document.session().world().tick,
        3,
        "the simulation ticks during play"
    );

    // Direct session commands model play-time simulation side effects; the
    // document edit ops stay locked (asserted separately).
    let spec: EntitySpec =
        serde_json::from_value(json!({ "name": "play-only" })).expect("valid spec");
    document
        .session_mut()
        .apply(Command::Spawn { spec })
        .expect("play-time spawn");
    assert_eq!(document.session().world().entities.len(), 2);

    document.exit_play().expect("exit play");
    assert!(!document.is_playing());
    assert!(!document.is_paused());
    assert_eq!(
        document.session().world().entities.len(),
        1,
        "the play-time entity is gone"
    );
    assert_eq!(
        document.session().world().tick,
        0,
        "the tick returns to the pre-play value"
    );
    assert!(
        document.session().world().entities[&1].script.is_some(),
        "pre-play content, including the pre-play script, is restored"
    );
    assert!(
        document.session().revision() > revision_before,
        "the restore moves the revision forward"
    );
    assert!(
        document.is_dirty(),
        "the exit restore is conservatively dirty"
    );
    let _ = fs::remove_file(&scene_path);
}

/// While playing, every document edit operation, save, scene switch, and a
/// second enter_play refuses with the play-mode lock message; the play
/// state itself is untouched by the refusals.
#[test]
fn play_mode_blocks_edits_saves_and_scene_switches() {
    let scene_path = seed_scene("play-lock", "document-play-lock", false);
    let mut document = EditorDocument::new(session_for_play(), &scene_path).expect("open");
    document.enter_play().expect("enter play");
    let revision = document.session().revision();
    let spec: EntitySpec =
        serde_json::from_value(json!({ "name": "blocked" })).expect("valid spec");
    let rejects = |result: anyhow::Result<()>| {
        let error = result.expect_err("playing documents refuse edits");
        assert!(
            error.to_string().contains("playing"),
            "the refusal names the play lock: {error}"
        );
    };
    rejects(document.apply(
        "blocked",
        revision,
        Command::Spawn { spec: spec.clone() },
        |_| Vec::new(),
    ));
    rejects(document.create_entity(revision, spec).map(|_| ()));
    rejects(document.delete_entity(revision, 1));
    rejects(document.rename_entity(revision, 1, "blocked"));
    rejects(document.set_transform(
        revision,
        1,
        Transform {
            x: 1.0,
            y: 1.0,
            rotation: 0.0,
        },
    ));
    rejects(document.attach_component(revision, 1, COMPONENT, json!({ "current": 5 })));
    rejects(document.remove_component(revision, 1, COMPONENT));
    rejects(document.attach_script(revision, 1, "function f() {}".to_string()));
    rejects(document.undo().map(|_| ()));
    rejects(document.redo().map(|_| ()));
    rejects(document.save());
    rejects(document.save_as("target/document-play-copy.mnsc"));
    rejects(document.open_scene("target/wherever.mnsc"));
    rejects(document.enter_play());

    assert!(document.is_playing(), "refusals keep the play state");
    assert!(
        !document.is_dirty() || document.session().revision() >= revision,
        "ticks may move the revision but refusals do not mutate"
    );
    document.exit_play().expect("exit play");
    let _ = fs::remove_file(&scene_path);
}

/// open_scene refuses a dirty document, resets document state on success,
/// and keeps registry-validated dynamic component values intact.
#[test]
fn open_scene_guards_dirty_swaps_state_and_keeps_components() {
    let scene_a = seed_scene("open-a", "document-open-a", false);
    let scene_b = seed_scene("open-b", "document-open-b", true);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_a).expect("open");

    let spec: EntitySpec =
        serde_json::from_value(json!({ "name": "dirty-maker" })).expect("valid spec");
    let created = document
        .create_entity(document.session().revision(), spec)
        .expect("create");
    document.select([created]);
    assert!(document.is_dirty());

    let error = document
        .open_scene(&scene_b)
        .expect_err("a dirty document must refuse scene switches");
    assert!(
        error.to_string().contains("unsaved changes"),
        "the refusal asks for a save first: {error}"
    );
    assert_eq!(
        document.scene_path(),
        scene_a.as_str(),
        "the failed open keeps the old scene"
    );
    assert!(
        document.session().world().entities.contains_key(&created),
        "the failed open keeps the old world"
    );

    // After saving (undoing does not clear the revision-based dirty flag),
    // the switch succeeds and resets state.
    document.save().expect("save the dirty changes");
    assert!(!document.is_dirty());
    document
        .open_scene(&scene_b)
        .expect("open the clean switch");
    assert_eq!(document.scene_path(), scene_b.as_str());
    assert_eq!(document.scene_name(), "document-open-b");
    assert_eq!(
        document.undo_depth(),
        0,
        "history describes the previous scene and is cleared"
    );
    assert_eq!(document.redo_depth(), 0);
    assert!(document.selection().is_empty());
    assert!(
        !document.is_dirty(),
        "the new scene re-anchors the dirty state"
    );
    assert!(
        !document.session().world().entities.contains_key(&created),
        "the previous scene's entities are gone"
    );
    assert_eq!(
        document.session().world().entities[&1].extensions[COMPONENT],
        json!({ "current": 80, "maximum": 100 }),
        "registry-validated dynamic components keep their values"
    );
    let _ = fs::remove_file(&scene_a);
    let _ = fs::remove_file(&scene_b);
}

/// attach_script replaces through the engine validation, undo restores the
/// previous source, a first attach cannot be undone to scriptless (the core
/// command set has no script removal), and oversized sources surface
/// `script_too_large` without changing anything.
#[test]
fn attach_script_replaces_with_undo_and_surfaces_size_limits() {
    let scene_path = seed_scene("attach-script", "document-attach-script", false);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");

    document
        .attach_script(
            document.session().revision(),
            1,
            "function first() {}".to_string(),
        )
        .expect("first attach");
    assert_eq!(
        document.session().world().entities[&1]
            .script
            .as_ref()
            .expect("script attached")
            .source,
        "function first() {}"
    );

    // Replacing keeps one history step whose undo restores the previous
    // source.
    document
        .attach_script(
            document.session().revision(),
            1,
            "function second() {}".to_string(),
        )
        .expect("replace attach");
    assert_eq!(document.undo_depth(), 2, "both attach steps stack");
    assert_eq!(document.undo().expect("undo"), Some("attach_script"));
    assert_eq!(
        document.session().world().entities[&1]
            .script
            .as_ref()
            .expect("script restored")
            .source,
        "function first() {}"
    );
    document.redo().expect("redo");
    assert_eq!(
        document.session().world().entities[&1]
            .script
            .as_ref()
            .expect("script re-attached")
            .source,
        "function second() {}"
    );

    // Undoing the very first attach cannot restore "no script": the label is
    // reported and the script stays attached.
    let mut fresh =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("reopen");
    fresh
        .attach_script(
            fresh.session().revision(),
            1,
            "function only() {}".to_string(),
        )
        .expect("attach");
    assert_eq!(fresh.undo().expect("undo"), Some("attach_script"));
    assert!(
        fresh.session().world().entities[&1].script.is_some(),
        "the first attach cannot be undone to scriptless"
    );

    // An oversized source is rejected by the engine before any state
    // changes.
    let oversized = "x".repeat(MAX_SCRIPT_BYTES + 1);
    let error = fresh
        .attach_script(fresh.session().revision(), 1, oversized)
        .expect_err("an oversized source must be rejected");
    let engine_error = error.downcast_ref::<EngineError>().expect("engine error");
    assert_eq!(engine_error.code, "script_too_large");
    assert_eq!(
        fresh.session().world().entities[&1]
            .script
            .as_ref()
            .expect("script untouched")
            .source,
        "function only() {}"
    );
    let _ = fs::remove_file(&scene_path);
}

/// The document edits only through the session command API; direct engine
/// world mutation would bypass revision and event bookkeeping.
#[test]
fn document_source_never_takes_direct_world_access() {
    let source = include_str!("document.rs");
    assert!(
        !source.contains("world_mut"),
        "document.rs must edit through RuntimeSession::apply only"
    );
}

/// Viewport drag semantics: `preview_apply` moves the world without
/// touching the history stacks (revision bumps and the dirty flag follow
/// the session as with any apply), and `record_external_edit` commits the
/// whole drag as exactly one entry that undo/redo walk.
#[test]
fn drag_previews_leave_history_alone_and_commit_one_entry() {
    let scene_path = seed_scene("drag", "document-drag", false);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");
    // One normal edit so the undo stack is non-trivial.
    document
        .rename_entity(document.session().revision(), 1, "renamed")
        .expect("rename");
    let revision = document.session().revision();
    let start = Transform::default();
    let moved = Transform {
        x: 1.5,
        y: 0.0,
        rotation: 0.0,
    };

    // Intermediate previews: revision, dirty state, and world content move;
    // the stacks do not.
    for x in [0.5, 1.0, 1.5] {
        document
            .preview_apply(Command::SetTransform {
                entity: 1,
                transform: Transform {
                    x,
                    y: 0.0,
                    rotation: 0.0,
                },
            })
            .expect("preview");
    }
    assert!(
        document.session().revision() > revision,
        "previews bump the revision like any session apply"
    );
    assert_eq!(document.undo_depth(), 1, "previews never record history");
    assert_eq!(document.redo_depth(), 0);
    assert_eq!(document.session().world().entities[&1].transform, moved);
    assert!(document.is_dirty());

    // Release commit: exactly one additional entry, inverse to the start.
    document
        .record_external_edit(
            "move entity",
            vec![Command::SetTransform {
                entity: 1,
                transform: start,
            }],
            vec![Command::SetTransform {
                entity: 1,
                transform: moved,
            }],
        )
        .expect("commit");
    assert_eq!(document.undo_depth(), 2);
    assert_eq!(document.redo_depth(), 0);
    assert_eq!(document.undo().expect("undo"), Some("move entity"));
    assert_eq!(
        document.session().world().entities[&1].transform,
        start,
        "the undo restores the drag-start transform"
    );
    assert_eq!(document.redo().expect("redo"), Some("move entity"));
    assert_eq!(document.session().world().entities[&1].transform, moved);
    let _ = fs::remove_file(&scene_path);
}

/// An unchanged drag (snapped back to where it began) restores the start
/// transform exactly and records nothing; a commit also clears the redo
/// stack like a normal apply.
#[test]
fn unchanged_drag_restores_exactly_and_records_nothing() {
    let scene_path = seed_scene("drag-restore", "document-drag-restore", false);
    let mut document =
        EditorDocument::new(session_with_health(HEALTH_VERSION), &scene_path).expect("open");
    let start = Transform::default();
    let moved = Transform {
        x: 2.0,
        y: 0.0,
        rotation: 0.0,
    };

    // The unchanged-drag path: preview the restore through preview_apply
    // (the same path the UI uses) and record nothing.
    document
        .preview_apply(Command::SetTransform {
            entity: 1,
            transform: moved,
        })
        .expect("preview");
    document
        .preview_apply(Command::SetTransform {
            entity: 1,
            transform: start,
        })
        .expect("restore");
    assert_eq!(document.undo_depth(), 0, "nothing was recorded");
    assert_eq!(document.redo_depth(), 0);
    assert_eq!(
        document.session().world().entities[&1].transform,
        start,
        "the world is back at the drag-start transform exactly"
    );

    // A commit clears the redo stack like a normal apply.
    document
        .rename_entity(document.session().revision(), 1, "again")
        .expect("rename");
    document.undo().expect("undo the rename");
    assert_eq!(document.redo_depth(), 1);
    document
        .record_external_edit(
            "move entity",
            vec![Command::SetTransform {
                entity: 1,
                transform: start,
            }],
            vec![Command::SetTransform {
                entity: 1,
                transform: moved,
            }],
        )
        .expect("commit");
    assert_eq!(document.redo_depth(), 0, "the commit clears the redo stack");
    assert_eq!(document.undo_depth(), 1);
    let _ = fs::remove_file(&scene_path);
}

/// The drag paths respect the play-mode edit lock exactly like `apply`.
#[test]
fn play_mode_locks_the_drag_paths() {
    let scene_path = seed_scene("drag-play", "document-drag-play", false);
    let mut document = EditorDocument::new(session_for_play(), &scene_path).expect("open");
    document.enter_play().expect("enter play");
    let error = document
        .preview_apply(Command::SetTransform {
            entity: 1,
            transform: Transform::default(),
        })
        .expect_err("playing documents refuse previews");
    assert!(error.to_string().contains("playing"), "{error}");
    let error = document
        .record_external_edit(
            "move entity",
            vec![Command::SetTransform {
                entity: 1,
                transform: Transform::default(),
            }],
            vec![Command::SetTransform {
                entity: 1,
                transform: Transform {
                    x: 1.0,
                    y: 0.0,
                    rotation: 0.0,
                },
            }],
        )
        .expect_err("playing documents refuse commits");
    assert!(error.to_string().contains("playing"), "{error}");
    assert_eq!(
        document.undo_depth(),
        0,
        "the refused commit records nothing"
    );
    document.exit_play().expect("exit play");
    let _ = fs::remove_file(&scene_path);
}
