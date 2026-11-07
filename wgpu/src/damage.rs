// Modified by enzUI contributors. See FORK.md.
//! Which parts of a replayed frame changed since the last one (enzUI-ICED
//! #3666).
//!
//! The render lane paints a frame on every refresh while anything moves. When
//! only a spinner turns, or a chart takes a new reading, everything else is
//! painted exactly as before, so a lane that keeps its last frame needs to
//! paint again only where something changed. Every replayed layer is hashed:
//! its clip and groups as one signature, each quad, mesh, text section and
//! image as another, with the rectangle it paints. Comparing a frame with the
//! last one, a changed primitive is damaged where it was and where it is; a
//! changed layer, or a segment replayed into other layers, is damaged whole.
//! The changes are covered by a few rectangles, each painted in its own pass.
//!
//! Paint is hashed by value, or by the address of what a recording shares and
//! never changes (a retained paragraph, a clip path), which the tracker keeps
//! from reuse by holding both recordings compared. A custom primitive is
//! compared by the signatures it gives for what it paints. Other paint that
//! can change without a new recording (a primitive that cannot tell, an
//! editor) is damaged on every frame. A new frame signature, a different segment count, an owned-layout
//! replay or a canvas behind the tracker is a full frame.

use crate::core::renderer::Recording;
use crate::core::{Color, Point, Rectangle, Transformation, border};

use std::hash::Hasher as _;

/// What a replayed frame changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Damage {
    /// Anything may have changed: paint the whole frame.
    Full,
    /// Only these rectangles (logical pixels) changed.
    Region(Regions),
    /// Nothing changed.
    Nothing,
}

/// A few rectangles covering what a frame changed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Regions {
    rects: [Rectangle; Regions::MAX],
    len: usize,
}

impl Regions {
    /// How many rectangles a frame paints separately at most: each is one
    /// more pass over the layers it touches.
    pub(crate) const MAX: usize = 4;

    /// Changes this close together are painted as one rectangle.
    const NEAR: f32 = 8.0;

    /// At most [`Self::MAX`] rectangles covering `rects`, or `None` for none:
    /// rectangles that touch or nearly do are merged, then the pair whose
    /// union adds the least area, until few enough are left.
    pub(crate) fn cover(rects: impl IntoIterator<Item = Rectangle>) -> Option<Self> {
        let mut regions: Vec<Rectangle> = Vec::new();
        for rect in rects {
            let mut rect = rect;
            while let Some(index) = regions
                .iter()
                .position(|region| region.expand(Self::NEAR).intersects(&rect))
            {
                rect = rect.union(&regions.swap_remove(index));
            }
            regions.push(rect);
        }
        while regions.len() > Self::MAX {
            let area = |rect: &Rectangle| rect.width * rect.height;
            let mut best = (f32::INFINITY, 0, 1);
            for a in 0..regions.len() {
                for b in a + 1..regions.len() {
                    let added = area(&regions[a].union(&regions[b]))
                        - area(&regions[a])
                        - area(&regions[b]);
                    if added < best.0 {
                        best = (added, a, b);
                    }
                }
            }
            let merged = regions.swap_remove(best.2);
            regions[best.1] = regions[best.1].union(&merged);
        }
        if regions.is_empty() {
            return None;
        }
        let mut cover = Self {
            rects: [Rectangle::default(); Self::MAX],
            len: regions.len(),
        };
        cover.rects[..regions.len()].copy_from_slice(&regions);
        Some(cover)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Rectangle> {
        self.rects[..self.len].iter()
    }
}

/// The same rectangles, in any order.
impl PartialEq for Regions {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && self
                .iter()
                .all(|rect| other.iter().any(|other| other == rect))
    }
}

/// Hashes what recorded paint draws, so a segment of a new recording that
/// paints exactly what the last one did is not damaged.
#[derive(Default)]
pub(crate) struct PaintHasher(rustc_hash::FxHasher, Vec<usize>);

impl PaintHasher {
    pub(crate) fn finish(&self) -> u64 {
        self.0.finish()
    }

    /// Starts the next signature; group ids keep the order they were first
    /// seen in.
    pub(crate) fn restart(&mut self) {
        self.0 = rustc_hash::FxHasher::default();
    }

    /// Hashes an alpha group id by when it first appeared: recordings number
    /// their groups from a running count, and replay issues them again.
    pub(crate) fn group(&mut self, id: usize) {
        let index = match self.1.iter().position(|seen| *seen == id) {
            Some(index) => index,
            None => {
                self.1.push(id);
                self.1.len() - 1
            }
        };
        self.0.write_usize(index);
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.0.write_usize(bytes.len());
        self.0.write(bytes);
    }

    pub(crate) fn usize(&mut self, value: usize) {
        self.0.write_usize(value);
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.write_u64(value);
    }

    pub(crate) fn hash(&mut self, value: &impl std::hash::Hash) {
        value.hash(&mut self.0);
    }

    pub(crate) fn f32(&mut self, value: f32) {
        self.0.write_u32(value.to_bits());
    }

    pub(crate) fn point(&mut self, point: Point) {
        self.f32(point.x);
        self.f32(point.y);
    }

    pub(crate) fn rectangle(&mut self, rectangle: Rectangle) {
        self.f32(rectangle.x);
        self.f32(rectangle.y);
        self.f32(rectangle.width);
        self.f32(rectangle.height);
    }

    pub(crate) fn radius(&mut self, radius: border::Radius) {
        self.f32(radius.top_left);
        self.f32(radius.top_right);
        self.f32(radius.bottom_right);
        self.f32(radius.bottom_left);
    }

    pub(crate) fn color(&mut self, color: Color) {
        for channel in [color.r, color.g, color.b, color.a] {
            self.f32(channel);
        }
    }

    pub(crate) fn transformation(&mut self, transformation: Transformation) {
        let matrix: &[f32; 16] = transformation.as_ref();
        self.bytes(bytemuck::cast_slice(matrix));
    }

    pub(crate) fn option<T>(&mut self, value: Option<T>, hash: impl FnOnce(&mut Self, T)) {
        self.0.write_u8(u8::from(value.is_some()));
        if let Some(value) = value {
            hash(self, value);
        }
    }
}

/// One replayed layer: its signature (`None`: its paint changes every
/// frame), where it paints, and each primitive's signature and where it
/// paints.
#[derive(Debug, Default)]
struct LayerEntry {
    head: Option<u64>,
    extent: Option<Rectangle>,
    elements: Vec<(Option<u64>, Option<Rectangle>)>,
}

/// One replayed segment: a signature of where its tracks placed it, and its
/// layers.
#[derive(Debug, Default)]
struct SegmentEntry {
    key: u64,
    layers: Vec<LayerEntry>,
}

impl SegmentEntry {
    fn extent(&self) -> Option<Rectangle> {
        self.layers
            .iter()
            .filter_map(|layer| layer.extent)
            .reduce(|a, b| a.union(&b))
    }

    /// Adds where `self`, the segment's next replay, differs from `was`.
    fn compare(&self, was: &Self, add: &mut impl FnMut(Option<Rectangle>)) {
        if was.layers.len() != self.layers.len() {
            was.layers.iter().for_each(|layer| add(layer.extent));
            self.layers.iter().for_each(|layer| add(layer.extent));
            return;
        }
        for (before, after) in was.layers.iter().zip(&self.layers) {
            if before.head.is_none()
                || before.head != after.head
                || before.elements.len() != after.elements.len()
            {
                add(before.extent);
                add(after.extent);
                continue;
            }
            for (was, is) in before.elements.iter().zip(&after.elements) {
                if was.0.is_none() || was != is {
                    add(was.1);
                    add(is.1);
                }
            }
        }
    }
}

/// Tracks the damage of consecutive replays.
///
/// A replay of a new scene replays every segment ([`Self::begin`]); a replay
/// of the same scene may replay only the segments whose placement changed
/// ([`Self::begin_changed`]), the others painting exactly what they did.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// The scenes the last two replays painted. Holding them keeps the
    /// addresses their paint was hashed by from being reused while the two
    /// frames are compared.
    scenes: [Option<Recording>; 2],
    /// A signature of what applies to the whole frame.
    frame: u64,
    previous: Vec<SegmentEntry>,
    current: Vec<SegmentEntry>,
    /// The replay replays only changed segments.
    only_changed: bool,
    /// The segment of a replay of changed segments being recorded.
    changing: Option<(usize, SegmentEntry)>,
    /// Primitives of the layer being recorded.
    elements: Vec<(Option<u64>, Option<Rectangle>)>,
    full: bool,
    result: Option<Damage>,
    /// The canvas shows the `previous` replay: its damage was taken to be
    /// painted, and no replay was painted since without being finished.
    synced: bool,
    /// The rectangles the last comparison found changed, kept for reuse.
    changed: Vec<Rectangle>,
}

impl Tracker {
    /// Starts a replay of every segment of `recording`; `frame` is asked for
    /// a signature of what applies to the whole frame of a recording not
    /// replayed before.
    pub(crate) fn begin(&mut self, recording: &Recording, frame: impl FnOnce() -> u64) {
        self.full = false;
        if !self.scenes[1]
            .as_ref()
            .is_some_and(|scene| scene.same_revision(recording))
        {
            let frame = frame();
            self.full = self.scenes[1].is_none() || frame != self.frame;
            self.frame = frame;
            self.scenes[0] = self.scenes[1].replace(recording.clone());
        }
        self.current.clear();
        self.only_changed = false;
        self.changing = None;
        self.elements.clear();
        self.result = None;
    }

    /// Whether a replay of `recording`, of `segments` segments, may replay
    /// only its changed segments: the last replay was of the same scene, was
    /// finished in full or in part, and was painted.
    pub(crate) fn can_replay_changed(&self, recording: &Recording, segments: usize) -> bool {
        self.synced
            && self.previous.len() == segments
            && self.scenes[1]
                .as_ref()
                .is_some_and(|scene| scene.same_revision(recording))
    }

    /// Where the last replay placed segment `index`.
    pub(crate) fn key(&self, index: usize) -> Option<u64> {
        self.previous.get(index).map(|segment| segment.key)
    }

    /// Where segment `index` last painted.
    pub(crate) fn extent(&self, index: usize) -> Option<Rectangle> {
        self.previous.get(index).and_then(SegmentEntry::extent)
    }

    /// Where each layer of segment `index` last painted.
    pub(crate) fn layer_extents(
        &self,
        index: usize,
    ) -> impl Iterator<Item = Option<Rectangle>> + '_ {
        self.previous
            .get(index)
            .into_iter()
            .flat_map(|segment| segment.layers.iter().map(|layer| layer.extent))
    }

    /// Starts a replay of only the changed segments of the same scene.
    pub(crate) fn begin_changed(&mut self) {
        self.only_changed = true;
        self.changed.clear();
        self.changing = None;
        self.elements.clear();
        self.result = None;
    }

    /// Starts the scene's next segment, placed by `key`; in a replay of
    /// changed segments, segment `index`.
    pub(crate) fn segment(&mut self, index: usize, key: u64) {
        self.end_changed_segment();
        let entry = SegmentEntry {
            key,
            layers: Vec::new(),
        };
        if self.only_changed {
            self.changing = Some((index, entry));
        } else {
            self.current.push(entry);
        }
    }

    /// Records a primitive of the layer being recorded.
    pub(crate) fn element(&mut self, signature: Option<u64>, extent: Option<Rectangle>) {
        self.elements.push((signature, extent));
    }

    /// Records a replayed layer of the segment, after its primitives.
    pub(crate) fn layer(&mut self, signature: Option<u64>, extent: Option<Rectangle>) {
        let entry = LayerEntry {
            head: signature,
            extent,
            elements: std::mem::take(&mut self.elements),
        };
        match &mut self.changing {
            Some((_, segment)) => segment.layers.push(entry),
            None => {
                if let Some(segment) = self.current.last_mut() {
                    segment.layers.push(entry);
                }
            }
        }
    }

    /// Compares a changed segment just replayed with its last replay, and
    /// keeps it in its place.
    fn end_changed_segment(&mut self) {
        let Some((index, segment)) = self.changing.take() else {
            return;
        };
        let changed = &mut self.changed;
        let mut add = |extent: Option<Rectangle>| changed.extend(extent);
        match self.previous.get_mut(index) {
            Some(was) => {
                segment.compare(was, &mut add);
                *was = segment;
            }
            None => segment.layers.iter().for_each(|layer| add(layer.extent)),
        }
    }

    /// Ends a replay of changed segments.
    pub(crate) fn finish_changed(&mut self) {
        self.end_changed_segment();
        let damage = if self.synced {
            Regions::cover(self.changed.iter().copied()).map_or(Damage::Nothing, Damage::Region)
        } else {
            Damage::Full
        };
        self.result = Some(damage);
        // A replay that changed nothing leaves the canvas showing it, painted
        // or not.
        self.synced &= damage == Damage::Nothing;
    }

    /// The damage the last replay finished with, before it is taken.
    pub(crate) fn pending(&self) -> Option<Damage> {
        self.result
    }

    /// Forgets that the canvas shows the last replay, so the next replay is
    /// a full frame.
    pub(crate) fn invalidate(&mut self) {
        self.synced = false;
        self.result = None;
    }

    /// Ends the replay; an owned-layout replay (`evaluated`) changes layout,
    /// not just transforms, so it is a full frame.
    pub(crate) fn finish(&mut self, evaluated: bool) {
        let damage = if !self.synced
            || self.full
            || evaluated
            || self.current.len() != self.previous.len()
        {
            Damage::Full
        } else {
            let mut changed = std::mem::take(&mut self.changed);
            changed.clear();
            let mut add = |extent: Option<Rectangle>| changed.extend(extent);
            for (was, is) in self.previous.iter().zip(&self.current) {
                is.compare(was, &mut add);
            }
            let damage =
                Regions::cover(changed.iter().copied()).map_or(Damage::Nothing, Damage::Region);
            self.changed = changed;
            damage
        };
        std::mem::swap(&mut self.previous, &mut self.current);
        self.result = Some(damage);
        // A replay that changed nothing leaves the canvas showing it, painted
        // or not.
        self.synced &= damage == Damage::Nothing;
    }

    /// The damage of the last finished replay, once, for the frame about to
    /// be painted; a replay that did not finish (refused, or none since) is a
    /// full frame the next replay cannot compare with.
    pub(crate) fn take(&mut self) -> Damage {
        let damage = self.result.take();
        self.synced = damage.is_some();
        damage.unwrap_or(Damage::Full)
    }
}

/// What a frame drawn by widgets hashed by address: holding it keeps the
/// address from being reused by something else before the next frame is
/// compared with this one.
#[derive(Debug, Default)]
pub(crate) struct Held {
    pub(crate) paragraphs: Vec<crate::graphics::text::paragraph::Weak>,
    pub(crate) paths: Vec<std::sync::Arc<crate::graphics::layer::ClipPath>>,
}

impl Held {
    fn clear(&mut self) {
        self.paragraphs.clear();
        self.paths.clear();
    }
}

/// Tracks the damage of consecutive frames drawn by widgets on the UI
/// thread: the whole layer stack is hashed as one segment when the frame is
/// presented, and compared with the last frame presented.
#[derive(Debug, Default)]
pub(crate) struct FrameTracker {
    /// The last frame presented, if one was.
    previous: Option<SegmentEntry>,
    current: SegmentEntry,
    elements: Vec<(Option<u64>, Option<Rectangle>)>,
    /// What the last frame (`[0]`) and this one (`[1]`) hashed by address.
    held: [Held; 2],
    changed: Vec<Rectangle>,
}

impl FrameTracker {
    /// Hashes the frame about to be presented, `layers` with `land` landing
    /// a primitive's rectangle in its layer and `extent` giving where a layer
    /// paints, and returns where it differs from the last one presented.
    pub(crate) fn frame(
        &mut self,
        layers: &[crate::layer::Layer],
        land: impl Fn(&crate::layer::Layer, Rectangle) -> Option<Rectangle>,
        extent: impl Fn(&crate::layer::Layer) -> Option<Rectangle>,
    ) -> Damage {
        let Self {
            previous,
            current,
            elements,
            held,
            changed,
        } = self;
        held[1].clear();
        current.layers.clear();
        let mut paint = PaintHasher::default();
        for layer in layers {
            paint.restart();
            elements.clear();
            let head = layer.hash_paint(&mut paint, Some(&mut held[1]), |hash, rect| {
                elements.push((hash, land(layer, rect)));
            });
            current.layers.push(LayerEntry {
                head,
                extent: extent(layer),
                elements: std::mem::take(elements),
            });
        }
        let damage = match previous.as_mut() {
            None => Damage::Full,
            Some(was) => {
                changed.clear();
                current.compare(was, &mut |extent| changed.extend(extent));
                Regions::cover(changed.iter().copied()).map_or(Damage::Nothing, Damage::Region)
            }
        };
        let was = previous.get_or_insert_with(SegmentEntry::default);
        std::mem::swap(was, current);
        held.swap(0, 1);
        damage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Point, Size};

    fn rect(x: f32, y: f32) -> Option<Rectangle> {
        Some(Rectangle::new(Point::new(x, y), Size::new(10.0, 10.0)))
    }

    fn region(x: f32, width: f32) -> Damage {
        regions(&[(x, width)])
    }

    fn regions(spans: &[(f32, f32)]) -> Damage {
        let mut regions = Regions {
            rects: [Rectangle::default(); Regions::MAX],
            len: spans.len(),
        };
        for (rect, (x, width)) in regions.rects.iter_mut().zip(spans) {
            *rect = Rectangle::new(Point::new(*x, 0.0), Size::new(*width, 10.0));
        }
        Damage::Region(regions)
    }

    /// Replays `scene`: a still segment, and one whose single layer paints
    /// `paint` at `at`; takes the damage to paint it.
    fn replay(tracker: &mut Tracker, scene: &Recording, paint: Option<u64>, at: f32) -> Damage {
        tracker.begin(scene, || 0);
        segment(tracker, &[(Some(1), rect(0.0, 0.0))]);
        segment(tracker, &[(paint, rect(at, 0.0))]);
        tracker.finish(false);
        tracker.take()
    }

    /// A segment of one layer per entry, painting nothing but itself.
    fn segment(tracker: &mut Tracker, layers: &[(Option<u64>, Option<Rectangle>)]) {
        let index = tracker.current.len();
        tracker.segment(index, 0);
        for (paint, extent) in layers {
            tracker.layer(*paint, *extent);
        }
    }

    /// Only the layers that paint differently are damaged, where they were
    /// and where they are, whichever recording they came from; a new frame
    /// signature or segment count is full.
    ///
    /// Sabotage: damage only where a changed layer is now -- the region it
    /// left keeps its old paint.
    #[test]
    fn damage_covers_where_changed_layers_were_and_are() {
        let scene = Recording::new(());
        let mut tracker = Tracker::default();
        assert_eq!(
            replay(&mut tracker, &scene, Some(2), 100.0),
            Damage::Full,
            "the first scene"
        );
        assert_eq!(
            replay(&mut tracker, &scene, Some(2), 100.0),
            Damage::Nothing,
            "nothing changed"
        );
        assert_eq!(
            replay(&mut tracker, &scene, Some(3), 105.0),
            region(100.0, 15.0),
            "the moved layer, before and after"
        );
        let next = Recording::new(());
        assert_eq!(
            replay(&mut tracker, &next, Some(3), 105.0),
            Damage::Nothing,
            "the same paint"
        );
        assert_eq!(
            replay(&mut tracker, &next, Some(4), 105.0),
            region(105.0, 10.0),
            "new paint in place"
        );

        tracker.begin(&next, || unreachable!("the recording was seen"));
        segment(&mut tracker, &[(Some(1), rect(0.0, 0.0))]);
        tracker.finish(false);
        assert_eq!(tracker.take(), Damage::Full, "a different segment count");
        assert_eq!(tracker.take(), Damage::Full, "no replay since");

        let _ = replay(&mut tracker, &Recording::new(()), Some(4), 105.0);
        tracker.begin(&Recording::new(()), || 1);
        segment(&mut tracker, &[(Some(1), rect(0.0, 0.0))]);
        segment(&mut tracker, &[(Some(4), rect(105.0, 0.0))]);
        tracker.finish(false);
        assert_eq!(tracker.take(), Damage::Full, "another frame signature");
    }

    /// Within a layer that keeps its clip and groups, only its changed
    /// primitives are damaged; a layer with another primitive count is
    /// damaged whole.
    ///
    /// Sabotage: compare a layer only by its own signature -- the repainted
    /// tile keeps its old paint.
    #[test]
    fn only_changed_primitives_of_a_layer_are_damaged() {
        let scene = Recording::new(());
        let mut tracker = Tracker::default();
        let page = |tracker: &mut Tracker, tile: u64, tiles: usize| {
            tracker.begin(&scene, || 0);
            tracker.segment(0, 0);
            for (paint, at) in [(1, 0.0), (tile, 50.0)].into_iter().take(tiles) {
                tracker.element(Some(paint), rect(at, 0.0));
            }
            tracker.layer(
                Some(7),
                Some(Rectangle::new(Point::ORIGIN, Size::new(100.0, 10.0))),
            );
            tracker.finish(false);
            tracker.take()
        };
        let _ = page(&mut tracker, 2, 2);
        assert_eq!(
            page(&mut tracker, 3, 2),
            region(50.0, 10.0),
            "the repainted tile"
        );
        assert_eq!(page(&mut tracker, 3, 1), region(0.0, 100.0), "a tile fewer");
    }

    /// A segment replayed into a different number of layers is damaged
    /// whole; its other segments are compared layer by layer.
    ///
    /// Sabotage: compare the shorter run of layers pairwise -- the layer
    /// that disappeared keeps its old paint.
    #[test]
    fn a_segment_with_other_layers_is_damaged_whole() {
        let scene = Recording::new(());
        let mut tracker = Tracker::default();
        let page = |tracker: &mut Tracker, layers: usize| {
            tracker.begin(&scene, || 0);
            segment(
                tracker,
                &[(Some(1), rect(0.0, 0.0)), (Some(2), rect(50.0, 0.0))][..layers],
            );
            tracker.finish(false);
            tracker.take()
        };
        let _ = page(&mut tracker, 2);
        assert_eq!(page(&mut tracker, 2), Damage::Nothing);
        assert_eq!(page(&mut tracker, 1), regions(&[(0.0, 10.0), (50.0, 10.0)]));
    }

    /// Changes that nearly touch are painted as one rectangle; far apart,
    /// separately, until there are too many, when the closest merge.
    ///
    /// Sabotage: never merge -- five changes need five passes.
    #[test]
    fn regions_cover_changes_in_a_few_rectangles() {
        let square = |x: f32| Rectangle::new(Point::new(x, 0.0), Size::new(10.0, 10.0));
        let cover =
            |xs: &[f32]| Damage::Region(Regions::cover(xs.iter().map(|x| square(*x))).unwrap());
        assert_eq!(cover(&[0.0, 15.0]), region(0.0, 25.0), "8 px apart");
        assert_eq!(cover(&[0.0, 100.0]), regions(&[(0.0, 10.0), (100.0, 10.0)]));
        assert_eq!(
            cover(&[0.0, 100.0, 200.0, 300.0, 330.0]),
            regions(&[(0.0, 10.0), (100.0, 10.0), (200.0, 10.0), (300.0, 40.0)]),
            "the closest two of five"
        );
        assert!(Regions::cover([]).is_none());
    }

    /// Paint that changes without a new recording (a custom primitive, an
    /// editor) is damaged on every frame.
    ///
    /// Sabotage: compare unhashed paint like any other -- the still
    /// primitive is never painted again.
    #[test]
    fn unhashed_paint_is_damaged_every_frame() {
        let scene = Recording::new(());
        let mut tracker = Tracker::default();
        assert_eq!(replay(&mut tracker, &scene, None, 100.0), Damage::Full);
        assert_eq!(
            replay(&mut tracker, &scene, None, 100.0),
            region(100.0, 10.0)
        );
    }

    /// A replay whose frame was never painted (a failed acquire, a surface
    /// the canvas cannot be copied to) leaves the canvas behind the tracker,
    /// so the next frame is full however little changed.
    ///
    /// Sabotage: keep the canvas synced through an untaken replay -- the
    /// next frame damages only the last step.
    #[test]
    fn a_frame_after_an_unpainted_replay_is_full() {
        let scene = Recording::new(());
        let mut tracker = Tracker::default();
        let replay = |tracker: &mut Tracker, at: f32| {
            tracker.begin(&scene, || 0);
            segment(tracker, &[(Some(at.to_bits().into()), rect(at, 0.0))]);
            tracker.finish(false);
        };
        replay(&mut tracker, 0.0);
        assert_eq!(tracker.take(), Damage::Full, "a new scene");
        replay(&mut tracker, 1.0);
        assert!(matches!(tracker.take(), Damage::Region(_)));
        replay(&mut tracker, 2.0);
        replay(&mut tracker, 3.0);
        assert_eq!(tracker.take(), Damage::Full, "the canvas shows step 1");
        replay(&mut tracker, 4.0);
        assert!(matches!(tracker.take(), Damage::Region(_)));
        assert_eq!(tracker.take(), Damage::Full, "painted with no replay");
        replay(&mut tracker, 5.0);
        assert_eq!(tracker.take(), Damage::Full, "the canvas shows that paint");
    }
}

/// Reads `texture` back as tightly packed 4-byte pixels.
pub(crate) fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("iced_wgpu damage readback"),
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    let _ = queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let mapped = slice.get_mapped_range();
    mapped
        .chunks(row as usize)
        .flat_map(|line| line[..(width * 4) as usize].to_vec())
        .collect()
}

/// The pixels of two frames of the same size that differ by more than
/// rounding, and the rectangle they span.
pub(crate) fn difference(width: u32, a: &[u8], b: &[u8]) -> (usize, Option<Rectangle<u32>>) {
    let mut count = 0;
    let mut span: Option<(u32, u32, u32, u32)> = None;
    for (index, (a, b)) in a.chunks(4).zip(b.chunks(4)).enumerate() {
        if a.iter().zip(b).any(|(a, b)| a.abs_diff(*b) > 2) {
            count += 1;
            let (x, y) = (index as u32 % width, index as u32 / width);
            span = Some(span.map_or((x, y, x, y), |(l, t, r, b)| {
                (l.min(x), t.min(y), r.max(x), b.max(y))
            }));
        }
    }
    (
        count,
        span.map(|(l, t, r, b)| Rectangle {
            x: l,
            y: t,
            width: r - l + 1,
            height: b - t + 1,
        }),
    )
}

/// The lane's last frame, kept so a damaged frame paints only part of it.
#[derive(Debug)]
pub(crate) struct Canvas {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    /// The scale factor it was painted at.
    scale: f32,
}

impl Canvas {
    pub(crate) fn new(
        device: &wgpu::Device,
        size: crate::core::Size<u32>,
        format: wgpu::TextureFormat,
        scale: f32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu lane canvas"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            scale,
        }
    }

    pub(crate) fn fits(
        &self,
        size: crate::core::Size<u32>,
        format: wgpu::TextureFormat,
        scale: f32,
    ) -> bool {
        self.texture.width() == size.width
            && self.texture.height() == size.height
            && self.texture.format() == format
            && self.scale.to_bits() == scale.to_bits()
    }
}

#[cfg(test)]
mod frames {
    //! A lane that paints only damaged regions of its kept canvas shows, on
    //! every frame, exactly what a full replay paints.

    use crate::core::renderer::{self, NodeId, Renderer as _, Track};
    use crate::core::{Background, Color, Point, Rectangle, Size, Vector};
    use crate::graphics::mesh::Renderer as _;
    use crate::graphics::{Antialiasing, Shell, Viewport};
    use crate::{Engine, Renderer};

    const SIZE: Size<u32> = Size::new(480, 300);
    const MOVING: NodeId = NodeId(41);
    const TURNING: NodeId = NodeId(42);
    const DOTS: NodeId = NodeId(43);
    const DOTS_CENTER: Point = Point::new(380.0, 70.0);
    const DOT: Color = Color::from_rgb(0.1, 1.0, 0.3);

    fn renderer() -> Renderer {
        let engine = pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
                flags: wgpu::InstanceFlags::empty(),
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .expect("an adapter is required to render");
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .expect("device");
            Engine::new(
                &adapter,
                device,
                queue,
                wgpu::TextureFormat::Rgba8Unorm,
                Some(Antialiasing::MSAAx4),
                Shell::headless(),
            )
        });
        Renderer::new(engine, renderer::Settings::default())
    }

    fn quad(renderer: &mut Renderer, bounds: Rectangle, color: Color) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: crate::core::border::rounded(4.0),
                ..renderer::Quad::default()
            },
            Background::Color(color),
        );
    }

    /// A ring of round dots in a layer of its own, recorded as `DOTS`, as a
    /// spinner paints them.
    fn dots(renderer: &mut Renderer) {
        renderer.start_layer(Rectangle::new(
            Point::new(DOTS_CENTER.x - 20.0, DOTS_CENTER.y - 20.0),
            Size::new(40.0, 40.0),
        ));
        renderer.mark_recording(DOTS);
        for i in 0..6 {
            let angle = std::f32::consts::TAU * i as f32 / 6.0;
            let center = Point::new(
                DOTS_CENTER.x + 14.0 * angle.cos(),
                DOTS_CENTER.y + 14.0 * angle.sin(),
            );
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        Point::new(center.x - 3.0, center.y - 3.0),
                        Size::new(6.0, 6.0),
                    ),
                    border: crate::core::border::rounded(3.0),
                    snap: false,
                    ..renderer::Quad::default()
                },
                Background::Color(Color {
                    a: 0.4 + 0.1 * i as f32,
                    ..DOT
                }),
            );
        }
        renderer.mark_recording(NodeId::STATIC);
        renderer.end_layer();
    }

    /// A page of static paint (one of its tiles `shade`d, and a centred
    /// paragraph that says more when it is), a rounded card whose inner
    /// square moves, a square that turns, and a ring of dots that turns as a
    /// spinner does.
    fn scene(renderer: &mut Renderer, shade: f32) -> renderer::Recording {
        let window = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
        renderer.reset(window);
        assert!(renderer.start_recording(window));
        quad(renderer, window, Color::from_rgb(0.1, 0.1, 0.12));
        for i in 0..6 {
            quad(
                renderer,
                Rectangle::new(
                    Point::new(10.0 + 50.0 * i as f32, 10.0),
                    Size::new(40.0, 30.0),
                ),
                Color::from_rgb(0.2 + 0.1 * i as f32, if i == 2 { shade } else { 0.4 }, 0.6),
            );
        }
        let card = Rectangle::new(Point::new(20.0, 60.0), Size::new(160.0, 100.0));
        renderer.start_layer_rounded(card, 16.0.into());
        quad(renderer, card, Color::from_rgb(0.3, 0.3, 0.35));
        renderer.mark_recording(MOVING);
        quad(
            renderer,
            Rectangle::new(Point::new(30.0, 80.0), Size::new(40.0, 40.0)),
            Color::from_rgb(0.9, 0.5, 0.1),
        );
        renderer.mark_recording(NodeId::STATIC);
        renderer.end_layer();
        // A static mesh the moving square passes over.
        renderer.draw_mesh(crate::graphics::Mesh::Solid {
            buffers: crate::graphics::mesh::Indexed {
                vertices: [[60.0, 95.0], [150.0, 95.0], [150.0, 135.0]]
                    .into_iter()
                    .map(|position| crate::graphics::mesh::SolidVertex2D {
                        position,
                        color: crate::graphics::color::pack(Color::from_rgb(0.2, 0.5, 0.9)),
                    })
                    .collect(),
                indices: vec![0, 1, 2],
            },
            transformation: crate::core::Transformation::IDENTITY,
            clip_bounds: Rectangle::new(Point::new(60.0, 95.0), Size::new(90.0, 40.0)),
        });
        renderer.mark_recording(TURNING);
        quad(
            renderer,
            Rectangle::new(Point::new(240.0, 120.0), Size::new(30.0, 30.0)),
            Color::from_rgb(0.2, 0.8, 0.4),
        );
        renderer.mark_recording(NodeId::STATIC);
        dots(renderer);
        let paragraph = paragraph(if shade > 0.5 {
            "A longer paragraph now"
        } else {
            "Short"
        });
        crate::core::text::Renderer::fill_paragraph(
            renderer,
            &paragraph,
            Point::new(200.0, 230.0),
            Color::WHITE,
            window,
        );
        renderer.finish_recording().expect("the scene is recorded")
    }

    /// A single line, centred in 260 px.
    fn paragraph(content: &str) -> crate::graphics::text::Paragraph {
        use crate::core::text::Paragraph as _;

        crate::graphics::text::Paragraph::with_text(crate::core::Text {
            content,
            bounds: Size::new(260.0, 40.0),
            size: 18.0.into(),
            line_height: crate::core::text::LineHeight::default(),
            font: crate::core::Font::DEFAULT,
            align_x: crate::core::text::Alignment::Center,
            align_y: crate::core::alignment::Vertical::Top,
            shaping: crate::core::text::Shaping::Advanced,
            wrapping: crate::core::text::Wrapping::None,
            ellipsis: crate::core::text::Ellipsis::default(),
            hint_factor: None,
        })
    }

    /// The moving square `moved` steps along, the square and the dots
    /// `turned` steps round.
    fn tracks(moved: usize, turned: usize) -> Vec<Track> {
        let (m, t) = (moved as f32, turned as f32);
        vec![
            Track {
                translation: Vector::new(9.0 * m, 3.0 * m),
                ..Track::identity(MOVING)
            },
            Track {
                rotation: 0.4 * t,
                scale_origin: Point::new(255.0, 135.0),
                ..Track::identity(TURNING)
            },
            Track {
                rotation: 0.3 * t,
                scale_origin: DOTS_CENTER,
                ..Track::identity(DOTS)
            },
        ]
    }

    fn read(renderer: &Renderer, texture: &wgpu::Texture) -> Vec<u8> {
        super::read_pixels(&renderer.engine.device, &renderer.engine.queue, texture)
    }

    fn surface_texture(renderer: &Renderer) -> wgpu::Texture {
        sized_texture(renderer, SIZE)
    }

    fn sized_texture(renderer: &Renderer, size: Size<u32>) -> wgpu::Texture {
        renderer
            .engine
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
    }

    /// New recordings are compared by what they paint: one that paints the
    /// same is damaged only where nodes moved, one with a tile repainted
    /// also where the tile is.
    ///
    /// Sabotage: skip the scissored clear, paint only where a moved node is
    /// now, or leave a quad's colour out of the paint signature -- the frames
    /// diverge.
    #[test]
    fn a_damaged_frame_matches_a_full_replay() {
        let mut lane = renderer();
        let first = scene(&mut lane, 0.4);
        let same = scene(&mut lane, 0.4);
        let repainted = scene(&mut lane, 0.9);
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let target = surface_texture(&lane);
        lane.track_damage(true);
        let background = Color::BLACK;
        let window = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
        // (moved, turned, scene): the square moving inside the rounded card
        // is replayed with the whole scene; the turning square and dots
        // alone, outside any clip, are replayed with just what they paint
        // over.
        for (moved, turned, recording) in [
            (0, 0, &first),
            (1, 1, &first),
            (2, 2, &first),
            (2, 2, &first),
            (2, 3, &first),
            (2, 4, &first),
            (3, 5, &same),
            (4, 6, &repainted),
            (4, 7, &repainted),
            (8, 8, &first),
        ] {
            let frame = (moved, turned);
            lane.reset(window);
            assert!(lane.replay_tracks(recording, &tracks(moved, turned)));
            let _ = lane.present_damaged(background, &target, &viewport, false);
            let damaged = read(&lane, &target);

            // A renderer with a fresh canvas paints the whole frame.
            let mut full = renderer();
            full.track_damage(true);
            full.reset(window);
            assert!(full.replay_tracks(recording, &tracks(moved, turned)));
            let full_target = surface_texture(&full);
            let _ = full.present_damaged(background, &full_target, &viewport, false);
            let expected = read(&full, &full_target);

            let (differing, _) = super::difference(SIZE.width, &damaged, &expected);
            assert_eq!(differing, 0, "frame {frame:?}: {differing} pixels differ");
        }
        assert!(
            lane.partial_frames() >= 7,
            "the moving frames after the first were painted in part ({})",
            lane.partial_frames()
        );
        assert!(
            lane.changed_replays() >= 3,
            "the frames only the unclipped nodes changed replayed what changed ({})",
            lane.changed_replays()
        );
    }

    /// A frame drawn by widgets: the static page above a strip of clear
    /// color, the rounded card with its square `moved` steps along, and
    /// `label` under it.
    fn widget_frame(
        renderer: &mut Renderer,
        shade: f32,
        moved: usize,
        label: &crate::graphics::text::Paragraph,
    ) {
        let window = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
        renderer.reset(window);
        // The clear color shows below the page.
        quad(
            renderer,
            Rectangle::with_size(Size::new(window.width, window.height - 20.0)),
            Color::from_rgb(0.1, 0.1, 0.12),
        );
        for i in 0..6 {
            quad(
                renderer,
                Rectangle::new(
                    Point::new(10.0 + 50.0 * i as f32, 10.0),
                    Size::new(40.0, 30.0),
                ),
                Color::from_rgb(0.2 + 0.1 * i as f32, if i == 2 { shade } else { 0.4 }, 0.6),
            );
        }
        let card = Rectangle::new(Point::new(20.0, 60.0), Size::new(160.0, 100.0));
        renderer.start_layer_rounded(card, 16.0.into());
        quad(renderer, card, Color::from_rgb(0.3, 0.3, 0.35));
        quad(
            renderer,
            Rectangle::new(
                Point::new(30.0 + 9.0 * moved as f32, 80.0),
                Size::new(40.0, 40.0),
            ),
            Color::from_rgb(0.9, 0.5, 0.1),
        );
        renderer.end_layer();
        dots(renderer);
        crate::core::text::Renderer::fill_paragraph(
            renderer,
            label,
            Point::new(200.0, 230.0),
            Color::WHITE,
            window,
        );
    }

    /// What a renderer with a fresh canvas paints for the same frame, and
    /// its screenshot.
    fn full_frame(
        background: Color,
        viewport: &Viewport,
        shade: f32,
        moved: usize,
        label: &crate::graphics::text::Paragraph,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut full = renderer();
        full.track_frame_damage(true);
        widget_frame(&mut full, shade, moved, label);
        let target = surface_texture(&full);
        let _ = full.present_damaged(background, &target, viewport, false);
        (read(&full, &target), full.screenshot(viewport, background))
    }

    /// Frames drawn by widgets are compared by what their layers paint: a
    /// frame painted again only where it differs from the last one shows
    /// what a full paint does, after a square moved, a tile was repainted,
    /// a label was reshaped or replaced (by one just as wide too), or the
    /// clear color changed; a frame that changed nothing paints nothing.
    /// Its layers are kept whole, so a screenshot right after paints them
    /// all, and a content-only frame reuses only a frame painted in full.
    ///
    /// Sabotage: hash a widget's paragraph as volatile, leave its address
    /// out, drop the clear color from the frame signature, or clear the
    /// paint of the layers a partial frame skips, or reuse what a partial
    /// frame prepared -- frames diverge or the partial count falls.
    #[test]
    fn a_widget_frame_painted_in_part_matches_a_full_paint() {
        use crate::core::text::Paragraph as _;

        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        let mut label = paragraph("Short");
        // (shade, moved, background, label change)
        let frames: [(f32, usize, Color, Option<&str>); 9] = [
            (0.4, 0, Color::BLACK, None),
            (0.4, 1, Color::BLACK, None),
            (0.4, 1, Color::BLACK, None),
            (0.9, 2, Color::BLACK, None),
            (0.9, 2, Color::BLACK, Some("resize")),
            (0.9, 3, Color::BLACK, Some("A longer paragraph now")),
            (0.9, 3, Color::BLACK, Some("A longer paragraph too")),
            (0.9, 3, Color::WHITE, None),
            (0.9, 4, Color::WHITE, None),
        ];
        let mut last_full = false;
        for (index, (shade, moved, background, change)) in frames.into_iter().enumerate() {
            match change {
                Some("resize") => label.resize(Size::new(120.0, 40.0)),
                Some(content) => label = paragraph(content),
                None => {}
            }
            let painted = window.partial_frames() + window.full_frames();
            let partial = window.partial_frames();
            widget_frame(&mut window, shade, moved, &label);
            let _ = window.present_damaged(background, &target, &viewport, false);
            if index == 2 {
                assert_eq!(
                    window.partial_frames() + window.full_frames(),
                    painted,
                    "the frame that changed nothing painted nothing"
                );
            }
            let damaged = read(&window, &target);
            let (expected, expected_shot) = full_frame(background, &viewport, shade, moved, &label);
            let (differing, _) = super::difference(SIZE.width, &damaged, &expected);
            assert_eq!(differing, 0, "frame {index}: {differing} pixels differ");

            // The layers are whole again: a screenshot paints the frame.
            // Only a frame painted in full is presented again as it was
            // prepared.
            if window.partial_frames() + window.full_frames() != painted {
                last_full = window.partial_frames() == partial;
            }
            assert_eq!(window.reuse_frame(), last_full, "frame {index} reuse");
            let shot = window.screenshot(&viewport, background);
            let (differing, _) = super::difference(SIZE.width, &shot, &expected_shot);
            assert_eq!(
                differing, 0,
                "frame {index} screenshot: {differing} pixels differ"
            );
            // The screenshot prepared the whole stack.
            last_full = true;
        }
        assert!(
            window.partial_frames() >= 5,
            "the frames after the first that changed little were painted in part ({})",
            window.partial_frames()
        );

        // A transparent desktop-glass window has independent opaque panes.
        // Culling its unchanged sidebar must not turn the remaining rounded
        // content pane into a mask for the entire retained canvas.
        let panes = |renderer: &mut Renderer, shade: f32| {
            let bounds = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
            renderer.reset(bounds);
            renderer.start_layer_rounded(bounds, 22.0.into());
            quad(
                renderer,
                Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 300.0)),
                Color::WHITE,
            );
            let content = Rectangle::new(Point::new(150.0, 0.0), Size::new(330.0, 300.0));
            renderer.start_layer_rounded(content, 16.0.into());
            quad(renderer, content, Color::from_rgb(0.2, 0.3, 0.4));
            quad(
                renderer,
                Rectangle::new(Point::new(170.0, 40.0), Size::new(20.0, 20.0)),
                Color::from_rgb(0.2, shade, 0.4),
            );
            crate::core::text::Renderer::fill_paragraph(
                renderer,
                &label,
                Point::new(170.0, 0.0),
                Color::WHITE,
                content,
            );
            renderer.end_layer();
            renderer.end_layer();
        };
        let mut partial = renderer();
        partial.track_frame_damage(true);
        let target = surface_texture(&partial);
        panes(&mut partial, 0.4);
        let _ = partial.present_damaged(Color::TRANSPARENT, &target, &viewport, false);
        panes(&mut partial, 0.6);
        let _ = partial.present_damaged(Color::TRANSPARENT, &target, &viewport, false);
        assert_eq!(
            partial.partial_frames(),
            1,
            "the content pane repainted in part"
        );
        let mut full = renderer();
        panes(&mut full, 0.6);
        let reference = surface_texture(&full);
        let _ = full.present_damaged(Color::TRANSPARENT, &reference, &viewport, false);
        let (differing, _) = super::difference(
            SIZE.width,
            &read(&partial, &target),
            &read(&full, &reference),
        );
        assert_eq!(
            differing, 0,
            "partial content repaint must preserve the sidebar"
        );
    }

    /// A custom primitive that tells what it paints is compared like any
    /// other paint: a frame that moves a quad beside it, or changes what it
    /// paints, is painted in part and matches a full paint; one that cannot
    /// tell paints the frame in full.
    ///
    /// Sabotage: ignore `paint` in `Layer::hash_paint` (return `None` for
    /// any primitive), or keep forcing a full frame for every primitive --
    /// the partial count does not move; drop the primitive's areas from the
    /// layer's elements -- a changed signature damages nothing.
    #[test]
    fn a_primitive_that_tells_its_paint_is_painted_in_part() {
        use crate::primitive::{Pipeline, Primitive, Renderer as _};

        #[derive(Debug)]
        struct Declared(Option<u64>);
        struct Nothing;
        impl Pipeline for Nothing {
            fn new(_: &wgpu::Device, _: &wgpu::Queue, _: wgpu::TextureFormat) -> Self {
                Nothing
            }
        }
        impl Primitive for Declared {
            type Pipeline = Nothing;
            fn prepare(
                &self,
                _: &mut Nothing,
                _: &wgpu::Device,
                _: &wgpu::Queue,
                _: &Rectangle,
                _: &Viewport,
            ) {
            }
            fn draw(&self, _: &Nothing, _: &mut wgpu::RenderPass<'_>) -> bool {
                true
            }
            fn paint(&self, bounds: &Rectangle, area: &mut dyn FnMut(u64, Rectangle)) -> bool {
                self.0.map(|signature| area(signature, *bounds)).is_some()
            }
        }
        fn frame(renderer: &mut Renderer, moved: f32, signature: Option<u64>) {
            renderer.reset(Rectangle::with_size(Size::new(
                SIZE.width as f32,
                SIZE.height as f32,
            )));
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(Point::new(20.0 + moved, 20.0), Size::new(30.0, 30.0)),
                    ..renderer::Quad::default()
                },
                Background::Color(Color::from_rgb(0.9, 0.2, 0.2)),
            );
            renderer.draw_primitive(
                Rectangle::new(Point::new(300.0, 200.0), Size::new(60.0, 40.0)),
                Declared(signature),
            );
        }
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        // (quad moved by, signature, painted in part)
        let frames = [
            (0.0, Some(1), false),
            (4.0, Some(1), true),
            (4.0, Some(2), true),
            (8.0, None, false),
            (8.0, Some(2), false),
            (12.0, Some(2), true),
        ];
        for (index, (moved, signature, part)) in frames.into_iter().enumerate() {
            let partial = window.partial_frames();
            frame(&mut window, moved, signature);
            let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
            assert_eq!(window.partial_frames() > partial, part, "frame {index}");
            let mut full = renderer();
            full.track_frame_damage(true);
            frame(&mut full, moved, signature);
            let full_target = surface_texture(&full);
            let _ = full.present_damaged(Color::BLACK, &full_target, &viewport, false);
            let (differing, _) = super::difference(
                SIZE.width,
                &read(&window, &target),
                &read(&full, &full_target),
            );
            assert_eq!(differing, 0, "frame {index}: {differing} pixels differ");
        }
    }

    /// A window that shrinks right after a partial frame is painted whole at
    /// its new size: no region or scissor of the larger frame survives into
    /// it (a stale scissor past the smaller target is a validation panic).
    ///
    /// Sabotage: keep `paint_regions` when the damage is `Full` in
    /// `present_damaged` -- the next full frame draws within the old regions.
    #[test]
    fn a_partial_frame_then_a_smaller_window_paints_whole() {
        // A small square that moves, and a large panel whose colour changes:
        // over half the window, so its regions are measured and then painted
        // as a whole frame.
        let frame = |renderer: &mut Renderer, size: Size<u32>, x: f32, shade: f32| {
            renderer.reset(Rectangle::with_size(Size::new(
                size.width as f32,
                size.height as f32,
            )));
            quad(
                renderer,
                Rectangle::new(Point::new(10.0, 10.0), Size::new(400.0, 250.0)),
                Color::from_rgb(shade, 0.3, 0.3),
            );
            quad(
                renderer,
                Rectangle::new(Point::new(x, 270.0), Size::new(20.0, 20.0)),
                Color::from_rgb(0.2, 0.6, 0.9),
            );
        };
        let viewport = |size: Size<u32>| {
            Viewport::with_physical_size(
                size,
                renderer::Scale {
                    window: 1.0,
                    application: 1.0,
                },
            )
        };
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        frame(&mut window, SIZE, 400.0, 0.2);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport(SIZE), false);
        let partial = window.partial_frames();
        frame(&mut window, SIZE, 404.0, 0.8);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport(SIZE), false);
        assert_eq!(
            window.partial_frames(),
            partial,
            "most of the window changed: painted whole"
        );

        let small = Size::new(SIZE.width / 2, SIZE.height / 2);
        let smaller = sized_texture(&window, small);
        frame(&mut window, small, 20.0, 0.5);
        let _ = window.present_damaged(Color::BLACK, &smaller, &viewport(small), false);
        let mut full = renderer();
        full.track_frame_damage(true);
        frame(&mut full, small, 20.0, 0.5);
        let reference = sized_texture(&full, small);
        let _ = full.present_damaged(Color::BLACK, &reference, &viewport(small), false);
        let (differing, _) = super::difference(
            small.width,
            &read(&window, &smaller),
            &read(&full, &reference),
        );
        assert_eq!(differing, 0, "{differing} pixels differ from a whole paint");
    }

    /// A frame names for the present what it changed: the whole image for a
    /// first frame, the regions for a partial one, an empty list when
    /// nothing changed. Every pixel outside the regions is the pixel
    /// presented before, so the compositor may keep it.
    ///
    /// Sabotage: name no region for a partial frame in `present_damaged` --
    /// the moved quad's pixels change outside them.
    #[test]
    fn a_frame_names_what_it_changed_for_the_present() {
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let frame = |renderer: &mut Renderer, x: f32| {
            renderer.reset(Rectangle::with_size(Size::new(
                SIZE.width as f32,
                SIZE.height as f32,
            )));
            quad(
                renderer,
                Rectangle::new(Point::new(x, 40.5), Size::new(30.0, 30.0)),
                Color::from_rgb(0.9, 0.2, 0.2),
            );
            quad(
                renderer,
                Rectangle::new(Point::new(300.0, 200.0), Size::new(60.0, 40.0)),
                Color::from_rgb(0.2, 0.6, 0.9),
            );
        };
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        frame(&mut window, 20.25);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        assert_eq!(
            window.take_present_damage(),
            None,
            "a first frame changes everything"
        );
        let mut before = read(&window, &target);
        for x in [24.75, 60.0] {
            frame(&mut window, x);
            let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
            let named = window
                .take_present_damage()
                .expect("a partial frame names its regions");
            let after = read(&window, &target);
            let inside = |px: u32, py: u32| {
                named
                    .iter()
                    .any(|&[x, y, w, h]| px >= x && px < x + w && py >= y && py < y + h)
            };
            let width = SIZE.width;
            let outside = (0..SIZE.height)
                .flat_map(|py| (0..width).map(move |px| (px, py)))
                .filter(|&(px, py)| {
                    let at = ((py * width + px) * 4) as usize;
                    before[at..at + 4] != after[at..at + 4] && !inside(px, py)
                })
                .count();
            assert_eq!(outside, 0, "pixels changed outside {named:?}");
            let area: u32 = named.iter().map(|r| r[2] * r[3]).sum();
            assert!(area < SIZE.width * SIZE.height / 4, "{named:?}");
            before = after;
        }
        frame(&mut window, 60.0);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        assert_eq!(
            window.take_present_damage(),
            Some(Vec::new()),
            "nothing changed"
        );
    }

    /// Glass drawn into the kept canvas copies its backdrop straight out of
    /// the canvas: no scene is cleared or blitted back, and the frame matches
    /// one drawn through the scene.
    ///
    /// Sabotage: leave `direct_target` unset in `present_damaged` -- the frame
    /// blits a scene again; copy the backdrop from the scene alone in
    /// `resolve` -- the glass falls back to its opaque fill and the pixels
    /// differ.
    #[test]
    fn glass_in_the_canvas_takes_no_scene() {
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let label = paragraph("Short");
        let draw = |renderer: &mut Renderer| {
            widget_frame(renderer, 0.4, 1, &label);
            quad(
                renderer,
                Rectangle::new(Point::new(400.0, 150.0), Size::new(40.0, 40.0)),
                Color::from_rgb(0.9, 0.3, 0.3),
            );
            renderer.start_layer_material(
                Rectangle::new(Point::new(360.0, 120.0), Size::new(110.0, 100.0)),
                crate::core::material::Material {
                    radii: [12.0; 4],
                    blur_sigma: 6.0,
                    refraction: 1.0,
                    bevel_width: 10.0,
                    index_of_refraction: 1.5,
                    dispersion: 0.0,
                    color_diffusion: 0.0,
                    color_reach: 0.0,
                    optical_depth: 10.0,
                    tint: Color::WHITE,
                    tint_opacity: 0.5,
                    saturation: 1.0,
                    highlight: 0.0,
                    brightness: 0.0,
                    light_angle: 225.0,
                    profile_curvature: 2.5,
                    contrast_protection: 0.0,
                    protected_inset: 0.0,
                    probe: 0,
                    measure_only: false,
                },
            );
            renderer.end_layer();
        };

        let mut canvas = renderer();
        canvas.track_frame_damage(true);
        let target = surface_texture(&canvas);
        draw(&mut canvas);
        let _ = canvas.present_damaged(Color::BLACK, &target, &viewport, false);
        let statistics = canvas.material_statistics();
        assert_eq!(statistics.composites, 1, "the glass was composited");
        assert_eq!(statistics.fallback_composites, 0);
        assert_eq!(statistics.blits, 0, "no scene was blitted back");

        let mut scene = renderer();
        let reference = surface_texture(&scene);
        draw(&mut scene);
        let view = reference.create_view(&wgpu::TextureViewDescriptor::default());
        let _ = scene.present_measured(Some(Color::BLACK), &view, &viewport, false);
        assert_eq!(
            scene.material_statistics().blits,
            1,
            "the reference takes the scene"
        );

        let (differing, _) = super::difference(
            SIZE.width,
            &read(&canvas, &target),
            &read(&scene, &reference),
        );
        assert_eq!(differing, 0, "{differing} pixels differ");
    }

    /// A fully transparent image paints nothing, so it is not drawn: a frame
    /// with one is the frame without it.
    ///
    /// Sabotage: drop the opacity check in `draw_image` -- the transparent
    /// image is an element of its layer and the frame changes.
    #[cfg(feature = "image")]
    #[test]
    fn a_transparent_image_is_not_drawn() {
        use crate::core::image::Renderer as _;

        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        let handle = crate::core::image::Handle::from_rgba(1, 1, vec![255; 4]);
        let frame = |window: &mut Renderer, opacity: Option<f32>| {
            window.reset(Rectangle::with_size(Size::new(
                SIZE.width as f32,
                SIZE.height as f32,
            )));
            window.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(Point::new(10.0, 10.0), Size::new(40.0, 40.0)),
                    ..renderer::Quad::default()
                },
                Background::Color(Color::WHITE),
            );
            if let Some(opacity) = opacity {
                let image = crate::core::Image {
                    opacity,
                    ..crate::core::Image::new(handle.clone())
                };
                let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 300.0));
                window.draw_image(image, bounds, bounds);
            }
        };
        frame(&mut window, None);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        frame(&mut window, Some(0.0));
        assert!(
            window.unchanged_frame(Color::BLACK, &viewport),
            "a transparent image"
        );
        frame(&mut window, Some(0.5));
        assert!(
            !window.unchanged_frame(Color::BLACK, &viewport),
            "a visible image"
        );
    }

    /// A quad that paints nothing is not drawn: a transparent fill or
    /// gradient without border or shadow leaves the frame as it was.
    ///
    /// Sabotage: drop the check in `fill_quad` -- the transparent quads are
    /// elements of their layer and the frame changes.
    #[test]
    fn a_transparent_quad_is_not_drawn() {
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        let full = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 300.0));
        let frame = |window: &mut Renderer, extra: Option<Background>| {
            window.reset(Rectangle::with_size(Size::new(
                SIZE.width as f32,
                SIZE.height as f32,
            )));
            window.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(Point::new(10.0, 10.0), Size::new(40.0, 40.0)),
                    ..renderer::Quad::default()
                },
                Background::Color(Color::WHITE),
            );
            if let Some(background) = extra {
                window.fill_quad(
                    renderer::Quad {
                        bounds: full,
                        ..renderer::Quad::default()
                    },
                    background,
                );
            }
        };
        let gradient = |alpha: f32| {
            Background::Gradient(
                crate::core::gradient::Linear::new(0.0)
                    .add_stop(0.0, Color::from_rgba(0.0, 0.0, 0.0, alpha))
                    .add_stop(1.0, Color::from_rgba(0.0, 0.0, 0.0, alpha))
                    .into(),
            )
        };
        frame(&mut window, None);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        frame(&mut window, Some(Background::Color(Color::TRANSPARENT)));
        assert!(
            window.unchanged_frame(Color::BLACK, &viewport),
            "a transparent fill"
        );
        frame(&mut window, Some(gradient(0.0)));
        assert!(
            window.unchanged_frame(Color::BLACK, &viewport),
            "a transparent gradient"
        );
        frame(&mut window, Some(gradient(0.3)));
        assert!(
            !window.unchanged_frame(Color::BLACK, &viewport),
            "a visible gradient"
        );
    }

    /// A frame drawn by widgets that changed nothing needs no present at
    /// all: [`unchanged_frame`](crate::Renderer::unchanged_frame) says so
    /// for it alone, and the frame after a skipped one, measured ahead, is
    /// still painted to match a full paint. A present that could not follow
    /// the measurement leaves the next frame painted in full.
    ///
    /// Sabotage: answer `true` for any partial damage, or keep the canvas in
    /// `discard_frame_damage` -- a changed frame is skipped or the frame after
    /// a failed present is painted in part over a canvas it never reached.
    #[test]
    fn a_widget_frame_that_changed_nothing_is_not_presented() {
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let background = Color::BLACK;
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        let label = paragraph("Label");
        // (shade, moved, unchanged, acquisition fails)
        let frames: [(f32, usize, bool, bool); 7] = [
            (0.4, 0, false, false),
            (0.4, 0, true, false),
            (0.4, 1, false, false),
            (0.4, 1, true, false),
            (0.6, 1, false, true),
            (0.6, 2, false, false),
            (0.6, 2, true, false),
        ];
        for (index, (shade, moved, unchanged, fails)) in frames.into_iter().enumerate() {
            widget_frame(&mut window, shade, moved, &label);
            assert_eq!(
                window.unchanged_frame(background, &viewport),
                unchanged,
                "frame {index}"
            );
            if unchanged {
                continue;
            }
            if fails {
                window.discard_frame_damage();
                continue;
            }
            let full = window.full_frames();
            let _ = window.present_damaged(background, &target, &viewport, false);
            if index == 5 {
                assert_eq!(
                    window.full_frames(),
                    full + 1,
                    "painted in full after a failed present"
                );
            }
            let (expected, _) = full_frame(background, &viewport, shade, moved, &label);
            let (differing, _) = super::difference(SIZE.width, &read(&window, &target), &expected);
            assert_eq!(differing, 0, "frame {index}: {differing} pixels differ");
        }
    }

    /// A frame drawn by widgets with glass on screen is painted in part when
    /// nothing it changed is under the glass or near enough for its blur to
    /// sample, and in full when something is: the glass is composited from
    /// what is behind it. Glass whose own look changes is painted again too.
    ///
    /// Sabotage: paint in part whatever the glass samples -- the frame that
    /// changes a tile under the glass keeps the old blur; or leave a
    /// material's parameters out of its layer's signature -- the new tint
    /// is never painted.
    #[test]
    fn glass_is_painted_again_only_when_its_backdrop_changed() {
        let glass = crate::core::material::Material {
            radii: [12.0; 4],
            blur_sigma: 6.0,
            refraction: 1.0,
            bevel_width: 10.0,
            index_of_refraction: 1.5,
            dispersion: 0.0,
            color_diffusion: 0.0,
            color_reach: 0.0,
            optical_depth: 10.0,
            tint: Color::WHITE,
            tint_opacity: 0.5,
            saturation: 1.0,
            highlight: 0.0,
            brightness: 0.0,
            light_angle: 225.0,
            profile_curvature: 2.5,
            contrast_protection: 0.0,
            protected_inset: 0.0,
            probe: 0,
            measure_only: false,
        };
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let label = paragraph("Short");
        // The page, with glass over its right half and a tile under it.
        let draw = |renderer: &mut Renderer, moved: usize, under: f32, tint: f32| {
            widget_frame(renderer, 0.4, moved, &label);
            quad(
                renderer,
                Rectangle::new(Point::new(400.0, 150.0), Size::new(40.0, 40.0)),
                Color::from_rgb(under, 0.3, 0.3),
            );
            renderer.start_layer_material(
                Rectangle::new(Point::new(360.0, 120.0), Size::new(110.0, 100.0)),
                crate::core::material::Material {
                    tint_opacity: tint,
                    ..glass
                },
            );
            renderer.end_layer();
        };
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        // (moved, tile under the glass, glass tint, painted in part)
        for (index, (moved, under, tint, partial)) in [
            (0, 0.3, 0.5, false),
            (1, 0.3, 0.5, true),
            (1, 0.9, 0.5, false),
            (1, 0.9, 0.9, false),
        ]
        .into_iter()
        .enumerate()
        {
            let painted = window.partial_frames();
            draw(&mut window, moved, under, tint);
            let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
            if index > 0 {
                assert_eq!(
                    window.partial_frames() > painted,
                    partial,
                    "frame {index} painted in part"
                );
            }
            let mut full = renderer();
            full.track_frame_damage(true);
            draw(&mut full, moved, under, tint);
            let full_target = surface_texture(&full);
            let _ = full.present_damaged(Color::BLACK, &full_target, &viewport, false);
            let (differing, _) = super::difference(
                SIZE.width,
                &read(&window, &target),
                &read(&full, &full_target),
            );
            assert_eq!(differing, 0, "frame {index}: {differing} pixels differ");
        }
    }

    /// A frame whose change covers most of the window is painted in full,
    /// once: the regions measured before it went full are not each painted
    /// again.
    ///
    /// Sabotage: keep the measured regions when a frame goes full -- the full
    /// paint is drawn region by region.
    #[test]
    fn a_frame_changed_over_half_is_painted_once() {
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let label = paragraph("Short");
        let mut window = renderer();
        window.track_frame_damage(true);
        let target = surface_texture(&window);
        widget_frame(&mut window, 0.4, 0, &label);
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        // The page behind everything changes colour.
        let window_rect = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
        window.reset(window_rect);
        quad(&mut window, window_rect, Color::from_rgb(0.5, 0.2, 0.2));
        let full = window.full_frames();
        let _ = window.present_damaged(Color::BLACK, &target, &viewport, false);
        assert_eq!(window.full_frames(), full + 1, "the change went full");
        assert!(
            window.paint_regions.is_empty(),
            "a full frame kept {} regions to paint it by",
            window.paint_regions.len()
        );
    }

    /// Round dots turned by moving them land where a composite turning
    /// them would: the replay need not composite a spinner.
    ///
    /// Sabotage: turn the dots the other way, or about the wrong centre --
    /// they land elsewhere.
    #[test]
    fn turned_dots_land_where_a_composite_puts_them() {
        let window = Rectangle::with_size(Size::new(SIZE.width as f32, SIZE.height as f32));
        let viewport = Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        );
        let paint = |turns: bool, turned: usize| {
            let mut renderer = renderer();
            renderer.turns_geometry = turns;
            renderer.reset(window);
            assert!(renderer.start_recording(window));
            dots(&mut renderer);
            let recording = renderer.finish_recording().expect("the dots are recorded");
            renderer.reset(window);
            assert!(renderer.replay_tracks(&recording, &tracks(0, turned)));
            let target = surface_texture(&renderer);
            let _ = renderer.present_damaged(Color::BLACK, &target, &viewport, false);
            read(&renderer, &target)
        };
        // The brightness-weighted centre of each sixth of the ring.
        let centres = |pixels: &[u8]| -> Vec<Point> {
            let mut sums = [(0.0_f32, 0.0_f32, 0.0_f32); 6];
            for (index, pixel) in pixels.chunks(4).enumerate() {
                let weight = f32::from(pixel[1]);
                if weight < 8.0 {
                    continue;
                }
                let (x, y) = (
                    (index as u32 % SIZE.width) as f32 + 0.5,
                    (index as u32 / SIZE.width) as f32 + 0.5,
                );
                let angle = (y - DOTS_CENTER.y)
                    .atan2(x - DOTS_CENTER.x)
                    .rem_euclid(std::f32::consts::TAU);
                let sector = ((angle / std::f32::consts::TAU * 6.0 + 0.5) as usize) % 6;
                sums[sector].0 += weight * x;
                sums[sector].1 += weight * y;
                sums[sector].2 += weight;
            }
            sums.iter()
                .map(|(x, y, w)| Point::new(x / w, y / w))
                .collect()
        };
        for turned in [1, 2, 5] {
            let moved = centres(&paint(true, turned));
            let composited = centres(&paint(false, turned));
            for (moved, composited) in moved.iter().zip(&composited) {
                assert!(
                    moved.distance(*composited) < 0.35,
                    "turn {turned}: a dot moved to {moved:?}, composited to {composited:?}"
                );
            }
        }
    }
}
