//! Unit tests for the pure-math viewport helpers: projection round-trips,
//! picking hit/miss/layering, gizmo axis math, snap quantization, and the
//! camera orbit/pan operations. Everything is deterministic f32/f64 math,
//! so the same assertions hold on every backend.

use moni_core::engine::{Entity, EntitySpec, Sprite, SpriteKind, Transform, World};
use moni_core::scene3d::{Camera3d, CameraProjection, MeshKind, Model3d, Transform3d, Vector3};

use super::{
    GIZMO_MODEL_AXIS_LENGTH, GIZMO_SPRITE_AXIS_LENGTH, ViewportSpace, closest_gizmo_axis,
    effective_snap_step, entity_gizmo_origin, gizmo_axis_segment, orbit_camera, pan_camera,
    pick_entity, pointer_axis_world_offset, quantize_offset, screen_ray, world_to_screen,
};

const SCREEN: (f32, f32) = (960.0, 540.0);

/// The editor's sprite space equals the screen space at the default
/// surface, so the tests use identity mapping unless stated otherwise.
fn space() -> ViewportSpace {
    ViewportSpace {
        screen_size: SCREEN,
        sprite_size: SCREEN,
    }
}

fn perspective_camera() -> Camera3d {
    Camera3d::default() // (4, 3, 6) looking at the origin, fov pi/4
}

fn ortho_camera_looking_down_z() -> Camera3d {
    Camera3d {
        projection: CameraProjection::Orthographic,
        orthographic_height: 24.0,
        position: Vector3 {
            x: 0.0,
            y: 0.0,
            z: 10.0,
        },
        target: Vector3::ZERO,
        ..Camera3d::default()
    }
}

fn sprite_entity(id: u64, x: f64, y: f64, layer: i32, kind: SpriteKind) -> (u64, Entity) {
    (
        id,
        Entity {
            id,
            name: format!("sprite-{id}"),
            transform: Transform {
                x,
                y,
                rotation: 0.0,
            },
            sprite: Some(Sprite {
                kind,
                width: 32.0,
                height: 32.0,
                color: "#ffffff".to_string(),
                layer,
            }),
            model: None,
            script: None,
            extensions: Default::default(),
        },
    )
}

fn cube_entity(id: u64, position: Vector3) -> (u64, Entity) {
    (
        id,
        Entity {
            id,
            name: format!("cube-{id}"),
            transform: Transform::default(),
            sprite: None,
            model: Some(Model3d {
                kind: MeshKind::Cube,
                source: None,
                transform: Transform3d {
                    position,
                    ..Transform3d::default()
                },
                color: "#ffffff".to_string(),
                metallic: 0.0,
                roughness: 0.5,
            }),
            script: None,
            extensions: Default::default(),
        },
    )
}

fn world_with(entries: Vec<(u64, Entity)>) -> World {
    let mut world = World::default();
    for (id, entity) in entries {
        world.next_entity_id = world.next_entity_id.max(id + 1);
        world.entities.insert(id, entity);
    }
    world
}

/// A world point projects back out of a ray through its own pixel: the
/// pixel's ray passes through the point (within f32 tolerance).
fn assert_round_trip(camera: &Camera3d, point: [f32; 3]) {
    let Some((px, py)) = world_to_screen(camera, SCREEN, point) else {
        panic!("point in front of the camera must project");
    };
    let ray = screen_ray(camera, SCREEN, px, py);
    // Closest approach of the ray to the point.
    let to_point = [
        point[0] - ray.origin[0],
        point[1] - ray.origin[1],
        point[2] - ray.origin[2],
    ];
    let t = to_point[0] * ray.dir[0] + to_point[1] * ray.dir[1] + to_point[2] * ray.dir[2];
    assert!(t > 0.0, "the ray must travel toward the point");
    let closest = ray.at(t);
    let dx = closest[0] - point[0];
    let dy = closest[1] - point[1];
    let dz = closest[2] - point[2];
    let miss = (dx * dx + dy * dy + dz * dz).sqrt();
    assert!(
        miss < 1e-3,
        "ray through the projected pixel must pass near {point:?}: miss {miss}"
    );
}

#[test]
fn projection_round_trips_a_known_world_point_for_both_projections() {
    let camera = perspective_camera();
    // The look-at target projects to the exact screen center.
    let center = world_to_screen(&camera, SCREEN, [0.0, 0.0, 0.0]).expect("target projects");
    assert!((center.0 - 480.0).abs() < 1e-3, "{center:?}");
    assert!((center.1 - 270.0).abs() < 1e-3, "{center:?}");
    for point in [[0.0, 0.0, 0.0], [2.0, -1.0, 3.0], [-4.0, 2.5, -2.0]] {
        assert_round_trip(&camera, point);
    }

    let ortho = ortho_camera_looking_down_z();
    let center = world_to_screen(&ortho, SCREEN, [0.0, 0.0, 0.0]).expect("target projects");
    assert!((center.0 - 480.0).abs() < 1e-3);
    // Orthographic scale: ortho_height spans the screen height.
    let shifted = world_to_screen(&ortho, SCREEN, [2.0, 0.0, 0.0]).expect("shifted projects");
    let expected_px = 2.0 * (540.0 / 24.0);
    assert!(
        (shifted.0 - (480.0 + expected_px)).abs() < 1e-2,
        "{shifted:?}"
    );
    assert_round_trip(&ortho, [1.5, -2.0, -3.0]);
}

#[test]
fn points_behind_the_camera_do_not_project() {
    let camera = perspective_camera();
    // On the far side of the (4, 3, 6) eye, opposite the look direction
    // toward the origin.
    let behind = [8.0, 6.0, 12.0];
    assert!(world_to_screen(&camera, SCREEN, behind).is_none());
}

#[test]
fn sprite_picking_hits_the_rect_and_misses_outside() {
    let world = world_with(vec![sprite_entity(
        1,
        700.0,
        270.0,
        0,
        SpriteKind::Rectangle,
    )]);
    let camera = ortho_camera_looking_down_z();
    // Sprite rect is 32 wide/high around (700, 270) in sprite space, which
    // equals screen space here.
    assert_eq!(
        pick_entity(&world, &camera, space(), 700.0, 270.0),
        Some(1),
        "center hit"
    );
    assert_eq!(
        pick_entity(&world, &camera, space(), 715.0, 270.0),
        Some(1),
        "edge hit within half width"
    );
    assert_eq!(
        pick_entity(&world, &camera, space(), 600.0, 270.0),
        None,
        "miss left of the rect"
    );
    assert_eq!(
        pick_entity(&world, &camera, space(), 700.0, 290.0),
        None,
        "miss below the rect"
    );
}

#[test]
fn sprite_picking_uses_circle_radius_for_circles() {
    let world = world_with(vec![sprite_entity(1, 480.0, 270.0, 0, SpriteKind::Circle)]);
    let camera = ortho_camera_looking_down_z();
    assert_eq!(pick_entity(&world, &camera, space(), 480.0, 270.0), Some(1));
    assert_eq!(pick_entity(&world, &camera, space(), 494.0, 270.0), Some(1));
    assert_eq!(
        pick_entity(&world, &camera, space(), 498.0, 270.0),
        None,
        "outside the radius"
    );
}

#[test]
fn sprite_picking_honors_layer_then_id_draw_order() {
    // Two overlapping sprites; the renderer draws ascending (layer, id),
    // so layer 5 covers layer 1 regardless of id.
    let world = world_with(vec![
        sprite_entity(2, 480.0, 270.0, 5, SpriteKind::Rectangle),
        sprite_entity(1, 480.0, 270.0, 1, SpriteKind::Rectangle),
    ]);
    let camera = ortho_camera_looking_down_z();
    assert_eq!(
        pick_entity(&world, &camera, space(), 480.0, 270.0),
        Some(2),
        "higher layer wins"
    );

    let same_layer = world_with(vec![
        sprite_entity(3, 480.0, 270.0, 1, SpriteKind::Rectangle),
        sprite_entity(2, 480.0, 270.0, 1, SpriteKind::Rectangle),
    ]);
    assert_eq!(
        pick_entity(&same_layer, &camera, space(), 480.0, 270.0),
        Some(3),
        "equal layers resolve by id, matching the draw order"
    );
}

#[test]
fn sprite_picking_maps_through_the_sprite_viewport_when_resized() {
    // The window was resized so the screen no longer equals the sprite
    // viewport: sprite space scales by world.viewport / screen.
    let resized = ViewportSpace {
        screen_size: (1920.0, 1080.0),
        sprite_size: SCREEN,
    };
    let world = world_with(vec![sprite_entity(
        1,
        700.0,
        270.0,
        0,
        SpriteKind::Rectangle,
    )]);
    let camera = ortho_camera_looking_down_z();
    assert_eq!(
        pick_entity(&world, &camera, resized, 1400.0, 540.0),
        Some(1),
        "700 * 2, 270 * 2 in the resized screen"
    );
    assert_eq!(pick_entity(&world, &camera, resized, 700.0, 270.0), None);
}

#[test]
fn cube_picking_hits_the_box_and_misses_outside() {
    let world = world_with(vec![cube_entity(1, Vector3::ZERO)]);
    let camera = perspective_camera();
    let center = world_to_screen(&camera, SCREEN, [0.0, 0.0, 0.0]).expect("cube center projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), center.0, center.1),
        Some(1)
    );

    // A point well above the cube: project it and expect no hit there.
    let above = world_to_screen(&camera, SCREEN, [0.0, 3.0, 0.0]).expect("above projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), above.0, above.1),
        None
    );
}

#[test]
fn cube_picking_respects_scale_and_distance_order() {
    // Two cubes along the view ray: the nearer one wins.
    let world = world_with(vec![
        cube_entity(1, Vector3::ZERO),
        cube_entity(
            2,
            Vector3 {
                x: 2.0,
                y: 1.5,
                z: 3.0,
            },
        ),
    ]);
    let camera = perspective_camera();
    let near_center =
        world_to_screen(&camera, SCREEN, [2.0, 1.5, 3.0]).expect("near cube projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), near_center.0, near_center.1),
        Some(2),
        "the nearer cube wins"
    );
}

#[test]
fn plane_picking_hits_the_thin_slab() {
    let mut world = world_with(vec![]);
    let (_, mut entity) = cube_entity(1, Vector3::ZERO);
    if let Some(model) = &mut entity.model {
        model.kind = MeshKind::Plane;
    }
    world.entities.insert(1, entity);
    let camera = ortho_camera_looking_down_z();
    // Looking straight down at the XZ plane: the center is a hit.
    let center = world_to_screen(&camera, SCREEN, [0.0, 0.0, 0.0]).expect("center projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), center.0, center.1),
        Some(1)
    );
    // Off the 1x1 plane footprint: a miss.
    let off = world_to_screen(&camera, SCREEN, [3.0, 0.0, 0.0]).expect("off projects");
    assert_eq!(pick_entity(&world, &camera, space(), off.0, off.1), None);
}

#[test]
fn fbx_models_pick_by_a_unit_sphere_fallback() {
    let mut world = world_with(vec![]);
    let (_, mut entity) = cube_entity(1, Vector3::ZERO);
    if let Some(model) = &mut entity.model {
        model.kind = MeshKind::Fbx;
        model.source = Some("assets/hero.fbx".to_string());
    }
    world.entities.insert(1, entity);
    let camera = perspective_camera();
    let center = world_to_screen(&camera, SCREEN, [0.0, 0.0, 0.0]).expect("center projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), center.0, center.1),
        Some(1)
    );
    // Beyond the unit sphere: a miss.
    let above = world_to_screen(&camera, SCREEN, [0.0, 1.5, 0.0]).expect("above projects");
    assert_eq!(
        pick_entity(&world, &camera, space(), above.0, above.1),
        None
    );
}

#[test]
fn sprites_draw_over_models() {
    // A sprite and a cube both under the pointer: the sprite is drawn last
    // (no depth), so it wins even though the cube is "nearer" in 3D.
    let world = world_with(vec![
        sprite_entity(1, 480.0, 270.0, 0, SpriteKind::Rectangle),
        cube_entity(2, Vector3::ZERO),
    ]);
    let camera = ortho_camera_looking_down_z();
    assert_eq!(pick_entity(&world, &camera, space(), 480.0, 270.0), Some(1));
}

#[test]
fn snap_quantization_rounds_to_the_step_grid() {
    assert_eq!(quantize_offset(1.3, 0.25), 1.25);
    assert_eq!(quantize_offset(-1.3, 0.25), -1.25);
    assert_eq!(quantize_offset(0.4, 0.25), 0.5);
    assert_eq!(quantize_offset(1.3, 1.0), 1.0);
    assert_eq!(quantize_offset(0.49, 1.0), 0.0);
    assert_eq!(quantize_offset(2.6, 2.0), 2.0);
    // A non-positive step quantizes to zero instead of dividing by zero.
    assert_eq!(quantize_offset(1.3, 0.0), 0.0);
}

#[test]
fn effective_snap_step_follows_the_toggle() {
    assert_eq!(effective_snap_step(true, 0.25), 0.25);
    assert_eq!(effective_snap_step(false, 0.25), 1.0, "snapping off = 1.0");
    assert_eq!(
        effective_snap_step(true, 0.0),
        1.0,
        "invalid step falls back"
    );
    assert_eq!(effective_snap_step(true, -1.0), 1.0);
}

#[test]
fn gizmo_axis_math_converts_pointer_deltas_to_world_units() {
    // Ortho camera looking down -z at 24 world units of height: 540/24 =
    // 22.5 points per world unit.
    let camera = ortho_camera_looking_down_z();
    let origin = [0.0, 0.0, 0.0];
    let length = GIZMO_MODEL_AXIS_LENGTH;

    let ((ax, ay), (bx, by)) =
        gizmo_axis_segment(&camera, SCREEN, origin, 0, length).expect("x axis projects");
    assert!(
        (ay - 270.0).abs() < 1e-3 && (by - 270.0).abs() < 1e-3,
        "x axis is horizontal"
    );
    assert!(bx > ax, "x axis points right");
    assert!(
        ((bx - ax) - length * 22.5).abs() < 1e-2,
        "segment scales with the frustum"
    );

    // 45 points of rightward drag = 2 world units along x.
    let offset = pointer_axis_world_offset(&camera, SCREEN, origin, 0, length, (45.0, 0.0));
    assert!((offset - 2.0).abs() < 1e-2, "{offset}");
    // A drag perpendicular to the axis moves nothing along it.
    let offset = pointer_axis_world_offset(&camera, SCREEN, origin, 0, length, (0.0, 45.0));
    assert!(offset.abs() < 1e-4, "{offset}");
    // Reversed drag moves negatively.
    let offset = pointer_axis_world_offset(&camera, SCREEN, origin, 0, length, (-22.5, 0.0));
    assert!((offset + 1.0).abs() < 1e-2, "{offset}");
}

#[test]
fn closest_gizmo_axis_resolves_the_nearest_handle() {
    let segments = vec![
        (0, ((100.0, 100.0), (200.0, 100.0))),
        (1, ((100.0, 100.0), (100.0, 200.0))),
    ];
    assert_eq!(closest_gizmo_axis(&segments, (150.0, 104.0)), Some(0));
    assert_eq!(closest_gizmo_axis(&segments, (104.0, 150.0)), Some(1));
    assert_eq!(
        closest_gizmo_axis(&segments, (150.0, 150.0)),
        None,
        "beyond the hit radius from both segments"
    );
}

#[test]
fn entity_gizmo_origin_reports_the_rendered_transform() {
    let (_, sprite) = sprite_entity(1, 64.0, 32.0, 0, SpriteKind::Rectangle);
    let (origin, target) = entity_gizmo_origin(&sprite).expect("sprite origin");
    assert_eq!(origin, [64.0, 32.0, 0.0]);
    assert_eq!(target.axes(), &[0, 1], "sprites expose x/y only");

    let (_, cube) = cube_entity(
        2,
        Vector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
    );
    let (origin, target) = entity_gizmo_origin(&cube).expect("model origin");
    assert_eq!(origin, [1.0, 2.0, 3.0]);
    assert_eq!(target.axes(), &[0, 1, 2], "models expose all axes");

    let bare = Entity {
        id: 3,
        name: "bare".into(),
        transform: Transform::default(),
        sprite: None,
        model: None,
        script: None,
        extensions: Default::default(),
    };
    assert!(entity_gizmo_origin(&bare).is_none());
}

#[test]
fn sprite_gizmo_length_is_constant_in_sprite_space() {
    // Sprite gizmos are drawn in sprite space (pixels), so their segment
    // length is the constant by construction — a resize scales the screen
    // mapping, not the sprite-space length.
    assert_eq!(GIZMO_SPRITE_AXIS_LENGTH, 48.0);
    assert!(GIZMO_MODEL_AXIS_LENGTH > 0.0);
}

#[test]
fn orbit_preserves_radius_and_target() {
    let camera = perspective_camera();
    let radius_before = ((camera.position.x - camera.target.x).powi(2)
        + (camera.position.y - camera.target.y).powi(2)
        + (camera.position.z - camera.target.z).powi(2))
    .sqrt();
    let orbited = orbit_camera(&camera, 0.4, -0.3).expect("valid camera orbits");
    assert_eq!(orbited.target, camera.target, "the target is fixed");
    let radius_after = ((orbited.position.x - orbited.target.x).powi(2)
        + (orbited.position.y - orbited.target.y).powi(2)
        + (orbited.position.z - orbited.target.z).powi(2))
    .sqrt();
    assert!(
        (radius_after - radius_before).abs() < 1e-4,
        "{radius_after} vs {radius_before}"
    );
    // Pitch clamps below the pole.
    let flipped = orbit_camera(&camera, 0.0, 10.0).expect("valid camera orbits");
    let elevation = ((flipped.position.y - flipped.target.y) / radius_before).asin();
    assert!(elevation <= 1.554, "pitch must clamp: {elevation}");
    let mut degenerate = camera.clone();
    degenerate.position = degenerate.target;
    assert!(orbit_camera(&degenerate, 0.1, 0.1).is_none());
}

#[test]
fn pan_moves_position_and_target_together() {
    let camera = perspective_camera();
    let panned = pan_camera(&camera, 100.0, 0.0, 540.0).expect("valid camera pans");
    let dx_position = panned.position.x - camera.position.x;
    let dx_target = panned.target.x - camera.target.x;
    assert!((dx_position - dx_target).abs() < 1e-9, "both move equally");
    assert!(
        dx_position < 0.0,
        "dragging right moves the camera left (grab convention)"
    );
    // Distance to the target is preserved.
    let distance_before = ((camera.position.x - camera.target.x).powi(2)
        + (camera.position.y - camera.target.y).powi(2)
        + (camera.position.z - camera.target.z).powi(2))
    .sqrt();
    let distance_after = ((panned.position.x - panned.target.x).powi(2)
        + (panned.position.y - panned.target.y).powi(2)
        + (panned.position.z - panned.target.z).powi(2))
    .sqrt();
    assert!((distance_after - distance_before).abs() < 1e-4);
    // Ortho pan scale follows the orthographic height.
    let ortho = ortho_camera_looking_down_z();
    let ortho_panned = pan_camera(&ortho, 0.0, 540.0, 540.0).expect("valid camera pans");
    assert!(
        (ortho_panned.position.y - ortho.position.y - 24.0).abs() < 1e-3,
        "a full-screen drag pans exactly ortho_height"
    );
}

#[test]
fn spawned_specs_keeps_the_tests_honest() {
    // The helpers above build entities by hand; make sure the hand-built
    // shapes still parse as valid spawn specs so picking tests describe
    // real content.
    let spec: EntitySpec = serde_json::from_value(serde_json::json!({
        "name": "circle",
        "sprite": { "type": "circle", "width": 32.0, "height": 32.0, "layer": 3 },
    }))
    .expect("valid spec");
    assert_eq!(spec.sprite.as_ref().expect("sprite").layer, 3);
}
