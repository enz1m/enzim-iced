// Modified by enzUI contributors. See FORK.md.
//! Draw and stack layers of graphical primitives.
mod clip_path;
use crate::core::border;
use crate::core::{Rectangle, Transformation};
pub use clip_path::ClipPath;

/// A layer of graphical primitives.
///
/// Layers normally dictate a set of primitives that are
/// rendered in a specific order.
pub trait Layer: Default {
    /// Creates a new [`Layer`] with the given bounds.
    fn with_bounds(bounds: Rectangle) -> Self;

    /// Creates a new [`Layer`] with the given bounds and a rounded clip
    /// `radius` (enzICED #41).
    ///
    /// The default implementation ignores `radius` and falls back to
    /// [`with_bounds`] (rectangular clip). A backend that supports rounded
    /// clipping overrides this to record the radius so its present pass can
    /// mask the corners.
    ///
    /// [`with_bounds`]: Self::with_bounds
    fn with_clip_radius(bounds: Rectangle, radius: border::Radius) -> Self {
        let _ = radius;
        Self::with_bounds(bounds)
    }

    /// Sets the rounded clip `radius` on an already-recycled [`Layer`]
    /// (enzICED #41).
    ///
    /// The [`Stack`] recycles layer allocations across frames, so a layer that
    /// was rectangular last frame can be reused for a rounded clip this frame
    /// (and vice-versa). This is called alongside [`resize`] on reuse; the
    /// default implementation is a no-op for backends without rounded support.
    ///
    /// [`resize`]: Self::resize
    fn set_clip_radius(&mut self, radius: Option<border::Radius>) {
        let _ = radius;
    }

    /// Returns the current rounded clip radius of this [`Layer`], if any
    /// (enzICED #325).
    ///
    /// Used by [`Stack::push_clip`] to inherit the enclosing layer's rounded
    /// clip into a nested plain clip so that the child does not paint outside
    /// the parent's rounded corners.
    fn clip_radius(&self) -> Option<border::Radius> {
        None
    }

    /// Returns the current bounds of the [`Layer`].
    fn bounds(&self) -> Rectangle;

    /// Bounds promoted into retained copies; ordinary layers equal `bounds`.
    fn retained_bounds(&self) -> Rectangle {
        self.bounds()
    }

    /// Rounded radius promoted with [`Self::retained_bounds`].
    fn retained_clip_radius(&self) -> Option<border::Radius> {
        self.clip_radius()
    }

    /// The live presentation clip for an explicitly retained layer.
    ///
    /// `Some` is semantic metadata, not a geometry guess: a replay track may
    /// replace this clip while every ordinary/descendant clip is intersected.
    fn presentation_clip(&self) -> Option<Rectangle> {
        None
    }

    /// Resizes live and retained bounds independently.
    fn resize_retained(&mut self, visible: Rectangle, retained: Rectangle) {
        let _ = retained;
        self.resize(visible);
    }

    /// Stores the independently-derived rounded radii for live and retained
    /// geometry. Backends without retained rounded clips use the live radius.
    fn set_clip_radii(
        &mut self,
        visible: Option<border::Radius>,
        retained: Option<border::Radius>,
    ) {
        let _ = retained;
        self.set_clip_radius(visible);
    }

    /// Marks this layer as the replaceable presentation boundary of an
    /// explicitly retained clip. `None` means an ordinary fixed clip.
    fn set_presentation_clip(&mut self, presentation: Option<Rectangle>) {
        let _ = presentation;
    }

    /// Marks or clears fixed replay-clip metadata.
    fn set_fixed_replay_clip(&mut self, fixed: bool) {
        let _ = fixed;
    }

    /// The clip the ancestors of a presentation boundary opened, which its
    /// retained ink is recorded past and replay applies after moving it.
    fn enclosing_clip(&self) -> Option<Rectangle> {
        None
    }

    /// Stores [`Self::enclosing_clip`].
    fn set_enclosing_clip(&mut self, enclosing: Option<Rectangle>) {
        let _ = enclosing;
    }

    /// Declared clipping sources while capturing responsive paint; absent during ordinary drawing.
    fn semantic_clip(&self) -> Option<std::sync::Arc<ClipPath>> {
        None
    }

    /// Retains source identity across continuation layers without inferring it from equal bounds.
    fn set_semantic_clip(&mut self, _clip: Option<std::sync::Arc<ClipPath>>) {}

    /// Flushes and settles any pending group of primitives in the [`Layer`].
    ///
    /// This will be called when a [`Layer`] is finished. It allows layers to efficiently
    /// record primitives together and defer grouping until the end.
    fn flush(&mut self);

    /// Resizes the [`Layer`] to the given bounds.
    fn resize(&mut self, bounds: Rectangle);

    /// Clears all the layers contents and resets its bounds.
    fn reset(&mut self);

    /// Returns the start level of the [`Layer`].
    ///
    /// A level is a "sublayer" index inside of a [`Layer`].
    ///
    /// A [`Layer`] may draw multiple primitive types in a certain order.
    /// The level represents the lowest index of the primitive types it
    /// contains.
    ///
    /// Two layers A and B can therefore be merged if they have the same bounds,
    /// and the end level of A is lower or equal than the start level of B.
    fn start(&self) -> usize;

    /// Returns the end level of the [`Layer`].
    fn end(&self) -> usize;

    /// Returns whether this layer may be merged with `other`.
    ///
    /// Backends that retain semantic group boundaries can reject a merge even
    /// when primitive levels and clip bounds would otherwise allow it.
    fn can_merge(&self, _other: &Self) -> bool {
        true
    }

    /// Whether fragments belong to one native primitive batch, rather than merely equal clip bounds.
    fn same_native_batch(&self, _other: &Self) -> bool {
        false
    }

    /// Drops invisible drawing data without replacing the layer's clip provenance.
    fn discard_invisible_paint(&mut self) {}

    /// Merges a [`Layer`] with the current one.
    fn merge(&mut self, _layer: &mut Self);
}

/// A stack of layers used for drawing.
#[derive(Debug, Clone)]
pub struct Stack<T: Layer> {
    layers: Vec<T>,
    transformations: Vec<Transformation>,
    previous: Vec<usize>,
    current: usize,
    active_count: usize,
}

impl<T: Layer> Stack<T> {
    /// Creates a new empty [`Stack`].
    pub fn new() -> Self {
        Self {
            layers: vec![T::default()],
            transformations: vec![Transformation::IDENTITY],
            previous: vec![],
            current: 0,
            active_count: 1,
        }
    }

    /// Returns a mutable reference to the current [`Layer`] of the [`Stack`], together with
    /// the current [`Transformation`].
    #[inline]
    pub fn current_mut(&mut self) -> (&mut T, Transformation) {
        let transformation = self.transformation();

        (&mut self.layers[self.current], transformation)
    }

    /// Returns the current [`Transformation`] of the [`Stack`].
    #[inline]
    pub fn transformation(&self) -> Transformation {
        self.transformations.last().copied().unwrap()
    }

    /// Pushes a new clipping region in the [`Stack`]; creating a new layer in the
    /// process.
    ///
    /// The new rectangular clip is intersected with the enclosing layer's
    /// bounds. If the enclosing layer carries a rounded clip radius, the new
    /// layer also inherits an adjusted version of it so that the child does not
    /// paint outside the parent's rounded corners (enzICED #325/#1442).
    pub fn push_clip(&mut self, bounds: Rectangle) {
        self.push_clip_with_transform(bounds, self.transformation());
    }

    /// Opens a clip already expressed in viewport coordinates; primitive transforms are unchanged.
    pub fn push_clip_viewport(&mut self, bounds: Rectangle) {
        self.push_clip_with_transform(bounds, Transformation::IDENTITY);
    }

    fn push_clip_with_transform(&mut self, bounds: Rectangle, transformation: Transformation) {
        let semantic = self.layers[self.current].semantic_clip();
        // Capture the enclosing layer before advancing `self.current`.
        let parent_clip = self.layers[self.current].clip_radius();
        let parent_retained_clip = self.layers[self.current].retained_clip_radius();
        let parent_bounds = self.layers[self.current].bounds();
        let parent_retained = self.layers[self.current].retained_bounds();
        let requested_bounds = bounds * transformation;
        let bounds = cumulative_clip_bounds(parent_bounds, requested_bounds);
        let retained = cumulative_clip_bounds(parent_retained, requested_bounds);

        self.previous.push(self.current);

        self.current = self.active_count;
        self.active_count += 1;

        if self.current == self.layers.len() {
            self.layers.push(T::with_bounds(bounds));
        } else {
            self.layers[self.current].resize(bounds);
        }
        self.layers[self.current].resize_retained(bounds, retained);

        // Recycled layers must discard inherited radii when their clip becomes empty.
        let inherited = nonempty(bounds)
            .then_some(parent_clip)
            .flatten()
            .and_then(|r| inherited_clip_radius(parent_bounds, r, bounds));
        let retained_inherited = nonempty(retained)
            .then_some(parent_retained_clip)
            .flatten()
            .and_then(|r| inherited_clip_radius(parent_retained, r, retained));
        self.layers[self.current].set_clip_radii(inherited, retained_inherited);
        self.layers[self.current].set_presentation_clip(None);
        self.layers[self.current].set_fixed_replay_clip(false);
        self.layers[self.current].set_semantic_clip(
            semantic.map(|parent| ClipPath::push(&parent, requested_bounds, None)),
        );
    }

    /// Pushes a live clip while preserving a wider retained-ink clip (#2219).
    pub fn push_clip_retained(&mut self, visible: Rectangle, retained: Rectangle) {
        let semantic = self.layers[self.current].semantic_clip();
        let requested_bounds = retained * self.transformation();
        let parent_visible = self.layers[self.current].bounds();
        let parent_retained = self.layers[self.current].retained_bounds();
        let parent_visible_radius = self.layers[self.current].clip_radius();
        let parent_retained_radius = self.layers[self.current].retained_clip_radius();
        let transform = self.transformation();
        let visible = cumulative_clip_bounds(parent_visible, visible * transform);
        // Retained ink is not cut by the ancestors' clips: the node may move
        // under them at replay, which applies the enclosing clip after the
        // move. An entering slide keeps the part still outside the window.
        let retained = retained * transform;
        let visible_radius = nonempty(visible)
            .then_some(parent_visible_radius)
            .flatten()
            .and_then(|radius| inherited_clip_radius(parent_visible, radius, visible));
        let retained_radius = (nonempty(retained)
            && cumulative_clip_bounds(parent_retained, retained) == retained)
            .then_some(parent_retained_radius)
            .flatten()
            .and_then(|radius| inherited_clip_radius(parent_retained, radius, retained));

        self.previous.push(self.current);
        self.current = self.active_count;
        self.active_count += 1;
        if self.current == self.layers.len() {
            self.layers.push(T::with_bounds(visible));
        }
        self.layers[self.current].resize_retained(visible, retained);
        self.layers[self.current].set_clip_radii(visible_radius, retained_radius);
        self.layers[self.current].set_presentation_clip(Some(visible));
        self.layers[self.current]
            .set_enclosing_clip(finite(parent_retained).then_some(parent_retained));
        self.layers[self.current].set_fixed_replay_clip(false);
        self.layers[self.current].set_semantic_clip(
            semantic.map(|parent| ClipPath::push(&parent, requested_bounds, None)),
        );
    }

    /// Pushes a new **rounded** clipping region in the [`Stack`], rounding
    /// `bounds` by `radius`; creating a new layer in the process (enzICED #41).
    ///
    /// The rounded-corner counterpart of [`push_clip`]. Like it, the requested
    /// bounds are intersected with the enclosing clip. The radius is
    /// transformed with the current [`Transformation`] and adjusted when an
    /// ancestor crops away one of the requested rounded edges.
    ///
    /// [`push_clip`]: Self::push_clip
    pub fn push_clip_rounded(&mut self, bounds: Rectangle, radius: border::Radius) {
        self.push_clip_rounded_with_transform(bounds, radius, self.transformation());
    }

    /// Opens a rounded clip in viewport coordinates without transforming its bounds or radius again.
    pub fn push_clip_rounded_viewport(&mut self, bounds: Rectangle, radius: border::Radius) {
        self.push_clip_rounded_with_transform(bounds, radius, Transformation::IDENTITY);
    }

    fn push_clip_rounded_with_transform(
        &mut self,
        bounds: Rectangle,
        radius: border::Radius,
        transformation: Transformation,
    ) {
        let semantic = self.layers[self.current].semantic_clip();
        let parent_clip = self.layers[self.current].clip_radius();
        let parent_retained_clip = self.layers[self.current].retained_clip_radius();
        let parent_bounds = self.layers[self.current].bounds();
        let parent_retained = self.layers[self.current].retained_bounds();
        let requested_bounds = bounds * transformation;
        let bounds = cumulative_clip_bounds(parent_bounds, requested_bounds);
        let retained = cumulative_clip_bounds(parent_retained, requested_bounds);
        // Corner radii scale with the layer (same convention the backends use
        // for quad border radii under a scale transform).
        let requested_radius = radius * transformation.scale_factor();
        let own_radius = nonempty(bounds)
            .then(|| inherited_clip_radius(requested_bounds, requested_radius, bounds))
            .flatten();
        let inherited_radius = nonempty(bounds)
            .then_some(parent_clip)
            .flatten()
            .and_then(|r| inherited_clip_radius(parent_bounds, r, bounds));
        let radius = intersected_clip_radius(own_radius, inherited_radius);
        let retained_own_radius = nonempty(retained)
            .then(|| inherited_clip_radius(requested_bounds, requested_radius, retained))
            .flatten();
        let retained_inherited_radius = nonempty(retained)
            .then_some(parent_retained_clip)
            .flatten()
            .and_then(|r| inherited_clip_radius(parent_retained, r, retained));
        let retained_radius =
            intersected_clip_radius(retained_own_radius, retained_inherited_radius);

        self.previous.push(self.current);

        self.current = self.active_count;
        self.active_count += 1;

        if self.current == self.layers.len() {
            match radius {
                Some(radius) => self.layers.push(T::with_clip_radius(bounds, radius)),
                None => self.layers.push(T::with_bounds(bounds)),
            }
        } else {
            self.layers[self.current].resize(bounds);
        }
        self.layers[self.current].resize_retained(bounds, retained);
        self.layers[self.current].set_clip_radii(radius, retained_radius);
        self.layers[self.current].set_presentation_clip(None);
        self.layers[self.current].set_fixed_replay_clip(false);
        self.layers[self.current].set_semantic_clip(
            semantic
                .map(|parent| ClipPath::push(&parent, requested_bounds, Some(requested_radius))),
        );
    }

    /// Pops the current clipping region from the [`Stack`] and restores the previous one.
    ///
    /// The current layer will be recorded for drawing.
    pub fn pop_clip(&mut self) {
        self.flush();

        self.current = self.previous.pop().unwrap();
    }

    /// Starts a continuation layer at the current clip without opening a new
    /// clip scope.
    ///
    /// A retained compositing boundary must remain ordered between primitives
    /// recorded before and after it. Reusing the parent layer after such a
    /// boundary would batch both sides together and paint them both before the
    /// child layer. Backends with retained groups call this after closing the
    /// group so subsequent primitives receive a later layer index.
    pub fn split_current(&mut self) {
        self.flush();
        let semantic = self.layers[self.current].semantic_clip();
        let bounds = self.layers[self.current].bounds();
        let retained = self.layers[self.current].retained_bounds();
        let radius = self.layers[self.current].clip_radius();
        let retained_radius = self.layers[self.current].retained_clip_radius();
        let presentation_clip = self.layers[self.current].presentation_clip();
        let enclosing_clip = self.layers[self.current].enclosing_clip();

        self.current = self.active_count;
        self.active_count += 1;

        if self.current == self.layers.len() {
            match radius {
                Some(radius) => self.layers.push(T::with_clip_radius(bounds, radius)),
                None => self.layers.push(T::with_bounds(bounds)),
            }
        } else {
            self.layers[self.current].resize(bounds);
            self.layers[self.current].set_clip_radius(radius);
        }
        self.layers[self.current].resize_retained(bounds, retained);
        self.layers[self.current].set_clip_radii(radius, retained_radius);
        self.layers[self.current].set_presentation_clip(presentation_clip);
        self.layers[self.current].set_enclosing_clip(enclosing_clip);
        self.layers[self.current].set_semantic_clip(semantic);
    }

    /// Starts later paint at a new allocation identity with the same declared clipping sources.
    pub fn split_clip_continuation(&mut self) {
        self.split_current();
        if let Some(clip) = self.layers[self.current].semantic_clip() {
            self.layers[self.current].set_semantic_clip(Some(clip.next_allocation()));
        }
    }

    /// Marks the current continuation as the fixed enclosing clip of a scene
    /// segment. Descendant clips opened later clear this bit on creation.
    pub fn mark_current_fixed_replay_clip(&mut self) {
        self.layers[self.current].set_fixed_replay_clip(true);
    }

    /// The number of layers currently active in the [`Stack`] (enzICED #2219).
    ///
    /// A retained scene brackets a recording with this: every layer produced
    /// between two reads belongs to the recorded subtree.
    pub fn active_len(&self) -> usize {
        self.active_count
    }

    /// The clip bounds of the active layer at `index` (enzICED #2219).
    ///
    /// A retained scene records the clip that was in effect when a node opened,
    /// so a replay can tell that fixed ancestor clip from a clip a descendant
    /// opened inside the node -- only the latter travels with the node.
    pub fn clip_bounds_at(&self, index: usize) -> Option<Rectangle> {
        (index < self.active_count).then(|| self.layers[index].bounds())
    }

    /// Clone the active layers in `range` so they can be **retained** past this
    /// frame and replayed later (enzICED #2219).
    ///
    /// The caller is responsible for bracketing the range with a clip scope, so
    /// the captured layers are self-contained: primitives recorded before the
    /// scope opened live in an earlier layer and are not captured.
    pub fn capture(&self, range: std::ops::Range<usize>) -> Vec<T>
    where
        T: Clone,
    {
        let end = range.end.min(self.active_count);
        if range.start >= end {
            return Vec::new();
        }

        self.layers[range.start..end].to_vec()
    }

    /// Append previously [`capture`](Self::capture)d layers as new active
    /// layers, then continue recording in a fresh layer (enzICED #2219).
    ///
    /// This is the replay half: no widget is asked to draw again, the recorded
    /// primitives are simply re-submitted in order. Drawing continues in a
    /// split layer for the same reason [`split_current`](Self::split_current)
    /// exists — later primitives must keep a later layer index.
    pub fn append(&mut self, recorded: Vec<T>) {
        if recorded.is_empty() {
            return;
        }

        self.flush();

        for layer in recorded {
            let slot = self.active_count;
            self.active_count += 1;

            if slot == self.layers.len() {
                self.layers.push(layer);
            } else {
                self.layers[slot] = layer;
            }
        }

        self.split_current();
    }

    /// Pushes a new [`Transformation`] in the [`Stack`].
    ///
    /// Future drawing operations will be affected by this new [`Transformation`] until
    /// it is popped using [`pop_transformation`].
    ///
    /// [`pop_transformation`]: Self::pop_transformation
    pub fn push_transformation(&mut self, transformation: Transformation) {
        self.transformations
            .push(self.transformation() * transformation);
    }

    /// Pops the current [`Transformation`] in the [`Stack`].
    pub fn pop_transformation(&mut self) {
        let _ = self.transformations.pop();
    }

    /// Returns an iterator over immutable references to the layers in the [`Stack`].
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.layers[..self.active_count].iter()
    }

    /// Returns mutable references to the active layers in drawing order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.layers[..self.active_count].iter_mut()
    }

    /// Returns mutable references to the active layers in `range`.
    pub fn range_mut(&mut self, range: std::ops::Range<usize>) -> &mut [T] {
        let end = range.end.min(self.active_count);
        &mut self.layers[range.start.min(end)..end]
    }

    /// Returns the slice of layers in the [`Stack`].
    pub fn as_slice(&self) -> &[T] {
        &self.layers[..self.active_count]
    }

    /// Narrows queued display layers without reducing their immutable recording coverage.
    pub fn clip_display_range(&mut self, range: std::ops::Range<usize>, clip: Rectangle) {
        for layer in &mut self.layers[range] {
            let visible = cumulative_clip_bounds(layer.bounds(), clip);
            layer.resize_retained(visible, layer.retained_bounds());
        }
    }

    /// Flushes and settles any primitives in the [`Stack`].
    pub fn flush(&mut self) {
        self.layers[self.current].flush();
    }

    /// Flushes pending primitives in every active layer.
    ///
    /// A recording boundary can be opened while a nested layer is current.
    /// Pending primitives in an earlier sibling then live in another active
    /// layer, so flushing only [`Self::current_mut`] would leave them outside
    /// the immutable batches captured for retained replay.
    pub fn flush_active(&mut self) {
        for layer in &mut self.layers[..self.active_count] {
            layer.flush();
        }
    }

    /// Groups recorded segments by clip allocation, preserving order within each allocation.
    ///
    /// Frames without declared clips are unchanged. Post-clip paint must use
    /// [`Self::split_clip_continuation`] to remain separate from earlier paint.
    pub fn canonicalize_recorded_z_order(&mut self) {
        use std::collections::HashMap;
        use std::sync::Arc;

        let active = self.active_count;
        let mut allocation: HashMap<*const ClipPath, usize> = HashMap::new();
        for (index, layer) in self.layers[..active].iter().enumerate() {
            if let Some(clip) = layer.semantic_clip() {
                let _ = allocation
                    .entry(Arc::as_ptr(ClipPath::paint_allocation(&clip)))
                    .or_insert(index);
            }
        }
        if allocation.is_empty() {
            return;
        }

        let mut keyed: Vec<(usize, usize)> = (0..active)
            .map(|index| {
                let key = self.layers[index].semantic_clip().map_or(index, |clip| {
                    allocation[&Arc::as_ptr(ClipPath::paint_allocation(&clip))]
                });
                (key, index)
            })
            .collect();
        keyed.sort_unstable();
        if keyed.iter().enumerate().all(|(new, &(_, old))| new == old) {
            return;
        }

        let mut relocated = vec![0; active];
        for (new, &(_, old)) in keyed.iter().enumerate() {
            relocated[old] = new;
        }
        self.current = relocated[self.current];
        for previous in &mut self.previous {
            *previous = relocated[*previous];
        }

        let mut slots: Vec<Option<T>> = self
            .layers
            .splice(0..active, std::iter::empty())
            .map(Some)
            .collect();
        let reordered: Vec<T> = keyed
            .iter()
            .map(|&(_, old)| slots[old].take().expect("each active layer moved once"))
            .collect();
        let _ = self.layers.splice(0..0, reordered);
    }

    /// Performs layer merging wherever possible.
    ///
    /// Flushes and settles any primitives in the [`Stack`].
    pub fn merge(&mut self) {
        self.flush();
        for layer in &mut self.layers[..self.active_count] {
            let bounds = layer.bounds();
            if bounds.width == 0.0 || bounds.height == 0.0 {
                layer.discard_invisible_paint();
            }
        }
        self.canonicalize_recorded_z_order();

        // These are the layers left to process
        let mut left = self.active_count;

        // There must be at least 2 or more layers to merge
        while left > 1 {
            // We set our target as the topmost layer left to process
            let mut current = left - 1;
            let mut target = &self.layers[current];
            let mut target_start = target.start();
            let mut target_index = current;

            // We scan downwards for a contiguous block of mergeable layer candidates
            while current > 0 {
                current -= 1;

                let candidate = &self.layers[current];
                let start = candidate.start();
                let end = candidate.end();

                // We skip empty layers
                if end == 0 {
                    continue;
                }

                // Candidate can be merged if primitive sublayers do not overlap with
                // previous targets and the clipping bounds match
                if (end > target_start && !candidate.same_native_batch(target))
                    || candidate.bounds() != target.bounds()
                    || !candidate.can_merge(target)
                {
                    break;
                }

                // Candidate is not empty and can be merged into
                target = candidate;
                target_start = start;
                target_index = current;
            }

            // We merge all the layers scanned into the target
            //
            // Since we use `target_index` instead of `current`, we
            // deliberately avoid merging into empty layers.
            //
            // If no candidates were mergeable, this is a no-op.
            let (head, tail) = self.layers.split_at_mut(target_index + 1);
            let layer = &mut head[target_index];

            for middle in &mut tail[0..left - target_index - 1] {
                layer.merge(middle);
            }

            // An unmerged barrier candidate still needs to be considered as a target.
            left = if target_index > current {
                current + 1
            } else {
                current
            };
        }
    }

    /// Clears the layers of the [`Stack`], allowing reuse.
    ///
    /// It resizes the base layer bounds to the `new_bounds`.
    ///
    /// This will normally keep layer allocations for future drawing operations.
    pub fn reset(&mut self, new_bounds: Rectangle) {
        for layer in self.layers[..self.active_count].iter_mut() {
            layer.reset();
        }

        self.layers[0].resize(new_bounds);
        self.current = 0;
        self.active_count = 1;
        self.previous.clear();
    }
}

impl<T: Layer> Default for Stack<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Intersect a requested clip with its enclosing layer.
///
/// A disjoint or degenerate intersection is represented by a zero-area
/// rectangle, never by the requested rectangle and never by an absent clip.
/// Both renderers skip a zero-area layer, which makes a fully clipped child
/// honestly paint nothing (enzUI-ICED #1442).
fn cumulative_clip_bounds(parent: Rectangle, requested: Rectangle) -> Rectangle {
    parent.intersection(&requested).unwrap_or(Rectangle {
        x: parent.x.max(requested.x),
        y: parent.y.max(requested.y),
        width: 0.0,
        height: 0.0,
    })
}

fn finite(bounds: Rectangle) -> bool {
    [bounds.x, bounds.y, bounds.width, bounds.height]
        .iter()
        .all(|value| value.is_finite())
}

fn nonempty(bounds: Rectangle) -> bool {
    bounds.width > 0.0 && bounds.height > 0.0
}

/// Intersect two rounded rectangles that already share the same bounds.
///
/// At each corner, the larger radius is the more restrictive clip. Taking the
/// component-wise maximum therefore preserves both the requested rounded clip
/// and any rounded ancestor after their rectangular bounds are intersected.
fn intersected_clip_radius(
    own: Option<border::Radius>,
    inherited: Option<border::Radius>,
) -> Option<border::Radius> {
    match (own, inherited) {
        (Some(own), Some(inherited)) => Some(border::Radius {
            top_left: own.top_left.max(inherited.top_left),
            top_right: own.top_right.max(inherited.top_right),
            bottom_right: own.bottom_right.max(inherited.bottom_right),
            bottom_left: own.bottom_left.max(inherited.bottom_left),
        }),
        (Some(radius), None) | (None, Some(radius)) => Some(radius),
        (None, None) => None,
    }
}

/// Computes the effective rounded clip radius for a child layer whose
/// rectangular clip `child` sits inside a parent with `parent` bounds and
/// `parent_radius` (enzICED #325).
///
/// Returns `Some(radius)` when at least one corner retains a non-zero
/// radius, or `None` if the child is entirely outside the parent's rounded
/// corners (i.e. a plain rectangular clip suffices).
///
/// ## Approximation semantics
///
/// The exact intersection of a rectangle with a rounded rectangle is not
/// representable as a single `(bounds, Radius)` pair in general — the
/// result can be a complex path with straight and curved segments.  We use
/// a conservative per-corner approximation:
///
/// For each corner, the parent's arc is centred at distance `r` from both
/// edges.  The child's inset from the *same* corner of the parent is
/// `(dx, dy)`.  The effective child radius is
/// `max(0, r − max(dx, dy))`.
///
/// ### It never under-clips (the safety property)
///
/// This is the property the patch exists for: no pixel outside the
/// parent's arc can ever leak through.  Writing `m = max(dx, dy)` and
/// `r' = max(0, r − m)`, every point of the child's inherited arc is at
/// distance at most `|dx − m| + r' ≤ m + r' = r` from the parent's arc
/// centre, so the child's clip edge always lies on or inside the parent's.
/// Measured at **0.000000 px** of under-clip across 23,409 (radius, dx,
/// dy) combinations, and at **0 leaked pixels** rendering through this
/// backend (enzICED #328).
///
/// ### It does over-clip, and by more than a sliver
///
/// The cost of that safety is that the child loses some of its own corner.
/// The exact over-clip — how far the child's arc is pulled inside the
/// parent's — is
///
/// ```text
/// over_clip = r − √((dx − dy)² + (r − m)²)     for m < r
/// over_clip = 0                                 for m ≥ r
/// ```
///
/// which is bounded by **`max(dx, dy)`**, i.e. by the inset itself, with
/// equality exactly when `dx == dy`.  It is *not* governed by the
/// asymmetry `|dx − dy|`: at `dx == dy == 30` under `r == 60` the
/// asymmetry is zero and the over-clip is a full **30 px**.
///
/// The formula is **geometrically exact only when the child is not inset
/// at all** (`dx == dy == 0`), or when it clears the corner entirely
/// (`m ≥ r`, where the corner is square anyway).  Sharing a single edge is
/// *not* sufficient: `dx == 0, dy == 8` under `r == 60` over-clips by
/// 7.39 px.  Sharing an edge does keep the error smaller than the
/// symmetric case for the same inset, which is why the motivating case —
/// a scrollable spanning the full width of a rounded card — looks clean:
/// with `dx == 0` and a typical card radius of 8–16 px, the error is
/// ≈0.93–4.69 px and lands in the corner region the card's own border
/// already covers.
///
/// **Known limitation:** a deeply inset child inside a large-radius parent
/// visibly loses its own corners.  That is a cosmetic defect and is the
/// deliberate trade: over-clipping is cosmetic, under-clipping is the bug
/// this function exists to prevent, so the conservative direction is the
/// correct one.  Do not "improve" this formula in a way that ever
/// under-clips.
fn inherited_clip_radius(
    parent: Rectangle,
    parent_radius: border::Radius,
    child: Rectangle,
) -> Option<border::Radius> {
    // Insets of the child from each edge of the parent (clamped to ≥ 0).
    let left = (child.x - parent.x).max(0.0);
    let top = (child.y - parent.y).max(0.0);
    let right = ((parent.x + parent.width) - (child.x + child.width)).max(0.0);
    let bottom = ((parent.y + parent.height) - (child.y + child.height)).max(0.0);

    let tl = (parent_radius.top_left - left.max(top)).max(0.0);
    let tr = (parent_radius.top_right - right.max(top)).max(0.0);
    let br = (parent_radius.bottom_right - right.max(bottom)).max(0.0);
    let bl = (parent_radius.bottom_left - left.max(bottom)).max(0.0);

    if tl == 0.0 && tr == 0.0 && br == 0.0 && bl == 0.0 {
        return None;
    }

    Some(border::Radius {
        top_left: tl,
        top_right: tr,
        bottom_right: br,
        bottom_left: bl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vector;

    /// Minimal [`Layer`] used only to exercise the [`Stack`]'s transformation
    /// bookkeeping (enzICED #46). It carries nothing but its own bounds — the
    /// transformation stack is owned by the [`Stack`], not the layer.
    struct TestLayer {
        bounds: Rectangle,
        retained_bounds: Rectangle,
        clip_radius: Option<border::Radius>,
        retained_radius: Option<border::Radius>,
        presentation: Option<Rectangle>,
        fixed: bool,
    }

    impl Default for TestLayer {
        fn default() -> Self {
            Self {
                bounds: Rectangle::INFINITE,
                retained_bounds: Rectangle::INFINITE,
                clip_radius: None,
                retained_radius: None,
                presentation: None,
                fixed: false,
            }
        }
    }

    impl Layer for TestLayer {
        fn with_bounds(bounds: Rectangle) -> Self {
            Self {
                bounds,
                retained_bounds: bounds,
                clip_radius: None,
                retained_radius: None,
                presentation: None,
                fixed: false,
            }
        }

        fn with_clip_radius(bounds: Rectangle, radius: border::Radius) -> Self {
            Self {
                bounds,
                retained_bounds: bounds,
                clip_radius: Some(radius),
                retained_radius: Some(radius),
                presentation: None,
                fixed: false,
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
            self.retained_radius
        }

        fn presentation_clip(&self) -> Option<Rectangle> {
            self.presentation
        }

        fn resize_retained(&mut self, visible: Rectangle, retained: Rectangle) {
            self.bounds = visible;
            self.retained_bounds = retained;
        }

        fn set_clip_radii(
            &mut self,
            visible: Option<border::Radius>,
            retained: Option<border::Radius>,
        ) {
            self.clip_radius = visible;
            self.retained_radius = retained;
        }

        fn set_presentation_clip(&mut self, presentation: Option<Rectangle>) {
            self.presentation = presentation;
        }

        fn set_fixed_replay_clip(&mut self, fixed: bool) {
            self.fixed = fixed;
        }

        fn flush(&mut self) {}

        fn resize(&mut self, bounds: Rectangle) {
            self.bounds = bounds;
            self.retained_bounds = bounds;
            self.presentation = None;
            self.fixed = false;
        }

        fn reset(&mut self) {
            self.bounds = Rectangle::default();
            self.retained_bounds = Rectangle::default();
            self.clip_radius = None;
            self.retained_radius = None;
            self.presentation = None;
            self.fixed = false;
        }

        fn start(&self) -> usize {
            0
        }

        fn end(&self) -> usize {
            0
        }

        fn merge(&mut self, _layer: &mut Self) {}
    }

    #[test]
    fn nested_clips_use_the_rectangular_intersection_and_preserve_radius() {
        let mut stack: Stack<TestLayer> = Stack::new();
        let outer = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 18.0,
            height: 50.0,
        };
        let resting_child = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 220.0,
            height: 80.0,
        };

        stack.push_clip(outer);
        stack.push_clip_rounded(resting_child, border::Radius::new(6.0));

        let nested = &stack.as_slice()[2];
        assert_eq!(nested.bounds, outer);
        assert_eq!(
            nested.clip_radius,
            Some(border::Radius {
                top_left: 6.0,
                top_right: 0.0,
                bottom_right: 0.0,
                bottom_left: 0.0,
            }),
            "cropping must preserve the child's original top-left arc without \
             inventing rounded edges at the ancestor's moving right/bottom cut"
        );
    }

    #[test]
    fn disjoint_and_degenerate_nested_clips_become_empty_not_unclipped() {
        let mut stack: Stack<TestLayer> = Stack::new();
        stack.push_clip(Rectangle {
            x: 10.0,
            y: 20.0,
            width: 18.0,
            height: 50.0,
        });

        for child in [
            Rectangle {
                x: 100.0,
                y: 20.0,
                width: 40.0,
                height: 50.0,
            },
            Rectangle {
                x: 28.0,
                y: 20.0,
                width: 40.0,
                height: 50.0,
            },
            Rectangle {
                x: 12.0,
                y: 25.0,
                width: -1.0,
                height: 10.0,
            },
        ] {
            stack.push_clip_rounded(child, border::Radius::new(8.0));
            let nested = stack.as_slice().last().expect("nested layer");
            assert_eq!(nested.bounds.width, 0.0, "child={child:?}");
            assert_eq!(nested.bounds.height, 0.0, "child={child:?}");
            assert_eq!(nested.clip_radius, None, "child={child:?}");
            stack.pop_clip();
        }
    }

    #[test]
    fn retained_clip_metadata_propagates_and_recycled_layers_clear_it() {
        let mut stack: Stack<TestLayer> = Stack::new();
        let visible = Rectangle::new((0.0, 0.0).into(), crate::core::Size::new(20.0, 10.0));
        let retained = Rectangle::new((0.0, 0.0).into(), crate::core::Size::new(100.0, 80.0));
        stack.push_clip_retained(visible, retained);
        let presentation = &stack.as_slice()[1];
        assert_eq!(presentation.bounds, visible);
        assert_eq!(presentation.retained_bounds, retained);
        assert_eq!(presentation.presentation, Some(visible));

        let far = Rectangle::new((60.0, 40.0).into(), crate::core::Size::new(20.0, 20.0));
        stack.push_clip_rounded(far, border::Radius::new(7.0));
        let descendant = &stack.as_slice()[2];
        assert_eq!(
            descendant.bounds.width, 0.0,
            "live descendant must be clipped"
        );
        assert_eq!(
            descendant.retained_bounds, far,
            "retained descendant must survive"
        );
        assert_eq!(
            descendant.presentation, None,
            "descendant is never replaceable"
        );
        assert_eq!(descendant.retained_radius, Some(border::Radius::new(7.0)));

        stack.pop_clip();
        stack.pop_clip();
        stack.reset(Rectangle::with_size(crate::core::Size::new(50.0, 50.0)));
        stack.push_clip(Rectangle::with_size(crate::core::Size::new(30.0, 30.0)));
        let recycled = &stack.as_slice()[1];
        assert_eq!(
            recycled.presentation, None,
            "recycled presentation metadata leaked"
        );
        assert_eq!(recycled.retained_bounds, recycled.bounds);
        assert_eq!(recycled.retained_radius, recycled.clip_radius);
    }

    /// The bare stack reports the identity transform. This is the anchor:
    /// nothing pushed means no offset.
    #[test]
    fn empty_stack_is_identity() {
        let stack: Stack<TestLayer> = Stack::new();
        assert_eq!(stack.transformation(), Transformation::IDENTITY);
        assert_eq!(stack.transformation().translation(), Vector::new(0.0, 0.0));
    }

    /// A single pushed translation reads back exactly, calibrated against the
    /// literal vector — not by round-tripping the getter through a setter.
    #[test]
    fn single_translation_reads_back_exactly() {
        let mut stack: Stack<TestLayer> = Stack::new();
        stack.push_transformation(Transformation::translate(0.5, 0.25));

        // Absolute anchor: the exact literal we pushed, computed here, not
        // recovered from the stack.
        assert_eq!(stack.transformation().translation(), Vector::new(0.5, 0.25));

        stack.pop_transformation();
        assert_eq!(stack.transformation(), Transformation::IDENTITY);
    }

    /// The defining case: two nested translations must compose to their
    /// **sum**, not the innermost. A getter that returned only the innermost
    /// would read `(0.125, 0.5)` here and pass every single-level test while
    /// silently dropping the ancestor offset.
    #[test]
    fn nested_translations_sum_not_innermost() {
        let mut stack: Stack<TestLayer> = Stack::new();

        stack.push_transformation(Transformation::translate(0.5, 0.25));
        assert_eq!(stack.transformation().translation(), Vector::new(0.5, 0.25));

        stack.push_transformation(Transformation::translate(0.125, 0.5));

        // Absolute anchor computed by hand: 0.5 + 0.125 = 0.625,
        // 0.25 + 0.5 = 0.75. This is the SUM, and it is NOT the innermost
        // (0.125, 0.5).
        let cumulative = stack.transformation().translation();
        assert_eq!(cumulative, Vector::new(0.625, 0.75));
        assert_ne!(cumulative, Vector::new(0.125, 0.5), "must not be innermost");

        // Popping the inner scope restores the outer cumulative value.
        stack.pop_transformation();
        assert_eq!(stack.transformation().translation(), Vector::new(0.5, 0.25));
    }

    /// A snapping widget needs the cumulative transform to land on a device
    /// pixel. Anchor: a glyph logical-x of 10.0 under a fractional ancestor
    /// translation of 0.25 must be observed at absolute 10.25 — the position
    /// that will actually be rasterised — so flooring `10.25 * scale` snaps
    /// correctly. If the getter dropped the ancestor offset the widget would
    /// floor 10.0 and drift by 0.25 px.
    #[test]
    fn cumulative_offset_places_glyph_on_device_pixel() {
        let mut stack: Stack<TestLayer> = Stack::new();
        stack.push_transformation(Transformation::translate(0.25, 0.0));

        let logical_glyph_x = 10.0_f32;
        let observed = logical_glyph_x + stack.transformation().translation().x;

        // Absolute anchor: 10.0 + 0.25 = 10.25 (the rasterised position).
        assert_eq!(observed, 10.25);

        // At backing scale 2.0 the device column is floor(10.25 * 2) = 20,
        // which maps back to logical 10.0 — snapping to a real device pixel.
        let scale = 2.0_f32;
        let device = (observed * scale).floor();
        assert_eq!(device, 20.0);

        // Contrast: ignoring the ancestor offset floors 10.0*2 = 20 too here,
        // but the point is the *observed* value differs — prove it does.
        assert_ne!(observed, logical_glyph_x);
    }

    /// Composition is via matrix multiply, and a scale between two
    /// translations affects the accumulated offset — proving the transform is
    /// a full composition, not an additive translation accumulator that would
    /// ignore an intervening scale.
    #[test]
    fn scale_between_translations_composes_multiplicatively() {
        let mut stack: Stack<TestLayer> = Stack::new();

        // outer translate, then a 2x scale, then an inner translate.
        stack.push_transformation(Transformation::translate(1.0, 0.0));
        stack.push_transformation(Transformation::scale(2.0));
        stack.push_transformation(Transformation::translate(3.0, 0.0));

        // The inner translate is applied in the scaled space: the composed
        // matrix maps (0,0) to outer + scale*inner_translation on the x-axis.
        // outer.x = 1.0, scale = 2.0, inner.x = 3.0 -> 1.0 + 2.0*3.0 = 7.0.
        let t = stack.transformation();
        assert_eq!(t.translation(), Vector::new(7.0, 0.0));
        assert_eq!(t.scale_factor(), 2.0);

        // A naive additive getter would report 1.0 + 3.0 = 4.0 and be wrong.
        assert_ne!(t.translation().x, 4.0);
    }
}
