// Modified by enzUI contributors. See FORK.md.
//! Allocation-relative clip geometry shared by native drawing and owned replay.

use crate::{Padding, Rectangle};

/// Expands a logical allocation by nonnegative per-edge outsets.
pub fn bounds(bounds: Rectangle, outset: Padding) -> Rectangle {
    Rectangle {
        x: bounds.x - outset.left,
        y: bounds.y - outset.top,
        width: bounds.width + outset.left + outset.right,
        height: bounds.height + outset.top + outset.bottom,
    }
}
