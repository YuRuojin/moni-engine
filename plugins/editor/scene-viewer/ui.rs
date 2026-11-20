use crate::engine::World;
use crate::gpu::OverlayQuad;

const TEXT_COLOR: [f32; 4] = [0.85, 0.9, 1.0, 1.0];
const MUTED_COLOR: [f32; 4] = [0.45, 0.6, 0.8, 1.0];

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
        format!("CPU PREP {preparation_ms:.3} MS"),
        format!("CPU FRAME {frame_ms:.3} MS"),
        format!("GPU MAIN {:.3} MS", gpu.1),
        format!("GPU SHADOW {:.3} MS", gpu.0),
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
