// Modified by enzUI contributors. See FORK.md.
//! Shared native and owned allocation for driver-bound sizes and retained content.

use super::{Limits, Node};
use crate::{Length, Size};

/// Retained-content allocation policies; size changes reflow children without bitmap scaling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// Reflows a fixed retained subtree between two explicit sizes in logical pixels.
    Resize {
        /// Allocation at progress zero, in logical pixels.
        from: Size,
        /// Allocation at progress one, before fitting to the parent's limits.
        to: Size,
        /// Finite unit-interval interpolation value; native replay never runs a callback.
        progress: f32,
    },
    /// Reserves a fraction of a fixed width; the child keeps the full width.
    ///
    /// The siblings reflow against the reserved width every frame, while the
    /// child is laid out once at `resting` and shown through the reservation,
    /// so a busy panel is not re-measured at every intermediate width.
    Reserve {
        /// Fully open width in logical pixels.
        resting: f32,
        /// Current fraction of that width.
        progress: f32,
        /// Whether the child's end edge stays on the reservation's end edge
        /// (a panel sliding in from the start), rather than its start edge on
        /// the reservation's start edge.
        align_end: bool,
    },
    /// Measures retained content under the parent's remaining available size.
    Content {
        /// Native child sizing, including Fill and Shrink.
        sizing: Size<Length>,
    },
}
impl Rule {
    /// Resolves a declared size range; progress outside the unit interval is not admissible for replay.
    pub fn resized(from: Size, to: Size, progress: f32) -> Size {
        Size::new(
            from.width + (to.width - from.width) * progress,
            from.height + (to.height - from.height) * progress,
        )
    }
    /// Constraints passed to the ordinary child layout, also used by owned replay.
    pub fn child_limits(self, limits: &Limits) -> Limits {
        match self {
            Self::Resize { from, to, progress } => {
                let size = Self::resized(from, to, progress);
                limits
                    .width(Length::Fixed(size.width))
                    .height(Length::Fixed(size.height))
            }
            Self::Reserve { resting, .. } => limits.width(Length::Fixed(resting.max(0.0))),
            Self::Content { .. } => Limits::new(Size::ZERO, limits.max()),
        }
    }
    /// Wraps an already measured child without changing its glyph or paint metrics.
    pub fn finish(self, limits: &Limits, child: Node) -> Node {
        let size = match self {
            Self::Resize { .. } => self.child_limits(limits).max(),
            Self::Reserve {
                resting,
                progress,
                align_end,
            } => {
                let size = Size::new(
                    (resting * progress).min(limits.max().width),
                    child.size().height.min(limits.max().height),
                );
                let child = if align_end {
                    let x = size.width - child.size().width;
                    child.move_to(crate::Point::new(x, 0.0))
                } else {
                    child
                };
                return Node::with_children(size, vec![child]);
            }
            Self::Content { sizing } => {
                let size = limits.resolve(sizing.width, sizing.height, child.size());
                Size::new(size.width.min(limits.max().width), size.height)
            }
        };
        Node::with_children(size, vec![child])
    }
}
