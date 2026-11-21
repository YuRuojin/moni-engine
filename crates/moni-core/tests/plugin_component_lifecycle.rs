//! Cross-cutting acceptance test for the component lifecycle stage: a
//! third-party static plugin registers a schema-validated component during
//! the registration phase, a v1 scene migrates and loads through the session
//! registry, the component is edited over the protocol, saved with a version
//! table, unregistered (refused while in use, then allowed), and re-registered.
//! Written against public APIs only — the same surface an out-of-tree plugin
//! would use.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use moni_core::components::{ComponentDescriptor, PropertyField, PropertySchema, PropertyType};
use moni_core::engine::{Engine, Entity, Transform, World};
use moni_core::host::{
    Plugin, PluginContext, PluginError, PluginOutput, PluginRegistry, SystemStage,
};
use moni_core::profile::Profile;
use moni_core::scene;
use moni_core::session::RuntimeSession;

const COMPONENT: &str = "game.health";
const OWNER: &str = "third.game";
const SCENE_VERSION_V1: u32 = 1;
const SCENE_VERSION_V2: u32 = 2;

/// Third-party double: contributes `game.health` (v2 schema plus a v1→v2
/// migrator) during registration, nothing at start.
struct HealthPlugin;

impl Plugin for HealthPlugin {
    fn id(&self) -> &'static str {
        OWNER
    }

    fn systems(&self) -> Vec<moni_core::host::SystemDescriptor> {
        Vec::new()
    }

    fn register(&mut self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
        registry.register_component(ComponentDescriptor::dynamic(
            COMPONENT,
            None,
            OWNER,
            SCENE_VERSION_V2,
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
        registry.register_migrator(
            COMPONENT,
            SCENE_VERSION_V1,
            SCENE_VERSION_V2,
            Box::new(|value| {
                let object = value
                    .as_object()
                    .ok_or_else(|| "v1 health must be an object".to_string())?;
                let hp = object
                    .get("hp")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| "v1 health requires integer 'hp'".to_string())?;
                Ok(json!({ "current": hp, "maximum": 100 }))
            }),
        )?;
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

fn session_with_plugin() -> RuntimeSession {
    let mut session =
        RuntimeSession::new(Engine::new(), Profile::named("minimal").expect("minimal"));
    session
        .host_mut()
        .register_plugin(Box::new(HealthPlugin), &[])
        .expect("third-party plugin registers");
    session
}

/// Scene paths are workspace-relative by contract (same rule as the
/// protocol's save/load), so the test writes under the crate's target dir.
fn temp_scene_path(label: &str) -> PathBuf {
    PathBuf::from(format!(
        "target/lifecycle-{label}-{}.mnsc",
        std::process::id()
    ))
}

/// Writes the initial v2 scene (registry-current values), then rewrites it
/// into a v1 scene: old value shape plus a version table entry of 1.
fn write_v1_scene(session: &RuntimeSession, path: &PathBuf) {
    let mut world = World::default();
    world.entities.insert(
        1,
        Entity {
            id: 1,
            name: "hero".into(),
            transform: Transform::default(),
            sprite: None,
            model: None,
            script: None,
            extensions: BTreeMap::from([(
                COMPONENT.to_string(),
                json!({ "current": 80, "maximum": 100 }),
            )]),
        },
    );
    world.next_entity_id = 2;
    scene::save_scene_with_registry(&world, "lifecycle", path, session.engine().registry())
        .expect("v2 scene saves");
    let mut scene_json: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("scene readable"))
            .expect("scene parses");
    let entity = &mut scene_json["entities"][0];
    entity["components"][COMPONENT] = json!({ "hp": 80 });
    scene_json["extensions"][scene::COMPONENT_VERSIONS_KEY] =
        json!({ COMPONENT: SCENE_VERSION_V1 });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&scene_json).expect("serialize"),
    )
    .expect("v1 scene written");
}

#[test]
fn third_party_component_lifecycle_end_to_end() {
    let scene_path = temp_scene_path("full");
    let mut session = session_with_plugin();

    // Registration phase: the component is in the effective registry before
    // any plugin is started.
    assert!(!session.host_mut().is_started(OWNER));
    assert!(session.engine().registry().resolve(COMPONENT).is_some());

    write_v1_scene(&session, &scene_path);

    // Scene load migrates v1 → v2 through the registered migrator and
    // validates against the session registry.
    let loaded = session
        .load_scene(scene_path.to_str().expect("utf8 path"))
        .expect("v1 scene loads");
    assert_eq!(loaded["migrated"], json!([COMPONENT]));
    let value = session.world().entities[&1]
        .extensions
        .get(COMPONENT)
        .expect("component present after load");
    assert_eq!(value, &json!({ "current": 80, "maximum": 100 }));

    // Edit over the protocol: attach applies schema defaults.
    let (response, _) = session.handle_line(
        &json!({
            "id": "attach",
            "command": "attach_component",
            "args": { "entity": 1, "component": COMPONENT, "value": { "current": 50 } },
        })
        .to_string(),
    );
    assert!(response.ok, "attach succeeds: {:?}", response.error);
    let value = session.world().entities[&1]
        .extensions
        .get(COMPONENT)
        .expect("component present after attach");
    assert_eq!(
        value,
        &json!({ "current": 50, "maximum": 100 }),
        "defaults applied"
    );

    // Save through the registry-aware path: the version table reflects v2.
    scene::save_scene_with_registry(
        session.world(),
        "lifecycle",
        &scene_path,
        session.engine().registry(),
    )
    .expect("scene saves");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&scene_path).expect("saved scene readable"))
            .expect("saved scene parses");
    assert_eq!(
        saved["extensions"][scene::COMPONENT_VERSIONS_KEY][COMPONENT],
        json!(SCENE_VERSION_V2)
    );

    // Unregistering refuses while entities still carry the component.
    let error = session
        .host_mut()
        .unregister(OWNER)
        .expect_err("in-use component must block unregistration");
    assert!(format!("{error}").contains(COMPONENT));
    assert!(session.engine().registry().resolve(COMPONENT).is_some());

    // Removing the last usage unlocks plain unregistration.
    let (response, _) = session.handle_line(
        &json!({
            "id": "remove",
            "command": "remove_component",
            "args": { "entity": 1, "component": COMPONENT },
        })
        .to_string(),
    );
    assert!(response.ok, "remove succeeds: {:?}", response.error);
    let removed = session
        .host_mut()
        .unregister(OWNER)
        .expect("unregisters once unused");
    assert_eq!(removed, vec![OWNER]);
    assert!(session.engine().registry().resolve(COMPONENT).is_none());

    // Re-registration starts the cycle over cleanly.
    session
        .host_mut()
        .register_plugin(Box::new(HealthPlugin), &[])
        .expect("plugin re-registers");
    assert!(session.engine().registry().resolve(COMPONENT).is_some());
    let (response, _) = session.handle_line(
        &json!({
            "id": "reattach",
            "command": "attach_component",
            "args": { "entity": 1, "component": COMPONENT, "value": { "current": 99 } },
        })
        .to_string(),
    );
    assert!(
        response.ok,
        "attach works after re-registration: {:?}",
        response.error
    );

    let _ = std::fs::remove_file(&scene_path);
}

#[test]
fn invalid_scene_value_never_replaces_the_session_world() {
    let scene_path = temp_scene_path("invalid");
    let mut session = session_with_plugin();

    // A v1 scene whose migrator output violates the v2 schema ('current' is
    // not an integer): load must fail and leave the session untouched.
    write_v1_scene(&session, &scene_path);
    let mut scene_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&scene_path).expect("scene readable"))
            .expect("scene parses");
    scene_json["entities"][0]["components"][COMPONENT] = json!({ "hp": "not-a-number" });
    std::fs::write(
        &scene_path,
        serde_json::to_string_pretty(&scene_json).expect("serialize"),
    )
    .expect("broken scene written");

    let (revision, event_seq) = (session.revision(), session.event_seq());
    let error = session
        .load_scene(scene_path.to_str().expect("utf8 path"))
        .expect_err("migrator output violating the schema must fail");
    assert_eq!(error.code, "invalid_component");
    assert_eq!(
        session.revision(),
        revision,
        "failed load keeps the revision"
    );
    assert_eq!(
        session.event_seq(),
        event_seq,
        "failed load emits no events"
    );
    assert!(
        session.world().entities.is_empty(),
        "failed load keeps the old world"
    );

    let _ = std::fs::remove_file(&scene_path);
}
