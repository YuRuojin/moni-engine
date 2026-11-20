use std::path::Path;

use anyhow::{Context, bail};
use ufbx::{CoordinateAxes, GeometryTransformHandling, LoadOpts, MaterialMap, SpaceConversion};

#[derive(Debug, Clone, Copy)]
pub struct FbxVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: Option<[f32; 4]>,
}

#[derive(Debug)]
pub struct FbxMesh {
    pub vertices: Vec<FbxVertex>,
}

pub fn load_fbx(path: &Path) -> anyhow::Result<FbxMesh> {
    let filename = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("FBX path is not valid UTF-8: {}", path.display()))?;
    let scene = ufbx::load_file(
        filename,
        LoadOpts {
            ignore_animation: true,
            ignore_embedded: true,
            ignore_missing_external_files: true,
            generate_missing_normals: true,
            use_blender_pbr_material: true,
            space_conversion: SpaceConversion::ModifyGeometry,
            geometry_transform_handling: GeometryTransformHandling::ModifyGeometry,
            target_axes: CoordinateAxes::right_handed_y_up(),
            target_unit_meters: 1.0,
            normalize_normals: true,
            ..LoadOpts::default()
        },
    )
    .map_err(|error| anyhow::anyhow!("{error:?}"))
    .with_context(|| format!("could not parse FBX {}", path.display()))?;

    let mut vertices = Vec::new();
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    let mut triangle_indices = Vec::new();
    for node in &scene.nodes {
        if !node.visible {
            continue;
        }
        let Some(mesh) = &node.mesh else {
            continue;
        };
        for (face_index, face) in mesh.faces.iter().copied().enumerate() {
            if face.num_indices < 3 || mesh.face_hole.get(face_index).copied().unwrap_or(false) {
                continue;
            }
            ufbx::triangulate_face_vec(&mut triangle_indices, mesh, face);
            let color = material_color(mesh, face_index);
            for &index in &triangle_indices {
                let index = index as usize;
                let position =
                    ufbx::transform_position(&node.geometry_to_world, mesh.vertex_position[index]);
                let normal = if mesh.vertex_normal.exists {
                    let normal = ufbx::transform_direction(
                        &node.geometry_to_world,
                        mesh.vertex_normal[index],
                    );
                    normalize([normal.x as f32, normal.y as f32, normal.z as f32])
                } else {
                    [0.0, 0.0, 0.0]
                };
                let position = [position.x, position.y, position.z];
                if position.iter().any(|value| !value.is_finite()) {
                    bail!("FBX {} contains a non-finite vertex", path.display());
                }
                for axis in 0..3 {
                    minimum[axis] = minimum[axis].min(position[axis]);
                    maximum[axis] = maximum[axis].max(position[axis]);
                }
                vertices.push(FbxVertex {
                    position: position.map(|value| value as f32),
                    normal,
                    color,
                });
            }
        }
    }
    if vertices.is_empty() {
        bail!("FBX {} contains no visible triangles", path.display());
    }

    let center = [
        (minimum[0] + maximum[0]) / 2.0,
        (minimum[1] + maximum[1]) / 2.0,
        (minimum[2] + maximum[2]) / 2.0,
    ];
    let extent = (maximum[0] - minimum[0])
        .max(maximum[1] - minimum[1])
        .max(maximum[2] - minimum[2]);
    if !extent.is_finite() || extent <= 0.0 {
        bail!("FBX {} has no finite spatial extent", path.display());
    }
    let scale = 2.0 / extent;
    for vertex in &mut vertices {
        let source = [
            f64::from(vertex.position[0]),
            f64::from(vertex.position[1]),
            f64::from(vertex.position[2]),
        ];
        vertex.position = [
            ((source[0] - center[0]) * scale) as f32,
            ((source[1] - center[1]) * scale) as f32,
            ((source[2] - center[2]) * scale) as f32,
        ];
    }
    for triangle in vertices.chunks_exact_mut(3) {
        if triangle
            .iter()
            .all(|vertex| dot(vertex.normal, vertex.normal) > 0.000001)
        {
            continue;
        }
        let first = subtract(triangle[1].position, triangle[0].position);
        let second = subtract(triangle[2].position, triangle[0].position);
        let normal = normalize(cross(first, second));
        for vertex in triangle {
            if dot(vertex.normal, vertex.normal) <= 0.000001 {
                vertex.normal = normal;
            }
        }
    }

    Ok(FbxMesh { vertices })
}

fn material_color(mesh: &ufbx::Mesh, face_index: usize) -> Option<[f32; 4]> {
    let index = (*mesh.face_material.get(face_index)?) as usize;
    let material = mesh.materials.get(index)?;
    color_from_map(&material.fbx.diffuse_color).or_else(|| color_from_map(&material.pbr.base_color))
}

fn color_from_map(map: &MaterialMap) -> Option<[f32; 4]> {
    if !map.has_value {
        return None;
    }
    let color = map.value_vec4;
    Some([
        color.x.clamp(0.0, 1.0) as f32,
        color.y.clamp(0.0, 1.0) as f32,
        color.z.clamp(0.0, 1.0) as f32,
        color.w.clamp(0.0, 1.0) as f32,
    ])
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
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

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length <= f32::EPSILON {
        [0.0, 0.0, 0.0]
    } else {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    }
}
