// Modified by enzUI contributors. See FORK.md.
//! Platform specific settings for Linux.

/// The platform specific window settings of an application.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlatformSpecific {
    /// Sets the application id of the window.
    ///
    /// As a best practice, it is suggested to select an application id that match
    /// the basename of the application’s .desktop file.
    pub application_id: String,

    /// Whether bypass the window manager mapping for x11 windows
    ///
    /// This flag is particularly useful for creating UI elements that need precise
    /// positioning and immediate display without window manager interference.
    pub override_redirect: bool,

    /// Ask the compositor to decorate the window, and draw none of winit's
    /// own decorations when it declines: the application then draws its own
    /// title bar and frame, following
    /// [`window::decorations`](crate::window::decorations) (enzICED). On X11
    /// the window gets no window-manager decorations.
    pub app_drawn_decorations: bool,

    /// Restore this window's placement from an earlier run, where the Wayland
    /// compositor offers `xdg_session_manager_v1` (enzICED). Read the id to
    /// store for next time with `window::wayland_session_id` once the window
    /// is open.
    pub session: Option<SessionToplevel>,

    /// The drop shadow around a window that draws its own frame, from its
    /// first frame (enzICED); change it later with `window::set_shadow`.
    pub shadow: Option<crate::window::Shadow>,
}

/// A window's place in the window session (enzICED).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionToplevel {
    /// The session id stored from the last run; `None` starts a session.
    pub session_id: Option<String>,
    /// This window's name in the session, unique among the open windows,
    /// such as `"main"` or `"document-3"`.
    pub toplevel: String,
}
