// Modified by enzUI contributors. See FORK.md.
//! Sampled-backdrop material passes: an offscreen scene, one bounded shared
//! source, a separable filter, and one composite per region (enzUI-ICED #2950).
//!
//! # Why an offscreen scene exists
//!
//! A swapchain view is render-attachment only, and no texture can be sampled
//! while it is the attachment of the pass that reads it. A frame containing any
//! material is therefore rendered into a texture this module owns and blitted
//! back at the end. A frame with no material allocates nothing, clears nothing
//! and blits nothing: the historical direct path is taken exactly.
//!
//! # What one ordered boundary costs
//!
//! At a material layer the backend has already ended its render pass. This
//! module copies the *padded union* of that boundary's regions out of the
//! scene, filters the copy once per distinct sigma, and composites each region
//! over the scene. Regions sharing a boundary share both the copy and the
//! filter, so a toolbar of five glass buttons is one copy and one filter pair,
//! not five.
//!
//! # What it refuses
//!
//! An unsupported target format is refused *before* drawing, through
//! [`State::support`], so widgets paint opaque panels instead. A boundary that
//! cannot resolve a source after drawing — a zero-area rectangle, or a scene
//! that could not be allocated — still composites, with [`Uniform`]'s fallback
//! flag set, filling the region with its own colour. Neither path can leave a
//! hole, and neither can report a blur that did not happen.
//!
//! # Inside an alpha group
//!
//! A region inside an offscreen alpha group (a fade, say) sits over two
//! things: the scene beneath the group, and what the group has painted so
//! far, which is still in its own target. The boundary copies the scene and
//! composites each open group over that copy at its opacity before filtering
//! ([`Backdrop::Groups`]), so the glass shows the backdrop it will show once
//! the group ends, apart from the group's own opacity (enzUI-ICED #3408).

use crate::core::material::{Material, Uniform};
use crate::core::{Point, Rectangle, Size, Vector};

/// Material resource and command counts for the most recent frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Statistics {
    /// Regions composited in that frame.
    pub regions: usize,
    /// Ordered boundaries resolved in that frame.
    pub boundaries: usize,
    /// Source rectangles copied out of the scene.
    pub source_resolves: usize,
    /// Filter passes issued: two per distinct sigma.
    pub filter_passes: usize,
    /// Regions composited from a filtered source.
    pub composites: usize,
    /// Regions composited as an explicit opaque fallback.
    pub fallback_composites: usize,
    /// Scene blits issued; at most one per frame.
    pub blits: usize,
    /// Render passes this state began: the scene clear, filters, probes and
    /// the blit. Composites draw in the frame's own pass and add none.
    pub render_passes: usize,
    /// Textures allocated since renderer creation.
    pub target_allocations: u64,
    /// Full-frame textures currently retained.
    pub retained_targets: usize,
    /// Logical RGBA8 bytes retained by those textures.
    pub retained_bytes: u64,
}

/// The largest blur sigma, in logical pixels, this backend spends a pass on.
const MAX_BLUR_SIGMA: f32 = 32.0;

/// Taps one filter pass will spend to each side, in physical pixels.
///
/// Three sigma covers 99.7% of the kernel; the cap bounds a single pass on a
/// scaled display, where a 32-logical-pixel sigma would otherwise ask for 192
/// taps per axis.
const MAX_FILTER_RADIUS: f32 = 64.0;

/// The tap radius below which a filter pass is an identity copy and is skipped.
///
/// The Gaussian's outermost tap weight at `radius = 3 * sigma` is
/// `exp(-4.5) = 0.011`, so a sigma whose whole reach is under one texel moves
/// no visible energy: the backend binds the unfiltered source into the blurred
/// slot instead of spending two full passes to reproduce it. The shader takes
/// the same view below `optics.w < 0.001`, and because the slot it binds *is*
/// the source, the two thresholds cannot disagree about pixels.
const MIN_FILTER_REACH: f32 = 1.0;

/// The filter uniform's size in bytes: axis, sigma and radius, then the
/// inclusive texel bounds of the rectangle the backend copied.
const FILTER_UNIFORM_SIZE: u64 = 32;

/// Distinct sigmas one boundary may filter at before regions start sharing the
/// nearest filtered target.
const MAX_FILTERED: usize = 2;

/// Pool slot holding the copy of the scene.
const SOURCE: usize = 0;

/// What a boundary's regions sample.
pub enum Backdrop<'a> {
    /// The scene holds everything painted beneath the boundary.
    Scene,
    /// Open alpha groups hold part of it. The callback composites their
    /// content so far over the copied source, within the given physical
    /// scissor `[x, y, width, height]`, before it is filtered.
    Groups(&'a mut dyn FnMut(&mut wgpu::CommandEncoder, &wgpu::TextureView, [u32; 4])),
    /// Nothing beneath the boundary can be sampled.
    Unavailable,
}

/// Pool slot holding the first filter axis.
const SCRATCH: usize = 1;

/// First pool slot holding a finished filtered source.
const FILTERED: usize = 2;

/// Most materials one frame measures the backdrop brightness of.
const MAX_PROBES: u32 = 256;

/// Frames a measured brightness is kept without being measured again.
const PROBE_RETENTION: u64 = 120;

/// The probe atlas's format: plain 8-bit channels, so a written value is the
/// value read back, whatever the frame's own format does.
const PROBE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One material region recorded while drawing, in logical pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Region {
    /// The region's rectangle, after the transformation in effect.
    pub bounds: Rectangle,
    /// The parameters to composite it with.
    pub material: Material,
}

impl Region {
    /// Moves a recorded region with the retained paint it belongs to.
    pub fn translate(&mut self, translation: Vector) {
        self.bounds += translation;
    }
}

/// Whether a target format can hold a sampled backdrop.
///
/// An allowlist rather than a feature query: these four are the formats this
/// backend already composites alpha groups in, and each is renderable, copyable
/// and filterable wherever wgpu runs. Anything else is refused rather than
/// attempted.
fn format_supported(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb
    )
}

pub(crate) struct State {
    format: wgpu::TextureFormat,
    pipelines: Option<Pipelines>,
    size: Size<u32>,
    scene: Option<Target>,
    /// The frame's own target when it can be copied from: layers render
    /// straight into it and boundaries copy their backdrop out of it, so no
    /// scene is allocated, cleared or blitted back.
    direct: Option<wgpu::Texture>,
    targets: Vec<Target>,
    uniforms: Vec<wgpu::Buffer>,
    filter_uniforms: Vec<wgpu::Buffer>,
    uniform_cursor: usize,
    filter_cursor: usize,
    /// The rectangle copied out of the scene at the current boundary.
    source: Option<crate::core::Rectangle<u32>>,
    /// Filtered slots produced at the current boundary, by sigma.
    filtered: Vec<(f32, usize)>,
    /// The current boundary's composites, drawn by [`State::draw_composites`]
    /// into the pass that resumes the frame.
    composites: Vec<(wgpu::BindGroup, crate::core::Rectangle<u32>)>,
    statistics: Statistics,
    /// Backdrop-brightness measurement: one atlas texel per probing material
    /// this frame, copied to a staging buffer and read back a frame or two
    /// later, never waited for.
    probes: Probes,
    waker: Option<crate::core::shell::Waker>,
}

/// The backdrop-brightness measurement's resources and results.
#[derive(Default)]
struct Probes {
    atlas: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// Probe keys measured this frame, in atlas order.
    frame: Vec<u64>,
    staging: Vec<Staging>,
    // The source frame prevents delayed readbacks from replacing newer measurements.
    results: std::collections::HashMap<u64, (f32, u64)>,
    frame_number: u64,
}

struct Staging {
    buffer: wgpu::Buffer,
    keys: Vec<u64>,
    frame: u64,
    phase: StagingPhase,
}

enum StagingPhase {
    Idle,
    /// The copy was recorded; it is mapped once its submission has gone out.
    Copied,
    Mapping(std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>),
}

struct Pipelines {
    probe: wgpu::RenderPipeline,
    filter: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    filter_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl State {
    /// Creates the state. Nothing is allocated on the GPU until a frame
    /// actually contains a material.
    pub fn new(format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            pipelines: None,
            size: Size::new(0, 0),
            scene: None,
            direct: None,
            targets: Vec::new(),
            uniforms: Vec::new(),
            filter_uniforms: Vec::new(),
            uniform_cursor: 0,
            filter_cursor: 0,
            source: None,
            filtered: Vec::new(),
            composites: Vec::new(),
            statistics: Statistics::default(),
            probes: Probes::default(),
            waker: None,
        }
    }

    /// The latest measured backdrop brightness for `probe`, if any.
    pub fn luminance(&self, probe: u64) -> Option<f32> {
        self.probes.results.get(&probe).map(|(value, _)| *value)
    }

    pub fn luminance_with_frame(&self, probe: u64) -> Option<(f32, u64)> {
        self.probes.results.get(&probe).copied()
    }

    pub fn frame(&self) -> u64 {
        self.probes.frame_number
    }

    pub fn set_waker(&mut self, waker: crate::core::shell::Waker) {
        self.waker = Some(waker);
    }

    pub fn request_redraw(&self) {
        if let Some(waker) = &self.waker {
            waker.wake();
        }
    }

    /// Collects finished readbacks and maps the copies the previous frame
    /// submitted. Never blocks.
    fn collect_probes(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        self.probes.frame_number += 1;
        let frame = self.probes.frame_number;

        for staging in &mut self.probes.staging {
            match std::mem::replace(&mut staging.phase, StagingPhase::Idle) {
                StagingPhase::Idle => {}
                StagingPhase::Copied => {
                    let (send, ready) = std::sync::mpsc::sync_channel(1);
                    staging
                        .buffer
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            let _ = send.try_send(result);
                        });
                    staging.phase = StagingPhase::Mapping(ready);
                }
                StagingPhase::Mapping(ready) => match ready.try_recv() {
                    Ok(Ok(())) => {
                        {
                            let bytes = staging.buffer.slice(..).get_mapped_range();
                            for (index, key) in staging.keys.iter().enumerate() {
                                let value = f32::from(bytes[index * 4]) / 255.0;
                                let previous = self
                                    .probes
                                    .results
                                    .entry(*key)
                                    .or_insert((value, staging.frame));
                                if staging.frame >= previous.1 {
                                    *previous = (value, staging.frame);
                                }
                            }
                        }
                        staging.buffer.unmap();
                    }
                    Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        staging.phase = StagingPhase::Mapping(ready);
                    }
                },
            }
        }

        self.probes
            .results
            .retain(|_, (_, seen)| frame.saturating_sub(*seen) <= PROBE_RETENTION);
        self.probes.frame.clear();
    }

    /// Measures the backdrop under the region `uniform` describes into the
    /// next atlas texel.
    fn probe(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        uniform: usize,
        filtered: (usize, usize),
        key: u64,
    ) {
        let index = self.probes.frame.len() as u32;
        let (Some(pipelines), Some(buffer)) = (self.pipelines.as_ref(), self.uniforms.get(uniform))
        else {
            return;
        };
        if index >= MAX_PROBES || self.targets.len() <= filtered.0.max(filtered.1) {
            return;
        }

        if self.probes.atlas.is_none() {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("iced_wgpu material probe atlas"),
                size: wgpu::Extent3d {
                    width: MAX_PROBES,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PROBE_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.probes.atlas = Some((texture, view));
        }
        let Some((_, atlas)) = self.probes.atlas.as_ref() else {
            return;
        };

        let bind_group = pipelines.composite_bind_group(
            device,
            &self.targets[SOURCE].view,
            &self.targets[filtered.0].view,
            &self.targets[filtered.1].view,
            buffer,
        );
        self.statistics.render_passes += 1;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu material probe pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: atlas,
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
        pass.set_pipeline(&pipelines.probe);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_viewport(index as f32, 0.0, 1.0, 1.0, 0.0, 1.0);
        pass.set_scissor_rect(index, 0, 1, 1);
        pass.draw(0..3, 0..1);
        drop(pass);

        self.probes.frame.push(key);
    }

    /// Copies this frame's measurements toward the CPU through an idle
    /// staging buffer; a frame with none free skips its copy.
    fn submit_probes(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let count = self.probes.frame.len() as u32;
        let Some((atlas, _)) = self.probes.atlas.as_ref() else {
            return;
        };
        if count == 0 {
            return;
        }

        let slot = match self
            .probes
            .staging
            .iter()
            .position(|staging| matches!(staging.phase, StagingPhase::Idle))
        {
            Some(slot) => slot,
            None if self.probes.staging.len() < 3 => {
                self.probes.staging.push(Staging {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("iced_wgpu material probe staging"),
                        size: u64::from(MAX_PROBES) * 4,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    keys: Vec::new(),
                    frame: 0,
                    phase: StagingPhase::Idle,
                });
                self.probes.staging.len() - 1
            }
            None => return,
        };
        let staging = &mut self.probes.staging[slot];

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: atlas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(MAX_PROBES * 4),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: count,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        staging.keys = self.probes.frame.clone();
        staging.frame = self.probes.frame_number;
        staging.phase = StagingPhase::Copied;
    }

    /// What this backend can do with a material, given its target format.
    pub fn support(&self) -> crate::core::material::Support {
        if format_supported(self.format) {
            crate::core::material::Support::Optical {
                max_blur_sigma: MAX_BLUR_SIGMA,
            }
        } else {
            crate::core::material::Support::Unsupported
        }
    }

    /// Opens the frame.
    ///
    /// Returns the view every layer must be rendered into when this frame has a
    /// material, or `None` when it has none and must take the direct path.
    ///
    /// With `direct`, the frame's own target, and when it can be copied from
    /// and matches the frame, no scene is used: the layers render into the
    /// frame and [`is_direct`](Self::is_direct) is `true`.
    pub fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        size: Size<u32>,
        needed: bool,
        direct: Option<&wgpu::Texture>,
    ) -> Option<wgpu::TextureView> {
        self.direct = None;
        self.collect_probes(device);
        self.statistics.regions = 0;
        self.statistics.boundaries = 0;
        self.statistics.source_resolves = 0;
        self.statistics.filter_passes = 0;
        self.statistics.composites = 0;
        self.statistics.fallback_composites = 0;
        self.statistics.blits = 0;
        self.statistics.render_passes = 0;
        self.uniform_cursor = 0;
        self.filter_cursor = 0;
        self.source = None;
        self.filtered.clear();

        if self.size != size {
            self.scene = None;
            self.targets.clear();
            self.size = size;
        }

        if !needed || size.width == 0 || size.height == 0 || !format_supported(self.format) {
            // A frame without a material holds no material memory: the targets
            // are released rather than kept warm for a panel that may never
            // come back.
            self.scene = None;
            self.targets.clear();
            self.account();

            return None;
        }

        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device, self.format));
        }

        if let Some(texture) = direct.filter(|texture| {
            texture.usage().contains(wgpu::TextureUsages::COPY_SRC)
                && texture.format() == self.format
                && (texture.width(), texture.height()) == (size.width, size.height)
        }) {
            self.scene = None;
            self.direct = Some(texture.clone());
            self.account();

            return None;
        }

        if self.scene.is_none() {
            self.scene = Some(self.allocate(device, true));
        }

        let view = self.scene.as_ref()?.view.clone();

        // The scene starts empty and is blended back over the frame, so the
        // frame's own prior content survives exactly as it did when every
        // primitive blended into it directly.
        self.statistics.render_passes += 1;
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu material scene clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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

        self.account();

        Some(view)
    }

    /// Resolves one ordered boundary: copy the shared source, filter it, and
    /// queue a composite for every region of the boundary. The caller draws
    /// them with [`State::draw_composites`] as the first paint of the pass it
    /// resumes, so a boundary costs no pass of its own for them; a pass is two
    /// command buffers in wgpu-core (enzUI-ICED #3244).
    ///
    /// `backdrop` says where the content painted beneath the regions is; with
    /// [`Backdrop::Unavailable`] every region takes its opaque fallback instead
    /// of sampling pixels that are not its backdrop.
    pub fn boundary(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        regions: &[Region],
        clip: Rectangle,
        scale: f32,
        backdrop: Backdrop<'_>,
    ) {
        if regions.is_empty() {
            return;
        }

        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let frame = Rectangle::new(
            Point::ORIGIN,
            Size::new(self.size.width as f32, self.size.height as f32),
        );

        // A declared region with no visible texel costs nothing: no boundary,
        // no source, no pass, no composite.
        if !any_visible(regions, clip, scale, frame) {
            return;
        }

        self.statistics.boundaries += 1;
        self.source = None;
        self.filtered.clear();
        self.composites.clear();

        let logical = Size::new(frame.width / scale, frame.height / scale);

        let source = if matches!(backdrop, Backdrop::Unavailable) {
            None
        } else {
            self.source_rect(regions, scale, frame)
                .filter(|_| {
                    self.pipelines.is_some() && (self.scene.is_some() || self.direct.is_some())
                })
                .filter(|rect| self.resolve(device, encoder, *rect))
        };

        if let (Some(_), Backdrop::Groups(composite), Some(rect)) = (source, backdrop, self.source)
        {
            composite(
                encoder,
                &self.targets[SOURCE].view,
                [rect.x, rect.y, rect.width, rect.height],
            );
        }

        for region in regions {
            let Some(scissor) = scissor(region, clip, scale, frame) else {
                continue;
            };

            self.statistics.regions += 1;

            let filtered = source.and_then(|_| {
                let blur = self.filtered_slot(device, queue, encoder, region, scale, false)?;
                let diffused = self
                    .filtered_slot(device, queue, encoder, region, scale, true)
                    .unwrap_or(blur);

                Some((blur, diffused))
            });

            let uniform = Uniform::new(
                &region.material,
                region.bounds,
                logical,
                Point::ORIGIN,
                scale,
                filtered.is_none(),
            );

            // A measure-only region writes its uniform for the probe but
            // draws nothing.
            self.composite(
                device,
                queue,
                uniform,
                filtered,
                scissor,
                !region.material.measure_only,
            );

            if region.material.probe != 0 {
                match filtered {
                    Some(filtered) => {
                        let uniform = self.uniform_cursor.saturating_sub(1);
                        self.probe(device, encoder, uniform, filtered, region.material.probe);
                    }
                    // Nothing to sample: the region is its opaque fallback, so
                    // that fill is what its content sits on.
                    None => {
                        let tint = region.material.tint;
                        let encoded = |c: f32| {
                            let c = c.clamp(0.0, 1.0);
                            if c <= 0.003_130_8 {
                                c * 12.92
                            } else {
                                1.055 * c.powf(1.0 / 2.4) - 0.055
                            }
                        };
                        let luminance = 0.299 * encoded(tint.r)
                            + 0.587 * encoded(tint.g)
                            + 0.114 * encoded(tint.b);
                        let _ = self
                            .probes
                            .results
                            .insert(region.material.probe, (luminance, self.probes.frame_number));
                    }
                }
            }
        }

        self.source = None;
        self.filtered.clear();
    }

    /// Draws the composites the last boundary queued, over whatever `pass`
    /// renders to: the target the boundary was resolved for.
    pub fn draw_composites(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(pipelines) = self.pipelines.as_ref() else {
            self.composites.clear();
            return;
        };

        if self.composites.is_empty() {
            return;
        }

        pass.set_pipeline(&pipelines.composite);

        for (bind_group, scissor) in self.composites.drain(..) {
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
            pass.draw(0..6, 0..1);
        }
    }

    /// Whether this frame renders straight into its target (see
    /// [`begin_frame`](Self::begin_frame)).
    pub fn is_direct(&self) -> bool {
        self.direct.is_some()
    }

    /// Ends a frame rendered straight into its target: there is no scene to
    /// return, only the probes to submit.
    pub fn end_direct_frame(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        self.submit_probes(device, encoder);
        self.direct = None;
    }

    /// Returns the offscreen scene to the frame.
    pub fn end_frame(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
    ) {
        self.submit_probes(device, encoder);

        // The blit reads no uniform, but its bind group layout is the filter's,
        // so one buffer must exist even in a frame whose every region fell back
        // to opaque. Skipping the blit instead would drop the whole frame.
        if self.filter_uniforms.is_empty() {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu material filter uniform"),
                size: FILTER_UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.filter_uniforms.push(buffer);
        }

        let (Some(pipelines), Some(scene), Some(uniform)) = (
            self.pipelines.as_ref(),
            self.scene.as_ref(),
            self.filter_uniforms.first(),
        ) else {
            return;
        };

        let bind_group = pipelines.filter_bind_group(device, &scene.view, uniform);

        self.statistics.render_passes += 1;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu material scene blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame,
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

        pass.set_pipeline(&pipelines.blit);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);

        self.statistics.blits += 1;
    }

    /// The counts of the most recent frame.
    pub fn statistics(&self) -> Statistics {
        self.statistics
    }

    /// The padded union of a boundary's regions, in physical pixels, clamped to
    /// the frame.
    ///
    /// The sigma added on top of [`Material::sample_padding`] is what keeps the
    /// second filter axis reading texels the first axis actually wrote.
    fn source_rect(&self, regions: &[Region], scale: f32, frame: Rectangle) -> Option<Rectangle> {
        let mut union: Option<Rectangle> = None;

        for region in regions {
            let size = region.bounds.size();

            if size.width <= 0.0 || size.height <= 0.0 {
                continue;
            }

            let padding =
                (region.material.sample_padding(size) + 3.0 * region.material.blur_sigma) * scale;
            let bounds = physical(region.bounds, scale);
            let padded = Rectangle {
                x: bounds.x - padding,
                y: bounds.y - padding,
                width: bounds.width + 2.0 * padding,
                height: bounds.height + 2.0 * padding,
            };

            union = Some(match union {
                Some(current) => current.union(&padded),
                None => padded,
            });
        }

        union?
            .intersection(&frame)
            .filter(|rect| rect.width >= 1.0 && rect.height >= 1.0)
    }

    /// Copies the boundary's shared source out of the scene.
    fn resolve(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: Rectangle,
    ) -> bool {
        let Some(rect) = snap(source) else {
            return false;
        };

        while self.targets.len() < FILTERED + MAX_FILTERED {
            let target = self.allocate(device, false);
            self.targets.push(target);
        }

        self.account();

        let Some(scene) = self
            .scene
            .as_ref()
            .map(|scene| &scene.texture)
            .or(self.direct.as_ref())
        else {
            return false;
        };

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: scene,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::default(),
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.targets[SOURCE].texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::default(),
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );

        self.statistics.source_resolves += 1;
        self.source = Some(rect);

        true
    }

    /// The pool slot holding this region's blurred (or diffused) source,
    /// filtering it first if this boundary has not filtered that sigma yet.
    ///
    /// Returns the unfiltered source slot for a sigma whose reach is under one
    /// texel, so an unblurred material spends no pass. Returns `None` for a
    /// diffusion the composite can read from the blurred target instead, and
    /// when the boundary has already spent its filtered slots — regions then
    /// share the nearest sigma rather than growing the pool per region.
    fn filtered_slot(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        region: &Region,
        scale: f32,
        diffusion: bool,
    ) -> Option<usize> {
        let sigma = if diffusion {
            region.material.diffusion_sigma()?
        } else {
            region.material.blur_sigma
        };
        let sigma = sigma.clamp(0.0, MAX_BLUR_SIGMA) * scale;

        // Below one texel of reach the filter is an identity copy, so the
        // source is the answer. Checked before the slot cache, or an
        // unblurred region arriving after the slots are spent would be given a
        // neighbour's blur.
        if !filters(sigma) {
            let _ = self.source?;

            return Some(SOURCE);
        }

        if let Some((_, slot)) = self
            .filtered
            .iter()
            .copied()
            .find(|(existing, _)| (existing - sigma).abs() < 0.05)
        {
            return Some(slot);
        }

        if self.filtered.len() >= MAX_FILTERED {
            // Out of filtered slots: share the nearest sigma already spent
            // rather than allocating per region.
            return self
                .filtered
                .iter()
                .copied()
                .min_by(|(a, _), (b, _)| {
                    (a - sigma)
                        .abs()
                        .partial_cmp(&(b - sigma).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(_, slot)| slot);
        }

        let slot = FILTERED + self.filtered.len();
        let source = self.source?;

        self.filter(device, queue, encoder, SOURCE, SCRATCH, sigma, true, source);
        self.filter(device, queue, encoder, SCRATCH, slot, sigma, false, source);
        self.filtered.push((sigma, slot));

        Some(slot)
    }

    /// One axis of the separable Gaussian, scissored to the copied rectangle.
    #[allow(clippy::too_many_arguments)]
    fn filter(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: usize,
        output: usize,
        sigma: f32,
        horizontal: bool,
        source: crate::core::Rectangle<u32>,
    ) {
        let Some(pipelines) = self.pipelines.as_ref() else {
            return;
        };

        if self.filter_uniforms.len() <= self.filter_cursor {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu material filter uniform"),
                size: FILTER_UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.filter_uniforms.push(buffer);
        }

        let uniform = &self.filter_uniforms[self.filter_cursor];
        self.filter_cursor += 1;

        let radius = (3.0 * sigma).ceil().clamp(0.0, MAX_FILTER_RADIUS);
        let direction = if horizontal { [1.0, 0.0] } else { [0.0, 1.0] };

        queue.write_buffer(
            uniform,
            0,
            bytemuck::cast_slice(&filter_uniform(direction, sigma, radius, source)),
        );

        let bind_group = pipelines.filter_bind_group(device, &self.targets[input].view, uniform);

        self.statistics.render_passes += 1;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iced_wgpu material filter pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets[output].view,
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

        pass.set_pipeline(&pipelines.filter);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_scissor_rect(source.x, source.y, source.width, source.height);
        pass.draw(0..3, 0..1);
        drop(pass);

        self.statistics.filter_passes += 1;
    }

    /// Queues one region's composite.
    fn composite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uniform: Uniform,
        filtered: Option<(usize, usize)>,
        scissor: crate::core::Rectangle<u32>,
        draw: bool,
    ) {
        let Some(pipelines) = self.pipelines.as_ref() else {
            return;
        };

        if self.uniforms.len() <= self.uniform_cursor {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu material uniform"),
                size: Uniform::SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.uniforms.push(buffer);
        }

        let buffer = &self.uniforms[self.uniform_cursor];
        self.uniform_cursor += 1;

        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&uniform.floats()));

        if !draw {
            return;
        }

        // A fallback composite reads no texture at all, but a bind group still
        // needs three views: the scene stands in for all three.
        let views = match filtered {
            Some((blurred, diffused)) if self.targets.len() > blurred.max(diffused) => Some((
                &self.targets[SOURCE].view,
                &self.targets[blurred].view,
                &self.targets[diffused].view,
            )),
            _ => self
                .scene
                .as_ref()
                .or_else(|| self.targets.first())
                .map(|target| (&target.view, &target.view, &target.view)),
        };

        let Some((source, blurred, diffused)) = views else {
            return;
        };

        let bind_group = pipelines.composite_bind_group(device, source, blurred, diffused, buffer);
        self.composites.push((bind_group, scissor));

        if filtered.is_some() {
            self.statistics.composites += 1;
        } else {
            self.statistics.fallback_composites += 1;
        }
    }

    /// Allocates one full-frame target.
    fn allocate(&mut self, device: &wgpu::Device, scene: bool) -> Target {
        let usage = if scene {
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(if scene {
                "iced_wgpu material scene"
            } else {
                "iced_wgpu material target"
            }),
            size: wgpu::Extent3d {
                width: self.size.width.max(1),
                height: self.size.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.statistics.target_allocations += 1;

        Target { texture, view }
    }

    /// Refreshes the retained-resource counts.
    fn account(&mut self) {
        let retained = self.targets.len() + usize::from(self.scene.is_some());

        self.statistics.retained_targets = retained;
        self.statistics.retained_bytes =
            retained as u64 * u64::from(self.size.width) * u64::from(self.size.height) * 4;
    }
}

impl Pipelines {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let filter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu material filter layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(FILTER_UNIFORM_SIZE),
                    },
                    count: None,
                },
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu material composite layout"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(Uniform::SIZE),
                    },
                    count: None,
                },
            ],
        });

        // Optics are linear-light maths. A target that stores sRGB-encoded
        // values without converting them (the `web-colors` policy) has its
        // samples decoded and its results encoded in the shaders instead.
        let encoded = encoded_prelude(format);
        let filter_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iced_wgpu material filter shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(format!(
                "{encoded}{}",
                include_str!("shader/material_filter.wgsl")
            ))),
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iced_wgpu material shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(format!(
                "{encoded}{}",
                include_str!("shader/material.wgsl")
            ))),
        });

        let filter_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("iced_wgpu material filter pipeline layout"),
                bind_group_layouts: &[Some(&filter_layout)],
                immediate_size: 0,
            });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("iced_wgpu material composite pipeline layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });

        let filter = pipeline(
            device,
            "iced_wgpu material filter pipeline",
            &filter_pipeline_layout,
            &filter_shader,
            "vs_fullscreen",
            "fs_filter",
            format,
            None,
        );
        let blit = pipeline(
            device,
            "iced_wgpu material blit pipeline",
            &filter_pipeline_layout,
            &filter_shader,
            "vs_fullscreen",
            "fs_blit",
            format,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let composite = pipeline(
            device,
            "iced_wgpu material composite pipeline",
            &composite_pipeline_layout,
            &composite_shader,
            "vs_region",
            "fs_material",
            format,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );

        let probe = pipeline(
            device,
            "iced_wgpu material probe pipeline",
            &composite_pipeline_layout,
            &composite_shader,
            "vs_probe",
            "fs_probe",
            PROBE_FORMAT,
            None,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iced_wgpu material sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            probe,
            filter,
            blit,
            composite,
            filter_layout,
            composite_layout,
            sampler,
        }
    }

    fn filter_bind_group(
        &self,
        device: &wgpu::Device,
        input: &wgpu::TextureView,
        uniform: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu material filter bind group"),
            layout: &self.filter_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    fn composite_bind_group(
        &self,
        device: &wgpu::Device,
        source: &wgpu::TextureView,
        blurred: &wgpu::TextureView,
        diffused: &wgpu::TextureView,
        uniform: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu material composite bind group"),
            layout: &self.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(blurred),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(diffused),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }
}

/// The shader constant declaring whether `format` holds sRGB-encoded values
/// the hardware does not convert.
fn encoded_prelude(format: wgpu::TextureFormat) -> String {
    format!("const ENCODED: bool = {};\n", !format.is_srgb())
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex: &str,
    fragment: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
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
}

/// Whether a physical `sigma` is worth two filter passes.
///
/// False below [`MIN_FILTER_REACH`] texels of total reach, where the Gaussian
/// is an identity copy of its input.
fn filters(sigma: f32) -> bool {
    sigma.is_finite() && 3.0 * sigma >= MIN_FILTER_REACH
}

/// The filter uniform: axis, sigma, tap radius, and the inclusive texel bounds
/// of the rectangle the backend copied into the input target.
///
/// The bounds are what keep a pooled target's previous frame out of a filtered
/// result: `material_filter.wgsl` clamps every tap to them, so the taps a
/// padding-band pixel would otherwise take from unwritten texels fold back onto
/// texels this frame copied.
fn filter_uniform(
    direction: [f32; 2],
    sigma: f32,
    radius: f32,
    source: crate::core::Rectangle<u32>,
) -> [f32; 8] {
    [
        direction[0],
        direction[1],
        sigma.max(0.0001),
        radius,
        source.x as f32,
        source.y as f32,
        (source.x + source.width.max(1) - 1) as f32,
        (source.y + source.height.max(1) - 1) as f32,
    ]
}

/// The whole-texel rectangle one region may paint, or `None` when its layer
/// clip and the frame leave it none.
fn scissor(
    region: &Region,
    clip: Rectangle,
    scale: f32,
    frame: Rectangle,
) -> Option<crate::core::Rectangle<u32>> {
    physical(region.bounds, scale)
        .intersection(&physical(clip, scale))
        .and_then(|visible| visible.intersection(&frame))
        .and_then(snap)
}

/// Whether any of `regions` can put a pixel on `frame`.
///
/// The frame's own cost, the sampleable scene and the blit that returns it, is
/// decided by this answer, so it asks exactly what the per-region scissor asks.
pub(crate) fn any_visible(
    regions: &[Region],
    clip: Rectangle,
    scale: f32,
    frame: Rectangle,
) -> bool {
    regions
        .iter()
        .any(|region| scissor(region, clip, scale, frame).is_some())
}

/// Whether a visible region of `regions` reads or paints anywhere in
/// `damage` (logical rectangles): its bounds widened by how far its filters
/// and refraction sample the backdrop around it (enzUI-ICED #3669).
pub(crate) fn reaches(
    regions: &[Region],
    clip: Rectangle,
    scale: f32,
    frame: Rectangle,
    damage: &[Rectangle],
) -> bool {
    regions.iter().any(|region| {
        // What the composite samples, and a pixel of antialiasing.
        let reach = region.material.sample_padding(region.bounds.size()) + 1.0;
        let sampled = region.bounds.expand(reach);
        scissor(region, clip, scale, frame).is_some()
            && damage.iter().any(|damage| sampled.intersects(damage))
    })
}

/// A logical rectangle in physical pixels.
fn physical(rectangle: Rectangle, scale: f32) -> Rectangle {
    Rectangle {
        x: rectangle.x * scale,
        y: rectangle.y * scale,
        width: rectangle.width * scale,
        height: rectangle.height * scale,
    }
}

/// Snaps a physical rectangle to whole texels, dropping empty ones.
fn snap(rectangle: Rectangle) -> Option<crate::core::Rectangle<u32>> {
    if !rectangle.x.is_finite()
        || !rectangle.y.is_finite()
        || !rectangle.width.is_finite()
        || !rectangle.height.is_finite()
    {
        return None;
    }

    let x = rectangle.x.floor().max(0.0) as u32;
    let y = rectangle.y.floor().max(0.0) as u32;
    let width = ((rectangle.x + rectangle.width).ceil().max(0.0) as u32).saturating_sub(x);
    let height = ((rectangle.y + rectangle.height).ceil().max(0.0) as u32).saturating_sub(y);

    (width > 0 && height > 0).then_some(crate::core::Rectangle {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Color;

    fn glass() -> Material {
        Material {
            radii: [20.0; 4],
            blur_sigma: 12.0,
            refraction: 1.0,
            bevel_width: 18.0,
            index_of_refraction: 1.5,
            dispersion: 0.4,
            color_diffusion: 0.0,
            color_reach: 0.0,
            optical_depth: 18.0,
            tint: Color::from_rgb(0.1, 0.1, 0.12),
            tint_opacity: 0.18,
            saturation: 1.2,
            highlight: 0.5,
            brightness: 0.0,
            light_angle: 300.0,
            profile_curvature: 2.4,
            contrast_protection: 0.5,
            protected_inset: 6.0,
            probe: 0,
            measure_only: false,
        }
    }

    fn region(x: f32, y: f32) -> Region {
        Region {
            bounds: Rectangle::new(Point::new(x, y), Size::new(200.0, 60.0)),
            material: glass(),
        }
    }

    fn frame() -> Rectangle {
        Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0))
    }

    #[test]
    fn only_renderable_copyable_filterable_formats_are_offered() {
        assert!(
            State::new(wgpu::TextureFormat::Bgra8UnormSrgb)
                .support()
                .is_optical()
        );
        assert_eq!(
            State::new(wgpu::TextureFormat::Rgba16Float).support(),
            crate::core::material::Support::Unsupported,
            "an unlisted format must refuse before a widget decides what to paint"
        );
    }

    #[test]
    fn a_new_state_holds_no_resource_until_a_frame_needs_one() {
        let state = State::new(wgpu::TextureFormat::Bgra8UnormSrgb);

        assert_eq!(state.statistics(), Statistics::default());
        assert_eq!(state.statistics().target_allocations, 0);
        assert!(state.pipelines.is_none());
        assert!(state.scene.is_none());
    }

    #[test]
    fn the_shared_source_is_the_padded_union_of_a_boundarys_regions() {
        let state = State::new(wgpu::TextureFormat::Bgra8UnormSrgb);
        let regions = [region(200.0, 200.0), region(400.0, 300.0)];
        let padding = glass().sample_padding(Size::new(200.0, 60.0)) + 3.0 * glass().blur_sigma;

        let one = state
            .source_rect(&regions[..1], 1.0, frame())
            .expect("a visible region needs a source");
        let both = state
            .source_rect(&regions, 1.0, frame())
            .expect("two regions still need one source");

        assert_eq!(one.x, 200.0 - padding);
        assert!(
            both.width > one.width && both.height > one.height,
            "a second region widens the shared source instead of adding another"
        );
        assert!(
            both.width < frame().width,
            "the source is bounded by the regions, not by the frame"
        );
        assert!(
            padding > 3.0 * glass().blur_sigma,
            "the padding must cover the filter and the grazing ray, not one of them"
        );
    }

    #[test]
    fn a_source_is_clamped_to_the_frame_and_refused_when_empty() {
        let state = State::new(wgpu::TextureFormat::Bgra8UnormSrgb);
        let clamped = state
            .source_rect(&[region(-100.0, 10.0)], 1.0, frame())
            .expect("a partly offscreen region still has a source");

        assert!(clamped.x >= 0.0 && clamped.y >= 0.0);
        assert!(clamped.x + clamped.width <= frame().width);

        // 200 px wide at x = -400 ends left of the frame even after padding.
        assert_eq!(
            state.source_rect(&[region(-400.0, 10.0)], 1.0, frame()),
            None,
            "a wholly offscreen region needs no source"
        );

        let empty = Region {
            bounds: Rectangle::new(Point::new(10.0, 10.0), Size::new(0.0, 60.0)),
            material: glass(),
        };
        assert_eq!(state.source_rect(&[empty], 1.0, frame()), None);
        assert_eq!(state.source_rect(&[], 1.0, frame()), None);
    }

    #[test]
    fn the_source_grows_with_the_scale_factor() {
        let state = State::new(wgpu::TextureFormat::Bgra8UnormSrgb);
        // Large enough that neither source reaches the frame's edge.
        let frame = Rectangle::new(Point::ORIGIN, Size::new(2000.0, 1500.0));
        let one = state
            .source_rect(&[region(300.0, 300.0)], 1.0, frame)
            .unwrap();
        let two = state
            .source_rect(&[region(300.0, 300.0)], 2.0, frame)
            .unwrap();

        assert!(
            (two.width - 2.0 * one.width).abs() < 1.0e-3,
            "a physical source is the logical one at the frame's scale"
        );
    }

    #[test]
    fn a_recorded_region_travels_with_the_paint_it_belongs_to() {
        let mut moved = region(100.0, 100.0);
        moved.translate(Vector::new(-25.0, 40.0));

        assert_eq!(moved.bounds.x, 75.0);
        assert_eq!(moved.bounds.y, 140.0);
        assert_eq!(
            moved.bounds.width, 200.0,
            "replay moves, it does not resize"
        );
    }

    // The float count and byte size of one WGSL struct, from the shader source
    // the pipeline actually compiles. Alignment follows WGSL's rules for the
    // scalar and vector types these two uniforms are built from.
    fn wgsl_struct_floats(source: &str, name: &str) -> usize {
        let header = format!("struct {name} {{");
        let start = source
            .find(&header)
            .unwrap_or_else(|| panic!("shader has no `struct {name}`"))
            + header.len();
        let body = &source[start..start + source[start..].find('}').expect("struct end")];

        let mut floats = 0usize;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let Some((_, ty)) = line.trim_end_matches(',').split_once(':') else {
                continue;
            };
            let (size, align) = match ty.trim() {
                "f32" | "i32" | "u32" => (1, 1),
                "vec2<f32>" => (2, 2),
                "vec3<f32>" => (3, 4),
                "vec4<f32>" => (4, 4),
                other => panic!("unmapped WGSL type {other:?} in struct {name}"),
            };
            floats = floats.next_multiple_of(align) + size;
        }

        // Struct size rounds up to the largest member alignment, which is a
        // `vec4` in both of these.
        floats.next_multiple_of(4)
    }

    #[test]
    fn the_wgsl_uniforms_are_exactly_the_bytes_the_backend_writes() {
        // The parser first, on inputs whose answers are arithmetic: a struct
        // whose padding it ignored would make every assertion below pass for
        // the wrong reason.
        assert_eq!(
            wgsl_struct_floats("struct A {\n a: vec2<f32>,\n b: f32,\n};", "A"),
            4,
            "3 floats must round up to the vec4 struct alignment"
        );
        assert_eq!(
            wgsl_struct_floats(
                "struct B {\n a: vec2<f32>,\n b: f32,\n c: vec4<f32>,\n};",
                "B"
            ),
            8,
            "a vec4 after 3 floats must be padded to offset 4"
        );

        let filter = include_str!("shader/material_filter.wgsl");
        let composite = include_str!("shader/material.wgsl");

        assert_eq!(
            wgsl_struct_floats(filter, "Filter") * 4,
            FILTER_UNIFORM_SIZE as usize,
            "the filter uniform buffer and `struct Filter` must be one size"
        );
        assert_eq!(
            filter_uniform(
                [1.0, 0.0],
                4.0,
                12.0,
                crate::core::Rectangle {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                }
            )
            .len()
                * 4,
            FILTER_UNIFORM_SIZE as usize,
            "the backend must write every float the shader declares"
        );
        assert_eq!(
            wgsl_struct_floats(composite, "Material"),
            Uniform::FLOATS,
            "`struct Material` and `iced_core::material::Uniform` are one contract"
        );
        assert_eq!(
            wgsl_struct_floats(composite, "Material") * 4,
            Uniform::SIZE as usize
        );
    }

    #[test]
    fn a_filter_pass_clamps_its_taps_to_the_rectangle_it_copied() {
        let copied = crate::core::Rectangle {
            x: 30,
            y: 40,
            width: 100,
            height: 50,
        };
        let uniform = filter_uniform([0.0, 1.0], 6.0, 18.0, copied);

        assert_eq!(&uniform[..4], &[0.0, 1.0, 6.0, 18.0]);
        assert_eq!(
            &uniform[4..],
            &[30.0, 40.0, 129.0, 89.0],
            "the bounds must be inclusive, or the last copied column and row \
             would be unreachable"
        );

        // A single-texel copy must still describe a texel, not an empty span.
        let dot = filter_uniform(
            [1.0, 0.0],
            0.5,
            2.0,
            crate::core::Rectangle {
                x: 7,
                y: 9,
                width: 1,
                height: 1,
            },
        );
        assert_eq!(&dot[4..], &[7.0, 9.0, 7.0, 9.0]);

        // Sigma never reaches the shader as zero: a zero would divide by zero
        // in the weight, and the backend skips the pass entirely long before.
        assert!(filter_uniform([1.0, 0.0], 0.0, 0.0, copied)[2] > 0.0);
    }

    #[test]
    fn a_blur_under_one_texel_of_reach_spends_no_filter_pass() {
        assert!(!filters(0.0), "an unblurred material must filter nothing");
        assert!(!filters(0.33), "3 * 0.33 < 1 texel of reach");
        assert!(filters(0.34));
        assert!(filters(12.0));
        assert!(!filters(f32::NAN), "a NaN sigma must not run a pass");

        // The shader's own bypass sits below this threshold, and the slot the
        // backend binds for a skipped filter *is* the source, so whichever
        // branch the shader takes it reads the same texels.
        const {
            assert!(
                MIN_FILTER_REACH / 3.0 > 0.001,
                "the backend must skip before the shader's bypass, never after"
            );
        }
    }

    #[test]
    fn the_composite_reports_the_coverage_it_premultiplied_by() {
        // Over a fully opaque backdrop, premultiplying by the silhouette and
        // premultiplying by the backdrop's own coverage give identical pixels,
        // so no screenshot of an opaque window can distinguish them. This
        // guard is the only thing that holds the transparent case.
        let composite = include_str!("shader/material.wgsl");

        assert!(
            composite.contains("let alpha = mask * backdrop_alpha;"),
            "coverage must be the region's mask times the backdrop's own alpha"
        );
        assert!(
            composite.contains("return vec4<f32>(encode(color) * alpha, alpha);"),
            "the composite must premultiply by exactly the alpha it reports"
        );
        assert!(
            !composite.contains("return vec4<f32>(color * mask, mask);"),
            "premultiplying by the silhouette alone promotes a transparent \
             frame region into an opaque panel"
        );
        assert!(
            composite.contains("fn straight(sample: vec4<f32>) -> vec3<f32>"),
            "straight-colour maths needs an un-premultiplied sample"
        );

        // The filter and the blit share a bind group layout but not a
        // rectangle: swapping their clamps pins the whole frame to one texel.
        let filter = include_str!("shader/material_filter.wgsl");
        assert!(filter.contains("return load_frame(vec2<i32>(input.position.xy));"));
        assert!(filter.contains("var total = perceptual(load(center));"));
    }

    #[test]
    fn snapping_covers_a_fractional_rectangle_and_drops_an_empty_one() {
        let snapped = snap(Rectangle::new(
            Point::new(10.4, 20.6),
            Size::new(100.2, 30.1),
        ))
        .expect("a fractional rectangle still covers whole texels");

        assert_eq!(snapped.x, 10);
        assert_eq!(snapped.y, 20);
        assert_eq!(
            snapped.width, 101,
            "the copy must cover every touched texel"
        );
        assert_eq!(
            snap(Rectangle::new(Point::new(4.0, 4.0), Size::new(0.0, 8.0))),
            None
        );
        assert_eq!(
            snap(Rectangle::new(
                Point::new(f32::NAN, 0.0),
                Size::new(8.0, 8.0)
            )),
            None
        );
    }
}
