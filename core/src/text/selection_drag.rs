// Modified by enzUI contributors. See FORK.md.
//! Dragging selected text out of a text input or editor (enzICED).
//!
//! A press inside a selection followed by a drag of [`THRESHOLD`] pixels
//! hands the selected text to the handler the application installed with
//! [`set_handler`], which starts a platform drag-and-drop; the widget then
//! leaves its selection alone until the button is released. Without a
//! handler, or when the handler declines, the press selects as usual.

use std::sync::{Arc, RwLock};

/// How far the pointer must move, in logical pixels, before a press inside a
/// selection becomes a drag of it.
pub const THRESHOLD: f32 = 6.0;

/// What a selection drag carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionDrag {
    /// The selected text.
    pub text: String,
}

/// Starts a platform drag for a selection; returns whether one started.
pub type Handler = Arc<dyn Fn(SelectionDrag) -> bool + Send + Sync>;

static HANDLER: RwLock<Option<Handler>> = RwLock::new(None);

/// Installs (or with `None` removes) the handler that starts selection drags.
pub fn set_handler(handler: Option<Handler>) {
    *HANDLER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = handler;
}

/// Whether a handler is installed: only then does a press inside a selection
/// wait to see whether it becomes a drag.
pub fn is_available() -> bool {
    HANDLER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

/// Hands `drag` to the handler; `false` when there is none or it declined.
pub fn begin(drag: SelectionDrag) -> bool {
    let handler = HANDLER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    handler.is_some_and(|handler| handler(drag))
}
