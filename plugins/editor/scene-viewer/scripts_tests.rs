//! Tests for the tsc diagnostic parser, the raw-output tail, and the
//! diagnostic-file -> entity mapping. Pure functions only; no build is
//! spawned here.

use std::collections::BTreeMap;

use serde_json::json;

use moni_core::engine::{Entity, ScriptState, Transform, World};

use super::{OUTPUT_TAIL_CHARS, find_entity_by_script_source, output_tail, parse_ts_diagnostics};

fn entity(id: u64, source_path: Option<&str>, source: Option<&str>) -> (u64, Entity) {
    (
        id,
        Entity {
            id,
            name: format!("entity-{id}"),
            transform: Transform::default(),
            sprite: None,
            model: None,
            script: source_path.or(source).map(|source| ScriptState {
                source: source.to_string(),
                source_path: source_path.map(str::to_string),
                enabled: true,
                last_error: None,
            }),
            extensions: BTreeMap::new(),
        },
    )
}

fn world_with(entities: Vec<(u64, Entity)>) -> World {
    let next_entity_id = entities.iter().map(|(id, _)| *id).max().unwrap_or(0) + 1;
    let mut world = World::default();
    for (id, entity) in entities {
        world.entities.insert(id, entity);
    }
    world.next_entity_id = next_entity_id;
    world
}

#[test]
fn parser_reads_plain_tsc_lines_and_ignores_the_rest() {
    let output = "\
scripts/player.ts(12,5): error TS2322: Type 'number' is not assignable to type 'string'.
scripts/enemy.ts(3,1): warning TS6133: 'speed' is declared but its value is never read.
C:/projects/game/scripts/deep/nested.ts(40,9): error TS2551: Property 'postion' does not exist. Did you mean 'position'?
error TS18003: No inputs were found in config file 'tsconfig.json'.
npm notice run build finished
Found 2 errors.
    at createDiagnostics (node:internal)
";
    let diagnostics = parse_ts_diagnostics(output);
    assert_eq!(diagnostics.len(), 3, "only path-position lines parse");
    assert_eq!(diagnostics[0].file, "scripts/player.ts");
    assert_eq!(diagnostics[0].line, 12);
    assert_eq!(diagnostics[0].col, 5);
    assert_eq!(diagnostics[0].code, "TS2322");
    assert_eq!(
        diagnostics[0].message,
        "Type 'number' is not assignable to type 'string'."
    );
    assert_eq!(diagnostics[1].code, "TS6133");
    assert_eq!(
        diagnostics[2].file,
        "C:/projects/game/scripts/deep/nested.ts"
    );
}

#[test]
fn parser_is_lenient_about_noise_and_separators() {
    assert!(parse_ts_diagnostics("").is_empty());
    assert!(parse_ts_diagnostics("garbage line").is_empty());
    assert!(parse_ts_diagnostics("file.ts(nocolon): error TS1: x").is_empty());
    assert!(parse_ts_diagnostics("file.ts(abc,1): error TS1: x").is_empty());
    assert!(parse_ts_diagnostics("file.ts(1,1): notice TS1: x").is_empty());
    assert!(parse_ts_diagnostics("file.ts(1,1): error BAD1: x").is_empty());
    // Windows separators pass through unchanged into the file field; suffix
    // matching normalizes them later.
    let diagnostics = parse_ts_diagnostics("scripts\\main.ts(1,1): error TS1005: ';' expected.");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].file, "scripts\\main.ts");
    // Multi-line pretty output cannot be parsed; only the first line of a
    // wrapped diagnostic matches, continuations are ignored.
    let pretty =
        "scripts/main.ts(1,1): error TS2322: Type 'number' is not\n  assignable to type 'string'.";
    assert_eq!(parse_ts_diagnostics(pretty).len(), 1);
}

#[test]
fn locate_matches_script_source_paths_by_suffix() {
    // Path-loaded scene scripts carry `source_path` (the raw `core.script`
    // source field), which is the primary match target.
    let world = world_with(vec![
        entity(1, Some("build/main.js"), None),
        entity(2, Some("scripts/player.ts"), None),
    ]);
    assert_eq!(
        find_entity_by_script_source(&world, "scripts/player.ts"),
        Some(2)
    );
    assert_eq!(
        find_entity_by_script_source(&world, "project/scripts/player.ts"),
        Some(2),
        "a deeper diagnostic path still matches by suffix"
    );
    assert_eq!(
        find_entity_by_script_source(&world, "player.ts"),
        Some(2),
        "a bare file name matches by suffix"
    );
    assert_eq!(
        find_entity_by_script_source(&world, "scripts/main.ts"),
        None,
        "a compiled build/main.js source does not claim a .ts error"
    );
    assert_eq!(
        find_entity_by_script_source(&world, "scripts/other.ts"),
        None
    );

    // Inline attached sources are claimed only when they look like paths,
    // never when they are code.
    let inline = world_with(vec![entity(
        1,
        None,
        Some("function tick(ctx, dt) { return; }"),
    )]);
    assert_eq!(
        find_entity_by_script_source(&inline, "scripts/main.ts"),
        None
    );
    let path_like = world_with(vec![entity(1, None, Some("scripts/inline.ts"))]);
    assert_eq!(
        find_entity_by_script_source(&path_like, "scripts/inline.ts"),
        Some(1)
    );
}

#[test]
fn locate_prefers_the_lowest_matching_entity() {
    let world = world_with(vec![
        entity(7, Some("scripts/main.ts"), None),
        entity(3, Some("game/scripts/main.ts"), None),
    ]);
    assert_eq!(
        find_entity_by_script_source(&world, "scripts/main.ts"),
        Some(3),
        "both entities reference the file; the lower id wins"
    );
}

#[test]
fn entities_without_scripts_never_match() {
    let world = world_with(vec![entity(1, None, None)]);
    assert_eq!(find_entity_by_script_source(&world, "anything.ts"), None);
}

#[test]
fn output_tail_is_bounded_and_starts_on_a_line_boundary() {
    let text = "line\n".repeat(OUTPUT_TAIL_CHARS / 5 + 10);
    let tail = output_tail(&text, OUTPUT_TAIL_CHARS);
    assert!(tail.chars().count() <= OUTPUT_TAIL_CHARS);
    assert!(
        tail.starts_with("line\n"),
        "the first partial line is dropped"
    );
    assert!(tail.ends_with("line\n"));
    assert_eq!(output_tail("short", OUTPUT_TAIL_CHARS), "short");
}

#[test]
fn world_json_round_trip_keeps_script_state_shape() {
    // The mapping reads the same `ScriptState` the world serializes; this
    // pins the field shape the panel depends on.
    let world = world_with(vec![entity(1, Some("build/main.js"), None)]);
    let value = serde_json::to_value(&world).expect("world serializes");
    assert_eq!(
        value["entities"]["1"]["script"],
        json!({
            "source": "build/main.js",
            "source_path": "build/main.js",
            "enabled": true,
            "last_error": null,
        }),
        "path-loaded scripts keep the raw path beside the read source"
    );
}
