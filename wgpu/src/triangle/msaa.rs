// Modified by enzUI contributors. See FORK.md.
use crate::core::{Rectangle, Size, Transformation};
use crate::graphics;

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Distinct target sizes kept alive at once; the least recently used goes first.
const TARGET_BUCKETS: usize = 8;

#[derive(Debug, Clone)]
pub struct Pipeline {
    format: wgpu::TextureFormat,
    sampler: wgpu::Sampler,
    raw: wgpu::RenderPipeline,
    constant_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sample_count: u32,
    /// Multisample targets by bucketed size. A mesh layer draws into a target
    /// the size of its own bounds, so the clear, the resolve and the blit cost
    /// its area rather than the whole window's.
    targets: Arc<Mutex<Vec<(u64, Targets)>>>,
    uses: Arc<AtomicU64>,
}

impl Pipeline {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        antialiasing: graphics::Antialiasing,
    ) -> Pipeline {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

        let constant_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu::triangle:msaa uniforms layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
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

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu::triangle::msaa texture layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iced_wgpu::triangle::msaa pipeline layout"),
            bind_group_layouts: &[Some(&constant_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iced_wgpu triangle blit_shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "../shader/blit.wgsl"
            ))),
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iced_wgpu::triangle::msaa pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Self {
            format,
            sampler,
            raw: pipeline,
            constant_layout,
            texture_layout,
            sample_count: antialiasing.sample_count(),
            targets: Arc::new(Mutex::new(Vec::new())),
            uses: Arc::new(AtomicU64::new(0)),
        }
    }

    fn targets(&self, device: &wgpu::Device, region_size: Size<u32>) -> Targets {
        let size = bucket(region_size);
        let used = self.uses.fetch_add(1, Ordering::Relaxed);
        let mut targets = self.targets.lock().expect("Lock MSAA targets");

        if let Some((last_used, targets)) =
            targets.iter_mut().find(|(_, targets)| targets.size == size)
        {
            *last_used = used;
            return targets.clone();
        }

        if targets.len() == TARGET_BUCKETS
            && let Some(oldest) = targets
                .iter()
                .enumerate()
                .min_by_key(|(_, (last_used, _))| *last_used)
                .map(|(index, _)| index)
        {
            let _ = targets.swap_remove(oldest);
        }

        let created = Targets::new(
            device,
            self.format,
            &self.texture_layout,
            self.sample_count,
            size,
        );
        targets.push((used, created.clone()));
        created
    }
}

impl Pipeline {
    /// The sizes of the multisample targets alive now.
    #[cfg(test)]
    pub(super) fn target_sizes(&self) -> Vec<Size<u32>> {
        self.targets
            .lock()
            .expect("Lock MSAA targets")
            .iter()
            .map(|(_, targets)| targets.size)
            .collect()
    }
}

/// The target size for a region: a power of two up to 256 pixels, a multiple
/// of 256 above it, so nearby sizes share a target.
fn bucket(region: Size<u32>) -> Size<u32> {
    fn round(extent: u32) -> u32 {
        if extent <= 256 {
            extent.max(64).next_power_of_two()
        } else {
            extent.div_ceil(256) * 256
        }
    }

    Size::new(round(region.width), round(region.height))
}

#[derive(Debug, Clone)]
struct Targets {
    attachment: wgpu::TextureView,
    resolve: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: Size<u32>,
}

impl Targets {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        texture_layout: &wgpu::BindGroupLayout,
        sample_count: u32,
        size: Size<u32>,
    ) -> Targets {
        let extent = wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        };

        let attachment = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu::triangle::msaa attachment"),
            size: extent,
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu::triangle::msaa resolve target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let attachment = attachment.create_view(&wgpu::TextureViewDescriptor::default());

        let resolve = resolve.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu::triangle::msaa texture bind group"),
            layout: texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&resolve),
            }],
        });

        Targets {
            attachment,
            resolve,
            bind_group,
            size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Ratio {
    u: f32,
    v: f32,
    // Padding field for 16-byte alignment.
    // See https://docs.rs/wgpu/latest/wgpu/struct.DownlevelFlags.html#associatedconstant.BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED
    _padding: [f32; 2],
}

/// The multisample targets and blit uniforms of one mesh batch.
struct Batch {
    ratio: wgpu::Buffer,
    constants: wgpu::BindGroup,
    last_ratio: Option<Ratio>,
    region: Rectangle<u32>,
    targets: Option<Targets>,
}

/// Per-batch multisample state, indexed by the batch's position among the
/// frame's mesh batches. A reused frame renders the batches it prepared.
pub struct State {
    batches: Vec<Batch>,
}

impl State {
    pub fn new() -> Self {
        Self {
            batches: Vec::new(),
        }
    }

    /// Prepares batch `index` to draw the meshes inside `region`, in physical
    /// pixels of the frame, and returns the projection of those meshes into
    /// its target.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        pipeline: &Pipeline,
        index: usize,
        region: Rectangle<u32>,
    ) -> Transformation {
        while self.batches.len() <= index {
            let ratio = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu::triangle::msaa ratio"),
                size: std::mem::size_of::<Ratio>() as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });

            let constants = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iced_wgpu::triangle::msaa uniforms bind group"),
                layout: &pipeline.constant_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: ratio.as_entire_binding(),
                    },
                ],
            });

            self.batches.push(Batch {
                ratio,
                constants,
                last_ratio: None,
                region: Rectangle::default(),
                targets: None,
            });
        }

        let batch = &mut self.batches[index];
        let region_size = Size::new(region.width, region.height);
        let targets = pipeline.targets(device, region_size);

        let ratio = Ratio {
            u: region_size.width as f32 / targets.size.width as f32,
            v: region_size.height as f32 / targets.size.height as f32,
            _padding: [0.0; 2],
        };

        if Some(ratio) != batch.last_ratio {
            belt.write_buffer(
                encoder,
                &batch.ratio,
                0,
                NonZeroU64::new(std::mem::size_of::<Ratio>() as u64).expect("non-empty ratio"),
            )
            .copy_from_slice(bytemuck::bytes_of(&ratio));

            batch.last_ratio = Some(ratio);
        }

        let projection = Transformation::orthographic(targets.size.width, targets.size.height)
            * Transformation::translate(-(region.x as f32), -(region.y as f32));

        batch.region = region;
        batch.targets = Some(targets);

        projection
    }

    /// The frame position of batch `index`'s target origin.
    pub fn origin(&self, index: usize) -> (u32, u32) {
        self.batches
            .get(index)
            .map_or((0, 0), |batch| (batch.region.x, batch.region.y))
    }

    /// The size of batch `index`'s target.
    pub fn target_size(&self, index: usize) -> Size<u32> {
        self.batches
            .get(index)
            .and_then(|batch| batch.targets.as_ref())
            .map_or(Size::new(0, 0), |targets| targets.size)
    }

    pub fn render_pass<'a>(
        &self,
        index: usize,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> wgpu::RenderPass<'a> {
        let targets = self.batches[index]
            .targets
            .as_ref()
            .expect("MSAA batch prepared before render");

        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu.triangle.render_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.attachment,
                depth_slice: None,
                resolve_target: Some(&targets.resolve),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// Composites batch `index` onto `target`, inside its region only.
    pub fn render(
        &self,
        pipeline: &Pipeline,
        index: usize,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        let batch = &self.batches[index];
        let targets = batch
            .targets
            .as_ref()
            .expect("MSAA batch prepared before render");

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu::triangle::msaa render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        // The blit covers its viewport, which is the region the batch drew.
        render_pass.set_viewport(
            batch.region.x as f32,
            batch.region.y as f32,
            batch.region.width as f32,
            batch.region.height as f32,
            0.0,
            1.0,
        );
        render_pass.set_pipeline(&pipeline.raw);
        render_pass.set_bind_group(0, &batch.constants, &[]);
        render_pass.set_bind_group(1, &targets.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
