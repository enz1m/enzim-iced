// Modified by enzUI contributors. See FORK.md.
//! Owned scalar expressions and responsive placement for retained paint and text.
//!
//! Programs contain no widget state or callbacks. Fixed paint keeps its measured
//! size, text requests intrinsic layout from owned text resources, and media uses
//! native fit/crop/rotation rules. Responsive allocation does not scale document text.

mod clip;
mod container;
mod editor;
mod flex;
mod flow;
mod input;
mod media;
mod overlay;
mod pair;
mod priority_band;
mod responsive;
mod sample;
mod tab_strip;
pub use sample::{Sample, SampleBudget};
mod scroll;
pub mod scroll_paint;

use std::collections::BTreeSet;
use std::sync::Arc;

/// The largest expression/node population admitted in one program.
pub const MAX_ITEMS: usize = 16_384;
/// The maximum retained layout nesting depth.
pub const MAX_DEPTH: usize = 64;
/// Total native child measurements per sample, including repeated intrinsic probes.
pub const MAX_LAYOUT_VISITS: usize = MAX_ITEMS * 8;

/// A native viewport-culling gate whose material remains available for later visibility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cull {
    /// Separate physical scope identity; it must not alias a layout or paint identity.
    pub id: u64,
    /// Allocation whose bounds decide visibility.
    pub owner: u64,
    /// Absolute source allocation before scroll translation.
    pub recorded: Bounds,
    /// Also narrow the logical viewport passed to descendants, without cropping arbitrary quad paint.
    pub clip: bool,
}

/// Complete retained material with width-dependent membership and independently reflowed contents.
#[derive(Debug, Clone, PartialEq)]
pub struct TabStrip {
    /// Captured visible allocation in recording coordinates.
    pub recorded: Bounds,
    /// Complete capture extent, including undisplayed material variants.
    pub source: Bounds,
    /// Outer-width expression; descendants otherwise inherit their assigned width.
    pub width: Option<usize>,
    /// Native constraints at capture; live parent constraints override them.
    pub limits: crate::layout::Limits,
    /// Callback-free membership and allocation policy.
    pub policy: crate::layout::item_strip::Policy,
    /// Original sizing of every prepared material slot.
    pub children: Vec<crate::Size<crate::Length>>,
}

/// Logical-pixel geometry in the recording's coordinate space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bounds {
    /// Leading edge in logical pixels.
    pub x: f32,
    /// Top edge in logical pixels.
    pub y: f32,
    /// Nonnegative horizontal extent.
    pub width: f32,
    /// Nonnegative vertical extent.
    pub height: f32,
}

impl Bounds {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f32::is_finite)
            && self.width >= 0.0
            && self.height >= 0.0
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
    }

    /// Covers both finite rectangles, rounding outward when distant origins lose endpoint precision.
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        let mut width = right - x;
        let mut height = bottom - y;
        if x + width < right {
            width = width.next_up();
        }
        if y + height < bottom {
            height = height.next_up();
        }
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Intersects finite, nonnegative rectangles; disjoint inputs yield an empty rectangle.
    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x).min(self.x + self.width);
        let y = self.y.max(other.y).min(self.y + self.height);
        Self {
            x,
            y,
            width: ((self.x + self.width).min(other.x + other.width) - x).max(0.0),
            height: ((self.y + self.height).min(other.y + other.height) - y).max(0.0),
        }
    }

    /// Whether the complete rectangle is inside this clip.
    pub fn contains(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }
}

/// One renderer-sampled driver's scalar channels; bits in `available` identify valid slots.
#[derive(Debug, Clone, Copy)]
pub struct Input {
    /// The driver's stable node identity, distinct from layout node identities.
    pub node: u64,
    /// Translation x/y, opacity, scale, and optional clip x/y/width/height.
    pub values: [f32; 8],
    /// A set bit makes the corresponding scalar channel available.
    pub available: u8,
}

/// An expression index refers only to earlier entries in the expression vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Expression {
    /// A finite immutable scalar.
    Constant(f32),
    /// The current surface's logical width.
    ViewportWidth,
    /// The current surface's logical height.
    ViewportHeight,
    /// A scalar sampled by the renderer's existing animation clock.
    Driver {
        /// Nonzero identity of a clock driver, distinct from painted nodes.
        node: u64,
        /// Zero-based scalar slot in `Input::values`; its availability bit must be set.
        channel: u8,
    },
    /// Sum of two preceding expressions.
    Add(usize, usize),
    /// Difference of two preceding expressions.
    Subtract(usize, usize),
    /// Product of two preceding expressions.
    Multiply(usize, usize),
    /// Smaller of two preceding expressions.
    Min(usize, usize),
    /// Larger of two preceding expressions.
    Max(usize, usize),
}

impl Expression {
    fn references(self) -> Option<(usize, usize)> {
        match self {
            Self::Add(a, b)
            | Self::Subtract(a, b)
            | Self::Multiply(a, b)
            | Self::Min(a, b)
            | Self::Max(a, b) => Some((a, b)),
            _ => None,
        }
    }
}

/// Child placement direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Leading-to-trailing placement.
    Horizontal,
    /// Top-to-bottom placement.
    Vertical,
}

/// Whether intrinsic text may exceed its layout allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOverflow {
    /// Refuse intrinsic dimensions outside the allocation.
    Refuse,
    /// Preserve text and use captured scene clips, as ordinary text drawing does.
    ///
    /// Admission requires the program's clip to equal the recording coverage.
    SceneClip,
}

/// Layout expressions evaluated without consulting the widget tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    /// An explicit allocation-relative clipping scope around one independently owned child.
    Clip(Arc<Clip>),
    /// Transparent reservation or retained-content allocation with a fully owned child.
    Responsive(Arc<Responsive>),
    /// A native image allocation backed by a closed renderer-owned resource.
    Media(Arc<Media>),
    /// A viewport whose content reflows under owned scroll state.
    Scroll(Arc<Scroll>),
    /// A single-line input whose text, caret, icon and clipping share native geometry.
    Input(Arc<InputGeometry>),
    /// A complete editor viewport, including glyphs and decorations.
    Editor(Arc<Editor>),
    /// Childless or single-child geometry measured by its declared native allocation order.
    Container(Arc<Container>),
    /// Native line wrapping with independently measured child resources.
    Wrapping(Arc<Wrapping>),
    /// Shared native flex constraints, measured with owned child resources.
    Flex(Arc<Flex>),
    /// Overlapping children whose base determines the remaining layers' constraints.
    Overlay(Arc<Overlay>),
    /// Two cells whose allocation switches between declared row and stacked arrangements.
    Pair(Arc<Pair>),
    /// Four native slots with control priority and whole-target content clipping.
    Band(Arc<Band>),
    /// Complete document and control variants selected by a pure width-dependent policy.
    TabStrip(Arc<TabStrip>),
    /// Retained paint at its original size. Width-dependent opaque layout cannot use this leaf.
    Fixed {
        /// Fully captured logical paint footprint; its dimensions remain fixed during placement.
        recorded: Bounds,
    },
    /// A zero-area native leaf with no captured paint resources.
    /// Any change to its shaping constraints requires a fresh capture.
    UnpaintedLeaf {
        /// Captured logical allocation, including the nonzero axis used for sibling placement.
        recorded: Bounds,
        /// Exact shaping limits for which the allocation is known to have no area.
        maximum: crate::Size,
    },
    /// A retained paragraph reflowed by the backend before sibling placement.
    Text {
        /// Original logical allocation, used to anchor retained paint after reflow.
        recorded: Bounds,
        /// Maximum-width expression; `None` uses the available parent width.
        width: Option<usize>,
        /// Allocated-height expression; `None` uses the intrinsic reflowed height.
        height: Option<usize>,
        /// Shrink to intrinsic width instead of occupying the maximum width.
        shrink_width: bool,
        /// Maximum shaping height; separate from an explicitly allocated height.
        max_height: Option<usize>,
        /// Whether captured scene clipping may contain overflowing glyphs.
        overflow: TextOverflow,
    },
    /// Children in one line; overflowing allocations are refused rather than squeezed.
    Stack {
        /// Direction in which siblings advance, with leading-edge cross-axis alignment.
        axis: Axis,
        /// Outer-width expression; `None` fills the available width.
        width: Option<usize>,
        /// Outer-height expression; `None` uses the measured stack height.
        height: Option<usize>,
        /// Nonnegative main-axis spacing expression, in logical pixels.
        gap: usize,
    },
    /// Transparent insets around exactly one child.
    Padding {
        /// Outer-width expression; `None` fills the available width.
        width: Option<usize>,
        /// Outer-height expression; `None` includes the child's measured height and vertical insets.
        height: Option<usize>,
        /// Left/top/right/bottom expression indices, each producing nonnegative logical pixels.
        insets: [usize; 4],
    },
    /// Rows of fixed paint, with independent column and row gaps.
    Flow {
        /// Outer-width expression; `None` fills the available width.
        width: Option<usize>,
        /// Outer-height expression; `None` uses the measured wrapped height.
        height: Option<usize>,
        /// Nonnegative spacing expression between items in one row, in logical pixels.
        column_gap: usize,
        /// Nonnegative spacing expression between wrapped rows, in logical pixels.
        row_gap: usize,
    },
}

/// A native wrapper whose reservation can consume a sampled layout driver.
#[derive(Debug, Clone, PartialEq)]
pub struct Responsive {
    /// Original constraints; inherited parent constraints take precedence.
    pub limits: crate::layout::Limits,
    /// The same allocation rule used by the native widget.
    pub rule: crate::layout::responsive::Rule,
    /// Native child sizing, used during intrinsic measurement.
    pub child: crate::Size<crate::Length>,
    /// Stable reservation driver; absent for ordinary retained content.
    pub driver: Option<u64>,
    /// Lowered progress expression, distinct from width in logical pixels.
    pub progress: Option<usize>,
}

/// Immutable single-line input shape and paint state; contains no live widget references.
#[derive(Debug, Clone, PartialEq)]
pub struct InputGeometry {
    /// Original native allocation in logical pixels.
    pub recorded: Bounds,
    /// Parent limits before the input applies its own width and padding.
    pub limits: crate::layout::Limits,
    /// Native line, padding, and icon allocation policy.
    pub shape: crate::layout::input::Shape,
    /// Copied alignment and caret geometry for the captured draw revision.
    pub paint: crate::layout::input::Paint,
    /// Optional sampled root maximum width.
    pub width: Option<usize>,
}

/// Native wrapping constraints; all children remain independently owned.
#[derive(Debug, Clone, PartialEq)]
pub struct Wrapping {
    /// Parent constraints before native wrapping applies its own sizing.
    pub limits: crate::layout::Limits,
    /// Line packing, spacing and alignment in logical coordinates.
    pub rule: crate::layout::flow::Rule,
    /// Native child sizing in source order, including empty items.
    pub children: Vec<crate::Size<crate::Length>>,
    /// Optional sampled root maximum width.
    pub width: Option<usize>,
}

/// Immutable native flex constraints; children are measured by the existing widget flex algorithm.
#[derive(Debug, Clone, PartialEq)]
pub struct Flex {
    /// Charges gaps only between children whose main-axis extent exceeds `f32::EPSILON`.
    pub collapse_spacing: bool,
    /// Unbounded main-axis measurement; `None` retains ordinary flex allocation.
    pub sequence: Option<crate::layout::flex::SequenceFlow>,
    /// Optional edge-priority reservation around a residual-width middle slot.
    pub bookends: Option<crate::layout::flex::Bookends>,
    /// Main-axis ordering; cross-axis alignment is applied after intrinsic measurement.
    pub axis: Axis,
    /// Replaces the captured maximum width with a sampled expression at the root.
    pub width: Option<usize>,
    /// Parent limits captured before the container applies its own length constraints.
    pub limits: crate::layout::Limits,
    /// The container's declared width and height behavior.
    pub sizing: crate::Size<crate::Length>,
    /// Insets applied before measuring children.
    pub padding: crate::Padding,
    /// Nonnegative spacing expression between children.
    pub gap: usize,
    /// Cross-axis alignment within the resolved container extent.
    pub align: crate::Alignment,
    /// Constraints captured in the same child order as the program node's children.
    pub children: Vec<crate::Size<crate::Length>>,
}

/// Immutable four-slot policy shared with the native priority band.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    /// Replaces the captured maximum width when the band is the program root.
    pub width: Option<usize>,
    /// Parent constraints before applying the band's own height.
    pub limits: crate::layout::Limits,
    /// Measurement order, reservations, and bounded deficit negotiation.
    pub policy: crate::layout::priority_band::Specification,
    /// Original allocation before ancestor scroll translations.
    pub recorded: Bounds,
    /// Original whole-target content clip in the same coordinates as the allocation.
    pub recorded_content_clip: Bounds,
    /// Length declarations in content, centre, actions, controls order.
    pub children: Vec<crate::Size<crate::Length>>,
    /// At most three native probe results; differing constraints require a fresh native layout.
    pub probes: Vec<crate::layout::priority_band::Probe>,
}

/// A clamped native inset driven independently of viewport width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inset {
    /// Exact native arithmetic and logical-pixel dimensions.
    pub policy: crate::layout::inset::Policy,
    /// Nonpainted progress owner; it must be supplied on every owned sample.
    pub driver: u64,
    /// Lowered expression index, absent only before program construction.
    pub progress: Option<usize>,
    /// Native capture progress, used to validate the original padding.
    pub captured: f32,
    /// Right padding and end alignment instead of left padding and start alignment.
    pub trailing: bool,
}

/// Owned childless or single-child allocation; paint requires separate renderer support.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    /// Below this width a retained composition must be rebuilt by its native owner.
    pub reflow_min_width: Option<f32>,
    /// Optional progress-dependent inset; ordinary padding remains constant.
    pub inset: Option<Inset>,
    /// Narrows descendant text viewports to the allocation without adding a GPU clip layer.
    pub clip_content: bool,
    /// Native allocation order, shared by ordinary layout and replay.
    pub mode: crate::layout::container::Mode,
    /// Captured allocation eligible for owned quad paint; `None` declares layout only.
    pub recorded: Option<Bounds>,
    /// Replaces the captured maximum width with a sampled expression at the root.
    pub width: Option<usize>,
    /// Parent limits before applying the container's lengths.
    pub limits: crate::layout::Limits,
    /// Declared outer allocation behavior.
    pub sizing: crate::Size<crate::Length>,
    /// Requested insets, fitted according to the allocation mode.
    pub padding: crate::Padding,
    /// Horizontal and vertical alignment within the resolved padded space.
    pub alignment: [crate::Alignment; 2],
    /// The child's declared width and height; unused for childless atomic allocation.
    pub child: crate::Size<crate::Length>,
}

/// Native child allocation with a clip expanded by fixed logical-pixel outsets.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Driven reservation and retained material; absent for an ordinary clip.
    pub reveal: Option<Reveal>,
    /// Original logical allocation before ancestor scrolling.
    pub recorded: Bounds,
    /// Native parent constraints.
    pub limits: crate::layout::Limits,
    /// Child lengths, forwarded without padding or compression changes.
    pub child: crate::Size<crate::Length>,
    /// Clip expansion in logical pixels; not layout padding.
    pub outset: crate::Padding,
    /// Optional sampled root width.
    pub width: Option<usize>,
}

/// A normalized reveal driver never shares the identity of a physical paint track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reveal {
    /// Wrapper lengths, which may differ from the retained child's lengths.
    pub sizing: crate::Size<crate::Length>,
    /// Shared native and retained measurement policy.
    pub rule: crate::layout::reveal::Rule,
    /// Captured unit-interval progress, used only when no active driver is declared.
    pub captured: f32,
    /// Stable normalized progress identity while travelling.
    pub driver: Option<u64>,
    /// Lowered channel-zero expression; populated by layout admission.
    pub progress: Option<usize>,
    /// Complete child coverage before ancestor scrolling, not viewport authority.
    pub source: Bounds,
}

/// Owned constraints for overlapping layers; their order remains the native paint order.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    /// Parent limits before stack lengths are resolved.
    pub limits: crate::layout::Limits,
    /// Declared outer lengths.
    pub sizing: crate::Size<crate::Length>,
    /// The child measured first to determine the common allocation.
    pub base: usize,
    /// Child lengths in paint order, not measurement order.
    pub children: Vec<crate::Size<crate::Length>>,
    /// Optional sampled maximum-width expression at the root.
    pub width: Option<usize>,
}

/// A complete native pair policy with independently captured children.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    /// Parent constraints before applying the pair's width policy.
    pub limits: crate::layout::Limits,
    /// Both arrangements, including the strict width threshold.
    pub policy: crate::layout::pair::Policy,
    /// Native child lengths in leading/trailing order.
    pub children: [crate::Size<crate::Length>; 2],
    /// Optional sampled maximum-width expression at the root.
    pub width: Option<usize>,
}

/// Native editor allocation constraints; backend measurement owns the complete paint resource.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    /// Captured logical viewport before reflow.
    pub recorded: Bounds,
    /// Optional sampled maximum-width expression for a root editor.
    pub width: Option<usize>,
    /// Parent allocation limits before editor lengths are applied.
    pub limits: crate::layout::Limits,
    /// Declared viewport sizing.
    pub sizing: crate::Size<crate::Length>,
    /// Text insets inside the outer viewport.
    pub padding: crate::Padding,
}

/// The renderer resource class that must match a media declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// Decoded RGBA pixels.
    Raster,
    /// A resolved vector tree, not a path that replay may reopen.
    Vector,
}

/// Native media allocation; the matching draw binds its immutable resource revision.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// Original allocation before ancestor scrolling.
    pub recorded: Bounds,
    /// Optional root maximum-width expression.
    pub width: Option<usize>,
    /// Parent constraints before widget length resolution.
    pub limits: crate::layout::Limits,
    /// Shared native fit/crop/rotation behavior.
    pub geometry: crate::layout::media::Media,
    /// Required captured resource class.
    pub kind: MediaKind,
}

/// Native scroll allocation; paint and input are captured at draw time.
#[derive(Debug, Clone, PartialEq)]
pub struct Scroll {
    /// Captured viewport allocation.
    pub recorded: Bounds,
    /// Captured child allocation before scroll translation.
    pub recorded_content: Bounds,
    /// Paint identity for the overlay rails and thumbs.
    pub chrome: u64,
    /// Optional root width expression.
    pub width: Option<usize>,
    /// Parent allocation limits.
    pub limits: crate::layout::Limits,
    /// Declared viewport lengths.
    pub sizing: crate::Size<crate::Length>,
    /// Child lengths before measurement.
    pub child: crate::Size<crate::Length>,
    /// Enabled axes and rail sizing.
    pub config: crate::layout::scroll::Config,
}

/// A node's children precede it in the program and have exactly one parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique allocation identity in this program and its matching recording.
    pub id: u64,
    /// Allocation and placement recipe.
    pub layout: Layout,
    /// Child indices in paint/layout order.
    pub children: Vec<usize>,
}

/// Additional placement/opacity changes driven by arbitrary scalar expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Extra horizontal placement in logical pixels.
    OffsetX,
    /// Extra vertical placement in logical pixels.
    OffsetY,
    /// Group opacity multiplier in the inclusive range zero to one.
    Opacity,
}

/// Associates one retained paint leaf with a scalar expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Index of a retained paint or text leaf.
    pub node: usize,
    /// The affected property.
    pub target: Target,
    /// Index of the scalar expression.
    pub expression: usize,
}

/// A refusal is observable; no stale or endpoint-scaled geometry is substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// This renderer cannot own and evaluate a presentation program.
    UnsupportedBackend,
    /// No outermost recording is open, or it already owns a program.
    RecordingState,
    /// An attached program requires named-track replay, not translation-only replay.
    TrackReplayRequired,
    /// Paint contains callbacks or mutable widget text that cannot be independently replayed.
    OpaquePaint,
    /// Retained glyphs could not be bound to owned font assets.
    FontAssets,
    /// A recorded media resource is unavailable, outside its envelope or not closed for replay.
    MediaAssets,
    /// Docked travel cannot transfer without full-shell layout and presented-input ownership.
    DockedOwnershipRequired,
    /// The declared text node does not identify independently reflowable retained text.
    TextIdentity(u64),
    /// Expression, node, edge, binding, or depth budget exceeded.
    Budget,
    /// A forward/out-of-range expression reference or invalid driver declaration.
    Expression(usize),
    /// Invalid topology, empty identity, or malformed geometry.
    Node(usize),
    /// Reused node identity would alias independent content.
    DuplicateNode(u64),
    /// Binding does not name a paint/text leaf and existing expression exactly once.
    Binding(usize),
    /// A required driver/channel was not sampled for this frame.
    MissingDriver(u64),
    /// Driver inputs are not sorted by unique node identity.
    InputOrder,
    /// Runtime arithmetic or viewport input produced a nonfinite value.
    NonFinite,
    /// A nonpositive viewport cannot support presentation.
    Viewport,
    /// The allocation would resize/reflow a fixed paint leaf.
    ReflowRequired(u64),
    /// Paint would escape the clip whose ownership was recorded.
    ClipCoverage(u64),
    /// The recording does not contain a declared leaf, or contains a driver as paint.
    SceneIdentity(u64),
}

/// Immutable geometry/property data published atomically with retained paint.
#[derive(Debug)]
pub struct Program {
    culling: Vec<Option<Cull>>,
    expressions: Vec<Expression>,
    nodes: Vec<Node>,
    bindings: Vec<Binding>,
    origin: [usize; 2],
    clip: Bounds,
    layout_inputs: Vec<bool>,
    leaves: Vec<usize>,
    paint_nodes: Vec<u64>,
    node_ids: Vec<u64>,
    indices: Vec<(u64, usize)>,
    ancestors: Vec<Option<usize>>,
    scroll_scoped: Vec<bool>,
    clip_scoped: Vec<bool>,
    containers: Vec<(u64, usize)>,
    drivers: BTreeSet<u64>,
}

impl Program {
    /// Validates a bounded expression DAG and a single postordered layout tree.
    ///
    /// `origin` names expressions for the root's x/y; `clip` is the fixed recording
    /// coverage within which placements are supported. The last node is the root.
    ///
    /// # Errors
    /// Returns an explicit topology, identity, geometry or budget refusal.
    pub fn new(
        expressions: Vec<Expression>,
        nodes: Vec<Node>,
        bindings: Vec<Binding>,
        origin: [usize; 2],
        clip: Bounds,
    ) -> Result<Self, Error> {
        Self::new_with_culling(expressions, nodes, bindings, origin, clip, Vec::new())
    }

    /// Validates a program with separately identified native visibility scopes.
    ///
    /// # Errors
    /// Refuses duplicate or unknown owners, aliased scope identities, invalid bounds, or budget overflow.
    pub fn new_with_culling(
        expressions: Vec<Expression>,
        nodes: Vec<Node>,
        bindings: Vec<Binding>,
        origin: [usize; 2],
        clip: Bounds,
        culling: Vec<Cull>,
    ) -> Result<Self, Error> {
        if culling.len() > MAX_ITEMS.saturating_sub(nodes.len()) {
            return Err(Error::Budget);
        }
        let mut by_owner = std::collections::BTreeMap::new();
        for cull in culling {
            if cull.id == 0 || !cull.recorded.valid() || by_owner.insert(cull.owner, cull).is_some()
            {
                return Err(Error::SceneIdentity(cull.id));
            }
        }
        let culling: Vec<_> = nodes.iter().map(|node| by_owner.remove(&node.id)).collect();
        if let Some((owner, _)) = by_owner.first_key_value() {
            return Err(Error::SceneIdentity(*owner));
        }
        if expressions.len() > MAX_ITEMS
            || nodes.len() > MAX_ITEMS
            || bindings.len() > MAX_ITEMS
            || nodes.iter().map(|node| node.children.len()).sum::<usize>() > MAX_ITEMS
        {
            return Err(Error::Budget);
        }
        if expressions.is_empty() || nodes.is_empty() || !clip.valid() {
            return Err(Error::Node(0));
        }
        let mut drivers = BTreeSet::new();
        for (index, expression) in expressions.iter().enumerate() {
            match *expression {
                Expression::Constant(value) if !value.is_finite() => {
                    return Err(Error::Expression(index));
                }
                Expression::Driver { node, channel } => {
                    if node == 0 || node >= (1 << 63) || channel >= 8 {
                        return Err(Error::Expression(index));
                    }
                    let _ = drivers.insert(node);
                }
                expression => {
                    if expression
                        .references()
                        .is_some_and(|(a, b)| a >= index || b >= index)
                    {
                        return Err(Error::Expression(index));
                    }
                }
            }
        }
        let mut identities = BTreeSet::new();
        let mut ancestors = vec![None; nodes.len()];
        let mut scroll_scoped = vec![false; nodes.len()];
        let mut clip_scoped = vec![false; nodes.len()];
        for (index, node) in nodes.iter().enumerate().rev() {
            if culling[index].is_some()
                || matches!(&node.layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Material)
            {
                clip_scoped[index] = true;
            }
            for &child in &node.children {
                if child >= index || ancestors[child].is_some() {
                    return Err(Error::Node(index));
                }
                ancestors[child] = Some(index);
                scroll_scoped[child] =
                    scroll_scoped[index] || matches!(node.layout, Layout::Scroll(_));
                clip_scoped[child] = clip_scoped[index]
                    || matches!(node.layout, Layout::Clip(_))
                    || matches!(&node.layout, Layout::TabStrip(strip) if strip.policy.clips_to_bounds())
                    || matches!(&node.layout, Layout::Container(container) if container.mode == crate::layout::container::Mode::Material)
                    || (matches!(node.layout, Layout::Band(_))
                        && node.children.first() == Some(&child));
            }
        }
        let mut parents = vec![0usize; nodes.len()];
        let mut depths = vec![1usize; nodes.len()];
        let mut leaves = Vec::new();
        let mut layout_inputs = vec![false; expressions.len()];
        let mut require = |index: usize| {
            if let Some(slot) = layout_inputs.get_mut(index) {
                *slot = true;
                Ok(())
            } else {
                Err(Error::Expression(index))
            }
        };
        require(origin[0])?;
        require(origin[1])?;
        for (index, node) in nodes.iter().enumerate() {
            if node.id == 0 || drivers.contains(&node.id) {
                return Err(Error::Node(index));
            }
            if !identities.insert(node.id) {
                return Err(Error::DuplicateNode(node.id));
            }
            for &child in &node.children {
                if child >= index {
                    return Err(Error::Node(index));
                }
                parents[child] += 1;
                if parents[child] != 1 {
                    return Err(Error::Node(child));
                }
                depths[index] = depths[index].max(depths[child] + 1);
            }
            if depths[index] > MAX_DEPTH {
                return Err(Error::Budget);
            }
            match node.layout {
                Layout::Scroll(ref value) => {
                    if node.children.len() != 1
                        || !value.recorded.valid()
                        || !value.recorded_content.valid()
                        || !value.config.valid()
                        || ((!scroll_scoped[index] && !clip_scoped[index])
                            && !clip.contains(value.recorded))
                    {
                        return Err(Error::Node(index));
                    }
                    scroll::validate(value).map_err(|_| Error::Node(index))?;
                    if let Some(width) = value.width {
                        require(width)?;
                    }
                }
                Layout::Media(ref value) => {
                    if !node.children.is_empty()
                        || !value.recorded.valid()
                        || !value.geometry.is_resolved()
                        || ((!scroll_scoped[index] && !clip_scoped[index])
                            && !clip.contains(value.recorded))
                    {
                        return Err(Error::Node(index));
                    }
                    media::validate(value).map_err(|_| Error::Node(index))?;
                    if let Some(width) = value.width {
                        require(width)?;
                    }
                    leaves.push(index);
                }
                Layout::Input(ref value) => {
                    input::validate(value, node.children.len()).map_err(|_| Error::Node(index))?;
                    let _ = input::parts_of(&nodes, index)?;
                    if (!scroll_scoped[index] && !clip_scoped[index])
                        && !clip.contains(value.recorded)
                    {
                        return Err(Error::Node(index));
                    }
                    if let Some(width) = value.width {
                        require(width)?;
                    }
                }
                Layout::Editor(ref editor) => {
                    if !node.children.is_empty()
                        || !editor.recorded.valid()
                        || ((!scroll_scoped[index] && !clip_scoped[index])
                            && !clip.contains(editor.recorded))
                    {
                        return Err(Error::Node(index));
                    }
                    editor::validate(editor).map_err(|_| Error::Node(index))?;
                    if let Some(width) = editor.width {
                        require(width)?;
                    }
                    leaves.push(index);
                }
                Layout::Responsive(ref value) => {
                    responsive::validate(value, node.children.len())
                        .map_err(|_| Error::Node(index))?;
                    if let Some(progress) = value.progress {
                        require(progress)?;
                    }
                }
                Layout::Container(ref container) => {
                    if container.recorded.is_some_and(|bounds| {
                        !bounds.valid()
                            || ((!scroll_scoped[index] && !clip_scoped[index])
                                && !clip.contains(bounds))
                    }) {
                        return Err(Error::Node(index));
                    }
                    if container.mode == crate::layout::container::Mode::Material
                        && !ancestors[index].is_some_and(|parent| matches!(&nodes[parent].layout, Layout::TabStrip(strip) if strip.policy.wrapped()))
                    { return Err(Error::Node(index)); }
                    container::validate(container, node.children.len())
                        .map_err(|_| Error::Node(index))?;
                    if let Some(width) = container.width {
                        require(width)?;
                    }
                    if let Some(inset) = container.inset {
                        let progress = inset.progress.ok_or(Error::Node(index))?;
                        if !matches!(expressions.get(progress), Some(Expression::Driver { node, channel: 0 }) if *node == inset.driver)
                        {
                            return Err(Error::Node(index));
                        }
                        require(progress)?;
                    }
                }
                Layout::Clip(ref value) => {
                    clip::validate(value, node.children.len()).map_err(|_| Error::Node(index))?;
                    if !value.recorded.valid() {
                        return Err(Error::Node(index));
                    }
                    if let Some(reveal) = value.reveal
                        && let Some(driver) = reveal.driver
                    {
                        let progress = reveal.progress.ok_or(Error::Node(index))?;
                        if !matches!(expressions.get(progress), Some(Expression::Driver { node, channel: 0 }) if *node == driver)
                        {
                            return Err(Error::Node(index));
                        }
                        require(progress)?;
                    }
                    if let Some(width) = value.width {
                        require(width)?;
                    }
                }
                Layout::TabStrip(ref tabs) => {
                    if tabs.policy.wrapped() && node.children.iter().any(|&child| !matches!(&nodes[child].layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Material)) { return Err(Error::Node(index)); }
                    tab_strip::validate(tabs, node.children.len())
                        .map_err(|_| Error::Node(index))?;
                    if let Some(width) = tabs.width {
                        require(width)?;
                    }
                }
                Layout::Band(ref band) => {
                    priority_band::validate(band, node.children.len())
                        .map_err(|_| Error::Node(index))?;
                    if let Some(width) = band.width {
                        require(width)?;
                    }
                }
                Layout::Pair(ref pair) => {
                    pair::validate(pair, node.children.len()).map_err(|_| Error::Node(index))?;
                    if let Some(width) = pair.width {
                        require(width)?;
                    }
                }
                Layout::Overlay(ref overlay) => {
                    overlay::validate(overlay, node.children.len())
                        .map_err(|_| Error::Node(index))?;
                    if let Some(width) = overlay.width {
                        require(width)?;
                    }
                }
                Layout::Wrapping(ref value) => {
                    flow::validate(value, node.children.len()).map_err(|_| Error::Node(index))?;
                    if let Some(width) = value.width {
                        require(width)?;
                    }
                }
                Layout::Flex(ref flex) => {
                    flex::validate(flex, node.children.len()).map_err(|_| Error::Node(index))?;
                    if let Some(width) = flex.width {
                        require(width)?;
                    }
                    require(flex.gap)?;
                }
                Layout::UnpaintedLeaf { recorded, maximum } => {
                    if !recorded.valid()
                        || !node.children.is_empty()
                        || maximum.width.is_nan()
                        || maximum.height.is_nan()
                        || maximum.width < 0.0
                        || maximum.height < 0.0
                        || !((recorded.width == 0.0 && maximum.width == 0.0)
                            || (recorded.height == 0.0 && maximum.height == 0.0))
                    {
                        return Err(Error::Node(index));
                    }
                }
                Layout::Fixed { recorded } => {
                    if !recorded.valid()
                        || ((!scroll_scoped[index] && !clip_scoped[index])
                            && !clip.contains(recorded))
                        || !node.children.is_empty()
                    {
                        return Err(Error::Node(index));
                    }
                    leaves.push(index);
                }
                Layout::Text {
                    recorded,
                    width,
                    height,
                    max_height,
                    ..
                } => {
                    if !recorded.valid()
                        || ((!scroll_scoped[index] && !clip_scoped[index])
                            && !clip.contains(recorded))
                        || !node.children.is_empty()
                    {
                        return Err(Error::Node(index));
                    }
                    if let Some(width) = width {
                        require(width)?;
                    }
                    if let Some(height) = height {
                        require(height)?;
                    }
                    if let Some(max_height) = max_height {
                        require(max_height)?;
                    }
                    leaves.push(index);
                }
                Layout::Stack {
                    width, height, gap, ..
                } => {
                    if let Some(width) = width {
                        require(width)?;
                    }
                    if let Some(height) = height {
                        require(height)?;
                    }
                    require(gap)?;
                }
                Layout::Padding {
                    width,
                    height,
                    insets,
                } => {
                    if node.children.len() != 1 {
                        return Err(Error::Node(index));
                    }
                    if let Some(width) = width {
                        require(width)?;
                    }
                    if let Some(height) = height {
                        require(height)?;
                    }
                    for inset in insets {
                        require(inset)?;
                    }
                }
                Layout::Flow {
                    width,
                    height,
                    column_gap,
                    row_gap,
                } => {
                    if let Some(width) = width {
                        require(width)?;
                    }
                    if let Some(height) = height {
                        require(height)?;
                    }
                    require(column_gap)?;
                    require(row_gap)?;
                }
            }
        }
        if parents[..parents.len() - 1].iter().any(|count| *count != 1) {
            return Err(Error::Node(0));
        }
        let mut bound = BTreeSet::new();
        for (index, binding) in bindings.iter().enumerate() {
            if binding.node >= nodes.len()
                || !matches!(
                    nodes[binding.node].layout,
                    Layout::Fixed { .. } | Layout::Text { .. }
                )
                || binding.expression >= expressions.len()
                || !bound.insert((binding.node, binding.target as u8))
            {
                return Err(Error::Binding(index));
            }
        }
        let mut paint_nodes: Vec<_> = leaves.iter().map(|&leaf| nodes[leaf].id).collect();
        let mut indices: Vec<_> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();
        for (index, node) in nodes.iter().enumerate() {
            if let Layout::Scroll(value) = &node.layout {
                if value.chrome == 0
                    || drivers.contains(&value.chrome)
                    || !identities.insert(value.chrome)
                {
                    return Err(Error::DuplicateNode(value.chrome));
                }
                paint_nodes.extend([node.id, value.chrome]);
                indices.push((value.chrome, index));
            }
        }
        if paint_nodes.len() > MAX_ITEMS {
            return Err(Error::Budget);
        }
        for (index, cull) in culling
            .iter()
            .enumerate()
            .filter_map(|(index, cull)| cull.as_ref().map(|cull| (index, cull)))
        {
            if drivers.contains(&cull.id) || !identities.insert(cull.id) {
                return Err(Error::DuplicateNode(cull.id));
            }
            indices.push((cull.id, index));
        }
        indices.sort_unstable_by_key(|&(id, _)| id);
        paint_nodes.sort_unstable();
        let mut containers: Vec<_> = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                (matches!(&node.layout, Layout::Container(value) if value.mode != crate::layout::container::Mode::Material) || matches!(node.layout, Layout::Input(_))).then_some((node.id, index))
            })
            .collect();
        containers.sort_unstable_by_key(|&(node, _)| node);
        Ok(Self {
            culling,
            expressions,
            nodes,
            bindings,
            origin,
            clip,
            layout_inputs,
            leaves,
            paint_nodes,
            node_ids: identities.into_iter().collect(),
            indices,
            ancestors,
            scroll_scoped,
            clip_scoped,
            containers,
            drivers,
        })
    }

    /// Visibility scope attached to the exact allocation identity, excluding scope aliases.
    pub fn node_cull(&self, node: u64) -> Option<&Cull> {
        self.culling[self.index(node)?]
            .as_ref()
            .filter(|cull| cull.owner == node)
    }

    /// Metadata for a distinct native visibility scope.
    pub fn cull_source(&self, scope: u64) -> Option<&Cull> {
        self.culling[self.index(scope)?]
            .as_ref()
            .filter(|cull| cull.id == scope)
    }

    /// Whether a viewport exactly matches expanded source coverage of a declared enclosing owner.
    /// The backend separately checks ordinary logical viewports and active scope reentry.
    pub fn allows_source_capture_viewport(&self, node: u64, viewport: Bounds) -> bool {
        let Some(index) = self.index(node) else {
            return false;
        };
        if !viewport.valid() {
            return false;
        }
        if viewport == self.clip
            && self.layer_ancestors(node).iter().any(|source| {
                !matches!(
                    source,
                    super::LayoutLayer::Cull(_) | super::LayoutLayer::Inherited
                )
            })
        {
            return true;
        }
        let union = |source: Bounds| source.union(self.clip);
        let mut at = self.ancestors[index];
        while let Some(parent) = at {
            let coverage = match &self.nodes[parent].layout {
                Layout::TabStrip(strip) => Some(union(strip.source)),
                Layout::Scroll(scroll) => Some(union(scroll.recorded_content)),
                Layout::Clip(value) => value.reveal.map_or_else(
                    || clip::expanded(value.recorded, value.outset).ok().map(union),
                    |reveal| Some(union(reveal.source)),
                ),
                _ => None,
            };
            if coverage == Some(viewport) {
                return true;
            }
            at = self.ancestors[parent];
        }
        false
    }

    /// Checks declared leaves and nonpainted drivers against backend-recorded node membership.
    ///
    /// `painted` must be sorted and deduplicated by the recording backend.
    pub fn validate_scene(&self, painted: &[u64], clip: Bounds) -> Result<(), Error> {
        if !clip.contains(self.clip) {
            return Err(Error::ClipCoverage(self.nodes.last().unwrap().id));
        }
        for &leaf in &self.leaves {
            let id = self.nodes[leaf].id;
            if matches!(
                self.nodes[leaf].layout,
                Layout::Text {
                    overflow: TextOverflow::SceneClip,
                    ..
                }
            ) && self.clip != clip
            {
                return Err(Error::ClipCoverage(id));
            }
            if painted.binary_search(&id).is_err() {
                return Err(Error::SceneIdentity(id));
            }
        }
        for (id, scroll) in self.scroll_nodes() {
            for paint in [id, scroll.chrome] {
                if painted.binary_search(&paint).is_err() {
                    return Err(Error::SceneIdentity(paint));
                }
            }
        }
        for &driver in &self.drivers {
            if painted.binary_search(&driver).is_ok() {
                return Err(Error::SceneIdentity(driver));
            }
        }
        Ok(())
    }

    /// Whether the allocation may own layout-bound quad paint; material assignment slots cannot.
    pub fn is_container(&self, node: u64) -> bool {
        self.containers
            .binary_search_by_key(&node, |&(id, _)| id)
            .is_ok()
    }

    /// All nonpainted driver identities required by an owned sample, sorted and unique.
    pub fn layout_drivers(&self) -> impl Iterator<Item = u64> + '_ {
        self.drivers.iter().copied()
    }

    /// Whether a native reservation or inset consumes this driver as layout progress.
    pub fn drives_responsive(&self, driver: u64) -> bool {
        self.nodes.iter().any(|node| {
            matches!(&node.layout, Layout::Responsive(value) if value.driver == Some(driver) && value.progress.is_some())
                || matches!(&node.layout, Layout::Container(value) if value.inset.is_some_and(|inset| inset.driver == driver && inset.progress.is_some()))
        })
    }

    /// Captured allocation for a container eligible for owned quad paint.
    pub fn container_recorded(&self, node: u64) -> Option<Bounds> {
        let index = self
            .containers
            .binary_search_by_key(&node, |&(id, _)| id)
            .ok()?;
        match &self.nodes[self.containers[index].1].layout {
            Layout::Container(value) => value.recorded,
            Layout::Input(value) => Some(value.recorded),
            _ => unreachable!(),
        }
    }

    /// Whether this program owns an allocation identity, including transparent groups.
    pub fn owns_node(&self, node: u64) -> bool {
        self.node_ids.binary_search(&node).is_ok()
    }

    /// Identities of paint/text nodes that the backend must have retained.
    pub fn leaves(&self) -> impl Iterator<Item = u64> + '_ {
        self.leaves.iter().map(|&index| self.nodes[index].id)
    }

    /// Whether a node has a declared paint/text allocation, excluding transparent layout groups.
    pub fn owns_leaf(&self, node: u64) -> bool {
        self.paint_nodes.binary_search(&node).is_ok()
    }

    /// Text identities and original allocations whose retained paragraphs require backend layout.
    pub fn text_nodes(&self) -> impl Iterator<Item = (u64, Bounds)> + '_ {
        self.leaves
            .iter()
            .filter_map(|&index| match self.nodes[index].layout {
                Layout::Text { recorded, .. } => Some((self.nodes[index].id, recorded)),
                _ => None,
            })
    }

    /// Editor identities and declarations requiring owned viewport paint.
    pub fn editor_nodes(&self) -> impl Iterator<Item = (u64, &Editor)> + '_ {
        self.leaves
            .iter()
            .filter_map(|&index| match &self.nodes[index].layout {
                Layout::Editor(value) => Some((self.nodes[index].id, value.as_ref())),
                _ => None,
            })
    }

    /// Media identities and native allocation contracts; each requires captured paint.
    pub fn media_nodes(&self) -> impl Iterator<Item = (u64, &Media)> {
        self.nodes.iter().filter_map(|node| match &node.layout {
            Layout::Media(value) => Some((node.id, value.as_ref())),
            _ => None,
        })
    }

    /// Scroll viewport identities and their allocation contracts.
    pub fn scroll_nodes(&self) -> impl Iterator<Item = (u64, &Scroll)> + '_ {
        self.nodes.iter().filter_map(|node| match &node.layout {
            Layout::Scroll(value) => Some((node.id, value.as_ref())),
            _ => None,
        })
    }
    /// Owned scroll ancestors, outermost first; chrome excludes its own content offset.
    pub fn scroll_ancestors(&self, node: u64) -> Vec<u64> {
        let Some(mut index) = self.index(node) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        while let Some(parent) = self.ancestors[index] {
            if matches!(self.nodes[parent].layout, Layout::Scroll(_)) {
                result.push(self.nodes[parent].id);
            }
            index = parent;
        }
        result.reverse();
        result
    }
    /// Whether a declaration belongs below the named allocation owner, excluding self-membership.
    pub fn contains_descendant(&self, owner: u64, node: u64) -> bool {
        let Some(mut index) = self.index(node) else {
            return false;
        };
        while let Some(parent) = self.ancestors[index] {
            if self.nodes[parent].id == owner {
                return true;
            }
            index = parent;
        }
        false
    }

    /// Exact clock of a declared input caret leaf, not of arbitrary sibling paint.
    pub fn input_blink(&self, node: u64) -> Option<crate::layout::input::Blink> {
        let leaf = self.index(node)?;
        let body = self.ancestors[leaf]?;
        let clip = self.ancestors[body]?;
        let owner = self.ancestors[clip]?;
        let Layout::Input(input) = &self.nodes[owner].layout else {
            return None;
        };
        if input::parts(self, owner).ok()?[3] != leaf {
            return None;
        }
        input.paint.blink.map(|(_, clock)| clock)
    }

    fn index(&self, node: u64) -> Option<usize> {
        self.indices
            .binary_search_by_key(&node, |&(id, _)| id)
            .ok()
            .map(|index| self.indices[index].1)
    }
    /// Whether the node names an explicit allocation-relative clip scope.
    pub fn clip_node(&self, node: u64) -> Option<&Clip> {
        match &self.nodes[self.index(node)?].layout {
            Layout::Clip(clip) => Some(clip),
            _ => None,
        }
    }

    /// The four-slot declaration owning a content clip.
    pub fn band_node(&self, node: u64) -> Option<&Band> {
        match &self.nodes[self.index(node)?].layout {
            Layout::Band(band) => Some(band),
            _ => None,
        }
    }

    /// The complete document-strip declaration for this allocation.
    pub fn tab_strip_node(&self, node: u64) -> Option<&TabStrip> {
        match &self.nodes[self.index(node)?].layout {
            Layout::TabStrip(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the identity owns a declared physical clip or material visibility mask.
    pub fn is_layer_scope_node(&self, node: u64) -> bool {
        self.clip_node(node).is_some()
            || self.band_node(node).is_some()
            || self
                .tab_strip_node(node)
                .is_some_and(|strip| strip.policy.clips_to_bounds())
            || self.material_slot(node).is_some()
            || self.cull_source(node).is_some()
    }

    /// Ordered allocation-relative clip ancestors, from outermost to innermost.
    pub fn clip_ancestors(&self, node: u64) -> Vec<u64> {
        self.layer_ancestors(node)
            .into_iter()
            .filter_map(|source| match source {
                super::LayoutLayer::Clip(node)
                | super::LayoutLayer::BandContent(node)
                | super::LayoutLayer::TabStrip(node)
                | super::LayoutLayer::Material(node)
                | super::LayoutLayer::Cull(node) => Some(node.0),
                _ => None,
            })
            .collect()
    }

    /// Ordered physical clipping scopes expected on a painted node, including scrollbar chrome.
    pub fn layer_ancestors(&self, node: u64) -> Vec<super::LayoutLayer> {
        let Some(index) = self.index(node) else {
            return Vec::new();
        };
        let mut output: Vec<_> = self.culling[index]
            .as_ref()
            .filter(|cull| cull.owner == node || matches!(&self.nodes[index].layout, Layout::Scroll(scroll) if scroll.chrome == node))
            .map(|cull| super::LayoutLayer::Cull(super::NodeId(cull.id)))
            .into_iter()
            .collect();
        let mut at = self.ancestors[index];
        let mut branch = index;
        while let Some(parent) = at {
            match &self.nodes[parent].layout {
                Layout::TabStrip(strip) if strip.policy.clips_to_bounds() => output.push(
                    super::LayoutLayer::TabStrip(super::NodeId(self.nodes[parent].id)),
                ),
                Layout::Container(container)
                    if container.mode == crate::layout::container::Mode::Material =>
                {
                    output.push(super::LayoutLayer::Material(super::NodeId(
                        self.nodes[parent].id,
                    )));
                }
                Layout::Clip(_) => output.push(super::LayoutLayer::Clip(super::NodeId(
                    self.nodes[parent].id,
                ))),
                Layout::Band(_) if self.nodes[parent].children.first() == Some(&branch) => output
                    .push(super::LayoutLayer::BandContent(super::NodeId(
                        self.nodes[parent].id,
                    ))),
                Layout::Scroll(_) => output.push(super::LayoutLayer::Scroll {
                    node: super::NodeId(self.nodes[parent].id),
                    chrome: false,
                }),
                _ => {}
            }
            if let Some(cull) = &self.culling[parent] {
                output.push(super::LayoutLayer::Cull(super::NodeId(cull.id)));
            }
            branch = parent;
            at = self.ancestors[parent];
        }
        output.reverse();
        if let Layout::Scroll(scroll) = &self.nodes[index].layout
            && scroll.chrome == node
        {
            output.push(super::LayoutLayer::Scroll {
                node: super::NodeId(self.nodes[index].id),
                chrome: true,
            });
        }
        output
    }

    /// Layout ancestors that narrow the viewport passed to child drawing.
    pub fn viewport_ancestors(&self, node: u64) -> Vec<u64> {
        let Some(mut branch) = self.index(node) else {
            return Vec::new();
        };
        let mut output = Vec::new();
        let mut at = self.ancestors[branch];
        while let Some(index) = at {
            if matches!(&self.nodes[index].layout, Layout::Clip(value) if value.reveal.is_none())
                || self.culling[index].is_some_and(|cull| cull.clip)
                || matches!(&self.nodes[index].layout, Layout::Scroll(_))
                || matches!(&self.nodes[index].layout, Layout::TabStrip(strip) if strip.policy.clips_to_bounds())
                || matches!(&self.nodes[index].layout, Layout::Container(value) if value.clip_content || value.mode == crate::layout::container::Mode::Material)
                || (matches!(&self.nodes[index].layout, Layout::Band(_))
                    && self.nodes[index].children.first() == Some(&branch))
            {
                output.push(self.nodes[index].id);
            }
            branch = index;
            at = self.ancestors[index];
        }
        output.reverse();
        output
    }

    /// Returns assignment-slot metadata; only a declared material inventory may parent such a slot.
    pub fn material_slot(&self, node: u64) -> Option<&Container> {
        match &self.nodes[self.index(node)?].layout {
            Layout::Container(value) if value.mode == crate::layout::container::Mode::Material => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Captured bounds of a viewport-owning node, before ancestor scroll translation.
    pub fn recorded_viewport(&self, node: u64) -> Option<Bounds> {
        if let Some(cull) = self.node_cull(node).filter(|cull| cull.clip) {
            return Some(cull.recorded);
        }
        match &self.nodes[self.index(node)?].layout {
            Layout::Clip(value) => value
                .reveal
                .map(|reveal| reveal.source)
                .or_else(|| clip::expanded(value.recorded, value.outset).ok()),
            Layout::Scroll(value) => Some(value.recorded),
            Layout::Band(value) => Some(value.recorded_content_clip),
            Layout::TabStrip(value) => Some(value.recorded),
            Layout::Container(value) if value.clip_content => value.recorded,
            _ => None,
        }
    }

    /// Whether the allocation is enclosed by a declared layer clip or scroll viewport.
    pub fn is_clipped_scoped(&self, node: u64) -> bool {
        self.index(node)
            .is_some_and(|i| self.scroll_scoped[i] || self.clip_scoped[i])
    }

    /// Whether this paint allocation has an explicitly owned scroll clip.
    pub fn is_scroll_scoped(&self, node: u64) -> bool {
        self.index(node)
            .is_some_and(|index| self.scroll_scoped[index])
    }

    /// The declared recording coverage; runtime placement cannot escape it.
    pub fn clip(&self) -> Bounds {
        self.clip
    }
}

/// Backend-owned text layout; implementations must not consult widget state or UI-owned locks.
pub trait TextMeasurer {
    /// Reflows one captured paragraph at a nonnegative maximum width, in logical pixels.
    ///
    /// `height` limits shaping when present; otherwise return the natural height.
    /// Returns intrinsic unscaled dimensions; allocation and overflow policy are applied separately.
    fn measure(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error>;

    /// Measures an isolated paragraph probe, allowing positive infinity as an unconstrained width.
    /// Implementations must not replace the geometry subsequently used to paint a committed child.
    fn probe(&mut self, node: u64, _width: f32, _height: Option<f32>) -> Result<[f32; 2], Error> {
        Err(Error::TextIdentity(node))
    }

    /// Reflows complete editor content inside a finite logical text viewport.
    fn measure_editor(&mut self, node: u64, _width: f32, _height: f32) -> Result<[f32; 2], Error> {
        Err(Error::TextIdentity(node))
    }
    /// Draw-time scroll state; implementations retain visibility updates between samples.
    fn scroll_input(&mut self, _node: u64) -> Result<crate::layout::scroll::Input, Error> {
        Err(Error::UnsupportedBackend)
    }
    /// Supplies the sampled viewport for a paragraph; geometry-only measurers may ignore it.
    fn text_clip(&mut self, _node: u64, _clip: Option<Bounds>) {}

    /// Updates the seed for subsequent measurements, not compositor visibility.
    fn scroll_visible(&mut self, _node: u64, _visible: bool) {}
}

struct NoText;
impl TextMeasurer for NoText {
    fn measure(&mut self, node: u64, _: f32, _: Option<f32>) -> Result<[f32; 2], Error> {
        Err(Error::TextIdentity(node))
    }
}

/// One evaluated paint leaf. Translation is relative to the retained recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Stable painted scene node.
    pub node: u64,
    /// Actual unscaled logical geometry.
    pub bounds: Bounds,
    /// Offset from recorded paint to evaluated layout.
    pub translation: [f32; 2],
    /// Opacity multiplier, independent of text layout.
    pub opacity: f32,
}

/// Work performed by the current sample, not elapsed application work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Work {
    /// Changed expression results evaluated this sample.
    pub expressions: usize,
    /// Nodes measured this sample; zero when only paint properties changed.
    pub layout_nodes: usize,
    /// Intrinsic text measurements requested during this sample.
    pub text_measurements: usize,
}

impl Work {
    fn visit_layout(&mut self) -> Result<(), Error> {
        if self.layout_nodes >= MAX_LAYOUT_VISITS {
            return Err(Error::Budget);
        }
        self.layout_nodes += 1;
        Ok(())
    }

    fn measure_text(&mut self) -> Result<(), Error> {
        if self.text_measurements >= MAX_LAYOUT_VISITS {
            return Err(Error::Budget);
        }
        self.text_measurements += 1;
        Ok(())
    }
}

// Infinite constraints are intrinsic probes; final allocations must still be finite.
fn constraint(value: f32, node: usize) -> Result<f32, Error> {
    if value.is_nan() || value < 0.0 {
        Err(Error::Node(node))
    } else {
        Ok(value)
    }
}

/// Renderer-owned evaluation storage, reused across samples and replaced on revision changes.
#[derive(Debug, Default)]
pub struct State {
    presented_input: Option<Arc<Sample>>,
    program: Option<Arc<Program>>,
    values: Vec<f32>,
    changed: Vec<bool>,
    driver_slots: Vec<usize>,
    local: Vec<Bounds>,
    absolute: Vec<Bounds>,
    unscrolled: Vec<Bounds>,
    clips: Vec<Option<Bounds>>,
    text_clips: Vec<Option<Bounds>>,
    band_clips: Vec<Bounds>,
    scroll_offsets: Vec<crate::Vector>,
    placements: Vec<Placement>,
    viewport: [f32; 2],
    text_revision: Option<Arc<()>>,
    initialized: bool,
    work: Work,
}

impl State {
    /// Replaces backend-validated input data; evaluation alone never installs it.
    pub fn set_presented_input(&mut self, sample: Option<Arc<Sample>>) {
        self.presented_input = sample;
    }

    /// Borrows input data installed by the backend after exact source validation.
    pub fn presented_input(&self) -> Option<&Arc<Sample>> {
        self.presented_input.as_ref()
    }

    /// Samples immutable declarations using values produced by the presentation clock.
    ///
    /// Inputs must be sorted by unique driver node identity. Unchanged inputs
    /// reuse placement storage; changed native constraints can allocate layout scratch.
    /// Replacements discard old revision storage.
    /// Text leaves require [`Self::sample_with_text`] and are refused here.
    pub fn sample(
        &mut self,
        program: &Arc<Program>,
        viewport: [f32; 2],
        inputs: &[Input],
    ) -> Result<&[Placement], Error> {
        if let Some((node, _)) = program
            .text_nodes()
            .next()
            .or_else(|| {
                program
                    .editor_nodes()
                    .next()
                    .map(|(node, value)| (node, value.recorded))
            })
            .or_else(|| {
                program
                    .scroll_nodes()
                    .next()
                    .map(|(id, value)| (id, value.recorded))
            })
        {
            self.initialized = false;
            self.presented_input = None;
            self.work = Work::default();
            return Err(Error::TextIdentity(node));
        }
        self.sample_inner(program, viewport, inputs, None, &mut NoText)
    }

    /// Samples with backend-owned paragraph layout, invalidating intrinsic geometry on revision changes.
    ///
    /// `revision` must identify the captured text content, not only the reusable program.
    /// Its identity remains stable while the recording is replayed; new recordings use a new Arc.
    pub fn sample_with_text(
        &mut self,
        program: &Arc<Program>,
        viewport: [f32; 2],
        inputs: &[Input],
        revision: &Arc<()>,
        text: &mut impl TextMeasurer,
    ) -> Result<&[Placement], Error> {
        self.sample_inner(program, viewport, inputs, Some(revision), text)
    }

    fn sample_inner(
        &mut self,
        program: &Arc<Program>,
        viewport: [f32; 2],
        inputs: &[Input],
        revision: Option<&Arc<()>>,
        text: &mut dyn TextMeasurer,
    ) -> Result<&[Placement], Error> {
        let was_initialized = self.initialized;
        self.initialized = false;
        self.presented_input = None;
        self.work = Work::default();
        if !viewport.into_iter().all(f32::is_finite) {
            return Err(Error::NonFinite);
        }
        if viewport[0] <= 0.0 || viewport[1] <= 0.0 {
            return Err(Error::Viewport);
        }
        if inputs.len() > MAX_ITEMS {
            return Err(Error::Budget);
        }
        if inputs.windows(2).any(|pair| pair[0].node >= pair[1].node) {
            return Err(Error::InputOrder);
        }
        let replacement = self
            .program
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, program));
        if replacement {
            *self = Self {
                program: Some(program.clone()),
                values: vec![0.0; program.expressions.len()],
                changed: vec![false; program.expressions.len()],
                driver_slots: vec![usize::MAX; program.expressions.len()],
                local: vec![Bounds::default(); program.nodes.len()],
                absolute: vec![Bounds::default(); program.nodes.len()],
                unscrolled: vec![Bounds::default(); program.nodes.len()],
                clips: vec![None; program.nodes.len()],
                text_clips: vec![None; program.nodes.len()],
                band_clips: vec![Bounds::default(); program.nodes.len()],
                scroll_offsets: vec![crate::Vector::ZERO; program.nodes.len()],
                placements: Vec::with_capacity(program.leaves.len()),
                ..Self::default()
            };
        }
        let first = replacement || !was_initialized;
        let text_changed = match (revision, self.text_revision.as_ref()) {
            (Some(next), Some(current)) => !Arc::ptr_eq(next, current),
            (None, None) => false,
            _ => true,
        };
        let mut layout_dirty = first || self.viewport != viewport || text_changed;
        for (index, expression) in program.expressions.iter().enumerate() {
            let evaluate = first
                || match *expression {
                    Expression::Constant(_) => false,
                    Expression::ViewportWidth
                    | Expression::ViewportHeight
                    | Expression::Driver { .. } => true,
                    expression => expression
                        .references()
                        .is_some_and(|(a, b)| self.changed[a] || self.changed[b]),
                };
            self.changed[index] = false;
            if !evaluate {
                continue;
            }
            let value = match *expression {
                Expression::Constant(value) => value,
                Expression::ViewportWidth => viewport[0],
                Expression::ViewportHeight => viewport[1],
                Expression::Driver { node, channel } => {
                    let mut slot = self.driver_slots[index];
                    if inputs.get(slot).is_none_or(|input| input.node != node) {
                        slot = inputs
                            .binary_search_by_key(&node, |input| input.node)
                            .map_err(|_| Error::MissingDriver(node))?;
                        self.driver_slots[index] = slot;
                    }
                    let input = inputs[slot];
                    if input.available & (1 << channel) == 0 {
                        return Err(Error::MissingDriver(node));
                    }
                    input.values[channel as usize]
                }
                Expression::Add(a, b) => self.values[a] + self.values[b],
                Expression::Subtract(a, b) => self.values[a] - self.values[b],
                Expression::Multiply(a, b) => self.values[a] * self.values[b],
                Expression::Min(a, b) => self.values[a].min(self.values[b]),
                Expression::Max(a, b) => self.values[a].max(self.values[b]),
            };
            if !value.is_finite() {
                return Err(Error::NonFinite);
            }
            self.work.expressions += 1;
            self.changed[index] = first || value.to_bits() != self.values[index].to_bits();
            self.values[index] = value;
            layout_dirty |= self.changed[index] && program.layout_inputs[index];
        }
        if layout_dirty {
            let root = program.nodes.len() - 1;
            let _ = measure(
                program,
                &self.values,
                &mut self.local,
                root,
                viewport[0],
                &mut self.work,
                text,
            )?;
            self.local[root].x = self.values[program.origin[0]];
            self.local[root].y = self.values[program.origin[1]];
            self.absolute[root] = self.local[root];
            for index in (0..program.nodes.len()).rev() {
                let parent = self.absolute[index];
                for &child in &program.nodes[index].children {
                    self.absolute[child] = Bounds {
                        x: parent.x + self.local[child].x,
                        y: parent.y + self.local[child].y,
                        ..self.local[child]
                    };
                }
            }
            self.unscrolled.copy_from_slice(&self.absolute);
            self.clips.fill(None);
            self.text_clips.fill(None);
            self.scroll_offsets.fill(crate::Vector::ZERO);
            let root_clip = program.clip.intersection(Bounds {
                width: viewport[0],
                height: viewport[1],
                ..Bounds::default()
            });
            for index in (0..program.nodes.len()).rev() {
                let mut offset = self.scroll_offsets[index];
                self.absolute[index].x -= offset.x;
                self.absolute[index].y -= offset.y;
                let parent = self.absolute[index];
                let mut clip = self.clips[index];
                let mut text_clip = self.text_clips[index];
                if let Some(cull) = &program.culling[index] {
                    let visible = text_clip.unwrap_or(root_clip).intersection(parent);
                    if visible.width == 0.0 || visible.height == 0.0 {
                        let hidden = Bounds {
                            width: 0.0,
                            height: 0.0,
                            ..root_clip
                        };
                        clip = Some(hidden);
                        text_clip = Some(hidden);
                    } else if cull.clip {
                        text_clip = Some(visible);
                    }
                }
                if let Layout::Scroll(value) = &program.nodes[index].layout {
                    let child = program.nodes[index].children[0];
                    let input = text.scroll_input(program.nodes[index].id)?;
                    if !input.valid() {
                        return Err(Error::NonFinite);
                    }
                    offset += value.config.paint_translation(
                        input,
                        crate::Size::new(parent.width, parent.height),
                        crate::Size::new(self.local[child].width, self.local[child].height),
                    );
                    let visible = text_clip.unwrap_or(root_clip).intersection(parent);
                    clip = Some(clip.unwrap_or(root_clip).intersection(visible));
                    text_clip = Some(visible);
                }
                if matches!(&program.nodes[index].layout, Layout::TabStrip(strip) if strip.policy.clips_to_bounds())
                {
                    let visible = text_clip.unwrap_or(root_clip).intersection(parent);
                    clip = Some(clip.unwrap_or(root_clip).intersection(visible));
                    text_clip = Some(visible);
                }
                if matches!(&program.nodes[index].layout, Layout::Container(container) if container.mode == crate::layout::container::Mode::Material)
                {
                    if parent.width == 0.0 && parent.height == 0.0 {
                        let hidden = Bounds {
                            x: root_clip.x,
                            y: root_clip.y,
                            width: 0.0,
                            height: 0.0,
                        };
                        clip = Some(hidden);
                        text_clip = Some(hidden);
                    } else {
                        clip = Some(clip.unwrap_or(root_clip));
                        text_clip = Some(text_clip.unwrap_or(root_clip));
                    }
                }
                if let Layout::Clip(value) = &program.nodes[index].layout {
                    let bounds = clip::visible(parent, value, &self.values)?;
                    let visible = text_clip.unwrap_or(root_clip).intersection(bounds);
                    clip = Some(clip.unwrap_or(root_clip).intersection(visible));
                    if value.reveal.is_none() {
                        text_clip = Some(visible);
                    }
                }
                if matches!(&program.nodes[index].layout, Layout::Container(container) if container.clip_content)
                {
                    text_clip = Some(text_clip.unwrap_or(root_clip).intersection(parent));
                }
                let content_clip = if matches!(&program.nodes[index].layout, Layout::Band(_)) {
                    let mut bounds = priority_band::content_clip(program, &self.unscrolled, index);
                    bounds.x -= offset.x;
                    bounds.y -= offset.y;
                    self.band_clips[index] = bounds;
                    Some(text_clip.unwrap_or(root_clip).intersection(bounds))
                } else {
                    None
                };
                if matches!(&program.nodes[index].layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Unrealized)
                {
                    let visible = root_clip
                        .intersection(clip.unwrap_or(root_clip))
                        .intersection(text_clip.unwrap_or(root_clip))
                        .intersection(parent);
                    if visible.width > 0.0 && visible.height > 0.0 {
                        return Err(Error::ReflowRequired(program.nodes[index].id));
                    }
                }
                for (slot, &child) in program.nodes[index].children.iter().enumerate() {
                    self.scroll_offsets[child] = offset;
                    let boundary =
                        content_clip.filter(|_| slot == crate::layout::priority_band::CONTENT);
                    self.clips[child] = boundary
                        .map(|bounds| clip.unwrap_or(root_clip).intersection(bounds))
                        .or(clip);
                    self.text_clips[child] = boundary.or(text_clip);
                }
            }
        }

        self.placements.clear();
        for &leaf in &program.leaves {
            if matches!(program.nodes[leaf].layout, Layout::Text { .. }) {
                text.text_clip(program.nodes[leaf].id, self.text_clips[leaf]);
            }
            let recorded = match &program.nodes[leaf].layout {
                Layout::Fixed { recorded } | Layout::Text { recorded, .. } => *recorded,
                Layout::Editor(value) => value.recorded,
                Layout::Media(value) => value.recorded,
                _ => unreachable!(),
            };
            self.placements.push(Placement {
                node: program.nodes[leaf].id,
                bounds: self.absolute[leaf],
                translation: [
                    self.absolute[leaf].x - recorded.x
                        + input::paint_offset(program, leaf, self.absolute[leaf]),
                    self.absolute[leaf].y - recorded.y,
                ],
                opacity: 1.0,
            });
        }
        for binding in &program.bindings {
            let leaf = program
                .leaves
                .binary_search(&binding.node)
                .expect("validated leaf binding");
            let value = self.values[binding.expression];
            let placement = &mut self.placements[leaf];
            match binding.target {
                Target::OffsetX => {
                    placement.translation[0] += value;
                    placement.bounds.x += value;
                }
                Target::OffsetY => {
                    placement.translation[1] += value;
                    placement.bounds.y += value;
                }
                Target::Opacity => {
                    if !(0.0..=1.0).contains(&value) {
                        return Err(Error::Binding(binding.node));
                    }
                    placement.opacity = value;
                }
            }
        }
        let viewport_bounds = Bounds {
            width: viewport[0],
            height: viewport[1],
            ..Bounds::default()
        };
        for placement in &self.placements {
            if !placement.translation.into_iter().all(f32::is_finite) {
                return Err(Error::NonFinite);
            }
            let scoped = program.index(placement.node).and_then(|index| {
                if matches!(program.nodes[index].layout, Layout::Text { .. }) {
                    self.text_clips[index].or(self.clips[index])
                } else {
                    self.clips[index]
                }
            });
            if !placement.bounds.valid()
                || scoped.is_some_and(|clip| {
                    !program.clip.contains(clip) || !viewport_bounds.contains(clip)
                })
                || (scoped.is_none()
                    && (!program.clip.contains(placement.bounds)
                        || !viewport_bounds.contains(placement.bounds)))
            {
                self.initialized = false;
                return Err(Error::ClipCoverage(placement.node));
            }
        }
        self.viewport = viewport;
        self.text_revision = revision.cloned();
        self.initialized = true;
        Ok(&self.placements)
    }

    /// Paint placements from the last successful sample; empty after a failed sample.
    pub fn placements(&self) -> &[Placement] {
        if self.initialized {
            &self.placements
        } else {
            &[]
        }
    }

    /// Physical clip sampled from a declared reveal, distinct from its reservation.
    pub fn reveal_clip(&self, node: u64) -> Option<Bounds> {
        let program = self.program.as_ref()?;
        let declaration = program.clip_node(node)?;
        let _ = declaration.reveal?;
        clip::visible(self.bounds(node)?, declaration, &self.values).ok()
    }

    /// Allocation in sampled viewport coordinates after a successful evaluation.
    pub fn bounds(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let index = self.program.as_ref()?.index(node)?;
        Some(self.absolute[index])
    }
    /// Incoming child-drawing viewport before the named node applies its own clip.
    pub fn incoming_viewport(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let program = self.program.as_ref()?;
        let index = program.index(node)?;
        Some(
            self.text_clips[index].unwrap_or(program.clip.intersection(Bounds {
                width: self.viewport[0],
                height: self.viewport[1],
                ..Bounds::default()
            })),
        )
    }

    /// Whole-target content boundary of a priority band before its incoming viewport is applied.
    pub fn band_content_clip(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let program = self.program.as_ref()?;
        let index = program.index(node)?;
        matches!(program.nodes[index].layout, Layout::Band(_)).then_some(self.band_clips[index])
    }

    /// Current layout allocation before ancestor scroll translations.
    pub fn unscrolled_bounds(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let index = self.program.as_ref()?.index(node)?;
        Some(self.unscrolled[index])
    }
    /// Sum of current ancestor scroll offsets for the allocation.
    pub fn scroll_offset(&self, node: u64) -> Option<crate::Vector> {
        if !self.initialized {
            return None;
        }
        let index = self.program.as_ref()?.index(node)?;
        Some(self.scroll_offsets[index])
    }

    /// Owned ancestor clip; `None` before success or for an allocation without one.
    pub fn node_clip(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let index = self.program.as_ref()?.index(node)?;
        self.clips[index]
    }
    /// Child allocation in sampled viewport coordinates after ancestor scroll offsets.
    pub fn scroll_content(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let program = self.program.as_ref()?;
        let index = program.index(node)?;
        Some(self.absolute[*program.nodes[index].children.first()?])
    }

    /// A container allocation from the most recent successful sample, in logical scene coordinates.
    pub fn container_bounds(&self, node: u64) -> Option<Bounds> {
        if !self.initialized {
            return None;
        }
        let program = self.program.as_ref()?;
        let index = program
            .containers
            .binary_search_by_key(&node, |&(id, _)| id)
            .ok()?;
        Some(self.absolute[program.containers[index].1])
    }

    /// Deterministic work counters for the last sample.
    pub fn work(&self) -> Work {
        self.work
    }

    /// Releases the program and all revision-specific scratch storage.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn nonnegative(value: f32, node: usize) -> Result<f32, Error> {
    if value >= 0.0 && value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Node(node))
    }
}

fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    available: f32,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    work.visit_layout()?;
    let node = &program.nodes[index];
    if let Layout::Scroll(ref declaration) = node.layout {
        return scroll::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Input(ref declaration) = node.layout {
        return input::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Editor(ref declaration) = node.layout {
        return editor::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Responsive(ref declaration) = node.layout {
        return responsive::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Container(ref declaration) = node.layout {
        return container::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Clip(ref declaration) = node.layout {
        return clip::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::TabStrip(ref declaration) = node.layout {
        return tab_strip::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Band(ref declaration) = node.layout {
        return priority_band::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Pair(ref declaration) = node.layout {
        return pair::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Overlay(ref declaration) = node.layout {
        return overlay::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Wrapping(ref declaration) = node.layout {
        return flow::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Flex(ref declaration) = node.layout {
        return flex::measure(
            program,
            values,
            boxes,
            index,
            declaration,
            available,
            None,
            work,
            text,
        );
    }
    if let Layout::Media(ref declaration) = node.layout {
        return media::measure(program, values, boxes, index, declaration, available, None);
    }
    if let Layout::UnpaintedLeaf { recorded, maximum } = node.layout {
        if available != maximum.width {
            return Err(Error::ReflowRequired(node.id));
        }
        boxes[index] = Bounds {
            width: recorded.width,
            height: recorded.height,
            ..Bounds::default()
        };
        return Ok([recorded.width, recorded.height]);
    }
    if let Layout::Fixed { recorded } = node.layout {
        if recorded.width > available {
            return Err(Error::ReflowRequired(node.id));
        }
        boxes[index] = Bounds {
            width: recorded.width,
            height: recorded.height,
            ..Bounds::default()
        };
        return Ok([recorded.width, recorded.height]);
    }
    if let Layout::Text {
        width,
        height,
        shrink_width,
        max_height,
        overflow,
        ..
    } = node.layout
    {
        let width = nonnegative(width.map_or(available, |value| values[value]), index)?;
        let height = height
            .map(|value| nonnegative(values[value], index))
            .transpose()?;
        if width > available {
            return Err(Error::ReflowRequired(node.id));
        }
        let max_height = max_height
            .map(|value| nonnegative(values[value], index))
            .transpose()?;
        let shaping_height = match (height, max_height) {
            (Some(height), Some(maximum)) => Some(height.min(maximum)),
            (height, maximum) => height.or(maximum),
        };
        work.measure_text()?;
        let intrinsic = text.measure(node.id, width, shaping_height)?;
        if intrinsic
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
            || (overflow == TextOverflow::Refuse
                && (intrinsic[0] > width
                    || shaping_height.is_some_and(|height| intrinsic[1] > height)))
        {
            return Err(Error::ReflowRequired(node.id));
        }
        let width = if shrink_width {
            intrinsic[0].min(width)
        } else {
            width
        };
        let height = height.unwrap_or(intrinsic[1]);
        let height = shaping_height.map_or(height, |maximum| height.min(maximum));
        boxes[index] = Bounds {
            width,
            height,
            ..Bounds::default()
        };
        return Ok([width, height]);
    }
    if let Layout::Padding {
        width,
        height,
        insets,
    } = node.layout
    {
        let width = nonnegative(width.map_or(available, |value| values[value]), index)?;
        let mut padding = [0.0; 4];
        for (value, expression) in padding.iter_mut().zip(insets) {
            *value = nonnegative(values[expression], index)?;
        }
        if width > available || padding[0] + padding[2] > width {
            return Err(Error::ReflowRequired(node.id));
        }
        let child = node.children[0];
        let size = measure(
            program,
            values,
            boxes,
            child,
            width - padding[0] - padding[2],
            work,
            text,
        )?;
        let natural_height = padding[1] + size[1] + padding[3];
        let height = nonnegative(height.map_or(natural_height, |value| values[value]), index)?;
        if natural_height > height {
            return Err(Error::ReflowRequired(node.id));
        }
        boxes[child].x = padding[0];
        boxes[child].y = padding[1];
        boxes[index] = Bounds {
            width,
            height,
            ..Bounds::default()
        };
        return Ok([width, height]);
    }
    let (width, height, x_gap, y_gap, axis, wraps) = match node.layout {
        Layout::Stack {
            axis,
            width,
            height,
            gap,
        } => (width, height, gap, gap, axis, false),
        Layout::Flow {
            width,
            height,
            column_gap,
            row_gap,
        } => (width, height, column_gap, row_gap, Axis::Horizontal, true),
        Layout::Fixed { .. }
        | Layout::UnpaintedLeaf { .. }
        | Layout::Text { .. }
        | Layout::Padding { .. }
        | Layout::Responsive(_)
        | Layout::Wrapping(_)
        | Layout::Flex(_)
        | Layout::Overlay(_)
        | Layout::Pair(_)
        | Layout::Band(_)
        | Layout::TabStrip(_)
        | Layout::Clip(_)
        | Layout::Container(_)
        | Layout::Input(_)
        | Layout::Editor(_)
        | Layout::Media(_)
        | Layout::Scroll(_) => {
            unreachable!()
        }
    };
    let width = nonnegative(width.map_or(available, |value| values[value]), index)?;
    let x_gap = nonnegative(values[x_gap], index)?;
    let y_gap = nonnegative(values[y_gap], index)?;
    if width > available {
        return Err(Error::ReflowRequired(node.id));
    }
    let (mut x, mut y, mut line_height, mut occupied_width, mut occupied_height) =
        (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut first = true;
    for &child in &node.children {
        let size = measure(program, values, boxes, child, width, work, text)?;
        if axis == Axis::Horizontal {
            let proposed = if first { 0.0 } else { x + x_gap };
            if wraps && !first && proposed + size[0] > width {
                x = 0.0;
                y += line_height + y_gap;
                line_height = 0.0;
            } else {
                x = proposed;
            }
            boxes[child].x = x;
            boxes[child].y = y;
            x += size[0];
            line_height = line_height.max(size[1]);
            occupied_width = occupied_width.max(x);
            occupied_height = y + line_height;
        } else {
            if !first {
                y += y_gap;
            }
            boxes[child].x = 0.0;
            boxes[child].y = y;
            y += size[1];
            occupied_width = occupied_width.max(size[0]);
            occupied_height = y;
        }
        first = false;
    }
    let height = nonnegative(height.map_or(occupied_height, |value| values[value]), index)?;
    if occupied_width > width || occupied_height > height {
        return Err(Error::ReflowRequired(node.id));
    }
    boxes[index] = Bounds {
        width,
        height,
        ..Bounds::default()
    };
    Ok([width, height])
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod default_flex_tests;
