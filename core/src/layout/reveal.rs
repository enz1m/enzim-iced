// Modified by enzUI contributors. See FORK.md.
//! Retained-child measurement and visible reservation for size reveals.

use super::{Limits, Node};
use crate::{Rectangle, Size};

/// Allocation policy independent of the current physical clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// Measure under loose limits and hold selected axes at a reservation fraction.
    Size {
        /// Width and height axes respectively; at least one must be selected.
        axes: [bool; 2],
        /// Unit-interval fraction held until the native layout commits settlement.
        reservation: f32,
        /// Release the held reservation when this endpoint is actually presented.
        target: bool,
    },
    /// Measure under the original limits and continuously reserve a fixed resting width.
    Panel {
        /// Fully open reservation in logical pixels, independent of child compression.
        resting: f32,
    },
}
impl Rule {
    /// Rejects nonfinite dimensions, out-of-range fractions and empty axis selections.
    pub fn is_valid(self) -> bool {
        match self {
            Self::Size {
                axes, reservation, ..
            } => {
                axes.into_iter().any(|axis| axis)
                    && reservation.is_finite()
                    && (0.0..=1.0).contains(&reservation)
            }
            Self::Panel { resting } => resting.is_finite() && resting >= 0.0,
        }
    }

    /// Constraints used to remeasure the retained child at each live parent width.
    pub fn child_limits(self, limits: &Limits) -> Limits {
        match self {
            Self::Size { .. } => limits.loose(),
            Self::Panel { .. } => *limits,
        }
    }

    /// Reserves space without scaling the child's layout or paint metrics.
    pub fn finish(self, child: Node, progress: f32) -> Node {
        let full = child.size();
        let size = match self {
            Self::Size {
                axes,
                reservation,
                target,
            } => {
                let at_target = if target {
                    progress >= 1.0 - f32::EPSILON
                } else {
                    progress <= f32::EPSILON
                };
                let reservation = if at_target { progress } else { reservation };
                Size::new(
                    full.width * if axes[0] { reservation } else { 1.0 },
                    full.height * if axes[1] { reservation } else { 1.0 },
                )
            }
            Self::Panel { resting } => Size::new(resting * progress, full.height),
        };
        Node::with_children(size, vec![child])
    }

    /// Physical reveal inside the reserved allocation; a zero pin preserves native zero geometry.
    pub fn visible(self, mut bounds: Rectangle, progress: f32) -> Rectangle {
        if let Self::Size {
            axes, reservation, ..
        } = self
            && reservation > f32::EPSILON
        {
            let fraction = (progress / reservation).clamp(0.0, 1.0);
            if axes[0] {
                bounds.width *= fraction;
            }
            if axes[1] {
                bounds.height *= fraction;
            }
        }
        bounds
    }
}
