//! Allocate images explicitly to control presentation.
use crate::core::image::Handle;
use crate::core::window;
use crate::futures::futures::channel::oneshot;
use crate::task::{self, Task};

pub use crate::core::image::{Allocation, Error};

/// An image action.
#[derive(Debug)]
pub enum Action {
    /// Allocates the given [`Handle`].
    Allocate(Handle, oneshot::Sender<Result<Allocation, Error>>),
    /// Allocates the given [`Handle`] in the renderer of the given window.
    AllocateIn(
        window::Id,
        Handle,
        oneshot::Sender<Result<Allocation, Error>>,
    ),
}

/// Allocates an image [`Handle`].
///
/// When you obtain an [`Allocation`] explicitly, you get the guarantee
/// that using a [`Handle`] will draw the corresponding image immediately
/// in the next frame.
pub fn allocate(handle: impl Into<Handle>) -> Task<Result<Allocation, Error>> {
    task::oneshot(|sender| crate::Action::Image(Action::Allocate(handle.into(), sender)))
}

/// Allocates an image [`Handle`] in the renderer of the given window.
///
/// Each window owns its renderer and image cache, and [`allocate`] uploads
/// into an unspecified one. Use this for an image a secondary window draws,
/// so its [`Allocation`] guarantees the image in the renderer that draws it.
///
/// Fails with [`Error::Unsupported`] when the window does not exist (yet).
pub fn allocate_in(
    window: window::Id,
    handle: impl Into<Handle>,
) -> Task<Result<Allocation, Error>> {
    task::oneshot(move |sender| {
        crate::Action::Image(Action::AllocateIn(window, handle.into(), sender))
    })
}
