// Modified by enzUI contributors. See FORK.md.
//! Build window-based GUI applications.
use std::any::Any;

use crate::core::time::Instant;
use crate::core::window::{
    CursorGrab, Direction, Event, Icon, Id, Level, Mode, Monitor, Screenshot, Settings,
    UserAttention,
};
use crate::core::{Point, Size, Window};
use crate::futures::Subscription;
use crate::futures::event;
use crate::futures::futures::channel::oneshot;
use crate::task::{self, Task};

/// An operation to be performed on some window.
pub enum Action {
    /// Open a new window with some [`Settings`].
    Open(Id, Settings, oneshot::Sender<Id>),

    /// Open a new window with compositor activation context.
    OpenWithActivation(
        Id,
        Settings,
        crate::core::window::ActivationToken,
        oneshot::Sender<Id>,
    ),

    /// Close the window and exits the application.
    Close(Id),

    /// Gets the [`Id`] of the oldest window.
    GetOldest(oneshot::Sender<Option<Id>>),

    /// Gets the [`Id`] of the latest window.
    GetLatest(oneshot::Sender<Option<Id>>),

    /// Move the window with the left mouse button until the button is
    /// released.
    ///
    /// There's no guarantee that this will work unless the left mouse
    /// button was pressed immediately before this function is called.
    Drag(Id),

    /// Resize the window with the left mouse button until the button is
    /// released.
    ///
    /// There's no guarantee that this will work unless the left mouse
    /// button was pressed immediately before this function is called.
    DragResize(Id, Direction),

    /// Resize the window to the given logical dimensions.
    Resize(Id, Size),

    /// Get the current logical dimensions of the window.
    GetSize(Id, oneshot::Sender<Size>),

    /// Get if the current window is maximized or not.
    GetMaximized(Id, oneshot::Sender<bool>),

    /// Set the window to maximized or back
    Maximize(Id, bool),

    /// Get if the current window is minimized or not.
    ///
    /// ## Platform-specific
    /// - **Wayland:** Always `None`.
    GetMinimized(Id, oneshot::Sender<Option<bool>>),

    /// Set the window to minimized or back
    Minimize(Id, bool),

    /// Get the current logical coordinates of the window.
    GetPosition(Id, oneshot::Sender<Option<Point>>),

    /// Get the current scale factor (DPI) of the window.
    GetScaleFactor(Id, oneshot::Sender<f32>),

    /// Move the window to the given logical coordinates.
    ///
    /// Unsupported on Wayland.
    Move(Id, Point),

    /// Change the [`Mode`] of the window.
    SetMode(Id, Mode),

    /// Get the current [`Mode`] of the window.
    GetMode(Id, oneshot::Sender<Mode>),

    /// Toggle the window to maximized or back
    ToggleMaximize(Id),

    /// Toggle whether window has decorations.
    ///
    /// ## Platform-specific
    /// - **X11:** Not implemented.
    /// - **Web:** Unsupported.
    ToggleDecorations(Id),

    /// Request user attention to the window, this has no effect if the application
    /// is already focused. How requesting for user attention manifests is platform dependent,
    /// see [`UserAttention`] for details.
    ///
    /// Providing `None` will unset the request for user attention. Unsetting the request for
    /// user attention might not be done automatically by the WM when the window receives input.
    ///
    /// ## Platform-specific
    ///
    /// - **iOS / Android / Web:** Unsupported.
    /// - **macOS:** `None` has no effect.
    /// - **X11:** Requests for user attention must be manually cleared.
    /// - **Wayland:** Requires `xdg_activation_v1` protocol, `None` has no effect.
    RequestUserAttention(Id, Option<UserAttention>),

    /// Bring the window to the front and sets input focus. Has no effect if the window is
    /// already in focus, minimized, or not visible.
    ///
    /// This method steals input focus from other applications. Do not use this method unless
    /// you are certain that's what the user wants. Focus stealing can cause an extremely disruptive
    /// user experience.
    ///
    /// ## Platform-specific
    ///
    /// - **Web / Wayland:** Unsupported.
    GainFocus(Id),

    /// Requests a compositor-mediated token from the input context of a window.
    RequestActivationToken(
        Id,
        oneshot::Sender<
            Result<crate::core::window::ActivationToken, crate::core::window::ActivationError>,
        >,
    ),

    /// Activates a window with a token received through a trusted handoff.
    Activate(
        Id,
        crate::core::window::ActivationToken,
        oneshot::Sender<Result<(), crate::core::window::ActivationError>>,
    ),

    /// Change the window [`Level`].
    SetLevel(Id, Level),

    /// Show the system menu at cursor position.
    ///
    /// ## Platform-specific
    /// Android / iOS / macOS / Orbital / Web / X11: Unsupported.
    ShowSystemMenu(Id),

    /// Get the raw identifier unique to the window.
    GetRawId(Id, oneshot::Sender<u64>),

    /// Change the window [`Icon`].
    ///
    /// On Windows and X11, this is typically the small icon in the top-left
    /// corner of the titlebar.
    ///
    /// ## Platform-specific
    ///
    /// - **Web / Wayland / macOS:** Unsupported.
    ///
    /// - **Windows:** Sets `ICON_SMALL`. The base size for a window icon is 16x16, but it's
    ///   recommended to account for screen scaling and pick a multiple of that, i.e. 32x32.
    ///
    /// - **X11:** Has no universal guidelines for icon sizes, so you're at the whims of the WM. That
    ///   said, it's usually in the same ballpark as on Windows.
    SetIcon(Id, Icon),

    /// Cast a drop shadow around a window that draws its own frame, or none
    /// (enzICED).
    SetShadow(Id, Option<crate::core::window::Shadow>),

    /// Runs the closure with a reference to the [`Window`] with the given [`Id`].
    Run(Id, Box<dyn FnOnce(&dyn Window) + Send>),

    /// Requests the window's own Wayland objects for an XDG portal parent
    /// export, type-erased so this crate stays free of `wayland-client`.
    ///
    /// The payload, when present, is `Box<(wayland_client::Connection,
    /// wayland_client::protocol::wl_surface::WlSurface, u64)>`, where the `u64`
    /// is the surface identity the windowing backend minted for that surface.
    /// It is `None` for a closed window or one that is not Wayland.
    WaylandParentExport(Id, oneshot::Sender<Option<Box<dyn Any + Send>>>),

    /// The id of the window session, to restore placement next launch (enzICED).
    GetWaylandSessionId(Id, oneshot::Sender<Option<String>>),

    /// The window's `xdg_toplevel` proxy, type-erased (enzICED).
    WaylandToplevel(Id, oneshot::Sender<Option<Box<dyn Any + Send>>>),

    /// Queue an opaque native toplevel tag, returning whether the protocol is available.
    SetToplevelTag(Id, String, oneshot::Sender<bool>),

    /// Screenshot the viewport of the window.
    Screenshot(Id, oneshot::Sender<Screenshot>),

    /// Enable mouse passthrough for the given window.
    ///
    /// This disables mouse events for the window and passes mouse events
    /// through to whatever window is underneath.
    EnableMousePassthrough(Id),

    /// Disable mouse passthrough for the given window.
    ///
    /// This enables mouse events for the window and stops mouse events
    /// from being passed to whatever is underneath.
    DisableMousePassthrough(Id),

    /// Confine or lock the pointer to the window, or let it go (enzICED).
    SetCursorGrab(Id, CursorGrab, oneshot::Sender<bool>),

    /// Set the minimum inner window size.
    SetMinSize(Id, Option<Size>),

    /// Set the maximum inner window size.
    SetMaxSize(Id, Option<Size>),

    /// Set the window to be resizable or not.
    SetResizable(Id, bool),

    /// Set the window size increment.
    SetResizeIncrements(Id, Option<Size>),

    /// Get the logical dimensions of the monitor containing the window with the given [`Id`].
    GetMonitorSize(Id, oneshot::Sender<Option<Size>>),

    /// List the connected monitors (enzICED).
    GetMonitors(oneshot::Sender<Vec<Monitor>>),

    /// Set whether the system can automatically organize windows into tabs.
    ///
    /// See <https://developer.apple.com/documentation/appkit/nswindow/1646657-allowsautomaticwindowtabbing>
    SetAllowAutomaticTabbing(bool),

    /// Redraw all the windows.
    RedrawAll,

    /// Recompute the layouts of all the windows.
    RelayoutAll,
}

/// Subscribes to the frames of the window of the running application.
///
/// The resulting [`Subscription`] will produce items at a rate equal to the
/// refresh rate of the first application window. Note that this rate may be variable, as it is
/// normally managed by the graphics driver and/or the OS.
///
/// In any case, this [`Subscription`] is useful to smoothly draw application-driven
/// animations without missing any frames.
pub fn frames() -> Subscription<Instant> {
    event::listen_raw(|event, _status, _window| match event {
        crate::core::Event::Window(Event::RedrawRequested(at)) => Some(at),
        _ => None,
    })
}

/// Subscribes to all window events of the running application.
pub fn events() -> Subscription<(Id, Event)> {
    event::listen_with(|event, _status, id| {
        if let crate::core::Event::Window(event) = event {
            Some((id, event))
        } else {
            None
        }
    })
}

/// Subscribes to all [`Event::Opened`] occurrences in the running application.
pub fn open_events() -> Subscription<Id> {
    event::listen_with(|event, _status, id| {
        if let crate::core::Event::Window(Event::Opened { .. }) = event {
            Some(id)
        } else {
            None
        }
    })
}

/// Subscribes to all [`Event::Closed`] occurrences in the running application.
pub fn close_events() -> Subscription<Id> {
    event::listen_with(|event, _status, id| {
        if let crate::core::Event::Window(Event::Closed) = event {
            Some(id)
        } else {
            None
        }
    })
}

/// Subscribes to all [`Event::Resized`] occurrences in the running application.
pub fn resize_events() -> Subscription<(Id, Size)> {
    event::listen_with(|event, _status, id| {
        if let crate::core::Event::Window(Event::Resized(size)) = event {
            Some((id, size))
        } else {
            None
        }
    })
}

/// Subscribes to all [`Event::CloseRequested`] occurrences in the running application.
pub fn close_requests() -> Subscription<Id> {
    event::listen_with(|event, _status, id| {
        if let crate::core::Event::Window(Event::CloseRequested) = event {
            Some(id)
        } else {
            None
        }
    })
}

/// Opens a new window with the given [`Settings`]; producing the [`Id`]
/// of the new window on completion.
pub fn open(settings: Settings) -> (Id, Task<Id>) {
    let id = Id::unique();

    (
        id,
        task::oneshot(|channel| crate::Action::Window(Action::Open(id, settings, channel))),
    )
}

/// Opens a new window with single-use compositor activation context.
pub fn open_with_activation(
    settings: Settings,
    token: crate::core::window::ActivationToken,
) -> (Id, Task<Id>) {
    let id = Id::unique();

    (
        id,
        task::oneshot(|channel| {
            crate::Action::Window(Action::OpenWithActivation(id, settings, token, channel))
        }),
    )
}

/// Closes the window with `id`.
pub fn close<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::Close(id)))
}

/// Gets the window [`Id`] of the oldest window.
pub fn oldest() -> Task<Option<Id>> {
    task::oneshot(|channel| crate::Action::Window(Action::GetOldest(channel)))
}

/// Gets the window [`Id`] of the latest window.
pub fn latest() -> Task<Option<Id>> {
    task::oneshot(|channel| crate::Action::Window(Action::GetLatest(channel)))
}

/// Begins dragging the window while the left mouse button is held.
pub fn drag<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::Drag(id)))
}

/// Begins resizing the window while the left mouse button is held.
pub fn drag_resize<T>(id: Id, direction: Direction) -> Task<T> {
    task::effect(crate::Action::Window(Action::DragResize(id, direction)))
}

/// Resizes the window to the given logical dimensions.
pub fn resize<T>(id: Id, new_size: Size) -> Task<T> {
    task::effect(crate::Action::Window(Action::Resize(id, new_size)))
}

/// Set the window to be resizable or not.
pub fn set_resizable<T>(id: Id, resizable: bool) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetResizable(id, resizable)))
}

/// Set the inner maximum size of the window.
pub fn set_max_size<T>(id: Id, size: Option<Size>) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetMaxSize(id, size)))
}

/// Set the inner minimum size of the window.
pub fn set_min_size<T>(id: Id, size: Option<Size>) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetMinSize(id, size)))
}

/// Set the window size increment.
///
/// This is usually used by apps such as terminal emulators that need "blocky" resizing.
pub fn set_resize_increments<T>(id: Id, increments: Option<Size>) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetResizeIncrements(
        id, increments,
    )))
}

/// Gets the window size in logical dimensions.
pub fn size(id: Id) -> Task<Size> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetSize(id, channel)))
}

/// Gets the maximized state of the window with the given [`Id`].
pub fn is_maximized(id: Id) -> Task<bool> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetMaximized(id, channel)))
}

/// Maximizes the window.
pub fn maximize<T>(id: Id, maximized: bool) -> Task<T> {
    task::effect(crate::Action::Window(Action::Maximize(id, maximized)))
}

/// Gets the minimized state of the window with the given [`Id`].
pub fn is_minimized(id: Id) -> Task<Option<bool>> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetMinimized(id, channel)))
}

/// Minimizes the window.
pub fn minimize<T>(id: Id, minimized: bool) -> Task<T> {
    task::effect(crate::Action::Window(Action::Minimize(id, minimized)))
}

/// Gets the position in logical coordinates of the window with the given [`Id`].
pub fn position(id: Id) -> Task<Option<Point>> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetPosition(id, channel)))
}

/// Gets the scale factor of the window with the given [`Id`].
pub fn scale_factor(id: Id) -> Task<f32> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetScaleFactor(id, channel)))
}

/// Moves the window to the given logical coordinates.
pub fn move_to<T>(id: Id, position: Point) -> Task<T> {
    task::effect(crate::Action::Window(Action::Move(id, position)))
}

/// Gets the current [`Mode`] of the window.
pub fn mode(id: Id) -> Task<Mode> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetMode(id, channel)))
}

/// Changes the [`Mode`] of the window.
pub fn set_mode<T>(id: Id, mode: Mode) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetMode(id, mode)))
}

/// Toggles the window to maximized or back.
pub fn toggle_maximize<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::ToggleMaximize(id)))
}

/// Toggles the window decorations.
pub fn toggle_decorations<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::ToggleDecorations(id)))
}

/// Requests user attention to the window. This has no effect if the application
/// is already focused. How requesting for user attention manifests is platform dependent,
/// see [`UserAttention`] for details.
///
/// Providing `None` will unset the request for user attention. Unsetting the request for
/// user attention might not be done automatically by the WM when the window receives input.
pub fn request_user_attention<T>(id: Id, user_attention: Option<UserAttention>) -> Task<T> {
    task::effect(crate::Action::Window(Action::RequestUserAttention(
        id,
        user_attention,
    )))
}

/// Brings the window to the front and sets input focus. Has no effect if the window is
/// already in focus, minimized, or not visible.
///
/// This [`Task`] steals input focus from other applications. Do not use this method unless
/// you are certain that's what the user wants. Focus stealing can cause an extremely disruptive
/// user experience.
pub fn gain_focus<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::GainFocus(id)))
}

/// Requests a compositor-mediated activation token for a window.
///
/// The compositor may issue an ineffective token when the request is not tied
/// to an eligible user interaction. A successful result is not proof that a
/// later activation will be granted.
pub fn request_activation_token(
    id: Id,
) -> Task<Result<crate::core::window::ActivationToken, crate::core::window::ActivationError>> {
    task::oneshot(move |channel| crate::Action::Window(Action::RequestActivationToken(id, channel)))
}

/// Asks the compositor to activate a window using a single-use token.
///
/// Success means the native request was submitted; compositor policy still
/// decides whether focus changes.
pub fn activate(
    id: Id,
    token: crate::core::window::ActivationToken,
) -> Task<Result<(), crate::core::window::ActivationError>> {
    task::oneshot(move |channel| crate::Action::Window(Action::Activate(id, token, channel)))
}

/// Changes the window [`Level`].
pub fn set_level<T>(id: Id, level: Level) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetLevel(id, level)))
}

/// Shows the [system menu] at cursor position.
///
/// [system menu]: https://en.wikipedia.org/wiki/Common_menus_in_Microsoft_Windows#System_menu
pub fn show_system_menu<T>(id: Id) -> Task<T> {
    task::effect(crate::Action::Window(Action::ShowSystemMenu(id)))
}

/// Gets an identifier unique to the window, provided by the underlying windowing system. This is
/// not to be confused with [`Id`].
pub fn raw_id<Message>(id: Id) -> Task<u64> {
    task::oneshot(|channel| crate::Action::Window(Action::GetRawId(id, channel)))
}

/// Changes the [`Icon`] of the window.
pub fn set_icon<T>(id: Id, icon: Icon) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetIcon(id, icon)))
}

/// Casts `shadow` around the window, which draws its own frame, or none
/// (enzICED). See [`Shadow`](crate::core::window::Shadow).
pub fn set_shadow<T>(id: Id, shadow: Option<crate::core::window::Shadow>) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetShadow(id, shadow)))
}

/// Runs the given callback with a reference to the [`Window`] with the given [`Id`].
///
/// Note that if the window closes before this call is processed the callback will not be run.
pub fn run<T>(id: Id, f: impl FnOnce(&dyn Window) -> T + Send + 'static) -> Task<T>
where
    T: Send + 'static,
{
    task::oneshot(move |channel| {
        crate::Action::Window(Action::Run(
            id,
            Box::new(move |handle| {
                let _ = channel.send(f(handle));
            }),
        ))
    })
}

/// Requests the window's own Wayland objects for an XDG portal parent export.
///
/// Resolves to `None` off Wayland, or if the window is gone by the time the
/// request is handled. The boxed payload is `(wayland_client::Connection,
/// wayland_client::protocol::wl_surface::WlSurface, u64)`; the portal layer
/// downcasts it with `enzui_iced_wayland::owned_window_from_payload`.
pub fn wayland_parent_export(id: Id) -> Task<Option<Box<dyn Any + Send>>> {
    task::oneshot(move |channel| crate::Action::Window(Action::WaylandParentExport(id, channel)))
}

/// The id of the Wayland window session the window joined with
/// `PlatformSpecific::session` (enzICED), once the compositor has created or
/// restored it; store it and pass it back next launch. `None` without a
/// session or where the compositor offers none.
pub fn wayland_session_id(id: Id) -> Task<Option<String>> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetWaylandSessionId(id, channel)))
}

/// The window's own `xdg_toplevel` proxy (enzICED), boxed so this signature
/// names no Wayland type; `None` for a window that is not a Wayland
/// toplevel. A protocol that names the window, such as moving it with a drag
/// (`xdg_toplevel_drag_v1`), downcasts it to
/// `wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel`.
pub fn wayland_toplevel(id: Id) -> Task<Option<Box<dyn Any + Send>>> {
    task::oneshot(move |channel| crate::Action::Window(Action::WaylandToplevel(id, channel)))
}

/// Queues an opaque tag on this window's native Wayland toplevel.
///
/// Returns `false` for a closed window, an unavailable protocol, other platforms,
/// or tags outside 1–128 ASCII alphanumerics and `._:-`. A `true` result means
/// the request was queued; it is not a compositor acknowledgement. Use a random
/// per-window tag and the same tag for dialogs belonging to that origin.
pub fn set_toplevel_tag(id: Id, tag: impl Into<String>) -> Task<bool> {
    let tag = tag.into();
    task::oneshot(move |channel| crate::Action::Window(Action::SetToplevelTag(id, tag, channel)))
}

/// Captures a [`Screenshot`] from the window.
pub fn screenshot(id: Id) -> Task<Screenshot> {
    task::oneshot(move |channel| crate::Action::Window(Action::Screenshot(id, channel)))
}

/// Enables mouse passthrough for the given window.
///
/// This disables mouse events for the window and passes mouse events
/// through to whatever window is underneath.
pub fn enable_mouse_passthrough<Message>(id: Id) -> Task<Message> {
    task::effect(crate::Action::Window(Action::EnableMousePassthrough(id)))
}

/// Confines the pointer to the window, locks it in place or lets it go
/// (enzICED).
///
/// While grabbed, every movement also arrives as
/// [`mouse::Event::RelativeMotion`](crate::core::mouse::Event::RelativeMotion).
/// The result is `false` when the platform refuses the grab (X11 cannot lock,
/// some compositors offer neither). Wayland ends a grab when the window loses
/// focus; set it again on focus.
pub fn set_cursor_grab(id: Id, grab: CursorGrab) -> Task<bool> {
    task::oneshot(move |channel| crate::Action::Window(Action::SetCursorGrab(id, grab, channel)))
}

/// Disables mouse passthrough for the given window.
///
/// This enables mouse events for the window and stops mouse events
/// from being passed to whatever is underneath.
pub fn disable_mouse_passthrough<Message>(id: Id) -> Task<Message> {
    task::effect(crate::Action::Window(Action::DisableMousePassthrough(id)))
}

/// Gets the logical dimensions of the monitor containing the window with the given [`Id`].
pub fn monitor_size(id: Id) -> Task<Option<Size>> {
    task::oneshot(move |channel| crate::Action::Window(Action::GetMonitorSize(id, channel)))
}

/// Lists the connected monitors (enzICED): name, resolution, scale and
/// refresh rate. Wayland does not reveal where they sit in the desktop.
pub fn monitors() -> Task<Vec<Monitor>> {
    task::oneshot(|channel| crate::Action::Window(Action::GetMonitors(channel)))
}

/// Sets whether the system can automatically organize windows into tabs.
///
/// See <https://developer.apple.com/documentation/appkit/nswindow/1646657-allowsautomaticwindowtabbing>
pub fn allow_automatic_tabbing<T>(enabled: bool) -> Task<T> {
    task::effect(crate::Action::Window(Action::SetAllowAutomaticTabbing(
        enabled,
    )))
}

/// The set of windows one program update invalidated.
///
/// The runtime rebuilds (`view` + `UserInterface::build`), relayouts and
/// redraws a window only when that window is dirty. An update
/// that declares nothing at all dirties **every** window, which is the
/// behaviour every program had before this type existed; narrowing is opt-in
/// and per-update.
///
/// # Correctness
/// The declaration is a promise by the caller: *no other window's `view` can
/// observe a difference from this update*. Declaring a window that did not
/// change is merely wasteful, but failing to declare one that did change
/// leaves that window painting stale pixels until something else dirties it.
/// [`crate::Action::Window`] actions, `Reload`, backend/renderer changes and
/// widget operations always fall back to [`Invalidation::All`], so theme,
/// scale and reduced-motion changes cannot be narrowed by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalidation {
    /// Every open window may have changed. The compatibility default.
    All,
    /// Only these windows may have changed.
    ///
    /// Ids of windows that are unknown or already closing are ignored; the
    /// surviving windows in the set are still invalidated.
    Windows(Vec<Id>),
}

impl Invalidation {
    /// Declares that only the window with the given [`Id`] changed.
    pub fn only(id: Id) -> Self {
        Invalidation::Windows(vec![id])
    }

    /// Declares that no window changed.
    ///
    /// Useful for updates that only touch state no `view` reads.
    pub fn none() -> Self {
        Invalidation::Windows(Vec::new())
    }

    /// Adds another window to this declaration.
    pub fn and(mut self, id: Id) -> Self {
        match &mut self {
            Invalidation::All => {}
            Invalidation::Windows(ids) => {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }

        self
    }

    /// Returns the union of two declarations.
    ///
    /// [`Invalidation::All`] absorbs everything.
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Invalidation::All, _) | (_, Invalidation::All) => Invalidation::All,
            (Invalidation::Windows(a), Invalidation::Windows(b)) => {
                b.into_iter().fold(Invalidation::Windows(a), Self::and)
            }
        }
    }

    /// Returns `true` if this declaration covers every window.
    pub fn is_all(&self) -> bool {
        matches!(self, Invalidation::All)
    }

    /// Returns `true` if the window with the given [`Id`] is dirty.
    pub fn contains(&self, id: Id) -> bool {
        match self {
            Invalidation::All => true,
            Invalidation::Windows(ids) => ids.contains(&id),
        }
    }
}

impl FromIterator<Id> for Invalidation {
    fn from_iter<I: IntoIterator<Item = Id>>(ids: I) -> Self {
        ids.into_iter().fold(Invalidation::none(), Self::and)
    }
}

/// Declares the windows the current update invalidated.
///
/// Batch this with the rest of the [`Task`] an update returns:
///
/// ```no_run
/// # use iced_runtime::{Task, window};
/// # let editor = iced_runtime::core::window::Id::unique();
/// # fn save() -> Task<()> { Task::none() }
/// let _: Task<()> = Task::batch([save(), window::invalidate(window::Invalidation::only(editor))]);
/// ```
///
/// Declarations from every message in the same runtime turn are merged as a
/// union. Every update owns its declaration independently, so one message's
/// local scope cannot narrow another message that declares nothing. The
/// declaration must be immediately ready before its task first waits or is
/// deferred by the runtime's fairness budget; otherwise that update keeps the
/// global compatibility default. Any window action, `Reload` or widget
/// operation in the same turn also restores global invalidation.
pub fn invalidate<T>(invalidation: Invalidation) -> Task<T> {
    task::effect(crate::Action::Invalidate(invalidation))
}

/// Declares that only the window with the given [`Id`] was invalidated.
pub fn invalidate_only<T>(id: Id) -> Task<T> {
    invalidate(Invalidation::only(id))
}

/// Declares that every window was invalidated.
///
/// This is the default, so it is only needed to override a narrower
/// declaration made by another message in the same runtime turn — for
/// instance, a subscription or task that changed state every `view` reads.
pub fn invalidate_all<T>() -> Task<T> {
    invalidate(Invalidation::All)
}
