// Modified by enzUI contributors. See FORK.md.
use crate::{core::Color, graphics::color};
use std::num::NonZeroU64;

#[derive(Clone)]
pub(crate) struct Pipeline {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

pub(crate) struct State {
    color: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    /// The colour last written to `color`.
    written: std::cell::Cell<Option<[f32; 4]>>,
}

impl Pipeline {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("target initialization color"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(16),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("target initialization"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("target initialization"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
                r#"
@group(0) @binding(0) var<uniform> color: vec4<f32>;
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}
@fragment
fn fragment() -> @location(0) vec4<f32> {
    return color;
}
"#,
            )),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("target initialization"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { layout, pipeline }
    }
}

impl State {
    pub(crate) fn new(device: &wgpu::Device, pipeline: &Pipeline) -> Self {
        let color = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("target initialization color"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("target initialization color"),
            layout: &pipeline.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: color.as_entire_binding(),
            }],
        });
        Self {
            color,
            bindings,
            written: std::cell::Cell::new(None),
        }
    }

    pub(crate) fn prepare(
        &self,
        belt: &mut wgpu::util::StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        color: Color,
    ) {
        let [r, g, b, a] = color::pack(color).components();
        let value = [r * a, g * a, b * a, a];
        // The colour stays in its buffer from one frame to the next.
        if self.written.get() == Some(value) {
            return;
        }
        belt.write_buffer(encoder, &self.color, 0, NonZeroU64::new(16).unwrap())
            .copy_from_slice(bytemuck::cast_slice(&value));
        self.written.set(Some(value));
    }

    pub(crate) fn render<'a>(&'a self, pipeline: &'a Pipeline, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &self.bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}
