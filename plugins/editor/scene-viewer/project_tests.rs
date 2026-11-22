//! Tests for project loading, the recent-projects list, and project
//! creation. Every test uses its own temporary directories so nothing is
//! written to the real user config directory.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{EditorProject, RECENT_LIMIT, load_recent_projects, record_recent_project};

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "moni-editor-project-test-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temp root");
    root
}

fn write_project(root: &Path, scene: &str) {
    fs::write(
        root.join("game.mngp"),
        json!({
            "format": "moni-game-project",
            "version": 1,
            "name": root.file_name().and_then(|name| name.to_str()).expect("utf8 name"),
            "engine": env!("CARGO_PKG_VERSION"),
            "profile": "default",
            "scene": scene,
        })
        .to_string(),
    )
    .expect("write game.mngp");
}

#[test]
fn load_at_rejects_missing_project_and_missing_main_scene() {
    let root = temp_root("missing");
    let error = EditorProject::load_at(&root).expect_err("no game.mngp must fail");
    assert!(
        error.to_string().contains("game.mngp"),
        "the error names the missing manifest: {error}"
    );

    write_project(&root, "scenes/main.mnsc");
    let error = EditorProject::load_at(&root).expect_err("missing main scene must fail");
    assert!(
        error.to_string().contains("main scene"),
        "the error names the missing scene: {error}"
    );

    fs::create_dir_all(root.join("scenes")).expect("scenes dir");
    fs::write(root.join("scenes/main.mnsc"), "{\"name\":\"p\"}").expect("write scene");
    let project = EditorProject::load_at(&root).expect("complete project loads");
    assert_eq!(project.root, root);
    assert_eq!(project.main_scene, "scenes/main.mnsc");
    assert_eq!(project.profile_name, "default");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn recent_projects_persist_rotate_and_dedupe() {
    let config_dir = temp_root("recents");
    assert!(load_recent_projects(&config_dir).is_empty(), "no file yet");

    for index in 0..12 {
        let recents = record_recent_project(
            &config_dir,
            &format!("C:/projects/game-{index}"),
            &format!("game-{index}"),
        )
        .expect("record");
        assert_eq!(recents[0].path, format!("C:/projects/game-{index}"));
    }
    let loaded = load_recent_projects(&config_dir);
    assert_eq!(loaded.len(), RECENT_LIMIT, "the list stays bounded");
    assert_eq!(loaded[0].path, "C:/projects/game-11");
    assert_eq!(
        loaded[RECENT_LIMIT - 1].path,
        format!("C:/projects/game-{}", 12 - RECENT_LIMIT),
        "oldest entries drop off the end"
    );
    assert_eq!(loaded[0].name, "game-11");

    // Re-recording an existing path moves it to the front instead of
    // duplicating it, including the Windows backslash spelling.
    let recents =
        record_recent_project(&config_dir, "C:/projects/game-3", "game-3").expect("re-record");
    assert_eq!(recents.len(), RECENT_LIMIT);
    assert_eq!(recents[0].path, "C:/projects/game-3");
    assert!(
        recents[1..]
            .iter()
            .all(|entry| entry.path != "C:/projects/game-3"),
        "no duplicate entries"
    );
    let recents =
        record_recent_project(&config_dir, "C:\\projects\\game-3", "game-3").expect("re-record 2");
    assert_eq!(
        recents[0].path, "C:/projects/game-3",
        "separator variants normalize to one entry"
    );
    assert_eq!(recents.len(), RECENT_LIMIT);

    // A malformed file degrades to an empty list, and recording still works.
    fs::write(config_dir.join(".moni/editor-recent.json"), "{not json").expect("corrupt file");
    assert!(
        load_recent_projects(&config_dir).is_empty(),
        "malformed content reads as empty"
    );
    let recents =
        record_recent_project(&config_dir, "C:/projects/fresh", "fresh").expect("recover");
    assert_eq!(recents.len(), 1);
    let _ = fs::remove_dir_all(&config_dir);
}

#[test]
fn record_creates_missing_config_directories() {
    let config_dir = temp_root("nested").join("deep/deeper");
    record_recent_project(&config_dir, "C:/projects/x", "x").expect("record creates parents");
    assert!(config_dir.join(".moni/editor-recent.json").is_file());
    let _ = fs::remove_dir_all(&config_dir);
}
