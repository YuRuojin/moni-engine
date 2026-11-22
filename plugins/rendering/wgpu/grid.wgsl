// Editor viewport ground grid: unlit per-vertex-color lines drawn over the
// same camera uniform as the 3D pass. Lighting and shadows are deliberately
// not applied so the lattice keeps a constant tool-surface color in any scene.

struct Camera {
    view_projection: mat4x4<f32>,
    position: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

// Matches the shared Vertex3d layout; `normal` is unused here.
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
