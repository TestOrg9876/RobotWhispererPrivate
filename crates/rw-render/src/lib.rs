//! The 3D engine behind the point cloud and robot panes.
//!
//! One [`Renderer`] per process owns the GPU device, the shaders and the
//! pipelines. Each pane owns its own [`Scene`] — its own points, its own
//! camera — so two panes can show two different robots without knowing about
//! each other, while sharing the one expensive thing between them.
//!
//! GPUI has no portable way to hand a pane a live GPU surface (`paint_surface`
//! is macOS-only), so a scene is drawn to an offscreen texture and read back as
//! RGBA. That costs a copy per frame and is the reason [`Frame`] exists.

mod camera;
mod scene;

pub use camera::{Camera, Mat4};
pub use scene::{Coloring, Grid, Points, Scene};

/// One rendered image, in RGBA order, tightly packed.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The largest pane that will be drawn.
///
/// A readback allocates width × height × 4 bytes twice over, and a pane
/// stretched across two 4K monitors would ask for 130 MB a frame. Beyond this
/// the scene is drawn smaller and scaled up, which nobody can see on a point
/// cloud and everybody would notice as a stall.
const MAX_DIMENSION: u32 = 2560;

/// The shared GPU device, shaders and pipelines.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    points: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// What the adapter turned out to be, for the diagnostics line.
    pub adapter: String,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// `copy_texture_to_buffer` requires each row to start on a 256-byte boundary,
/// so the readback buffer is padded and unpadded again on the way out.
const COPY_ALIGNMENT: u32 = 256;

impl Renderer {
    /// Opens a device. Fails when there is no usable adapter at all, which is a
    /// pane that says so rather than an app that will not start.
    pub async fn new() -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|error| format!("no usable graphics adapter: {error}"))?;
        let info = adapter.get_info();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rw-render"),
                // The defaults, so the same code runs on WebGL2 in a browser as
                // on a desktop GPU. Nothing here needs more.
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("could not open the graphics device: {error}"))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene uniforms"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene uniforms"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let points = Self::pipeline(
            &device,
            &shader,
            &pipeline_layout,
            "point",
            wgpu::PrimitiveTopology::TriangleList,
        );
        let lines = Self::pipeline(
            &device,
            &shader,
            &pipeline_layout,
            "line",
            wgpu::PrimitiveTopology::LineList,
        );

        Ok(Self {
            device,
            queue,
            points,
            lines,
            uniforms,
            bind_group,
            adapter: format!("{} ({:?})", info.name, info.backend),
        })
    }

    fn pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        entry: &str,
        topology: wgpu::PrimitiveTopology,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(&format!("vs_{entry}")),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 32,
                            shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Draws a scene and reads it back.
    ///
    /// `None` when the pane has no area to draw into, which happens while a
    /// dock pane is being resized.
    pub fn render(&self, scene: &Scene, width: u32, height: u32) -> Option<Frame> {
        let width = width.min(MAX_DIMENSION);
        let height = height.min(MAX_DIMENSION);
        if width == 0 || height == 0 {
            return None;
        }

        let aspect = width as f32 / height as f32;
        self.queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&Uniforms {
                view_projection: scene.camera.view_projection(aspect),
                viewport: [width as f32, height as f32],
                point_size: scene.point_size,
                _padding: 0.,
            }),
        );

        let vertices = scene.vertices();
        let (point_count, line_count) = (vertices.points.len(), vertices.lines.len());
        let points = self.vertex_buffer("points", &vertices.points);
        let lines = self.vertex_buffer("lines", &vertices.lines);

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let row_bytes = width * 4;
        let padded = row_bytes.div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scene"),
            });
        {
            let view = target.create_view(&Default::default());
            let depth_view = depth.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: scene.background[0] as f64,
                            g: scene.background[1] as f64,
                            b: scene.background[2] as f64,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            if line_count > 0 {
                pass.set_pipeline(&self.lines);
                pass.set_vertex_buffer(0, lines.slice(..));
                pass.draw(0..line_count as u32, 0..1);
            }
            if point_count > 0 {
                pass.set_pipeline(&self.points);
                pass.set_vertex_buffer(0, points.slice(..));
                pass.draw(0..point_count as u32, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        // The map completes when the queue drains; polling for it is the only
        // portable way to wait, and on wasm this returns without blocking.
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;

        let mapped = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((row_bytes * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&mapped[start..start + row_bytes as usize]);
        }
        drop(mapped);
        readback.unmap();

        Some(Frame {
            width,
            height,
            rgba,
        })
    }

    fn vertex_buffer(&self, label: &str, vertices: &[Vertex]) -> wgpu::Buffer {
        // An empty buffer is invalid, and the draw is skipped anyway; one
        // vertex's worth of nothing is cheaper than branching at every use.
        let bytes = if vertices.is_empty() {
            vec![0u8; std::mem::size_of::<Vertex>()]
        } else {
            bytemuck::cast_slice(vertices).to_vec()
        };
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &bytes);
        buffer
    }
}

/// One vertex, laid out to match `scene.wgsl`.
///
/// `corner` is which end of a point's quad this vertex is, in units of half a
/// point; lines leave it at zero.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    _pad: f32,
    pub color: [f32; 4],
    pub corner: [f32; 2],
    _pad2: [f32; 2],
}

impl Vertex {
    pub fn new(position: [f32; 3], color: [f32; 4], corner: [f32; 2]) -> Self {
        Self {
            position,
            _pad: 0.,
            color,
            corner,
            _pad2: [0.; 2],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_projection: Mat4,
    viewport: [f32; 2],
    point_size: f32,
    _padding: f32,
}
