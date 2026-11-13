//! View-only baseplate. Never enters model geometry, picking, validation or saves.
use crate::{Camera, DEPTH_FORMAT, Gpu};

pub(crate) struct Ground {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    bounds: [f32; 4],
    pub phase: [f32; 4],
}

fn footprint(lo: glam::Vec3, hi: glam::Vec3) -> [f32; 4] {
    if !lo.is_finite() || !hi.is_finite() {
        return [-160.0, -160.0, 160.0, 160.0];
    }
    let mut bounds = [
        (lo.x / 20.0).floor() * 20.0 - 80.0,
        (lo.z / 20.0).floor() * 20.0 - 80.0,
        (hi.x / 20.0).ceil() * 20.0 + 80.0,
        (hi.z / 20.0).ceil() * 20.0 + 80.0,
    ];
    for axis in 0..2 {
        if bounds[axis + 2] - bounds[axis] < 320.0 {
            let center = ((bounds[axis] + bounds[axis + 2]) / 40.0).round() * 20.0;
            bounds[axis] = center - 160.0;
            bounds[axis + 2] = center + 160.0;
        }
    }
    bounds
}

impl Ground {
    pub fn radius(&self, target: glam::Vec3) -> f32 {
        [self.bounds[0], self.bounds[2]]
            .into_iter()
            .flat_map(|x| {
                [self.bounds[1], self.bounds[3]]
                    .into_iter()
                    .map(move |z| glam::vec3(x, self.phase[2], z))
            })
            .map(|p| p.distance(target))
            .fold(1.0, f32::max)
    }
    pub fn new(
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        samples: u32,
        bounds: (glam::Vec3, glam::Vec3),
    ) -> Self {
        let device = &gpu.device;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ground uniforms"),
            size: 96,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ground"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ground"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ground"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ground"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ground.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ghost baseplate"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind,
            uniform,
            bounds: footprint(bounds.0, bounds.1),
            phase: [
                0.0,
                0.0,
                if bounds.1.y.is_finite() {
                    bounds.1.y + 0.2
                } else {
                    0.2
                },
                0.0,
            ],
        }
    }

    pub fn draw(&self, gpu: &Gpu, pass: &mut wgpu::RenderPass<'_>, cam: &Camera, aspect: f32) {
        let mut values = cam.view_proj(aspect).to_cols_array().to_vec();
        values.extend_from_slice(&self.bounds);
        values.extend_from_slice(&self.phase);
        gpu.queue
            .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&values));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseplate_covers_whole_build_plus_four_studs() {
        assert_eq!(
            footprint(
                glam::vec3(-113.0, -80.0, 27.0),
                glam::vec3(901.0, 0.0, 247.0)
            ),
            [-200.0, -60.0, 1000.0, 340.0]
        );
        assert_eq!(
            footprint(
                glam::Vec3::splat(f32::INFINITY),
                glam::Vec3::splat(f32::NEG_INFINITY)
            ),
            [-160.0, -160.0, 160.0, 160.0]
        );
    }
}
