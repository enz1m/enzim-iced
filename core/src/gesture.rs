// Modified by enzUI contributors. See FORK.md.
//! Touchpad gestures (enzICED).
use crate::Vector;

/// Where a gesture is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The fingers touched down.
    Started,
    /// The gesture changed.
    Moved,
    /// The fingers lifted.
    Ended,
    /// The gesture was interrupted and its effect should be undone.
    Cancelled,
}

/// A touchpad gesture over the window, delivered at the pointer's position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Two or more fingers pinching: `scale` is the change since the last
    /// update, positive when spreading (zoom in), so `1.0 + scale` multiplies
    /// the current zoom.
    Pinch {
        /// The relative change in scale.
        scale: f32,
        /// The gesture's phase.
        phase: Phase,
    },
    /// Fingers rotating, in degrees since the last update, clockwise.
    Rotate {
        /// The change in angle.
        degrees: f32,
        /// The gesture's phase.
        phase: Phase,
    },
    /// Three or more fingers swiping, in logical pixels since the last update.
    Swipe {
        /// The change in position.
        delta: Vector,
        /// The gesture's phase.
        phase: Phase,
    },
}
