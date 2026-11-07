// Modified by enzUI contributors. See FORK.md.
//! Build window-based GUI applications.
pub mod icon;
pub mod screenshot;
pub mod settings;

mod activation;
mod cursor_grab;
mod decorations;
mod direction;
mod event;
mod id;
mod level;
mod mode;
mod monitor;
mod position;
mod redraw_request;
mod shadow;
mod user_attention;

pub use activation::{ActivationError, ActivationPlatform, ActivationToken};
pub use cursor_grab::CursorGrab;
pub use decorations::{
    Decorations, TiledEdges, decorations, decorations_of, set_decorations, viewing,
};
pub use direction::Direction;
pub use event::Event;
pub use icon::Icon;
pub use id::Id;
pub use level::Level;
pub use mode::Mode;
pub use monitor::Monitor;
pub use position::Position;
pub use redraw_request::RedrawRequest;
pub use screenshot::Screenshot;
pub use settings::Settings;
pub use shadow::{Shadow, ShadowLayer};
pub use user_attention::UserAttention;

/// A native operation requested by a widget for the window currently
/// dispatching its event.
///
/// Unlike application task window actions, these requests carry no
/// [`Id`]. The runtime binds them to the exact window whose [`crate::Shell`]
/// processed the event. The vocabulary is intentionally limited to operations
/// that are safe to perform without application policy.
#[derive(Debug, Clone, Copy)]
pub enum Request {
    /// Begin the compositor-owned resize gesture in the given direction.
    DragResize(Direction),
}

/// Authoritative native state of the window currently dispatching widget
/// events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeState {
    /// Whether the platform window can currently be resized.
    pub resizable: bool,
    /// Whether the platform window is maximized.
    pub maximized: bool,
    /// Whether the platform window is fullscreen.
    pub fullscreen: bool,
}

impl RuntimeState {
    /// Whether native edge resizing is currently available.
    pub fn can_resize(self) -> bool {
        self.resizable && !self.maximized && !self.fullscreen
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            resizable: true,
            maximized: false,
            fullscreen: false,
        }
    }
}

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::fmt::Debug;

pub use raw_window_handle;

/// A window managed by iced.
///
/// It implements both [`HasWindowHandle`] and [`HasDisplayHandle`].
pub trait Window: HasWindowHandle + HasDisplayHandle + Debug {}

impl<T> Window for T where T: HasWindowHandle + HasDisplayHandle + Debug {}

/// A headless window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Headless;

impl HasWindowHandle for Headless {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

impl HasDisplayHandle for Headless {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}
