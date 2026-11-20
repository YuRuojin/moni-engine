use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::engine::{SpriteKind, World};
use crate::fbx::{FbxMesh, load_fbx};
use crate::scene3d::{Camera3d, CameraProjection, Light, MeshKind, Transform3d, Vector3};

const CIRCLE_SEGMENTS: usize = 48;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const INITIAL_SPRITE_VERTEX_CAPACITY: usize = 1_024;
const INITIAL_MESH_VERTEX_CAPACITY: usize = 512;
const SHADOW_MAP_SIZE: u32 = 1_024;
const MAX_POINT_LIGHTS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

impl Vertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex3d {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 4],
}

impl Vertex3d {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x4
        ],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewportUniform {
    viewport: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
    position: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightingUniform {
    directional_direction: [f32; 4],
    directional_color: [f32; 4],
    point_count: [u32; 4],
    point_lights: [[f32; 4]; MAX_POINT_LIGHTS * 2],
    shadow_matrix: [[f32; 4]; 4],
    shadow_params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ModelUniform {
    transform: [[f32; 4]; 4],
    normal_transform: [[f32; 4]; 4],
    material: [f32; 4],
}

struct GpuTiming {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    frame_index: u64,
}

impl GpuTiming {
    fn new(device: &wgpu::Device) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("3D GPU timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 4,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D timestamp resolve buffer"),
            size: 32,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D timestamp readback buffer"),
            size: 32,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            frame_index: 0,
        }
    }

    fn is_sampling(&self) -> bool {
        self.frame_index.is_multiple_of(30)
    }

    fn writes(&self, first: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.is_sampling()
            .then_some(wgpu::RenderPassTimestampWrites {
                query_set: &self.query_set,
                beginning_of_pass_write_index: Some(first),
                end_of_pass_write_index: Some(first + 1),
            })
    }
}

struct FbxGpuObject {
    source: String,
    color: String,
    transform: Transform3d,
    vertex_buffer: wgpu::Buffer,
    vertex_count: u32,
    model_buffer: wgpu::Buffer,
    model_bind_group: wgpu::BindGroup,
}

#[derive(Debug, Clone, Copy)]
pub struct OverlayQuad {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStatus {
    Rendered,
    Reconfigure,
    Skip,
}

pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    pipeline_3d: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    lighting_buffer: wgpu::Buffer,
    shadow_camera_buffer: wgpu::Buffer,
    shadow_camera_bind_group: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    model_bind_group_layout: wgpu::BindGroupLayout,
    identity_model_bind_group: wgpu::BindGroup,
    fbx_objects: BTreeMap<u64, FbxGpuObject>,
    depth_view: wgpu::TextureView,
    sprite_vertices: Vec<Vertex>,
    mesh_vertices: Vec<Vertex3d>,
    sprite_order: Vec<(i32, u64)>,
    overlay_quads: Vec<OverlayQuad>,
    sprite_vertex_buffer: wgpu::Buffer,
    mesh_vertex_buffer: wgpu::Buffer,
    sprite_vertex_capacity: usize,
    mesh_vertex_capacity: usize,
    fbx_meshes: BTreeMap<String, FbxMesh>,
    load_fbx_assets: bool,
    gpu_timing: Option<GpuTiming>,
    gpu_samples: Vec<(f32, f32)>,
    last_preparation_time: Duration,
}

impl GpuRenderer {
    pub fn new(
        window: Arc<Window>,
        world: &World,
        collect_gpu_timestamps: bool,
        load_fbx_assets: bool,
    ) -> anyhow::Result<Self> {
        pollster::block_on(Self::new_async(
            window,
            world,
            collect_gpu_timestamps,
            load_fbx_assets,
        ))
    }

    async fn new_async(
        window: Arc<Window>,
        world: &World,
        collect_gpu_timestamps: bool,
        load_fbx_assets: bool,
    ) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window)
            .context("could not create a wgpu surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("could not find a compatible GPU adapter")?;
        let timestamp_features = if collect_gpu_timestamps
            && adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY)
        {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: timestamp_features,
                ..wgpu::DeviceDescriptor::default()
            })
            .await
            .context("could not create a GPU device")?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let present_mode = capabilities
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Fifo)
            .unwrap_or(capabilities.present_modes[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: Vec::new(),
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("moni sprite shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport uniform"),
            size: size_of::<ViewportUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport bind group"),
            layout: &viewport_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite pipeline layout"),
            bind_group_layouts: &[Some(&viewport_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex::LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let shader_3d = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("moni 3D shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader3d.wgsl").into()),
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D camera uniform"),
            size: size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lighting_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D lighting uniform"),
            size: size_of::<LightingUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D shadow camera uniform"),
            size: size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_view = create_shadow_view(&device);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("3D shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("3D camera bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX.union(wgpu::ShaderStages::FRAGMENT),
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX.union(wgpu::ShaderStages::FRAGMENT),
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let shadow_camera_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("3D shadow camera bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let camera_bind_group = create_camera_bind_group(
            &device,
            &camera_layout,
            &camera_buffer,
            &lighting_buffer,
            &shadow_view,
            &shadow_sampler,
            "3D camera bind group",
        );
        let shadow_camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("3D shadow camera bind group"),
            layout: &shadow_camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shadow_camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: lighting_buffer.as_entire_binding(),
                },
            ],
        });
        let model_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("3D model bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX.union(wgpu::ShaderStages::FRAGMENT),
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let identity_model_buffer = create_model_buffer(&device, "3D identity model");
        queue.write_buffer(
            &identity_model_buffer,
            0,
            bytemuck::bytes_of(&model_uniform(&Transform3d::default(), 0.0, 0.5)),
        );
        let identity_model_bind_group = create_model_bind_group(
            &device,
            &model_bind_group_layout,
            &identity_model_buffer,
            "3D identity model bind group",
        );
        let pipeline_layout_3d = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("3D pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&model_bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline_3d = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("3D pipeline"),
            layout: Some(&pipeline_layout_3d),
            vertex: wgpu::VertexState {
                module: &shader_3d,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex3d::LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader_3d,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("moni shadow shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shadow.wgsl").into()),
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("3D shadow pipeline layout"),
                bind_group_layouts: &[Some(&shadow_camera_layout), Some(&model_bind_group_layout)],
                immediate_size: 0,
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("3D shadow pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex3d::LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        let depth_view = create_depth_view(&device, &config);
        let sprite_vertex_buffer = create_vertex_buffer(
            &device,
            INITIAL_SPRITE_VERTEX_CAPACITY,
            size_of::<Vertex>(),
            "sprite vertices",
        );
        let mesh_vertex_buffer = create_vertex_buffer(
            &device,
            INITIAL_MESH_VERTEX_CAPACITY,
            size_of::<Vertex3d>(),
            "3D mesh vertices",
        );

        let gpu_timing = timestamp_features
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| GpuTiming::new(&device));
        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            pipeline_3d,
            shadow_pipeline,
            viewport_buffer,
            viewport_bind_group,
            camera_buffer,
            camera_bind_group,
            lighting_buffer,
            shadow_camera_buffer,
            shadow_camera_bind_group,
            shadow_view,
            model_bind_group_layout,
            identity_model_bind_group,
            fbx_objects: BTreeMap::new(),
            depth_view,
            sprite_vertices: Vec::with_capacity(INITIAL_SPRITE_VERTEX_CAPACITY),
            mesh_vertices: Vec::with_capacity(INITIAL_MESH_VERTEX_CAPACITY),
            sprite_order: Vec::with_capacity(64),
            overlay_quads: Vec::new(),
            sprite_vertex_buffer,
            mesh_vertex_buffer,
            sprite_vertex_capacity: INITIAL_SPRITE_VERTEX_CAPACITY,
            mesh_vertex_capacity: INITIAL_MESH_VERTEX_CAPACITY,
            fbx_meshes: BTreeMap::new(),
            load_fbx_assets,
            gpu_timing,
            gpu_samples: Vec::new(),
            last_preparation_time: Duration::ZERO,
        };
        renderer.prepare_fbx_objects(world)?;
        Ok(renderer)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth_view(&self.device, &self.config);
    }

    pub fn last_preparation_time(&self) -> Duration {
        self.last_preparation_time
    }

    pub fn loaded_mesh_vertices(&self) -> usize {
        self.fbx_meshes
            .values()
            .map(|mesh| mesh.vertices.len())
            .sum()
    }

    pub fn set_overlay_quads(&mut self, quads: Vec<OverlayQuad>) {
        self.overlay_quads = quads;
    }

    pub fn gpu_timestamps_supported(&self) -> bool {
        self.gpu_timing.is_some()
    }

    pub fn take_gpu_samples(&mut self) -> Vec<(f32, f32)> {
        std::mem::take(&mut self.gpu_samples)
    }

    fn read_gpu_samples(&mut self) -> anyhow::Result<()> {
        let Some(timing) = &self.gpu_timing else {
            return Ok(());
        };
        if !timing.is_sampling() {
            return Ok(());
        }
        let map_error = Arc::new(std::sync::Mutex::new(Option::<String>::None));
        let callback_error = Arc::clone(&map_error);
        timing
            .readback_buffer
            .map_async(wgpu::MapMode::Read, 0..32, move |result| {
                if let Err(error) = result {
                    *callback_error.lock().expect("map error mutex") = Some(error.to_string());
                }
            });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        if let Some(error) = map_error.lock().expect("map error mutex").take() {
            return Err(anyhow::anyhow!(error));
        }
        let bytes = timing.readback_buffer.get_mapped_range(..)?;
        let mut timestamps = [0_u64; 4];
        for (index, chunk) in bytes.chunks_exact(8).enumerate() {
            timestamps[index] = u64::from_le_bytes(chunk.try_into()?);
        }
        drop(bytes);
        timing.readback_buffer.unmap();
        let period = self.queue.get_timestamp_period();
        let elapsed = |start: u64, end: u64| {
            if end >= start {
                (end - start) as f32 * period / 1_000_000.0
            } else {
                0.0
            }
        };
        self.gpu_samples.push((
            elapsed(timestamps[0], timestamps[1]),
            elapsed(timestamps[2], timestamps[3]),
        ));
        Ok(())
    }

    fn prepare_fbx_objects(&mut self, world: &World) -> anyhow::Result<()> {
        if !self.load_fbx_assets {
            self.fbx_objects.clear();
            self.fbx_meshes.clear();
            return Ok(());
        }
        self.fbx_objects.retain(|id, _| {
            world.entities.get(id).is_some_and(|entity| {
                entity
                    .model
                    .as_ref()
                    .is_some_and(|model| model.kind == MeshKind::Fbx)
            })
        });
        for entity in world.entities.values() {
            let Some(model) = &entity.model else {
                continue;
            };
            if model.kind != MeshKind::Fbx {
                continue;
            }
            let source = model
                .source
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("entity {} has no FBX source", entity.id))?;
            if !self.fbx_meshes.contains_key(source) {
                let path = crate::workspace_path(source)?;
                let mesh = load_fbx(&path)?;
                self.fbx_meshes.insert(source.to_string(), mesh);
            }
            let rebuild = self
                .fbx_objects
                .get(&entity.id)
                .is_none_or(|object| object.source != source || object.color != model.color);
            if rebuild {
                let mesh = &self.fbx_meshes[source];
                let fallback = parse_color(&model.color);
                let vertices: Vec<Vertex3d> = mesh
                    .vertices
                    .iter()
                    .map(|vertex| Vertex3d {
                        position: vertex.position,
                        normal: vertex.normal,
                        color: vertex.color.unwrap_or(fallback),
                    })
                    .collect();
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some(source),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let model_buffer = create_model_buffer(&self.device, "3D model transform");
                let model_bind_group = create_model_bind_group(
                    &self.device,
                    &self.model_bind_group_layout,
                    &model_buffer,
                    "3D model transform bind group",
                );
                self.fbx_objects.insert(
                    entity.id,
                    FbxGpuObject {
                        source: source.to_string(),
                        color: model.color.clone(),
                        transform: model.transform,
                        vertex_buffer,
                        vertex_count: u32::try_from(vertices.len())
                            .context("FBX vertex count exceeds u32")?,
                        model_buffer,
                        model_bind_group,
                    },
                );
            }
            let object = self
                .fbx_objects
                .get_mut(&entity.id)
                .expect("FBX object was prepared");
            object.transform = model.transform;
            self.queue.write_buffer(
                &object.model_buffer,
                0,
                bytemuck::bytes_of(&model_uniform(
                    &model.transform,
                    model.metallic,
                    model.roughness,
                )),
            );
        }
        Ok(())
    }

    pub fn render(&mut self, world: &World) -> anyhow::Result<RenderStatus> {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                return Ok(RenderStatus::Reconfigure);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(RenderStatus::Skip);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow::anyhow!(
                    "wgpu surface acquisition failed validation"
                ));
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(timing) = &mut self.gpu_timing {
            timing.frame_index += 1;
        }
        let preparation_started = Instant::now();
        self.prepare_fbx_objects(world)?;
        build_vertices(world, &mut self.sprite_vertices, &mut self.sprite_order);
        for quad in &self.overlay_quads {
            push_rectangle(
                &mut self.sprite_vertices,
                quad.x,
                quad.y,
                quad.width,
                quad.height,
                0.0,
                quad.color,
            );
        }
        build_vertices_3d(world, &mut self.mesh_vertices);
        upload_vertices(
            &self.device,
            &self.queue,
            &mut self.sprite_vertex_buffer,
            &mut self.sprite_vertex_capacity,
            &self.sprite_vertices,
            "sprite vertices",
        );
        upload_vertices(
            &self.device,
            &self.queue,
            &mut self.mesh_vertex_buffer,
            &mut self.mesh_vertex_capacity,
            &self.mesh_vertices,
            "3D mesh vertices",
        );
        let aspect = self.config.width as f64 / self.config.height as f64;
        let camera = CameraUniform {
            view_projection: view_projection(&world.camera, aspect),
            position: camera_position(&world.camera),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        let shadow_camera = shadow_camera(world);
        let shadow_matrix = view_projection(&shadow_camera, 1.0);
        let shadow_camera_uniform = CameraUniform {
            view_projection: shadow_matrix,
            position: camera_position(&shadow_camera),
        };
        self.queue.write_buffer(
            &self.shadow_camera_buffer,
            0,
            bytemuck::bytes_of(&shadow_camera_uniform),
        );
        let lighting = lighting_uniform(world, shadow_matrix);
        self.queue
            .write_buffer(&self.lighting_buffer, 0, bytemuck::bytes_of(&lighting));
        let viewport = ViewportUniform {
            viewport: [
                world.viewport.width as f32,
                world.viewport.height as f32,
                0.0,
                0.0,
            ],
        };
        self.queue
            .write_buffer(&self.viewport_buffer, 0, bytemuck::bytes_of(&viewport));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("moni render encoder"),
            });
        if shadows_enabled(world) {
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.gpu_timing.as_ref().and_then(|timing| timing.writes(0)),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            shadow_pass.set_pipeline(&self.shadow_pipeline);
            shadow_pass.set_bind_group(0, &self.shadow_camera_bind_group, &[]);
            if !self.mesh_vertices.is_empty() {
                shadow_pass.set_bind_group(1, &self.identity_model_bind_group, &[]);
                shadow_pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
                shadow_pass.draw(0..self.mesh_vertices.len() as u32, 0..1);
            }
            for object in self
                .fbx_objects
                .values()
                .filter(|object| object_visible(&shadow_matrix, &object.transform))
            {
                shadow_pass.set_bind_group(1, &object.model_bind_group, &[]);
                shadow_pass.set_vertex_buffer(0, object.vertex_buffer.slice(..));
                shadow_pass.draw(0..object.vertex_count, 0..1);
            }
        }
        let background = parse_color(&world.viewport.background);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("moni render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: background[0] as f64,
                        g: background[1] as f64,
                        b: background[2] as f64,
                        a: background[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: self.gpu_timing.as_ref().and_then(|timing| timing.writes(2)),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !self.mesh_vertices.is_empty() {
            pass.set_pipeline(&self.pipeline_3d);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.identity_model_bind_group, &[]);
            pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
            pass.draw(0..self.mesh_vertices.len() as u32, 0..1);
        }
        for object in self
            .fbx_objects
            .values()
            .filter(|object| object_visible(&camera.view_projection, &object.transform))
        {
            pass.set_pipeline(&self.pipeline_3d);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &object.model_bind_group, &[]);
            pass.set_vertex_buffer(0, object.vertex_buffer.slice(..));
            pass.draw(0..object.vertex_count, 0..1);
        }
        if !self.sprite_vertices.is_empty() {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, self.sprite_vertex_buffer.slice(..));
            pass.draw(0..self.sprite_vertices.len() as u32, 0..1);
        }
        drop(pass);
        if self.gpu_timing.as_ref().is_some_and(GpuTiming::is_sampling) {
            let timing = self.gpu_timing.as_ref().expect("GPU timing exists");
            encoder.resolve_query_set(&timing.query_set, 0..4, &timing.resolve_buffer, 0);
            encoder.copy_buffer_to_buffer(
                &timing.resolve_buffer,
                0,
                &timing.readback_buffer,
                0,
                32,
            );
        }
        self.last_preparation_time = preparation_started.elapsed();
        self.queue.submit(Some(encoder.finish()));
        self.read_gpu_samples()?;
        self.queue.present(output);
        Ok(RenderStatus::Rendered)
    }
}

fn create_shadow_view(device: &wgpu::Device) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("3D directional shadow map"),
        size: wgpu::Extent3d {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn create_camera_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    camera: &wgpu::Buffer,
    lighting: &wgpu::Buffer,
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: lighting.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(shadow_sampler),
            },
        ],
    })
}

fn create_model_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size_of::<ModelUniform>() as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_model_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}

fn model_uniform(transform: &Transform3d, metallic: f32, roughness: f32) -> ModelUniform {
    ModelUniform {
        transform: transform_matrix(transform, false),
        normal_transform: transform_matrix(transform, true),
        material: [metallic, roughness, 0.0, 0.0],
    }
}

fn transform_matrix(transform: &Transform3d, normal: bool) -> [[f32; 4]; 4] {
    let scale = if normal {
        [
            1.0 / transform.scale.x as f32,
            1.0 / transform.scale.y as f32,
            1.0 / transform.scale.z as f32,
        ]
    } else {
        [
            transform.scale.x as f32,
            transform.scale.y as f32,
            transform.scale.z as f32,
        ]
    };
    let x = rotate_vector([scale[0], 0.0, 0.0], transform.rotation);
    let y = rotate_vector([0.0, scale[1], 0.0], transform.rotation);
    let z = rotate_vector([0.0, 0.0, scale[2]], transform.rotation);
    let translation = if normal {
        [0.0, 0.0, 0.0]
    } else {
        [
            transform.position.x as f32,
            transform.position.y as f32,
            transform.position.z as f32,
        ]
    };
    [
        [x[0], y[0], z[0], 0.0],
        [x[1], y[1], z[1], 0.0],
        [x[2], y[2], z[2], 0.0],
        [translation[0], translation[1], translation[2], 1.0],
    ]
}

fn create_vertex_buffer(
    device: &wgpu::Device,
    capacity: usize,
    element_size: usize,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (capacity * element_size) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn upload_vertices<T: Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &mut wgpu::Buffer,
    capacity: &mut usize,
    vertices: &[T],
    label: &str,
) {
    if vertices.len() > *capacity {
        *capacity = vertices.len().next_power_of_two();
        *buffer = create_vertex_buffer(device, *capacity, size_of::<T>(), label);
    }
    if !vertices.is_empty() {
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(vertices));
    }
}

fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("3D depth texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn build_vertices_3d(world: &World, vertices: &mut Vec<Vertex3d>) {
    vertices.clear();
    for entity in world.entities.values() {
        let Some(model) = &entity.model else {
            continue;
        };
        let color = parse_color(&model.color);
        match model.kind {
            MeshKind::Cube => push_cube(vertices, &model.transform, color),
            MeshKind::Plane => push_quad(
                vertices,
                [
                    [-0.5, 0.0, 0.5],
                    [0.5, 0.0, 0.5],
                    [0.5, 0.0, -0.5],
                    [-0.5, 0.0, -0.5],
                ],
                [0.0, 1.0, 0.0],
                &model.transform,
                color,
            ),
            MeshKind::Fbx => {}
        }
    }
}

fn push_cube(vertices: &mut Vec<Vertex3d>, transform: &Transform3d, color: [f32; 4]) {
    let faces = [
        (
            [
                [-0.5, -0.5, 0.5],
                [0.5, -0.5, 0.5],
                [0.5, 0.5, 0.5],
                [-0.5, 0.5, 0.5],
            ],
            [0.0, 0.0, 1.0],
        ),
        (
            [
                [0.5, -0.5, -0.5],
                [-0.5, -0.5, -0.5],
                [-0.5, 0.5, -0.5],
                [0.5, 0.5, -0.5],
            ],
            [0.0, 0.0, -1.0],
        ),
        (
            [
                [0.5, -0.5, 0.5],
                [0.5, -0.5, -0.5],
                [0.5, 0.5, -0.5],
                [0.5, 0.5, 0.5],
            ],
            [1.0, 0.0, 0.0],
        ),
        (
            [
                [-0.5, -0.5, -0.5],
                [-0.5, -0.5, 0.5],
                [-0.5, 0.5, 0.5],
                [-0.5, 0.5, -0.5],
            ],
            [-1.0, 0.0, 0.0],
        ),
        (
            [
                [-0.5, 0.5, 0.5],
                [0.5, 0.5, 0.5],
                [0.5, 0.5, -0.5],
                [-0.5, 0.5, -0.5],
            ],
            [0.0, 1.0, 0.0],
        ),
        (
            [
                [-0.5, -0.5, -0.5],
                [0.5, -0.5, -0.5],
                [0.5, -0.5, 0.5],
                [-0.5, -0.5, 0.5],
            ],
            [0.0, -1.0, 0.0],
        ),
    ];
    for (points, normal) in faces {
        push_quad(vertices, points, normal, transform, color);
    }
}

fn push_quad(
    vertices: &mut Vec<Vertex3d>,
    points: [[f32; 3]; 4],
    normal: [f32; 3],
    transform: &Transform3d,
    color: [f32; 4],
) {
    let normal = transform_normal(normal, transform);
    for index in [0, 1, 2, 0, 2, 3] {
        vertices.push(Vertex3d {
            position: transform_point(points[index], transform),
            normal,
            color,
        });
    }
}

fn transform_point(point: [f32; 3], transform: &Transform3d) -> [f32; 3] {
    let scaled = [
        point[0] * transform.scale.x as f32,
        point[1] * transform.scale.y as f32,
        point[2] * transform.scale.z as f32,
    ];
    let rotated = rotate_vector(scaled, transform.rotation);
    [
        rotated[0] + transform.position.x as f32,
        rotated[1] + transform.position.y as f32,
        rotated[2] + transform.position.z as f32,
    ]
}

fn transform_normal(normal: [f32; 3], transform: &Transform3d) -> [f32; 3] {
    let scaled = [
        normal[0] / transform.scale.x as f32,
        normal[1] / transform.scale.y as f32,
        normal[2] / transform.scale.z as f32,
    ];
    normalize(rotate_vector(scaled, transform.rotation))
}

fn rotate_vector(vector: [f32; 3], rotation: Vector3) -> [f32; 3] {
    let (sin_x, cos_x) = (rotation.x as f32).sin_cos();
    let (sin_y, cos_y) = (rotation.y as f32).sin_cos();
    let (sin_z, cos_z) = (rotation.z as f32).sin_cos();
    let mut result = vector;
    result = [
        result[0],
        result[1] * cos_x - result[2] * sin_x,
        result[1] * sin_x + result[2] * cos_x,
    ];
    result = [
        result[0] * cos_y + result[2] * sin_y,
        result[1],
        -result[0] * sin_y + result[2] * cos_y,
    ];
    [
        result[0] * cos_z - result[1] * sin_z,
        result[0] * sin_z + result[1] * cos_z,
        result[2],
    ]
}

fn object_visible(matrix: &[[f32; 4]; 4], transform: &Transform3d) -> bool {
    let center = [
        transform.position.x as f32,
        transform.position.y as f32,
        transform.position.z as f32,
    ];
    let scale = transform
        .scale
        .x
        .abs()
        .max(transform.scale.y.abs())
        .max(transform.scale.z.abs()) as f32;
    let radius = 3.0_f32.sqrt() * scale;
    let row = |index: usize| {
        [
            matrix[0][index],
            matrix[1][index],
            matrix[2][index],
            matrix[3][index],
        ]
    };
    let (x, y, z, w) = (row(0), row(1), row(2), row(3));
    let add = |a: [f32; 4], b: [f32; 4]| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
    let sub = |a: [f32; 4], b: [f32; 4]| [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
    let planes = [add(w, x), sub(w, x), add(w, y), sub(w, y), z, sub(w, z)];
    for plane in planes {
        let length = (plane[0] * plane[0] + plane[1] * plane[1] + plane[2] * plane[2]).sqrt();
        if length > f32::EPSILON {
            let distance =
                plane[0] * center[0] + plane[1] * center[1] + plane[2] * center[2] + plane[3];
            if distance / length < -radius {
                return false;
            }
        }
    }
    true
}

fn camera_position(camera: &Camera3d) -> [f32; 4] {
    [
        camera.position.x as f32,
        camera.position.y as f32,
        camera.position.z as f32,
        0.0,
    ]
}

fn directional_light(world: &World) -> Option<(&Vector3, &[f32; 3], f32, bool)> {
    world.lights.iter().find_map(|light| match light {
        Light::Directional {
            direction,
            color,
            intensity,
            shadows,
        } => Some((direction, color, *intensity, *shadows)),
        _ => None,
    })
}

fn shadows_enabled(world: &World) -> bool {
    directional_light(world).is_some_and(|(_, _, _, shadows)| shadows)
}

fn shadow_camera(world: &World) -> Camera3d {
    let direction = directional_light(world)
        .map(|(direction, _, _, _)| {
            let length =
                (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z)
                    .sqrt();
            [
                direction.x / length,
                direction.y / length,
                direction.z / length,
            ]
        })
        .unwrap_or([-0.45, -0.8, -0.35]);
    Camera3d {
        projection: CameraProjection::Orthographic,
        orthographic_height: 12.0,
        position: Vector3 {
            x: -direction[0] * 10.0,
            y: -direction[1] * 10.0,
            z: -direction[2] * 10.0,
        },
        target: Vector3::ZERO,
        near: 0.1,
        far: 40.0,
        ..Camera3d::default()
    }
}

fn lighting_uniform(world: &World, shadow_matrix: [[f32; 4]; 4]) -> LightingUniform {
    let mut uniform = LightingUniform {
        shadow_matrix,
        shadow_params: [
            1.0 / SHADOW_MAP_SIZE as f32,
            0.002,
            if shadows_enabled(world) { 1.0 } else { 0.0 },
            0.0,
        ],
        ..LightingUniform::zeroed()
    };
    if let Some((direction, color, intensity, _)) = directional_light(world) {
        uniform.directional_direction = [
            direction.x as f32,
            direction.y as f32,
            direction.z as f32,
            intensity,
        ];
        uniform.directional_color = [color[0], color[1], color[2], 1.0];
    }
    let mut point_count = 0;
    for light in &world.lights {
        let Light::Point {
            position,
            color,
            intensity,
            range,
        } = light
        else {
            continue;
        };
        if point_count == MAX_POINT_LIGHTS {
            break;
        }
        uniform.point_lights[point_count * 2] = [
            position.x as f32,
            position.y as f32,
            position.z as f32,
            *range,
        ];
        uniform.point_lights[point_count * 2 + 1] = [color[0], color[1], color[2], *intensity];
        point_count += 1;
    }
    uniform.point_count[0] = point_count as u32;
    uniform
}

fn view_projection(camera: &Camera3d, aspect: f64) -> [[f32; 4]; 4] {
    let eye = [
        camera.position.x as f32,
        camera.position.y as f32,
        camera.position.z as f32,
    ];
    let target = [
        camera.target.x as f32,
        camera.target.y as f32,
        camera.target.z as f32,
    ];
    let forward = normalize(subtract(target, eye));
    let mut right = cross(forward, [0.0, 1.0, 0.0]);
    if dot(right, right) < 0.000001 {
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
            let f = 1.0 / (camera.fov_y / 2.0).tan();
            projection[0][0] = (f / aspect) as f32;
            projection[1][1] = f as f32;
            projection[2][2] = (camera.far / (camera.near - camera.far)) as f32;
            projection[2][3] = -1.0;
            projection[3][2] = (camera.near * camera.far / (camera.near - camera.far)) as f32;
        }
        CameraProjection::Orthographic => {
            let half_height = camera.orthographic_height / 2.0;
            let half_width = half_height * aspect;
            projection[0][0] = (1.0 / half_width) as f32;
            projection[1][1] = (1.0 / half_height) as f32;
            projection[2][2] = (1.0 / (camera.near - camera.far)) as f32;
            projection[3][2] = (camera.near / (camera.near - camera.far)) as f32;
            projection[3][3] = 1.0;
        }
    }
    multiply_matrices(projection, view)
}

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
    let length = dot(vector, vector).sqrt();
    [vector[0] / length, vector[1] / length, vector[2] / length]
}

fn build_vertices(world: &World, vertices: &mut Vec<Vertex>, sprite_order: &mut Vec<(i32, u64)>) {
    vertices.clear();
    sprite_order.clear();
    sprite_order.extend(world.entities.values().filter_map(|entity| {
        entity
            .sprite
            .as_ref()
            .map(|sprite| (sprite.layer, entity.id))
    }));
    sprite_order.sort_unstable();

    for &(_, id) in sprite_order.iter() {
        let Some(entity) = world.entities.get(&id) else {
            continue;
        };
        let Some(sprite) = entity.sprite.as_ref() else {
            continue;
        };
        let color = parse_color(&sprite.color);
        match sprite.kind {
            SpriteKind::Rectangle => push_rectangle(
                vertices,
                entity.transform.x,
                entity.transform.y,
                sprite.width,
                sprite.height,
                entity.transform.rotation,
                color,
            ),
            SpriteKind::Circle => push_circle(
                vertices,
                entity.transform.x,
                entity.transform.y,
                sprite.width,
                sprite.height,
                entity.transform.rotation,
                color,
            ),
        }
    }
}

fn push_rectangle(
    vertices: &mut Vec<Vertex>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: f64,
    color: [f32; 4],
) {
    let half_width = width / 2.0;
    let half_height = height / 2.0;
    let corners = [
        (-half_width, -half_height),
        (half_width, -half_height),
        (half_width, half_height),
        (-half_width, half_height),
    ];
    let points = corners.map(|(local_x, local_y)| rotate_point(x, y, local_x, local_y, rotation));
    for index in [0, 1, 2, 0, 2, 3] {
        vertices.push(Vertex {
            position: points[index],
            color,
        });
    }
}

fn push_circle(
    vertices: &mut Vec<Vertex>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: f64,
    color: [f32; 4],
) {
    let center = [x as f32, y as f32];
    for segment in 0..CIRCLE_SEGMENTS {
        let start = segment as f64 / CIRCLE_SEGMENTS as f64 * std::f64::consts::TAU;
        let end = (segment + 1) as f64 / CIRCLE_SEGMENTS as f64 * std::f64::consts::TAU;
        let first = rotate_point(
            x,
            y,
            start.cos() * width / 2.0,
            start.sin() * height / 2.0,
            rotation,
        );
        let second = rotate_point(
            x,
            y,
            end.cos() * width / 2.0,
            end.sin() * height / 2.0,
            rotation,
        );
        vertices.push(Vertex {
            position: center,
            color,
        });
        vertices.push(Vertex {
            position: first,
            color,
        });
        vertices.push(Vertex {
            position: second,
            color,
        });
    }
}

fn rotate_point(x: f64, y: f64, local_x: f64, local_y: f64, rotation: f64) -> [f32; 2] {
    let (sin, cos) = rotation.sin_cos();
    [
        (x + local_x * cos - local_y * sin) as f32,
        (y + local_x * sin + local_y * cos) as f32,
    ]
}

fn parse_color(value: &str) -> [f32; 4] {
    let value = value.strip_prefix('#').unwrap_or(value);
    let red = color_channel(value, 0);
    let green = color_channel(value, 2);
    let blue = color_channel(value, 4);
    let alpha = color_channel(value, 6).unwrap_or(255);
    match (red, green, blue) {
        (Some(red), Some(green), Some(blue)) if value.len() == 6 || value.len() == 8 => [
            srgb_to_linear(red),
            srgb_to_linear(green),
            srgb_to_linear(blue),
            alpha as f32 / 255.0,
        ],
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}

fn color_channel(value: &str, offset: usize) -> Option<u8> {
    value
        .get(offset..offset + 2)
        .and_then(|channel| u8::from_str_radix(channel, 16).ok())
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = value as f32 / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
