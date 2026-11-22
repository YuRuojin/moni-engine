//! Project-mode integration for the editor document: with the process
//! working directory moved to the project root (what `moni editor
//! --project` and in-app project opens do), the document resolves, edits,
//! and saves every workspace-relative path under the project root.
//!
//! This lives in its own integration-test binary because the working
//! directory is process-wide: unit tests in the same binary as the document
//! tests run in parallel and must not chdir.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use moni_core::engine::{Engine, Entity, ScriptState, Transform, World};
use moni_core::profile::Profile;
use moni_core::project::GameProject;
use moni_core::scene;
use moni_core::session::RuntimeSession;
use moni_editor::document::EditorDocument;

fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "moni-editor-project-integration-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temp project root");
    root
}

fn world_with_main_scene() -> World {
    let mut world = World::default();
    world.entities.insert(
        1,
        Entity {
            id: 1,
            name: "hero".into(),
            transform: Transform::default(),
            sprite: None,
            model: None,
            // A path-loaded script: the engine reads it against the working
            // directory, so a successful load proves project-root
            // resolution for scripts too.
            script: Some(ScriptState {
                source: String::new(),
                source_path: Some("scripts/main.js".to_string()),
                enabled: true,
                last_error: None,
            }),
            extensions: BTreeMap::new(),
        },
    );
    world.next_entity_id = 2;
    world
}

#[test]
fn project_scene_edit_and_save_resolve_under_the_project_root() {
    let root = temp_root();

    // Seed the project: manifest, main scene, alternate scene, and the
    // script file the main scene references.
    let session = RuntimeSession::new(Engine::new(), Profile::named("editor").expect("profile"));
    fs::create_dir_all(root.join("scenes")).expect("scenes dir");
    fs::create_dir_all(root.join("scripts")).expect("scripts dir");
    fs::write(
        root.join("scripts/main.js"),
        "function tick() {}".as_bytes(),
    )
    .expect("write script");
    scene::save_scene_with_registry(
        &world_with_main_scene(),
        "integration-main",
        &root.join("scenes/main.mnsc"),
        session.engine().registry(),
    )
    .expect("seed main scene");
    scene::save_scene_with_registry(
        &World::default(),
        "integration-alt",
        &root.join("scenes/alt.mnsc"),
        session.engine().registry(),
    )
    .expect("seed alt scene");
    fs::write(
        root.join("game.mngp"),
        serde_json::json!({
            "format": "moni-game-project",
            "version": 1,
            "name": "integration",
            "engine": env!("CARGO_PKG_VERSION"),
            "profile": "default",
            "scene": "scenes/main.mnsc",
        })
        .to_string(),
    )
    .expect("write game.mngp");

    // The composition-root open flow: load the project, verify the main
    // scene, then move the working directory to the project root.
    let project = GameProject::load(&root).expect("load project");
    let main_scene = project.scene_path(&root).expect("main scene path");
    assert!(main_scene.is_file(), "the main scene exists under the root");
    std::env::set_current_dir(&root).expect("chdir into the project root");

    // From here on every workspace-relative path resolves under the project
    // root — including the scene's script path.
    let mut document =
        EditorDocument::new(session, &project.scene).expect("open the project main scene");
    assert_eq!(document.scene_path(), "scenes/main.mnsc");
    assert_eq!(document.scene_name(), "integration-main");

    document
        .rename_entity(document.session().revision(), 1, "renamed-in-project")
        .expect("edit");
    assert!(document.is_dirty());
    document.save().expect("save under the project root");

    // The save landed inside the project, not next to the test binary.
    let saved = fs::read_to_string(root.join("scenes/main.mnsc")).expect("saved scene readable");
    assert!(
        saved.contains("renamed-in-project"),
        "the edit reached the project's scene file"
    );

    // A scene switch through the guarded path resolves under the root too.
    document
        .open_scene("scenes/alt.mnsc")
        .expect("switch scenes inside the project");
    assert_eq!(document.scene_path(), "scenes/alt.mnsc");
    assert!(document.session().world().entities.is_empty());
    assert!(
        !document.is_dirty(),
        "the fresh scene re-anchors dirty state"
    );
}
