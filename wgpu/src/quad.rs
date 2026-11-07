// Modified by enzUI contributors. See FORK.md.
mod gradient;
mod solid;

use gradient::Gradient;
use solid::Solid;

use crate::core::{Background, Point, Rectangle, Transformation};
use crate::graphics;
use crate::graphics::color;

use bytemuck::{Pod, Zeroable};

use std::cell::Cell;
use std::mem;

/// The smallest instance capacity a quad sublayer is allocated with.
///
/// enzICED #1515: this used to be `INITIAL_INSTANCES = 2_000`, allocated
/// *eagerly for both fill kinds* in every clip layer — 256 KiB of solid plus
/// 512 KiB of gradient instance storage per layer, whether or not the layer
/// held a single quad. Sublayers are now created on first use and sized to the
/// instances actually present, with this floor so a one-quad layer that grows
/// by a quad at a time does not reallocate on every frame. `Buffer` rounds to
/// the next power of two, so growth past the floor is geometric.
const MIN_INSTANCES: usize = 16;

/// Frames an unused layer slot or oversized instance buffer must stay unused
/// before it is released.
///
/// enzICED #1515: reclamation happens in [`State::trim`], which runs while the
/// frame's encoder is still open, so a slot is only ever released after it has
/// gone unused for a whole window — several submissions — never in the frame
/// that referenced it.
const RECLAIM_FRAMES: u64 = 3;

/// How much unused instance capacity a retained sublayer may keep before it is
/// reallocated smaller.
const SHRINK_SLACK: usize = 4;

/// The result of staging one sublayer upload (enzICED #1515 accounting).
#[derive(Debug, Clone, Copy, Default)]
pub struct Upload {
    /// Bytes handed to the staging belt.
    pub uploaded_bytes: u64,
    /// GPU buffers created to satisfy the upload.
    pub buffers_created: u64,
    /// GPU buffers released while preparing.
    pub buffers_released: u64,
}

impl Upload {
    fn add(&mut self, other: Upload) {
        self.uploaded_bytes += other.uploaded_bytes;
        self.buffers_created += other.buffers_created;
        self.buffers_released += other.buffers_released;
    }
}

/// Mechanism counts for the quad pipeline's per-clip-layer GPU storage.
///
/// enzICED #1515. Every field is a count or a byte total, so it is independent
/// of the machine and can be asserted by a gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuadStatistics {
    /// Frames prepared since the renderer was created.
    pub frame: u64,
    /// Clip layers prepared in the last frame.
    pub active_layers: usize,
    /// Layer slots still held (>= `active_layers`).
    pub retained_layers: usize,
    /// Retained slots holding a solid instance buffer.
    pub solid_buffers: usize,
    /// Retained slots holding a gradient instance buffer.
    pub gradient_buffers: usize,
    /// Bytes committed to GPU instance buffers right now.
    pub instance_bytes: u64,
    /// Bytes committed to GPU uniform buffers right now.
    pub uniform_bytes: u64,
    /// `instance_bytes + uniform_bytes`.
    pub allocated_bytes: u64,
    /// Peak of `allocated_bytes` observed at the end of any frame.
    pub peak_allocated_bytes: u64,
    /// GPU buffers created since the renderer was created.
    pub buffers_created: u64,
    /// GPU buffers released since the renderer was created.
    pub buffers_released: u64,
    /// Bind groups created since the renderer was created.
    pub bind_groups_created: u64,
    /// Layer slots reclaimed since the renderer was created.
    pub layers_reclaimed: u64,
    /// Bytes staged for upload during the last frame.
    pub uploaded_bytes: u64,
    /// Bytes staged for upload since the renderer was created.
    pub total_uploaded_bytes: u64,
    /// Draw calls the quad pipeline recorded during the last frame.
    pub draw_calls: u64,
    /// Uniform bind-group binds the quad pipeline recorded last frame.
    pub bind_group_changes: u64,
}

// enzICED #41: the rounded clip region moved to `crate::clip` in stage 3, when
// the image and mesh pipelines started consuming it too. Re-exported here so
// `quad::ClipRegion` keeps resolving.
pub use crate::clip::ClipRegion;

/// The properties of a quad.
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct Quad {
    /// The position of the [`Quad`].
    pub position: [f32; 2],

    /// The size of the [`Quad`].
    pub size: [f32; 2],

    /// The border color of the [`Quad`], in __linear RGB__.
    pub border_color: color::Packed,

    /// The border radii of the [`Quad`].
    pub border_radius: [f32; 4],

    /// The border width of the [`Quad`].
    pub border_width: f32,

    /// The shadow color of the [`Quad`].
    pub shadow_color: color::Packed,

    /// The shadow offset of the [`Quad`].
    pub shadow_offset: [f32; 2],

    /// The shadow blur radius of the [`Quad`].
    pub shadow_blur_radius: f32,

    /// Whether the [`Quad`] should be snapped to the pixel grid.
    pub snap: u32,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    solid: solid::Pipeline,
    gradient: gradient::Pipeline,
    constant_layout: wgpu::BindGroupLayout,
}

#[derive(Default)]
pub struct State {
    layers: Vec<Layer>,
    prepare_layer: usize,
    // enzICED #1515: reclamation window and mechanism counts.
    frame: u64,
    window_frames: u64,
    window_peak_layers: usize,
    active_layers: usize,
    peak_allocated_bytes: u64,
    buffers_created: u64,
    buffers_released: u64,
    bind_groups_created: u64,
    layers_reclaimed: u64,
    frame_uploaded_bytes: u64,
    last_uploaded_bytes: u64,
    total_uploaded_bytes: u64,
    draw_calls: Cell<u64>,
    bind_group_changes: Cell<u64>,
    last_draw_calls: u64,
    last_bind_group_changes: u64,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mechanism counts for the quad pipeline's GPU storage (enzICED #1515).
    pub fn statistics(&self) -> QuadStatistics {
        let instance_bytes = self.layers.iter().map(Layer::instance_bytes).sum::<u64>();
        let uniform_bytes = self.layers.len() as u64 * mem::size_of::<Uniforms>() as u64;

        QuadStatistics {
            frame: self.frame,
            active_layers: self.active_layers,
            retained_layers: self.layers.len(),
            solid_buffers: self.layers.iter().filter(|l| l.solid.is_some()).count(),
            gradient_buffers: self.layers.iter().filter(|l| l.gradient.is_some()).count(),
            instance_bytes,
            uniform_bytes,
            allocated_bytes: instance_bytes + uniform_bytes,
            peak_allocated_bytes: self.peak_allocated_bytes,
            buffers_created: self.buffers_created,
            buffers_released: self.buffers_released,
            bind_groups_created: self.bind_groups_created,
            layers_reclaimed: self.layers_reclaimed,
            uploaded_bytes: self.last_uploaded_bytes,
            total_uploaded_bytes: self.total_uploaded_bytes,
            draw_calls: self.last_draw_calls,
            bind_group_changes: self.last_bind_group_changes,
        }
    }

    pub fn prepare(
        &mut self,
        pipeline: &Pipeline,
        device: &wgpu::Device,
        belt: &mut wgpu::util::StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        quads: &Batch,
        transformation: Transformation,
        scale: f32,
        clip: ClipRegion,
    ) {
        if self.layers.len() <= self.prepare_layer {
            self.layers
                .push(Layer::new(device, &pipeline.constant_layout));

            // enzICED #1515: a fresh slot is one uniform buffer and one bind
            // group; its instance buffers are created lazily below.
            self.buffers_created += 1;
            self.bind_groups_created += 1;
        }

        let layer = &mut self.layers[self.prepare_layer];
        let upload = layer.prepare(device, encoder, belt, quads, transformation, scale, clip);

        self.buffers_created += upload.buffers_created;
        self.buffers_released += upload.buffers_released;
        self.frame_uploaded_bytes += upload.uploaded_bytes;

        self.prepare_layer += 1;
    }

    pub fn render<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        layer: usize,
        bounds: Rectangle<u32>,
        quads: &Batch,
        render_pass: &mut wgpu::RenderPass<'a>,
    ) {
        if let Some(layer) = self.layers.get(layer) {
            render_pass.set_scissor_rect(bounds.x, bounds.y, bounds.width, bounds.height);

            let mut solid_offset = 0;
            let mut gradient_offset = 0;

            for (kind, count) in &quads.order {
                match kind {
                    Kind::Solid => {
                        // enzICED #1515: the sublayer exists because the batch
                        // has solids; `prepare` created it for exactly this.
                        if let Some(solid) = &layer.solid {
                            pipeline.solid.render(
                                render_pass,
                                &layer.constants,
                                solid,
                                solid_offset..(solid_offset + count),
                            );

                            self.draw_calls.set(self.draw_calls.get() + 1);
                            self.bind_group_changes
                                .set(self.bind_group_changes.get() + 1);
                        }

                        solid_offset += count;
                    }
                    Kind::Gradient => {
                        if let Some(gradient) = &layer.gradient {
                            pipeline.gradient.render(
                                render_pass,
                                &layer.constants,
                                gradient,
                                gradient_offset..(gradient_offset + count),
                            );

                            self.draw_calls.set(self.draw_calls.get() + 1);
                            self.bind_group_changes
                                .set(self.bind_group_changes.get() + 1);
                        }

                        gradient_offset += count;
                    }
                }
            }
        }
    }

    pub fn trim(&mut self) {
        // enzICED #1515: close out the frame's counters, then reclaim any
        // capacity that a whole window of frames did not need. Reclamation
        // only ever touches slots and capacity unused for `RECLAIM_FRAMES`
        // consecutive frames, so a stable workload never frees anything and
        // an in-flight command buffer can never reference what is dropped.
        self.frame += 1;
        self.active_layers = self.prepare_layer;
        self.last_uploaded_bytes = self.frame_uploaded_bytes;
        self.total_uploaded_bytes += self.frame_uploaded_bytes;
        self.frame_uploaded_bytes = 0;
        self.last_draw_calls = self.draw_calls.replace(0);
        self.last_bind_group_changes = self.bind_group_changes.replace(0);

        self.window_peak_layers = self.window_peak_layers.max(self.prepare_layer);
        self.window_frames += 1;
        self.prepare_layer = 0;

        if self.window_frames >= RECLAIM_FRAMES {
            let keep = self.window_peak_layers;

            if self.layers.len() > keep {
                for layer in self.layers.drain(keep..) {
                    self.buffers_released += layer.buffer_count();
                    self.layers_reclaimed += 1;
                }
            }

            self.window_frames = 0;
            self.window_peak_layers = 0;
        }

        let allocated = self.statistics().allocated_bytes;
        self.peak_allocated_bytes = self.peak_allocated_bytes.max(allocated);
    }
}

impl Pipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Pipeline {
        let constant_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu::quad uniforms layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // enzICED #41 stage 2: the fragment shader now reads the clip
                // region from these uniforms, so make them visible to it too.
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        mem::size_of::<Uniforms>() as wgpu::BufferAddress
                    ),
                },
                count: None,
            }],
        });

        Self {
            solid: solid::Pipeline::new(device, format, &constant_layout),
            gradient: gradient::Pipeline::new(device, format, &constant_layout),
            constant_layout,
        }
    }
}

#[derive(Debug)]
pub struct Layer {
    constants: wgpu::BindGroup,
    constants_buffer: wgpu::Buffer,
    /// What `constants_buffer` holds, so an unchanged layer uploads nothing
    /// (enzUI-ICED #3668).
    written: Option<Uniforms>,
    // enzICED #1515: created on first use of that fill kind, so a solid-only
    // layer never allocates gradient storage and vice versa.
    solid: Option<solid::Layer>,
    gradient: Option<gradient::Layer>,
    window_frames: u64,
    solid_peak: usize,
    gradient_peak: usize,
}

impl Layer {
    pub fn new(device: &wgpu::Device, constant_layout: &wgpu::BindGroupLayout) -> Self {
        let constants_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iced_wgpu::quad uniforms buffer"),
            size: mem::size_of::<Uniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let constants = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu::quad uniforms bind group"),
            layout: constant_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: constants_buffer.as_entire_binding(),
            }],
        });

        Self {
            constants,
            constants_buffer,
            written: None,
            solid: None,
            gradient: None,
            window_frames: 0,
            solid_peak: 0,
            gradient_peak: 0,
        }
    }

    /// The bytes this layer's instance buffers hold on the GPU.
    fn instance_bytes(&self) -> u64 {
        self.solid.as_ref().map_or(0, solid::Layer::allocated_bytes)
            + self
                .gradient
                .as_ref()
                .map_or(0, gradient::Layer::allocated_bytes)
    }

    /// GPU buffers this layer owns: its uniforms plus any live sublayer.
    fn buffer_count(&self) -> u64 {
        1 + u64::from(self.solid.is_some()) + u64::from(self.gradient.is_some())
    }

    /// Releases sublayer capacity that a whole window of frames did not use.
    ///
    /// enzICED #1515: runs *before* this frame's upload, and never below the
    /// window peak, so the buffer it may recreate is fully rewritten below and
    /// the frame being prepared cannot lose data.
    fn reclaim(&mut self, device: &wgpu::Device, quads: &Batch) -> Upload {
        self.solid_peak = self.solid_peak.max(quads.solids.len());
        self.gradient_peak = self.gradient_peak.max(quads.gradients.len());
        self.window_frames += 1;

        if self.window_frames < RECLAIM_FRAMES {
            return Upload::default();
        }

        let mut reclaimed = Upload::default();

        match (&mut self.solid, self.solid_peak) {
            (slot @ Some(_), 0) => {
                *slot = None;
                reclaimed.buffers_released += 1;
            }
            (Some(solid), peak) if solid.capacity() > (peak * SHRINK_SLACK).max(MIN_INSTANCES) => {
                if solid.shrink(device, peak) {
                    reclaimed.buffers_created += 1;
                    reclaimed.buffers_released += 1;
                }
            }
            _ => {}
        }

        match (&mut self.gradient, self.gradient_peak) {
            (slot @ Some(_), 0) => {
                *slot = None;
                reclaimed.buffers_released += 1;
            }
            (Some(gradient), peak)
                if gradient.capacity() > (peak * SHRINK_SLACK).max(MIN_INSTANCES) =>
            {
                if gradient.shrink(device, peak) {
                    reclaimed.buffers_created += 1;
                    reclaimed.buffers_released += 1;
                }
            }
            _ => {}
        }

        self.window_frames = 0;
        self.solid_peak = 0;
        self.gradient_peak = 0;

        reclaimed
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        quads: &Batch,
        transformation: Transformation,
        scale: f32,
        clip: ClipRegion,
    ) -> Upload {
        let mut upload = Upload {
            uploaded_bytes: mem::size_of::<Uniforms>() as u64,
            ..Upload::default()
        };

        self.update(encoder, belt, transformation, scale, clip);

        upload.add(self.reclaim(device, quads));

        if !quads.solids.is_empty() {
            let solid = match &mut self.solid {
                Some(solid) => solid,
                slot => {
                    upload.buffers_created += 1;
                    slot.insert(solid::Layer::new(device, quads.solids.len()))
                }
            };

            upload.add(solid.prepare(device, encoder, belt, &quads.solids));
        }

        if !quads.gradients.is_empty() {
            let gradient = match &mut self.gradient {
                Some(gradient) => gradient,
                slot => {
                    upload.buffers_created += 1;
                    slot.insert(gradient::Layer::new(device, quads.gradients.len()))
                }
            };

            upload.add(gradient.prepare(device, encoder, belt, &quads.gradients));
        }

        upload
    }

    pub fn update(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        transformation: Transformation,
        scale: f32,
        clip: ClipRegion,
    ) {
        let uniforms = Uniforms::new(transformation, scale, clip);
        if self
            .written
            .is_some_and(|written| bytemuck::bytes_of(&written) == bytemuck::bytes_of(&uniforms))
        {
            return;
        }
        self.written = Some(uniforms);
        let bytes = bytemuck::bytes_of(&uniforms);

        belt.write_buffer(
            encoder,
            &self.constants_buffer,
            0,
            (bytes.len() as u64).try_into().expect("Sized uniforms"),
        )
        .copy_from_slice(bytes);
    }
}

/// A group of [`Quad`]s rendered together.
#[derive(Default, Debug, Clone)]
pub struct Batch {
    /// The solid quads of the [`Layer`].
    solids: Vec<Solid>,

    /// The gradient quads of the [`Layer`].
    gradients: Vec<Gradient>,

    /// The quad order of the [`Layer`].
    order: Order,
}

/// The quad order of a [`Layer`]; stored as a tuple of the quad type & its count.
type Order = Vec<(Kind, usize)>;

impl Batch {
    /// Hashes how the quads are ordered (enzUI-ICED #3666).
    pub(crate) fn hash_order(&self, state: &mut crate::damage::PaintHasher) {
        state.usize(self.solids.len());
        state.usize(self.gradients.len());
        for (kind, count) in &self.order {
            state.usize(usize::from(*kind == Kind::Gradient));
            state.usize(*count);
        }
    }

    /// Each quad's instance bytes and geometry, solids first.
    pub(crate) fn elements(&self) -> impl Iterator<Item = (&[u8], &Quad)> {
        self.solids
            .iter()
            .map(|solid| (bytemuck::bytes_of(solid), &solid.quad))
            .chain(
                self.gradients
                    .iter()
                    .map(|gradient| (bytemuck::bytes_of(gradient), &gradient.quad)),
            )
    }

    /// Whether every quad is a solid circle with no offset shadow, so it
    /// looks the same at any turn about any point (enzUI-ICED #3668).
    pub(crate) fn all_round(&self) -> bool {
        const SLACK: f32 = 1e-3;
        self.gradients.is_empty()
            && self.solids.iter().all(|solid| {
                let quad = &solid.quad;
                let diameter = quad.size[0];
                (quad.size[1] - diameter).abs() <= SLACK
                    && quad
                        .border_radius
                        .iter()
                        .all(|radius| (radius - diameter / 2.0).abs() <= SLACK)
                    && (quad.shadow_color.components()[3] <= 0.0
                        || quad.shadow_offset == [0.0, 0.0])
            })
    }

    /// How far from `center` the quads reach, shadows included.
    pub(crate) fn reach_from(&self, center: Point) -> f32 {
        self.geometry()
            .map(|quad| {
                let middle = Point::new(
                    quad.position[0] + quad.size[0] / 2.0,
                    quad.position[1] + quad.size[1] / 2.0,
                );
                let shadow = if quad.shadow_color.components()[3] > 0.0 {
                    quad.shadow_blur_radius.max(0.0) * 2.0
                } else {
                    0.0
                };
                middle.distance(center) + quad.size[0] / 2.0 + shadow
            })
            .fold(0.0, f32::max)
    }

    /// Turns round quads by `(sin, cos)` and scales them by `scale` about
    /// `center`: each moves its middle there and grows in place.
    pub(crate) fn turn_round(&mut self, center: Point, scale: f32, sin: f32, cos: f32) {
        for solid in &mut self.solids {
            let quad = &mut solid.quad;
            let (x, y) = (
                (quad.position[0] + quad.size[0] / 2.0 - center.x) * scale,
                (quad.position[1] + quad.size[1] / 2.0 - center.y) * scale,
            );
            let middle = Point::new(center.x + x * cos - y * sin, center.y + x * sin + y * cos);
            quad.size = [quad.size[0] * scale, quad.size[1] * scale];
            quad.border_radius = quad.border_radius.map(|radius| radius * scale);
            quad.border_width *= scale;
            quad.shadow_blur_radius *= scale;
            quad.position = [middle.x - quad.size[0] / 2.0, middle.y - quad.size[1] / 2.0];
            quad.snap = 0;
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.solids.len() + self.gradients.len()
    }

    /// The solid quads of the batch, with their colours.
    pub(crate) fn solids(&self) -> impl Iterator<Item = &Solid> {
        self.solids.iter()
    }

    /// The geometry of every quad in the batch, solids first.
    pub(crate) fn geometry(&self) -> impl Iterator<Item = &Quad> {
        self.solids
            .iter()
            .map(|solid| &solid.quad)
            .chain(self.gradients.iter().map(|gradient| &gradient.quad))
    }

    pub(crate) fn matches(&self, expected: &Self) -> bool {
        self.order == expected.order
            && bytemuck::cast_slice::<Solid, u8>(&self.solids)
                == bytemuck::cast_slice::<Solid, u8>(&expected.solids)
            && bytemuck::cast_slice::<Gradient, u8>(&self.gradients)
                == bytemuck::cast_slice::<Gradient, u8>(&expected.gradients)
    }

    /// Offset every quad by `translation` (enzICED #2219 scene replay).
    ///
    /// Quads are baked into absolute coordinates when they are recorded
    /// (`Layer::draw_quad` multiplies by the transformation in effect), so a
    /// replayed batch is re-placed by moving each quad rather than by pushing a
    /// transform the pipeline would have to honour. Only the position moves:
    /// size, radii, border width and shadow are translation-invariant.
    ///
    /// A gradient quad additionally carries its interpolation chord in the same
    /// absolute space (`graphics::gradient::pack`), so that moves too --
    /// otherwise a replayed panel slides underneath a gradient that stays
    /// nailed to the screen (#2219 review finding 2).
    pub fn translate(&mut self, translation: crate::core::Vector) {
        for solid in &mut self.solids {
            solid.quad.position[0] += translation.x;
            solid.quad.position[1] += translation.y;
        }

        for gradient in &mut self.gradients {
            gradient.quad.position[0] += translation.x;
            gradient.quad.position[1] += translation.y;
            gradient.gradient.translate(translation);
        }
    }

    /// Returns true if there are no quads of any type in [`Quads`].
    pub fn is_empty(&self) -> bool {
        self.solids.is_empty() && self.gradients.is_empty()
    }

    /// Adds a [`Quad`] with the provided `Background` type to the quad [`Layer`].
    pub fn add(&mut self, quad: Quad, background: &Background) {
        let kind = match background {
            Background::Color(color) => {
                self.solids.push(Solid {
                    color: color::pack(*color),
                    quad,
                });

                Kind::Solid
            }
            Background::Gradient(gradient) => {
                self.gradients.push(Gradient {
                    gradient: graphics::gradient::pack(
                        gradient,
                        Rectangle::new(quad.position.into(), quad.size.into()),
                    ),
                    quad,
                });

                Kind::Gradient
            }
        };

        match self.order.last_mut() {
            Some((last_kind, count)) if kind == *last_kind => {
                *count += 1;
            }
            _ => {
                self.order.push((kind, 1));
            }
        }
    }

    pub fn clear(&mut self) {
        self.solids.clear();
        self.gradients.clear();
        self.order.clear();
    }

    pub fn append(&mut self, batch: &mut Batch) {
        self.solids.append(&mut batch.solids);
        self.gradients.append(&mut batch.gradients);
        self.order.append(&mut batch.order);
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
/// The kind of a quad.
enum Kind {
    /// A solid quad
    Solid,
    /// A gradient quad
    Gradient,
}

fn color_target_state(format: wgpu::TextureFormat) -> [Option<wgpu::ColorTargetState>; 1] {
    [Some(wgpu::ColorTargetState {
        format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    })]
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Zeroable, bytemuck::Pod)]
struct Uniforms {
    transform: [f32; 16],
    scale: f32,
    // Uniforms must be aligned to their largest member,
    // this uses a mat4x4<f32> which aligns to 16, so align to that
    _padding: [f32; 3],
    // enzICED #41 stage 2: rounded clip region, physical pixels. `clip_rect`
    // is [x, y, w, h]; `clip_radius` is [tl, tr, br, bl]. All-zero radius means
    // "no rounded clip" and the shader takes an early-out.
    clip_rect: [f32; 4],
    clip_radius: [f32; 4],
}

impl Uniforms {
    fn new(transformation: Transformation, scale: f32, clip: ClipRegion) -> Uniforms {
        Self {
            transform: *transformation.as_ref(),
            scale,
            _padding: [0.0; 3],
            clip_rect: clip.rect,
            clip_radius: clip.radius,
        }
    }
}

impl Default for Uniforms {
    fn default() -> Self {
        Self {
            transform: *Transformation::IDENTITY.as_ref(),
            scale: 1.0,
            _padding: [0.0; 3],
            clip_rect: [0.0; 4],
            clip_radius: [0.0; 4],
        }
    }
}
