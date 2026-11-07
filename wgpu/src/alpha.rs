// Modified by enzUI contributors. See FORK.md.
//! Full-frame render-target pooling and compositing for subtree alpha groups.

use crate::core::{Point, Size};

use std::num::NonZeroU64;

/// Renderer-owned alpha-group resource and command counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Statistics {
    /// Alpha groups opened in the most recently rendered frame.
    pub groups: usize,
    /// Maximum simultaneous nesting depth in that frame.
    pub max_depth: usize,
    /// Group composites issued in that frame (zero-alpha groups are omitted).
    pub composites: usize,
    /// Standalone target-clear passes issued in the most recently rendered frame.
    pub clear_passes: usize,
    /// Zero-alpha composites skipped in that frame.
    pub zero_alpha_skips: usize,
    /// Passes that composited several sibling clip groups at once.
    pub batched_composite_passes: usize,
    /// Render-target textures allocated since renderer creation.
    pub target_allocations: u64,
    /// Full-frame render targets currently retained by the pool.
    pub retained_targets: usize,
    /// Logical RGBA8 bytes retained by the target pool.
    pub retained_bytes: u64,
}

/// Bytes of the mask uniform block.
const MASK_UNIFORM_BYTES: u64 = 80;

/// What lies under a group masked in place: the clear colour, and an
/// optional solid quad of the group's own shape, both premultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MaskBackdrop {
    pub clear: [f32; 4],
    pub base: [f32; 4],
    /// 1, or 2 when the base quad's layer also clips it to the same shape.
    pub base_coverage_power: f32,
}

pub(crate) struct State {
    pipeline: wgpu::RenderPipeline,
    /// Scales the target by a rounded clip's coverage (enzUI-ICED #3442).
    mask_pipeline: wgpu::RenderPipeline,
    mask_backdrop_pipeline: wgpu::RenderPipeline,
    mask_buffer: wgpu::Buffer,
    mask_bind_group: wgpu::BindGroup,
    /// A mask the next composite draws in its own pass, after compositing.
    trailing_mask: Option<MaskBackdrop>,
    texture_layout: wgpu::BindGroupLayout,
    uniform_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    scaled_sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    targets: Vec<Target>,
    uniforms: Vec<Uniform>,
    size: Size<u32>,
    uniform_cursor: usize,
    statistics: Statistics,
}

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    /// The same view through a linear sampler, for a shrunk composite
    /// (enzICED): nearest sampling drops whole rows and columns of text when
    /// a dialog scales in or out.
    scaled_bind_group: wgpu::BindGroup,
}

struct Uniform {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl State {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu alpha texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu alpha uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iced_wgpu alpha pipeline layout"),
            bind_group_layouts: &[Some(&texture_layout), Some(&uniform_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iced_wgpu alpha shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(format!(
                "{}\n{}",
                include_str!("shader/clip.wgsl"),
                include_str!("shader/alpha.wgsl")
            ))),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iced_wgpu alpha pipeline"),
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
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let mask_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu mask uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(MASK_UNIFORM_BYTES),
                },
                count: None,
            }],
        });
        let mask_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iced_wgpu mask shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(format!(
                "{}\n{}",
                include_str!("shader/clip.wgsl"),
                include_str!("shader/mask.wgsl")
            ))),
        });
        // Multiplies every channel of the target by the fragment's coverage.
        let multiply = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };
        let mask_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iced_wgpu mask pipeline layout"),
            bind_group_layouts: &[Some(&mask_layout)],
            immediate_size: 0,
        });
        let mask_pipeline_with = |entry_point: &str, blend: wgpu::BlendComponent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("iced_wgpu mask pipeline"),
                layout: Some(&mask_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &mask_shader,
                    entry_point: Some("mask_vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &mask_shader,
                    entry_point: Some(entry_point),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: blend,
                            alpha: blend,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let mask_pipeline = mask_pipeline_with("mask_multiply_fs", multiply);
        let mask_backdrop_pipeline = mask_pipeline_with(
            "mask_backdrop_fs",
            wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        );
        let mask_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iced_wgpu mask uniforms"),
            size: MASK_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mask_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu mask bind group"),
            layout: &mask_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: mask_buffer.as_entire_binding(),
            }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iced_wgpu alpha sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let scaled_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iced_wgpu alpha scaled sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            mask_pipeline,
            mask_backdrop_pipeline,
            mask_buffer,
            mask_bind_group,
            trailing_mask: None,
            texture_layout,
            uniform_layout,
            sampler,
            scaled_sampler,
            format,
            targets: Vec::new(),
            uniforms: Vec::new(),
            size: Size::new(0, 0),
            uniform_cursor: 0,
            statistics: Statistics::default(),
        }
    }

    pub fn begin_frame(&mut self, device: &wgpu::Device, size: Size<u32>, max_depth: usize) {
        if self.size != size {
            self.targets.clear();
            self.size = size;
        }

        while self.targets.len() < max_depth {
            self.targets.push(Target::new(
                device,
                &self.texture_layout,
                &self.sampler,
                &self.scaled_sampler,
                size,
                self.format,
                self.targets.len(),
            ));
            self.statistics.target_allocations += 1;
        }

        self.uniform_cursor = 0;
        self.statistics.groups = 0;
        self.statistics.max_depth = max_depth;
        self.statistics.composites = 0;
        self.statistics.batched_composite_passes = 0;
        self.statistics.clear_passes = 0;
        self.statistics.zero_alpha_skips = 0;
        self.statistics.retained_targets = self.targets.len();
        self.statistics.retained_bytes =
            self.targets.len() as u64 * u64::from(size.width) * u64::from(size.height) * 4;
    }

    pub fn target(&self, depth: usize) -> wgpu::TextureView {
        self.targets[depth].view.clone()
    }

    /// Counts a group that paints into a target already open for a sibling.
    pub fn open_shared(&mut self) {
        self.statistics.groups += 1;
    }

    pub fn open(&mut self, encoder: &mut wgpu::CommandEncoder, depth: usize, clear: bool) {
        self.statistics.groups += 1;
        if !clear {
            return;
        }
        self.statistics.clear_passes += 1;
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu alpha clear pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets[depth].view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// Turns what `render_pass`'s target holds, a rounded group drawn
    /// straight over a known `backdrop`, into the group composited over it
    /// with the coverage of `clip` (enzUI-ICED #3442).
    pub fn mask(
        &self,
        queue: &wgpu::Queue,
        render_pass: &mut wgpu::RenderPass<'_>,
        clip: crate::clip::ClipRegion,
        backdrop: &MaskBackdrop,
    ) {
        self.write_mask(queue, clip, backdrop);
        self.draw_mask(render_pass, backdrop);
    }

    /// [`Self::mask`], drawn by the next composite in its own pass once it
    /// has composited, to save a pass of its own.
    pub fn mask_after_next_composite(
        &mut self,
        queue: &wgpu::Queue,
        clip: crate::clip::ClipRegion,
        backdrop: &MaskBackdrop,
    ) {
        self.write_mask(queue, clip, backdrop);
        self.trailing_mask = Some(*backdrop);
    }

    fn write_mask(
        &self,
        queue: &wgpu::Queue,
        clip: crate::clip::ClipRegion,
        backdrop: &MaskBackdrop,
    ) {
        let mut uniforms = [0.0_f32; 20];
        uniforms[..4].copy_from_slice(&clip.rect);
        uniforms[4..8].copy_from_slice(&clip.radius);
        uniforms[8..12].copy_from_slice(&backdrop.clear);
        uniforms[12..16].copy_from_slice(&backdrop.base);
        uniforms[16] = backdrop.base_coverage_power;
        queue.write_buffer(&self.mask_buffer, 0, bytemuck::cast_slice(&uniforms));
    }

    fn draw_mask(&self, render_pass: &mut wgpu::RenderPass<'_>, backdrop: &MaskBackdrop) {
        render_pass.set_scissor_rect(0, 0, self.size.width, self.size.height);
        render_pass.set_bind_group(0, &self.mask_bind_group, &[]);
        render_pass.set_pipeline(&self.mask_pipeline);
        render_pass.draw(0..6, 0..1);
        if backdrop.clear[3] > 0.0 || backdrop.base[3] > 0.0 {
            render_pass.set_pipeline(&self.mask_backdrop_pipeline);
            render_pass.draw(0..6, 0..1);
        }
    }

    /// Composites the group at `source_depth` onto `destination`: faded,
    /// scaled and turned about `scale_origin`, clipped to `clip`, and limited
    /// to the physical `scissor` `[x, y, width, height]` when one is given.
    pub fn composite_within(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_depth: usize,
        destination: &wgpu::TextureView,
        alpha: f32,
        scale: f32,
        rotation: f32,
        scale_origin: Point,
        clip: crate::clip::ClipRegion,
        scissor: Option<[u32; 4]>,
    ) {
        if alpha <= 0.0 {
            self.statistics.zero_alpha_skips += 1;
            return;
        }

        if self.uniforms.len() <= self.uniform_cursor {
            self.uniforms.push(Uniform::new(
                device,
                &self.uniform_layout,
                self.uniforms.len(),
            ));
        }
        let uniform = &self.uniforms[self.uniform_cursor];
        self.uniform_cursor += 1;
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let rotation = if rotation.is_finite() { rotation } else { 0.0 };
        let origin_x = scale_origin.x / self.size.width.max(1) as f32;
        let origin_y = scale_origin.y / self.size.height.max(1) as f32;
        queue.write_buffer(
            &uniform.buffer,
            0,
            bytemuck::cast_slice(&[
                alpha,
                scale,
                origin_x,
                origin_y,
                clip.rect[0],
                clip.rect[1],
                clip.rect[2],
                clip.rect[3],
                clip.radius[0],
                clip.radius[1],
                clip.radius[2],
                clip.radius[3],
                rotation,
                0.0,
                0.0,
                0.0,
            ]),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu alpha composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
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
        pass.set_pipeline(&self.pipeline);
        let (mut left, mut top, mut right, mut bottom) = (0, 0, self.size.width, self.size.height);
        if clip.radius.iter().any(|radius| *radius > 0.0)
            && clip.rect.iter().all(|value| value.is_finite())
        {
            let [x, y, width, height] = clip.rect;
            // Keep the antialiasing fringe while avoiding full-frame sampling for small masks.
            left = (x.floor() - 1.0).clamp(0.0, self.size.width as f32) as u32;
            top = (y.floor() - 1.0).clamp(0.0, self.size.height as f32) as u32;
            right = ((x + width).ceil() + 1.0).clamp(0.0, self.size.width as f32) as u32;
            bottom = ((y + height).ceil() + 1.0).clamp(0.0, self.size.height as f32) as u32;
        }
        if let Some([x, y, width, height]) = scissor {
            left = left.max(x);
            top = top.max(y);
            right = right.min(x.saturating_add(width));
            bottom = bottom.min(y.saturating_add(height));
        }
        if right <= left || bottom <= top {
            return;
        }
        if (left, top, right, bottom) != (0, 0, self.size.width, self.size.height) {
            pass.set_scissor_rect(left, top, right - left, bottom - top);
        }
        let target = &self.targets[source_depth];
        // Only shrinking skips source rows; enlarging repeats them, and keeps
        // nearest so a clip edge stays exact. A turned surface falls between
        // texels and is filtered.
        let source = if scale < 1.0 || rotation != 0.0 {
            &target.scaled_bind_group
        } else {
            &target.bind_group
        };
        pass.set_bind_group(0, source, &[]);
        pass.set_bind_group(1, &uniform.bind_group, &[]);
        pass.draw(0..6, 0..1);
        if let Some(backdrop) = self.trailing_mask.take() {
            self.draw_mask(&mut pass, &backdrop);
        }
        drop(pass);

        self.statistics.composites += 1;
    }

    /// Composites several regions of the outermost target into `destination`
    /// in one pass, each at its own opacity through its own clip, which must be
    /// a rectangle the others do not overlap (enzUI-ICED #3406).
    pub fn composite_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::TextureView,
        regions: impl IntoIterator<Item = (f32, crate::clip::ClipRegion)>,
    ) {
        let mut draws = Vec::new();
        for (alpha, clip) in regions {
            if alpha <= 0.0 {
                self.statistics.zero_alpha_skips += 1;
                continue;
            }
            let [x, y, width, height] = clip.rect;
            if ![x, y, width, height].iter().all(|value| value.is_finite()) {
                continue;
            }
            let left = (x.floor() - 1.0).clamp(0.0, self.size.width as f32) as u32;
            let top = (y.floor() - 1.0).clamp(0.0, self.size.height as f32) as u32;
            let right = ((x + width).ceil() + 1.0).clamp(0.0, self.size.width as f32) as u32;
            let bottom = ((y + height).ceil() + 1.0).clamp(0.0, self.size.height as f32) as u32;
            if right <= left || bottom <= top {
                continue;
            }
            if self.uniforms.len() <= self.uniform_cursor {
                self.uniforms.push(Uniform::new(
                    device,
                    &self.uniform_layout,
                    self.uniforms.len(),
                ));
            }
            let uniform = self.uniform_cursor;
            self.uniform_cursor += 1;
            queue.write_buffer(
                &self.uniforms[uniform].buffer,
                0,
                bytemuck::cast_slice(&[
                    alpha,
                    1.0,
                    0.0,
                    0.0,
                    clip.rect[0],
                    clip.rect[1],
                    clip.rect[2],
                    clip.rect[3],
                    clip.radius[0],
                    clip.radius[1],
                    clip.radius[2],
                    clip.radius[3],
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ]),
            );
            draws.push((uniform, [left, top, right - left, bottom - top]));
        }
        if draws.is_empty() {
            return;
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu alpha batched composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.targets[0].bind_group, &[]);
        for (uniform, [x, y, width, height]) in &draws {
            pass.set_scissor_rect(*x, *y, *width, *height);
            pass.set_bind_group(1, &self.uniforms[*uniform].bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
        if let Some(backdrop) = self.trailing_mask.take() {
            self.draw_mask(&mut pass, &backdrop);
        }
        drop(pass);

        self.statistics.composites += draws.len();
        self.statistics.batched_composite_passes += 1;
    }

    pub fn statistics(&self) -> Statistics {
        self.statistics
    }
}

impl Target {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        scaled_sampler: &wgpu::Sampler,
        size: Size<u32>,
        format: wgpu::TextureFormat,
        depth: usize,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu alpha target"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("iced_wgpu alpha target {depth}")),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });

        let scaled_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("iced_wgpu alpha scaled target {depth}")),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(scaled_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });

        Self {
            _texture: texture,
            view,
            bind_group,
            scaled_bind_group,
        }
    }
}

impl Uniform {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, index: usize) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("iced_wgpu alpha uniform {index}")),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("iced_wgpu alpha uniform bind group {index}")),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Self { buffer, bind_group }
    }
}
