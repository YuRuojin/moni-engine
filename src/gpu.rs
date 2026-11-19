use std::sync::Arc;

use anyhow::Context;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::engine::{SpriteKind, World};

const CIRCLE_SEGMENTS: usize = 48;

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
struct ViewportUniform {
    viewport: [f32; 4],
}

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
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
}

impl GpuRenderer {
    pub fn new(window: Arc<Window>) -> anyhow::Result<Self> {
        pollster::block_on(Self::new_async(window))
    }

    async fn new_async(window: Arc<Window>) -> anyhow::Result<Self> {
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
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
            depth_stencil: None,
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

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            viewport_buffer,
            viewport_bind_group,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
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
        let vertices = build_vertices(world);
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
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !vertices.is_empty() {
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sprite vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
        drop(pass);
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(output);
        Ok(RenderStatus::Rendered)
    }
}

fn build_vertices(world: &World) -> Vec<Vertex> {
    let mut sprites: Vec<_> = world
        .entities
        .values()
        .filter_map(|entity| entity.sprite.as_ref().map(|sprite| (entity, sprite)))
        .collect();
    sprites.sort_by_key(|(entity, sprite)| (sprite.layer, entity.id));

    let mut vertices = Vec::new();
    for (entity, sprite) in sprites {
        let color = parse_color(&sprite.color);
        match sprite.kind {
            SpriteKind::Rectangle => push_rectangle(
                &mut vertices,
                entity.transform.x,
                entity.transform.y,
                sprite.width,
                sprite.height,
                entity.transform.rotation,
                color,
            ),
            SpriteKind::Circle => push_circle(
                &mut vertices,
                entity.transform.x,
                entity.transform.y,
                sprite.width,
                sprite.height,
                entity.transform.rotation,
                color,
            ),
        }
    }
    vertices
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
    let points: Vec<[f32; 2]> = corners
        .into_iter()
        .map(|(local_x, local_y)| rotate_point(x, y, local_x, local_y, rotation))
        .collect();
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
