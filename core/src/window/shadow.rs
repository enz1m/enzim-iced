// Modified by enzUI contributors. See FORK.md.
//! The drop shadow around a window that draws its own frame (enzICED).
use crate::{Color, Vector};

/// A drop shadow the platform paints around a window whose application draws
/// its own frame, outside the window's bounds as the compositor knows them.
///
/// The compositor's idea of the window (snapping, tiling, an active-window
/// border or highlight) stays the window itself; the shadow is not part of it.
/// A thin band of it next to each edge resizes the window; the rest lets
/// input through. It is left out on edges the window is tiled against and
/// while maximized or fullscreen. Wayland only: X11 and other platforms ignore
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    /// The window's corner radius in logical pixels, which the shadow follows.
    pub corner_radius: f32,
    /// The layers while the window is active, drawn first to last.
    pub active: Vec<ShadowLayer>,
    /// The layers while another window is active.
    pub inactive: Vec<ShadowLayer>,
    /// How far past each edge, in logical pixels, a press resizes the window.
    pub resize_border: f32,
}

/// One layer of a window [`Shadow`], with CSS `box-shadow` meaning: the
/// window's rounded outline moved by `offset`, grown by `spread` and blurred
/// by `blur`, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowLayer {
    /// The layer's colour.
    pub color: Color,
    /// How far the layer is moved.
    pub offset: Vector,
    /// The blur radius; the edge fades over about this distance.
    pub blur: f32,
    /// How far the outline grows before blurring.
    pub spread: f32,
}
