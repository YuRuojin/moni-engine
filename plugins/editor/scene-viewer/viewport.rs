//! Pure-math viewport helpers for the editor scene viewer: camera
//! projection, pointer picking, gizmo axis math, camera mouse operations,
//! and snap quantization. No rendering, no GUI, no state.
//!
//! The matrix conventions mirror the wgpu renderer's `view_projection`
//! (column-major `[[f32; 4]; 4]` storage, the same look-at basis with its
//! degenerate-up fallback, and the same projection coefficients), so a
//! projected point lands on the pixel the renderer actually draws it to —
//! without depending on the render crate.
//!
//! One deliberate asymmetry: 3D models live in world space under the
//! camera, but the renderer draws 2D sprites in a pixel-space overlay
//! (`shader.wgsl` maps sprite vertices by `world.viewport`, no camera), so
//! sprite picking and the sprite gizmo work in that pixel space instead of
//! intersecting the world `z = 0` plane. Mirroring what the renderer
//! actually draws keeps selection aligned with the picture.
//!
//! Everything here is deterministic f32/f64 math with no platform or
//! backend dependency.

use moni_core::engine::{Entity, EntityId, Sprite, SpriteKind, Transform, World};
use moni_core::scene3d::{Camera3d, CameraProjection, MeshKind, Model3d};

/// Screen distance (egui points) at which the pointer counts as grabbing a
/// gizmo axis handle.
pub const GIZMO_HIT_RADIUS_PX: f32 = 14.0;

/// World-space length of the move-gizmo axis lines drawn for 3D models.
pub const GIZMO_MODEL_AXIS_LENGTH: f32 = 1.5;

/// Sprite-space length of the move-gizmo axis lines drawn for 2D sprites.
/// Sprite space is the renderer's pixel space, so the gizmo keeps a
/// constant on-screen length by construction.
pub const GIZMO_SPRITE_AXIS_LENGTH: f32 = 48.0;

/// Orbit sensitivity: radians of camera yaw/pitch per egui point of drag.
pub const ORBIT_RADIANS_PER_POINT: f32 = 0.01;

/// Smallest pitch magnitude (radians) orbiting allows before clamping;
/// keeps the look-at basis away from its degenerate straight-down form.
const ORBIT_PITCH_LIMIT: f32 = 1.553_343; // just under 89 degrees

/// The two screen spaces pointer math has to bridge: `screen_size` is the
/// egui window size in points (where pointer events are measured) and
/// `sprite_size` is the renderer's sprite viewport (`world.viewport`), the
/// pixel space sprites are drawn in. The two coincide at the editor's
/// default 960x540 surface and diverge once the window is resized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportSpace {
    pub screen_size: (f32, f32),
    pub sprite_size: (f32, f32),
}

impl ViewportSpace {
    /// Maps an egui point to sprite space (x right, y down, sized by
    /// `world.viewport`).
    pub fn sprite_point(&self, px: f32, py: f32) -> (f32, f32) {
        let (sw, sh) = self.screen_size;
        let (vw, vh) = self.sprite_size;
        if sw <= 0.0 || sh <= 0.0 {
            return (0.0, 0.0);
        }
        (px * vw / sw, py * vh / sh)
    }

    /// Maps an egui point delta to a sprite-space delta.
    pub fn sprite_delta(&self, dx: f32, dy: f32) -> (f32, f32) {
        self.sprite_point(dx, dy)
    }

    /// Maps a sprite-space position back to an egui point (the gizmo
    /// origin of a sprite entity).
    pub fn screen_point(&self, sx: f32, sy: f32) -> (f32, f32) {
        let (sw, sh) = self.screen_size;
        let (vw, vh) = self.sprite_size;
        if vw <= 0.0 || vh <= 0.0 {
            return (0.0, 0.0);
        }
        (sx * sw / vw, sy * sh / vh)
    }
}

/// A world-space ray from the camera through a viewport pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: [f32; 3],
    pub dir: [f32; 3],
}

impl Ray {
    /// The point at parameter `t` along the ray.
    pub fn at(&self, t: f32) -> [f32; 3] {
        [
            self.origin[0] + self.dir[0] * t,
            self.origin[1] + self.dir[1] * t,
            self.origin[2] + self.dir[2] * t,
        ]
    }
}

/// Projects a world point to egui screen points (origin top-left).
/// Returns `None` when the point lies on or behind the camera plane, where
/// the perspective projection has no meaningful pixel.
pub fn world_to_screen(
    camera: &Camera3d,
    screen_size: (f32, f32),
    point: [f32; 3],
) -> Option<(f32, f32)> {
    let (width, height) = screen_size;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let vp = view_projection(camera, width / height);
    let clip = transform_point4(&vp, [point[0], point[1], point[2], 1.0]);
    if clip[3] <= 0.0 {
        return None;
    }
    let ndc_x = clip[0] / clip[3];
    let ndc_y = clip[1] / clip[3];
    Some(((ndc_x * 0.5 + 0.5) * width, (0.5 - ndc_y * 0.5) * height))
}

/// The world-space ray through an egui point: origin at the camera eye for
/// perspective, on the near-plane rectangle for orthographic.
pub fn screen_ray(camera: &Camera3d, screen_size: (f32, f32), px: f32, py: f32) -> Ray {
    let (width, height) = screen_size;
    let (right, up, forward) = camera_basis(camera);
    let ndc_x = if width > 0.0 {
        px / width * 2.0 - 1.0
    } else {
        0.0
    };
    let ndc_y = if height > 0.0 {
        1.0 - py / height * 2.0
    } else {
        0.0
    };
    match camera.projection {
        CameraProjection::Perspective => {
            let tan_half = (camera.fov_y as f32 / 2.0).tan();
            let dir = normalize(add(
                add(
                    scale(right, ndc_x * tan_half * (width / height)),
                    scale(up, ndc_y * tan_half),
                ),
                forward,
            ));
            Ray {
                origin: vec3_f32(camera.position),
                dir,
            }
        }
        CameraProjection::Orthographic => {
            let half_height = camera.orthographic_height as f32 / 2.0;
            let half_width = half_height * (width / height);
            let origin = add(
                vec3_f32(camera.position),
                add(
                    scale(right, ndc_x * half_width),
                    scale(up, ndc_y * half_height),
                ),
            );
            Ray {
                origin,
                dir: forward,
            }
        }
    }
}

/// Picks the entity under an egui point, mirroring the renderer's draw
/// order: sprites are drawn last (no depth), so the topmost sprite hit —
/// the largest `(layer, id)` exactly like the renderer's ascending
/// `(layer, id)` sprite order — wins over any model; among models the
/// nearest ray hit wins. Miss on everything returns `None`.
pub fn pick_entity(
    world: &World,
    camera: &Camera3d,
    space: ViewportSpace,
    px: f32,
    py: f32,
) -> Option<EntityId> {
    let sprite_hit = pick_sprite(world, space, px, py);
    if let Some(entity) = sprite_hit {
        return Some(entity);
    }
    pick_model(world, camera, space.screen_size, px, py)
}

/// Topmost sprite hit in the renderer's sprite space, honoring the
/// `(layer, id)` draw order: entities iterate in ascending id, and a hit
/// replaces the previous best only when its `(layer, id)` sorts higher.
fn pick_sprite(world: &World, space: ViewportSpace, px: f32, py: f32) -> Option<EntityId> {
    let (sx, sy) = space.sprite_point(px, py);
    let mut best: Option<(i32, EntityId)> = None;
    for entity in world.entities.values() {
        let Some(sprite) = &entity.sprite else {
            continue;
        };
        if sprite_space_hit(sprite, &entity.transform, sx, sy) {
            let key = (sprite.layer, entity.id);
            if best.is_none_or(|previous| key > previous) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, id)| id)
}

/// Nearest 3D model hit by ray, with the picking approximations documented
/// on [`model_hit`]: axis-aligned boxes around cube/plane transforms
/// (rotation ignored) and a unit sphere for fbx sources of unknown bounds.
fn pick_model(
    world: &World,
    camera: &Camera3d,
    screen_size: (f32, f32),
    px: f32,
    py: f32,
) -> Option<EntityId> {
    let ray = screen_ray(camera, screen_size, px, py);
    let mut best: Option<(f32, EntityId)> = None;
    for entity in world.entities.values() {
        let Some(model) = &entity.model else {
            continue;
        };
        let Some(t) = model_hit(model, &ray) else {
            continue;
        };
        if best.is_none_or(|(previous_t, _)| t < previous_t) {
            best = Some((t, entity.id));
        }
    }
    best.map(|(_, id)| id)
}

/// Ray hit test for one model. Cubes are axis-aligned boxes of half extents
/// `0.5 * |scale|` (rotation ignored — an AABB approximation of the
/// rotated mesh); planes the same with zero thickness on y; fbx bounds are
/// unknown at edit time, so the pick uses a unit sphere at the transform
/// position (documented approximation). Returns the entry parameter `t`.
fn model_hit(model: &Model3d, ray: &Ray) -> Option<f32> {
    let position = vec3_f32(model.transform.position);
    match model.kind {
        MeshKind::Cube => {
            let half = [
                (0.5 * model.transform.scale.x.abs()) as f32,
                (0.5 * model.transform.scale.y.abs()) as f32,
                (0.5 * model.transform.scale.z.abs()) as f32,
            ];
            ray_aabb(ray, position, half)
        }
        MeshKind::Plane => {
            let half = [
                (0.5 * model.transform.scale.x.abs()) as f32,
                0.0,
                (0.5 * model.transform.scale.z.abs()) as f32,
            ];
            ray_aabb(ray, position, half)
        }
        MeshKind::Fbx => ray_sphere(ray, position, 1.0),
    }
}

/// Slab ray-vs-AABB: returns the entry parameter `t` (>= 0) when the ray
/// reaches the box, `None` otherwise. Degenerate (zero-thickness) slabs
/// fall out naturally: both slab boundaries coincide.
fn ray_aabb(ray: &Ray, center: [f32; 3], half: [f32; 3]) -> Option<f32> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    for axis in 0..3 {
        let extent = half[axis];
        let delta = ray.dir[axis];
        let offset = center[axis] - ray.origin[axis];
        if delta.abs() < 1e-9 {
            if offset.abs() > extent {
                return None;
            }
            continue;
        }
        let mut t1 = (offset - extent) / delta;
        let mut t2 = (offset + extent) / delta;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return None;
        }
    }
    let entry = t_min.max(0.0);
    (entry <= t_max).then_some(entry)
}

/// Nearest positive ray-vs-sphere hit parameter, or `None` on a miss.
fn ray_sphere(ray: &Ray, center: [f32; 3], radius: f32) -> Option<f32> {
    let oc = subtract(ray.origin, center);
    let b = dot(oc, ray.dir);
    let c = dot(oc, oc) - radius * radius;
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    let near = -b - root;
    let far = -b + root;
    let entry = if near >= 0.0 { near } else { far };
    (entry >= 0.0).then_some(entry)
}

/// Sprite rect/circle test in sprite space, mirroring the renderer's
/// rectangle and ellipse geometry. Rotation is ignored (an axis-aligned
/// approximation of the rotated quad).
fn sprite_space_hit(sprite: &Sprite, transform: &Transform, sx: f32, sy: f32) -> bool {
    let center_x = transform.x as f32;
    let center_y = transform.y as f32;
    let half_width = sprite.width as f32 / 2.0;
    let half_height = sprite.height as f32 / 2.0;
    match sprite.kind {
        SpriteKind::Rectangle => {
            (sx - center_x).abs() <= half_width && (sy - center_y).abs() <= half_height
        }
        SpriteKind::Circle => {
            let dx = (sx - center_x) / half_width.max(1e-6);
            let dy = (sy - center_y) / half_height.max(1e-6);
            dx * dx + dy * dy <= 1.0
        }
    }
}

/// The screen-space endpoints of one sprite-gizmo axis segment. Sprite
/// space maps onto the egui screen through [`ViewportSpace`], x right and
/// y down, matching the renderer's sprite overlay.
pub fn sprite_axis_segment(
    origin: (f32, f32),
    axis: usize,
    length: f32,
) -> ((f32, f32), (f32, f32)) {
    match axis {
        0 => (origin, (origin.0 + length, origin.1)),
        _ => (origin, (origin.0, origin.1 + length)),
    }
}

/// The screen-space endpoints of one model-gizmo axis segment: the
/// projection of the origin and of `origin + axis * length`. `None` when
/// either endpoint is behind the camera or the axis points at the camera
/// so hard that it projects to a point.
pub fn gizmo_axis_segment(
    camera: &Camera3d,
    screen_size: (f32, f32),
    origin: [f32; 3],
    axis: usize,
    length: f32,
) -> Option<((f32, f32), (f32, f32))> {
    let start = world_to_screen(camera, screen_size, origin)?;
    let tip = world_to_screen(camera, screen_size, axis_point(origin, axis, length))?;
    Some((start, tip))
}

/// The world-space movement along `axis` caused by a pointer delta: the
/// delta is projected onto the screen-space axis segment and scaled by
/// world-units-per-pixel measured from that segment's projected length.
/// Returns 0 when the segment cannot be projected or degenerates.
pub fn pointer_axis_world_offset(
    camera: &Camera3d,
    screen_size: (f32, f32),
    origin: [f32; 3],
    axis: usize,
    length: f32,
    pointer_delta: (f32, f32),
) -> f32 {
    let Some((start, tip)) = gizmo_axis_segment(camera, screen_size, origin, axis, length) else {
        return 0.0;
    };
    let screen_axis = [tip.0 - start.0, tip.1 - start.1];
    let length_px = (screen_axis[0] * screen_axis[0] + screen_axis[1] * screen_axis[1]).sqrt();
    if length_px < 1e-5 {
        return 0.0;
    }
    let along_px =
        (pointer_delta.0 * screen_axis[0] + pointer_delta.1 * screen_axis[1]) / length_px;
    along_px * (length / length_px)
}

/// Distance from `p` to the segment `a`-`b` in screen points.
pub fn segment_distance_px(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let ab = (b.0 - a.0, b.1 - a.1);
    let ap = (p.0 - a.0, p.1 - a.1);
    let length_sq = ab.0 * ab.0 + ab.1 * ab.1;
    let t = if length_sq < 1e-9 {
        0.0
    } else {
        ((ap.0 * ab.0 + ap.1 * ab.1) / length_sq).clamp(0.0, 1.0)
    };
    let dx = p.0 - (a.0 + ab.0 * t);
    let dy = p.1 - (a.1 + ab.1 * t);
    (dx * dx + dy * dy).sqrt()
}

/// Closest gizmo axis to the pointer among the given screen segments, or
/// `None` when every segment is farther than [`GIZMO_HIT_RADIUS_PX`].
pub fn closest_gizmo_axis(
    segments: &[(usize, ((f32, f32), (f32, f32)))],
    p: (f32, f32),
) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for &(axis, (a, b)) in segments {
        let distance = segment_distance_px(a, b, p);
        if distance <= GIZMO_HIT_RADIUS_PX && best.is_none_or(|(best_d, _)| distance < best_d) {
            best = Some((distance, axis));
        }
    }
    best.map(|(_, axis)| axis)
}

/// Effective quantization step for gizmo movement: the snap step while
/// snapping is on, whole units (1.0) when it is off, and also 1.0 when the
/// configured step is not positive (a zero step would quantize everything
/// to zero).
pub fn effective_snap_step(enabled: bool, step: f64) -> f64 {
    if enabled && step.is_finite() && step > 0.0 {
        step
    } else {
        1.0
    }
}

/// Quantizes an accumulated drag offset to the step grid.
pub fn quantize_offset(offset: f32, step: f64) -> f32 {
    let step = step as f32;
    if step <= 0.0 {
        return 0.0;
    }
    (offset / step).round() * step
}

/// Orbits the camera around its target by yaw/pitch deltas (radians):
/// positive yaw rotates the camera position counterclockwise around the
/// world Y axis, positive pitch raises its elevation. The orbit radius is
/// preserved and pitch clamps just under the pole so the look-at basis
/// never degenerates. Returns `None` for a degenerate camera
/// (position == target).
pub fn orbit_camera(camera: &Camera3d, yaw_delta: f32, pitch_delta: f32) -> Option<Camera3d> {
    let offset = subtract(vec3_f32(camera.position), vec3_f32(camera.target));
    let radius = length(offset);
    if radius < 1e-6 {
        return None;
    }
    let yaw = offset[0].atan2(offset[2]) + yaw_delta;
    let pitch =
        ((offset[1] / radius).asin() + pitch_delta).clamp(-ORBIT_PITCH_LIMIT, ORBIT_PITCH_LIMIT);
    let new_offset = [
        radius * pitch.cos() * yaw.sin(),
        radius * pitch.sin(),
        radius * pitch.cos() * yaw.cos(),
    ];
    let mut moved = camera.clone();
    moved.position = moni_core::scene3d::Vector3 {
        x: (camera.target.x + new_offset[0] as f64),
        y: (camera.target.y + new_offset[1] as f64),
        z: (camera.target.z + new_offset[2] as f64),
    };
    Some(moved)
}

/// Pans the camera in its own plane by pointer deltas: the grab convention
/// (content follows the pointer). The world-units-per-point scale matches
/// what the frustum shows at the target distance for perspective and the
/// orthographic height for orthographic. Both position and target move
/// equally, so the look-at contract (position != target) is preserved.
pub fn pan_camera(camera: &Camera3d, dx: f32, dy: f32, screen_height: f32) -> Option<Camera3d> {
    if screen_height <= 0.0 {
        return None;
    }
    let (right, up, _forward) = camera_basis(camera);
    let world_per_px = match camera.projection {
        CameraProjection::Perspective => {
            let distance = length(subtract(vec3_f32(camera.target), vec3_f32(camera.position)));
            distance * 2.0 * (camera.fov_y as f32 / 2.0).tan() / screen_height
        }
        CameraProjection::Orthographic => camera.orthographic_height as f32 / screen_height,
    };
    let delta = add(
        scale(right, -dx * world_per_px),
        scale(up, dy * world_per_px),
    );
    let mut moved = camera.clone();
    moved.position = moni_core::scene3d::Vector3 {
        x: camera.position.x + delta[0] as f64,
        y: camera.position.y + delta[1] as f64,
        z: camera.position.z + delta[2] as f64,
    };
    moved.target = moni_core::scene3d::Vector3 {
        x: camera.target.x + delta[0] as f64,
        y: camera.target.y + delta[1] as f64,
        z: camera.target.z + delta[2] as f64,
    };
    Some(moved)
}

/// The gizmo origin of an entity: the sprite position on the z = 0 plane
/// for sprite entities, the 3D model position for model entities.
pub fn entity_gizmo_origin(entity: &Entity) -> Option<([f32; 3], GizmoTarget)> {
    if let Some(model) = &entity.model {
        return Some((
            [
                model.transform.position.x as f32,
                model.transform.position.y as f32,
                model.transform.position.z as f32,
            ],
            GizmoTarget::Model,
        ));
    }
    entity.sprite.as_ref().map(|_| {
        (
            [entity.transform.x as f32, entity.transform.y as f32, 0.0],
            GizmoTarget::Sprite,
        )
    })
}

/// Which transform a gizmo drag moves. Mirrors the renderer: sprites
/// follow the entity's 2D transform in pixel space, models follow their
/// own 3D transform, so the drag commit uses the command that actually
/// renders the entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GizmoTarget {
    Sprite,
    Model,
}

impl GizmoTarget {
    /// The axes this target exposes: sprites move on x/y only (the engine
    /// 2D transform has no z), models on all three.
    pub fn axes(self) -> &'static [usize] {
        match self {
            GizmoTarget::Sprite => &[0, 1],
            GizmoTarget::Model => &[0, 1, 2],
        }
    }
}

fn axis_point(origin: [f32; 3], axis: usize, length: f32) -> [f32; 3] {
    let mut point = origin;
    point[axis] += length;
    point
}

/// The camera basis (right, up, forward) with the renderer's exact
/// look-at convention, including its degenerate-up fallback.
fn camera_basis(camera: &Camera3d) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let eye = vec3_f32(camera.position);
    let target = vec3_f32(camera.target);
    let forward = normalize(subtract(target, eye));
    let mut right = cross(forward, [0.0, 1.0, 0.0]);
    if dot(right, right) < 0.000_001 {
        right = cross(forward, [0.0, 0.0, 1.0]);
    }
    let right = normalize(right);
    let up = cross(right, forward);
    (right, up, forward)
}

/// View-projection matrix with the renderer's exact conventions
/// (column-major `[[f32; 4]; 4]` storage; see `view_projection` in the
/// wgpu plugin for the source this mirrors).
fn view_projection(camera: &Camera3d, aspect: f32) -> [[f32; 4]; 4] {
    let eye = vec3_f32(camera.position);
    let target = vec3_f32(camera.target);
    let forward = normalize(subtract(target, eye));
    let mut right = cross(forward, [0.0, 1.0, 0.0]);
    if dot(right, right) < 0.000_001 {
        right = cross(forward, [0.0, 0.0, 1.0]);
    }
    let right = normalize(right);
    let up = cross(right, forward);
    let view = [
        [right[0], up[0], -forward[0], 0.0],
        [right[1], up[1], -forward[1], 0.0],
        [right[2], up[2], -forward[2], 0.0],
        [-dot(right, eye), -dot(up, eye), dot(forward, eye), 1.0],
    ];

    let mut projection = [[0.0; 4]; 4];
    match camera.projection {
        CameraProjection::Perspective => {
            let f = 1.0 / (camera.fov_y as f32 / 2.0).tan();
            projection[0][0] = f / aspect;
            projection[1][1] = f;
            projection[2][2] = (camera.far / (camera.near - camera.far)) as f32;
            projection[2][3] = -1.0;
            projection[3][2] = (camera.near * camera.far / (camera.near - camera.far)) as f32;
        }
        CameraProjection::Orthographic => {
            let half_height = camera.orthographic_height as f32 / 2.0;
            let half_width = half_height * aspect;
            projection[0][0] = 1.0 / half_width;
            projection[1][1] = 1.0 / half_height;
            projection[2][2] = 1.0 / (camera.near - camera.far) as f32;
            projection[3][2] = (camera.near / (camera.near - camera.far)) as f32;
            projection[3][3] = 1.0;
        }
    }
    multiply_matrices(projection, view)
}

/// Matrix product under the renderer's column-major storage convention.
fn multiply_matrices(left: [[f32; 4]; 4], right: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut output = [[0.0; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            output[column][row] = (0..4)
                .map(|index| left[index][row] * right[column][index])
                .sum();
        }
    }
    output
}

/// Applies a column-vector times a column-major matrix.
fn transform_point4(matrix: &[[f32; 4]; 4], point: [f32; 4]) -> [f32; 4] {
    let mut output = [0.0; 4];
    for component in 0..4 {
        output[component] = (0..4)
            .map(|index| matrix[index][component] * point[index])
            .sum();
    }
    output
}

fn vec3_f32(value: moni_core::scene3d::Vector3) -> [f32; 3] {
    [value.x as f32, value.y as f32, value.z as f32]
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(vector: [f32; 3], factor: f32) -> [f32; 3] {
    [vector[0] * factor, vector[1] * factor, vector[2] * factor]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length(vector: [f32; 3]) -> f32 {
    dot(vector, vector).sqrt()
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let len = length(vector);
    if len < 1e-9 {
        return [0.0, 0.0, 0.0];
    }
    scale(vector, 1.0 / len)
}

#[cfg(test)]
#[path = "viewport_tests.rs"]
mod viewport_tests;
