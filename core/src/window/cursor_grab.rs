// Modified by enzUI contributors. See FORK.md.
/// How a window holds the mouse pointer (enzICED); see
/// `window::set_cursor_grab`.
///
/// While grabbed, the window gets
/// [`mouse::Event::RelativeMotion`](crate::mouse::Event::RelativeMotion)
/// with every movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorGrab {
    /// The pointer moves freely.
    #[default]
    None,
    /// The pointer stays inside the window.
    Confined,
    /// The pointer stays where it is; only relative motion arrives. Hide the
    /// cursor with `mouse::Interaction::Hidden` for a first-person view.
    Locked,
}
