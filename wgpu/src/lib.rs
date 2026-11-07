// Modified by enzUI contributors. See FORK.md.
//! A [`wgpu`] renderer for [Iced].
//!
//! ![The native path of the Iced ecosystem](https://github.com/iced-rs/iced/blob/0525d76ff94e828b7b21634fa94a747022001c83/docs/graphs/native.png?raw=true)
//!
//! [`wgpu`] supports most modern graphics backends: Vulkan, Metal, DX11, and
//! DX12 (OpenGL and WebGL are still WIP). Additionally, it will support the
//! incoming [WebGPU API].
//!
//! Currently, `iced_wgpu` supports the following primitives:
//! - Text, which is rendered using [`glyphon`].
//! - Quads or rectangles, with rounded borders and a solid background color.
//! - Clip areas, useful to implement scrollables or hide overflowing content.
//! - Images and SVG, loaded from memory or the file system.
//! - Meshes of triangles, useful to draw geometry freely.
//!
//! [Iced]: https://github.com/iced-rs/iced
//! [`wgpu`]: https://github.com/gfx-rs/wgpu-rs
//! [WebGPU API]: https://gpuweb.github.io/gpuweb/
//! [`glyphon`]: https://github.com/grovesNL/glyphon
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/iced-rs/iced/9ab6923e943f784985e9ef9ca28b10278297225d/docs/logo.svg"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(missing_docs)]
pub mod layer;
pub mod primitive;
pub mod window;

#[cfg(feature = "geometry")]
pub mod geometry;

mod alpha;
mod buffer;
mod clear;
mod clip;
mod color;
mod damage;
mod device;
mod engine;
mod frame_dump;
mod material;
mod native_input;
mod nudge;
#[cfg(feature = "geometry")]
mod owned_border;
mod owned_clip;
#[cfg(any(feature = "image", feature = "svg"))]
mod owned_media;
#[cfg(not(any(feature = "image", feature = "svg")))]
#[path = "owned_media/null.rs"]
mod owned_media;
mod owned_quad;
mod owned_scroll;
mod quad;
mod readback_audit;
mod rounded_elision;
mod text;
mod triangle;

#[cfg(any(feature = "image", feature = "svg"))]
#[path = "image/mod.rs"]
mod image;

#[cfg(not(any(feature = "image", feature = "svg")))]
#[path = "image/null.rs"]
mod image;

use buffer::Buffer;
use std::sync::Arc;

use iced_debug as debug;
pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use wgpu;

pub use alpha::Statistics as AlphaGroupStatistics;
pub use device::{Health, imports_dma_buf};
pub use engine::Engine;
pub use layer::Layer;
pub use material::Statistics as MaterialStatistics;
pub use primitive::Primitive;
pub use quad::QuadStatistics;
pub use text::TextAtlasStatistics;

/// The renderer's own WGSL, for custom primitives that build on its
/// materials rather than copying them.
pub mod shader {
    /// The material composite: `vs_region`, the `Material` uniform at group 0
    /// binding 4, backdrop sampling (`filtered`) and colour encoding helpers.
    /// Prefix it with `const ENCODED: bool = ..;` (true unless the target
    /// format is sRGB) and append fragment entry points of your own.
    pub const MATERIAL: &str = include_str!("shader/material.wgsl");
    /// The separable backdrop blur (`vs_fullscreen`, `fs_filter`) materials
    /// use for frost; also needs the `ENCODED` prefix.
    pub const MATERIAL_FILTER: &str = include_str!("shader/material_filter.wgsl");
}

#[cfg(any(feature = "image", feature = "svg"))]
pub use image::{ImageCachePolicy, ImageCacheStatistics};

#[cfg(feature = "geometry")]
pub use geometry::Geometry;

use crate::core::renderer;
use crate::core::{Background, Color, Font, Point, Rectangle, Size, Transformation, Vector};
use crate::graphics::mesh;
use crate::graphics::text::{Editor, Paragraph};
use crate::graphics::{Shell, Viewport};

fn clip_region_of(
    layer: &Layer,
    scale_factor: f32,
    window_mask: Option<layer::AlphaGroup>,
) -> clip::ClipRegion {
    match layer.clip_radius {
        Some(radius)
            if !layer
                .alpha_groups
                .iter()
                .chain(window_mask.as_ref())
                .any(|group| {
                    Some(group.id) == layer.clip_group || group.clip == Some((layer.bounds, radius))
                }) =>
        {
            let bounds = layer.bounds * scale_factor;
            clip::ClipRegion {
                rect: [bounds.x, bounds.y, bounds.width, bounds.height],
                radius: [
                    radius.top_left * scale_factor,
                    radius.top_right * scale_factor,
                    radius.bottom_right * scale_factor,
                    radius.bottom_left * scale_factor,
                ],
            }
        }
        _ => clip::ClipRegion::NONE,
    }
}

/// Whether an outermost group can share its target with siblings: it is a
/// clip, so its paint stays inside a known rectangle, and is not scaled.
fn batchable(group: layer::AlphaGroup) -> bool {
    group.clip.is_some() && group.scale == 1.0 && group.rotation == 0.0
}

/// A rectangle that contains every pixel `layer` paints: the union of its
/// primitives' drawn bounds, each as its own renderer scissors it, landed by
/// [`land`]. Custom primitives, unflushed paint and cached text batches
/// count as the whole clip (enzUI-ICED #3666).
fn damage_extent(layer: &Layer) -> Option<Rectangle> {
    if !layer.primitives.is_empty() || layer.has_pending() {
        return Some(layer.bounds);
    }
    let mut extent: Option<Rectangle> = None;
    let mut add = |rect: Rectangle| {
        extent = Some(extent.map_or(rect, |extent| extent.union(&rect)));
    };
    for region in &layer.materials {
        add(region.bounds);
    }
    for quad in layer.quads.geometry() {
        add(layer::quad_reach(quad));
    }
    for item in layer.triangles.iter() {
        match item {
            triangle::Item::Group {
                transformation,
                meshes,
            } => {
                for mesh in meshes {
                    add(mesh.clip_bounds() * *transformation);
                }
            }
            triangle::Item::Cached {
                transformation,
                cache,
            } => {
                for mesh in cache.batch().iter() {
                    add(mesh.clip_bounds() * *transformation);
                }
            }
        }
    }
    for item in layer.text.iter() {
        match item {
            text::Item::Group {
                transformation,
                text,
            } => {
                for section in text {
                    add(layer::text_reach(section) * *transformation);
                }
            }
            text::Item::Cached { .. } => return land(layer, layer.bounds),
        }
    }
    #[cfg(any(feature = "image", feature = "svg"))]
    for image in &layer.images {
        add(layer::image_reach(image));
    }
    land(layer, extent?)
}

/// Where paint within `rect` lands on screen: within the layer's clip, then
/// moved by its scaled or turned groups, which can reach past the paint's
/// own rectangle (a turning square's corners), within their reach.
fn land(layer: &Layer, rect: Rectangle) -> Option<Rectangle> {
    let mut extent = rect.intersection(&layer.bounds)?;
    for group in layer.alpha_groups.iter().rev() {
        if (group.scale - 1.0).abs() <= f32::EPSILON && group.rotation == 0.0 {
            continue;
        }
        let landed = {
            let (sin, cos) = group.rotation.sin_cos();
            let origin = group.scale_origin;
            let corners = [
                Point::new(extent.x, extent.y),
                Point::new(extent.x + extent.width, extent.y),
                Point::new(extent.x, extent.y + extent.height),
                Point::new(extent.x + extent.width, extent.y + extent.height),
            ]
            .map(|corner| {
                let (x, y) = (
                    (corner.x - origin.x) * group.scale,
                    (corner.y - origin.y) * group.scale,
                );
                Point::new(origin.x + x * cos - y * sin, origin.y + x * sin + y * cos)
            });
            let (left, right) = corners
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.x), r.max(p.x)));
            let (top, bottom) = corners
                .iter()
                .fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.y), b.max(p.y)));
            Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top)).expand(1.0)
        };
        extent = match group.reach {
            Some(reach) => landed.intersection(&reach)?,
            None => landed,
        };
    }
    Some(extent)
}

/// The rectangle a parent-level layer can paint into: its primitives' union,
/// or its whole clip when it holds text, meshes, custom primitives or shadows.
fn paint_extent(layer: &Layer) -> Rectangle {
    if !layer.text.is_empty()
        || !layer.triangles.is_empty()
        || !layer.primitives.is_empty()
        || layer.has_pending()
    {
        return layer.bounds;
    }
    let mut extent: Option<Rectangle> = None;
    let mut add = |rect: Rectangle| {
        extent = Some(extent.map_or(rect, |extent| extent.union(&rect)));
    };
    for quad in layer.quads.geometry() {
        if quad.shadow_color.components()[3] > 0.0 {
            return layer.bounds;
        }
        add(Rectangle::new(
            Point::new(quad.position[0], quad.position[1]),
            Size::new(quad.size[0], quad.size[1]),
        ));
    }
    #[cfg(any(feature = "image", feature = "svg"))]
    for image in &layer.images {
        match image {
            #[cfg(feature = "image")]
            image::Image::Raster {
                bounds,
                clip_bounds,
                ..
            } => add(bounds.intersection(clip_bounds).unwrap_or(*bounds)),
            #[cfg(feature = "svg")]
            image::Image::Vector {
                bounds,
                clip_bounds,
                ..
            } => add(bounds.intersection(clip_bounds).unwrap_or(*bounds)),
            #[allow(unreachable_patterns)]
            _ => return layer.bounds,
        }
    }
    extent
        .and_then(|extent| extent.intersection(&layer.bounds))
        .unwrap_or(Rectangle::new(Point::ORIGIN, Size::ZERO))
}

/// Whether paint in `extent` leaves a clip group's rectangle alone.
fn clear_of(group: layer::AlphaGroup, extent: Rectangle) -> bool {
    group.clip.is_some_and(|(clip, _)| {
        clip.intersection(&extent)
            .is_none_or(|overlap| overlap.width <= 0.0 || overlap.height <= 0.0)
    })
}

/// Whether two clip groups' rectangles leave each other's paint alone.
fn apart(a: layer::AlphaGroup, b: layer::AlphaGroup) -> bool {
    match (a.clip, b.clip) {
        (Some((a, _)), Some((b, _))) => a
            .intersection(&b)
            .is_none_or(|overlap| overlap.width <= 0.0 || overlap.height <= 0.0),
        _ => false,
    }
}

/// Where `bounds` lands once scaled and turned about `origin`: the box
/// around its four turned corners.
fn composited_reach(bounds: Rectangle, scale: f32, rotation: f32, origin: Point) -> Rectangle {
    let (sin, cos) = rotation.sin_cos();
    let corners = [
        Point::new(bounds.x, bounds.y),
        Point::new(bounds.x + bounds.width, bounds.y),
        Point::new(bounds.x, bounds.y + bounds.height),
        Point::new(bounds.x + bounds.width, bounds.y + bounds.height),
    ]
    .map(|corner| {
        let (x, y) = ((corner.x - origin.x) * scale, (corner.y - origin.y) * scale);
        Point::new(origin.x + x * cos - y * sin, origin.y + x * sin + y * cos)
    });
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for corner in corners {
        left = left.min(corner.x);
        top = top.min(corner.y);
        right = right.max(corner.x);
        bottom = bottom.max(corner.y);
    }
    Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top))
}

/// The physical scissor a group's composite needs, one pixel wider on each
/// side for filtering; `None` when its reach is not known.
fn group_scissor(group: layer::AlphaGroup, scale: f32) -> Option<[u32; 4]> {
    let reach = group.reach? * scale;
    let left = (reach.x.floor() - 1.0).max(0.0);
    let top = (reach.y.floor() - 1.0).max(0.0);
    let right = (reach.x + reach.width).ceil() + 1.0;
    let bottom = (reach.y + reach.height).ceil() + 1.0;
    (right > left && bottom > top).then_some([
        left as u32,
        top as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    ])
}

fn group_clip_region(group: layer::AlphaGroup, scale: f32) -> clip::ClipRegion {
    group
        .clip
        .map_or(clip::ClipRegion::NONE, |(bounds, radius)| {
            let bounds = bounds * scale;
            clip::ClipRegion {
                rect: [bounds.x, bounds.y, bounds.width, bounds.height],
                radius: [
                    radius.top_left * scale,
                    radius.top_right * scale,
                    radius.bottom_right * scale,
                    radius.bottom_left * scale,
                ],
            }
        })
}

#[derive(Default)]
struct LayerScope {
    /// How many recorded nodes enclosed this scope when it opened: replay
    /// moves its clip with those nodes only.
    depth: usize,
    alpha: Option<layer::AlphaGroup>,
    visibility_only: bool,
    reveal: Option<(usize, Rectangle)>,
    /// The first layer of a rounded clip group with no clip source, which may
    /// be dropped when its mask is redundant (enzUI-ICED #3406).
    rounded_from: Option<usize>,
}

/// Where preparation/encoding ended and queue submission began, for one
/// [`Renderer::present_measured`] (enzICED #2250).
///
/// The two spans meet exactly at one instant and never overlap: the same
/// `Instant` is the end of `prepare` and the start of `submit`, so no interval
/// can be filed under both names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeSpans {
    /// `Renderer::prepare` + `Renderer::render` into the command encoder, plus
    /// the staging-belt finish.
    pub prepare: (std::time::Instant, std::time::Instant),
    /// `CommandEncoder::finish` + `Queue::submit` + staging-belt recall.
    pub submit: (std::time::Instant, std::time::Instant),
}

/// Per-renderer ownership retained for presentation replay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetainedResourceStatistics {
    pub font_resources: usize,
    pub font_bytes: usize,
    pub media_resources: usize,
    pub media_bytes: usize,
    pub cached_media_entries: usize,
    pub cached_media_bytes: u64,
    pub atlas_allocations: usize,
    pub atlas_bytes: u64,
    pub pending_media_jobs: usize,
    pub pending_callbacks: usize,
    pub pending_callback_bytes: usize,
}

/// What a fully prepared frame was prepared against: presenting it again
/// without preparing is only right while all of it still holds
/// (enzUI-ICED #3442).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Prepared {
    layers_generation: u64,
    physical_size: Size<u32>,
    scale_factor: u32,
    text_epoch: u64,
}

/// A [`wgpu`] graphics renderer for [`iced`].
///
/// [`wgpu`]: https://github.com/gfx-rs/wgpu-rs
/// [`iced`]: https://github.com/iced-rs/iced
pub struct Renderer {
    engine: Engine,
    settings: renderer::Settings,

    layers: layer::Stack,
    scale: Option<renderer::Scale>,
    active_alpha_groups: Vec<layer::AlphaGroup>,
    active_clip_sources: layer::ClipSources,
    layer_scopes: Vec<LayerScope>,
    // Open recordings are ordered from outermost to innermost.
    recordings: Vec<Recording>,
    presentation_state: core::renderer::presentation::State,
    sample_budget: core::renderer::presentation::SampleBudget,
    presentation_recording: Option<core::renderer::Recording>,
    /// What each replay changed, when this renderer presents a lane that keeps
    /// its last frame (enzUI-ICED #3666).
    damage: Option<damage::Tracker>,
    /// What each frame drawn by widgets changed, when this renderer
    /// presents a window that keeps its last frame (enzUI-ICED #3669).
    frame_damage: Option<damage::FrameTracker>,
    /// The damage [`unchanged_frame`](Renderer::unchanged_frame) measured
    /// for the frame about to be presented.
    frame_damage_ahead: Option<damage::Damage>,
    /// The last frame, which a damaged frame paints only in part.
    canvas: Option<damage::Canvas>,
    /// The texture a frame is drawn into, while it is the canvas: glass
    /// copies its backdrop straight out of it (see `material::State`).
    direct_target: Option<wgpu::Texture>,
    /// The color the canvas was last cleared to.
    canvas_clear: Option<Color>,
    /// The physical rectangle the next frame's clear is limited to.
    clear_scissor: Option<Rectangle<u32>>,
    /// The frame being drawn paints only part of the canvas: its unpainted
    /// layers are still on screen, so nothing they use is trimmed.
    partial_frame: bool,
    /// How many lane frames painted only their damage.
    partial_frames: u64,
    /// How many frames presented through the canvas painted all of it.
    full_frames: u64,
    /// The last replay replayed only the segments whose placement changed
    /// and those painting where they change: the scene and tracks, for a
    /// full replay when the frame must be painted in full.
    partial_replay: Option<(
        core::renderer::Recording,
        Vec<core::renderer::Track>,
        core::time::Instant,
    )>,
    /// The bounds the layer stack was last reset to.
    stack_bounds: Rectangle,
    /// A full replay is under way: no replay of changed segments.
    replaying_full: bool,
    /// How many replays replayed only what changed.
    changed_replays: u64,
    /// Turned nodes of meshes and round quads turn their geometry rather
    /// than being composited; a test turns it off for a reference.
    turns_geometry: bool,

    /// The regions, in physical pixels, a partial frame paints one by one.
    paint_regions: Vec<Rectangle<u32>>,
    /// enzUI: what the last frame changed on the canvas, `[x, y, width,
    /// height]` in physical pixels; `None` for all of it.
    present_damage: Option<Vec<[u32; 4]>>,
    /// An image was still loading when the last lane frame was painted: it
    /// can appear on the next without a new recording.
    images_pending: bool,
    presentation_sample_valid: bool,
    presentation_inputs: Vec<core::renderer::presentation::Input>,
    presentation_tracks: Vec<core::renderer::Track>,
    presentation_viewport: Option<Size>,
    presentation_refusal: Option<core::renderer::presentation::Error>,

    docked_recorder: core::renderer::docked::Recorder,
    independent_replay: bool,
    presentation_resources_active: bool,
    media_claims: owned_media::Assets,
    native_vector_claims: std::collections::BTreeMap<u64, Option<usize>>,
    native_vector_overflow: bool,
    next_alpha_group: usize,
    alpha_segment: usize,
    alpha: alpha::State,
    material: material::State,

    clear: clear::State,
    quad: quad::State,
    triangle: triangle::State,
    text: text::State,
    text_viewport: text::Viewport,

    #[cfg(any(feature = "svg", feature = "image"))]
    image: image::State,

    // TODO: Centralize all the image feature handling
    #[cfg(any(feature = "svg", feature = "image"))]
    image_cache: std::cell::RefCell<image::Cache>,

    staging_belt: wgpu::util::StagingBelt,
    /// Bumped whenever the layer stack is reset (enzUI-ICED #3442).
    layers_generation: u64,
    /// What the last fully prepared frame was prepared against.
    prepared: Option<Prepared>,
    /// Whether the next draw presents the prepared frame again.
    reuse_next: bool,
    /// The generation of the layer stack last merged for drawing.
    merged: Option<u64>,
    /// A second renderer over the same device that paints a frame again
    /// for `ICED_UI_DAMAGE_CHECK`.
    checker: Option<Box<Renderer>>,
    /// The outermost rounded group drawn straight to the frame and masked
    /// afterwards, instead of composited from a target (enzUI-ICED #3442).
    window_mask: Option<(layer::AlphaGroup, alpha::MaskBackdrop)>,
    primary_render_passes: usize,
    /// Render passes a frame opened by cause, for `ENZUI_RENDER_PASS_TRACE`:
    /// meshes, alpha groups, shader primitives.
    pass_causes: [usize; 3],
    /// Rounded clip groups dropped because their mask was redundant, since
    /// the renderer was created (enzUI-ICED #3406).
    rounded_elisions: u64,
    readback_audit_enabled: bool,
    readback_audit_failure_only: bool,
    last_readback_audit: Option<[u32; 2]>,
}

/// A scene recording in progress (enzICED #2219).
///
/// `start` is the first not-yet-captured active layer; `node` is the scene node
/// currently being attributed to; `segments` are the runs already closed by a
/// `mark_recording` call.
#[derive(Debug)]
struct Recording {
    recorder: core::renderer::docked::Recorder,
    fixed_layout_quads: usize,
    docked: Option<core::renderer::docked::Capture>,
    start: usize,
    bounds: Rectangle,
    presentation: Option<std::sync::Arc<core::renderer::presentation::Program>>,
    node_clips: Vec<(u64, Rectangle)>,
    /// The offset each node was painted at, from `mark_recording_at`.
    baselines: Vec<(u64, core::Vector)>,
    /// The transform each node's ancestors applied when it was marked.
    origins: Vec<(u64, Transformation)>,
    owned_quads: std::collections::BTreeMap<u64, owned_quad::OwnedQuad>,
    editors: std::collections::BTreeMap<u64, text::editor::Captured>,
    scrolls: std::collections::BTreeMap<u64, owned_scroll::Captured>,
    media: std::collections::BTreeMap<u64, owned_media::Captured>,
    /// The node path currently being recorded, outermost first. Empty means
    /// static content. A path rather than a single node because effects nest,
    /// and a track on a parent has to reach its descendants' paint.
    path: Vec<core::renderer::NodeId>,
    /// Alpha groups active when the current segment opened. Everything at or
    /// beyond this depth was pushed *inside* the innermost node.
    base: usize,
    /// The `base` each enclosing node was opened at, so leaving one restores it.
    bases: Vec<usize>,
    /// The clip in effect when the current segment opened (enzICED #2219).
    ///
    /// It belongs to the node's PARENT -- `Motion` opens its clip scope and
    /// only then marks the node -- so a replay keeps it fixed while every
    /// narrower clip inside the segment travels with the node.
    clip: Rectangle,
    /// How many of the current path's nodes enclosed `clip` when it opened.
    ///
    /// A node marked without opening a clip of its own leaves its children
    /// under a clip an outer scope opened, which must not move with it.
    clip_depth: usize,
    segments: Vec<Segment>,
    /// Set when some captured layer could not take ownership of its content
    /// (enzICED #2219). A poisoned recording is never handed out: the caller
    /// gets `None` from `finish_recording` and draws normally.
    unretainable: bool,
}

/// A retained scene: the layers a draw pass recorded, split into identified
/// segments, owned by the renderer so they can be replayed after the widgets
/// that produced them are gone (enzICED #2219).
///
/// Segments are kept in painting order, so replaying the scene reproduces the
/// original ordering exactly even when a moving node sits between two static
/// siblings.
#[derive(Debug, Clone)]
pub struct Scene {
    segments: Vec<Segment>,
    bounds: Rectangle,
    program_nodes: Vec<u64>,
    node_clips: Vec<(u64, Rectangle)>,
    /// The offset each node's paint was recorded at, sorted by node. A track
    /// on one of these nodes is its whole translation, so a replay moves the
    /// paint by the difference (enzUI-ICED #3251).
    baselines: Vec<(u64, core::Vector)>,
    /// The transform each node's ancestors applied when it was marked, sorted
    /// by node: a track's clip is in the node's layout coordinates, and a
    /// scrolled or translated ancestor moves them on screen.
    origins: Vec<(u64, Transformation)>,
    owned_quads: std::collections::BTreeMap<u64, owned_quad::OwnedQuad>,
    editors: std::collections::BTreeMap<u64, text::editor::Captured>,
    scrolls: std::collections::BTreeMap<u64, owned_scroll::Captured>,
    media: std::collections::BTreeMap<u64, owned_media::Captured>,
    media_assets: owned_media::Assets,
    fonts: Option<std::sync::Arc<text::FontAssets>>,
    layout_revision: std::sync::Arc<()>,
}

/// One identified run of recorded layers inside a [`Scene`].
#[derive(Debug, Clone)]
pub struct Segment {
    path: Vec<core::renderer::NodeId>,
    /// The clip that was in effect when this run opened (enzICED #2219).
    clip: Rectangle,
    /// How many of `path`'s nodes enclosed `clip` when it opened.
    clip_depth: usize,
    /// The alpha-group depth this segment was recorded at (enzICED #2219).
    ///
    /// Groups shallower than this belong to the node's *ancestors* and are
    /// re-established by whatever context replays the scene; groups at or
    /// beyond it were pushed inside the node -- a nested fade, say -- and must
    /// survive replay or the inner subtree loses its opacity.
    base: usize,
    layers: Vec<layer::Layer>,
}

impl Segment {
    /// Whether a track reaches this run: one of its path's nodes has one.
    fn tracked(&self, tracks: &[core::renderer::Track]) -> bool {
        self.path
            .iter()
            .any(|node| tracks.iter().any(|track| track.node == *node))
    }

    /// The key of this run replayed where it was recorded.
    fn unplaced_key(&self) -> u64 {
        Placement::unplaced(self.path.len()).key()
    }

    /// The scene nodes this run of layers belongs to, outermost first.
    pub fn path(&self) -> &[core::renderer::NodeId] {
        &self.path
    }

    /// The recorded layers.
    pub fn layers(&self) -> &[layer::Layer] {
        &self.layers
    }

    /// The clip that was in effect when this run opened (enzICED #2219).
    ///
    /// A replay keeps this rectangle fixed under the node's own travel: it is
    /// the enclosing `Motion` clip, and a panel slides *within* it.
    pub fn clip(&self) -> Rectangle {
        self.clip
    }
}

/// A composed uniform affine used while replaying nested compositor tracks.
///
/// `map(p) = scale * p + offset`. Keeping this form is necessary because two
/// scales about different origins (especially with translations between them)
/// cannot be represented by a product scale and the last origin.
#[derive(Debug, Clone, Copy)]
struct ReplayAffine {
    scale: f32,
    offset: core::Vector,
}

/// Where a recorded segment replays: what its path's tracks compose to.
struct Placement {
    translation: core::Vector,
    opacity: f32,
    scale: f32,
    rotation: f32,
    origin: core::Point,
    clip: Option<Rectangle>,
    clip_maps: Vec<ReplayAffine>,
}

impl Placement {
    /// Where a segment of a path of `depth` nodes replays when no track
    /// reaches it: where it was recorded.
    fn unplaced(depth: usize) -> Self {
        Self {
            translation: core::Vector::ZERO,
            opacity: 1.0,
            scale: 1.0,
            rotation: 0.0,
            origin: core::Point::ORIGIN,
            clip: None,
            clip_maps: vec![ReplayAffine::IDENTITY; depth.max(1)],
        }
    }

    /// A signature of the placement; equal placements replay a segment's
    /// paint the same way.
    fn key(&self) -> u64 {
        let mut state = damage::PaintHasher::default();
        for value in [
            self.translation.x,
            self.translation.y,
            self.opacity,
            self.scale,
            self.rotation,
            self.origin.x,
            self.origin.y,
        ] {
            state.f32(value);
        }
        state.option(self.clip, damage::PaintHasher::rectangle);
        state.usize(self.clip_maps.len());
        for map in &self.clip_maps {
            state.f32(map.scale);
            state.f32(map.offset.x);
            state.f32(map.offset.y);
        }
        state.finish()
    }
}

/// `vector` rounded to whole device pixels at `factor` pixels per unit.
fn snap_vector(vector: core::Vector, factor: f32) -> core::Vector {
    core::Vector::new(
        (vector.x * factor).round() / factor,
        (vector.y * factor).round() / factor,
    )
}

impl ReplayAffine {
    const IDENTITY: Self = Self {
        scale: 1.0,
        offset: core::Vector::ZERO,
    };

    /// A pure translation.
    fn translation(offset: core::Vector) -> Self {
        Self { scale: 1.0, offset }
    }

    /// The affine one track applies on its own.
    fn of_track(track: &core::renderer::Track) -> Self {
        let scale = if track.scale.is_finite() && track.scale > 0.0 {
            track.scale
        } else {
            1.0
        };

        Self {
            scale,
            offset: track.translation
                + core::Vector::new(
                    (1.0 - scale) * track.scale_origin.x,
                    (1.0 - scale) * track.scale_origin.y,
                ),
        }
    }

    /// enzUI-ICED #3083: round a pure translation to whole device pixels.
    ///
    /// Glyphs truncate their vertical position and crisp quads round theirs,
    /// so under a fractional translation a label and its background disagree
    /// by a pixel on some frames and the label wobbles. Moving the whole group
    /// by whole device pixels keeps every element's at-rest relationship. A
    /// scaling transform is left exact: it has no fixed pixel grid to keep.
    fn snap_translation(&mut self, device_pixels_per_unit: f32) {
        if self.scale == 1.0 && device_pixels_per_unit.is_finite() && device_pixels_per_unit > 0.0 {
            self.offset = snap_vector(self.offset, device_pixels_per_unit);
        }
    }

    /// Composes `self ∘ inner`: `inner` is applied first, in `self`'s space.
    fn append(&mut self, inner: Self) {
        self.offset += core::Vector::new(self.scale * inner.offset.x, self.scale * inner.offset.y);
        self.scale *= inner.scale;
    }

    fn append_inner(&mut self, track: &core::renderer::Track) {
        self.append(Self::of_track(track));
    }

    /// Whether this affine maps every point to itself.
    fn is_identity(self) -> bool {
        (self.scale - 1.0).abs() <= f32::EPSILON && self.offset.x == 0.0 && self.offset.y == 0.0
    }

    /// The part of this affine that is still applied *after* `translation` has
    /// been baked into the replayed layers, i.e. `self ∘ translate(-t)`.
    ///
    /// This is exactly the compositor group replay opens, and therefore the
    /// transform a replay-space clip must be pulled back through (#2251).
    fn after_translation(self, translation: core::Vector) -> Self {
        Self {
            scale: self.scale,
            offset: self.offset
                - core::Vector::new(self.scale * translation.x, self.scale * translation.y),
        }
    }

    /// The inverse map, or the identity for a degenerate scale.
    fn inverse(self) -> Self {
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Self::IDENTITY;
        }

        Self {
            scale: 1.0 / self.scale,
            offset: core::Vector::new(-self.offset.x / self.scale, -self.offset.y / self.scale),
        }
    }

    /// Maps a rectangle through this affine. A uniform scale keeps a rectangle
    /// axis-aligned, so no bounding-box widening is involved.
    fn map_rect(self, rectangle: Rectangle) -> Rectangle {
        Rectangle {
            x: self.scale * rectangle.x + self.offset.x,
            y: self.scale * rectangle.y + self.offset.y,
            width: self.scale * rectangle.width,
            height: self.scale * rectangle.height,
        }
    }

    /// Canonical form consumed by replay.
    ///
    /// A non-unit uniform affine has a unique fixed point, so represent it as a
    /// scale around that point. This keeps a single Scale track on exactly the
    /// same compositor sampling grid as its live frame. At unit scale there is
    /// no fixed point and the offset remains a plain translation.
    fn canonical(self) -> (core::Vector, f32, core::Point) {
        let residue = 1.0 - self.scale;
        if residue.abs() <= f32::EPSILON {
            (self.offset, 1.0, core::Point::ORIGIN)
        } else {
            (
                core::Vector::ZERO,
                self.scale,
                core::Point::new(self.offset.x / residue, self.offset.y / residue),
            )
        }
    }
}

fn retain_layers(
    layers: Vec<layer::Layer>,
) -> Result<Vec<layer::Layer>, core::renderer::presentation::Error> {
    layers.iter().map(layer::Layer::retain).collect()
}

fn presentation_bounds(bounds: Rectangle) -> core::renderer::presentation::Bounds {
    core::renderer::presentation::Bounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

impl Scene {
    /// A signature of what applies to the whole frame of this scene, for
    /// comparing it with the last one replayed (enzUI-ICED #3666).
    fn frame_signature(&self) -> u64 {
        let mut frame = damage::PaintHasher::default();
        frame.rectangle(self.bounds);
        // Shared, never changed: the scene keeps the address from reuse.
        frame.option(self.fonts.as_ref(), |state, fonts| {
            state.usize(std::sync::Arc::as_ptr(fonts) as usize);
        });
        frame.finish()
    }
    /// The offset `node` was recorded at by `mark_recording_at`, if any.
    /// The transform `node`'s ancestors applied when it was recorded.
    fn origin(&self, node: core::renderer::NodeId) -> Option<Transformation> {
        self.origins
            .binary_search_by_key(&node.0, |(id, _)| *id)
            .ok()
            .map(|index| self.origins[index].1)
    }

    fn baseline(&self, node: core::renderer::NodeId) -> Option<core::Vector> {
        self.baselines
            .binary_search_by_key(&node.0, |(id, _)| *id)
            .ok()
            .map(|index| self.baselines[index].1)
    }

    fn validate_presentation(
        &self,
        program: &core::renderer::presentation::Program,
    ) -> Result<(), core::renderer::presentation::Error> {
        use core::renderer::presentation::Error;
        program.validate_scene(&self.program_nodes, presentation_bounds(self.bounds))?;
        for segment in &self.segments {
            if segment.layers.iter().all(layer::Layer::is_empty) {
                continue;
            }
            let Some(node) = segment.path.last().map(|node| node.0) else {
                if segment
                    .layers
                    .iter()
                    .all(|layer| layer.is_empty() || layer.is_fixed_layout_paint())
                {
                    continue;
                }
                return Err(Error::OpaquePaint);
            };
            if !program.owns_leaf(node)
                && !program.is_container(node)
                && !program
                    .scroll_nodes()
                    .any(|(id, scroll)| id == node || scroll.chrome == node)
            {
                return Err(Error::OpaquePaint);
            }
        }
        for node in program.leaves() {
            let index = self
                .node_clips
                .binary_search_by_key(&node, |(id, _)| *id)
                .map_err(|_| Error::SceneIdentity(node))?;
            if index > 0 && self.node_clips[index - 1].0 == node
                || self
                    .node_clips
                    .get(index + 1)
                    .is_some_and(|(id, _)| *id == node)
            {
                return Err(Error::SceneIdentity(node));
            }
            if !program.is_clipped_scoped(node)
                && !presentation_bounds(self.node_clips[index].1).contains(program.clip())
            {
                return Err(Error::ClipCoverage(node));
            }
        }
        owned_quad::validate(self, program)?;
        text::editor::validate(self, program)?;
        owned_clip::validate_sources(self, program)?;
        owned_scroll::validate(self, program)?;
        owned_media::validate(self, program)
    }

    /// Whether every command in this scene owns its content, and can therefore
    /// be replayed after the widgets that recorded it are gone (enzICED #2219).
    pub fn is_retained(&self) -> bool {
        self.segments
            .iter()
            .flat_map(|segment| segment.layers.iter())
            .all(layer::Layer::is_retained)
    }

    /// The segments this scene will replay, in painting order.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The scene nodes this scene can address.
    pub fn nodes(&self) -> impl Iterator<Item = core::renderer::NodeId> + '_ {
        self.segments
            .iter()
            .flat_map(|segment| segment.path.iter().copied())
    }
}

impl Renderer {
    fn start_recording_from(
        &mut self,
        bounds: Rectangle,
        recorder: core::renderer::docked::Recorder,
    ) -> bool {
        recorder.observe_layout();
        // A recording is bracketed by a clip scope so the captured layers are
        // self-contained: primitives recorded before it live in an earlier
        // layer and cannot leak into the retained scene node.
        <Self as core::Renderer>::start_layer(self, bounds);
        // `push_clip` has already created the scope's own layer and advanced
        // the active count past it, so the recorded range starts one back.
        let start = self.layers.active_len().saturating_sub(1);
        self.recordings.push(Recording {
            recorder,
            fixed_layout_quads: 0,
            docked: None,
            start,
            bounds,
            presentation: None,
            node_clips: Vec::new(),
            baselines: Vec::new(),
            origins: Vec::new(),
            owned_quads: std::collections::BTreeMap::new(),
            editors: std::collections::BTreeMap::new(),
            scrolls: std::collections::BTreeMap::new(),
            media: std::collections::BTreeMap::new(),
            path: Vec::new(),
            base: self.active_alpha_groups.len(),
            bases: Vec::new(),
            clip: self
                .layers
                .clip_bounds_at(start)
                .unwrap_or(Rectangle::INFINITE),
            clip_depth: 0,
            segments: Vec::new(),
            unretainable: false,
        });
        true
    }

    fn fill_owned_quad(&mut self, node: core::renderer::NodeId, captured: owned_quad::OwnedQuad) {
        use core::Renderer as _;
        use core::renderer::presentation::{Error, MAX_ITEMS};
        let quad = captured.quad;
        let quads = captured.paint_quads(quad.bounds, None, None);
        let draw = |renderer: &mut Self| {
            for &(quad, background) in quads.iter().flatten() {
                renderer.fill_quad(quad, background);
            }
            #[cfg(feature = "geometry")]
            if let Some(border) = captured.border {
                let (layer, transform) = renderer.layers.current_mut();
                owned_border::paint(layer, quad.bounds, border, transform);
            }
        };
        let owner = self.recordings.iter().any(|recording| {
            recording
                .presentation
                .as_ref()
                .is_some_and(|program| program.owns_node(node.0))
        });
        if !owner {
            if captured.blink.is_some() && !self.recordings.is_empty() {
                self.refuse_layout_layer(Error::OpaquePaint, false);
                draw(self);
                return;
            }
            if let Some(value) = captured.editor_scrollbars {
                for (quad, color) in value
                    .paint
                    .quads(quad.bounds, value.metrics)
                    .into_iter()
                    .flatten()
                {
                    self.fill_quad(quad, color);
                }
            } else {
                let fixed = self.recordings.len() == 1
                    && self.recordings[0].presentation.is_some()
                    && self.recordings[0].path.is_empty()
                    && captured.valid();
                if fixed {
                    let count = quads.iter().flatten().count();
                    let recording = &mut self.recordings[0];
                    if recording.fixed_layout_quads.saturating_add(count) > MAX_ITEMS {
                        recording.unretainable = true;
                        self.presentation_refusal = Some(Error::Budget);
                    } else {
                        recording.fixed_layout_quads += count;
                        self.layers.current_mut().0.fixed_layout_quads += count;
                    }
                }
                draw(self);
            }
            return;
        }
        let eligible = self.recordings.len() == 1
            && self.recordings.last().is_some_and(|recording| {
                recording
                    .presentation
                    .as_ref()
                    .and_then(|program| program.container_recorded(node.0))
                    == Some(presentation_bounds(captured.allocation))
                    && !recording.owned_quads.contains_key(&node.0)
            })
            && captured.editor_scrollbars.is_none_or(|value| {
                self.recordings[0].editors.contains_key(&value.editor)
                    && self.recordings[0]
                        .presentation
                        .as_ref()
                        .is_some_and(|program| program.contains_descendant(node.0, value.editor))
            })
            && captured.valid()
            && self.scroll_transform_matches(node.0);
        if !eligible {
            self.reject_layout_paint(node);
            draw(self);
            return;
        }
        if self.recordings[0].owned_quads.len() >= MAX_ITEMS {
            self.recordings[0].unretainable = true;
            self.presentation_refusal = Some(Error::Budget);
            draw(self);
            return;
        }
        let _ = self.recordings[0].owned_quads.insert(node.0, captured);
        self.mark_recording(node);
        draw(self);
        self.mark_recording(core::renderer::NodeId::STATIC);
    }

    #[cfg(feature = "svg")]
    fn claim_native_vector(&mut self, handle: &core::svg::Handle) {
        let id = handle.id();
        if self.native_vector_claims.len() < core::renderer::presentation::MAX_ITEMS
            || self.native_vector_claims.contains_key(&id)
        {
            let identity = self.image_cache.borrow().vector_identity(handle);
            let _ = self.native_vector_claims.entry(id).or_insert(identity);
        } else {
            self.native_vector_overflow = true;
        }
    }

    #[cfg(any(feature = "image", feature = "svg"))]
    fn capture_media(
        &mut self,
        node: core::renderer::NodeId,
        capture: owned_media::Captured,
    ) -> bool {
        use core::Renderer as _;
        use core::renderer::presentation::{Error, MAX_ITEMS};
        let declared = self
            .recordings
            .last()
            .and_then(|recording| recording.presentation.as_ref())
            .is_some_and(|program| program.media_nodes().any(|(id, _)| id == node.0));
        if !declared {
            return false;
        }
        let valid = self.recordings.len() == 1
            && self.recordings[0]
                .presentation
                .as_ref()
                .unwrap()
                .media_nodes()
                .find(|(id, _)| *id == node.0)
                .is_some_and(|(_, media)| capture.valid(media))
            && self.scroll_transform_matches(node.0)
            && !self.recordings[0].media.contains_key(&node.0)
            && self.recordings[0].media.len() < MAX_ITEMS;
        if !valid {
            self.reject_layout_paint(node);
            self.presentation_refusal = Some(Error::OpaquePaint);
            return false;
        }
        let _ = self.recordings[0].media.insert(node.0, capture);
        self.mark_recording(node);
        true
    }

    // Preserve the first refusal: descendants of a poisoned recording may report only its missing clip declaration.
    fn refuse_layout_layer(
        &mut self,
        error: core::renderer::presentation::Error,
        owned_only: bool,
    ) {
        // Read before poisoning: this refusal's own flag must not make it look
        // like a consequence of an earlier one.
        let cascade = self
            .recordings
            .iter()
            .any(|recording| recording.unretainable);
        for recording in &mut self.recordings {
            if !owned_only || recording.presentation.is_some() {
                recording.unretainable = true;
            }
        }
        if !cascade {
            self.presentation_refusal = Some(error);
        }
    }

    fn scroll_transform_matches(&self, node: u64) -> bool {
        self.recordings
            .last()
            .and_then(|recording| {
                recording
                    .presentation
                    .as_ref()
                    .map(|program| (recording, program))
            })
            .is_some_and(|(recording, program)| {
                owned_scroll::transform(program, &recording.scrolls, node)
                    .is_ok_and(|transform| transform == self.layers.transformation())
            })
    }

    fn sample_presentation(
        &mut self,
        recording: &core::renderer::Recording,
        scene: &Scene,
        tracks: &[core::renderer::Track],
        merged: &mut Vec<core::renderer::Track>,
    ) -> Result<bool, core::renderer::presentation::Error> {
        use core::renderer::presentation::{Error, MAX_ITEMS};
        let Some(program) = recording.presentation() else {
            self.text.clear_reflow();
            self.text.clear_editors();
            self.text.clear_scroll();
            if self.presentation_recording.take().is_some() {
                self.presentation_state.clear();
                self.presentation_inputs = Vec::new();
                *merged = Vec::new();
            }
            return Ok(false);
        };
        if tracks.len() > MAX_ITEMS {
            return Err(Error::Budget);
        }
        if self
            .presentation_recording
            .as_ref()
            .is_none_or(|previous| !previous.same_revision(recording))
        {
            scene.validate_presentation(program)?;
            self.presentation_recording = Some(recording.clone());
        }
        self.presentation_inputs.clear();
        self.presentation_inputs
            .extend(tracks.iter().map(core::renderer::Track::presentation_input));
        self.presentation_inputs
            .sort_unstable_by_key(|input| input.node);
        let viewport = self.presentation_viewport.unwrap_or(scene.bounds.size());
        self.text.prepare_scroll(scene);
        self.text.prepare_editors(scene)?;
        let placements = if program.text_nodes().next().is_some()
            || program.editor_nodes().next().is_some()
            || program.scroll_nodes().next().is_some()
        {
            if !self.independent_replay {
                return Err(Error::UnsupportedBackend);
            }
            self.text.prepare_reflow(scene, program)?;
            self.presentation_state.sample_with_text(
                program,
                [viewport.width, viewport.height],
                &self.presentation_inputs,
                &scene.layout_revision,
                &mut self.text,
            )?
        } else {
            self.text.clear_reflow();
            self.text.clear_editors();
            self.presentation_state.sample(
                program,
                [viewport.width, viewport.height],
                &self.presentation_inputs,
            )?
        };
        self.text.place_reflow(placements);
        merged.clear();
        merged.extend_from_slice(tracks);
        merged.sort_unstable_by_key(|track| track.node);
        let sampled_count = merged.len();
        for placement in placements {
            let node = core::renderer::NodeId(placement.node);
            if let Ok(index) =
                merged[..sampled_count].binary_search_by_key(&node, |track| track.node)
            {
                let track = &mut merged[index];
                track.translation +=
                    Vector::new(placement.translation[0], placement.translation[1]);
                track.opacity *= placement.opacity;
            } else {
                merged.push(core::renderer::Track {
                    translation: Vector::new(placement.translation[0], placement.translation[1]),
                    opacity: placement.opacity,
                    ..core::renderer::Track::identity(node)
                });
            }
        }
        merged.sort_unstable_by_key(|track| track.node);
        Ok(true)
    }

    /// Copies the current frame's successful owned replay under a bounded snapshot lease.
    ///
    /// Reset or a refused replay revokes sampling, without invalidating previously issued
    /// snapshots. This is allocation data, not compositor confirmation or an input lease.
    pub fn layout_sample(
        &mut self,
    ) -> Result<
        Option<std::sync::Arc<core::renderer::presentation::Sample>>,
        core::renderer::presentation::Error,
    > {
        if !self.presentation_sample_valid {
            return Ok(None);
        }
        let mut sample = self.sample_budget.capture(&self.presentation_state)?;
        let native = (|| {
            let mut frame = self.text.capture_native_editors()?;
            let scene = self
                .presentation_recording
                .as_ref()
                .and_then(|recording| recording.payload::<Scene>())
                .ok_or(core::renderer::presentation::Error::RecordingState)?;
            frame.capture_scrolls(scene, &self.presentation_state, &self.text.scroll_inputs)?;
            Ok::<_, core::renderer::presentation::Error>(frame)
        })();
        if let Ok(frame) = native
            && (!frame.editors.is_empty() || !frame.scrolls.is_empty())
        {
            Arc::get_mut(&mut sample)
                .ok_or(core::renderer::presentation::Error::RecordingState)?
                .set_native_payload(Arc::new(frame));
        }

        Ok(Some(sample))
    }

    /// Deterministic owned-layout work from the most recent program replay.
    pub fn presentation_work(&self) -> core::renderer::presentation::Work {
        self.presentation_state.work()
    }

    /// Replays one recorded segment with its compositor properties applied
    /// (enzICED #2219).
    ///
    /// Opacity and clip are applied by wrapping the appended layers in the
    /// renderer's existing alpha-group and clip scopes rather than by editing
    /// the recorded primitives: that is the machinery that already composites a
    /// subtree off-screen, so overlapping primitives inside a fading node do
    /// not show through one another.
    /// Where `segment` replays under `tracks`: what its path's tracks
    /// compose to (enzICED #2219, #2251, enzUI-ICED #3083, #3251).
    fn place(
        &self,
        segment: &Segment,
        tracks: &[core::renderer::Track],
        evaluated: bool,
        scene: &Scene,
    ) -> Placement {
        // Tracks COMPOSE down the path. A segment painted inside a fading
        // parent by a settled child has no track of its own, but the
        // parent's must still reach it -- otherwise the child's paint
        // replays at full opacity inside a fading ancestor, which is a
        // whole-panel error that looks entirely plausible.
        //
        // A segment no track touches replays exactly where it was recorded,
        // so an unchanged sibling keeps its pixels without anyone asking it
        // anything.
        let mut affine = ReplayAffine::IDENTITY;
        let mut opacity = 1.0;
        let mut clip: Option<Rectangle> = None;
        // Turns add up down the path, about the turning node's origin as
        // its ancestors have moved it.
        let mut rotation = 0.0;
        // The turning node's origin, and whether that node is this
        // segment's own (its ancestors' movement is applied, snapped,
        // after the loop) rather than an ancestor of it.
        let mut rotation_origin: Option<(core::Point, bool)> = None;
        // The affine of the node's ANCESTORS only. The clip the node was
        // recorded under belongs to its parent, so it rides that transform
        // -- scale included -- and not the node's own travel (#2219 review
        // finding 3, #2251).
        let mut enclosing = ReplayAffine::IDENTITY;
        // `prefix[k]` is what the path's first `k` nodes apply: a clip
        // opened inside them moves with them and no further.
        let mut prefix = Vec::with_capacity(segment.path.len());

        for (depth, node) in segment.path.iter().enumerate() {
            prefix.push(affine);
            let track = if evaluated {
                tracks
                    .binary_search_by_key(node, |track| track.node)
                    .ok()
                    .map(|index| &tracks[index])
            } else {
                tracks.iter().find(|track| track.node == *node)
            };
            let Some(track) = track else { continue };
            // A plain track on a node recorded at a baseline is the
            // node's whole translation; the paint already carries the
            // baseline (enzUI-ICED #3251).
            let mut track = *track;
            if !evaluated && let Some(baseline) = scene.baseline(*node) {
                track.translation -= baseline;
            }
            if let Some(origin) = scene.origin(*node) {
                // Clip and origin are in the node's layout coordinates.
                track.clip = track.clip.map(|clip| clip * origin);
                track.scale_origin = track.scale_origin * origin;
            }
            let track = &track;

            if depth + 1 < segment.path.len() {
                enclosing.append_inner(track);
            }

            if track.rotation.is_finite() && track.rotation != 0.0 {
                rotation += track.rotation;
                let own = depth + 1 == segment.path.len();
                rotation_origin = Some(if own {
                    (track.scale_origin, true)
                } else {
                    let origin = affine.map_rect(Rectangle::new(track.scale_origin, Size::ZERO));
                    (core::Point::new(origin.x, origin.y), false)
                });
            }

            affine.append_inner(track);
            opacity *= track.opacity;

            // Every `Track::clip` is documented in the coordinates its node
            // is REPLAYED into, so nested clips intersect in that one
            // space, whatever transform each of them sits under (#2251).
            clip = match (clip, track.clip) {
                (Some(outer), Some(inner)) => outer.intersection(&inner),
                (existing, None) => existing,
                (None, inner) => inner,
            };
        }

        // enzUI-ICED #3083: a moving group steps together, by whole
        // device pixels, so text and its background cannot drift apart.
        // Only plain motion tracks: an evaluated presentation program's
        // placements are layout (a reflowed paragraph and the paint it
        // pushes down) and must stay exactly where a direct draw puts them.
        if !evaluated {
            let factor = <Self as core::Renderer>::hint_factor(self).unwrap_or(1.0);
            affine.snap_translation(factor);
            enclosing.snap_translation(factor);
            for step in &mut prefix {
                step.snap_translation(factor);
            }
        }
        let (translation, scale, scale_origin) = affine.canonical();

        // The compositor group is the only transform still applied after a
        // layer has been clipped, so a replay-space clip is pulled back
        // through it exactly once here; the enclosing recorded clip is
        // pushed forward by the ancestors and pulled back by the same group
        // (#2251). At unit scale both are the identity and this is the
        // plain ancestor translation it has always been.
        let pull_back = affine.after_translation(translation).inverse();
        let clip = clip.map(|clip| pull_back.map_rect(clip));

        let clip_maps: Vec<ReplayAffine> = if prefix.is_empty() {
            vec![pull_back]
        } else {
            prefix
                .iter()
                .map(|step| {
                    let mut map = pull_back;
                    map.append(*step);
                    map
                })
                .collect()
        };

        Placement {
            translation,
            opacity,
            scale,
            rotation,
            // A scale and a turn share the composite's origin; a plain turn
            // uses its own.
            origin: match rotation_origin {
                Some((origin, own)) if (scale - 1.0).abs() <= f32::EPSILON => {
                    if own {
                        let origin = enclosing.map_rect(Rectangle::new(origin, Size::ZERO));
                        core::Point::new(origin.x, origin.y)
                    } else {
                        origin
                    }
                }
                _ => scale_origin,
            },
            clip,
            clip_maps,
        }
    }

    /// Replays `layers`, recorded as `segment`, where `placement` puts them.
    fn replay_placed(
        &mut self,
        layers: Vec<layer::Layer>,
        segment: &Segment,
        placement: &Placement,
        clip_reissued: &mut Vec<(usize, usize)>,
    ) {
        self.replay_segment(
            layers,
            segment.base,
            segment.clip,
            segment.clip_depth,
            &placement.clip_maps,
            placement.translation,
            placement.opacity,
            placement.scale,
            placement.rotation,
            placement.origin,
            placement.clip,
            clip_reissued,
        );
    }

    /// Tells the damage tracker what the layers replayed from `from` on paint.
    fn hash_replayed(&mut self, from: usize, paint: &mut damage::PaintHasher) {
        let Self { layers, damage, .. } = self;
        let Some(damage) = damage.as_mut() else {
            return;
        };
        let layers = layers.as_slice();
        for layer in layers.get(from.min(layers.len())..).unwrap_or_default() {
            paint.restart();
            let head = layer.hash_paint(paint, None, |hash, rect| {
                damage.element(hash, land(layer, rect));
            });
            damage.layer(head, damage_extent(layer));
        }
    }

    /// Replays only what changed since the last replay of the same scene
    /// (enzUI-ICED #3668): the segments whose placement changed, compared
    /// with their last replay for the damage, then those and every segment
    /// painting into that damage, in order. Returns `false`, the layers
    /// reset, when the scene must be replayed in full instead.
    fn replay_changed(
        &mut self,
        recording: &core::renderer::Recording,
        scene: &Scene,
        tracks: &[core::renderer::Track],
    ) -> bool {
        let segments = &scene.segments;
        if self.images_pending
            || !self
                .damage
                .as_ref()
                .is_some_and(|damage| damage.can_replay_changed(recording, segments.len()))
        {
            return false;
        }
        // A segment no track reaches is placed where it was recorded; only
        // the others are placed again.
        let placements: Vec<Option<Placement>> = segments
            .iter()
            .map(|segment| {
                segment
                    .tracked(tracks)
                    .then(|| self.place(segment, tracks, false, scene))
            })
            .collect();
        let changed: Vec<usize> = {
            let damage = self.damage.as_ref().expect("damage is tracked");
            (0..segments.len())
                .filter(|index| {
                    let key = placements[*index]
                        .as_ref()
                        .map_or_else(|| segments[*index].unplaced_key(), Placement::key);
                    damage.key(*index) != Some(key)
                })
                .collect()
        };
        let mut clip_reissued = Vec::new();
        let mut paint = damage::PaintHasher::default();
        if let Some(damage) = self.damage.as_mut() {
            damage.begin_changed();
        }
        let mut placements: Vec<Option<Placement>> = placements;
        let mut placed = |index: usize| -> Placement {
            placements[index]
                .take()
                .unwrap_or_else(|| Placement::unplaced(segments[index].path.len()))
        };
        let mut kept: Vec<(usize, Placement)> = Vec::with_capacity(changed.len());
        for &index in &changed {
            let placement = placed(index);
            if let Some(damage) = self.damage.as_mut() {
                damage.segment(index, placement.key());
            }
            let from = self.layers.as_slice().len();
            self.replay_placed(
                segments[index].layers.clone(),
                &segments[index],
                &placement,
                &mut clip_reissued,
            );
            self.hash_replayed(from, &mut paint);
            kept.push((index, placement));
        }
        let damage = self.damage.as_mut().expect("damage is tracked");
        damage.finish_changed();

        let regions = match damage.pending() {
            Some(damage::Damage::Nothing) => return true,
            Some(damage::Damage::Region(regions)) => regions,
            _ => {
                damage.invalidate();
                self.reset_stack();
                return false;
            }
        };
        // A change over half the frame is painted in full.
        let frame = scene.bounds.width * scene.bounds.height;
        let area: f32 = regions
            .iter()
            .map(|region| region.width * region.height)
            .sum();
        if area * 2.0 > frame {
            damage.invalidate();
            self.reset_stack();
            return false;
        }

        // Unchanged segments painting into the damage are painted again;
        // one replayed where it was recorded needs only its layers painting
        // there, each the layer it replayed to last time.
        let painting = |extent: Option<Rectangle>| {
            extent.is_some_and(|extent| regions.iter().any(|region| extent.intersects(region)))
        };
        let mut repainted: Vec<(usize, Option<Vec<usize>>)> = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            if changed.binary_search(&index).is_ok() || segment.tracked(tracks) {
                if changed.binary_search(&index).is_ok() || painting(damage.extent(index)) {
                    repainted.push((index, None));
                }
            } else {
                let layers: Vec<usize> = damage
                    .layer_extents(index)
                    .enumerate()
                    .filter(|(_, extent)| painting(*extent))
                    .map(|(layer, _)| layer)
                    .collect();
                if !layers.is_empty() {
                    repainted.push((index, Some(layers)));
                }
            }
        }
        if repainted.len() != changed.len() {
            self.reset_stack();
            let mut clip_reissued = Vec::new();
            for (index, layers) in repainted {
                let segment = &segments[index];
                let placement = match kept.binary_search_by_key(&index, |(index, _)| *index) {
                    Ok(position) => std::mem::replace(
                        &mut kept[position].1,
                        Placement::unplaced(segment.path.len()),
                    ),
                    Err(_) => placed(index),
                };
                let layers = match layers {
                    None => segment.layers.clone(),
                    Some(layers) => layers
                        .into_iter()
                        .filter_map(|layer| segment.layers.get(layer).cloned())
                        .collect(),
                };
                self.replay_placed(layers, segment, &placement, &mut clip_reissued);
            }
        }
        true
    }

    /// Empties the layer stack, back to the bounds it was last reset to.
    fn reset_stack(&mut self) {
        self.layers.reset(self.stack_bounds);
        self.layers_generation += 1;
        self.active_alpha_groups.clear();
        self.active_clip_sources.clear();
        self.layer_scopes.clear();
        self.next_alpha_group = 0;
        self.alpha_segment = 0;
    }

    fn replay_segment(
        &mut self,
        mut layers: Vec<layer::Layer>,
        base: usize,
        recorded_clip: Rectangle,
        clip_depth: usize,
        // What the segment's first `k` nodes apply, pulled back through the
        // compositor group, for every `k` below its path's length.
        clip_maps: &[ReplayAffine],
        translation: core::Vector,
        opacity: f32,
        scale: f32,
        rotation: f32,
        scale_origin: core::Point,
        clip: Option<Rectangle>,
        clip_reissued: &mut Vec<(usize, usize)>,
    ) {
        if layers.is_empty() {
            return;
        }

        // What the node's ancestors apply; a clip opened at a shallower depth
        // moves only with the nodes that enclosed it then.
        let clip_map = clip_maps.last().copied().unwrap_or(ReplayAffine::IDENTITY);
        let map_at = |depth: usize| {
            clip_maps
                .get(depth.min(clip_maps.len().saturating_sub(1)))
                .copied()
                .unwrap_or(clip_map)
        };

        // Replay-space clips are pulled back through residual compositor scaling by the caller.
        let fixed_clip = map_at(clip_depth).map_rect(recorded_clip);

        // Enclosing viewports follow ancestors, not the leaf's own reflow translation.
        for layer in &mut layers {
            layer.restore_presentation_clip(clip_map, map_at);
            layer.map_layout_clip(clip_map);
        }

        let moves = translation.x != 0.0 || translation.y != 0.0 || !clip_map.is_identity();

        if moves {
            // `translate` distinguishes the node's own enclosing clip -- which
            // travels only with the node's ANCESTORS -- from a clip a
            // descendant opened inside the node, which travels with the node
            // (#2219 review finding 3). A descendant clip that leaves the fixed
            // clip entirely is dropped.
            layers.retain_mut(|layer| layer.translate(translation, fixed_clip));

            if layers.is_empty() {
                return;
            }
        }

        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };

        if opacity <= 0.0 {
            // A fully transparent node is not submitted at all. Skipping it is
            // the same picture and strictly less GPU work.
            return;
        }

        if let Some(clip) = clip {
            // A clip is applied by narrowing each replayed layer's own clip
            // region, not by opening a scope around them: `append` writes the
            // recorded layers in whole, so a surrounding scope's bounds would
            // be overwritten by the recorded ones and the clip would be a no-op
            // (observed: a clip track changed 0 pixels).
            //
            // The layer clipped to exactly the node's OWN recorded clip is the
            // one the effect opened for this frame's reveal, and a track
            // REPLACES it -- exactly as a track's opacity replaces the alpha
            // group the node was recorded under. Intersecting it instead makes
            // a GROWING reveal impossible: a `Size`/`Reserve` travel recorded
            // at a near-zero first strip could only ever be narrowed, so the
            // animation would freeze at its first extent and snap when the UI
            // thread finally drew again (#2219 effects review, High 1).
            //
            // Replacement is safe because the ink is all still there: the
            // recorded clip is a scissor rectangle, while the primitives, their
            // image/text clip bounds and their geometry were recorded against
            // the node's FULL reservation. Any layer NARROWER than the node's
            // own clip is a clip a descendant opened, and that one is still
            // only intersected -- a descendant must never be widened past what
            // it asked for.
            layers.retain_mut(|layer| {
                if layer.replace_presentation_clip(clip) {
                    return layer.bounds.width > 0.0 && layer.bounds.height > 0.0;
                }

                match layer.bounds.intersection(&clip) {
                    Some(bounds) => {
                        layer.bounds = bounds;
                        true
                    }
                    // Fully clipped away: dropping it is the same picture and
                    // strictly less GPU work.
                    None => false,
                }
            });

            if layers.is_empty() {
                return;
            }
        }

        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let rotation = if rotation.is_finite() { rotation } else { 0.0 };
        let mut composites = opacity < 1.0 || (scale - 1.0).abs() > f32::EPSILON || rotation != 0.0;
        // An opaque node of meshes turns its geometry instead (a spinner): no
        // offscreen group, so no extra passes (enzUI-ICED #3668).
        if composites
            && self.turns_geometry
            && opacity >= 1.0
            && layers
                .iter_mut()
                .all(|layer| layer.turn_meshes(base, scale, rotation, scale_origin, false))
        {
            for layer in &mut layers {
                let _ = layer.turn_meshes(base, scale, rotation, scale_origin, true);
            }
            composites = false;
        }
        let scopes = usize::from(composites);

        if composites {
            let bounds = layers
                .iter()
                .map(|layer| layer.bounds)
                .reduce(|a, b| a.union(&b))
                .unwrap_or(Rectangle::INFINITE);

            self.start_composite_layer(bounds, opacity, scale, rotation, scale_origin);
        }

        // Captured clip scopes span sibling paint segments; alpha scopes follow the live enclosing groups.
        // Reissued ids keep separate recordings and live groups from sharing a render target.
        let mut reissued: Vec<(usize, usize)> = Vec::new();

        for layer in &mut layers {
            let split = base.min(layer.alpha_groups.len());
            let inner = layer.alpha_groups.split_off(split);
            let enclosing_clips = layer
                .alpha_groups
                .iter()
                .filter(|group| group.clip.is_some())
                .copied()
                .map(|group| (group, false));
            let recorded_groups = enclosing_clips
                .chain(inner.into_iter().map(|group| (group, true)))
                .collect::<Vec<_>>();
            let mut groups = self.active_alpha_groups.clone();

            for (mut group, inside) in recorded_groups {
                let ids = if group.clip.is_some() {
                    &mut *clip_reissued
                } else {
                    &mut reissued
                };
                let id = match ids.iter().find(|(old, _)| *old == group.id) {
                    Some((_, new)) => *new,
                    None => {
                        let new = self.next_alpha_group;
                        self.next_alpha_group += 1;
                        ids.push((group.id, new));
                        new
                    }
                };

                let declared = layer
                    .clip_sources
                    .iter()
                    .any(|(source, _)| *source == group.id);
                group.id = id;
                if let Some((bounds, radius)) = group.clip {
                    group.clip = Some(if inside && !declared {
                        (bounds + translation, radius)
                    } else {
                        let map = map_at(group.depth);
                        (map.map_rect(bounds), radius * map.scale)
                    });
                }
                groups.push(group);
            }

            layer.clip_group = layer
                .clip_group
                .and_then(|old| {
                    clip_reissued
                        .iter()
                        .find(|(id, _)| *id == old)
                        .map(|(_, id)| *id)
                })
                .or_else(|| {
                    self.active_alpha_groups
                        .iter()
                        .rev()
                        .find(|group| group.clip.is_some())
                        .map(|group| group.id)
                });
            let mut sources = self.active_clip_sources.clone();
            sources.extend(layer.clip_sources.iter().filter_map(|(old, path)| {
                clip_reissued
                    .iter()
                    .find(|(id, _)| id == old)
                    .map(|(_, new)| (*new, path.clone()))
            }));
            layer.clip_sources = sources;
            layer.alpha_groups = groups;
            layer.alpha_segment = self.alpha_segment;
        }

        // Replay counts as a USE against fork patches 29 and 34, and it does so
        // BY CONSTRUCTION rather than through a side-channel hook:
        //
        //  * the appended layers go through the ordinary `prepare` pass with
        //    every live layer, so each replayed glyph and image is uploaded and
        //    touched exactly as a freshly drawn one would be; and
        //  * `text::Storage::trim` keeps an upload while `upload.text`
        //    (`Arc<[Text]>`) has a strong reference, and a retained `Scene`
        //    holds precisely that `Arc` through its cloned `text::Item::Cached`.
        //    Retention IS the strong reference, which is why this could not
        //    work before `Layer` became cloneable.
        self.layers.append(layers);
        self.tag_current_layer();

        for _ in 0..scopes {
            <Self as core::Renderer>::end_layer(self);
        }
    }

    /// Opens a renderer-owned offscreen group for retained replay properties.
    /// Unlike widget alpha groups, this may stay opaque and still exist solely
    /// to scale the complete node -- including text and custom primitives -- as
    /// one texture.
    fn start_composite_layer(
        &mut self,
        bounds: Rectangle,
        alpha: f32,
        scale: f32,
        rotation: f32,
        scale_origin: core::Point,
    ) {
        self.layers.push_clip(bounds);
        let reach = self
            .layers
            .clip_bounds_at(self.layers.active_len().saturating_sub(1))
            .filter(|clip| {
                [clip.x, clip.y, clip.width, clip.height]
                    .iter()
                    .all(|v| v.is_finite())
            })
            .map(|clip| composited_reach(clip, scale, rotation, scale_origin));
        let group = layer::AlphaGroup {
            id: self.next_alpha_group,
            alpha,
            scale,
            scale_origin,
            rotation,
            reach,
            clip: None,
            retained_clip: None,
            depth: self.node_depth(),
        };
        self.next_alpha_group += 1;
        self.active_alpha_groups.push(group);
        self.tag_current_layer();
        self.push_scope(LayerScope {
            alpha: Some(group),
            ..LayerScope::default()
        });
    }

    /// Whether the device this renderer draws with has been lost. A lost
    /// device renders nothing more; the compositor is rebuilt on a new one.
    pub fn device_lost(&self) -> bool {
        self.engine.device_lost()
    }

    pub(crate) fn confirm_device_lost(&self) -> bool {
        self.engine.confirm_device_lost()
    }

    pub fn new(engine: Engine, settings: renderer::Settings) -> Self {
        Self {
            settings,
            layers: layer::Stack::new(),
            scale: None,
            active_alpha_groups: Vec::new(),
            active_clip_sources: Vec::new(),
            layer_scopes: Vec::new(),
            recordings: Vec::new(),
            presentation_state: core::renderer::presentation::State::default(),
            sample_budget: core::renderer::presentation::SampleBudget::default(),
            presentation_recording: None,
            damage: None,
            frame_damage: None,
            frame_damage_ahead: None,
            canvas: None,
            direct_target: None,
            canvas_clear: None,
            clear_scissor: None,
            partial_frame: false,
            partial_frames: 0,
            full_frames: 0,
            partial_replay: None,
            stack_bounds: Rectangle::default(),
            replaying_full: false,
            changed_replays: 0,
            turns_geometry: true,
            paint_regions: Vec::new(),
            present_damage: None,
            images_pending: false,
            presentation_sample_valid: false,
            presentation_inputs: Vec::new(),
            presentation_tracks: Vec::new(),
            presentation_viewport: None,
            presentation_refusal: None,
            docked_recorder: core::renderer::docked::Recorder::default(),
            independent_replay: false,
            presentation_resources_active: false,
            media_claims: owned_media::Assets::default(),
            native_vector_claims: std::collections::BTreeMap::new(),
            native_vector_overflow: false,
            next_alpha_group: 0,
            alpha_segment: 0,
            alpha: alpha::State::new(&engine.device, engine.format),
            material: material::State::new(engine.format),

            clear: clear::State::new(&engine.device, &engine.clear_pipeline),
            quad: quad::State::new(),
            triangle: triangle::State::new(&engine.triangle_pipeline),
            text: text::State::new(),
            text_viewport: engine.text_pipeline.create_viewport(&engine.device),

            #[cfg(any(feature = "svg", feature = "image"))]
            image: image::State::new(),

            #[cfg(any(feature = "svg", feature = "image"))]
            image_cache: std::cell::RefCell::new(engine.create_image_cache()),

            // TODO: Resize belt smartly (?)
            // It would be great if the `StagingBelt` API exposed methods
            // for introspection to detect when a resize may be worth it.
            layers_generation: 0,
            prepared: None,
            reuse_next: false,
            merged: None,
            checker: None,
            window_mask: None,
            primary_render_passes: 0,
            pass_causes: [0; 3],
            rounded_elisions: 0,
            readback_audit_enabled: false,
            readback_audit_failure_only: false,
            last_readback_audit: None,
            staging_belt: wgpu::util::StagingBelt::new(
                engine.device.clone(),
                buffer::MAX_WRITE_SIZE as u64,
            ),

            engine,
        }
    }

    /// Creates the presentation worker's renderer with private font and atlas state.
    ///
    /// Retained replay requires owned text assets and refuses arbitrary primitive
    /// callbacks. Ordinary UI drawing continues to use [`Renderer::new`].
    pub fn new_presentation(engine: Engine, settings: renderer::Settings) -> Self {
        let mut renderer = Self::new(engine.independent_presentation(), settings);
        renderer.independent_replay = true;
        #[cfg(feature = "image")]
        renderer.image_cache.get_mut().disable_source_capture();
        renderer
    }

    fn prepare_scene_fonts(
        &mut self,
        scene: &Scene,
    ) -> Result<(), core::renderer::presentation::Error> {
        #[cfg(any(feature = "image", feature = "svg"))]
        {
            scene.media_assets.install(
                #[cfg(feature = "image")]
                &self.engine.device,
                #[cfg(feature = "image")]
                &self.engine.queue,
                &mut self.image_cache.borrow_mut(),
                &mut self.media_claims,
                &self.native_vector_claims,
                self.native_vector_overflow,
            )?;
            self.presentation_resources_active |= scene.media_assets.usage().0 > 0;
        }
        if !self.independent_replay {
            return Ok(());
        }
        if scene
            .segments
            .iter()
            .flat_map(|segment| &segment.layers)
            .any(|layer| !layer.primitives.is_empty())
        {
            return Err(core::renderer::presentation::Error::OpaquePaint);
        }
        if self.text.use_owned_fonts(scene.fonts.as_ref())? {
            self.presentation_resources_active = true;
            self.engine.text_pipeline = self
                .engine
                .text_pipeline
                .independent(&self.engine.device, &self.engine.queue);
            self.text_viewport = self
                .engine
                .text_pipeline
                .create_viewport(&self.engine.device);
        }
        Ok(())
    }

    fn retire_presentation_resources(&mut self) {
        if !self.presentation_resources_active {
            return;
        }
        self.presentation_resources_active = false;
        // A lost device makes every new texture invalid, and the text pipeline
        // allocates its atlas at once; the compositor replaces this renderer anyway.
        if self.engine.confirm_device_lost() {
            self.recordings.clear();
            self.presentation_recording = None;
            self.presentation_inputs.clear();
            self.presentation_tracks.clear();
            self.presentation_state.clear();
            self.presentation_sample_valid = false;
            return;
        }
        let bounds = self
            .presentation_viewport
            .map(Rectangle::with_size)
            .unwrap_or(Rectangle::INFINITE);
        self.layers.reset(bounds);
        self.layers_generation += 1;
        self.recordings.clear();
        self.presentation_recording = None;
        self.presentation_inputs.clear();
        self.presentation_tracks.clear();
        self.presentation_state.clear();
        self.presentation_sample_valid = false;
        self.presentation_refusal = None;
        self.text.retire_owned_fonts();
        self.engine.text_pipeline = self
            .engine
            .text_pipeline
            .independent(&self.engine.device, &self.engine.queue);
        self.text_viewport = self
            .engine
            .text_pipeline
            .create_viewport(&self.engine.device);

        #[cfg(any(feature = "image", feature = "svg"))]
        {
            self.image = image::State::new();
            let policy = self.image_cache.get_mut().policy();
            let mut cache = self.engine.create_image_cache();
            cache.set_policy(policy);
            #[cfg(feature = "image")]
            cache.disable_source_capture();
            *self.image_cache.get_mut() = cache;
        }
        self.media_claims = owned_media::Assets::default();
        self.native_vector_claims.clear();
        self.native_vector_overflow = false;
    }

    fn begin_rounded_group(&mut self) {
        let depth = self.node_depth();
        let (layer, _) = self.layers.current_mut();
        let clip_path = layer.clip_path.clone();
        let clip = layer.clip_radius.map(|radius| (layer.bounds, radius));
        let retained_clip = layer
            .retained_rounded_clip()
            .filter(|retained| Some(*retained) != clip);
        let group = clip
            .filter(|clip| {
                clip_path.is_some()
                    || !self
                        .active_alpha_groups
                        .iter()
                        .any(|g| g.clip == Some(*clip))
            })
            .map(|clip| {
                let group = layer::AlphaGroup {
                    id: self.next_alpha_group,
                    alpha: 1.0,
                    scale: 1.0,
                    scale_origin: Point::ORIGIN,
                    rotation: 0.0,
                    reach: None,
                    clip: Some(clip),
                    retained_clip,
                    depth,
                };
                self.next_alpha_group += 1;
                self.active_alpha_groups.push(group);
                if let Some(path) = &clip_path {
                    self.active_clip_sources.push((group.id, path.clone()));
                }
                group
            });
        let rounded_from = group
            .is_some()
            .then(|| self.layers.active_len().saturating_sub(1));
        self.tag_current_layer();
        self.push_scope(LayerScope {
            alpha: group,
            rounded_from,
            ..LayerScope::default()
        });
    }

    /// Drops a rounded clip group whose mask changes no pixel, so its layers
    /// clip each primitive themselves instead of compositing a target.
    fn elide_rounded_group(&mut self, group: layer::AlphaGroup, layers: std::ops::Range<usize>) {
        let range = self.layers.range_mut(layers);
        if !rounded_elision::is_redundant(range, &group) {
            return;
        }
        for layer in range {
            layer.alpha_groups.retain(|active| active.id != group.id);
            // A replay reissues no clip of a group that is gone; the layer
            // keeps its own clip, which replay moves with it.
            layer.clip_sources.retain(|(id, _)| *id != group.id);
            // An outer clip group still masks this layer, but it owns this
            // layer's rounded clip only if it is the same clip; otherwise the
            // layer clips its own primitives.
            let own = layer.clip_radius.map(|radius| (layer.bounds, radius));
            layer.clip_group = layer
                .alpha_groups
                .iter()
                .rev()
                .find(|active| active.clip.is_some())
                .filter(|active| own.is_none() || active.clip == own)
                .map(|active| active.id);
        }
        self.rounded_elisions += 1;
    }

    /// How many recorded nodes enclose what is drawn now.
    fn node_depth(&self) -> usize {
        self.recordings
            .last()
            .map_or(0, |recording| recording.path.len())
    }

    fn push_scope(&mut self, mut scope: LayerScope) {
        scope.depth = self.node_depth();
        self.layer_scopes.push(scope);
    }

    fn tag_current_layer(&mut self) {
        let groups = self.active_alpha_groups.clone();
        let segment = self.alpha_segment;
        let (layer, _) = self.layers.current_mut();
        layer.clip_group = groups
            .iter()
            .rev()
            .find(|group| group.clip.is_some())
            .map(|group| group.id);
        layer.alpha_groups = groups;
        layer.clip_sources = self.active_clip_sources.clone();
        layer.alpha_segment = segment;
    }

    /// Rounded clip groups dropped since the renderer was created because their
    /// mask could not change a pixel (enzUI-ICED #3406).
    pub fn rounded_clip_elisions(&self) -> u64 {
        self.rounded_elisions
    }

    /// Returns alpha-group pool and command counts for the most recent frame.
    pub fn alpha_group_statistics(&self) -> AlphaGroupStatistics {
        self.alpha.statistics()
    }

    /// Returns material pass, composite and target counts for the most recent
    /// frame (enzUI-ICED #2950).
    ///
    /// A frame that drew no material reports zeros, including zero retained
    /// targets: the offscreen scene a material needs is not allocated, and the
    /// direct path to the frame is taken.
    pub fn material_statistics(&self) -> MaterialStatistics {
        self.material.statistics()
    }

    // `ENZUI_RENDER_PASS_TRACE=1` prints each presented frame's render
    // passes; a Vulkan driver begins a command buffer for each (enzUI-ICED
    // #3244).
    fn trace_render_passes(&self) {
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*TRACE.get_or_init(|| {
            std::env::var_os("ENZUI_RENDER_PASS_TRACE")
                .is_some_and(|value| value == "1" || value == "2")
        }) {
            return;
        }
        if std::env::var_os("ENZUI_RENDER_PASS_TRACE").is_some_and(|value| value == "2") {
            let mut groups: Vec<layer::AlphaGroup> = Vec::new();
            for layer in self.layers.iter() {
                for group in &layer.alpha_groups {
                    if !groups.iter().any(|known| known.id == group.id) {
                        groups.push(*group);
                    }
                }
            }
            for group in groups {
                eprintln!(
                    "enzui-render-group alpha={} clip={:?} depth={}",
                    group.alpha,
                    group.clip.map(|(bounds, radius)| (
                        bounds.x.round(),
                        bounds.y.round(),
                        bounds.width.round(),
                        bounds.height.round(),
                        radius.top_left
                    )),
                    group.depth
                );
            }
        }
        let material = self.material.statistics();
        eprintln!(
            "enzui-render-passes primary={} meshes={} groups={} primitives={} material={} \
             material_boundaries={} material_regions={} material_filter={} \
             material_composite={} material_fallback={}",
            self.primary_render_passes,
            self.pass_causes[0],
            self.pass_causes[1],
            self.pass_causes[2],
            material.render_passes,
            material.boundaries,
            material.regions,
            material.filter_passes,
            material.composites,
            material.fallback_composites,
        );
    }

    /// Changes how long unused renderer-owned image resources are retained.
    #[cfg(any(feature = "image", feature = "svg"))]
    pub fn set_image_cache_policy(&mut self, policy: ImageCachePolicy) {
        self.image_cache.get_mut().set_policy(policy);
    }

    /// Returns image/SVG cache, upload, eviction, and atlas mechanism counts.
    #[cfg(any(feature = "image", feature = "svg"))]
    pub fn image_cache_statistics(&self) -> ImageCacheStatistics {
        self.image_cache.borrow().statistics()
    }

    /// Returns resources currently retained by this renderer for presentation replay.
    pub fn retained_resource_statistics(&self) -> RetainedResourceStatistics {
        let fonts = self.text.owned_font_usage();
        let (media_resources, media_bytes) = self.media_claims.usage();
        #[cfg(any(feature = "image", feature = "svg"))]
        let images = self.image_cache.borrow().statistics();
        RetainedResourceStatistics {
            font_resources: fonts.resources,
            font_bytes: fonts.bytes,
            media_resources,
            media_bytes,
            #[cfg(any(feature = "image", feature = "svg"))]
            cached_media_entries: images
                .raster_entries
                .saturating_add(images.svg_entries)
                .saturating_add(images.svg_raster_entries),
            #[cfg(not(any(feature = "image", feature = "svg")))]
            cached_media_entries: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            cached_media_bytes: images.raster_bytes.saturating_add(images.svg_bytes),
            #[cfg(not(any(feature = "image", feature = "svg")))]
            cached_media_bytes: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            atlas_allocations: images.atlas_allocations,
            #[cfg(not(any(feature = "image", feature = "svg")))]
            atlas_allocations: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            atlas_bytes: images.atlas_occupied_bytes,
            #[cfg(not(any(feature = "image", feature = "svg")))]
            atlas_bytes: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            pending_media_jobs: images.pending_jobs,
            #[cfg(not(any(feature = "image", feature = "svg")))]
            pending_media_jobs: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            pending_callbacks: images.pending_callbacks,
            #[cfg(not(any(feature = "image", feature = "svg")))]
            pending_callbacks: 0,
            #[cfg(any(feature = "image", feature = "svg"))]
            pending_callback_bytes: images.pending_callback_bytes,
            #[cfg(not(any(feature = "image", feature = "svg")))]
            pending_callback_bytes: 0,
        }
    }

    /// Returns quad clip-layer buffer, upload, draw and reclamation counts.
    ///
    /// enzICED #1515.
    pub fn quad_statistics(&self) -> QuadStatistics {
        self.quad.statistics()
    }

    /// Returns text glyph rasterization, upload and atlas growth mechanism counts.
    ///
    /// enzICED #1514.
    pub fn text_atlas_statistics(&self) -> TextAtlasStatistics {
        self.engine.text_pipeline.statistics()
    }

    /// Caps text atlas growth at `max_size` texels per side.
    ///
    /// enzICED #1514.
    pub fn set_text_atlas_max_size(&self, max_size: u32) {
        self.engine.text_pipeline.set_max_atlas_size(max_size);
    }

    /// Resets the counters returned by [`Renderer::text_atlas_statistics`].
    ///
    /// enzICED #1514.
    pub fn reset_text_atlas_statistics(&self) {
        self.engine.text_pipeline.reset_statistics();
    }

    /// Checks the physical vector-raster envelope before acquisition or submission.
    pub fn validate_owned_media_viewport(
        &mut self,
        viewport: &Viewport,
    ) -> Result<(), core::renderer::presentation::Error> {
        #[cfg(any(feature = "image", feature = "svg"))]
        if let Err(error) = self.media_claims.validate_viewport(
            self.layers.iter(),
            viewport.scale_factor(),
            self.engine.device.limits().max_texture_dimension_2d,
        ) {
            self.presentation_refusal = Some(error);
            return Err(error);
        }
        Ok(())
    }

    /// Record commands that draw the current primitives to the target texture view.
    ///
    /// You must call [`finish`](Self::finish) and [`recall`](Self::recall) when submitting
    /// the resulting [`wgpu::CommandEncoder`].
    pub fn draw(
        &mut self,
        clear_color: Option<Color>,
        target: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> wgpu::CommandEncoder {
        self.validate_owned_media_viewport(viewport)
            .expect("Owned vector frame exceeds its raster envelope");
        let mut encoder =
            self.engine
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("iced_wgpu encoder"),
                });

        let target_prepared = Prepared {
            layers_generation: self.layers_generation,
            physical_size: viewport.physical_size(),
            scale_factor: viewport.scale_factor().to_bits(),
            text_epoch: 0,
        };
        let reuse = std::mem::take(&mut self.reuse_next)
            && self.prepared.as_ref().is_some_and(|prepared| {
                Prepared {
                    text_epoch: prepared.text_epoch,
                    ..target_prepared
                } == *prepared
                    && prepared.text_epoch == self.text_epoch()
            });

        if reuse {
            // enzUI-ICED #3442: everything but the custom primitives is as it
            // was prepared for the last frame, and its buffers are intact
            // because nothing was trimmed since.
            self.prepare_primitives(viewport);
            self.render_regions(&mut encoder, target, clear_color, viewport);
            self.engine.trim_primitives();
            return encoder;
        }

        self.prepare(&mut encoder, viewport, clear_color);
        self.prepared = Some(Prepared {
            text_epoch: self.text_epoch(),
            ..target_prepared
        });
        self.render_regions(&mut encoder, target, clear_color, viewport);

        self.quad.trim();
        self.triangle.trim();
        self.text.trim();
        #[cfg(any(feature = "svg", feature = "image"))]
        self.image.trim();

        if self.partial_frame {
            // Unpainted layers still own their glyphs; image liveness was marked before setting their paint aside.
            self.engine.trim_primitives();
        } else {
            self.engine.trim();
        }

        #[cfg(any(feature = "svg", feature = "image"))]
        self.image_cache
            .borrow_mut()
            .trim(&self.engine.device, &mut encoder);

        encoder
    }

    pub fn present(
        &mut self,
        clear_color: Option<Color>,
        _format: wgpu::TextureFormat,
        frame: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> wgpu::SubmissionIndex {
        let (submission, _spans) = self.present_measured(clear_color, frame, viewport, false);
        submission
    }

    /// Writes a frame about to be presented to `ICED_FRAME_DUMP`, if set.
    pub(crate) fn dump_presented(&self, texture: &wgpu::Texture, lane: bool) {
        frame_dump::capture(&self.engine.device, &self.engine.queue, texture, lane);
    }

    /// Keeps the lane's last frame from now on, so a replay that moved only
    /// small nodes paints again only where they were and are
    /// (enzUI-ICED #3666); `false` paints every frame in full again.
    pub fn track_damage(&mut self, enabled: bool) {
        if !enabled {
            self.damage = None;
            self.canvas = None;
        } else if self.damage.is_none() {
            self.damage = Some(damage::Tracker::default());
        }
    }

    /// How many frames painted only the part of the canvas they changed.
    pub fn partial_frames(&self) -> u64 {
        self.partial_frames
    }

    /// How many frames presented through the canvas painted all of it.
    pub fn full_frames(&self) -> u64 {
        self.full_frames
    }

    /// How many replays of a scene replayed only the runs whose placement
    /// changed, and those painting where it changed (enzUI-ICED #3668).
    pub fn changed_replays(&self) -> u64 {
        self.changed_replays
    }

    /// The canvas the last lane frame was drawn into, if it keeps one.
    pub(crate) fn canvas_texture(&self) -> Option<&wgpu::Texture> {
        self.canvas.as_ref().map(|canvas| &canvas.texture)
    }

    /// Keeps the window's last frame from now on, so a frame drawn by
    /// widgets paints again only where it differs from the last one
    /// presented (enzUI-ICED #3669); `false` paints every frame in full
    /// again.
    pub fn track_frame_damage(&mut self, enabled: bool) {
        if !enabled {
            self.frame_damage = None;
            self.canvas = None;
        } else if self.frame_damage.is_none() {
            self.frame_damage = Some(damage::FrameTracker::default());
        }
    }

    /// A copy of the layer stack as the next frame draws it (flushed and
    /// merged), to be drawn again by [`Self::present_snapshot`].
    pub(crate) fn layer_snapshot(&mut self) -> layer::Stack {
        self.layers.flush_active();
        self.merge_layers();
        self.layers.clone()
    }

    /// Presents `snapshot` in full into `target` with a second renderer, as
    /// the frame it was taken from would be drawn in full the first time.
    /// Returns `false` when an image the frame shows is still loading in the
    /// second renderer, so `target` lacks it.
    pub(crate) fn present_snapshot(
        &mut self,
        snapshot: layer::Stack,
        clear_color: Color,
        format: wgpu::TextureFormat,
        target: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> bool {
        let checker = self
            .checker
            .get_or_insert_with(|| Box::new(Renderer::new(self.engine.clone(), self.settings)));
        checker.layers = snapshot;
        checker.layers_generation += 1;
        // Merged already.
        checker.merged = Some(checker.layers_generation);
        checker.active_clip_sources = self.active_clip_sources.clone();
        let _ = checker.present(Some(clear_color), format, target, viewport);
        #[cfg(any(feature = "svg", feature = "image"))]
        if checker.image_cache.borrow().has_pending() {
            return false;
        }
        true
    }

    /// Whether frames drawn by widgets are painted only where they changed
    /// ([`track_frame_damage`](Self::track_frame_damage)).
    pub fn tracks_frame_damage(&self) -> bool {
        self.frame_damage.is_some()
    }

    /// Whether this renderer presents through a kept canvas
    /// ([`track_damage`](Self::track_damage),
    /// [`track_frame_damage`](Self::track_frame_damage)).
    pub fn tracks_damage(&self) -> bool {
        self.damage.is_some() || self.frame_damage.is_some()
    }

    /// Whether the frame drawn by widgets since the last present is the one
    /// the canvas already holds, so presenting it would change nothing on
    /// screen and the present can be skipped (no swapchain image, copy or
    /// commit).
    ///
    /// Answers only for a renderer tracking frame damage whose canvas fits
    /// `viewport` and was cleared to `clear_color`, with no image loading.
    /// The damage it measures is this frame's: the present that follows a
    /// `false` uses it, and one that cannot follow must
    /// [`discard_frame_damage`](Self::discard_frame_damage).
    pub fn unchanged_frame(&mut self, clear_color: Color, viewport: &Viewport) -> bool {
        let size = viewport.physical_size();
        let scale = viewport.scale_factor();
        if self.frame_damage.is_none()
            || self.images_pending
            || self.canvas_clear != Some(clear_color)
            || !self
                .canvas
                .as_ref()
                .is_some_and(|canvas| canvas.fits(size, canvas.texture.format(), scale))
        {
            return false;
        }
        self.layers.flush_active();
        self.merge_layers();
        let Some(tracker) = self.frame_damage.as_mut() else {
            return false;
        };
        let damage = tracker.frame(self.layers.as_slice(), land, damage_extent);
        if damage == damage::Damage::Nothing {
            return true;
        }
        self.frame_damage_ahead = Some(damage);
        false
    }

    /// Forgets the canvas and any damage measured ahead of a present that did
    /// not happen, so the next present paints in full: the damage tracker
    /// already counts the frame the canvas never received.
    pub fn discard_frame_damage(&mut self) {
        if self.frame_damage_ahead.take().is_some() {
            self.canvas = None;
        }
    }

    /// Presents the frame into `target`, a surface texture whose content is
    /// not kept: drawn into the canvas, in full or only where it changed,
    /// then copied to `target`. A lane frame changed where its replay did; a
    /// frame drawn by widgets where its layers differ from the last frame
    /// presented.
    ///
    /// A frame is painted in full after a new scene, a resize, a new clear
    /// color or an owned-layout replay, for a visible material (drawn by
    /// widgets: one whose backdrop or paint it changed) or (drawn by
    /// widgets) a custom primitive, or when the change covers half the
    /// frame; otherwise only the layers that reach the changed rectangle are
    /// painted, clipped to it, over the canvas.
    /// enzUI: what the last [`Self::present_damaged`] frame changed, as
    /// `[x, y, width, height]` rectangles in physical pixels; `None` for the
    /// whole frame. Taken once.
    pub fn take_present_damage(&mut self) -> Option<Vec<[u32; 4]>> {
        self.present_damage.take()
    }

    pub fn present_damaged(
        &mut self,
        clear_color: Color,
        target: &wgpu::Texture,
        viewport: &Viewport,
        measure: bool,
    ) -> (wgpu::SubmissionIndex, Option<EncodeSpans>) {
        let size = viewport.physical_size();
        let format = target.format();
        let scale = viewport.scale_factor();
        let frame = Rectangle::new(
            Point::ORIGIN,
            Size::new(size.width as f32, size.height as f32),
        );
        let prepare_started = measure.then(std::time::Instant::now);
        let fresh = !self
            .canvas
            .as_ref()
            .is_some_and(|canvas| canvas.fits(size, format, scale));
        if fresh {
            self.canvas = Some(damage::Canvas::new(
                &self.engine.device,
                size,
                format,
                scale,
            ));
        }
        if self.frame_damage.is_some() {
            // Queued text and meshes are hashed as what they paint, and the
            // stack as it is drawn, so the paint a partial frame sets aside
            // goes back to the layers it came from.
            self.layers.flush_active();
            self.merge_layers();
        }
        let damage = match (self.frame_damage_ahead.take(), self.frame_damage.as_mut()) {
            (Some(damage), _) => damage,
            (None, Some(tracker)) => tracker.frame(self.layers.as_slice(), land, damage_extent),
            (None, None) => self
                .damage
                .as_mut()
                .map_or(damage::Damage::Full, damage::Tracker::take),
        };
        let cleared = self.canvas_clear.replace(clear_color) != Some(clear_color);
        // What the frame changes on the canvas: the regions, even when they
        // are painted as a whole frame, unless a material or a primitive
        // could change pixels outside them.
        let mut present_damage = None;
        let damage = match damage {
            _ if fresh || cleared || self.images_pending => damage::Damage::Full,
            damage::Damage::Region(regions) => {
                // Whole physical pixels, outward, so no partly covered pixel
                // keeps stale paint.
                self.paint_regions.clear();
                self.paint_regions
                    .extend(regions.iter().filter_map(|region| {
                        let physical = *region * Transformation::scale(scale);
                        let left = physical.x.floor().max(0.0);
                        let top = physical.y.floor().max(0.0);
                        let right = (physical.x + physical.width).ceil().min(frame.width);
                        let bottom = (physical.y + physical.height).ceil().min(frame.height);
                        (right > left && bottom > top).then_some(Rectangle {
                            x: left as u32,
                            y: top as u32,
                            width: (right - left) as u32,
                            height: (bottom - top) as u32,
                        })
                    }));
                let area: u64 = self
                    .paint_regions
                    .iter()
                    .map(|region| u64::from(region.width) * u64::from(region.height))
                    .sum();
                // A frame drawn by widgets repaints only where nothing it
                // changed is under or near a material: a material is
                // composited from what is behind it, and a lane frame does
                // not tell where that changed.
                let changed: Vec<Rectangle> = regions.iter().copied().collect();
                let materials = self.layers.iter().any(|layer| {
                    if self.frame_damage.is_some() {
                        crate::material::reaches(
                            &layer.materials,
                            layer.bounds,
                            scale,
                            frame,
                            &changed,
                        )
                    } else {
                        crate::material::any_visible(&layer.materials, layer.bounds, scale, frame)
                    }
                });
                // A custom primitive that cannot tell what it paints is
                // painted with the whole frame.
                let primitives = self.frame_damage.is_some()
                    && self.layers.iter().any(|layer| {
                        layer
                            .primitives
                            .iter()
                            .any(|instance| !instance.paint(&mut |_, _| {}))
                    });
                if !materials && !primitives {
                    present_damage = Some(
                        self.paint_regions
                            .iter()
                            .map(|region| [region.x, region.y, region.width, region.height])
                            .collect(),
                    );
                }
                if self.paint_regions.is_empty() {
                    damage::Damage::Nothing
                } else if materials
                    || primitives
                    || area * 2 > u64::from(size.width) * u64::from(size.height)
                {
                    damage::Damage::Full
                } else {
                    damage::Damage::Region(regions)
                }
            }
            damage => damage,
        };
        self.present_damage = match damage {
            damage::Damage::Nothing => Some(Vec::new()),
            _ => present_damage,
        };
        if damage == damage::Damage::Full {
            // Regions measured for a frame then painted in full would each
            // paint every layer again.
            self.paint_regions.clear();
        }

        // A frame painted in full needs every segment, not just the changed
        // ones a partial replay left in the stack.
        let partial_replay = self.partial_replay.take();
        if let (damage::Damage::Full, Some((recording, tracks, at))) = (damage, partial_replay) {
            <Self as core::Renderer>::reset(self, self.stack_bounds);
            self.replaying_full = true;
            let _ = <Self as core::Renderer>::replay_tracks_at(self, &recording, &tracks, at);
            self.replaying_full = false;
            if let Some(tracker) = self.damage.as_mut() {
                let _ = tracker.take();
            }
        }
        let mut encoder = match damage {
            damage::Damage::Nothing => {
                self.engine
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("iced_wgpu encoder"),
                    })
            }
            damage::Damage::Full => {
                self.full_frames += 1;
                let canvas = self.canvas.take().expect("the canvas was just made");
                self.direct_target = Some(canvas.texture.clone());
                let encoder = self.draw(Some(clear_color), &canvas.view, viewport);
                self.direct_target = None;
                self.canvas = Some(canvas);
                encoder
            }
            damage::Damage::Region(_) => {
                let logical: Vec<Rectangle> = self
                    .paint_regions
                    .iter()
                    .map(|region| {
                        Rectangle::<f32>::from(*region) * Transformation::scale(1.0 / scale)
                    })
                    .collect();
                // Layers keep their bounds (a rounded group's mask is its
                // layer's clip); each region's scissor limits them instead.
                // A lane replays its layers again for the next frame; the
                // layers of a frame drawn by widgets are kept, for a
                // screenshot or a content-only frame, so their paint is only
                // set aside.
                // The materials of the layers painted are clear of every
                // change (the frame would be painted in full otherwise), so
                // they are set aside too and the canvas keeps them.
                let keep = self.frame_damage.is_some();
                // A window with custom primitives (each telling what it
                // paints) and no material is prepared whole and painted only
                // within the regions, so a content-only frame can still
                // present again what this one prepared.
                let whole = keep
                    && self.layers.iter().any(|layer| !layer.primitives.is_empty())
                    && self.layers.iter().all(|layer| layer.materials.is_empty());
                #[cfg(any(feature = "image", feature = "svg"))]
                {
                    let mut cache = self.image_cache.borrow_mut();
                    let frame = Rectangle::with_size(viewport.logical_size());
                    for layer in self.layers.iter() {
                        if let Some(visible) = layer.bounds.intersection(&frame) {
                            cache.mark_used(&layer.images, scale, visible);
                        }
                    }
                }
                let mut aside = Vec::new();
                let mut materials_aside = Vec::new();
                for (index, layer) in self.layers.iter_mut().enumerate().filter(|_| !whole) {
                    if !damage_extent(layer).is_some_and(|extent| {
                        logical.iter().any(|region| extent.intersects(region))
                    }) {
                        if keep {
                            aside.push((index, layer.take_paint()));
                        } else {
                            layer.clear_paint();
                        }
                    } else if keep && !layer.materials.is_empty() {
                        materials_aside.push((index, std::mem::take(&mut layer.materials)));
                    }
                }
                // Prepared once for every region.
                self.clear_scissor = self.paint_regions.iter().copied().reduce(|a, b| {
                    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
                    Rectangle {
                        x,
                        y,
                        width: (a.x + a.width).max(b.x + b.width) - x,
                        height: (a.y + a.height).max(b.y + b.height) - y,
                    }
                });
                self.partial_frame = true;
                self.partial_frames += 1;
                let canvas = self.canvas.take().expect("the canvas was just made");
                self.direct_target = Some(canvas.texture.clone());
                let encoder = self.draw(Some(clear_color), &canvas.view, viewport);
                self.direct_target = None;
                self.canvas = Some(canvas);
                self.clear_scissor = None;
                self.partial_frame = false;
                // Prepared for part of the stack only: a content-only frame
                // draws its widgets again instead.
                if !whole {
                    self.prepared = None;
                }
                self.paint_regions.clear();
                let layers = self.layers.range_mut(0..usize::MAX);
                for (index, paint) in aside {
                    layers[index].restore_paint(paint);
                }
                for (index, materials) in materials_aside {
                    layers[index].materials = materials;
                }
                encoder
            }
        };

        #[cfg(any(feature = "svg", feature = "image"))]
        {
            self.images_pending = self.image_cache.borrow().has_pending();
        }
        let canvas = self.canvas.as_ref().expect("the canvas was just made");
        encoder.copy_texture_to_texture(
            canvas.texture.as_image_copy(),
            target.as_image_copy(),
            wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
        );

        self.staging_belt.finish();
        let prepare_ended = measure.then(std::time::Instant::now);
        let submission = self.engine.queue.submit([encoder.finish()]);
        self.staging_belt.recall();
        self.trace_render_passes();

        let spans = match (prepare_started, prepare_ended) {
            (Some(started), Some(ended)) => Some(EncodeSpans {
                prepare: (started, ended),
                submit: (ended, std::time::Instant::now()),
            }),
            _ => None,
        };
        (submission, spans)
    }

    /// [`present`](Self::present), optionally reporting where preparation and
    /// encoding ended and queue submission began (enzICED #2250).
    ///
    /// Preparation and command encoding used to happen inside a span the stage
    /// ledger labelled `Submit`, so a frame that was expensive to encode was
    /// indistinguishable from one the queue was slow to take. The boundary is
    /// measured here because it exists nowhere else: from the compositor,
    /// `present` is one call.
    ///
    /// `measure` is honoured rather than assumed: with `false` no timestamp is
    /// taken at all, which is what an ordinary build does on every frame.
    pub fn present_measured(
        &mut self,
        clear_color: Option<Color>,
        frame: &wgpu::TextureView,
        viewport: &Viewport,
        measure: bool,
    ) -> (wgpu::SubmissionIndex, Option<EncodeSpans>) {
        let prepare_started = measure.then(std::time::Instant::now);

        let encoder = self.draw(clear_color, frame, viewport);

        self.staging_belt.finish();

        let prepare_ended = measure.then(std::time::Instant::now);

        let submission = self.engine.queue.submit([encoder.finish()]);
        self.staging_belt.recall();
        self.trace_render_passes();

        let spans = match (prepare_started, prepare_ended) {
            (Some(started), Some(ended)) => Some(EncodeSpans {
                prepare: (started, ended),
                submit: (ended, std::time::Instant::now()),
            }),
            _ => None,
        };

        (submission, spans)
    }

    /// Renders the current surface to an offscreen buffer.
    ///
    /// Returns RGBA bytes of the texture data.
    pub fn screenshot(&mut self, viewport: &Viewport, background_color: Color) -> Vec<u8> {
        let [pixels] = self.screenshot_copies::<1>(viewport, background_color, None);
        pixels
    }

    /// Copies one rendered frame into two independently mapped RGBA buffers.
    ///
    /// This diagnostic renders once, without a second layout, replay or draw.
    ///
    /// # Panics
    /// Panics on submission or mapping failure, or if the GPU wait exceeds five seconds.
    pub fn screenshot_pair(
        &mut self,
        viewport: &Viewport,
        background_color: Color,
    ) -> [Vec<u8>; 2] {
        self.screenshot_copies::<2>(
            viewport,
            background_color,
            Some(std::time::Duration::from_secs(5)),
        )
    }

    /// Enables a GPU alpha scan on diagnostic screenshots; disabled by default.
    ///
    /// The scan follows the ordinary texture copies and reads the native render target.
    /// It changes GPU scheduling.
    /// It is not part of ordinary presentation or performance measurement.
    pub fn enable_readback_audit(&mut self) {
        self.readback_audit_enabled = true;
        self.readback_audit_failure_only = false;
    }

    /// Scans the unchanged GPU target only after an opaque screenshot yields nonopaque bytes.
    ///
    /// Successful frames add no GPU commands. The failing frame's ordinary copies and
    /// mapping finish before a separately submitted scan, bounded to five seconds.
    pub fn enable_deferred_readback_audit(&mut self) {
        self.readback_audit_enabled = true;
        self.readback_audit_failure_only = true;
    }

    /// Last GPU nonopaque-pixel count and first row-major index (`u32::MAX` if none).
    pub fn readback_audit(&self) -> Option<[u32; 2]> {
        self.last_readback_audit
    }

    /// Text/quad pass count from the last draw, excluding private alpha-composite passes.
    pub fn primary_render_passes(&self) -> usize {
        self.primary_render_passes
    }

    /// Process-local resource identities and paint counts, not persistent scene keys.
    ///
    /// Owned-font identities are ordered as immutable snapshot and mutable font system.
    /// This diagnostic does not acquire the UI font lock or the atlas write lock.
    pub fn readback_state(&self) -> String {
        let mut counts = [0usize; 6];
        for layer in self.layers.iter() {
            counts[0] += 1;
            counts[1] += usize::from(!layer.quads.is_empty());
            counts[2] += layer.text.len();
            counts[3] += layer.images.len();
            counts[4] += layer.triangles.len() + layer.primitives.len();
            counts[5] = counts[5].max(layer.alpha_groups.len());
        }
        format!(
            "thread={:?} independent={} passes={} gpu_alpha_audit={:?} format={:?} gamma={} text_atlas={} owned_fonts={:?} primitive_storage={:p} layers/quad_layers/text_batches/images/meshes_and_primitives/alpha_depth={counts:?}",
            std::thread::current().id(),
            self.independent_replay,
            self.primary_render_passes,
            self.last_readback_audit,
            self.engine.format,
            graphics::color::GAMMA_CORRECTION,
            self.engine.text_pipeline.atlas_identity(),
            self.text.owned_font_identity(),
            std::sync::Arc::as_ptr(&self.engine.primitive_storage)
        )
    }

    fn screenshot_copies<const COPIES: usize>(
        &mut self,
        viewport: &Viewport,
        background_color: Color,
        timeout: Option<std::time::Duration>,
    ) -> [Vec<u8>; COPIES] {
        #[derive(Clone, Copy, Debug)]
        struct BufferDimensions {
            width: u32,
            height: u32,
            unpadded_bytes_per_row: usize,
            padded_bytes_per_row: usize,
        }

        impl BufferDimensions {
            fn new(size: Size<u32>) -> Self {
                let unpadded_bytes_per_row = size.width as usize * 4;
                let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
                let padded_bytes_per_row_padding =
                    (alignment - unpadded_bytes_per_row % alignment) % alignment;
                let padded_bytes_per_row = unpadded_bytes_per_row + padded_bytes_per_row_padding;

                Self {
                    width: size.width,
                    height: size.height,
                    unpadded_bytes_per_row,
                    padded_bytes_per_row,
                }
            }
        }

        let dimensions = BufferDimensions::new(viewport.physical_size());

        let texture_extent = wgpu::Extent3d {
            width: dimensions.width,
            height: dimensions.height,
            depth_or_array_layers: 1,
        };

        let texture = self.engine.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu.offscreen.source_texture"),
            size: texture_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.engine.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self.draw(Some(background_color), &view, viewport);
        self.last_readback_audit = None;
        let audit_texture = self.readback_audit_enabled.then(|| texture.clone());

        let texture = crate::color::convert(
            &self.engine.device,
            &mut encoder,
            texture,
            if graphics::color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
        );

        let output_buffers: [wgpu::Buffer; COPIES] = std::array::from_fn(|_| {
            let buffer = self.engine.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu.offscreen.output_texture_buffer"),
                size: (dimensions.padded_bytes_per_row * dimensions.height as usize) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(dimensions.padded_bytes_per_row as u32),
                        rows_per_image: None,
                    },
                },
                texture_extent,
            );
            buffer
        });
        let audit = audit_texture
            .as_ref()
            .filter(|_| !self.readback_audit_failure_only)
            .map(|texture| {
                readback_audit::Audit::record(&self.engine.device, &mut encoder, texture)
            });

        self.staging_belt.finish();
        let index = self.engine.queue.submit([encoder.finish()]);
        self.staging_belt.recall();
        let audit = audit.map(readback_audit::Audit::map);
        let (send, receive) = std::sync::mpsc::sync_channel(COPIES);
        for (index, buffer) in output_buffers.iter().enumerate() {
            let send = send.clone();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = send.try_send((index, result));
                });
        }
        drop(send);
        let _ = self
            .engine
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout,
            })
            .expect("Screenshot submission must complete before readback");
        self.last_readback_audit = audit.map(readback_audit::Pending::finish);
        let mut mapped = [false; COPIES];
        for _ in 0..COPIES {
            let (index, result) = receive
                .try_recv()
                .expect("Screenshot mapping callback must complete");
            result.expect("Screenshot buffer mapping must succeed");
            assert!(!mapped[index], "Screenshot mapping callback repeated");
            mapped[index] = true;
        }
        let pixels: [Vec<u8>; COPIES] = std::array::from_fn(|index| {
            let buffer = &output_buffers[index];
            let view = buffer.slice(..).get_mapped_range();
            let mut pixels =
                Vec::with_capacity(dimensions.unpadded_bytes_per_row * dimensions.height as usize);
            for row in view.chunks_exact(dimensions.padded_bytes_per_row) {
                pixels.extend_from_slice(&row[..dimensions.unpadded_bytes_per_row]);
            }
            drop(view);
            buffer.unmap();
            pixels
        });
        if self.readback_audit_failure_only
            && background_color.a == 1.0
            && pixels
                .iter()
                .any(|copy| copy.chunks_exact(4).any(|pixel| pixel[3] != 255))
        {
            let mut diagnostic =
                self.engine
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("deferred readback audit"),
                    });
            let audit = readback_audit::Audit::record(
                &self.engine.device,
                &mut diagnostic,
                audit_texture.as_ref().unwrap(),
            );
            let submitted = self.engine.queue.submit([diagnostic.finish()]);
            let pending = audit.map();
            let _ = self
                .engine
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submitted),
                    timeout: Some(std::time::Duration::from_secs(5)),
                })
                .expect("Deferred GPU audit submission");
            self.last_readback_audit = Some(pending.finish());
        }
        pixels
    }

    /// Counts glyphs added to the shared text atlas. Eviction and growth only
    /// happen when one is added, so an unchanged count means the glyphs a
    /// prepared frame points at are still where they were.
    fn text_epoch(&self) -> u64 {
        let statistics = self.engine.text_pipeline.statistics();
        statistics
            .glyphs_rasterized
            .wrapping_add(statistics.glyphs_without_pixels)
            .wrapping_add(statistics.growths)
    }

    /// Prepares only the custom primitives of the current layers.
    fn prepare_primitives(&mut self, viewport: &Viewport) {
        let scale_factor = viewport.scale_factor();
        // A damaged lane frame paints only its damage (enzUI-ICED #3666).
        let physical_bounds = self.clear_scissor.map_or_else(
            || Rectangle::<f32>::from(Rectangle::with_size(viewport.physical_size())),
            Rectangle::<f32>::from,
        );
        let mut primitive_storage = self
            .engine
            .primitive_storage
            .write()
            .expect("Write primitive storage");

        for layer in self.layers.iter() {
            if layer.primitives.is_empty()
                || physical_bounds
                    .intersection(&(layer.bounds * scale_factor))
                    .and_then(nudge::snap)
                    .is_none()
            {
                continue;
            }

            for instance in &layer.primitives {
                instance.primitive.prepare(
                    &mut primitive_storage,
                    &self.engine.device,
                    &self.engine.queue,
                    self.engine.format,
                    &instance.bounds,
                    viewport,
                );
            }
        }
    }

    /// Picks the window's outermost rounded group to mask in place: one that
    /// every painted layer starts with, drawn over the clear colour and at
    /// most one solid quad of its own shape (a window's background), with no
    /// material sampling what it painted. Its layers then draw straight to
    /// the frame, which [`alpha::State::mask`] turns into the composite's
    /// result, without a full-window target and composite pass every frame
    /// (enzUI-ICED #3442).
    fn take_window_mask(
        &mut self,
        clear_color: Option<Color>,
        scale_factor: f32,
    ) -> Option<(layer::AlphaGroup, alpha::MaskBackdrop)> {
        // Partial repainting sets unchanged layers aside. The remaining
        // panes cannot establish ownership of the whole retained canvas.
        if self.partial_frame {
            return None;
        }
        let clear = clear_color?;
        let [r, g, b, a] = graphics::color::pack(clear).components();
        let mut backdrop = alpha::MaskBackdrop {
            clear: [r * a, g * a, b * a, a],
            base: [0.0; 4],
            base_coverage_power: 1.0,
        };
        let (group, base) = {
            let mut painted = self
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| !layer.is_empty());
            let (first, first_layer) = painted.next()?;
            match first_layer.alpha_groups.first() {
                Some(group) => (*group, None),
                None => {
                    let (_, second) = painted.next()?;
                    (*second.alpha_groups.first()?, Some(first))
                }
            }
        };
        let (clip, radius) = group.clip?;
        if group.alpha < 1.0
            || group.scale != 1.0
            || group.rotation != 0.0
            || self
                .active_clip_sources
                .iter()
                .any(|(id, _)| *id == group.id)
        {
            return None;
        }
        if let Some(base) = base {
            let layer = &self.layers.iter().nth(base)?;
            let mut solids = layer.quads.solids();
            let solid = solids.next()?;
            let quad = solid.quad;
            let own_clip = clip_region_of(layer, scale_factor, None);
            let shape = [
                radius.top_left,
                radius.top_right,
                radius.bottom_right,
                radius.bottom_left,
            ];
            let exact = solids.next().is_none()
                && layer.quads.len() == 1
                && layer.text.is_empty()
                && layer.images.is_empty()
                && layer.triangles.is_empty()
                && layer.primitives.is_empty()
                && layer.materials.is_empty()
                && !layer.has_pending()
                && layer.clip_path.is_none()
                && quad.position == [clip.x, clip.y]
                && quad.size == [clip.width, clip.height]
                && quad.border_radius == shape
                && quad.border_width == 0.0
                && quad.shadow_color.components()[3] == 0.0
                && (own_clip == clip::ClipRegion::NONE
                    || layer.clip_radius.map(|own| (layer.bounds, own)) == Some((clip, radius)));
            if !exact {
                return None;
            }
            let [r, g, b, a] = solid.color.components();
            backdrop.base = [r * a, g * a, b * a, a];
            if own_clip != clip::ClipRegion::NONE {
                backdrop.base_coverage_power = 2.0;
            }
        }
        let owns_every_layer = self
            .layers
            .iter()
            .enumerate()
            .filter(|(index, layer)| !layer.is_empty() && Some(*index) != base)
            .all(|(_, layer)| {
                layer.alpha_groups.first().map(|first| first.id) == Some(group.id)
                    && layer.materials.is_empty()
                    && layer.clip_path.is_none()
            });
        if !owns_every_layer {
            return None;
        }
        for layer in self.layers.iter_mut() {
            layer.alpha_groups.retain(|active| active.id != group.id);
        }
        Some((group, backdrop))
    }

    /// Merges the layer stack for drawing, once per stack: merging a merged
    /// stack again may order its layers differently, so a stack drawn twice
    /// (a screenshot, a content-only frame, a frame's paint set aside and
    /// restored) must keep the order its first draw gave it.
    fn merge_layers(&mut self) {
        if self.merged != Some(self.layers_generation) {
            self.layers.merge();
            self.merged = Some(self.layers_generation);
        }
    }

    fn prepare(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        viewport: &Viewport,
        clear_color: Option<Color>,
    ) {
        let scale_factor = viewport.scale_factor();

        self.text_viewport
            .update(&self.engine.queue, viewport.physical_size());

        let physical_bounds =
            Rectangle::<f32>::from(Rectangle::with_size(viewport.physical_size()));

        self.merge_layers();

        self.window_mask = self.take_window_mask(clear_color, scale_factor);

        // Mesh batches are numbered in the order `render` draws them.
        let mut mesh_batch = 0;

        for layer in self.layers.iter() {
            let clip_region = clip_region_of(
                layer,
                scale_factor,
                self.window_mask.map(|(group, _)| group),
            );

            let clip_bounds = layer.bounds * scale_factor;

            let Some(region) = physical_bounds
                .intersection(&clip_bounds)
                .and_then(nudge::snap)
            else {
                continue;
            };

            if !layer.quads.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Quad);

                self.quad.prepare(
                    &self.engine.quad_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &layer.quads,
                    viewport.projection(),
                    scale_factor,
                    clip_region,
                );

                prepare_span.finish();
            }

            if !layer.triangles.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Triangle);

                self.triangle.prepare(
                    &self.engine.triangle_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &layer.triangles,
                    Transformation::scale(scale_factor),
                    viewport.physical_size(),
                    clip_region,
                    mesh_batch,
                    region,
                );
                mesh_batch += 1;

                prepare_span.finish();
            }

            if !layer.primitives.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Shader);

                let mut primitive_storage = self
                    .engine
                    .primitive_storage
                    .write()
                    .expect("Write primitive storage");

                for instance in &layer.primitives {
                    instance.primitive.prepare(
                        &mut primitive_storage,
                        &self.engine.device,
                        &self.engine.queue,
                        self.engine.format,
                        &instance.bounds,
                        viewport,
                    );
                }

                prepare_span.finish();
            }

            #[cfg(any(feature = "svg", feature = "image"))]
            if !layer.images.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Image);

                self.image.prepare(
                    &self.engine.image_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &mut self.image_cache.borrow_mut(),
                    &layer.images,
                    viewport.projection(),
                    scale_factor,
                    clip_region,
                );

                prepare_span.finish();
            }

            if !layer.text.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Text);

                self.text.prepare(
                    &self.engine.text_pipeline,
                    &self.engine.device,
                    &self.engine.queue,
                    &self.text_viewport,
                    encoder,
                    &layer.text,
                    layer.bounds,
                    Transformation::scale(scale_factor),
                );

                prepare_span.finish();
            }
        }
    }

    /// Renders the prepared frame, once per region of a partial frame, each
    /// within its own scissor (enzUI-ICED #3666). Every region opens and
    /// composites the same groups, so the composite uniforms the queue writes
    /// for one region hold the same values for the next.
    fn render_regions(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clear_color: Option<Color>,
        viewport: &Viewport,
    ) {
        if self.paint_regions.is_empty() {
            self.render(encoder, target, clear_color, viewport);
            return;
        }
        let prepared_within = self.clear_scissor;
        let regions = std::mem::take(&mut self.paint_regions);
        for region in &regions {
            self.clear_scissor = Some(*region);
            self.render(encoder, target, clear_color, viewport);
        }
        self.paint_regions = regions;
        self.clear_scissor = prepared_within;
    }

    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        clear_color: Option<Color>,
        viewport: &Viewport,
    ) {
        use std::mem::ManuallyDrop;

        let max_depth = self
            .layers
            .iter()
            .map(|layer| layer.alpha_groups.len())
            .max()
            .unwrap_or(0);
        self.alpha
            .begin_frame(&self.engine.device, viewport.physical_size(), max_depth);

        // enzUI-ICED #2950: a frame with a material a clip leaves visible
        // renders into a texture this renderer owns, because a swapchain view
        // cannot be sampled. Any other frame keeps the direct path, which
        // allocates, clears and blits nothing.
        let output = frame;
        let physical_size = viewport.physical_size();
        let physical_frame = Rectangle::new(
            core::Point::ORIGIN,
            Size::new(physical_size.width as f32, physical_size.height as f32),
        );
        let scene = self.material.begin_frame(
            &self.engine.device,
            encoder,
            physical_size,
            self.layers.iter().any(|layer| {
                crate::material::any_visible(
                    &layer.materials,
                    layer.bounds,
                    viewport.scale_factor(),
                    physical_frame,
                )
            }),
            self.direct_target.as_ref(),
        );
        let backdrop_available = scene.is_some() || self.material.is_direct();
        let frame = scene.as_ref().unwrap_or(frame);

        if let Some(color) = clear_color {
            self.clear.prepare(&mut self.staging_belt, encoder, color);
        }
        self.primary_render_passes = 1;
        self.pass_causes = [0; 3];
        let mut render_pass =
            ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iced_wgpu render pass"),
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
            }));

        if clear_color.is_some() {
            if let Some(scissor) = self.clear_scissor {
                render_pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
            }
            self.clear
                .render(&self.engine.clear_pipeline, &mut render_pass);
            if self.clear_scissor.is_some() {
                let size = viewport.physical_size();
                render_pass.set_scissor_rect(0, 0, size.width, size.height);
            }
        }
        let mut quad_layer = 0;
        let mut mesh_layer = 0;
        let mut mesh_batch = 0;
        let mut text_layer = 0;

        #[cfg(any(feature = "svg", feature = "image"))]
        let mut image_layer = 0;

        let scale_factor = viewport.scale_factor();
        // A damaged lane frame paints only its damage (enzUI-ICED #3666).
        let physical_bounds = self.clear_scissor.map_or_else(
            || Rectangle::<f32>::from(Rectangle::with_size(viewport.physical_size())),
            Rectangle::<f32>::from,
        );

        let scale = Transformation::scale(scale_factor);
        let mut active_alpha_groups: Vec<layer::AlphaGroup> = Vec::new();
        // Closed outermost clip groups still in the first target, composited
        // together later (enzUI-ICED #3406).
        let mut deferred: Vec<layer::AlphaGroup> = Vec::new();
        // Whether the first target holds paint of groups composited while it
        // stayed open, so it cannot be painted again without a clear.
        let mut stale = false;

        for layer in self.layers.iter().filter(|layer| !layer.is_empty()) {
            let common = active_alpha_groups
                .iter()
                .zip(&layer.alpha_groups)
                .take_while(|(active, next)| active.id == next.id)
                .count();

            // Layers share one pass until the target changes. Images and
            // in-pass primitives draw into it; meshes and primitives that need
            // the encoder end and reopen it themselves. Each pass is two
            // command buffers in wgpu-core (enzUI-ICED #3244).
            let change_target =
                common != active_alpha_groups.len() || common != layer.alpha_groups.len();

            // enzUI-ICED #3406: an outermost clip group closing straight into a
            // sibling clip group that overlaps no pending one keeps painting
            // the same target in the same pass. Both regions stay apart in it
            // and are composited later, in one pass, each through its own mask.
            let sibling = (change_target
                && !stale
                && common == 0
                && active_alpha_groups.len() == 1
                && layer.alpha_groups.len() == 1
                && layer.materials.is_empty())
            .then(|| (active_alpha_groups[0], layer.alpha_groups[0]))
            .filter(|(closing, opening)| {
                batchable(*closing)
                    && batchable(*opening)
                    && deferred
                        .iter()
                        .chain([closing])
                        .all(|pending| apart(*pending, *opening))
            });

            // Paint between pending groups must not land on one of them: it
            // would be covered when they are composited after it.
            let extent = layer.alpha_groups.is_empty().then(|| paint_extent(layer));
            let clear_of_pending =
                |deferred: &[layer::AlphaGroup], extra: Option<layer::AlphaGroup>| {
                    layer.materials.is_empty()
                        && extent.is_some_and(|extent| {
                            deferred
                                .iter()
                                .chain(extra.as_ref())
                                .all(|pending| clear_of(*pending, extent))
                        })
                };

            if !change_target
                && !deferred.is_empty()
                && layer.alpha_groups.is_empty()
                && !clear_of_pending(&deferred, None)
            {
                let _ = ManuallyDrop::into_inner(render_pass);
                self.alpha.composite_batch(
                    &self.engine.device,
                    &self.engine.queue,
                    encoder,
                    frame,
                    deferred
                        .drain(..)
                        .map(|group| (group.alpha, group_clip_region(group, scale_factor))),
                );
                render_pass =
                    ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
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
                    }));
                self.primary_render_passes += 1;
                self.pass_causes[1] += 1;
            }

            if let Some((closing, opening)) = sibling {
                deferred.push(closing);
                active_alpha_groups.clear();
                self.alpha.open_shared();
                active_alpha_groups.push(opening);
            } else if change_target {
                let _ = ManuallyDrop::into_inner(render_pass);
                // An outermost clip group closing into parent paint clear of
                // every pending group waits to be composited with them.
                if common == 0
                    && active_alpha_groups.len() == 1
                    && batchable(active_alpha_groups[0])
                    && clear_of_pending(&deferred, Some(active_alpha_groups[0]))
                {
                    deferred.extend(active_alpha_groups.pop());
                }
                // A new outermost clip group apart from every pending group
                // paints the same target without clearing it. Only pending
                // groups have painted it since it was last cleared.
                let reuse_first = common == 0
                    && !stale
                    && active_alpha_groups.is_empty()
                    && !deferred.is_empty()
                    && layer.materials.is_empty()
                    && layer.alpha_groups.first().is_some_and(|opening| {
                        batchable(*opening)
                            && deferred.iter().all(|pending| apart(*pending, *opening))
                    });
                if !deferred.is_empty() && !reuse_first && !layer.alpha_groups.is_empty() {
                    self.alpha.composite_batch(
                        &self.engine.device,
                        &self.engine.queue,
                        encoder,
                        frame,
                        deferred
                            .drain(..)
                            .map(|group| (group.alpha, group_clip_region(group, scale_factor))),
                    );
                }
                if !deferred.is_empty() && !active_alpha_groups.is_empty() {
                    stale = true;
                    self.alpha.composite_batch(
                        &self.engine.device,
                        &self.engine.queue,
                        encoder,
                        frame,
                        deferred
                            .drain(..)
                            .map(|group| (group.alpha, group_clip_region(group, scale_factor))),
                    );
                }
                while active_alpha_groups.len() > common {
                    let depth = active_alpha_groups.len() - 1;
                    let group = active_alpha_groups.pop().expect("an alpha group is active");

                    if depth == 0 {
                        self.alpha.composite_within(
                            &self.engine.device,
                            &self.engine.queue,
                            encoder,
                            depth,
                            frame,
                            group.alpha,
                            group.scale,
                            group.rotation,
                            group.scale_origin * scale,
                            group_clip_region(group, scale_factor),
                            group_scissor(group, scale_factor),
                        );
                    } else {
                        let destination = self.alpha.target(depth - 1);
                        self.alpha.composite_within(
                            &self.engine.device,
                            &self.engine.queue,
                            encoder,
                            depth,
                            &destination,
                            group.alpha,
                            group.scale,
                            group.rotation,
                            group.scale_origin * scale,
                            group_clip_region(group, scale_factor),
                            group_scissor(group, scale_factor),
                        );
                    }
                }

                let opened = &layer.alpha_groups[common..];
                for (index, &group) in opened.iter().enumerate() {
                    let depth = active_alpha_groups.len();
                    if reuse_first && index == 0 {
                        self.alpha.open_shared();
                    } else {
                        if depth == 0 {
                            stale = false;
                        }
                        // Intermediate targets can receive a child composite before their own paint.
                        self.alpha.open(encoder, depth, index + 1 < opened.len());
                    }
                    active_alpha_groups.push(group);
                }

                let alpha_target = active_alpha_groups
                    .len()
                    .checked_sub(1)
                    .map(|depth| self.alpha.target(depth));
                let target = alpha_target.as_ref().unwrap_or(frame);

                render_pass =
                    ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: if opened.is_empty() || reuse_first {
                                    wgpu::LoadOp::Load
                                } else {
                                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    }));

                self.primary_render_passes += 1;
                self.pass_causes[1] += 1;
            }
            let alpha_target = active_alpha_groups
                .len()
                .checked_sub(1)
                .map(|depth| self.alpha.target(depth));
            let target = alpha_target.as_ref().unwrap_or(frame);

            // enzUI-ICED #2950: resolve this layer's materials before any of
            // its own paint. Inside offscreen alpha groups part of the content
            // beneath is still in their targets, so each open group is
            // composited over the copied scene at its opacity first (#3408).
            if !layer.materials.is_empty() {
                let _ = ManuallyDrop::into_inner(render_pass);

                let device = &self.engine.device;
                let queue = &self.engine.queue;
                let alpha = &mut self.alpha;
                let groups = &active_alpha_groups;
                let mut composite_groups =
                    |encoder: &mut wgpu::CommandEncoder,
                     destination: &wgpu::TextureView,
                     scissor: [u32; 4]| {
                        let mut opacity = 1.0;
                        for (depth, group) in groups.iter().enumerate() {
                            opacity *= group.alpha;
                            alpha.composite_within(
                                device,
                                queue,
                                encoder,
                                depth,
                                destination,
                                opacity,
                                group.scale,
                                group.rotation,
                                group.scale_origin * scale,
                                group_clip_region(*group, scale_factor),
                                Some(scissor),
                            );
                        }
                    };
                let backdrop = match (backdrop_available, active_alpha_groups.is_empty()) {
                    (false, _) => material::Backdrop::Unavailable,
                    (true, true) => material::Backdrop::Scene,
                    (true, false) => material::Backdrop::Groups(&mut composite_groups),
                };

                self.material.boundary(
                    &self.engine.device,
                    &self.engine.queue,
                    encoder,
                    &layer.materials,
                    layer.bounds,
                    scale_factor,
                    backdrop,
                );

                self.primary_render_passes += 1;
                self.pass_causes[1] += 1;
                render_pass =
                    ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
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
                    }));
                self.material.draw_composites(&mut render_pass);
            }

            let Some((physical_bounds, scissor_rect)) = physical_bounds
                .intersection(&(layer.bounds * scale_factor))
                .and_then(|bounds| Some((bounds, nudge::snap(bounds)?)))
            else {
                // A layer outside this region of a partial frame was prepared
                // all the same: its slots are passed over, so the layers
                // after it draw their own (enzUI-ICED #3666).
                if self.clear_scissor.is_some()
                    && physical_frame
                        .intersection(&(layer.bounds * scale_factor))
                        .and_then(nudge::snap)
                        .is_some()
                {
                    quad_layer += usize::from(!layer.quads.is_empty());
                    if !layer.triangles.is_empty() {
                        mesh_layer += layer
                            .triangles
                            .iter()
                            .filter(|item| matches!(item, triangle::Item::Group { .. }))
                            .count();
                        mesh_batch += 1;
                    }
                    #[cfg(any(feature = "svg", feature = "image"))]
                    {
                        image_layer += usize::from(!layer.images.is_empty());
                    }
                    text_layer += layer
                        .text
                        .iter()
                        .filter(|item| matches!(item, text::Item::Group { .. }))
                        .count();
                }
                continue;
            };

            if !layer.quads.is_empty() {
                let render_span = debug::render(debug::Primitive::Quad);
                self.quad.render(
                    &self.engine.quad_pipeline,
                    quad_layer,
                    scissor_rect,
                    &layer.quads,
                    &mut render_pass,
                );
                render_span.finish();

                quad_layer += 1;
            }

            if !layer.triangles.is_empty() {
                let _ = ManuallyDrop::into_inner(render_pass);

                let render_span = debug::render(debug::Primitive::Triangle);
                mesh_layer += self.triangle.render(
                    &self.engine.triangle_pipeline,
                    encoder,
                    target,
                    mesh_layer,
                    &layer.triangles,
                    physical_bounds,
                    scale,
                    mesh_batch,
                );
                mesh_batch += 1;
                render_span.finish();

                self.primary_render_passes += 1;
                self.pass_causes[0] += 1;
                render_pass =
                    ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
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
                    }));
            }

            if !layer.primitives.is_empty() {
                let render_span = debug::render(debug::Primitive::Shader);

                let primitive_storage = self
                    .engine
                    .primitive_storage
                    .read()
                    .expect("Read primitive storage");

                let mut need_render = Vec::new();

                for instance in &layer.primitives {
                    let bounds = instance.bounds * scale;

                    if let Some(clip_bounds) = (instance.bounds * scale)
                        .intersection(&physical_bounds)
                        .and_then(nudge::snap)
                    {
                        render_pass.set_viewport(
                            bounds.x,
                            bounds.y,
                            bounds.width,
                            bounds.height,
                            0.0,
                            1.0,
                        );

                        render_pass.set_scissor_rect(
                            clip_bounds.x,
                            clip_bounds.y,
                            clip_bounds.width,
                            clip_bounds.height,
                        );

                        let drawn = instance
                            .primitive
                            .draw(&primitive_storage, &mut render_pass);

                        if !drawn {
                            need_render.push((instance, clip_bounds));
                        }
                    }
                }

                render_pass.set_viewport(
                    0.0,
                    0.0,
                    viewport.physical_width() as f32,
                    viewport.physical_height() as f32,
                    0.0,
                    1.0,
                );

                render_pass.set_scissor_rect(
                    0,
                    0,
                    viewport.physical_width(),
                    viewport.physical_height(),
                );

                if !need_render.is_empty() {
                    let _ = ManuallyDrop::into_inner(render_pass);

                    for (instance, clip_bounds) in need_render {
                        instance.primitive.render(
                            &primitive_storage,
                            encoder,
                            target,
                            &clip_bounds,
                        );
                    }

                    self.primary_render_passes += 1;
                    self.pass_causes[2] += 1;
                    render_pass =
                        ManuallyDrop::new(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("iced_wgpu render pass"),
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
                        }));
                }

                render_span.finish();
            }

            #[cfg(any(feature = "svg", feature = "image"))]
            if !layer.images.is_empty() {
                let render_span = debug::render(debug::Primitive::Image);
                self.image.render(
                    &self.engine.image_pipeline,
                    image_layer,
                    scissor_rect,
                    &mut render_pass,
                );
                render_span.finish();

                image_layer += 1;
            }

            if !layer.text.is_empty() {
                let render_span = debug::render(debug::Primitive::Text);
                text_layer += self.text.render(
                    &self.engine.text_pipeline,
                    &self.text_viewport,
                    text_layer,
                    &layer.text,
                    scissor_rect,
                    &mut render_pass,
                );
                render_span.finish();
            }
        }

        // enzUI-ICED #3442: the window's rounded group was drawn straight to
        // the frame; scale it by the group's coverage, in this pass when no
        // other group is still to be composited onto the frame.
        let window_mask = self.window_mask;
        let masked_in_pass =
            window_mask.is_some() && deferred.is_empty() && active_alpha_groups.is_empty();
        if let Some((group, backdrop)) = window_mask.filter(|_| masked_in_pass) {
            render_pass.set_viewport(
                0.0,
                0.0,
                viewport.physical_width() as f32,
                viewport.physical_height() as f32,
                0.0,
                1.0,
            );
            render_pass.set_scissor_rect(
                0,
                0,
                viewport.physical_width(),
                viewport.physical_height(),
            );
            self.alpha.mask(
                &self.engine.queue,
                &mut render_pass,
                group_clip_region(group, scale_factor),
                &backdrop,
            );
        }

        let _ = ManuallyDrop::into_inner(render_pass);

        // Otherwise the last composite onto the frame draws the mask in its
        // own pass: the depth-0 group's, or else the deferred batch's.
        let mut trailing = window_mask.filter(|_| !masked_in_pass);
        if active_alpha_groups.is_empty()
            && !deferred.is_empty()
            && let Some((group, backdrop)) = trailing.take()
        {
            self.alpha.mask_after_next_composite(
                &self.engine.queue,
                group_clip_region(group, scale_factor),
                &backdrop,
            );
        }

        if !deferred.is_empty() {
            self.alpha.composite_batch(
                &self.engine.device,
                &self.engine.queue,
                encoder,
                frame,
                deferred
                    .drain(..)
                    .map(|group| (group.alpha, group_clip_region(group, scale_factor))),
            );
        }

        while let Some(group) = active_alpha_groups.pop() {
            let depth = active_alpha_groups.len();

            if depth == 0 {
                if let Some((group, backdrop)) = trailing.take() {
                    self.alpha.mask_after_next_composite(
                        &self.engine.queue,
                        group_clip_region(group, scale_factor),
                        &backdrop,
                    );
                }
                self.alpha.composite_within(
                    &self.engine.device,
                    &self.engine.queue,
                    encoder,
                    depth,
                    frame,
                    group.alpha,
                    group.scale,
                    group.rotation,
                    group.scale_origin * scale,
                    group_clip_region(group, scale_factor),
                    group_scissor(group, scale_factor),
                );
            } else {
                let destination = self.alpha.target(depth - 1);
                self.alpha.composite_within(
                    &self.engine.device,
                    &self.engine.queue,
                    encoder,
                    depth,
                    &destination,
                    group.alpha,
                    group.scale,
                    group.rotation,
                    group.scale_origin * scale,
                    group_clip_region(group, scale_factor),
                    group_scissor(group, scale_factor),
                );
            }
        }

        if let Some((group, backdrop)) = trailing {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iced_wgpu window mask pass"),
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
            self.alpha.mask(
                &self.engine.queue,
                &mut render_pass,
                group_clip_region(group, scale_factor),
                &backdrop,
            );
            self.primary_render_passes += 1;
        }

        if scene.is_some() {
            self.material
                .end_frame(&self.engine.device, encoder, output);
        } else if self.material.is_direct() {
            self.material.end_direct_frame(&self.engine.device, encoder);
        }

        debug::layers_rendered(|| {
            self.layers
                .iter()
                .filter(|layer| {
                    !layer.is_empty()
                        && physical_bounds
                            .intersection(&(layer.bounds * scale_factor))
                            .is_some_and(|viewport| nudge::snap(viewport).is_some())
                })
                .count()
        });
    }

    /// Prepares currently mapped buffers for use in a submission.
    ///
    /// Usually, this method is only needed if you are calling [`Renderer::draw`] directly,
    /// instead of relying on [`Renderer::present`].
    ///
    /// You must call this method _before_ submitting the resulting [`wgpu::CommandEncoder`]
    /// of [`Renderer::draw`] to a [`wgpu::Queue`].
    pub fn finish(&mut self) {
        self.staging_belt.finish();
    }

    /// Recalls all of the closed buffers back to be reused.
    ///
    /// Usually, this method is only needed if you are calling [`Renderer::draw`] directly,
    /// instead of relying on [`Renderer::present`] to a [`wgpu::Queue`].
    ///
    /// You must call this method _after_ submitting the resulting [`wgpu::CommandEncoder`]
    /// of [`Renderer::draw`] to a [`wgpu::Queue`].
    pub fn recall(&mut self) {
        self.staging_belt.recall();
    }
}

impl core::Renderer for Renderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push_clip(bounds);
        self.tag_current_layer();
        self.push_scope(LayerScope::default());
    }

    fn start_layer_retained(&mut self, visible: Rectangle, retained: Rectangle) {
        let enclosing_depth = self.layer_scopes.last().map_or(0, |scope| scope.depth);
        self.layers.push_clip_retained(visible, retained);
        self.layers
            .current_mut()
            .0
            .set_enclosing_depth(enclosing_depth);
        self.tag_current_layer();
        self.push_scope(LayerScope::default());
    }

    fn start_layer_composite(
        &mut self,
        bounds: Rectangle,
        alpha: f32,
        scale: f32,
        rotation: f32,
        origin: Point,
    ) {
        // The origin is where the widget is laid out; the group composites in
        // window space, under whatever its ancestors translated or scaled.
        let origin = origin * self.layers.transformation();
        self.start_composite_layer(bounds, alpha, scale, rotation, origin);
    }

    fn end_layer_composite(&mut self) {
        <Self as core::Renderer>::end_layer(self);
    }

    fn start_layer_alpha(&mut self, bounds: Rectangle, alpha: f32) {
        self.layers.push_clip(bounds);

        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            1.0
        };

        // The opaque endpoint takes the historical direct path exactly: no
        // target is allocated, cleared, sampled, or composited.
        let group = (alpha < 1.0).then(|| {
            let group = layer::AlphaGroup {
                id: self.next_alpha_group,
                alpha,
                scale: 1.0,
                scale_origin: core::Point::ORIGIN,
                rotation: 0.0,
                reach: None,
                clip: None,
                retained_clip: None,
                depth: self.node_depth(),
            };
            self.next_alpha_group += 1;
            self.active_alpha_groups.push(group);
            group
        });

        self.tag_current_layer();
        self.push_scope(LayerScope {
            alpha: group,
            ..LayerScope::default()
        });
    }

    // enzUI-ICED #2950: the ordered material boundary. The region is recorded on
    // the layer it opens, so `render` composites it after everything already
    // drawn and before this layer's own contents -- which is exactly the
    // guarantee that a material never samples its own foreground.
    fn material_support(&self) -> core::material::Support {
        self.material.support()
    }

    fn material_luminance(&self, probe: u64) -> Option<f32> {
        self.material.luminance(probe)
    }

    fn material_luminance_with_frame(&self, probe: u64) -> Option<(f32, u64)> {
        self.material.luminance_with_frame(probe)
    }

    fn material_frame(&self) -> u64 {
        self.material.frame()
    }

    fn set_material_waker(&mut self, waker: core::shell::Waker) {
        self.material.set_waker(waker);
    }

    fn request_material_redraw(&self) {
        self.material.request_redraw();
    }

    fn start_layer_material(&mut self, bounds: Rectangle, material: core::material::Material) {
        self.layers.push_clip(bounds);
        self.tag_current_layer();
        self.push_scope(LayerScope::default());

        let (layer, transformation) = self.layers.current_mut();

        // The region keeps its full laid-out geometry; the layer's own clip is
        // applied as the composite's scissor at render time, so a partly
        // scrolled-out panel still bends like the whole panel.
        layer.materials.push(crate::material::Region {
            bounds: bounds * transformation,
            material,
        });
    }

    fn start_layer_rounded(&mut self, bounds: Rectangle, radius: core::border::Radius) {
        self.layers.push_clip_rounded(bounds, radius);
        self.begin_rounded_group();
    }

    fn start_layout_layer(
        &mut self,
        source: core::renderer::LayoutLayer,
        bounds: Rectangle,
        radius: Option<core::border::Radius>,
    ) -> bool {
        use core::renderer::{LayoutLayer, presentation::Error};
        let admission = (|| {
            let Some(recording) = self.recordings.last() else {
                return Ok(None);
            };
            let Some(program) = recording.presentation.as_ref() else {
                return Ok(None);
            };
            let (parent, transform) = self.layers.current_mut();
            if !parent
                .clip_path
                .as_ref()
                .is_some_and(|path| path.is_declared())
            {
                return Err(Error::OpaquePaint);
            }
            let node = match source {
                LayoutLayer::Clip(node)
                | LayoutLayer::BandContent(node)
                | LayoutLayer::TabStrip(node)
                | LayoutLayer::Material(node)
                | LayoutLayer::Cull(node)
                | LayoutLayer::Scroll { node, .. } => Some(node),
                LayoutLayer::Inherited => None,
            };
            let Some(node) = node else {
                if radius.is_some() {
                    return Err(Error::OpaquePaint);
                }
                return Ok(Some((None, None)));
            };
            // A recording may cover more than the program's allocation:
            // `request_docked_presentation` admits a capture contained in it,
            // and `Program::validate_scene` asks only for that containment.
            // Coverage a scene-clipped leaf needs exactly is enforced there.
            if !presentation_bounds(recording.bounds).contains(program.clip())
                || transform != owned_scroll::transform(program, &recording.scrolls, node.0)?
            {
                return Err(Error::ClipCoverage(node.0));
            }
            let expected = owned_clip::capture_source(program, &recording.scrolls, source)?;
            let observed = bounds * transform;
            let valid = match source {
                LayoutLayer::Clip(_) | LayoutLayer::BandContent(_) | LayoutLayer::TabStrip(_) => {
                    let recorded = program
                        .recorded_viewport(node.0)
                        .ok_or(Error::SceneIdentity(node.0))?;
                    let raw = Rectangle {
                        x: recorded.x,
                        y: recorded.y,
                        width: recorded.width,
                        height: recorded.height,
                    } * transform;
                    observed == expected
                        || (!program.scroll_ancestors(node.0).is_empty()
                            && presentation_bounds(observed)
                                .contains(presentation_bounds(expected))
                            && presentation_bounds(raw).contains(presentation_bounds(observed)))
                }
                LayoutLayer::Scroll { .. } => {
                    observed.intersection(&parent.bounds) == expected.intersection(&parent.bounds)
                }
                LayoutLayer::Material(_) | LayoutLayer::Cull(_) => {
                    radius.is_none()
                        && observed.intersection(&parent.bounds)
                            == expected.intersection(&parent.bounds)
                }
                LayoutLayer::Inherited => false,
            };
            if !valid {
                return Err(Error::ClipCoverage(node.0));
            }
            Ok(Some((
                Some(expected),
                matches!(
                    source,
                    LayoutLayer::Clip(_)
                        | LayoutLayer::BandContent(_)
                        | LayoutLayer::TabStrip(_)
                        | LayoutLayer::Material(_)
                        | LayoutLayer::Cull(_)
                )
                .then_some(node),
            )))
        })();
        let mut marked = false;
        let (owned, viewport_bounds) = match admission {
            Ok(Some((bounds, node))) => {
                if let Some(node) = node {
                    self.mark_recording(node);
                    marked = true;
                }
                (true, bounds)
            }
            Ok(None) => (false, None),
            Err(error) => {
                self.refuse_layout_layer(error, true);
                (false, None)
            }
        };
        if let Some(bounds) = viewport_bounds {
            if let Some(radius) = radius {
                self.layers.push_clip_rounded_viewport(bounds, radius);
                self.begin_rounded_group();
            } else {
                self.layers.push_clip_viewport(bounds);
                self.tag_current_layer();
                self.push_scope(LayerScope::default());
            }
        } else if let Some(radius) = radius {
            self.start_layer_rounded(bounds, radius);
        } else {
            self.start_layer(bounds);
        }
        if owned {
            self.layer_scopes
                .last_mut()
                .expect("a layout layer was opened")
                .visibility_only =
                matches!(source, LayoutLayer::Material(_) | LayoutLayer::Cull(_));
            let (layer, _) = self.layers.current_mut();
            let bound = layer
                .clip_path
                .as_ref()
                .ok_or(Error::OpaquePaint)
                .and_then(|path| graphics::layer::ClipPath::bind(path, source));
            match bound {
                Ok(path) => {
                    layer.clip_path = Some(path.clone());
                    if let Some(group) = self.layer_scopes.last().and_then(|scope| scope.alpha) {
                        if let Some((_, source)) = self
                            .active_clip_sources
                            .iter_mut()
                            .find(|(id, _)| *id == group.id)
                        {
                            *source = path;
                        }
                        self.tag_current_layer();
                    }
                }
                Err(error) => self.refuse_layout_layer(error, false),
            }
        }
        marked
    }

    fn layout_clip_capture(
        &self,
        node: core::renderer::NodeId,
    ) -> Option<core::renderer::ClipCapture> {
        let recording = self.recordings.last()?;
        let program = recording.presentation.as_ref()?;
        let _ = program.clip_node(node.0)?;
        let transform = owned_scroll::transform(program, &recording.scrolls, node.0).ok()?;
        let mask = owned_clip::capture_source(
            program,
            &recording.scrolls,
            core::renderer::LayoutLayer::Clip(node),
        )
        .ok()?;
        let source = program.recorded_viewport(node.0)?.union(program.clip());
        Some(core::renderer::ClipCapture {
            mask: mask * transform.inverse(),
            viewport: Rectangle {
                x: source.x,
                y: source.y,
                width: source.width,
                height: source.height,
            },
        })
    }

    fn layout_reveal_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        let program = self.recordings.last()?.presentation.as_ref()?;
        let bounds = program
            .clip_node(node.0)?
            .reveal?
            .source
            .union(program.clip());
        Some(Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        })
    }

    fn start_layout_reveal(&mut self, node: core::renderer::NodeId, progress: f32) -> bool {
        use core::renderer::{LayoutLayer, presentation::Error};
        let admission = (|| {
            let Some(recording) = self.recordings.last() else {
                return Ok(None);
            };
            let Some(program) = recording.presentation.as_ref() else {
                return Ok(None);
            };
            let declaration = program
                .clip_node(node.0)
                .ok_or(Error::SceneIdentity(node.0))?;
            let reveal = declaration.reveal.ok_or(Error::SceneIdentity(node.0))?;
            if !progress.is_finite()
                || !(0.0..=1.0).contains(&progress)
                || (reveal.driver.is_none() && progress != reveal.captured)
            {
                return Err(Error::NonFinite);
            }
            let transform = owned_scroll::transform(program, &recording.scrolls, node.0)?;
            let retained =
                owned_clip::capture_source(program, &recording.scrolls, LayoutLayer::Clip(node))?;
            let bounds = declaration.recorded;
            let visible = reveal.rule.visible(
                Rectangle {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                },
                progress,
            ) * transform;
            Ok(Some((retained * transform.inverse(), visible)))
        })();
        let (retained, visible) = match admission {
            Ok(Some(value)) => value,
            Ok(None) => return false,
            Err(error) => {
                self.refuse_layout_layer(error, true);
                return false;
            }
        };
        if !self.start_layout_layer(LayoutLayer::Clip(node), retained, None) {
            self.end_layer();
            return false;
        }
        self.layer_scopes
            .last_mut()
            .expect("a reveal layer is open")
            .reveal = Some((self.layers.active_len() - 1, visible));
        true
    }

    fn end_layer(&mut self) {
        let end = self.layers.active_len();
        self.layers.pop_clip();

        let scope = self.layer_scopes.pop().expect("a layer scope is active");
        if let Some((start, clip)) = scope.reveal {
            self.layers.clip_display_range(start..end, clip);
        }
        if let Some(group) = scope.alpha {
            let popped = self
                .active_alpha_groups
                .pop()
                .expect("an alpha group is active");
            debug_assert_eq!(popped.id, group.id);
            self.active_clip_sources.retain(|(id, _)| *id != group.id);

            self.alpha_segment = self.alpha_segment.wrapping_add(1);

            if let Some(from) = scope.rounded_from {
                self.elide_rounded_group(group, from..end);
            }
        }
        if scope.visibility_only {
            self.layers.split_current();
        } else {
            self.layers.split_clip_continuation();
        }
        self.tag_current_layer();
    }

    fn record_presentation(
        &mut self,
        program: std::sync::Arc<core::renderer::presentation::Program>,
    ) -> Result<(), core::renderer::presentation::Error> {
        use core::renderer::presentation::Error;
        if self.recordings.len() != 1 {
            self.presentation_refusal = Some(Error::RecordingState);
            return Err(Error::RecordingState);
        }
        let Some(recording) = self.recordings.last_mut() else {
            self.presentation_refusal = Some(Error::RecordingState);
            return Err(Error::RecordingState);
        };
        if recording.presentation.is_some() {
            self.presentation_refusal = Some(Error::RecordingState);
            return Err(Error::RecordingState);
        }
        for leaf in program.leaves() {
            if recording.path.iter().any(|node| node.0 == leaf)
                || recording
                    .segments
                    .iter()
                    .any(|segment| segment.path.iter().any(|node| node.0 == leaf))
            {
                self.presentation_refusal = Some(Error::SceneIdentity(leaf));
                return Err(Error::SceneIdentity(leaf));
            }
        }
        let (layer, _) = self.layers.current_mut();
        if layer.bounds != recording.bounds {
            recording.unretainable = true;
            self.presentation_refusal = Some(Error::ClipCoverage(0));
            return Err(Error::ClipCoverage(0));
        }
        layer.clip_path = Some(graphics::layer::ClipPath::scene(
            layer.bounds,
            layer.clip_radius,
        ));
        recording.presentation = Some(program);
        self.presentation_refusal = None;
        Ok(())
    }

    fn request_docked_presentation(
        &mut self,
        capture: core::renderer::docked::Capture,
    ) -> Result<(), core::renderer::presentation::Error> {
        use core::renderer::presentation::Error;
        if self.recordings.is_empty() {
            return Err(Error::RecordingState);
        }
        if self.recordings.len() != 1 {
            for recording in &mut self.recordings {
                recording.unretainable = true;
            }
            self.presentation_refusal = Some(Error::RecordingState);
            return Err(Error::RecordingState);
        }
        let recording = self.recordings.last_mut().ok_or(Error::RecordingState)?;
        if !capture.is_valid()
            || recording.docked.is_some()
            || !presentation_bounds(recording.bounds).contains(presentation_bounds(capture.bounds))
        {
            recording.unretainable = true;
            self.presentation_refusal = Some(Error::RecordingState);
            return Err(Error::RecordingState);
        }
        let complete_drivers = capture
            .layout
            .as_ref()
            .is_ok_and(|program| capture.covers_program(program));
        let layout = capture.layout.clone();
        let driver = capture.animation.node.0;
        let bounds = capture.bounds;
        recording.docked = Some(capture);
        let program = match layout {
            Ok(program) => program,
            Err(error) => {
                recording.unretainable = true;
                self.presentation_refusal = Some(error);
                return Err(error);
            }
        };
        if !complete_drivers
            || !program.drives_responsive(driver)
            || program.clip() != presentation_bounds(bounds)
        {
            self.presentation_refusal = Some(Error::DockedOwnershipRequired);
            return Err(Error::DockedOwnershipRequired);
        }
        self.record_presentation(program)
    }

    fn begin_scroll_view(
        &mut self,
        node: core::renderer::NodeId,
        bounds: Rectangle,
        content: Rectangle,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        paint: core::renderer::presentation::scroll_paint::Paint,
    ) -> Option<Rectangle> {
        use core::renderer::presentation::{Error, MAX_ITEMS};
        let program = self
            .recordings
            .last()
            .and_then(|recording| recording.presentation.as_ref())
            .cloned()?;
        let (_, declaration) = program.scroll_nodes().find(|(id, _)| *id == node.0)?;
        let captured = owned_scroll::Captured {
            bounds,
            content,
            config,
            input,
            paint,
            chrome: declaration.chrome,
        };
        if self.recordings.len() != 1
            || self.recordings[0].scrolls.contains_key(&node.0)
            || self.recordings[0].scrolls.len() >= MAX_ITEMS
            || declaration.recorded != presentation_bounds(bounds)
            || declaration.recorded_content != presentation_bounds(content)
            || declaration.config != config
            || !captured.valid()
            || !self.scroll_transform_matches(node.0)
        {
            self.reject_layout_paint(node);
            self.presentation_refusal = Some(Error::OpaquePaint);
            return None;
        }
        let clip = program.clip();
        let x = content.x.min(clip.x);
        let y = content.y.min(clip.y);
        let capture = Rectangle {
            x,
            y,
            width: (content.x + content.width).max(clip.x + clip.width) - x,
            height: (content.y + content.height).max(clip.y + clip.height) - y,
        };
        let _ = self.recordings[0].scrolls.insert(node.0, captured);
        self.mark_recording(node);
        Some(capture)
    }
    fn draw_scroll_chrome(
        &mut self,
        node: Option<core::renderer::NodeId>,
        bounds: Rectangle,
        content: Rectangle,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        paint: core::renderer::presentation::scroll_paint::Paint,
    ) {
        let chrome = node.and_then(|node| {
            self.recordings
                .last()?
                .scrolls
                .get(&node.0)
                .map(|source| source.chrome)
        });
        if let Some(chrome) = chrome {
            self.mark_recording(core::renderer::NodeId(chrome));
        }
        let quads = paint.chrome(config, input, bounds, content);
        if chrome.is_some() && quads.is_empty() {
            self.fill_quad(
                core::renderer::Quad {
                    bounds: Rectangle::new(bounds.position(), Size::ZERO),
                    ..Default::default()
                },
                Color::TRANSPARENT,
            );
        }
        for (quad, fill) in quads {
            self.fill_quad(quad, fill);
        }
        if chrome.is_some() {
            self.mark_recording(core::renderer::NodeId::STATIC);
        }
    }
    fn end_scroll_view(&mut self, _node: core::renderer::NodeId) {
        self.mark_recording(core::renderer::NodeId::STATIC);
    }

    fn layout_culling_capture(
        &self,
        scope: core::renderer::NodeId,
        observed: Rectangle,
    ) -> Option<core::renderer::CullCapture> {
        let recording = self.recordings.last()?;
        let program = recording.presentation.as_ref()?;
        let cull = program.cull_source(scope.0)?;
        let transform = owned_scroll::transform(program, &recording.scrolls, cull.owner).ok()?;
        let mask = owned_clip::capture_source(
            program,
            &recording.scrolls,
            core::renderer::LayoutLayer::Cull(scope),
        )
        .ok()?;
        let viewport =
            owned_clip::capture_viewport(program, &recording.scrolls, cull.owner).ok()?;
        let incoming = viewport * transform.inverse();
        let viewport = if cull.clip {
            let bounds = Rectangle {
                x: cull.recorded.x,
                y: cull.recorded.y,
                width: cull.recorded.width,
                height: cull.recorded.height,
            } * transform;
            viewport.intersection(&bounds).unwrap_or(Rectangle {
                x: viewport.x.max(bounds.x),
                y: viewport.y.max(bounds.y),
                width: 0.0,
                height: 0.0,
            })
        } else {
            viewport
        };
        let viewport = viewport * transform.inverse();
        if observed != incoming
            && !(recording.path.contains(&scope) && observed == viewport)
            && !program.allows_source_capture_viewport(cull.owner, presentation_bounds(observed))
        {
            return None;
        }
        Some(core::renderer::CullCapture {
            mask: mask * transform.inverse(),
            viewport,
        })
    }

    fn layout_culling_active(&self, scope: core::renderer::NodeId) -> bool {
        self.recordings
            .last()
            .is_some_and(|recording| recording.path.contains(&scope))
    }

    fn retains_layout_leaf(&self, node: core::renderer::NodeId) -> bool {
        self.recordings.len() == 1
            && self.recordings[0]
                .presentation
                .as_ref()
                .is_some_and(|program| program.owns_leaf(node.0))
    }

    fn layout_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        let program = self.recordings.last()?.presentation.as_ref()?;
        program.owns_node(node.0).then(|| {
            let clip = program.clip();
            Rectangle {
                x: clip.x,
                y: clip.y,
                width: clip.width,
                height: clip.height,
            }
        })
    }

    fn tab_strip_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        let program = self.recordings.last()?.presentation.as_ref()?;
        let bounds = program.tab_strip_node(node.0)?.source.union(program.clip());
        Some(Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        })
    }

    fn editor_presentation_height(&self, node: core::renderer::NodeId) -> Option<f32> {
        self.recordings.iter().rev().find_map(|recording| {
            recording.presentation.as_ref().and_then(|program| {
                program
                    .editor_nodes()
                    .any(|(id, _)| id == node.0)
                    .then_some(program.clip().height)
            })
        })
    }

    fn fill_layout_editor_scrollbars(
        &mut self,
        owner: core::renderer::NodeId,
        editor: core::renderer::NodeId,
        source: Option<Arc<()>>,
        bounds: Rectangle,
        metrics: core::text::editor::ScrollMetrics,
        paint: core::text::editor::scrollbar::Paint,
    ) {
        let recording = self.recordings.last().filter(|r| r.presentation.is_some());
        if recording.is_some_and(|r| {
            !source.as_ref().is_some_and(|identity| {
                r.editors.get(&editor.0).is_some_and(|captured| {
                    captured.snapshot.matches_scroll_source(identity, metrics)
                        && paint.projection.is_none_or(|projection| {
                            projection.materialized_lines() == captured.snapshot.source_line_count()
                                && captured.paint.gutter.is_none_or(|gutter| {
                                    gutter.line_offset == projection.first_line()
                                })
                                && projection.apply(metrics).is_some()
                                && projection.covers(metrics)
                        })
                })
            })
        }) {
            self.reject_layout_paint(owner);
            for (quad, color) in paint.quads(bounds, metrics).into_iter().flatten() {
                self.fill_quad(quad, color);
            }
            return;
        }
        let mut captured = owned_quad::OwnedQuad::new(
            core::renderer::Quad {
                bounds,
                ..Default::default()
            },
            Color::TRANSPARENT.into(),
        );
        captured.editor_scrollbars = Some(owned_quad::EditorScrollbars {
            editor: editor.0,
            metrics,
            paint,
        });
        self.fill_owned_quad(owner, captured);
    }

    fn fill_layout_quad(
        &mut self,
        node: core::renderer::NodeId,
        quad: core::renderer::Quad,
        background: Background,
    ) {
        self.fill_owned_quad(node, owned_quad::OwnedQuad::new(quad, background));
    }

    fn fill_layout_blink(
        &mut self,
        node: core::renderer::NodeId,
        quad: core::renderer::Quad,
        background: Background,
        blink: core::layout::input::Blink,
        at: core::time::Instant,
    ) {
        let mut owned = owned_quad::OwnedQuad::new(quad, background);
        owned.blink = Some((blink, at));
        self.fill_owned_quad(node, owned);
    }

    fn fill_layout_rule(
        &mut self,
        node: core::renderer::NodeId,
        allocation: Rectangle,
        quad: core::renderer::Quad,
        background: Background,
        paint: core::layout::rule::Paint,
    ) {
        self.fill_owned_quad(
            node,
            owned_quad::OwnedQuad {
                allocation,
                quad,
                background,
                rule: Some(paint),
                editor_scrollbars: None,
                blink: None,
                #[cfg(feature = "geometry")]
                border: None,
                hint_factor: self.hint_factor().unwrap_or(1.0),
            },
        );
    }

    fn reject_layout_paint(&mut self, node: core::renderer::NodeId) {
        for recording in &mut self.recordings {
            if recording
                .presentation
                .as_ref()
                .is_some_and(|program| program.owns_node(node.0))
            {
                recording.unretainable = true;
                self.presentation_refusal = Some(core::renderer::presentation::Error::OpaquePaint);
            }
        }
    }

    fn reconcile_presented_scroll(
        &self,
        node: core::renderer::NodeId,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        viewport: Size,
        content: Size,
    ) -> Option<bool> {
        self.presentation_state
            .presented_input()?
            .native_payload::<native_input::Frame>()?
            .scrolls
            .get(&node.0)?
            .reconcile(config, input, viewport, content)
    }

    fn install_presented_geometry(
        &mut self,
        recording: &core::renderer::Recording,
        sample: std::sync::Arc<core::renderer::presentation::Sample>,
    ) -> bool {
        self.presentation_state.set_presented_input(None);
        let Some(scene) = recording.payload::<Scene>() else {
            return false;
        };
        let Some(program) = recording.presentation() else {
            return false;
        };
        if !sample.geometry_reconcilable()
            || !sample.same_source(program, Some(&scene.layout_revision))
        {
            return false;
        }
        self.presentation_state.set_presented_input(Some(sample));
        true
    }

    fn install_presented_layout(
        &mut self,
        recording: &core::renderer::Recording,
        sample: std::sync::Arc<core::renderer::presentation::Sample>,
    ) -> bool {
        self.presentation_state.set_presented_input(None);
        let Some(scene) = recording.payload::<Scene>() else {
            return false;
        };
        let Some(program) = recording.presentation() else {
            return false;
        };
        if !sample.same_source(program, Some(&scene.layout_revision)) {
            return false;
        }
        if !sample.layout_input_only() {
            if !sample.geometry_reconcilable() {
                return false;
            }
            let Some(frame) = sample.native_payload::<native_input::Frame>() else {
                return false;
            };
            if !frame.matches(scene, program) {
                return false;
            }
        }
        self.presentation_state.set_presented_input(Some(sample));
        true
    }

    fn presented_layout(&self) -> Option<&std::sync::Arc<core::renderer::presentation::Sample>> {
        self.presentation_state.presented_input()
    }

    fn clear_presented_layout(&mut self) {
        self.presentation_state.set_presented_input(None);
    }

    fn presentation_refusal(&self) -> Option<core::renderer::presentation::Error> {
        self.presentation_refusal
    }

    fn start_recording(&mut self, bounds: Rectangle) -> bool {
        let recorder = self.recordings.first().map_or_else(
            || self.docked_recorder.clone(),
            |recording| recording.recorder.clone(),
        );
        self.start_recording_from(bounds, recorder)
    }

    fn start_staged_recording(&mut self, bounds: Rectangle) -> bool {
        if !self.recordings.is_empty() {
            return false;
        }
        self.start_recording_from(bounds, self.docked_recorder.stage())
    }

    fn start_layout_recording(&mut self, node: core::renderer::NodeId) -> bool {
        let active = self
            .recordings
            .last()
            .and_then(|recording| recording.presentation.as_ref())
            .is_some_and(|program| program.owns_leaf(node.0));
        if active {
            if !self.scroll_transform_matches(node.0) {
                self.reject_layout_paint(node);
            }
            self.mark_recording(node);
        }
        active
    }

    fn mark_recording_at(&mut self, node: core::renderer::NodeId, baseline: core::Vector) {
        self.mark_recording(node);
        if node == core::renderer::NodeId::STATIC {
            return;
        }
        if let Some(recording) = self.recordings.last_mut() {
            recording.baselines.push((node.0, baseline));
        }
    }

    fn mark_recording(&mut self, node: core::renderer::NodeId) {
        // `mark_recording` is called by widgets during ordinary drawing too,
        // where its renderer contract is a no-op. Keep the all-layer flush
        // strictly behind this check: walking every active layer outside a
        // recording would add per-frame work to every Motion draw (#2676).
        if self.recordings.is_empty() {
            return;
        }

        // Settle anything buffered before closing the run, or the boundary
        // between a static sibling and a moving node would land in the wrong
        // segment and the moving node would drag its neighbour along. Flush
        // EVERY active layer: a preceding text sibling can still be pending on
        // its parent while the moving node has already opened a nested clip.
        // Capturing that pending text is not enough because replay appends the
        // captured layers without making any of them current (#2676).
        self.layers.flush_active();

        let end = self.layers.active_len();
        let depth = self.active_alpha_groups.len();
        let transformation = self.layers.transformation();
        let recording = self
            .recordings
            .last_mut()
            .expect("recording presence was checked before flushing");
        if node != core::renderer::NodeId::STATIC {
            recording.origins.push((node.0, transformation));
        }

        let layers = self.layers.capture(recording.start..end);

        if !layers.is_empty() {
            // Retention is what makes the scene outlive the frame: a captured
            // layer still holding a weak paragraph reference would paint a
            // blank strip once the recording widget's paragraph went away
            // (enzICED #2219). `retain` is fail-closed, so a subtree that
            // cannot be owned poisons the recording instead of shipping paint
            // that can vanish.
            match retain_layers(layers) {
                Ok(layers) => recording.segments.push(Segment {
                    path: recording.path.clone(),
                    base: recording.base,
                    clip: recording.clip,
                    clip_depth: recording.clip_depth,
                    layers,
                }),
                Err(error) => {
                    recording.unretainable = true;
                    self.presentation_refusal = Some(error);
                }
            }
        }

        // `STATIC` means "leave the node I am in", which for a nested effect is
        // a return to its PARENT, not a collapse to static content. Without the
        // stack, an inner effect would drop everything its parent painted after
        // it into unaddressable static content (#2219).
        if node == core::renderer::NodeId::STATIC {
            let _ = recording.path.pop();
            recording.base = recording.bases.pop().unwrap_or(0);
        } else {
            recording.bases.push(recording.base);
            recording.path.push(node);
            recording.base = depth;
        }

        recording.start = end;

        // Later primitives must not merge back into the layer that just closed
        // a segment, or they would be captured twice.
        self.layers.split_current();
        self.layers.mark_current_fixed_replay_clip();
        self.tag_current_layer();

        // The clip the new segment opens under: `split_current` keeps the clip
        // in effect, which for a marked node is the scope its PARENT opened
        // (enzICED #2219 review finding 3).
        let current = self.layers.active_len().saturating_sub(1);
        let clip = self
            .layers
            .clip_bounds_at(current)
            .unwrap_or(Rectangle::INFINITE);

        let clip_depth = self.layer_scopes.last().map_or(0, |scope| scope.depth);
        if let Some(recording) = self.recordings.last_mut() {
            recording.clip = clip;
            recording.clip_depth = clip_depth.min(recording.path.len());
            if recording.presentation.is_some() && node != core::renderer::NodeId::STATIC {
                if recording.node_clips.len() < core::renderer::presentation::MAX_ITEMS {
                    recording.node_clips.push((node.0, clip));
                } else {
                    recording.unretainable = true;
                    self.presentation_refusal = Some(core::renderer::presentation::Error::Budget);
                }
            }
        }
    }

    fn finish_recording(&mut self) -> Option<core::renderer::Recording> {
        // Settle pending meshes/text into the current layer before capturing,
        // or a retained node would miss whatever was still buffered.
        self.layers.flush();

        let end = self.layers.active_len();
        let mut recording = self.recordings.pop()?;
        let tail = self.layers.capture(recording.start..end);

        if !tail.is_empty() {
            match retain_layers(tail) {
                Ok(layers) => recording.segments.push(Segment {
                    path: recording.path.clone(),
                    base: recording.base,
                    clip: recording.clip,
                    clip_depth: recording.clip_depth,
                    layers,
                }),
                Err(error) => {
                    recording.unretainable = true;
                    self.presentation_refusal = Some(error);
                }
            }
        }

        <Self as core::Renderer>::end_layer(self);

        if recording.unretainable {
            // An explicit, measured miss rather than a scene that can paint a
            // hole: the caller redraws this subtree every frame (#2219).
            return None;
        }

        if recording.segments.is_empty() {
            return None;
        }
        let mut program_nodes = Vec::new();
        if recording.presentation.is_some() {
            program_nodes.extend(
                recording
                    .segments
                    .iter()
                    .flat_map(|segment| segment.path.iter().map(|node| node.0)),
            );
            program_nodes.sort_unstable();
            program_nodes.dedup();
        }
        recording.node_clips.sort_unstable_by_key(|(node, _)| *node);
        recording.baselines.sort_by_key(|(node, _)| *node);
        recording.baselines.dedup_by_key(|(node, _)| *node);
        recording.origins.sort_by_key(|(node, _)| *node);
        recording.origins.dedup_by_key(|(node, _)| *node);
        // `request_docked_presentation` stores the capture before it decides, so
        // this flag is a request rather than ownership. It refuses a capture
        // outside the recording and poisons that recording, which is why
        // containment is not re-tested here.
        let docked_tenure = if let Some(capture) = &recording.docked {
            let owned = recording.presentation.as_ref().is_some_and(|program| {
                capture.covers_program(program)
                    && program.drives_responsive(capture.animation.node.0)
                    && program.clip() == presentation_bounds(capture.bounds)
            });
            if !owned {
                self.presentation_refusal =
                    Some(core::renderer::presentation::Error::DockedOwnershipRequired);
                return None;
            }
            Some(recording.recorder.tenure())
        } else {
            None
        };
        #[cfg(any(feature = "image", feature = "svg"))]
        let media_scale = self.scale.map_or(1.0, core::renderer::Scale::total);
        let media_assets = match owned_media::Assets::capture(
            &mut self.image_cache.borrow_mut(),
            &mut recording.segments,
            media_scale,
        ) {
            Ok(assets) => assets,
            Err(error) => {
                self.presentation_refusal = Some(error);
                return None;
            }
        };
        #[cfg(not(any(feature = "image", feature = "svg")))]
        let media_assets = owned_media::Assets::default();
        let fonts = match self
            .text
            .capture_fonts(&mut recording.segments, &recording.editors)
        {
            Ok(fonts) => fonts,
            Err(error) => {
                self.presentation_refusal = Some(error);
                return None;
            }
        };
        let mut scene = Scene {
            fonts,
            layout_revision: std::sync::Arc::new(()),
            segments: recording.segments,
            bounds: recording.bounds,
            program_nodes,
            node_clips: recording.node_clips,
            baselines: recording.baselines,
            origins: recording.origins,
            owned_quads: recording.owned_quads,
            editors: recording.editors,
            scrolls: recording.scrolls,
            media: recording.media,
            media_assets,
        };
        if let Some(program) = recording.presentation {
            if let Err(error) = owned_clip::validate(&scene, &program)
                .and_then(|()| owned_scroll::normalize(&mut scene, &program))
                .and_then(|()| scene.validate_presentation(&program))
            {
                self.presentation_refusal = Some(error);
                return None;
            }
            let recording = core::renderer::Recording::new(scene).with_presentation(program);
            Some(match docked_tenure {
                Some(tenure) => recording.with_docked_tenure(tenure),
                None => recording,
            })
        } else {
            Some(core::renderer::Recording::new(scene))
        }
    }

    fn replay(&mut self, recording: &core::renderer::Recording, translation: core::Vector) -> bool {
        self.presentation_sample_valid = false;
        if recording.presentation().is_some() {
            self.presentation_refusal =
                Some(core::renderer::presentation::Error::TrackReplayRequired);
            return false;
        }
        let Some(scene) = recording.payload::<Scene>() else {
            // A recording from another backend, or from a renderer that has
            // since been replaced. Report the miss; the caller redraws.
            return false;
        };

        if let Err(error) = self.prepare_scene_fonts(scene) {
            self.presentation_refusal = Some(error);
            return false;
        }
        let segments = scene.segments.clone();
        // enzUI-ICED #3083: see `ReplayAffine::snap_translation`.
        let factor = self.hint_factor().unwrap_or(1.0);
        let translation = if factor.is_finite() && factor > 0.0 {
            snap_vector(translation, factor)
        } else {
            translation
        };

        let mut clip_reissued = Vec::new();
        for segment in segments {
            self.replay_segment(
                segment.layers,
                segment.base,
                segment.clip,
                segment.clip_depth,
                &[ReplayAffine::translation(translation)],
                translation,
                1.0,
                1.0,
                0.0,
                core::Point::ORIGIN,
                None,
                &mut clip_reissued,
            );
        }

        true
    }

    fn reuse_frame(&mut self) -> bool {
        self.reuse_next = self.prepared.is_some();
        self.reuse_next
    }

    fn replay_tracks(
        &mut self,
        recording: &core::renderer::Recording,
        tracks: &[core::renderer::Track],
    ) -> bool {
        self.replay_tracks_at(recording, tracks, core::time::Instant::now())
    }

    fn replay_tracks_at(
        &mut self,
        recording: &core::renderer::Recording,
        tracks: &[core::renderer::Track],
        at: core::time::Instant,
    ) -> bool {
        self.presentation_sample_valid = false;
        self.presentation_refusal = None;
        let Some(scene) = recording.payload::<Scene>() else {
            return false;
        };
        // Anything laid out since this capture was admitted owns geometry the
        // scene does not describe, so the frame is refused and the caller draws.
        if recording
            .docked_tenure()
            .is_some_and(|tenure| !tenure.is_current())
        {
            self.presentation_refusal =
                Some(core::renderer::presentation::Error::DockedOwnershipRequired);
            self.presentation_recording = None;
            self.presentation_state.clear();
            self.text.clear_scroll();
            return false;
        }
        if let Err(error) = self.prepare_scene_fonts(scene) {
            self.presentation_refusal = Some(error);
            return false;
        }
        let mut merged = std::mem::take(&mut self.presentation_tracks);
        let evaluated = match self.sample_presentation(recording, scene, tracks, &mut merged) {
            Ok(evaluated) => evaluated,
            Err(error) => {
                self.presentation_refusal = Some(error);
                self.presentation_tracks = merged;
                self.presentation_recording = None;
                self.presentation_state.clear();
                self.text.clear_scroll();
                return false;
            }
        };
        // The same scene again: only what moved is replayed (enzUI-ICED
        // #3668).
        if !evaluated && !self.replaying_full && self.replay_changed(recording, scene, tracks) {
            self.partial_replay = Some((recording.clone(), tracks.to_vec(), at));
            self.changed_replays += 1;
            self.presentation_tracks = merged;
            self.presentation_sample_valid = false;
            return true;
        }
        self.partial_replay = None;
        let tracks = if evaluated { merged.as_slice() } else { tracks };
        if let Some(damage) = self.damage.as_mut() {
            damage.begin(recording, || scene.frame_signature());
        }
        // Only an owned-layout replay changes the segments; a plain one
        // clones just the layers it replays.
        let mut segments = std::borrow::Cow::Borrowed(scene.segments.as_slice());
        if evaluated
            && let Err(error) = owned_quad::apply(
                &scene.owned_quads,
                &self.presentation_state,
                self.hint_factor().unwrap_or(1.0),
                &mut self.text,
                segments.to_mut(),
                at,
            )
            .and_then(|()| self.text.apply_reflow(segments.to_mut()))
            .and_then(|()| {
                self.text
                    .apply_editors(self.presentation_state.placements(), segments.to_mut())
            })
            .and_then(|()| {
                owned_media::apply(
                    scene,
                    recording.presentation().unwrap(),
                    &self.presentation_state,
                    segments.to_mut(),
                )
            })
            .and_then(|()| {
                owned_scroll::apply(
                    scene,
                    recording.presentation().unwrap(),
                    &self.presentation_state,
                    &mut self.text,
                    segments.to_mut(),
                )
            })
            .and_then(|()| {
                owned_clip::apply(
                    recording.presentation().unwrap(),
                    &self.presentation_state,
                    segments.to_mut(),
                )
            })
        {
            self.presentation_refusal = Some(error);
            self.presentation_tracks = merged;
            self.presentation_recording = None;
            self.presentation_state.clear();
            self.text.clear_reflow();
            self.text.clear_editors();
            self.text.clear_scroll();
            return false;
        }

        let mut clip_reissued = Vec::new();
        // What each replayed layer paints, its groups numbered in the order
        // this frame opens them.
        let mut paint = damage::PaintHasher::default();
        for index in 0..segments.len() {
            let layers = match &mut segments {
                std::borrow::Cow::Owned(segments) => std::mem::take(&mut segments[index].layers),
                std::borrow::Cow::Borrowed(segments) => segments[index].layers.clone(),
            };
            let segment = &segments[index];
            let placement = if evaluated || segment.tracked(tracks) {
                self.place(segment, tracks, evaluated, scene)
            } else {
                Placement::unplaced(segment.path.len())
            };
            if let Some(damage) = self.damage.as_mut() {
                damage.segment(index, placement.key());
            }
            let from = self.layers.as_slice().len();
            self.replay_placed(layers, segment, &placement, &mut clip_reissued);
            self.hash_replayed(from, &mut paint);
        }
        if let Some(damage) = self.damage.as_mut() {
            damage.finish(evaluated);
        }

        self.presentation_tracks = merged;
        self.presentation_sample_valid = evaluated;
        true
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        self.layers.push_transformation(transformation);
    }

    fn end_transformation(&mut self) {
        self.layers.pop_transformation();
    }

    fn fill_quad(&mut self, quad: core::renderer::Quad, background: impl Into<Background>) {
        let background = background.into();
        // A quad that paints nothing is not drawn: a transparent full-window
        // fill (a dim waiting to fade in) still reads and blends every pixel.
        let clear = match &background {
            Background::Color(color) => color.a <= 0.0,
            Background::Gradient(core::Gradient::Linear(linear)) => linear
                .stops
                .iter()
                .flatten()
                .all(|stop| stop.color.a <= 0.0),
        };
        if clear
            && (quad.border.width <= 0.0 || quad.border.color.a <= 0.0)
            && quad.shadow.color.a <= 0.0
        {
            return;
        }
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_quad(quad, background, transformation);
    }

    fn allocate_image(
        &self,
        _handle: &core::image::Handle,
        _callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>) + Send + 'static,
    ) {
        #[cfg(feature = "image")]
        self.image_cache
            .borrow_mut()
            .allocate_image(_handle, _callback);
    }

    fn hint(&mut self, scale_factor: renderer::Scale) {
        self.scale = Some(scale_factor);
    }

    fn scale(&self) -> Option<renderer::Scale> {
        let scale_factor = self.scale?;

        Some(renderer::Scale {
            application: scale_factor.application * self.layers.transformation().scale_factor(),
            ..scale_factor
        })
    }

    fn current_transformation(&self) -> Transformation {
        // The layer stack composes every pushed transformation onto its
        // parent, so `transformation()` is already the cumulative transform
        // in effect (enzICED #46).
        self.layers.transformation()
    }

    fn tick(&mut self) {
        #[cfg(feature = "image")]
        self.image_cache.get_mut().receive();
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        self.presentation_state.set_presented_input(None);
        self.presentation_sample_valid = false;
        self.text.begin_font_frame();
        self.media_claims = owned_media::Assets::default();
        self.native_vector_claims.clear();
        self.native_vector_overflow = false;
        self.presentation_viewport = Some(new_bounds.size());
        self.stack_bounds = new_bounds;
        self.layers.reset(new_bounds);
        self.layers_generation += 1;
        self.active_alpha_groups.clear();
        self.active_clip_sources.clear();
        self.layer_scopes.clear();
        self.next_alpha_group = 0;
        self.alpha_segment = 0;
    }

    fn settings(&self) -> renderer::Settings {
        self.settings
    }
}

impl core::text::Renderer for Renderer {
    type Paragraph = Paragraph;
    type Editor = Editor;

    const ICON_FONT: Font = Font::new("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn reconcile_presented_editor(
        &self,
        node: core::renderer::NodeId,
        editor: &mut Self::Editor,
        viewport: Size,
    ) -> bool {
        let Some(sample) = self.presentation_state.presented_input() else {
            return false;
        };
        let Some(frame) = sample.native_payload::<native_input::Frame>() else {
            return false;
        };
        let Some(geometry) = frame.editors.get(&node.0) else {
            return false;
        };
        geometry.viewport() == viewport && editor.reconcile_geometry(geometry).is_ok()
    }

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();

        layer.draw_paragraph(text, position, color, clip_bounds, transformation);
    }

    fn draw_editor_view(
        &mut self,
        node: Option<core::renderer::NodeId>,
        editor: &Self::Editor,
        paint: &core::text::editor::paint::Paint<'_, core::Font>,
    ) {
        use core::Renderer as _;
        use core::renderer::presentation::{Error, MAX_ITEMS};
        use core::text::Editor as _;
        let Some(node) = node else {
            core::text::editor::paint::draw(self, editor, paint);
            return;
        };
        let owner = self.recordings.iter().any(|recording| {
            recording
                .presentation
                .as_ref()
                .is_some_and(|program| program.owns_node(node.0))
        });
        if !owner {
            core::text::editor::paint::draw(self, editor, paint);
            return;
        }
        let capture = (|| {
            if self.recordings.len() != 1 || !self.scroll_transform_matches(node.0) {
                return Err(Error::OpaquePaint);
            }
            let recording = &self.recordings[0];
            let program = recording
                .presentation
                .as_ref()
                .ok_or(Error::RecordingState)?;
            let (_, declaration) = program
                .editor_nodes()
                .find(|(id, _)| *id == node.0)
                .ok_or(Error::OpaquePaint)?;
            if declaration.recorded != presentation_bounds(paint.bounds)
                || declaration.padding != paint.padding
                || recording.editors.contains_key(&node.0)
            {
                return Err(Error::OpaquePaint);
            }
            if recording.editors.len() >= MAX_ITEMS {
                return Err(Error::Budget);
            }
            let viewport = paint.text_bounds().size();
            let metrics = editor.buffer().metrics();
            let factor = editor.hint_factor().unwrap_or(1.0);
            if editor.bounds().width != viewport.width
                || editor.bounds().height < viewport.height
                || metrics.font_size != paint.size.0 * factor
                || metrics.line_height != paint.line_height.to_absolute(paint.size).0 * factor
            {
                return Err(Error::OpaquePaint);
            }
            let check = owned_quad::OwnedQuad::new(
                core::renderer::Quad {
                    bounds: paint.bounds,
                    border: paint.border,
                    ..Default::default()
                },
                paint.background,
            );
            if !check.valid()
                || !paint.size.0.is_finite()
                || paint.size.0 <= 0.0
                || !paint.caret_width.is_finite()
                || paint.caret_width < 0.0
            {
                return Err(Error::NonFinite);
            }
            let gutter_probe = if let Some(gutter) = paint.gutter {
                if gutter.width > paint.padding.left {
                    return Err(Error::OpaquePaint);
                }
                for row in editor.line_number_rows(viewport.height)? {
                    let _ = gutter.number(row).ok_or(Error::Budget)?;
                }
                let mut probe = crate::graphics::text::cosmic_text::Buffer::new_empty(metrics);
                probe.set_text(
                    "0123456789",
                    &crate::graphics::text::to_attributes(paint.font),
                    crate::graphics::text::cosmic_text::Shaping::Advanced,
                    None,
                );
                Some(Arc::new(probe))
            } else {
                None
            };
            Ok(text::editor::Captured {
                snapshot: editor.snapshot(program.clip().height)?,
                paint: paint.owned()?,
                empty: editor.is_empty(),
                gutter_probe,
            })
        })();
        match capture {
            Ok(captured) => {
                let _ = self.recordings[0].editors.insert(node.0, captured);
                self.mark_recording(node);
                core::text::editor::paint::draw(self, editor, paint);
                self.mark_recording(core::renderer::NodeId::STATIC);
            }
            Err(error) => {
                self.reject_layout_paint(node);
                self.presentation_refusal = Some(error);
                core::text::editor::paint::draw(self, editor, paint);
            }
        }
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_editor(editor, position, color, clip_bounds, transformation);
    }

    fn fill_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text(text, position, color, clip_bounds, transformation);
    }
}

impl graphics::text::Renderer for Renderer {
    fn fill_raw(&mut self, raw: graphics::text::Raw) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text_raw(raw, transformation);
    }
}

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.image_cache
            .borrow_mut()
            .load_image(&self.engine.device, &self.engine.queue, handle)
    }

    fn measure_image(&self, handle: &Self::Handle) -> Option<core::Size<u32>> {
        self.image_cache.borrow_mut().measure_image(handle)
    }

    fn draw_layout_image(
        &mut self,
        node: Option<core::renderer::NodeId>,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        scale: f32,
    ) {
        use core::Renderer as _;
        let captured = node.is_some_and(|node| {
            self.capture_media(
                node,
                owned_media::Captured {
                    paint: graphics::Image::Raster {
                        image: image.clone(),
                        bounds,
                        clip_bounds,
                    },
                    scale,
                },
            )
        });
        self.draw_image(image, bounds, clip_bounds);
        if captured {
            self.mark_recording(core::renderer::NodeId::STATIC);
        }
    }

    fn draw_image(&mut self, image: core::Image, bounds: Rectangle, clip_bounds: Rectangle) {
        // A fully transparent image paints nothing; drawn, a full-window one
        // still reads and blends every pixel.
        if image.opacity <= 0.0 {
            return;
        }
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_raster(image, bounds, clip_bounds, transformation);
    }
}

#[cfg(feature = "svg")]
impl core::svg::Renderer for Renderer {
    fn measure_svg(&self, handle: &core::svg::Handle) -> core::Size<u32> {
        self.image_cache.borrow_mut().measure_svg(handle)
    }

    fn draw_layout_svg(
        &mut self,
        node: Option<core::renderer::NodeId>,
        svg: core::Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        use core::Renderer as _;
        let captured = node.is_some_and(|node| {
            self.capture_media(
                node,
                owned_media::Captured {
                    paint: graphics::Image::Vector {
                        svg: svg.clone(),
                        bounds,
                        clip_bounds,
                    },
                    scale: 1.0,
                },
            )
        });
        self.draw_svg(svg, bounds, clip_bounds);
        if captured {
            self.mark_recording(core::renderer::NodeId::STATIC);
        }
    }

    fn draw_svg(&mut self, svg: core::Svg, bounds: Rectangle, clip_bounds: Rectangle) {
        self.claim_native_vector(&svg.handle);
        if svg.opacity <= 0.0 {
            return;
        }
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_svg(svg, bounds, clip_bounds, transformation);
    }
}

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, mesh: graphics::Mesh) {
        debug_assert!(
            !mesh.indices().is_empty(),
            "Mesh must not have empty indices"
        );

        debug_assert!(
            mesh.indices().len().is_multiple_of(3),
            "Mesh indices length must be a multiple of 3"
        );

        let (layer, transformation) = self.layers.current_mut();
        layer.draw_mesh(mesh, transformation);
    }

    fn draw_mesh_cache(&mut self, cache: mesh::Cache) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_mesh_cache(cache, transformation);
    }
}

#[cfg(feature = "geometry")]
impl graphics::geometry::Renderer for Renderer {
    type Geometry = Geometry;
    type Frame = geometry::Frame;

    fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
        geometry::Frame::new(bounds)
    }

    fn draw_layout_border(
        &mut self,
        node: core::renderer::NodeId,
        bounds: Rectangle,
        border: graphics::geometry::border::Border,
    ) {
        let mut paint = owned_quad::OwnedQuad::new(
            core::renderer::Quad {
                bounds,
                ..Default::default()
            },
            Color::TRANSPARENT.into(),
        );
        paint.border = Some(border);
        self.fill_owned_quad(node, paint);
    }

    fn draw_geometry(&mut self, geometry: Self::Geometry) {
        #[cfg(feature = "svg")]
        {
            let images: &[graphics::Image] = match &geometry {
                Geometry::Live { images, .. } => images,
                Geometry::Cached(cache) => cache.images.as_deref().unwrap_or_default(),
            };
            for image in images {
                if let graphics::Image::Vector { svg, .. } = image {
                    self.claim_native_vector(&svg.handle);
                }
            }
        }
        let (layer, transformation) = self.layers.current_mut();

        match geometry {
            Geometry::Live {
                meshes,
                images,
                text,
            } => {
                layer.draw_mesh_group(meshes, transformation);

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, transformation);
            }
            Geometry::Cached(cache) => {
                if let Some(meshes) = cache.meshes {
                    layer.draw_mesh_cache(meshes, transformation);
                }

                if let Some(images) = cache.images {
                    for image in images.iter().cloned() {
                        layer.draw_image(image, transformation);
                    }
                }

                if let Some(text) = cache.text {
                    layer.draw_text_cache(text, transformation);
                }
            }
        }
    }
}

impl primitive::Renderer for Renderer {
    fn draw_primitive(&mut self, bounds: Rectangle, primitive: impl Primitive) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_primitive(bounds, primitive, transformation);
    }
}

impl graphics::compositor::Default for crate::Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(settings: renderer::Settings, backend: Option<&str>) -> Option<Self> {
        if backend.is_some_and(|backend| backend != "wgpu") {
            return None;
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok()?;

        let (device, queue) = device::request(
            &adapter,
            &wgpu::DeviceDescriptor {
                label: Some("iced_wgpu [headless]"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            },
        )
        .await
        .ok()?;

        let engine = Engine::new(
            &adapter,
            device,
            queue,
            if graphics::color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            Some(graphics::Antialiasing::MSAAx4),
            Shell::headless(),
        );

        Some(Self::new(engine, settings))
    }

    fn name(&self) -> String {
        "wgpu".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        self.screenshot(
            &Viewport::with_physical_size(
                size,
                renderer::Scale {
                    window: 1.0,
                    application: scale_factor,
                },
            ),
            background_color,
        )
    }
}
