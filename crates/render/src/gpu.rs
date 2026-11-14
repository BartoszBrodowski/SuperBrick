//! wgpu side: device setup, pipelines, scene buffers, and offscreen frames with readback.

use std::time::Instant;

use wgpu::util::DeviceExt;

use crate::scene::{Batch, Camera, CondVertex, Instance, MeshRange, Scene, Vertex};

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub(crate) const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.80,
    g: 0.82,
    b: 0.85,
    a: 1.0,
};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
}

impl Gpu {
    /// A device with no window, for offscreen frames and benchmarks.
    pub fn headless() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cad"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU device: {e}"))?;
        Ok(Gpu {
            device,
            queue,
            info,
        })
    }

    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    light: [f32; 4],
    viewport: [f32; 4],
}

const TRI_ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32];
const COND_ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32, 2 => Float32x3, 3 => Float32x3, 9 => Float32x3];
const INST_ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
    4 => Float32x3, 5 => Float32x3, 6 => Float32x3, 7 => Float32x3, 8 => Uint32, 10 => Uint32, 11 => Uint32
];

/// Instance flag bits (see `Instance::flags`).
pub const FLAG_SELECTED: u32 = 1;
pub const FLAG_HIDDEN: u32 = 2;
pub const FLAG_GHOST: u32 = 4;
pub const FLAG_DELETE: u32 = 8;
pub const FLAG_STALE: u32 = 16;
pub const FLAG_HIDDEN_GHOST: u32 = 32;
pub const FLAG_PICK_HIDDEN: u32 = 64;
/// Context part faded toward the background (opaque, no hatch).
pub const FLAG_FADE: u32 = 128;
/// Problem part drawn solid red (not hatched), so it shows through hatched blockers.
pub const FLAG_ERROR: u32 = 256;
/// Perspective edge-line pull towards the eye, as a fraction of the distance (about two pixels of
/// depth at a 30° field of view on a 1000-pixel-high view).
const LINE_PULL: f32 = 1e-3;
pub const PICK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

fn vertex_layout(stride: usize, attrs: &[wgpu::VertexAttribute]) -> wgpu::VertexBufferLayout<'_> {
    wgpu::VertexBufferLayout {
        array_stride: stride as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: attrs,
    }
}

fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<Instance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &INST_ATTRS,
    }
}

/// Byte stride of one view's globals in a multi-view uniform buffer: dynamic uniform offsets must
/// be multiples of `min_uniform_buffer_offset_alignment`, which is at most 256 on every backend.
pub const GLOBALS_STRIDE: u64 = 256;

/// Layout of bind group 0: one view's globals (bound with a dynamic offset) and the colour palette.
pub(crate) fn globals_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Globals>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// The compiled pipelines for one colour format and sample count. Compiling them costs tens of
/// milliseconds, so long-lived callers build them once and share them; clones share the handles.
#[derive(Clone, Debug)]
pub struct Pipelines {
    pub format: wgpu::TextureFormat,
    pub samples: u32,
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) opaque: wgpu::RenderPipeline,
    pub(crate) transparent: wgpu::RenderPipeline,
    pub(crate) lines: wgpu::RenderPipeline,
    pub(crate) cond: wgpu::RenderPipeline,
    pub(crate) pick: wgpu::RenderPipeline,
}

impl Pipelines {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32) -> Pipelines {
        Pipelines::with_layout(gpu, &globals_layout(&gpu.device), format, samples)
    }

    /// Pipelines over an existing bind group layout (from [`globals_layout`]), so bind groups made
    /// once can be used with pipelines for several formats.
    pub(crate) fn with_layout(
        gpu: &Gpu,
        bgl: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Pipelines {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cad shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cad"),
            bind_group_layouts: &[Some(bgl)],
            immediate_size: 0,
        });
        let make = |label: &str,
                    vs: &str,
                    fs: &str,
                    vlayout: wgpu::VertexBufferLayout,
                    topology: wgpu::PrimitiveTopology,
                    blend: Option<wgpu::BlendState>,
                    depth_write: bool,
                    fmt: wgpu::TextureFormat,
                    samples: u32| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(vlayout), Some(instance_layout())],
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: fmt,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let tri_layout = vertex_layout(size_of::<Vertex>(), &TRI_ATTRS);
        let cond_layout = vertex_layout(size_of::<CondVertex>(), &COND_ATTRS);
        let tl = wgpu::PrimitiveTopology::TriangleList;
        let ll = wgpu::PrimitiveTopology::LineList;
        let opaque = make(
            "opaque",
            "vs_tri",
            "fs_tri",
            tri_layout.clone(),
            tl,
            None,
            true,
            format,
            samples,
        );
        let pick = make(
            "pick",
            "vs_pick",
            "fs_pick",
            tri_layout.clone(),
            tl,
            None,
            true,
            PICK_FORMAT,
            1,
        );
        let transparent = make(
            "transparent",
            "vs_tri",
            "fs_tri",
            tri_layout.clone(),
            tl,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            format,
            samples,
        );
        let lines = make(
            "lines", "vs_line", "fs_line", tri_layout, ll, None, true, format, samples,
        );
        let cond = make(
            "cond",
            "vs_cond",
            "fs_line",
            cond_layout,
            ll,
            None,
            true,
            format,
            samples,
        );
        Pipelines {
            format,
            samples,
            layout: bgl.clone(),
            opaque,
            transparent,
            lines,
            cond,
            pick,
        }
    }

    /// Bind group 0 over `globals` (one view per [`GLOBALS_STRIDE`] bytes) and `palette`.
    pub(crate) fn bind_group(
        &self,
        device: &wgpu::Device,
        globals: &wgpu::Buffer,
        palette: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: globals,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<Globals>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: palette.as_entire_binding(),
                },
            ],
        })
    }
}

/// What one scene draw needs: the atlas and instance buffers and the batches over them.
pub(crate) struct DrawList<'a> {
    pub tri: &'a wgpu::Buffer,
    pub line: &'a wgpu::Buffer,
    pub cond: &'a wgpu::Buffer,
    pub inst: &'a wgpu::Buffer,
    pub meshes: &'a [MeshRange],
    pub batches: &'a [Batch],
    pub edges: bool,
}

/// Record a scene's draws into a pass whose bind group 0 is already set: opaque bodies, edge and
/// conditional lines, then transparent bodies.
pub(crate) fn draw_list(pass: &mut wgpu::RenderPass, p: &Pipelines, d: &DrawList) {
    pass.set_vertex_buffer(1, d.inst.slice(..));
    let draw = |pass: &mut wgpu::RenderPass,
                transparent: bool,
                pick: fn(&MeshRange) -> std::ops::Range<u32>| {
        for b in d.batches.iter().filter(|b| b.transparent == transparent) {
            let r = pick(&d.meshes[b.mesh as usize]);
            if !r.is_empty() && !b.instances.is_empty() {
                pass.draw(r, b.instances.clone());
            }
        }
    };
    pass.set_pipeline(&p.opaque);
    pass.set_vertex_buffer(0, d.tri.slice(..));
    draw(pass, false, |m| m.tris.clone());
    if d.edges {
        pass.set_pipeline(&p.lines);
        pass.set_vertex_buffer(0, d.line.slice(..));
        draw(pass, false, |m| m.lines.clone());
        draw(pass, true, |m| m.lines.clone());
        pass.set_pipeline(&p.cond);
        pass.set_vertex_buffer(0, d.cond.slice(..));
        draw(pass, false, |m| m.cond.clone());
        draw(pass, true, |m| m.cond.clone());
    }
    pass.set_pipeline(&p.transparent);
    pass.set_vertex_buffer(0, d.tri.slice(..));
    draw(pass, true, |m| m.tris.clone());
}

/// The per-view globals for a camera drawn into a `size` viewport.
pub(crate) fn view_globals(cam: &Camera, size: (u32, u32)) -> Globals {
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    let light = glam::Vec3::new(-0.5, -1.0, -0.7).normalize();
    Globals {
        view_proj: cam.view_proj(aspect).to_cols_array_2d(),
        eye: cam.eye().extend(1.0).to_array(),
        light: light.extend(0.0).to_array(),
        // See `pulled` in shader.wgsl.
        viewport: if cam.ortho {
            [size.0 as f32, size.1 as f32, 2e-4, 0.0]
        } else {
            [size.0 as f32, size.1 as f32, 0.0, LINE_PULL]
        },
    }
}

/// GPU copy of a [`Scene`] plus the pipelines to draw it.
pub struct Renderer {
    ground: Option<crate::ground::Ground>,
    pub samples: u32,
    globals: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipes: Pipelines,
    tri_buf: wgpu::Buffer,
    line_buf: wgpu::Buffer,
    cond_buf: wgpu::Buffer,
    inst_buf: wgpu::Buffer,
    /// CPU copy of the instance buffer, for flag updates.
    inst_cpu: Vec<Instance>,
    /// Flattened part index → its instances (body plus any instanced studs).
    by_part: Vec<Vec<u32>>,
    meshes: Vec<MeshRange>,
    batches: Vec<Batch>,
    edges: bool,
    /// Draw edge and conditional lines (when the scene has them). Viewports turn this off while the
    /// camera moves on big models: lines are about 45% of the frame.
    pub show_edges: bool,
    pub background: wgpu::Color,
}

pub(crate) fn buffer<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    // Never zero-sized: some backends reject empty vertex buffers.
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let pad = [0u8; 64];
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: if bytes.is_empty() { &pad } else { bytes },
        usage,
    })
}

impl Renderer {
    /// Compile pipelines and upload `scene`. Callers that make many renderers should compile
    /// [`Pipelines`] once and use [`Renderer::with`].
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, samples: u32, scene: &Scene) -> Renderer {
        Renderer::with(gpu, &Pipelines::new(gpu, format, samples), scene)
    }

    /// Upload `scene` and draw it with already compiled pipelines.
    pub fn with(gpu: &Gpu, pipes: &Pipelines, scene: &Scene) -> Renderer {
        let device = &gpu.device;
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let palette = buffer(
            device,
            "palette",
            &scene.palette,
            wgpu::BufferUsages::STORAGE,
        );
        let bind = pipes.bind_group(device, &globals, &palette);
        let vb = wgpu::BufferUsages::VERTEX;
        Renderer {
            ground: None,
            background: BACKGROUND,
            samples: pipes.samples,
            globals,
            bind,
            pipes: pipes.clone(),
            tri_buf: buffer(device, "tris", &scene.tri_verts, vb),
            line_buf: buffer(device, "lines", &scene.line_verts, vb),
            cond_buf: buffer(device, "cond", &scene.cond_verts, vb),
            inst_buf: buffer(
                device,
                "instances",
                &scene.instances,
                vb | wgpu::BufferUsages::COPY_DST,
            ),
            inst_cpu: scene.instances.clone(),
            by_part: {
                let mut v: Vec<Vec<u32>> = Vec::new();
                for (k, i) in scene.instances.iter().enumerate() {
                    let p = i.id as usize;
                    if v.len() <= p {
                        v.resize(p + 1, Vec::new());
                    }
                    v[p].push(k as u32);
                }
                v
            },
            meshes: scene.meshes.clone(),
            batches: scene.batches.clone(),
            edges: scene.edges,
            show_edges: true,
        }
    }

    /// Record one frame into `color` (resolving into `resolve` when multisampled).
    pub fn encode(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        resolve: Option<&wgpu::TextureView>,
        depth: &wgpu::TextureView,
        cam: &Camera,
        size: (u32, u32),
    ) {
        self.encode_pass(gpu, enc, color, resolve, depth, cam, size, false);
    }

    /// Draw a transient placement preview over a previously rendered viewport.
    pub fn encode_overlay(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        resolve: Option<&wgpu::TextureView>,
        depth: &wgpu::TextureView,
        cam: &Camera,
        size: (u32, u32),
    ) {
        self.encode_pass(gpu, enc, color, resolve, depth, cam, size, true);
    }

    /// Translate the transient scene without rebuilding meshes (including its studs).
    pub fn translate(&mut self, gpu: &Gpu, delta: glam::Vec3) {
        for instance in &mut self.inst_cpu {
            instance.t = (glam::Vec3::from_array(instance.t) + delta).to_array();
        }
        if !self.inst_cpu.is_empty() {
            gpu.queue
                .write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&self.inst_cpu));
        }
    }
    /// Apply a rigid transform to a small transient preview, including its stud instances.
    pub fn transform(&mut self, gpu: &Gpu, r: glam::Mat3, t: glam::Vec3) {
        for i in &mut self.inst_cpu {
            i.t = (r * glam::Vec3::from_array(i.t) + t).to_array();
            i.c0 = (r * glam::Vec3::from_array(i.c0)).to_array();
            i.c1 = (r * glam::Vec3::from_array(i.c1)).to_array();
            i.c2 = (r * glam::Vec3::from_array(i.c2)).to_array();
        }
        if !self.inst_cpu.is_empty() {
            gpu.queue
                .write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&self.inst_cpu));
        }
    }

    fn encode_pass(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        resolve: Option<&wgpu::TextureView>,
        depth: &wgpu::TextureView,
        cam: &Camera,
        size: (u32, u32),
        overlay: bool,
    ) {
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let g = view_globals(cam, size);
        gpu.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load: if overlay {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(self.background)
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: if overlay {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(1.0)
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(ground) = &self.ground {
            ground.draw(gpu, &mut pass, cam, aspect);
        }
        pass.set_bind_group(0, &self.bind, &[0]);
        draw_list(
            &mut pass,
            &self.pipes,
            &DrawList {
                tri: &self.tri_buf,
                line: &self.line_buf,
                cond: &self.cond_buf,
                inst: &self.inst_buf,
                meshes: &self.meshes,
                batches: &self.batches,
                edges: self.edges && self.show_edges,
            },
        );
    }
}

impl Renderer {
    /// Enable the view-only studded ground guide, sized to the build plus four studs.
    pub fn enable_ground(
        &mut self,
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        bounds: (glam::Vec3, glam::Vec3),
    ) {
        self.ground = Some(crate::ground::Ground::new(
            gpu,
            format,
            self.samples,
            bounds,
        ));
    }
    pub fn ground_radius(&self, target: glam::Vec3) -> f32 {
        self.ground
            .as_ref()
            .map_or(0.0, |ground| ground.radius(target))
    }
    pub fn set_ground_phase(&mut self, phase: glam::Vec2) {
        if let Some(ground) = &mut self.ground {
            ground.phase[0] = phase.x;
            ground.phase[1] = phase.y;
        }
    }
    /// Number of flattened parts this renderer knows about (instance ids are `0..part_count`).
    pub fn part_count(&self) -> usize {
        self.by_part.len()
    }

    /// Set the flag bits of every instance of each listed part (flattened part index), then upload.
    pub fn set_part_flags(&mut self, gpu: &Gpu, changes: impl IntoIterator<Item = (u32, u32)>) {
        let mut touched: Vec<u32> = Vec::new();
        for (part, flags) in changes {
            for &k in self
                .by_part
                .get(part as usize)
                .map_or(&[][..], Vec::as_slice)
            {
                let inst = &mut self.inst_cpu[k as usize];
                if inst.flags != flags {
                    inst.flags = flags;
                    touched.push(k);
                }
            }
        }
        if touched.is_empty() {
            return;
        }
        let stride = size_of::<Instance>() as u64;
        if touched.len() > 512 {
            gpu.queue
                .write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&self.inst_cpu));
            return;
        }
        // Coalesce runs of neighbouring instances into one write each.
        touched.sort_unstable();
        let mut i = 0;
        while i < touched.len() {
            let mut j = i + 1;
            while j < touched.len() && touched[j] == touched[j - 1] + 1 {
                j += 1;
            }
            let (a, b) = (touched[i] as usize, touched[j - 1] as usize + 1);
            gpu.queue.write_buffer(
                &self.inst_buf,
                a as u64 * stride,
                bytemuck::cast_slice(&self.inst_cpu[a..b]),
            );
            i = j;
        }
    }

    /// Clear flags on every part.
    pub fn clear_flags(&mut self, gpu: &Gpu) {
        if self.inst_cpu.iter().any(|i| i.flags != 0) {
            self.inst_cpu.iter_mut().for_each(|i| i.flags = 0);
            gpu.queue
                .write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&self.inst_cpu));
        }
    }

    /// The flattened part index under pixel `at` (top-left origin) of a `size` view, if any.
    /// Renders a 1-pixel scissored ID pass and reads it back; blocks for one GPU round trip.
    pub fn pick(
        &self,
        gpu: &Gpu,
        picker: &mut Picker,
        cam: &Camera,
        size: (u32, u32),
        at: (u32, u32),
    ) -> Option<u32> {
        if size.0 == 0 || size.1 == 0 || at.0 >= size.0 || at.1 >= size.1 {
            return None;
        }
        picker.ensure(gpu, size);
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let g = Globals {
            view_proj: cam.view_proj(aspect).to_cols_array_2d(),
            eye: cam.eye().extend(1.0).to_array(),
            light: [0.0, -1.0, 0.0, 0.0],
            viewport: [size.0 as f32, size.1 as f32, 0.0, 0.0],
        };
        gpu.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pick"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &picker.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &picker.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_scissor_rect(at.0, at.1, 1, 1);
            pass.set_bind_group(0, &self.bind, &[0]);
            pass.set_vertex_buffer(1, self.inst_buf.slice(..));
            pass.set_pipeline(&self.pipes.pick);
            pass.set_vertex_buffer(0, self.tri_buf.slice(..));
            for b in &self.batches {
                let r = self.meshes[b.mesh as usize].tris.clone();
                if !r.is_empty() {
                    pass.draw(r, b.instances.clone());
                }
            }
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &picker.tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: at.0,
                    y: at.1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &picker.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([enc.finish()]);
        let slice = picker.buf.slice(0..4);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.wait();
        let v = {
            let data = slice.get_mapped_range().expect("pick buffer is mapped");
            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
        };
        picker.buf.unmap();
        v.checked_sub(1)
    }
}

/// ID target for [`Renderer::pick`] (R32Uint, part index + 1, 0 = background).
pub struct Picker {
    size: (u32, u32),
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    buf: wgpu::Buffer,
}

impl Picker {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Picker {
        let size = (size.0.max(1), size.1.max(1));
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let mk = |label, format, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let tex = mk(
            "pick ids",
            PICK_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = mk(
            "pick depth",
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pick readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Picker {
            size,
            view: tex.create_view(&Default::default()),
            depth: depth.create_view(&Default::default()),
            tex,
            buf,
        }
    }

    fn ensure(&mut self, gpu: &Gpu, size: (u32, u32)) {
        if self.size != size {
            *self = Picker::new(gpu, size);
        }
    }
}

/// An offscreen render target with CPU readback.
pub struct Offscreen {
    pub width: u32,
    pub height: u32,
    color_view: wgpu::TextureView,
    resolve: Option<(wgpu::Texture, wgpu::TextureView)>,
    color: wgpu::Texture,
    depth_view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_row: u32,
}

impl Offscreen {
    pub fn new(gpu: &Gpu, samples: u32, width: u32, height: u32) -> Offscreen {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let tex = |label: &str, format, samples, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let ra = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let color = tex(
            "color",
            COLOR_FORMAT,
            samples,
            if samples > 1 {
                ra
            } else {
                ra | wgpu::TextureUsages::COPY_SRC
            },
        );
        let resolve = (samples > 1).then(|| {
            let t = tex(
                "resolve",
                COLOR_FORMAT,
                1,
                ra | wgpu::TextureUsages::COPY_SRC,
            );
            let v = t.create_view(&Default::default());
            (t, v)
        });
        let depth = tex("depth", DEPTH_FORMAT, samples, ra);
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: padded_row as u64 * height as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Offscreen {
            width,
            height,
            color_view: color.create_view(&Default::default()),
            resolve,
            color,
            depth_view: depth.create_view(&Default::default()),
            readback,
            padded_row,
        }
    }

    /// Draw one frame and submit it (does not wait).
    pub fn render(&self, gpu: &Gpu, r: &Renderer, cam: &Camera) -> wgpu::SubmissionIndex {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.encode(
            gpu,
            &mut enc,
            &self.color_view,
            self.resolve.as_ref().map(|(_, v)| v),
            &self.depth_view,
            cam,
            (self.width, self.height),
        );
        gpu.queue.submit([enc.finish()])
    }

    /// Copy the last frame back as tightly packed RGBA8 (sRGB).
    pub fn read_rgba(&self, gpu: &Gpu) -> Vec<u8> {
        let src = self.resolve.as_ref().map_or(&self.color, |(t, _)| t);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: src,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([enc.finish()]);
        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.wait();
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        {
            let data = slice.get_mapped_range().expect("readback buffer is mapped");
            for row in data
                .chunks(self.padded_row as usize)
                .take(self.height as usize)
            {
                out.extend_from_slice(&row[..(self.width * 4) as usize]);
            }
        }
        self.readback.unmap();
        out
    }
}

pub fn save_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(f, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    w.write_image_data(rgba).map_err(std::io::Error::other)?;
    Ok(())
}

/// Frame timings while orbiting a full turn.
pub struct OrbitTimes {
    /// Submit-to-done time of each frame, waiting for each before the next.
    pub latency_ms: Vec<f64>,
    /// Average time per frame when frames are submitted back to back.
    pub throughput_ms: f64,
}

pub fn bench_orbit(
    gpu: &Gpu,
    r: &Renderer,
    off: &Offscreen,
    cam: &Camera,
    frames: u32,
) -> OrbitTimes {
    let mut cam = *cam;
    let step = std::f32::consts::TAU / frames.max(1) as f32;
    let mut latency_ms = Vec::with_capacity(frames as usize);
    for _ in 0..frames {
        cam.yaw += step;
        let t = Instant::now();
        off.render(gpu, r, &cam);
        gpu.wait();
        latency_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let t = Instant::now();
    for _ in 0..frames {
        cam.yaw += step;
        off.render(gpu, r, &cam);
    }
    gpu.wait();
    OrbitTimes {
        latency_ms,
        throughput_ms: t.elapsed().as_secs_f64() * 1e3 / frames.max(1) as f64,
    }
}
