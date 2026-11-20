struct Camera {
    view_projection: mat4x4<f32>,
    position: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

struct Lighting {
    directional_direction: vec4<f32>,
    directional_color: vec4<f32>,
    point_count: vec4<u32>,
    point_lights: array<vec4<f32>, 16>,
    shadow_matrix: mat4x4<f32>,
    shadow_params: vec4<f32>,
}

@group(0) @binding(1)
var<uniform> lighting: Lighting;
@group(0) @binding(2)
var shadow_texture: texture_depth_2d;
@group(0) @binding(3)
var shadow_sampler: sampler_comparison;

struct Model {
    transform: mat4x4<f32>,
    normal_transform: mat4x4<f32>,
    material: vec4<f32>,
}

@group(1) @binding(0)
var<uniform> model: Model;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_position: vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_position = model.transform * vec4<f32>(input.position, 1.0);
    output.position = camera.view_projection * world_position;
    output.normal = normalize((model.normal_transform * vec4<f32>(input.normal, 0.0)).xyz);
    output.color = input.color;
    output.world_position = world_position.xyz;
    return output;
}

fn shadow_visibility(world_position: vec3<f32>, normal: vec3<f32>, light_direction: vec3<f32>) -> f32 {
    if lighting.shadow_params.z < 0.5 {
        return 1.0;
    }
    let clip = lighting.shadow_matrix * vec4<f32>(world_position, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let bias = lighting.shadow_params.y * max(1.0 - dot(normal, light_direction), 0.1);
    let depth = ndc.z - bias;
    var visibility = 0.0;
    for (var x = -1; x <= 1; x += 1) {
        for (var y = -1; y <= 1; y += 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * lighting.shadow_params.x;
            visibility += textureSampleCompareLevel(shadow_texture, shadow_sampler, uv + offset, depth);
        }
    }
    return visibility / 9.0;
}

fn shade(
    normal: vec3<f32>,
    view_direction: vec3<f32>,
    light_direction: vec3<f32>,
    radiance: vec3<f32>,
    base_color: vec3<f32>,
    metallic: f32,
    roughness: f32,
) -> vec3<f32> {
    let diffuse_light = max(dot(normal, light_direction), 0.0);
    let halfway = normalize(view_direction + light_direction);
    let specular_power = mix(128.0, 4.0, roughness);
    let specular = pow(max(dot(normal, halfway), 0.0), specular_power);
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let diffuse = base_color * (1.0 - metallic) * diffuse_light;
    return radiance * (diffuse + f0 * specular);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let view_direction = normalize(camera.position.xyz - input.world_position);
    let metallic = clamp(model.material.x, 0.0, 1.0);
    let roughness = clamp(model.material.y, 0.05, 1.0);
    let directional_direction = normalize(-lighting.directional_direction.xyz);
    let directional_radiance = lighting.directional_color.rgb * lighting.directional_direction.w;
    let visibility = shadow_visibility(input.world_position, normal, directional_direction);
    var color = input.color.rgb * 0.18;
    color += shade(normal, view_direction, directional_direction, directional_radiance * visibility, input.color.rgb, metallic, roughness);

    let point_count = min(lighting.point_count.x, 8u);
    for (var index = 0u; index < point_count; index += 1u) {
        let position_range = lighting.point_lights[index * 2u];
        let color_intensity = lighting.point_lights[index * 2u + 1u];
        let delta = position_range.xyz - input.world_position;
        let distance = length(delta);
        let attenuation = pow(clamp(1.0 - distance / position_range.w, 0.0, 1.0), 2.0);
        let light_direction = normalize(delta);
        let radiance = color_intensity.rgb * color_intensity.w * attenuation;
        color += shade(normal, view_direction, light_direction, radiance, input.color.rgb, metallic, roughness);
    }
    return vec4<f32>(color, input.color.a);
}
