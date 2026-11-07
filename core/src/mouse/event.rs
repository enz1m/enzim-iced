// Modified by enzUI contributors. See FORK.md.
use crate::Point;

use super::Button;

/// A mouse event.
///
/// _**Note:** This type is largely incomplete! If you need to track
/// additional events, feel free to [open an issue] and share your use case!_
///
/// [open an issue]: https://github.com/iced-rs/iced/issues
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// The mouse cursor entered the window.
    CursorEntered,

    /// The mouse cursor left the window.
    CursorLeft,

    /// The mouse cursor was moved
    CursorMoved {
        /// The new position of the mouse cursor
        position: Point,
    },

    /// A mouse button was pressed.
    ButtonPressed(Button),

    /// A mouse button was released.
    ButtonReleased(Button),

    /// The mouse wheel was scrolled.
    WheelScrolled {
        /// The scroll movement.
        delta: ScrollDelta,
    },

    /// The mouse moved by `delta`, unaccelerated, while the window holds a
    /// pointer grab (enzICED); see `window::set_cursor_grab`. A locked
    /// cursor does not move, so this is the only motion a first-person
    /// camera or a 3D view's orbit drag gets.
    RelativeMotion {
        /// The movement in the pointer's own units, roughly physical pixels.
        delta: crate::Vector,
    },
}

/// A scroll movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollDelta {
    /// A line-based scroll movement
    Lines {
        /// The number of horizontal lines scrolled
        x: f32,

        /// The number of vertical lines scrolled
        y: f32,
    },
    /// A pixel-based scroll movement.
    ///
    /// The values are **logical** (device-independent) pixels, in the same
    /// coordinate space widgets are laid out in, so a delta of `N` moves
    /// content by `N` logical pixels regardless of the window's HiDPI scale
    /// factor. Backends that report a physical delta (for example `iced_winit`
    /// on a fractional-scale Wayland output) normalize it with the window
    /// scale factor before constructing this variant; see enzUI #2831.
    Pixels {
        /// The number of horizontal logical pixels scrolled
        x: f32,
        /// The number of vertical logical pixels scrolled
        y: f32,
    },
}
