struct Camera {
    view_projection: mat4x4<f32>,
    position: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

struct Model {
    transform: mat4x4<f32>,
    normal_transform: mat4x4<f32>,
    material: vec4<f32>,
}

@group(1) @binding(0)
var<uniform> model: Model;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.view_projection * model.transform * vec4<f32>(position, 1.0);
}
