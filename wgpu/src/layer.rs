// Modified by enzUI contributors. See FORK.md.
use crate::core::border;
use crate::core::{
    self, Background, Color, Point, Rectangle, Size, Svg, Transformation, Vector, renderer,
};
use crate::damage::{Held, PaintHasher};
use crate::graphics;
use crate::graphics::Mesh;
use crate::graphics::color;
use crate::graphics::layer;
use crate::graphics::mesh;
use crate::graphics::text::{Editor, Paragraph};
use crate::image::{self, Image};
use crate::material;
use crate::primitive::{self, Primitive};
use crate::quad::{self, Quad};
use crate::text::{self, Text};
use crate::triangle;

pub type Stack = layer::Stack<Layer>;
pub(crate) type ClipSources = Vec<(usize, std::sync::Arc<graphics::layer::ClipPath>)>;

// Group identities preserve painting order across retained layers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AlphaGroup {
    pub id: usize,
    pub alpha: f32,
    /// Uniform scale applied when this offscreen group is composited.
    pub scale: f32,
    /// Scale and rotation origin in logical window coordinates.
    pub scale_origin: Point,
    /// Rotation about `scale_origin` applied when the group is composited,
    /// in radians, clockwise on screen.
    pub rotation: f32,
    /// Where the group's paint can land once composited, in logical window
    /// coordinates; `None` when it is not known. The composite touches only
    /// these pixels: a small turning or scaling group then costs its own
    /// area, not the whole window's.
    pub reach: Option<Rectangle>,
    pub clip: Option<(Rectangle, crate::core::border::Radius)>,
    /// The clip a captured copy keeps, when a retained-ink layer opened the
    /// group: the live clip is cut at the visible bounds, the recording's
    /// reaches as far as its ink.
    pub retained_clip: Option<(Rectangle, crate::core::border::Radius)>,
    /// How many recorded nodes enclosed the group when it opened: replay
    /// moves its clip with those nodes only.
    pub depth: usize,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub(crate) clip_path: Option<std::sync::Arc<graphics::layer::ClipPath>>,
    pub bounds: Rectangle,
    /// Wider clip used only when this live layer is cloned into a recording.
    retained_bounds: Rectangle,
    /// Live clip of an explicitly retained presentation boundary.
    ///
    /// This is `None` for every ordinary and descendant clip. Replay therefore
    /// never guesses ownership from rectangle equality (#2219).
    presentation_clip: Option<Rectangle>,
    /// Rounded radius paired with `presentation_clip`.
    presentation_clip_radius: Option<border::Radius>,
    /// Fixed enclosing clip captured at a scene-segment boundary.
    fixed_replay_clip: bool,
    /// Ancestors' clip of a presentation boundary, applied after replay
    /// moves its retained ink.
    enclosing_clip: Option<Rectangle>,
    /// How many recorded nodes enclosed `enclosing_clip` when it opened.
    enclosing_depth: usize,
    /// Rounded radius paired with `retained_bounds`.
    retained_clip_radius: Option<border::Radius>,
    /// Rounded clip radius for this layer, in the layer's own (post-transform)
    /// coordinate space (enzICED #41 stage 2). `None` means a plain
    /// rectangular clip (the historical behaviour).
    pub clip_radius: Option<border::Radius>,
    /// Alpha groups enclosing this layer, outermost first.
    ///
    /// IDs keep equal-alpha sibling groups distinct through stack merging.
    pub(crate) alpha_groups: Vec<AlphaGroup>,
    pub(crate) clip_group: Option<usize>,
    pub(crate) clip_sources: ClipSources,
    /// Recording segment separated by a completed alpha boundary.
    pub(crate) alpha_segment: usize,
    /// Sampled-backdrop materials composited *before* this layer's own
    /// contents, in recorded order (enzUI-ICED #2950).
    ///
    /// A region belongs to the layer whose content is its foreground, which is
    /// what makes the boundary ordered: everything already drawn is its
    /// backdrop, everything in this layer is not.
    pub(crate) materials: Vec<material::Region>,
    pub quads: quad::Batch,
    pub(crate) fixed_layout_quads: usize,
    pub triangles: triangle::Batch,
    pub primitives: primitive::Batch,
    pub images: image::Batch,
    pub text: text::Batch,
    pending_meshes: Vec<Mesh>,
    pending_text: Vec<Text>,
}

/// What a [`Layer`] paints, taken out of it while a frame paints only part
/// of the window.
#[derive(Debug)]
pub(crate) struct Paint {
    materials: Vec<material::Region>,
    quads: quad::Batch,
    fixed_layout_quads: usize,
    triangles: triangle::Batch,
    primitives: primitive::Batch,
    images: image::Batch,
    text: text::Batch,
    pending_meshes: Vec<Mesh>,
    pending_text: Vec<Text>,
}

impl Layer {
    /// Hashes what this layer paints (enzUI-ICED #3666).
    ///
    /// Returns a signature of everything but its primitives (its materials
    /// included), `None` when its paint can change without a new recording
    /// (a custom primitive that cannot tell what it paints, unflushed paint).
    /// Calls `element` with each primitive's
    /// signature (`None` for text the recording does not own) and the
    /// rectangle it paints within, before the layer's clip and groups.
    ///
    /// With `held`, a frame drawn by widgets rather than replayed: what is
    /// hashed by address (a widget's paragraph, a clip path) is kept in
    /// `held`, so the address is not reused while the frame is compared.
    pub(crate) fn hash_paint(
        &self,
        state: &mut PaintHasher,
        mut held: Option<&mut Held>,
        mut element: impl FnMut(Option<u64>, Rectangle),
    ) -> Option<u64> {
        let Self {
            clip_path,
            bounds,
            retained_bounds,
            presentation_clip,
            presentation_clip_radius,
            fixed_replay_clip,
            enclosing_clip,
            enclosing_depth,
            retained_clip_radius,
            clip_radius,
            alpha_groups,
            clip_group,
            clip_sources,
            alpha_segment,
            materials,
            quads,
            fixed_layout_quads,
            triangles,
            primitives,
            images,
            text,
            pending_meshes,
            pending_text,
        } = self;
        if !pending_meshes.is_empty() || !pending_text.is_empty() {
            return None;
        }
        // A custom primitive that tells what it paints is compared like any
        // other paint; one that cannot leaves the layer to be painted whole.
        state.usize(primitives.len());
        for instance in primitives {
            if !instance.paint(&mut |signature, rect| element(Some(signature), rect)) {
                return None;
            }
        }
        // A material's paint is its region, its parameters and what is behind
        // it; the backdrop is the caller's to compare.
        state.usize(materials.len());
        for region in materials {
            state.rectangle(region.bounds);
            hash_material(state, &region.material);
        }

        // A clip path is shared, never changed: a recording that holds it
        // keeps its address from being reused.
        state.option(clip_path.as_ref(), |state, path| {
            state.usize(std::sync::Arc::as_ptr(path) as usize);
        });
        if let (Some(held), Some(path)) = (held.as_deref_mut(), clip_path) {
            held.paths.push(path.clone());
        }
        state.rectangle(*bounds);
        state.rectangle(*retained_bounds);
        state.option(*presentation_clip, PaintHasher::rectangle);
        state.option(*presentation_clip_radius, PaintHasher::radius);
        state.usize(usize::from(*fixed_replay_clip));
        state.option(*enclosing_clip, PaintHasher::rectangle);
        state.usize(*enclosing_depth);
        state.option(*retained_clip_radius, PaintHasher::radius);
        state.option(*clip_radius, PaintHasher::radius);
        state.usize(alpha_groups.len());
        for group in alpha_groups {
            let AlphaGroup {
                id,
                alpha,
                scale,
                scale_origin,
                rotation,
                reach,
                clip,
                retained_clip,
                depth,
            } = *group;
            state.group(id);
            state.f32(alpha);
            state.f32(scale);
            state.point(scale_origin);
            state.f32(rotation);
            state.option(reach, PaintHasher::rectangle);
            for clip in [clip, retained_clip] {
                state.option(clip, |state, (bounds, radius)| {
                    state.rectangle(bounds);
                    state.radius(radius);
                });
            }
            state.usize(depth);
        }
        state.option(*clip_group, PaintHasher::group);
        state.usize(clip_sources.len());
        for (id, path) in clip_sources {
            state.group(*id);
            state.usize(std::sync::Arc::as_ptr(path) as usize);
            if let Some(held) = held.as_deref_mut() {
                held.paths.push(path.clone());
            }
        }
        state.usize(*alpha_segment);
        state.usize(*fixed_layout_quads);

        quads.hash_order(state);
        for (bytes, quad) in quads.elements() {
            let mut hash = PaintHasher::default();
            hash.bytes(bytes);
            element(Some(hash.finish()), quad_reach(quad));
        }

        state.usize(triangles.len());
        for item in triangles {
            match item {
                triangle::Item::Group {
                    transformation,
                    meshes,
                } => {
                    state.usize(meshes.len());
                    for mesh in meshes {
                        let mut hash = PaintHasher::default();
                        hash.transformation(*transformation);
                        hash_mesh(&mut hash, mesh);
                        element(Some(hash.finish()), mesh.clip_bounds() * *transformation);
                    }
                }
                triangle::Item::Cached {
                    transformation,
                    cache,
                } => {
                    let mut hash = PaintHasher::default();
                    hash.transformation(*transformation);
                    hash.hash(&cache.id());
                    hash.usize(cache.version());
                    let reach = cache
                        .batch()
                        .iter()
                        .map(|mesh| mesh.clip_bounds() * *transformation)
                        .reduce(|a, b| a.union(&b));
                    if let Some(reach) = reach {
                        element(Some(hash.finish()), reach);
                    }
                }
            }
        }

        #[cfg(any(feature = "image", feature = "svg"))]
        {
            state.usize(images.len());
            for image in images {
                let mut hash = PaintHasher::default();
                hash_image(&mut hash, image);
                element(Some(hash.finish()), image_reach(image));
            }
        }
        #[cfg(not(any(feature = "image", feature = "svg")))]
        let _ = images;

        state.usize(text.len());
        for item in text {
            match item {
                text::Item::Group {
                    transformation,
                    text,
                } => {
                    state.usize(text.len());
                    for text in text {
                        let mut hash = PaintHasher::default();
                        hash.transformation(*transformation);
                        let owned = hash_text(&mut hash, text, held.as_deref_mut());
                        element(
                            owned.then(|| hash.finish()),
                            text_reach(text) * *transformation,
                        );
                    }
                }
                text::Item::Cached {
                    transformation,
                    cache,
                } => {
                    let mut hash = PaintHasher::default();
                    hash.transformation(*transformation);
                    let (id, version) = cache.identity();
                    hash.u64(id);
                    hash.usize(version);
                    element(Some(hash.finish()), *bounds);
                }
            }
        }
        Some(state.finish())
    }

    /// Turns and scales this layer's meshes by `rotation` and `scale` about
    /// `origin` (window coordinates), the picture an offscreen group
    /// composited with them would show, when that is exactly the same: the
    /// layer holds only solid meshes and round quads, opens no group of its
    /// own past `base`,
    /// and every mesh's geometry lies inside its clip before the turn and
    /// after it, and its clip inside the layer's. Returns whether it can; only with `apply` does it
    /// turn them (enzUI-ICED #3668).
    pub(crate) fn turn_meshes(
        &mut self,
        base: usize,
        scale: f32,
        rotation: f32,
        origin: Point,
        apply: bool,
    ) -> bool {
        // Canvas geometry brings an empty text group along.
        let no_text = self
            .text
            .iter()
            .all(|item| matches!(item, text::Item::Group { text, .. } if text.is_empty()));
        if !self.quads.all_round()
            || !no_text
            || !self.primitives.is_empty()
            || !self.materials.is_empty()
            || !self.pending_meshes.is_empty()
            || !self.pending_text.is_empty()
            || self.alpha_groups.len() > base
            || self.clip_radius.is_some()
        {
            return false;
        }
        #[cfg(any(feature = "image", feature = "svg"))]
        if !self.images.is_empty() {
            return false;
        }
        let (sin, cos) = rotation.sin_cos();
        // A mesh's local space maps to the window by a uniform scale `k` and
        // a translation `t`; the turn about `origin` is then a turn about
        // `(origin - t) / k` in local space.
        let local = |item: Transformation, mesh: &Mesh| -> Option<(f32, Vector)> {
            let matrix: [f32; 16] = (item * mesh.transformation()).into();
            (matrix[1] == 0.0 && matrix[4] == 0.0 && matrix[0] > 0.0 && matrix[0] == matrix[5])
                .then(|| (matrix[0], Vector::new(matrix[12], matrix[13])))
        };
        let turn = |point: Point, center: Point| {
            let (x, y) = ((point.x - center.x) * scale, (point.y - center.y) * scale);
            Point::new(center.x + x * cos - y * sin, center.y + x * sin + y * cos)
        };
        // To a thousandth of a pixel: geometry that reaches its clip's edge
        // does so with rounding either way.
        let within = |inner: Rectangle, outer: Rectangle| {
            const SLACK: f32 = 1e-3;
            inner.x >= outer.x - SLACK
                && inner.y >= outer.y - SLACK
                && inner.x + inner.width <= outer.x + outer.width + SLACK
                && inner.y + inner.height <= outer.y + outer.height + SLACK
        };
        // Everything is checked before anything changes.
        for item in &self.triangles {
            let triangle::Item::Group {
                transformation,
                meshes,
            } = item
            else {
                return false;
            };
            for mesh in meshes {
                let Mesh::Solid {
                    buffers,
                    clip_bounds,
                    ..
                } = mesh
                else {
                    return false;
                };
                let Some((k, t)) = local(*transformation, mesh) else {
                    return false;
                };
                let Some(drawn) = buffers
                    .vertices
                    .iter()
                    .map(|vertex| {
                        Rectangle::new(
                            Point::new(vertex.position[0], vertex.position[1]),
                            Size::ZERO,
                        )
                    })
                    .reduce(|a, b| a.union(&b))
                else {
                    continue;
                };
                let center = Point::new((origin.x - t.x) / k, (origin.y - t.y) / k);
                let to_window = |rect: Rectangle| {
                    Rectangle::new(
                        Point::new(rect.x * k + t.x, rect.y * k + t.y),
                        Size::new(rect.width * k, rect.height * k),
                    )
                };
                // Neither the turn nor the clips cut anything, so the
                // geometry may turn under unchanged clips: at any angle it
                // stays within its farthest vertex's distance of the centre.
                let reach = buffers
                    .vertices
                    .iter()
                    .map(|vertex| {
                        Vector::new(vertex.position[0] - center.x, vertex.position[1] - center.y)
                    })
                    .map(|offset| offset.x.hypot(offset.y))
                    .fold(0.0_f32, f32::max)
                    * scale;
                let turned = Rectangle::new(
                    Point::new(center.x - reach, center.y - reach),
                    Size::new(2.0 * reach, 2.0 * reach),
                );
                if !within(drawn, *clip_bounds)
                    || !within(turned, *clip_bounds)
                    || !within(to_window(*clip_bounds), self.bounds)
                {
                    return false;
                }
            }
        }
        // Round quads keep within their farthest reach of the centre.
        if !self.quads.is_empty() {
            let reach = self.quads.reach_from(origin) * scale;
            let turned = Rectangle::new(
                Point::new(origin.x - reach, origin.y - reach),
                Size::new(2.0 * reach, 2.0 * reach),
            );
            if !within(turned, self.bounds) {
                return false;
            }
        }
        if !apply {
            return true;
        }
        self.quads.turn_round(origin, scale, sin, cos);
        for item in &mut self.triangles {
            let triangle::Item::Group {
                transformation,
                meshes,
            } = item
            else {
                continue;
            };
            for mesh in meshes {
                let Some((k, t)) = local(*transformation, mesh) else {
                    continue;
                };
                let center = Point::new((origin.x - t.x) / k, (origin.y - t.y) / k);
                if let Mesh::Solid { buffers, .. } = mesh {
                    for vertex in &mut buffers.vertices {
                        let point =
                            turn(Point::new(vertex.position[0], vertex.position[1]), center);
                        vertex.position = [point.x, point.y];
                    }
                }
            }
        }
        true
    }

    /// Restores the live presentation clip before replaying a retained copy.
    ///
    /// `clip_map` carries the recorded clip into the space the replayed layers
    /// are clipped in: the node's ancestors' transform, pulled back through the
    /// compositor group replay is about to open (#2251).
    ///
    /// `enclosing_map` maps the clip around the layer, which moves only with
    /// the nodes that enclosed it when it opened.
    pub(crate) fn restore_presentation_clip(
        &mut self,
        clip_map: crate::ReplayAffine,
        enclosing_map: impl Fn(usize) -> crate::ReplayAffine,
    ) {
        if let Some(bounds) = self.presentation_clip {
            let bounds = clip_map.map_rect(bounds);
            self.presentation_clip = Some(bounds);
            let enclosing_map = enclosing_map(self.enclosing_depth);
            self.enclosing_clip = self.enclosing_clip.map(|clip| enclosing_map.map_rect(clip));
            self.bounds = bounds;
            self.clip_radius = self.presentation_clip_radius;
        }
    }

    /// Replaces only an explicitly retained presentation clip.
    ///
    /// Retained ink is recorded past the ancestors' clip, so that clip is
    /// applied here, after the node moved.
    ///
    /// Returns `false` for an ordinary/descendant layer, whose clip must only
    /// ever be intersected by the caller.
    pub(crate) fn replace_presentation_clip(&mut self, clip: Rectangle) -> bool {
        if self.presentation_clip.is_none() {
            return false;
        }

        let clip = match self.enclosing_clip {
            Some(enclosing) => clip.intersection(&enclosing).unwrap_or(Rectangle {
                x: clip.x.max(enclosing.x),
                y: clip.y.max(enclosing.y),
                width: 0.0,
                height: 0.0,
            }),
            None => clip,
        };
        self.bounds = self
            .retained_bounds
            .intersection(&clip)
            .unwrap_or(Rectangle {
                x: self.retained_bounds.x.max(clip.x),
                y: self.retained_bounds.y.max(clip.y),
                width: 0.0,
                height: 0.0,
            });
        self.clip_radius = self.retained_clip_radius;
        true
    }

    /// Records how many recorded nodes enclosed this layer's enclosing clip.
    pub(crate) fn set_enclosing_depth(&mut self, depth: usize) {
        self.enclosing_depth = depth;
    }

    /// The rounded clip a captured copy of this layer keeps.
    pub(crate) fn retained_rounded_clip(&self) -> Option<(Rectangle, border::Radius)> {
        self.retained_clip_radius
            .map(|radius| (self.retained_bounds, radius))
    }

    pub(crate) fn map_layout_clip(&mut self, map: crate::ReplayAffine) {
        if self.clip_path.is_some() {
            self.bounds = map.map_rect(self.bounds);
            self.retained_bounds = map.map_rect(self.retained_bounds);
            self.clip_radius = self.clip_radius.map(|radius| radius * map.scale);
            self.retained_clip_radius = self.retained_clip_radius.map(|radius| radius * map.scale);
        }
    }

    pub(crate) fn owned_layout_clip(&mut self, clip: Rectangle, radius: Option<border::Radius>) {
        self.owned_scroll_clip(clip);
        self.clip_radius = radius;
        self.retained_clip_radius = radius;
    }

    pub(crate) fn owned_scroll_clip(&mut self, clip: Rectangle) {
        self.bounds = clip;
        self.retained_bounds = clip;
        self.presentation_clip = None;
        self.presentation_clip_radius = None;
        self.clip_radius = None;
        self.retained_clip_radius = None;
        self.fixed_replay_clip = true;
    }
    pub(crate) fn normalize_scroll(
        &mut self,
        offset: core::Vector,
        source_clip: Rectangle,
        coverage: Rectangle,
    ) -> Result<(), core::renderer::presentation::Error> {
        use core::renderer::presentation::Error;
        if (self.bounds != source_clip
            && !(self.bounds.width <= 0.0
                && self.bounds.height <= 0.0
                && source_clip.width <= 0.0
                && source_clip.height <= 0.0))
            || (self.clip_radius.is_some() && self.clip_path.is_none())
            || self.presentation_clip.is_some()
            || !self.triangles.is_empty()
            || !self.primitives.is_empty()
            || !self.images.is_empty()
        {
            return Err(Error::OpaquePaint);
        }
        self.flush_text();
        self.quads.translate(offset);
        for item in &mut self.text {
            let text::Item::Group {
                transformation,
                text,
            } = item
            else {
                return Err(Error::OpaquePaint);
            };
            let group = Transformation::translate(offset.x, offset.y) * *transformation;
            for section in text {
                match section {
                    Text::Paragraph { transformation, .. } => {
                        *transformation = group * *transformation;
                        if *transformation != Transformation::IDENTITY {
                            return Err(Error::OpaquePaint);
                        }
                    }
                    Text::Raw {
                        raw,
                        transformation,
                        ..
                    } => {
                        let combined = group * *transformation;
                        if combined.scale_factor() != 1.0 {
                            return Err(Error::OpaquePaint);
                        }
                        raw.position = raw.position * combined;
                        raw.clip_bounds = raw.clip_bounds * combined;
                        *transformation = Transformation::IDENTITY;
                    }
                    _ => return Err(Error::OpaquePaint),
                }
            }
            *transformation = Transformation::IDENTITY;
        }
        self.owned_scroll_clip(coverage);
        Ok(())
    }

    /// Takes what this layer paints, leaving it painting nothing until
    /// [`Self::restore_paint`].
    pub(crate) fn take_paint(&mut self) -> Paint {
        Paint {
            materials: std::mem::take(&mut self.materials),
            quads: std::mem::take(&mut self.quads),
            fixed_layout_quads: std::mem::take(&mut self.fixed_layout_quads),
            triangles: std::mem::take(&mut self.triangles),
            primitives: std::mem::take(&mut self.primitives),
            images: std::mem::take(&mut self.images),
            text: std::mem::take(&mut self.text),
            pending_meshes: std::mem::take(&mut self.pending_meshes),
            pending_text: std::mem::take(&mut self.pending_text),
        }
    }

    /// Gives back what [`Self::take_paint`] took.
    pub(crate) fn restore_paint(&mut self, paint: Paint) {
        let Paint {
            materials,
            quads,
            fixed_layout_quads,
            triangles,
            primitives,
            images,
            text,
            pending_meshes,
            pending_text,
        } = paint;
        self.materials = materials;
        self.quads = quads;
        self.fixed_layout_quads = fixed_layout_quads;
        self.triangles = triangles;
        self.primitives = primitives;
        self.images = images;
        self.text = text;
        self.pending_meshes = pending_meshes;
        self.pending_text = pending_text;
    }

    pub(crate) fn clear_paint(&mut self) {
        self.materials.clear();
        self.quads.clear();
        self.fixed_layout_quads = 0;
        self.triangles.clear();
        self.primitives.clear();
        self.images.clear();
        self.text.clear();
        self.pending_meshes.clear();
        self.pending_text.clear();
    }

    /// Whether primitives are still queued for this layer's batches.
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending_meshes.is_empty() || !self.pending_text.is_empty()
    }

    pub(crate) fn is_fixed_layout_paint(&self) -> bool {
        self.fixed_layout_quads > 0
            && self.fixed_layout_quads == self.quads.len()
            && self.text.is_empty()
            && self.triangles.is_empty()
            && self.primitives.is_empty()
            && self.images.is_empty()
            && self.pending_meshes.is_empty()
            && self.pending_text.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
            && self.quads.is_empty()
            && self.triangles.is_empty()
            && self.primitives.is_empty()
            && self.images.is_empty()
            && self.text.is_empty()
            && self.pending_meshes.is_empty()
            && self.pending_text.is_empty()
    }

    pub fn draw_quad(
        &mut self,
        quad: renderer::Quad,
        background: Background,
        transformation: Transformation,
    ) {
        let bounds = quad.bounds * transformation;

        let quad = Quad {
            position: [bounds.x, bounds.y],
            size: [bounds.width, bounds.height],
            border_color: color::pack(quad.border.color),
            border_radius: (quad.border.radius * transformation.scale_factor()).into(),
            border_width: quad.border.width * transformation.scale_factor(),
            shadow_color: color::pack(quad.shadow.color),
            shadow_offset: (quad.shadow.offset * transformation.scale_factor()).into(),
            shadow_blur_radius: quad.shadow.blur_radius * transformation.scale_factor(),
            snap: quad.snap as u32,
        };

        self.quads.add(quad, &background);
    }

    pub fn draw_paragraph(
        &mut self,
        paragraph: &Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let paragraph = Text::Paragraph {
            paragraph: paragraph.downgrade(),
            // Per-frame recording borrows; `Text::retain` (#2219) is what takes
            // ownership when a scene must outlive the frame.
            retained: None,
            position,
            color,
            clip_bounds,
            transformation,
        };

        self.pending_text.push(paragraph);
    }

    pub fn draw_editor(
        &mut self,
        editor: &Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let editor = Text::Editor {
            editor: editor.downgrade(),
            position,
            color,
            clip_bounds,
            transformation,
        };

        self.pending_text.push(editor);
    }

    pub fn draw_text(
        &mut self,
        text: crate::core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let text = Text::Cached {
            content: text.content,
            bounds: Rectangle::new(position, text.bounds) * transformation,
            color,
            size: text.size * transformation.scale_factor(),
            line_height: text.line_height.to_absolute(text.size) * transformation.scale_factor(),
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            wrapping: text.wrapping,
            ellipsis: text.ellipsis,
            clip_bounds: clip_bounds * transformation,
        };

        self.pending_text.push(text);
    }

    pub fn draw_text_raw(&mut self, raw: graphics::text::Raw, transformation: Transformation) {
        let raw = Text::Raw {
            raw,
            // Per-frame recording borrows; `Text::retain` (#2219) upgrades to a
            // strong buffer when a scene must outlive the frame.
            retained: None,
            transformation,
        };

        self.pending_text.push(raw);
    }

    pub fn draw_image(&mut self, image: Image, transformation: Transformation) {
        match image {
            Image::Raster {
                image,
                bounds,
                clip_bounds,
            } => {
                self.draw_raster(image, bounds, clip_bounds, transformation);
            }
            Image::Vector {
                svg,
                bounds,
                clip_bounds,
            } => {
                self.draw_svg(svg, bounds, clip_bounds, transformation);
            }
        }
    }

    pub fn draw_raster(
        &mut self,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let image = Image::Raster {
            image: core::Image {
                border_radius: image.border_radius * transformation.scale_factor(),
                ..image
            },
            bounds: bounds * transformation,
            clip_bounds: clip_bounds * transformation,
        };

        self.images.push(image);
    }

    pub fn draw_svg(
        &mut self,
        svg: Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let svg = Image::Vector {
            svg,
            bounds: bounds * transformation,
            clip_bounds: clip_bounds * transformation,
        };

        self.images.push(svg);
    }

    pub fn draw_mesh(&mut self, mut mesh: Mesh, transformation: Transformation) {
        match &mut mesh {
            Mesh::Solid {
                transformation: local_transformation,
                ..
            }
            | Mesh::Gradient {
                transformation: local_transformation,
                ..
            } => {
                *local_transformation = *local_transformation * transformation;
            }
        }

        self.pending_meshes.push(mesh);
    }

    pub fn draw_mesh_group(&mut self, meshes: Vec<Mesh>, transformation: Transformation) {
        self.flush_meshes();

        self.triangles.push(triangle::Item::Group {
            meshes,
            transformation,
        });
    }

    pub fn draw_mesh_cache(&mut self, cache: mesh::Cache, transformation: Transformation) {
        self.flush_meshes();

        self.triangles.push(triangle::Item::Cached {
            cache,
            transformation,
        });
    }

    pub fn draw_text_group(&mut self, text: Vec<Text>, transformation: Transformation) {
        self.flush_text();

        self.text.push(text::Item::Group {
            text,
            transformation,
        });
    }

    pub fn draw_text_cache(&mut self, cache: text::Cache, transformation: Transformation) {
        self.flush_text();

        self.text.push(text::Item::Cached {
            cache,
            transformation,
        });
    }

    pub fn draw_primitive(
        &mut self,
        bounds: Rectangle,
        primitive: impl Primitive,
        transformation: Transformation,
    ) {
        let bounds = bounds * transformation;

        self.primitives
            .push(primitive::Instance::new(bounds, primitive));
    }

    #[allow(clippy::drain_collect)] // We want to reuse `Layer` capacity
    fn flush_meshes(&mut self) {
        if !self.pending_meshes.is_empty() {
            self.triangles.push(triangle::Item::Group {
                transformation: Transformation::IDENTITY,
                meshes: self.pending_meshes.drain(..).collect(),
            });
        }
    }

    #[allow(clippy::drain_collect)] // We want to reuse `Layer` capacity
    fn flush_text(&mut self) {
        if !self.pending_text.is_empty() {
            self.text.push(text::Item::Group {
                transformation: Transformation::IDENTITY,
                text: self.pending_text.drain(..).collect(),
            });
        }
    }
}

impl Layer {
    pub(crate) fn retain(&self) -> Result<Layer, crate::core::renderer::presentation::Error> {
        fn retain_text(
            text: &[Text],
        ) -> Result<Vec<Text>, crate::core::renderer::presentation::Error> {
            text.iter().map(crate::text::retain_section).collect()
        }

        let mut retained = self.clone();
        // Only the captured copy promotes retained bounds; live drawing keeps its own clip.
        retained.bounds = retained.retained_bounds;
        retained.clip_radius = retained.retained_clip_radius;
        for group in &mut retained.alpha_groups {
            if let Some(clip) = group.retained_clip {
                group.clip = Some(clip);
            }
        }

        for item in &mut retained.text {
            match item {
                text::Item::Group { text, .. } => {
                    *text = retain_text(text)?;
                }
                // capture_fonts materializes cached runs before scene publication.
                text::Item::Cached { .. } => {}
            }
        }

        retained.pending_text = retain_text(&retained.pending_text)?;

        Ok(retained)
    }

    /// Whether every recorded command in this layer owns its content.
    pub(crate) fn is_retained(&self) -> bool {
        let owned = |text: &Vec<Text>| text.iter().all(Text::is_retained);

        self.text.iter().all(|item| match item {
            text::Item::Group { text, .. } => owned(text),
            text::Item::Cached { .. } => true,
        }) && owned(&self.pending_text)
    }

    /// Translates retained primitives; uniform scale remains a compositor operation.
    ///
    /// Declared layout clips already carry sampled geometry and are unchanged here.
    /// Other enclosing clips use `fixed_clip` after ancestor mapping; unowned
    /// descendant clips travel with their primitives. Returns false when a travelling
    /// descendant clip leaves `fixed_clip` entirely.
    pub fn translate(&mut self, translation: core::Vector, fixed_clip: Rectangle) -> bool {
        self.flush_meshes();
        self.flush_text();

        if self.clip_path.is_none() {
            self.retained_bounds += translation;
        }

        if self.clip_path.is_none() {
            if let Some(presentation) = self.presentation_clip {
                // The presentation clip already includes ancestor transforms, not the node's travel.
                self.bounds = presentation;
            } else if self.fixed_replay_clip {
                self.bounds = fixed_clip;
            } else {
                let travelled = self.bounds + translation;

                match travelled.intersection(&fixed_clip) {
                    Some(bounds) => self.bounds = bounds,
                    None => return false,
                }
            }
        }

        for region in &mut self.materials {
            region.translate(translation);
        }

        self.quads.translate(translation);

        for item in &mut self.triangles {
            item.translate(translation);
        }

        for item in &mut self.text {
            item.translate(translation);
        }

        for image in &mut self.images {
            match image {
                graphics::Image::Raster {
                    bounds,
                    clip_bounds,
                    ..
                }
                | graphics::Image::Vector {
                    bounds,
                    clip_bounds,
                    ..
                } => {
                    *bounds += translation;
                    *clip_bounds += translation;
                }
            }
        }

        for instance in &mut self.primitives {
            instance.bounds += translation;
        }

        true
    }
}

impl graphics::Layer for Layer {
    fn with_bounds(bounds: Rectangle) -> Self {
        Self {
            bounds,
            retained_bounds: bounds,
            presentation_clip: None,
            presentation_clip_radius: None,
            retained_clip_radius: None,
            ..Self::default()
        }
    }

    fn with_clip_radius(bounds: Rectangle, radius: border::Radius) -> Self {
        Self {
            bounds,
            retained_bounds: bounds,
            presentation_clip: None,
            presentation_clip_radius: None,
            retained_clip_radius: Some(radius),
            clip_radius: Some(radius),
            ..Self::default()
        }
    }

    fn set_clip_radius(&mut self, radius: Option<border::Radius>) {
        self.clip_radius = radius;
    }

    fn clip_radius(&self) -> Option<border::Radius> {
        self.clip_radius
    }

    fn bounds(&self) -> Rectangle {
        self.bounds
    }

    fn retained_bounds(&self) -> Rectangle {
        self.retained_bounds
    }

    fn retained_clip_radius(&self) -> Option<border::Radius> {
        self.retained_clip_radius
    }

    fn presentation_clip(&self) -> Option<Rectangle> {
        self.presentation_clip
    }

    fn resize_retained(&mut self, visible: Rectangle, retained: Rectangle) {
        self.clip_path = None;
        self.bounds = visible;
        self.retained_bounds = retained;
    }

    fn set_clip_radii(
        &mut self,
        visible: Option<border::Radius>,
        retained: Option<border::Radius>,
    ) {
        self.clip_radius = visible;
        self.retained_clip_radius = retained;
    }

    fn set_presentation_clip(&mut self, presentation: Option<Rectangle>) {
        self.presentation_clip = presentation;
        self.presentation_clip_radius = presentation.and(self.clip_radius);
    }

    fn enclosing_clip(&self) -> Option<Rectangle> {
        self.enclosing_clip
    }

    fn set_enclosing_clip(&mut self, enclosing: Option<Rectangle>) {
        self.enclosing_clip = enclosing;
        self.enclosing_depth = 0;
    }

    fn set_fixed_replay_clip(&mut self, fixed: bool) {
        self.fixed_replay_clip = fixed;
    }

    fn semantic_clip(&self) -> Option<std::sync::Arc<graphics::layer::ClipPath>> {
        self.clip_path.clone()
    }

    fn set_semantic_clip(&mut self, clip: Option<std::sync::Arc<graphics::layer::ClipPath>>) {
        self.clip_path = clip;
    }

    fn flush(&mut self) {
        self.flush_meshes();
        self.flush_text();
    }

    fn resize(&mut self, bounds: Rectangle) {
        self.clip_path = None;
        self.bounds = bounds;
        self.retained_bounds = bounds;
        self.presentation_clip = None;
        self.presentation_clip_radius = None;
        self.fixed_replay_clip = false;
        self.enclosing_clip = None;
        self.retained_clip_radius = self.clip_radius;
    }

    fn reset(&mut self) {
        self.clip_path = None;
        self.bounds = Rectangle::INFINITE;
        self.retained_bounds = Rectangle::INFINITE;
        self.presentation_clip = None;
        self.presentation_clip_radius = None;
        self.fixed_replay_clip = false;
        self.enclosing_clip = None;
        self.retained_clip_radius = None;
        self.clip_radius = None;
        self.alpha_groups.clear();
        self.clip_group = None;
        self.clip_sources.clear();
        self.alpha_segment = 0;

        self.clear_paint();
    }

    fn start(&self) -> usize {
        // A material boundary is ordered: no neighbour may merge across it, in
        // either direction, or foreground paint would land in a backdrop.
        if !self.materials.is_empty() {
            return 0;
        }

        if !self.quads.is_empty() {
            return 1;
        }

        if !self.triangles.is_empty() {
            return 2;
        }

        if !self.primitives.is_empty() {
            return 3;
        }

        if !self.images.is_empty() {
            return 4;
        }

        if !self.text.is_empty() {
            return 5;
        }

        usize::MAX
    }

    fn end(&self) -> usize {
        if !self.materials.is_empty() {
            return 6;
        }

        if !self.text.is_empty() {
            return 5;
        }

        if !self.images.is_empty() {
            return 4;
        }

        if !self.primitives.is_empty() {
            return 3;
        }

        if !self.triangles.is_empty() {
            return 2;
        }

        if !self.quads.is_empty() {
            return 1;
        }

        0
    }

    fn can_merge(&self, other: &Self) -> bool {
        self.materials.is_empty()
            && other.materials.is_empty()
            && (matches!((&self.clip_path, &other.clip_path), (None, None))
                || self.same_native_batch(other))
            && self.alpha_groups == other.alpha_groups
            && self.clip_group == other.clip_group
            && self.alpha_segment == other.alpha_segment
            && self.retained_bounds == other.retained_bounds
            && self.presentation_clip == other.presentation_clip
            && self.presentation_clip_radius == other.presentation_clip_radius
            && self.fixed_replay_clip == other.fixed_replay_clip
            && self.enclosing_clip == other.enclosing_clip
            && self.retained_clip_radius == other.retained_clip_radius
            && self.clip_radius == other.clip_radius
    }

    fn same_native_batch(&self, other: &Self) -> bool {
        match (&self.clip_path, &other.clip_path) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(
                graphics::layer::ClipPath::paint_allocation(a),
                graphics::layer::ClipPath::paint_allocation(b),
            ),
            _ => false,
        }
    }

    fn discard_invisible_paint(&mut self) {
        self.clear_paint();
    }

    fn merge(&mut self, layer: &mut Self) {
        self.quads.append(&mut layer.quads);
        self.fixed_layout_quads += std::mem::take(&mut layer.fixed_layout_quads);
        self.triangles.append(&mut layer.triangles);
        self.primitives.append(&mut layer.primitives);
        self.images.append(&mut layer.images);
        for mut item in layer.text.drain(..) {
            item.bake_translation();
            if let Some(previous) = self.text.last_mut() {
                previous.bake_translation();
            }
            if let Some(text::Item::Group {
                transformation,
                text,
            }) = self.text.last_mut()
                && let text::Item::Group {
                    transformation: next_transformation,
                    text: next,
                } = &mut item
                && transformation == next_transformation
            {
                text.append(next);
            } else {
                self.text.push(item);
            }
        }
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            clip_path: None,
            bounds: Rectangle::INFINITE,
            retained_bounds: Rectangle::INFINITE,
            presentation_clip: None,
            presentation_clip_radius: None,
            fixed_replay_clip: false,
            enclosing_clip: None,
            enclosing_depth: 0,
            retained_clip_radius: None,
            clip_radius: None,
            alpha_groups: Vec::new(),
            clip_group: None,
            clip_sources: Vec::new(),
            alpha_segment: 0,
            materials: Vec::new(),
            quads: quad::Batch::default(),
            fixed_layout_quads: 0,
            triangles: triangle::Batch::default(),
            primitives: primitive::Batch::default(),
            text: text::Batch::default(),
            images: image::Batch::default(),
            pending_meshes: Vec::new(),
            pending_text: Vec::new(),
        }
    }
}

/// The rectangle a quad paints within, its shadow included.
pub(crate) fn quad_reach(quad: &Quad) -> Rectangle {
    let rect = Rectangle::new(
        Point::new(quad.position[0], quad.position[1]),
        Size::new(quad.size[0], quad.size[1]),
    );
    if quad.shadow_color.components()[3] > 0.0 {
        let reach = quad.shadow_blur_radius.max(0.0) * 2.0
            + quad.shadow_offset[0].abs().max(quad.shadow_offset[1].abs());
        rect.expand(reach + 1.0)
    } else {
        rect
    }
}

/// The rectangle a text command paints within, before its item's
/// transformation.
pub(crate) fn text_reach(text: &Text) -> Rectangle {
    match text {
        // A paragraph paints its lines (its minimum bounds) from `position`:
        // left-aligned and top-aligned lines start there, other alignments
        // place them anywhere in its available bounds. Lines wider or taller
        // than those bounds overflow them, to either side when not aligned
        // to the start, and glyphs may reach a little past their lines.
        Text::Paragraph {
            paragraph: weak,
            retained,
            position,
            clip_bounds,
            transformation,
            ..
        } => {
            use crate::core::alignment::Vertical;
            use crate::core::text::{Alignment, Paragraph as _};

            let Some(paragraph) = retained.clone().or_else(|| weak.upgrade()) else {
                return *clip_bounds * *transformation;
            };

            let bounds = paragraph.bounds();
            let lines = paragraph.min_bounds();
            let span = |starts: bool, available: f32, used: f32| {
                if starts {
                    (0.0, used)
                } else {
                    let overflow = (used - available).max(0.0);
                    (-overflow, available.max(used) + 2.0 * overflow)
                }
            };
            let (x, width) = span(
                matches!(
                    paragraph.align_x(),
                    Alignment::Default | Alignment::Left | Alignment::Justified
                ),
                bounds.width,
                lines.width,
            );
            let (y, height) = span(
                paragraph.align_y() == Vertical::Top,
                bounds.height,
                lines.height,
            );
            let pad = (lines.height * 0.25).clamp(3.0, 12.0) + 2.0;
            let painted = Rectangle::new(
                Point::new(position.x + x - pad, position.y + y - pad),
                Size::new(width + 2.0 * pad, height + 2.0 * pad),
            );
            painted.intersection(clip_bounds).unwrap_or(Rectangle::new(
                Point::new(clip_bounds.x, clip_bounds.y),
                Size::ZERO,
            )) * *transformation
        }
        Text::Editor {
            clip_bounds,
            transformation,
            ..
        } => *clip_bounds * *transformation,
        Text::Cached { clip_bounds, .. } => *clip_bounds,
        Text::Raw {
            raw,
            transformation,
            ..
        } => raw.clip_bounds * *transformation,
    }
}

/// Where a text command's glyphs paint: for a paragraph, from its laid-out
/// lines (exact whatever its alignment, as the glyph pass places them from
/// `position`), else [`text_reach`]. A paragraph its widget no longer holds
/// falls back too.
pub(crate) fn text_glyph_reach(text: &Text) -> Rectangle {
    use crate::core::text::Paragraph as _;

    let Text::Paragraph {
        paragraph,
        retained,
        position,
        clip_bounds,
        transformation,
        ..
    } = text
    else {
        return text_reach(text);
    };
    let Some(paragraph) = retained.clone().or_else(|| paragraph.upgrade()) else {
        return text_reach(text);
    };
    let buffer = paragraph.buffer();
    // Glyphs are laid out at the hinted size and scaled back when drawn.
    let unhint = paragraph.hint_factor().unwrap_or(1.0).max(f32::EPSILON);
    let scroll = buffer.scroll().horizontal;
    let mut lines: Option<Rectangle> = None;
    let mut line_height: f32 = 0.0;
    for run in buffer.layout_runs() {
        let (left, right) = run
            .glyphs
            .iter()
            .fold((f32::MAX, f32::MIN), |(left, right), glyph| {
                (left.min(glyph.x), right.max(glyph.x + glyph.w))
            });
        if left > right {
            continue;
        }
        let line = Rectangle::new(
            Point::new((left - scroll) / unhint, run.line_top / unhint),
            Size::new((right - left) / unhint, run.line_height / unhint),
        );
        line_height = line_height.max(line.height);
        lines = Some(lines.map_or(line, |lines| lines.union(&line)));
    }
    let Some(lines) = lines else {
        // Nothing laid out paints nothing.
        return Rectangle::new(*position, Size::ZERO) * *transformation;
    };
    // Glyph ink may reach past its line box (accents, descenders) and, less
    // far, past its advance (italics, side bearings), by a share of a line;
    // the position is also nudged by up to half a pixel when hinted.
    let pad_y = (line_height * 0.25).clamp(2.0, 8.0) + 1.0;
    let pad_x = (line_height * 0.2).clamp(1.0, 6.0) + 1.0;
    let painted = Rectangle::new(
        Point::new(position.x + lines.x - pad_x, position.y + lines.y - pad_y),
        Size::new(lines.width + 2.0 * pad_x, lines.height + 2.0 * pad_y),
    );
    painted.intersection(clip_bounds).unwrap_or(Rectangle::new(
        Point::new(clip_bounds.x, clip_bounds.y),
        Size::ZERO,
    )) * *transformation
}

/// The rectangle an image paints within.
#[cfg(any(feature = "image", feature = "svg"))]
pub(crate) fn image_reach(image: &Image) -> Rectangle {
    match image {
        #[cfg(feature = "image")]
        Image::Raster {
            bounds,
            clip_bounds,
            ..
        } => bounds.intersection(clip_bounds).unwrap_or(*bounds),
        #[cfg(feature = "svg")]
        Image::Vector {
            bounds,
            clip_bounds,
            ..
        } => bounds.intersection(clip_bounds).unwrap_or(*bounds),
    }
}

#[cfg(any(feature = "image", feature = "svg"))]
fn hash_image(state: &mut PaintHasher, image: &Image) {
    match image {
        #[cfg(feature = "image")]
        Image::Raster {
            image,
            bounds,
            clip_bounds,
        } => {
            let core::Image {
                handle,
                filter_method,
                rotation,
                border_radius,
                opacity,
            } = image;
            state.usize(0);
            state.hash(&handle.id());
            state.hash(filter_method);
            state.f32(rotation.0);
            state.radius(*border_radius);
            state.f32(*opacity);
            state.rectangle(*bounds);
            state.rectangle(*clip_bounds);
        }
        #[cfg(feature = "svg")]
        Image::Vector {
            svg,
            bounds,
            clip_bounds,
        } => {
            let Svg {
                handle,
                color,
                rotation,
                opacity,
            } = svg;
            state.usize(1);
            state.u64(handle.id());
            state.option(*color, PaintHasher::color);
            state.f32(rotation.0);
            state.f32(*opacity);
            state.rectangle(*bounds);
            state.rectangle(*clip_bounds);
        }
    }
}

fn hash_material(state: &mut PaintHasher, material: &crate::core::material::Material) {
    let crate::core::material::Material {
        radii,
        blur_sigma,
        refraction,
        bevel_width,
        index_of_refraction,
        dispersion,
        color_diffusion,
        color_reach,
        optical_depth,
        tint,
        tint_opacity,
        saturation,
        highlight,
        brightness,
        light_angle,
        profile_curvature,
        contrast_protection,
        protected_inset,
        probe,
        measure_only,
    } = *material;
    for value in radii.into_iter().chain([
        blur_sigma,
        refraction,
        bevel_width,
        index_of_refraction,
        dispersion,
        color_diffusion,
        color_reach,
        optical_depth,
        tint_opacity,
        saturation,
        highlight,
        brightness,
        light_angle,
        profile_curvature,
        contrast_protection,
        protected_inset,
    ]) {
        state.f32(value);
    }
    state.color(tint);
    state.u64(probe);
    state.usize(usize::from(measure_only));
}

fn hash_mesh(state: &mut PaintHasher, mesh: &Mesh) {
    match mesh {
        Mesh::Solid {
            buffers,
            transformation,
            clip_bounds,
        } => {
            state.usize(0);
            state.bytes(bytemuck::cast_slice(&buffers.vertices));
            state.bytes(bytemuck::cast_slice(&buffers.indices));
            state.transformation(*transformation);
            state.rectangle(*clip_bounds);
        }
        Mesh::Gradient {
            buffers,
            transformation,
            clip_bounds,
        } => {
            state.usize(1);
            state.bytes(bytemuck::cast_slice(&buffers.vertices));
            state.bytes(bytemuck::cast_slice(&buffers.indices));
            state.transformation(*transformation);
            state.rectangle(*clip_bounds);
        }
    }
}

/// Hashes a recorded text command; `false` for text the recording does not
/// own, which can change without a new recording.
fn hash_text(state: &mut PaintHasher, text: &Text, held: Option<&mut Held>) -> bool {
    // A widget's paragraph is changed only into a new allocation while a
    // weak reference to it lives, and `held` keeps one.
    if let (
        Text::Paragraph {
            paragraph,
            retained: None,
            position,
            color,
            clip_bounds,
            transformation,
        },
        Some(held),
    ) = (text, held)
    {
        state.usize(paragraph.as_ptr() as usize);
        held.paragraphs.push(paragraph.clone());
        state.point(*position);
        state.color(*color);
        state.rectangle(*clip_bounds);
        state.transformation(*transformation);
        return true;
    }
    match text {
        Text::Paragraph {
            paragraph,
            retained: Some(_),
            position,
            color,
            clip_bounds,
            transformation,
        } => {
            // A retained paragraph is shared, never changed in place, and
            // the recording keeps its address from being reused.
            state.usize(paragraph.as_ptr() as usize);
            state.point(*position);
            state.color(*color);
            state.rectangle(*clip_bounds);
            state.transformation(*transformation);
            true
        }
        Text::Cached {
            content,
            bounds,
            color,
            size,
            line_height,
            font,
            align_x,
            align_y,
            shaping,
            wrapping,
            ellipsis,
            clip_bounds,
        } => {
            state.hash(content);
            state.rectangle(*bounds);
            state.color(*color);
            state.f32(size.0);
            state.f32(line_height.0);
            state.hash(font);
            state.hash(align_x);
            state.hash(align_y);
            state.hash(shaping);
            state.hash(wrapping);
            state.hash(ellipsis);
            state.rectangle(*clip_bounds);
            true
        }
        Text::Paragraph { retained: None, .. } | Text::Editor { .. } | Text::Raw { .. } => false,
    }
}

#[cfg(test)]
#[path = "layer/image_batch_tests.rs"]
mod image_batch_tests;

#[cfg(test)]
mod tests {
    use super::*;
    // `start`, `end` and `can_merge` are `graphics::Layer` methods, and the
    // impl above names the trait by path rather than importing it.
    use crate::graphics::Layer as _;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    fn material() -> crate::core::material::Material {
        crate::core::material::Material {
            radii: [12.0; 4],
            blur_sigma: 8.0,
            refraction: 1.0,
            bevel_width: 16.0,
            index_of_refraction: 1.5,
            dispersion: 0.1,
            color_diffusion: 0.0,
            color_reach: 0.0,
            optical_depth: 14.0,
            tint: Color::from_rgb(0.1, 0.1, 0.1),
            tint_opacity: 0.1,
            saturation: 1.0,
            highlight: 0.2,
            brightness: 0.2,
            light_angle: 225.0,
            profile_curvature: 2.5,
            contrast_protection: 1.0,
            protected_inset: 6.0,
            probe: 0,
            measure_only: false,
        }
    }

    fn glass_layer() -> Layer {
        let mut layer = Layer::default();
        layer.materials.push(material::Region {
            bounds: rect(40.0, 60.0, 200.0, 48.0),
            material: material(),
        });
        layer
    }

    #[test]
    fn a_material_layer_refuses_to_merge_in_either_direction() {
        let glass = glass_layer();
        let plain = Layer::default();

        // The control first: two ordinary layers with the same clip state do
        // merge, so the refusals below are the material and not the fixture.
        assert!(plain.can_merge(&Layer::default()));

        assert!(
            !glass.can_merge(&plain),
            "paint after a boundary must not join the layer that opened it"
        );
        assert!(
            !plain.can_merge(&glass),
            "paint before a boundary must not join it either, or it would \
             stop being part of the backdrop"
        );
        assert!(!glass.can_merge(&glass_layer()));
    }

    #[test]
    fn a_material_layer_spans_every_batch_kind_so_nothing_reorders_across_it() {
        let glass = glass_layer();

        assert_eq!(glass.start(), 0);
        assert_eq!(
            glass.end(),
            6,
            "a boundary must span past the last batch kind, or a later text \
             batch could be hoisted in front of it"
        );

        // A layer holding only quads occupies one kind, which is what makes
        // the span above meaningful rather than universal.
        let mut quads = Layer::default();
        quads.quads.add(
            Quad {
                position: [0.0, 0.0],
                size: [10.0, 10.0],
                border_color: color::pack(Color::BLACK),
                border_radius: [0.0; 4],
                border_width: 0.0,
                shadow_color: color::pack(Color::TRANSPARENT),
                shadow_offset: [0.0, 0.0],
                shadow_blur_radius: 0.0,
                snap: 0,
            },
            &Background::Color(Color::BLACK),
        );
        assert_eq!(quads.start(), 1);
        assert_eq!(quads.end(), 1);

        let empty = Layer::default();
        assert_eq!(empty.start(), usize::MAX);
        assert_eq!(empty.end(), 0);
    }

    #[test]
    fn a_material_layer_is_not_empty_and_does_not_survive_a_reuse() {
        let mut glass = glass_layer();

        assert!(
            !glass.is_empty(),
            "a layer whose only content is a boundary must still be visited"
        );

        glass.clear_paint();
        assert!(
            glass.materials.is_empty(),
            "a reused layer must not replay last frame's glass"
        );
        assert!(glass.is_empty());
    }

    #[test]
    fn a_retained_material_replays_with_the_paint_it_belongs_to() {
        let mut recorded = glass_layer();
        let origin = recorded.materials[0].bounds;

        // A retained lane clones the live layer and replays the clone; the
        // clone must carry the boundary, or the replayed frame would paint the
        // panel's foreground with no panel under it.
        let mut replayed = recorded.clone();
        assert_eq!(replayed.materials.len(), 1);

        let travel = core::Vector::new(-12.0, 30.0);
        assert!(replayed.translate(travel, Rectangle::INFINITE));

        let moved = replayed.materials[0].bounds;
        assert_eq!(moved.x, origin.x + travel.x);
        assert_eq!(moved.y, origin.y + travel.y);
        assert_eq!(moved.width, origin.width);
        assert_eq!(moved.height, origin.height);
        assert_eq!(
            replayed.materials[0].material.blur_sigma,
            material().blur_sigma,
            "replay must not re-tune the material it is replaying"
        );

        // The recording it was cloned from is untouched, so a lane replay
        // cannot walk the live layer's regions away frame after frame.
        assert_eq!(recorded.materials[0].bounds.x, origin.x);
        assert_eq!(recorded.materials[0].bounds.y, origin.y);

        // A translation that carries the layer out of its fixed replay clip is
        // refused wholesale, regions included.
        recorded.bounds = rect(0.0, 0.0, 50.0, 50.0);
        assert!(!recorded.translate(core::Vector::new(500.0, 0.0), rect(0.0, 0.0, 100.0, 100.0)));
    }
}
