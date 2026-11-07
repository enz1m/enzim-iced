// Modified by enzUI contributors. See FORK.md.
//! A renderer-agnostic native GUI runtime.
//!
//! ![The native path of the Iced ecosystem](https://github.com/iced-rs/iced/blob/master/docs/graphs/native.png?raw=true)
//!
//! `iced_runtime` takes [`iced_core`] and builds a native runtime on top of it.
//!
//! [`iced_core`]: https://github.com/iced-rs/iced/tree/master/core
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/iced-rs/iced/9ab6923e943f784985e9ef9ca28b10278297225d/docs/logo.svg"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod backend;
pub mod clipboard;
pub mod fairness;
pub mod font;
pub mod image;
pub mod keyboard;
#[cfg(feature = "performance-observer")]
pub mod performance;
pub mod system;
pub mod task;
pub mod user_interface;
pub mod widget;
pub mod window;

pub use iced_core as core;
pub use iced_futures as futures;

pub use task::Task;
pub use user_interface::UserInterface;

use crate::core::Event;

use std::fmt;

/// An action that the iced runtime can perform.
pub enum Action<T> {
    /// Output some value.
    Output(T),

    /// Run a widget operation.
    Widget(Box<dyn core::widget::Operation>),

    /// Run a widget operation against **one exact window** and no other.
    ///
    /// [`Action::Widget`] walks every live window, so an operation that names a
    /// widget [`Id`](core::widget::Id) present in two windows (a daemon's two
    /// document windows, the same view function) applies to both — and an
    /// accessibility action resolved in one window would move focus or write a
    /// value in the other. This variant carries the window the operation is
    /// allowed to touch; see [`operate_in_window`].
    WidgetIn {
        /// The only window whose user interface may be operated.
        window: core::window::Id,
        /// The operation to run in that window.
        operation: Box<dyn core::widget::Operation>,
    },

    /// Run a clipboard action.
    Clipboard(clipboard::Action),

    /// Run a window action.
    Window(window::Action),

    /// Run a system action.
    System(system::Action),

    /// Run a font action.
    Font(font::Action),

    /// Run an image action.
    Image(image::Action),

    /// Run a backend action.
    Backend(backend::Action),

    /// Produce an event.
    Event {
        /// The [`window::Id`](core::window::Id) of the event.
        window: core::window::Id,
        /// The [`Event`] to be produced.
        event: Event,
    },

    /// Poll any resources that may have pending computations.
    Tick,

    /// Recreate all user interfaces and redraw all windows.
    Reload,

    /// Declare which windows the current program update invalidated.
    ///
    /// This is an **opt-in** narrowing of the runtime's default behaviour: an
    /// update that declares nothing invalidates every window, exactly as it
    /// always did. See [`window::Invalidation`].
    Invalidate(window::Invalidation),

    /// Exits the runtime.
    ///
    /// This will normally close any application windows and
    /// terminate the runtime loop.
    Exit,
}

impl<T> Action<T> {
    /// Creates a new [`Action::Widget`] with the given [`widget::Operation`](core::widget::Operation).
    pub fn widget(operation: impl core::widget::Operation + 'static) -> Self {
        Self::Widget(Box::new(operation))
    }

    /// Creates a new [`Action::WidgetIn`] that runs `operation` in `window`
    /// only.
    pub fn widget_in(
        window: core::window::Id,
        operation: impl core::widget::Operation + 'static,
    ) -> Self {
        Self::WidgetIn {
            window,
            operation: Box::new(operation),
        }
    }

    fn output<O>(self) -> Result<T, Action<O>> {
        match self {
            Action::Output(output) => Ok(output),
            Action::Widget(operation) => Err(Action::Widget(operation)),
            Action::WidgetIn { window, operation } => Err(Action::WidgetIn { window, operation }),
            Action::Clipboard(action) => Err(Action::Clipboard(action)),
            Action::Window(action) => Err(Action::Window(action)),
            Action::System(action) => Err(Action::System(action)),
            Action::Font(action) => Err(Action::Font(action)),
            Action::Image(action) => Err(Action::Image(action)),
            Action::Backend(action) => Err(Action::Backend(action)),
            Action::Event { window, event } => Err(Action::Event { window, event }),
            Action::Tick => Err(Action::Tick),
            Action::Reload => Err(Action::Reload),
            Action::Invalidate(invalidation) => Err(Action::Invalidate(invalidation)),
            Action::Exit => Err(Action::Exit),
        }
    }
}

impl<T> fmt::Debug for Action<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Output(output) => write!(f, "Action::Output({output:?})"),
            Action::Widget { .. } => {
                write!(f, "Action::Widget")
            }
            Action::WidgetIn { window, .. } => {
                write!(f, "Action::WidgetIn({window})")
            }
            Action::Clipboard(action) => {
                write!(f, "Action::Clipboard({action:?})")
            }
            Action::Window(_) => write!(f, "Action::Window"),
            Action::System(action) => write!(f, "Action::System({action:?})"),
            Action::Font(action) => {
                write!(f, "Action::Font({action:?})")
            }
            Action::Image(action) => write!(f, "Action::Image({action:?})"),
            Action::Backend(action) => write!(f, "Action::Backend({action:?})"),
            Action::Event { window, event } => write!(
                f,
                "Action::Event {{ window: {window:?}, event: {event:?} }}"
            ),
            Action::Tick => write!(f, "Action::Tick"),
            Action::Reload => write!(f, "Action::Reload"),
            Action::Invalidate(invalidation) => {
                write!(f, "Action::Invalidate({invalidation:?})")
            }
            Action::Exit => write!(f, "Action::Exit"),
        }
    }
}

/// Creates a [`Task`] that exits the iced runtime.
///
/// This will normally close any application windows and
/// terminate the runtime loop.
pub fn exit<T>() -> Task<T> {
    task::effect(Action::Exit)
}

/// Runs `operation`, and every operation it chains, against **one** window.
///
/// The single owner of [`Action::WidgetIn`]'s targeting rule: `operate` is
/// called once per chain step with the window the action named, and must apply
/// the operation to *that* window's user interface only, returning whether such
/// a window was live. Shells (and tests that stand in for one) go through this
/// function so the "only the named window" contract cannot drift between the
/// `Action::Widget` fan-out and the scoped path.
///
/// Returns `true` if the window was live for the first step. A window that was
/// closed between the update turn and the action returns `false`; the operation
/// is dropped rather than redirected to some other window, because applying an
/// action to the wrong window is worse than dropping it.
pub fn operate_in_window(
    window: core::window::Id,
    operation: Box<dyn core::widget::Operation>,
    mut operate: impl FnMut(core::window::Id, &mut dyn core::widget::Operation) -> bool,
) -> bool {
    let mut current = Some(operation);
    let mut applied = false;
    let mut first = true;

    while let Some(mut operation) = current.take() {
        let live = operate(window, operation.as_mut());
        if first {
            applied = live;
            first = false;
        }

        match operation.finish() {
            core::widget::operation::Outcome::None | core::widget::operation::Outcome::Some(()) => {
            }
            core::widget::operation::Outcome::Chain(next) => {
                current = Some(next);
            }
        }
    }

    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Rectangle;
    use crate::core::widget::Id;
    use crate::core::widget::operation::{Focusable, Operation, Outcome};

    /// A focusable state that records every focus/unfocus it received.
    #[derive(Default)]
    struct Recorder {
        focused: bool,
    }

    impl Focusable for Recorder {
        fn is_focused(&self) -> bool {
            self.focused
        }

        fn focus(&mut self) {
            self.focused = true;
        }

        fn unfocus(&mut self) {
            self.focused = false;
        }
    }

    /// An operation that chains itself once, so the runner's chain handling is
    /// exercised rather than assumed.
    struct ChainOnce {
        remaining: usize,
    }

    impl Operation for ChainOnce {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn finish(&self) -> Outcome<()> {
            if self.remaining > 0 {
                Outcome::Chain(Box::new(ChainOnce {
                    remaining: self.remaining - 1,
                }))
            } else {
                Outcome::None
            }
        }
    }

    /// Every step of the chain is offered to the window the action named, and
    /// to no other window.
    ///
    /// Sabotage: pass any other id (or iterate a window list) inside
    /// `operate_in_window` and the recorded windows red.
    #[test]
    fn every_chain_step_is_offered_to_the_named_window_only() {
        let target = core::window::Id::unique();
        let other = core::window::Id::unique();
        let asked = std::cell::RefCell::new(Vec::new());

        let applied = operate_in_window(
            target,
            Box::new(ChainOnce { remaining: 2 }),
            |window, _operation| {
                asked.borrow_mut().push(window);
                true
            },
        );

        assert!(applied, "the window was live for the first step");
        assert_eq!(
            *asked.borrow(),
            vec![target, target, target],
            "the chain must stay inside the named window; {other} was never a \
             candidate",
        );
    }

    /// A window closed between the update turn and the action is not
    /// redirected: the operation is dropped and the caller learns it.
    #[test]
    fn a_dead_window_drops_the_operation_instead_of_redirecting_it() {
        let target = core::window::Id::unique();
        let mut state = Recorder::default();

        let applied = operate_in_window(
            target,
            Box::new(core::widget::operation::focusable::focus::<()>(Id::new(
                "gone",
            ))),
            |_window, _operation| false,
        );

        assert!(
            !applied,
            "a closed window must be reported, not substituted"
        );
        assert!(
            !state.is_focused(),
            "no widget state was touched for the dead window",
        );

        // Positive control: the very same operation does reach a live window's
        // state, so the assertion above is about the window, not the operation.
        let mut live = Box::new(core::widget::operation::focusable::focus::<()>(Id::new(
            "gone",
        )));
        live.focusable(Some(&Id::new("gone")), Rectangle::default(), &mut state);
        assert!(state.is_focused(), "positive control: focus reaches state");
    }
}
