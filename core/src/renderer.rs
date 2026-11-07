// Modified by enzUI contributors. See FORK.md.
//! Write your own renderer.
pub mod docked;
pub mod presentation;

#[cfg(debug_assertions)]
mod null;

use crate::border;
use crate::image;
use crate::material;
use crate::text;
use crate::{
    Background, Border, Color, Font, Pixels, Rectangle, Shadow, Size, Transformation, Vector,
};

/// Whether anti-aliasing should be avoided by snapping primitive coordinates to the
/// pixel grid.
pub const CRISP: bool = cfg!(feature = "crisp");

/// Renderer-owned paint that can outlive the widgets that recorded it.
///
/// Clones share the backend payload and any attached presentation program;
/// widgets do not take ownership of backend textures or layer storage.
#[derive(Clone)]
pub struct Recording {
    payload: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    presentation: Option<std::sync::Arc<presentation::Program>>,
    docked_tenure: Option<docked::Tenure>,
}

impl Recording {
    /// Wraps a backend payload as a [`Recording`].
    pub fn new<T: std::any::Any + Send + Sync>(payload: T) -> Self {
        Self {
            payload: std::sync::Arc::new(payload),
            presentation: None,
            docked_tenure: None,
        }
    }

    /// Borrows the backend payload, if it has the expected type.
    ///
    /// Returns `None` for a recording produced by a *different* backend, which
    /// is what makes a renderer swap (or a fallback) safe rather than a
    /// mis-cast: the replay simply reports that it did not happen.
    pub fn payload<T: std::any::Any + Send + Sync>(&self) -> Option<&T> {
        self.payload.downcast_ref::<T>()
    }

    /// Couples immutable layout/property declarations to these exact scene bytes.
    ///
    /// The recording backend must validate node membership and clip coverage
    /// before replaying an attached program.
    pub fn with_presentation(mut self, program: std::sync::Arc<presentation::Program>) -> Self {
        self.presentation = Some(program);
        self
    }

    /// Binds an admitted full-shell capture to the layout identities its scene
    /// was recorded from.
    pub fn with_docked_tenure(mut self, tenure: docked::Tenure) -> Self {
        self.docked_tenure = Some(tenure);
        self
    }

    /// The tenure of the full-shell capture this recording published, if any.
    pub fn docked_tenure(&self) -> Option<&docked::Tenure> {
        self.docked_tenure.as_ref()
    }

    /// The owned declarations carried by this scene revision.
    pub fn presentation(&self) -> Option<&std::sync::Arc<presentation::Program>> {
        self.presentation.as_ref()
    }

    /// Whether both paint and program have the same immutable owners.
    pub fn same_revision(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.payload, &other.payload)
            && match (&self.presentation, &other.presentation) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recording(<backend payload>)")
    }
}

/// The identity of retained paint or an owned layout allocation.
///
/// Zero denotes static content. The upper half of the range is reserved for
/// automatically lowered layout nodes; explicit motion/driver IDs use the lower half.
/// Automatic layout groups may be recording-local; animation driver identities
/// must remain stable across scene revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

static NEXT_LAYOUT_IDENTITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl NodeId {
    /// The node every recording starts in: content no track animates.
    pub const STATIC: Self = Self(0);

    pub(crate) fn unique_layout() -> Self {
        use std::sync::atomic::Ordering;
        let id = NEXT_LAYOUT_IDENTITY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1).filter(|next| *next < (1 << 63))
            })
            .expect("automatic presentation identity exhausted");
        Self(id | (1 << 63))
    }

    /// How many automatic layout identities the process has minted.
    ///
    /// Monotonic across every window, and never reused.
    #[must_use]
    pub fn layout_watermark() -> u64 {
        NEXT_LAYOUT_IDENTITY.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Native culling state in the widget's drawing coordinate system.
#[derive(Debug, Clone, Copy)]
pub struct CullCapture {
    /// Physical visibility mask; visible subtrees inherit the existing clip.
    pub mask: Rectangle,
    /// Logical viewport passed to child drawing, independent of source-capture coverage.
    pub viewport: Rectangle,
}

/// Source material coverage and visible mask for an owned allocation clip.
#[derive(Debug, Clone, Copy)]
pub struct ClipCapture {
    /// Physical clipping bounds in the widget's drawing coordinates.
    pub mask: Rectangle,
    /// Material viewport for child drawing; not a visibility or input region.
    pub viewport: Rectangle,
}

/// Declared source of a clipping layer during responsive scene capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutLayer {
    /// The allocation and outsets of an explicit layout clip node.
    Clip(NodeId),
    /// Whole-target content boundary of a four-slot priority band.
    BandContent(NodeId),
    /// The clipped viewport of a complete document-strip material inventory.
    TabStrip(NodeId),
    /// Hidden material clips to nothing; visible material inherits its enclosing viewport.
    Material(NodeId),
    /// A native viewport visibility gate, independent of the subtree's paint allocation.
    Cull(NodeId),
    /// A scroll viewport, or its separately drawn scrollbar clip.
    Scroll {
        /// Viewport identity, not the separate scrollbar-paint identity.
        node: NodeId,
        /// Selects the native scrollbar clip, which extends the visible viewport by up to two pixels.
        chrome: bool,
    },
    /// A paint-order layer that introduces no clip beyond its inherited viewport.
    Inherited,
}

/// A compositor animation track: the properties applied to one retained scene
/// node when the scene is replayed (enzICED #2219).
///
/// These are the properties a compositor can apply to already-recorded paint
/// without asking a widget anything: placement, opacity, uniform scale,
/// rotation and clipping. Scale and rotation are applied to the node's
/// renderer-owned offscreen target, not by re-deriving baked
/// quad/text/custom-primitive geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    /// The node this track drives.
    pub node: NodeId,
    /// Where the node's recorded paint is placed, relative to where it was
    /// recorded.
    pub translation: Vector,
    /// The opacity applied to the node as a whole, composited off-screen so
    /// overlapping primitives inside it do not show through each other.
    pub opacity: f32,
    /// Uniform scale applied to the retained node as one composited surface.
    pub scale: f32,
    /// Scale and rotation origin, in the coordinates the node's layout is in,
    /// mapped like [`clip`](Self::clip).
    pub scale_origin: crate::Point,
    /// Rotation about [`scale_origin`](Self::scale_origin), in radians,
    /// clockwise on screen. Like scale, it turns the node as one composited
    /// surface: a widget that turns paints through
    /// [`Renderer::with_layer_composite`] with the same angle, so a drawn and
    /// a replayed frame sample the same texture.
    pub rotation: f32,
    /// An optional clip applied to the node, in the coordinates its layout
    /// is in. It is not moved by `translation`; replay maps it through the
    /// transform the node's ancestors applied when it was recorded, so a
    /// node inside a scrolled or translated parent is clipped where it is
    /// shown.
    pub clip: Option<Rectangle>,
}

impl Track {
    /// Exposes the sampled scalar channels used by owned presentation expressions.
    pub fn presentation_input(&self) -> presentation::Input {
        let clip = self.clip.unwrap_or_default();
        presentation::Input {
            node: self.node.0,
            values: [
                self.translation.x,
                self.translation.y,
                self.opacity,
                self.scale,
                clip.x,
                clip.y,
                clip.width,
                clip.height,
            ],
            available: if self.clip.is_some() { 0xff } else { 0x0f },
        }
    }

    /// A track that leaves the node exactly where it was recorded.
    pub fn identity(node: NodeId) -> Self {
        Self {
            node,
            translation: Vector::ZERO,
            opacity: 1.0,
            scale: 1.0,
            scale_origin: crate::Point::ORIGIN,
            rotation: 0.0,
            clip: None,
        }
    }

    /// Returns whether this track leaves its node untouched, in which case a
    /// replay can take the cheapest path.
    pub fn is_identity(&self) -> bool {
        self.translation == Vector::ZERO
            && self.opacity >= 1.0
            && (self.scale - 1.0).abs() <= f32::EPSILON
            && self.rotation == 0.0
            && self.clip.is_none()
    }

    /// Interpolates between two track values.
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;

        Self {
            node: a.node,
            translation: Vector::new(
                mix(a.translation.x, b.translation.x),
                mix(a.translation.y, b.translation.y),
            ),
            opacity: mix(a.opacity, b.opacity),
            scale: mix(a.scale, b.scale),
            scale_origin: crate::Point::new(
                mix(a.scale_origin.x, b.scale_origin.x),
                mix(a.scale_origin.y, b.scale_origin.y),
            ),
            rotation: mix(a.rotation, b.rotation),
            clip: match (a.clip, b.clip) {
                (Some(a), Some(b)) => Some(Rectangle {
                    x: mix(a.x, b.x),
                    y: mix(a.y, b.y),
                    width: mix(a.width, b.width),
                    height: mix(a.height, b.height),
                }),
                // A clip that only one end declares is applied for the whole
                // travel: growing one out of nothing would clip the node away
                // on the first frame.
                (Some(clip), None) | (None, Some(clip)) => Some(clip),
                (None, None) => None,
            },
        }
    }
}

/// A renderer-owned animation for one scene node: the *description* of a
/// track's whole travel, which the lane samples from the presentation clock
/// (enzICED #2219).
///
/// A [`Track`] is one sampled instant; a [`TrackAnimation`] is the curve. The
/// difference is the entire isolation claim: a widget that publishes sampled
/// tracks needs the UI thread to wake for every frame, while a widget that
/// publishes an animation hands the render lane everything it needs to keep
/// advancing the node on its own clock -- including while the UI thread is
/// blocked in unrelated work.
///
/// The easing lives in the *keyframes*, not in the lane: the effect samples its
/// own curve into `keyframes` (which is what keeps theme easing, reduced motion
/// and per-effect shaping on the widget side), and the lane only interpolates
/// between neighbours on a normalised timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAnimation {
    /// The node this animation drives.
    pub node: NodeId,
    /// When the travel started, on the same clock the lane samples.
    pub start: crate::time::Instant,
    /// How long the travel lasts. A zero duration is a settled value.
    pub duration: crate::time::Duration,
    /// The eased curve, as `(t, value)` pairs with `t` ascending over `0..=1`.
    ///
    /// At least two are required; an animation with fewer is refused by
    /// [`TrackAnimation::is_usable`] rather than silently sampled as a
    /// constant.
    pub keyframes: Vec<(f32, Track)>,
    /// Increments whenever the effect retargets or reverses (enzICED #2219).
    ///
    /// The lane uses it to tell a *continuation* of the same travel from a new
    /// one, so a retarget does not restart a track that is already running and
    /// a re-publication of the same travel does not jump it back.
    pub generation: u64,
}

impl TrackAnimation {
    /// Whether this animation can be sampled at all.
    pub fn is_usable(&self) -> bool {
        self.keyframes.len() >= 2 && self.duration > crate::time::Duration::ZERO
    }

    /// The normalised progress of this animation at `now`, clamped to `0..=1`.
    pub fn progress(&self, now: crate::time::Instant) -> f32 {
        if self.duration == crate::time::Duration::ZERO {
            return 1.0;
        }

        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();

        (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Whether the travel has run out at `now`.
    pub fn is_finished(&self, now: crate::time::Instant) -> bool {
        self.progress(now) >= 1.0
    }

    /// The track this animation presents at `now`.
    ///
    /// Returns `None` when the animation has no usable curve, so a caller can
    /// never mistake a malformed animation for a settled one.
    pub fn sample(&self, now: crate::time::Instant) -> Option<Track> {
        if !self.is_usable() {
            return None;
        }

        let t = self.progress(now);

        // Keyframes are in time order: the first at or after `t` and the one
        // before it, found by bisection (a travel can carry thousands).
        let index = self.keyframes.partition_point(|keyframe| keyframe.0 < t);
        let Some(keyframe) = self.keyframes.get(index) else {
            return self.keyframes.last().map(|keyframe| keyframe.1);
        };
        let previous = &self.keyframes[index.saturating_sub(1)];
        let span = keyframe.0 - previous.0;
        let local = if span > f32::EPSILON {
            (t - previous.0) / span
        } else {
            1.0
        };

        Some(Track::lerp(&previous.1, &keyframe.1, local))
    }
}

/// A component that can be used by widgets to draw themselves on a screen.
pub trait Renderer {
    /// Attaches one owned layout/property program to the outermost open recording.
    ///
    /// Call before painting its declared leaves. Programs require named-track
    /// replay so they receive their current driver values.
    ///
    /// # Errors
    /// Returns a capability/state refusal rather than accepting unsupported work.
    fn record_presentation(
        &mut self,
        _program: std::sync::Arc<presentation::Program>,
    ) -> Result<(), presentation::Error> {
        Err(presentation::Error::UnsupportedBackend)
    }

    /// Registers responsive shell intent in the current capture, not a handoff acknowledgement.
    ///
    /// Success only records the request. Finishing must refuse a scene without complete
    /// responsive layout and presented-input ownership; ordinary drawing remains available.
    fn request_docked_presentation(
        &mut self,
        _capture: docked::Capture,
    ) -> Result<(), presentation::Error> {
        Err(presentation::Error::UnsupportedBackend)
    }

    /// Starts an owned scroll scope and returns its source-capture viewport when supported.
    ///
    /// The returned viewport controls source culling only; ordinary paint clipping must
    /// remain unchanged. A supported scope must be paired with [`Self::end_scroll_view`].
    fn begin_scroll_view(
        &mut self,
        _node: NodeId,
        _bounds: Rectangle,
        _content: Rectangle,
        _config: crate::layout::scroll::Config,
        _input: crate::layout::scroll::Input,
        _paint: presentation::scroll_paint::Paint,
    ) -> Option<Rectangle> {
        None
    }
    /// Draws scroll overlay chrome; a retained backend also binds its paint identity.
    fn draw_scroll_chrome(
        &mut self,
        _node: Option<NodeId>,
        bounds: Rectangle,
        content: Rectangle,
        config: crate::layout::scroll::Config,
        input: crate::layout::scroll::Input,
        paint: presentation::scroll_paint::Paint,
    ) {
        for (quad, fill) in paint.chrome(config, input, bounds, content) {
            self.fill_quad(quad, fill);
        }
    }
    /// Closes a scope whose [`Self::begin_scroll_view`] returned a capture viewport.
    fn end_scroll_view(&mut self, _node: NodeId) {}

    /// Reports whether an empty declared leaf must still supply capture material.
    /// Returns false outside supported owned capture; this grants no paint or input visibility.
    fn retains_layout_leaf(&self, _node: NodeId) -> bool {
        false
    }

    /// Source-culling coverage for a declared node in the active owned capture.
    ///
    /// Ordinary drawing returns `None`. Physical paint clipping must not use this coverage.
    fn layout_capture_viewport(&self, _node: NodeId) -> Option<Rectangle> {
        None
    }

    /// Canonical logical viewport and physical mask for a declared native culling scope.
    /// Widened material-capture coverage must not decide whether the source is visible.
    fn layout_culling_capture(&self, _scope: NodeId, _viewport: Rectangle) -> Option<CullCapture> {
        None
    }

    /// Retains an allocation clip's child material even when its visible mask is empty.
    /// Ordinary drawing and unsupported clip identities return `None`.
    fn layout_clip_capture(&self, _node: NodeId) -> Option<ClipCapture> {
        None
    }

    /// Whether the same culling scope is already active through an enclosing native draw guard.
    fn layout_culling_active(&self, _scope: NodeId) -> bool {
        false
    }

    /// Source-culling viewport for all variants of an owned document strip.
    /// Painting still uses its declared visible clip; ordinary drawing returns `None`.
    fn tab_strip_capture_viewport(&self, _node: NodeId) -> Option<Rectangle> {
        None
    }

    /// Maximum viewport height for an editor owned by the active presentation program.
    fn editor_presentation_height(&self, _node: NodeId) -> Option<f32> {
        None
    }

    /// Draws editor overlay chrome, binding its geometry to the owned editor when supported.
    fn fill_layout_editor_scrollbars(
        &mut self,
        owner: NodeId,
        _editor: NodeId,
        _source: Option<std::sync::Arc<()>>,
        bounds: Rectangle,
        metrics: crate::text::editor::ScrollMetrics,
        paint: crate::text::editor::scrollbar::Paint,
    ) {
        self.reject_layout_paint(owner);
        for (quad, color) in paint.quads(bounds, metrics).into_iter().flatten() {
            self.fill_quad(quad, color);
        }
    }

    /// Draws owned allocation paint when supported, otherwise refuses that responsive capture.
    ///
    /// Paint is still emitted for the current UI frame. Backends must not accept unsupported
    /// layout paint as static commands in a responsive recording.
    fn fill_layout_quad(&mut self, node: NodeId, quad: Quad, background: Background) {
        self.reject_layout_paint(node);
        self.fill_quad(quad, background);
    }

    /// Draws a clocked input caret while preserving native paint on unsupported backends.
    /// Responsive capture must retain both phases or refuse, never freeze one phase.
    fn fill_layout_blink(
        &mut self,
        node: NodeId,
        mut quad: Quad,
        background: Background,
        blink: crate::layout::input::Blink,
        at: crate::time::Instant,
    ) {
        self.reject_layout_paint(node);
        if blink.visible(at) {
            self.fill_quad(quad, background);
        } else {
            quad.bounds.width = 0.0;
            quad.bounds.height = 0.0;
            self.fill_quad(quad, crate::Color::TRANSPARENT);
        }
    }

    /// Draws allocation-bound rule geometry or refuses unsupported responsive paint.
    fn fill_layout_rule(
        &mut self,
        node: NodeId,
        _allocation: Rectangle,
        quad: Quad,
        background: Background,
        _paint: crate::layout::rule::Paint,
    ) {
        self.reject_layout_paint(node);
        self.fill_quad(quad, background);
    }

    /// Validates native scroll state against the installed frame and returns measured rail visibility.
    /// Unknown metadata, changed offsets/configuration or different allocated sizes refuse.
    fn reconcile_presented_scroll(
        &self,
        _node: NodeId,
        _config: crate::layout::scroll::Config,
        _input: crate::layout::scroll::Input,
        _viewport: Size,
        _content: Size,
    ) -> Option<bool> {
        None
    }

    /// Installs confirmed geometry for a synchronous, read-only semantics pass.
    /// The caller validates the receipt and clears the candidate before dispatching input.
    fn install_presented_geometry(
        &mut self,
        _recording: &Recording,
        _sample: std::sync::Arc<presentation::Sample>,
    ) -> bool {
        false
    }

    /// Installs a compositor-validated layout candidate bound to this recording.
    ///
    /// The caller verifies feedback; the backend validates program, draw revision and native
    /// payload. Native adapters must succeed before dispatch. Failure revokes prior input data.
    fn install_presented_layout(
        &mut self,
        _recording: &Recording,
        _sample: std::sync::Arc<presentation::Sample>,
    ) -> bool {
        false
    }

    /// The current reconciliation candidate; presence alone does not authorize native dispatch.
    fn presented_layout(&self) -> Option<&std::sync::Arc<presentation::Sample>> {
        None
    }

    /// Revokes batch input data on invalidation, new drawing or surface retirement.
    fn clear_presented_layout(&mut self) {}

    /// Refuses an open responsive recording owning `node` when its paint cannot follow layout.
    ///
    /// Ordinary drawing and recordings that do not own the node are unchanged.
    fn reject_layout_paint(&mut self, node: NodeId) {
        let _ = node;
    }

    /// The most recent program admission/evaluation refusal, if any.
    fn presentation_refusal(&self) -> Option<presentation::Error> {
        None
    }

    /// Starts capturing every primitive recorded from now on, so the result can
    /// be replayed on a later frame without asking any widget to draw again
    /// (enzICED #2219).
    ///
    /// `bounds` opens a clip scope, which is what makes the capture
    /// self-contained. Returns `false` when this renderer cannot retain paint;
    /// the caller must then keep drawing normally. The default implementation
    /// returns `false`, so every existing renderer keeps its exact behaviour
    /// and the retained path is opt-in per backend.
    ///
    /// Must be paired with [`finish_recording`](Self::finish_recording).
    fn start_recording(&mut self, bounds: Rectangle) -> bool {
        let _ = bounds;
        false
    }

    /// Starts replacement capture without expiring the currently published layout tenure.
    ///
    /// Returns `false` without drawing when unsupported. Pair success with
    /// [`Self::finish_recording`]; a returned staged tenure must be activated by
    /// the surface owner between frames before replay. Nested recordings share the stage.
    fn start_staged_recording(&mut self, _bounds: Rectangle) -> bool {
        false
    }

    /// Attributes everything recorded from now on to scene node `node`
    /// (enzICED #2219).
    ///
    /// This is what gives a recording internal structure: the paint before the
    /// mark stays static, the paint after it belongs to `node` and can be
    /// driven by a [`Track`]. Calling it again with
    /// [`NodeId::STATIC`] returns to static content, so a moving subtree can sit
    /// in the middle of a frame with unmoved siblings on both sides and keep
    /// its painting order.
    ///
    /// Has no effect outside a recording.
    fn mark_recording(&mut self, node: NodeId) {
        let _ = node;
    }

    /// [`mark_recording`](Self::mark_recording) for a node whose paint is
    /// recorded already moved by `baseline`.
    ///
    /// The recording keeps the baseline with the node, and a [`Track`]
    /// replaying that node is then read as the node's whole translation: the
    /// replay moves the paint by `track.translation - baseline`. A widget that
    /// draws its own travel can so publish one absolute curve, and a scene
    /// recorded mid-travel stays consistent with it whichever frame drew it
    /// (enzUI-ICED #3251). A backend that does not record ignores it.
    fn mark_recording_at(&mut self, node: NodeId, baseline: Vector) {
        let _ = baseline;
        self.mark_recording(node);
    }

    /// Opens an automatically identified paint scope only when an active program owns the node.
    ///
    /// A `true` return must be paired with `mark_recording(NodeId::STATIC)` after drawing.
    /// Unsupported backends and ordinary drawing return `false` without changing recording state.
    fn start_layout_recording(&mut self, node: NodeId) -> bool {
        let _ = node;
        false
    }

    /// Closes the scope opened by [`start_recording`](Self::start_recording)
    /// and returns the retained recording, if one was captured.
    ///
    /// The captured primitives are **also** left in the current frame, so
    /// recording never costs an extra pass: the frame that records is a normal
    /// frame that happens to keep a copy.
    fn finish_recording(&mut self) -> Option<Recording> {
        None
    }

    /// Re-submits a previously captured `recording`, offset by `translation`,
    /// without re-entering any widget (enzICED #2219).
    ///
    /// Returns `false` when this renderer cannot replay the recording -- a
    /// foreign backend payload, or a backend with no retained path -- in which
    /// case the caller must fall back to drawing the subtree.
    fn replay(&mut self, recording: &Recording, translation: Vector) -> bool {
        let _ = (recording, translation);
        false
    }

    /// Arranges for the next present to show the frame last drawn again,
    /// preparing only its custom primitives, so a primitive whose content
    /// changed is drawn anew without any widget drawing (enzUI-ICED #3442).
    ///
    /// Returns `false` when the backend cannot: nothing is arranged and the
    /// caller must draw the frame as usual.
    fn reuse_frame(&mut self) -> bool {
        false
    }

    /// Replays owned paint with named tracks and any attached presentation program.
    ///
    /// Nodes unaffected by either retain their recorded placement without
    /// re-entering widgets. Returns `false` when replay cannot be performed;
    /// the caller must draw normally instead of treating refusal as presentation.
    fn replay_tracks(&mut self, recording: &Recording, tracks: &[Track]) -> bool {
        let _ = (recording, tracks);
        false
    }

    /// Replays allocation and time-dependent paint at one monotonic frame instant.
    /// Backends without retained clocks keep their ordinary replay behavior.
    fn replay_tracks_at(
        &mut self,
        recording: &Recording,
        tracks: &[Track],
        _at: crate::time::Instant,
    ) -> bool {
        self.replay_tracks(recording, tracks)
    }

    /// Starts recording a new layer.
    fn start_layer(&mut self, bounds: Rectangle);

    /// Starts a layer whose current frame is clipped to `visible_bounds`, while
    /// a retained copy preserves ink through `retained_bounds` (#2219).
    fn start_layer_retained(&mut self, visible_bounds: Rectangle, retained_bounds: Rectangle) {
        let _ = retained_bounds;
        self.start_layer(visible_bounds);
    }

    /// Draws a live-clipped layer while preserving wider retained ink.
    fn with_layer_retained(
        &mut self,
        visible_bounds: Rectangle,
        retained_bounds: Rectangle,
        f: impl FnOnce(&mut Self),
    ) {
        self.start_layer_retained(visible_bounds, retained_bounds);
        f(self);
        self.end_layer();
    }

    /// Closes the current layer; subsequent drawing follows it in painter order.
    fn end_layer(&mut self);

    /// Draws the primitives recorded in the given closure in a new layer.
    ///
    /// The layer will clip its contents to the provided `bounds`.
    fn with_layer(&mut self, bounds: Rectangle, f: impl FnOnce(&mut Self)) {
        self.start_layer(bounds);
        f(self);
        self.end_layer();
    }

    /// Starts recording a compositing layer with uniform `alpha`.
    ///
    /// A backend that supports subtree compositing records every primitive in
    /// the layer at full strength and applies `alpha` once when the completed
    /// layer is blended into its parent. This distinction is load-bearing:
    /// applying alpha to each primitive separately would darken overlaps, and
    /// nested alpha layers must multiply.
    ///
    /// The method is additive. Its default implementation ignores `alpha` and
    /// forwards to [`start_layer`](Self::start_layer), preserving the fully
    /// opaque historical behaviour of renderers that do not implement
    /// compositing. Callers must pair it with [`end_layer`](Self::end_layer).
    fn start_layer_alpha(&mut self, bounds: Rectangle, alpha: f32) {
        let _ = alpha;
        self.start_layer(bounds);
    }

    /// Draws the primitives recorded in the closure in one compositing layer.
    ///
    /// This is the closure counterpart of
    /// [`start_layer_alpha`](Self::start_layer_alpha).
    fn with_layer_alpha(&mut self, bounds: Rectangle, alpha: f32, f: impl FnOnce(&mut Self)) {
        self.start_layer_alpha(bounds, alpha);
        f(self);
        self.end_layer();
    }

    /// Starts one opacity/scale/rotation compositing group, scaled and turned
    /// by `rotation` radians (clockwise on screen) about `origin`.
    ///
    /// The default preserves the historical fallback by transforming
    /// primitives inside an alpha layer. Backends with offscreen subtree
    /// compositing override this pair so live and retained Scale frames use the
    /// same texture path.
    fn start_layer_composite(
        &mut self,
        bounds: Rectangle,
        alpha: f32,
        scale: f32,
        rotation: f32,
        origin: crate::Point,
    ) {
        self.start_layer_alpha(bounds, alpha);
        self.start_transformation(
            Transformation::translate(origin.x, origin.y)
                * Transformation::rotate(rotation)
                * Transformation::scale(scale)
                * Transformation::translate(-origin.x, -origin.y),
        );
    }

    /// Ends a group opened by [`Self::start_layer_composite`].
    fn end_layer_composite(&mut self) {
        self.end_transformation();
        self.end_layer();
    }

    /// Draws one opacity/scale/rotation compositing group; see
    /// [`start_layer_composite`](Self::start_layer_composite).
    fn with_layer_composite(
        &mut self,
        bounds: Rectangle,
        alpha: f32,
        scale: f32,
        rotation: f32,
        origin: crate::Point,
        f: impl FnOnce(&mut Self),
    ) {
        self.start_layer_composite(bounds, alpha, scale, rotation, origin);
        f(self);
        self.end_layer_composite();
    }

    /// What this renderer can do with a sampled-backdrop [`Material`]
    /// (enzUI-ICED #2950).
    ///
    /// Ask **before** deciding what to paint. A material region paints no fill
    /// of its own when the answer is [`material::Support::Optical`], because
    /// the composite writes those pixels; when the answer is
    /// [`material::Support::Unsupported`] the caller must paint an opaque
    /// panel instead, or leave a hole.
    ///
    /// The default is `Unsupported`, so every renderer that does not override
    /// this keeps its exact previous behaviour and no caller is ever told a
    /// blur happened that did not.
    ///
    /// [`Material`]: crate::material::Material
    fn material_support(&self) -> material::Support {
        material::Support::Unsupported
    }

    /// The most recent average brightness, `0.0` to `1.0` in perceptual
    /// (sRGB-encoded) terms, of the backdrop behind the material whose
    /// [`probe`](material::Material::probe) is `probe`, or `None` when it has
    /// not been measured.
    ///
    /// Measured on the GPU and read back asynchronously, so it describes a
    /// frame or two ago. The default measures nothing.
    fn material_luminance(&self, probe: u64) -> Option<f32> {
        let _ = probe;
        None
    }

    /// The latest material luminance and the renderer frame that sampled it.
    /// The frame identifies the source, rather than asynchronous delivery time.
    /// Backends without frame provenance return frame zero.
    fn material_luminance_with_frame(&self, probe: u64) -> Option<(f32, u64)> {
        self.material_luminance(probe).map(|value| (value, 0))
    }

    /// The most recent renderer frame number; zero without material frame provenance.
    fn material_frame(&self) -> u64 {
        0
    }

    /// Install the owning window's redraw callback for draw-time material requests.
    fn set_material_waker(&mut self, _waker: crate::shell::Waker) {}

    /// Wake the owning window to complete an asynchronous material measurement.
    /// The default does nothing for backends without material readback.
    fn request_material_redraw(&self) {}

    /// Starts a layer whose backdrop is filtered by `material` before the
    /// layer's own contents are drawn (enzUI-ICED #2950).
    ///
    /// This is an **ordered** boundary, which is the whole point: everything
    /// painted before it is the material's source, and everything painted
    /// inside it is foreground the material must never sample. A backend that
    /// supports it resolves the content so far, filters it once for every
    /// region sharing that source, composites this region, and only then draws
    /// the layer.
    ///
    /// The method is additive. The default ignores `material` and forwards to
    /// [`start_layer`](Self::start_layer), which is correct precisely because
    /// a caller must consult [`material_support`](Self::material_support)
    /// first and paint its own opaque panel when the answer is unsupported.
    ///
    /// Pair it with [`end_layer`](Self::end_layer), like every other layer.
    fn start_layer_material(&mut self, bounds: Rectangle, material: material::Material) {
        let _ = material;
        self.start_layer(bounds);
    }

    /// Draws the primitives recorded in the closure over a filtered backdrop.
    ///
    /// The closure counterpart of
    /// [`start_layer_material`](Self::start_layer_material).
    fn with_layer_material(
        &mut self,
        bounds: Rectangle,
        material: material::Material,
        f: impl FnOnce(&mut Self),
    ) {
        self.start_layer_material(bounds, material);
        f(self);
        self.end_layer();
    }

    /// Starts recording a new layer whose contents are clipped to `bounds`
    /// rounded by `radius` (enzICED #41).
    ///
    /// This is the rounded-corner counterpart of [`start_layer`]. It exists
    /// because iced's clip stack is otherwise rectangle-only end to end, so a
    /// child painted inside a rounded container spills over the corner arc
    /// (enzUI-ICED #41, enzUI #80). Rounded clipping cannot be expressed at the
    /// widget layer and must be provided by the renderer.
    ///
    /// The method is **additive**: the default implementation ignores `radius`
    /// and forwards to [`start_layer`], preserving the exact previous behaviour
    /// for every renderer that does not override it. A backend that can honour
    /// the radius (currently `iced_tiny_skia`) overrides this to mask the
    /// rounded corners; a backend that cannot (currently `iced_wgpu`, whose
    /// hardware clip is `set_scissor_rect`) keeps the rectangular fallback.
    ///
    /// Callers must still pair this with [`end_layer`], exactly like
    /// [`start_layer`].
    ///
    /// [`start_layer`]: Self::start_layer
    /// [`end_layer`]: Self::end_layer
    fn start_layer_rounded(&mut self, bounds: Rectangle, radius: border::Radius) {
        let _ = radius;
        self.start_layer(bounds);
    }

    /// Draws the primitives recorded in the given closure in a new layer whose
    /// contents are clipped to `bounds` rounded by `radius` (enzICED #41).
    ///
    /// The rounded-corner counterpart of [`with_layer`].
    ///
    /// [`with_layer`]: Self::with_layer
    fn with_layer_rounded(
        &mut self,
        bounds: Rectangle,
        radius: border::Radius,
        f: impl FnOnce(&mut Self),
    ) {
        self.start_layer_rounded(bounds, radius);
        f(self);
        self.end_layer();
    }

    /// Opens a layer whose clipping geometry has an explicit responsive source.
    ///
    /// A true return opens a recording-node scope as well; close the layer and
    /// then return that scope to `NodeId::STATIC`. Ordinary rendering is unchanged.
    fn start_layout_layer(
        &mut self,
        source: LayoutLayer,
        bounds: Rectangle,
        radius: Option<border::Radius>,
    ) -> bool {
        let _ = source;
        if let Some(radius) = radius {
            self.start_layer_rounded(bounds, radius);
        } else {
            self.start_layer(bounds);
        }
        false
    }

    /// Complete declared reveal material coverage during capture; never an ordinary drawing viewport.
    fn layout_reveal_capture_viewport(&self, node: NodeId) -> Option<Rectangle> {
        let _ = node;
        None
    }

    /// Opens a declared retained reveal at unit-interval progress during owned capture.
    /// A false result opens no layer. A true result requires both `end_layer` and a static recording mark.
    fn start_layout_reveal(&mut self, node: NodeId, progress: f32) -> bool {
        let _ = (node, progress);
        false
    }

    /// Draws in a declared layout-relative clip, preserving native drawing when capture is unavailable.
    fn with_layout_layer(
        &mut self,
        source: LayoutLayer,
        bounds: Rectangle,
        radius: Option<border::Radius>,
        draw: impl FnOnce(&mut Self),
    ) {
        let marked = self.start_layout_layer(source, bounds, radius);
        draw(self);
        self.end_layer();
        if marked {
            self.mark_recording(NodeId::STATIC);
        }
    }

    /// Starts recording with a new [`Transformation`].
    fn start_transformation(&mut self, transformation: Transformation);

    /// Ends recording a new layer.
    ///
    /// The new layer will clip its contents to the provided `bounds`.
    fn end_transformation(&mut self);

    /// Applies a [`Transformation`] to the primitives recorded in the given closure.
    fn with_transformation(&mut self, transformation: Transformation, f: impl FnOnce(&mut Self)) {
        self.start_transformation(transformation);
        f(self);
        self.end_transformation();
    }

    /// Applies a translation to the primitives recorded in the given closure.
    fn with_translation(&mut self, translation: Vector, f: impl FnOnce(&mut Self)) {
        self.with_transformation(Transformation::translate(translation.x, translation.y), f);
    }

    /// Paints `f` as the scene node `node`, moved by `offset`: the way a
    /// widget draws something it animates with [`Track`]s.
    ///
    /// The offset is snapped to whole device pixels, as replay snaps a
    /// track's movement, so a frame drawn here and a frame replayed from a
    /// recording put every edge on the same pixel. The node is marked at the
    /// snapped offset ([`mark_recording_at`](Self::mark_recording_at)) and
    /// static content resumes after `f`. Publish tracks with the node's whole
    /// translation, unsnapped; replay snaps it the same way.
    fn with_motion(&mut self, node: NodeId, offset: Vector, f: impl FnOnce(&mut Self)) {
        let factor = self.hint_factor().unwrap_or(1.0);
        let offset = if factor.is_finite() && factor > 0.0 {
            Vector::new(
                (offset.x * factor).round() / factor,
                (offset.y * factor).round() / factor,
            )
        } else {
            offset
        };
        self.mark_recording_at(node, offset);
        self.with_translation(offset, f);
        self.mark_recording(NodeId::STATIC);
    }

    /// Returns the **cumulative** [`Transformation`] currently in effect
    /// (enzICED #46).
    ///
    /// Every [`with_transformation`] / [`with_translation`] scope composes onto
    /// the enclosing one, so this returns the *product* of all the transforms
    /// on the active stack — the transform that will actually be applied to
    /// primitives recorded right now, not just the innermost scope. A getter
    /// returning only the innermost translation reads as correct in every
    /// single-level test and then silently loses ancestor offsets under
    /// nesting, so the cumulative transform is deliberately the contract.
    ///
    /// # Why this exists
    ///
    /// A snapping widget (e.g. a monospaced text grid) rounds its glyph columns
    /// to the pixel grid using absolute coordinates. That snap is only correct
    /// if no fractional transform is applied *after* the widget draws. An
    /// ancestor `with_translation(Vector::new(0.5, 0.25))` shifts the already
    /// snapped columns by a sub-pixel amount and re-introduces the exact blur
    /// the snap exists to prevent. Without a way to read the transform in
    /// effect the widget cannot compensate; with it, the widget can snap in the
    /// coordinate space that will actually be rasterised.
    ///
    /// # Scale interaction
    ///
    /// The returned [`Transformation`] carries **both** translation and scale.
    /// [`Transformation::translation`] gives the accumulated offset (logical
    /// units) and [`Transformation::scale_factor`] the accumulated scale.
    /// [`scale`](Self::scale) is a separate, hint-derived backing scale
    /// (window × application) used for metrics hinting; it is *not* folded into
    /// this transform. To land on a device pixel a caller composes both: the
    /// device position of a logical point `p` is
    /// `(p * current_transformation()) * scale().total()`. Snapping in device
    /// space therefore floors `((p * T).x) * s` and maps back.
    ///
    /// # Trait surface
    ///
    /// This is a **provided** method whose default returns
    /// [`Transformation::IDENTITY`], so it is source-compatible: no existing
    /// external implementor breaks. Every in-tree backend that actually tracks
    /// a transformation stack (`iced_wgpu`, `iced_tiny_skia`, and the
    /// `fallback::Renderer` wrapper) **overrides** it to return the real
    /// cumulative transform. A backend that keeps the default (like the null
    /// `()` renderer, which applies no transforms at all) reports identity,
    /// which is correct for a renderer that never composes a transform. A
    /// backend that *does* compose transforms but forgets to override this
    /// would silently report identity — that is the documented risk of the
    /// default, and the reason both real backends override it here rather than
    /// inheriting the default.
    ///
    /// [`with_transformation`]: Self::with_transformation
    /// [`with_translation`]: Self::with_translation
    fn current_transformation(&self) -> Transformation {
        Transformation::IDENTITY
    }

    /// Fills a [`Quad`] with the provided [`Background`].
    fn fill_quad(&mut self, quad: Quad, background: impl Into<Background>);

    /// Creates an [`image::Allocation`] for the given [`image::Handle`] and calls the given callback with it.
    fn allocate_image(
        &self,
        handle: &image::Handle,
        callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
    );

    /// Provides hints to the [`Renderer`] about the rendering target.
    ///
    /// This may be used internally by the [`Renderer`] to perform optimizations
    /// and/or improve rendering quality.
    ///
    /// For instance, providing a [`Scale`] may be used by some renderers to
    /// perform metrics hinting internally in physical coordinates while keeping
    /// layout coordinates logical and, therefore, maintain linearity.
    fn hint(&mut self, scale: Scale);

    /// Returns the last [`Scale`] provided as a [`hint`](Self::hint).
    fn scale(&self) -> Option<Scale>;

    /// Returns the last hint factor provided as a [`hint`](Self::hint),
    /// only if [`Settings::metrics_hinting`] is enabled.
    fn hint_factor(&self) -> Option<f32> {
        if !self.settings().metrics_hinting {
            return None;
        }

        self.scale().map(Scale::total)
    }

    /// Resets the [`Renderer`] to start drawing in the `new_bounds` from scratch.
    fn reset(&mut self, new_bounds: Rectangle);

    /// Returns the [`Settings`] of this [`Renderer`].
    fn settings(&self) -> Settings;

    /// Polls any concurrent computations that may be pending in the [`Renderer`].
    ///
    /// By default, it does nothing.
    fn tick(&mut self) {}
}

/// A polygon with four sides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// The bounds of the [`Quad`].
    pub bounds: Rectangle,

    /// The [`Border`] of the [`Quad`]. The border is drawn on the inside of the [`Quad`].
    pub border: Border,

    /// The [`Shadow`] of the [`Quad`].
    pub shadow: Shadow,

    /// Whether the [`Quad`] should be snapped to the pixel grid.
    pub snap: bool,
}

impl Default for Quad {
    fn default() -> Self {
        Self {
            bounds: Rectangle::with_size(Size::ZERO),
            border: Border::default(),
            shadow: Shadow::default(),
            snap: CRISP,
        }
    }
}

/// The styling attributes of a [`Renderer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The text color
    pub text_color: Color,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            text_color: Color::BLACK,
        }
    }
}

/// A headless renderer is a renderer that can render offscreen without
/// a window nor a compositor.
pub trait Headless {
    /// Creates a new [`Headless`] renderer;
    fn new(settings: Settings, backend: Option<&str>) -> impl Future<Output = Option<Self>>
    where
        Self: Sized;

    /// Returns the unique name of the renderer.
    ///
    /// This name may be used by testing libraries to uniquely identify
    /// snapshots.
    fn name(&self) -> String;

    /// Draws offscreen into a screenshot, returning a collection of
    /// bytes representing the rendered pixels in RGBA order.
    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8>;
}

/// The settings of a [`Renderer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The default [`Font`] to use.
    pub font: Font,

    /// The default size of text.
    ///
    /// By default, it will be set to `16.0`.
    pub text_size: Pixels,

    /// The default line height of text.
    ///
    /// By default, it will be set to `LineHeight::Relative(1.3)`.
    pub line_height: text::LineHeight,

    /// Whether the [`Renderer`] should perform metrics hinting.
    ///
    /// By default, it is enabled.
    pub metrics_hinting: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font: Font::DEFAULT,
            text_size: Pixels(16.0),
            line_height: text::LineHeight::default(),
            metrics_hinting: true,
        }
    }
}

/// The scale factor of a [`Renderer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    /// The global scale factor of the window.
    ///
    /// This is normally controlled by the OS, and applied globally to all apps.
    pub window: f32,

    /// The local scale factor of the application.
    pub application: f32,
}

impl Scale {
    /// Returns the total scale factor applied by the [`Renderer`].
    ///
    /// This is the product of the window and application scale factors.
    pub fn total(self) -> f32 {
        self.window * self.application
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self {
            window: 1.0,
            application: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    /// A travel's track at any instant is the interpolation of the two
    /// keyframes around it, whichever way they are found: sampling bisects,
    /// and matches a scan of every keyframe.
    ///
    /// Sabotage: pair the found keyframe with itself, or with the one after
    /// it -- the samples between keyframes differ.
    #[test]
    fn a_sampled_travel_interpolates_the_keyframes_around_it() {
        let start = crate::time::Instant::now();
        let node = NodeId(7);
        let keyframes: Vec<(f32, Track)> = [0.0, 0.1, 0.1, 0.35, 0.6, 0.6001, 1.0]
            .into_iter()
            .enumerate()
            .map(|(index, at)| {
                (
                    at,
                    Track {
                        translation: crate::Vector::new(
                            index as f32 * 10.0,
                            (index * index) as f32,
                        ),
                        ..Track::identity(node)
                    },
                )
            })
            .collect();
        let animation = TrackAnimation {
            node,
            start,
            duration: crate::time::Duration::from_millis(1000),
            keyframes: keyframes.clone(),
            generation: 0,
        };
        let scanned = |t: f32| {
            let mut previous = &keyframes[0];
            for keyframe in &keyframes {
                if keyframe.0 >= t {
                    let span = keyframe.0 - previous.0;
                    let local = if span > f32::EPSILON {
                        (t - previous.0) / span
                    } else {
                        1.0
                    };
                    return Track::lerp(&previous.1, &keyframe.1, local);
                }
                previous = keyframe;
            }
            previous.1
        };
        for millis in (0..=1100).step_by(7) {
            let now = start + crate::time::Duration::from_millis(millis);
            let t = animation.progress(now);
            assert_eq!(
                animation.sample(now).map(|track| track.translation),
                Some(scanned(t).translation),
                "at {millis} ms"
            );
        }
    }

    use super::*;
    use crate::image;

    /// A minimal [`Renderer`] that tracks a cumulative transformation stack
    /// exactly the way the real backends (`iced_wgpu`, `iced_tiny_skia`) do:
    /// each pushed transform is composed onto the top of the stack, and
    /// [`current_transformation`] returns the top (enzICED #46).
    ///
    /// This lets the trait contract be exercised through the trait's own
    /// [`with_translation`] / [`with_transformation`] scoping API without
    /// pulling in a GPU/CPU backend.
    ///
    /// [`current_transformation`]: Renderer::current_transformation
    /// [`with_translation`]: Renderer::with_translation
    /// [`with_transformation`]: Renderer::with_transformation
    struct StackRenderer {
        transformations: Vec<Transformation>,
        started_layers: Vec<Rectangle>,
    }

    impl StackRenderer {
        fn new() -> Self {
            Self {
                transformations: vec![Transformation::IDENTITY],
                started_layers: Vec::new(),
            }
        }
    }

    impl Renderer for StackRenderer {
        fn start_layer(&mut self, bounds: Rectangle) {
            self.started_layers.push(bounds);
        }
        fn end_layer(&mut self) {}

        fn start_transformation(&mut self, transformation: Transformation) {
            // Compose onto the current top — the exact semantics of
            // `graphics::layer::Stack::push_transformation`.
            let composed = *self.transformations.last().unwrap() * transformation;
            self.transformations.push(composed);
        }

        fn end_transformation(&mut self) {
            let _ = self.transformations.pop();
        }

        fn current_transformation(&self) -> Transformation {
            *self.transformations.last().unwrap()
        }

        fn fill_quad(&mut self, _quad: Quad, _background: impl Into<Background>) {}

        fn allocate_image(
            &self,
            _handle: &image::Handle,
            _callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
        ) {
        }

        fn hint(&mut self, _scale: Scale) {}
        fn scale(&self) -> Option<Scale> {
            None
        }
        fn reset(&mut self, _new_bounds: Rectangle) {}
        fn settings(&self) -> Settings {
            Settings::default()
        }
    }

    /// The additive alpha seam keeps existing renderer implementations source
    /// compatible and fully opaque by forwarding to their plain layer path.
    #[test]
    fn layer_alpha_default_forwards_to_plain_layer() {
        let mut renderer = StackRenderer::new();
        let bounds = Rectangle {
            x: 3.0,
            y: 5.0,
            width: 40.0,
            height: 20.0,
        };

        renderer.start_layer_alpha(bounds, 0.25);
        renderer.end_layer();

        assert_eq!(renderer.started_layers, vec![bounds]);
    }

    /// A renderer with nothing pushed reports identity — the anchor.
    #[test]
    fn current_transformation_defaults_to_identity() {
        let renderer = StackRenderer::new();
        assert_eq!(renderer.current_transformation(), Transformation::IDENTITY);
    }

    /// Inside a single `with_translation(0.5, 0.25)` the widget reads back
    /// exactly that fractional offset — calibrated against the literal vector,
    /// an absolute anchor, not by inverting the getter.
    #[test]
    fn reads_back_single_fractional_translation() {
        let mut renderer = StackRenderer::new();
        let mut observed = None;

        renderer.with_translation(Vector::new(0.5, 0.25), |r| {
            observed = Some(r.current_transformation().translation());
        });

        assert_eq!(observed, Some(Vector::new(0.5, 0.25)));

        // The scope has ended; the widget outside reads identity again.
        assert_eq!(
            renderer.current_transformation().translation(),
            Vector::new(0.0, 0.0)
        );
    }

    /// The defining case the brief demands: nested translations read back the
    /// **sum**, not the innermost. A getter returning only the innermost would
    /// report (0.125, 0.5) at the deepest point and pass a single-level test
    /// while dropping the ancestor's (0.5, 0.25).
    #[test]
    fn nested_translations_read_back_the_sum() {
        let mut renderer = StackRenderer::new();
        let mut inner = None;
        let mut outer_after_inner = None;

        renderer.with_translation(Vector::new(0.5, 0.25), |r| {
            r.with_translation(Vector::new(0.125, 0.5), |r| {
                inner = Some(r.current_transformation().translation());
            });
            // Back in the outer scope: inner has been popped.
            outer_after_inner = Some(r.current_transformation().translation());
        });

        // Absolute anchor computed by hand: 0.5 + 0.125 = 0.625,
        // 0.25 + 0.5 = 0.75 — the SUM.
        assert_eq!(inner, Some(Vector::new(0.625, 0.75)));
        // And explicitly NOT the innermost.
        assert_ne!(inner, Some(Vector::new(0.125, 0.5)));
        // The outer scope is restored after the inner one closes.
        assert_eq!(outer_after_inner, Some(Vector::new(0.5, 0.25)));
    }

    /// The value is exactly what a snapping widget needs to land on a device
    /// pixel. A glyph at logical-x 12.0 under an ancestor fractional
    /// translation of (0.5, 0.0) is actually rasterised at 12.5; flooring
    /// `12.5 * scale` snaps to a real device column. Ignoring the ancestor
    /// offset would floor 12.0 and drift by half a pixel — the exact blur the
    /// snap exists to prevent.
    #[test]
    fn cumulative_translation_snaps_glyph_to_device_pixel() {
        let mut renderer = StackRenderer::new();
        let logical_glyph_x = 12.0_f32;
        let scale = 2.0_f32; // e.g. a 2x HiDPI backing scale

        let mut snapped_logical = None;

        renderer.with_translation(Vector::new(0.5, 0.0), |r| {
            let offset = r.current_transformation().translation().x;
            // Absolute rasterised position of the glyph.
            let device_x = logical_glyph_x + offset; // 12.5
            assert_eq!(device_x, 12.5);
            // Snap to the device grid, then map back to logical space.
            let snapped_device = (device_x * scale).floor(); // floor(25.0) = 25
            snapped_logical = Some(snapped_device / scale - offset); // 25/2 - 0.5 = 12.0
        });

        // The widget must draw at logical 12.0 to land on device pixel 25.
        assert_eq!(snapped_logical, Some(12.0));
    }
}
