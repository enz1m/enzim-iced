// Modified by enzUI contributors. See FORK.md.
//! Access the clipboard.
use crate::core::clipboard::{Content, Error, Kind};

pub use platform::*;

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Runtime-owned clipboard provider hook.
///
/// An external crate installs a [`Provider`] through [`set_provider`] before
/// the event loop starts. When the runtime creates its clipboard it offers
/// the display connection to that provider; the provider either adopts it
/// (returning a [`Backend`] the runtime uses for every read and write) or
/// declines, in which case the built-in arboard transport is used.
pub mod platform {
    use super::*;

    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;

    use winit::event_loop::OwnedDisplayHandle;

    /// A clipboard transport owned by the embedding framework.
    ///
    /// Implementations receive the event loop's own display connection, so a
    /// windowing system whose selection is bound to a connection, seat, focus
    /// and input serial can honour all four. Callbacks may run on any thread.
    pub trait Backend: Send + 'static {
        /// Reads the current selection.
        fn read(&self, kind: Kind, callback: Box<dyn FnOnce(Result<Content, Error>) + Send>);

        /// Claims the selection and serves `content` until something else
        /// claims it.
        fn write(&mut self, content: Content, callback: Box<dyn FnOnce(Result<(), Error>) + Send>);

        /// Reads the primary selection (the text a middle click pastes).
        ///
        /// The default reports [`Error::ContentNotAvailable`] for a transport
        /// without a primary selection.
        fn read_primary(
            &self,
            kind: Kind,
            callback: Box<dyn FnOnce(Result<Content, Error>) + Send>,
        ) {
            let _ = kind;
            callback(Err(Error::ContentNotAvailable));
        }

        /// Offers `content` as the primary selection.
        ///
        /// The default reports [`Error::ClipboardUnavailable`].
        fn write_primary(
            &mut self,
            content: Content,
            callback: Box<dyn FnOnce(Result<(), Error>) + Send>,
        ) {
            let _ = content;
            callback(Err(Error::ClipboardUnavailable));
        }
    }

    /// Which selection a request addresses.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Selection {
        Clipboard,
        Primary,
    }

    /// What a [`Provider`] made of the running display connection.
    pub enum Connection {
        /// A transport bound to this connection.
        Adopted(Box<dyn Backend>),

        /// This display is not one the provider handles; the built-in
        /// clipboard is used instead.
        Unsupported,

        /// This display is one the provider handles, and it could not be
        /// used. The clipboard reports this error rather than falling back
        /// to a different transport and reporting success from it.
        Refused(Error),
    }

    /// Supplies a [`Backend`] for the display the event loop owns.
    pub trait Provider: Send + Sync + 'static {
        /// Called once per event loop, before the first window exists.
        ///
        /// The handle is given rather than lent so that a backend built on the
        /// display's raw pointer can keep it, which is what makes the pointer
        /// valid for as long as that backend lives.
        fn connect(&self, display: OwnedDisplayHandle) -> Connection;
    }

    static PROVIDER: OnceLock<Box<dyn Provider>> = OnceLock::new();

    /// Installs the clipboard provider for this process.
    ///
    /// Returns `false` when one is already installed; the first installation
    /// wins, so a library cannot silently replace an application's choice.
    /// Call before running the program: the provider is consulted while the
    /// event loop starts.
    pub fn set_provider(provider: impl Provider) -> bool {
        PROVIDER.set(Box::new(provider)).is_ok()
    }

    /// A buffer for short-term storage and transfer within and between
    /// applications.
    pub struct Clipboard {
        state: State,
    }

    enum State {
        Provided {
            backend: Box<dyn Backend>,
        },
        Connected {
            clipboard: Arc<Mutex<arboard::Clipboard>>,
        },
        Refused {
            error: Error,
        },
        Unavailable,
    }

    impl Clipboard {
        /// Creates a new [`Clipboard`] with no display connection.
        ///
        /// The built-in transport is used, which on some windowing systems
        /// cannot reach the selection of the focused window at all. Prefer
        /// [`Clipboard::connected`].
        pub fn new() -> Self {
            let clipboard = arboard::Clipboard::new();

            let state = match clipboard {
                Ok(clipboard) => State::Connected {
                    clipboard: Arc::new(Mutex::new(clipboard)),
                },
                Err(_) => State::Unavailable,
            };

            Clipboard { state }
        }

        /// Creates a new [`Clipboard`] for the display the event loop owns.
        ///
        /// An installed [`Provider`] is offered that display first; without
        /// one, or when the provider does not handle this display, the
        /// built-in transport is used exactly as [`Clipboard::new`] would.
        pub fn connected(display: &OwnedDisplayHandle) -> Self {
            match PROVIDER
                .get()
                .map(|provider| provider.connect(display.clone()))
            {
                Some(Connection::Adopted(backend)) => Clipboard {
                    state: State::Provided { backend },
                },
                Some(Connection::Refused(error)) => Clipboard {
                    state: State::Refused { error },
                },
                Some(Connection::Unsupported) | None => Self::new(),
            }
        }

        /// Reads the current content of the [`Clipboard`] as text.
        pub fn read(
            &self,
            kind: Kind,
            callback: impl FnOnce(Result<Content, Error>) + Send + 'static,
        ) {
            self.read_from(Selection::Clipboard, kind, callback);
        }

        /// Reads the primary selection.
        pub fn read_primary(
            &self,
            kind: Kind,
            callback: impl FnOnce(Result<Content, Error>) + Send + 'static,
        ) {
            self.read_from(Selection::Primary, kind, callback);
        }

        /// Writes the given contents to the [`Clipboard`].
        pub fn write(
            &mut self,
            content: Content,
            callback: impl FnOnce(Result<(), Error>) + Send + 'static,
        ) {
            self.write_to(Selection::Clipboard, content, callback);
        }

        /// Offers the given contents as the primary selection.
        pub fn write_primary(
            &mut self,
            content: Content,
            callback: impl FnOnce(Result<(), Error>) + Send + 'static,
        ) {
            self.write_to(Selection::Primary, content, callback);
        }

        fn read_from(
            &self,
            selection: Selection,
            kind: Kind,
            callback: impl FnOnce(Result<Content, Error>) + Send + 'static,
        ) {
            let clipboard = match &self.state {
                State::Provided { backend } => {
                    match selection {
                        Selection::Clipboard => backend.read(kind, Box::new(callback)),
                        Selection::Primary => backend.read_primary(kind, Box::new(callback)),
                    }
                    return;
                }
                State::Refused { error } => {
                    callback(Err(error.clone()));
                    return;
                }
                State::Connected { clipboard } => clipboard,
                State::Unavailable => {
                    callback(Err(Error::ClipboardUnavailable));
                    return;
                }
            };

            let clipboard = clipboard.clone();

            let _ = thread::spawn(move || {
                let Ok(mut clipboard) = clipboard.lock() else {
                    callback(Err(Error::ClipboardUnavailable));
                    return;
                };

                let Some(get) = target_get(clipboard.get(), selection) else {
                    callback(Err(Error::ContentNotAvailable));
                    return;
                };

                let result = match kind {
                    Kind::Text => get.text().map(Content::Text),
                    Kind::Html => get.html().map(Content::Html),
                    #[cfg(feature = "image")]
                    Kind::Image => get.image().map(|image| {
                        let rgba = crate::core::Bytes::from_owner(image.bytes);
                        let size = crate::core::Size {
                            width: image.width as u32,
                            height: image.height as u32,
                        };

                        Content::Image(crate::core::clipboard::Image { rgba, size })
                    }),
                    Kind::Files => get.file_list().map(Content::Files),
                    kind => {
                        log::warn!("unsupported clipboard kind: {kind:?}");

                        Err(arboard::Error::ContentNotAvailable)
                    }
                }
                .map_err(to_error);

                callback(result);
            });
        }

        fn write_to(
            &mut self,
            selection: Selection,
            content: Content,
            callback: impl FnOnce(Result<(), Error>) + Send + 'static,
        ) {
            let clipboard = match &mut self.state {
                State::Provided { backend } => {
                    match selection {
                        Selection::Clipboard => backend.write(content, Box::new(callback)),
                        Selection::Primary => backend.write_primary(content, Box::new(callback)),
                    }
                    return;
                }
                State::Refused { error } => {
                    callback(Err(error.clone()));
                    return;
                }
                State::Connected { clipboard } => clipboard,
                State::Unavailable => {
                    callback(Err(Error::ClipboardUnavailable));
                    return;
                }
            };

            let clipboard = clipboard.clone();

            let _ = thread::spawn(move || {
                let Ok(mut clipboard) = clipboard.lock() else {
                    callback(Err(Error::ClipboardUnavailable));
                    return;
                };

                let Some(set) = target_set(clipboard.set(), selection) else {
                    callback(Err(Error::ClipboardUnavailable));
                    return;
                };

                let result = match content {
                    Content::Text(text) => set.text(text),
                    Content::Html(html) => set.html(html, None),
                    #[cfg(feature = "image")]
                    Content::Image(image) => set.image(arboard::ImageData {
                        bytes: image.rgba.as_ref().into(),
                        width: image.size.width as usize,
                        height: image.size.height as usize,
                    }),
                    Content::Files(files) => set.file_list(&files),
                    content => {
                        log::warn!("unsupported clipboard content: {content:?}");

                        Err(arboard::Error::ClipboardNotSupported)
                    }
                }
                .map_err(to_error);

                callback(result);
            });
        }
    }

    /// Points an arboard read at the requested selection; `None` where the
    /// platform has no primary selection.
    fn target_get(get: arboard::Get<'_>, selection: Selection) -> Option<arboard::Get<'_>> {
        match selection {
            Selection::Clipboard => Some(get),
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
            Selection::Primary => {
                use arboard::GetExtLinux;
                Some(get.clipboard(arboard::LinuxClipboardKind::Primary))
            }
            #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
            Selection::Primary => None,
        }
    }

    /// Points an arboard write at the requested selection; `None` where the
    /// platform has no primary selection.
    fn target_set(set: arboard::Set<'_>, selection: Selection) -> Option<arboard::Set<'_>> {
        match selection {
            Selection::Clipboard => Some(set),
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
            Selection::Primary => {
                use arboard::SetExtLinux;
                Some(set.clipboard(arboard::LinuxClipboardKind::Primary))
            }
            #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
            Selection::Primary => None,
        }
    }

    fn to_error(error: arboard::Error) -> Error {
        match error {
            arboard::Error::ContentNotAvailable => Error::ContentNotAvailable,
            arboard::Error::ClipboardNotSupported => Error::ClipboardUnavailable,
            arboard::Error::ClipboardOccupied => Error::ClipboardOccupied,
            arboard::Error::ConversionFailure => Error::ConversionFailure,
            arboard::Error::Unknown { description } => Error::Unknown {
                description: Arc::new(description),
            },
            error => Error::Unknown {
                description: Arc::new(error.to_string()),
            },
        }
    }
}

// TODO: Wasm support
#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;

    /// A buffer for short-term storage and transfer within and between
    /// applications.
    pub struct Clipboard;

    impl Clipboard {
        /// Creates a new [`Clipboard`] for the given window.
        pub fn new() -> Self {
            Self
        }

        /// Reads the current content of the [`Clipboard`] as text.
        pub fn read(&self, _kind: Kind, callback: impl FnOnce(Result<Content, Error>)) {
            callback(Err(Error::ClipboardUnavailable));
        }

        /// Writes the given text contents to the [`Clipboard`].
        pub fn write(&mut self, _content: Content, callback: impl FnOnce(Result<(), Error>)) {
            callback(Err(Error::ClipboardUnavailable));
        }

        /// Reads the primary selection.
        pub fn read_primary(&self, _kind: Kind, callback: impl FnOnce(Result<Content, Error>)) {
            callback(Err(Error::ClipboardUnavailable));
        }

        /// Offers contents as the primary selection.
        pub fn write_primary(
            &mut self,
            _content: Content,
            callback: impl FnOnce(Result<(), Error>),
        ) {
            callback(Err(Error::ClipboardUnavailable));
        }
    }
}
