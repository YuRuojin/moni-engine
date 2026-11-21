use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use moni_core::engine::World;
use moni_core::host::OverlayQuad;
use moni_core::scene3d::MeshKind;

const TEXT_COLOR: [f32; 4] = [0.85, 0.9, 1.0, 1.0];
const MUTED_COLOR: [f32; 4] = [0.45, 0.6, 0.8, 1.0];

/// Rebuild gate for the viewer overlay: the quad list is rebuilt only when the
/// signature of the overlay inputs changes. When `quads` returns `None` the
/// renderer keeps the previously installed quads, so per-character quad
/// construction and string formatting are skipped on unchanged frames.
#[derive(Default)]
pub struct OverlayCache {
    signature: Option<OverlaySignature>,
}

impl OverlayCache {
    /// Returns freshly built quads when the overlay inputs changed since the
    /// previous build, or `None` when the previously installed quads remain
    /// valid.
    pub fn quads(
        &mut self,
        world: &World,
        scene_path: &str,
        preparation_ms: f64,
        frame_ms: f64,
        gpu_samples: &[(f32, f32)],
        loaded_vertices: usize,
    ) -> Option<Vec<OverlayQuad>> {
        let signature = OverlaySignature::capture(
            world,
            scene_path,
            preparation_ms,
            frame_ms,
            gpu_samples,
            loaded_vertices,
        );
        if self.signature.as_ref() == Some(&signature) {
            return None;
        }
        self.signature = Some(signature);
        Some(build_overlay(
            world,
            scene_path,
            preparation_ms,
            frame_ms,
            gpu_samples,
            loaded_vertices,
        ))
    }
}

/// Inputs that determine the overlay contents. Frame timings are quantized to
/// 0.1 ms so jitter between frames does not force quad rebuilds; entity
/// content is summarized as a hash over the fields the overlay displays.
#[derive(Debug, PartialEq)]
struct OverlaySignature {
    viewport: (u32, u32),
    scene_path: String,
    entities_hash: u64,
    light_count: usize,
    preparation_decis_ms: i64,
    frame_decis_ms: i64,
    gpu_main_decis_ms: i64,
    gpu_shadow_decis_ms: i64,
    loaded_vertices: usize,
}

impl OverlaySignature {
    fn capture(
        world: &World,
        scene_path: &str,
        preparation_ms: f64,
        frame_ms: f64,
        gpu_samples: &[(f32, f32)],
        loaded_vertices: usize,
    ) -> Self {
        let gpu = gpu_samples.last().copied().unwrap_or((0.0, 0.0));
        Self {
            viewport: (world.viewport.width, world.viewport.height),
            scene_path: scene_path.to_string(),
            entities_hash: entities_summary_hash(world),
            light_count: world.lights.len(),
            preparation_decis_ms: quantize_ms(preparation_ms),
            frame_decis_ms: quantize_ms(frame_ms),
            gpu_main_decis_ms: quantize_ms(f64::from(gpu.1)),
            gpu_shadow_decis_ms: quantize_ms(f64::from(gpu.0)),
            loaded_vertices,
        }
    }
}

/// Hashes the per-entity fields the overlay displays: id, name, model kind,
/// metallic, roughness, source file name, and script presence. Sprites and
/// transforms are not displayed and stay out of the summary.
fn entities_summary_hash(world: &World) -> u64 {
    let mut hasher = DefaultHasher::new();
    for entity in world.entities.values() {
        entity.id.hash(&mut hasher);
        entity.name.hash(&mut hasher);
        if let Some(model) = &entity.model {
            model_kind_code(model.kind).hash(&mut hasher);
            model.metallic.to_bits().hash(&mut hasher);
            model.roughness.to_bits().hash(&mut hasher);
            file_name(model.source.as_deref().unwrap_or("")).hash(&mut hasher);
        }
        entity.script.is_some().hash(&mut hasher);
    }
    hasher.finish()
}

fn model_kind_code(kind: MeshKind) -> u8 {
    match kind {
        MeshKind::Cube => 0,
        MeshKind::Plane => 1,
        MeshKind::Fbx => 2,
    }
}

/// Quantizes a millisecond timing to 0.1 ms buckets; the same quantization
/// feeds the displayed values so the text cannot drift from the signature.
fn quantize_ms(value_ms: f64) -> i64 {
    (value_ms * 10.0).round() as i64
}

/// Renders a millisecond timing at the 0.1 ms granularity used by the overlay
/// signature.
fn display_ms(value_ms: f64) -> f64 {
    quantize_ms(value_ms) as f64 / 10.0
}

/// Builds the full overlay quad list. Timings are displayed at the 0.1 ms
/// granularity used by [`OverlaySignature`]; callers that rebuild every frame
/// should prefer [`OverlayCache::quads`].
pub fn build_overlay(
    world: &World,
    scene_path: &str,
    preparation_ms: f64,
    frame_ms: f64,
    gpu_samples: &[(f32, f32)],
    loaded_vertices: usize,
) -> Vec<OverlayQuad> {
    let mut quads = Vec::new();
    let width = f64::from(world.viewport.width);
    let height = f64::from(world.viewport.height);
    rect(
        &mut quads,
        8.0,
        8.0,
        340.0,
        height - 16.0,
        [0.015, 0.025, 0.045, 0.88],
    );
    rect(
        &mut quads,
        width - 280.0,
        8.0,
        272.0,
        144.0,
        [0.015, 0.025, 0.045, 0.88],
    );

    let mut left = Vec::new();
    left.push("MONI SCENE VIEWER".to_string());
    left.push(format!("SCENE {scene_path}"));
    left.push(format!(
        "ENTITIES {} LIGHTS {}",
        world.entities.len(),
        world.lights.len()
    ));
    left.push("HIERARCHY".to_string());
    for entity in world.entities.values() {
        left.push(format!("{} {}", entity.id, entity.name));
        if let Some(model) = &entity.model {
            left.push(format!(
                "  {:?} M {:.2} R {:.2}",
                model.kind, model.metallic, model.roughness
            ));
            if let Some(source) = &model.source {
                left.push(format!("  {}", file_name(source)));
            }
        }
        if entity.script.is_some() {
            left.push("  SCRIPT ATTACHED".to_string());
        }
    }
    left.push("READ ONLY - NO SCRIPT TICK".to_string());
    left.push("ARROWS MOVE CAMERA".to_string());
    left.push("PGUP/PGDN ZOOM HOME RESET".to_string());
    for (index, line) in left.iter().enumerate() {
        text(
            &mut quads,
            18.0,
            18.0 + index as f64 * 18.0,
            line,
            TEXT_COLOR,
            2,
        );
    }

    let gpu = gpu_samples.last().copied().unwrap_or((0.0, 0.0));
    let right = [
        "PERFORMANCE".to_string(),
        format!("CPU PREP {:.1} MS", display_ms(preparation_ms)),
        format!("CPU FRAME {:.1} MS", display_ms(frame_ms)),
        format!("GPU MAIN {:.1} MS", display_ms(f64::from(gpu.1))),
        format!("GPU SHADOW {:.1} MS", display_ms(f64::from(gpu.0))),
        format!("FBX VERTICES {loaded_vertices}"),
        "GPU SAMPLES EVERY 30 FRAMES".to_string(),
    ];
    for (index, line) in right.iter().enumerate() {
        text(
            &mut quads,
            width - 270.0,
            18.0 + index as f64 * 18.0,
            line,
            MUTED_COLOR,
            2,
        );
    }
    quads
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn rect(quads: &mut Vec<OverlayQuad>, x: f64, y: f64, width: f64, height: f64, color: [f32; 4]) {
    quads.push(OverlayQuad {
        x: x + width / 2.0,
        y: y + height / 2.0,
        width,
        height,
        color,
    });
}

fn text(quads: &mut Vec<OverlayQuad>, x: f64, y: f64, value: &str, color: [f32; 4], scale: u32) {
    let uppercase = value.to_uppercase();
    for (character_index, character) in uppercase.chars().enumerate() {
        let glyph = glyph(character);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    let pixel_x = x + (character_index * 6 + column) as f64 * f64::from(scale);
                    let pixel_y = y + row as f64 * f64::from(scale);
                    rect(
                        quads,
                        pixel_x,
                        pixel_y,
                        f64::from(scale),
                        f64::from(scale),
                        color,
                    );
                }
            }
        }
    }
}

fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 17, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '/' => [1, 2, 2, 4, 8, 16, 16],
        '\\' => [16, 8, 8, 4, 2, 1, 1],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '%' => [17, 17, 2, 4, 8, 17, 17],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        _ => [0, 0, 0, 0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moni_core::engine::Entity;

    fn named_world(name: &str) -> World {
        let mut world = World::default();
        world.entities.insert(
            1,
            Entity {
                id: 1,
                name: name.to_string(),
                transform: Default::default(),
                sprite: None,
                model: None,
                script: None,
                extensions: Default::default(),
            },
        );
        world
    }

    #[test]
    fn quantize_ms_buckets_timings_at_tenth_millisecond() {
        assert_eq!(quantize_ms(0.0), 0);
        assert_eq!(quantize_ms(12.34), 123);
        assert_eq!(quantize_ms(12.399), 124);
        assert_eq!(display_ms(12.34), 12.3);
    }

    #[test]
    fn overlay_cache_rebuilds_only_on_signature_change() {
        let mut world = named_world("cube");
        let mut cache = OverlayCache::default();
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.0, 2.0, &[], 100)
                .is_some()
        );

        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.0, 2.0, &[], 100)
                .is_none(),
            "unchanged inputs keep the built quads"
        );

        // Sub-0.1 ms timing jitter does not force a rebuild.
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.04, 2.04, &[], 100)
                .is_none()
        );

        // Crossing a 0.1 ms bucket rebuilds.
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.11, 2.0, &[], 100)
                .is_some()
        );

        // Entity content changes rebuild.
        world.entities.get_mut(&1).unwrap().name = "renamed".to_string();
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.11, 2.0, &[], 100)
                .is_some()
        );

        // A new GPU sample rebuilds once, then stays cached.
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.11, 2.0, &[(3.25, 4.5)], 100)
                .is_some()
        );
        assert!(
            cache
                .quads(&world, "scene.mnsc", 1.11, 2.0, &[(3.25, 4.5)], 100)
                .is_none()
        );
    }
}
