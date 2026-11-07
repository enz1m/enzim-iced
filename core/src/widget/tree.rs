// Modified by enzUI contributors. See FORK.md.
//! Store internal widget state in a state tree to ensure continuity.
use crate::{Size, Vector, Widget};

use std::any::{self, Any};
use std::borrow::{Borrow, BorrowMut};
use std::fmt;

/// A persistent state widget tree.
///
/// A [`Tree`] is normally associated with a specific widget in the widget tree.
#[derive(Debug)]
pub struct Tree {
    /// The tag of the [`Tree`].
    pub tag: Tag,

    /// The [`State`] of the [`Tree`].
    pub state: State,

    /// The translation of the widget, relative to its parent.
    pub translation: Vector,

    /// The size of the widget.
    pub size: Size,

    /// The children of the widget.
    pub children: Vec<Tree>,

    /// Cached frame-listener interest used by targeted redraw routing.
    #[doc(hidden)]
    pub frame_interest: bool,

    /// Cached indices of direct children whose paths contain frame listeners.
    #[doc(hidden)]
    pub frame_children: Vec<usize>,

    /// Owned-presentation declarations of the last layout (enzICED); a
    /// childless [`layout::Node`](crate::layout::Node) that carries no geometry.
    #[doc(hidden)]
    pub presentation: Option<Box<crate::layout::Node>>,

    /// A synthetic node its parent's layout interposed between itself and
    /// this widget (a strip's material slot), childless and positioned in
    /// the parent (enzICED).
    #[doc(hidden)]
    pub interposed: Option<Box<crate::layout::Node>>,
}

impl Tree {
    /// Cancels registered gestures without layout, model actions or focus changes.
    /// Called on ownership failure, not ordinary animation frames; custom interactive state must opt in.
    pub fn cancel_interactions(&mut self) -> usize {
        let mut pending = vec![self];
        let mut cancelled = 0;
        while let Some(tree) = pending.pop() {
            if let State::Interactive(state) = &mut tree.state {
                state.cancel_interaction();
                cancelled += 1;
            }
            pending.extend(&mut tree.children);
        }
        cancelled
    }

    /// Creates an empty, stateless [`Tree`] with no children.
    pub fn empty() -> Self {
        Self {
            tag: Tag::stateless(),
            state: State::None,
            translation: Vector::ZERO,
            size: Size::ZERO,
            children: Vec::new(),
            frame_interest: true,
            frame_children: Vec::new(),
            presentation: None,
            interposed: None,
        }
    }

    /// Creates a new [`Tree`] for the provided [`Widget`].
    pub fn new<'a, Message, Theme, Renderer>(
        widget: impl Borrow<dyn Widget<Message, Theme, Renderer> + 'a>,
    ) -> Self
    where
        Renderer: crate::Renderer,
    {
        let widget = widget.borrow();

        let mut tree = Self {
            tag: widget.tag(),
            state: widget.state(),
            translation: Vector::ZERO,
            size: Size::ZERO,
            children: Vec::new(),
            frame_interest: true,
            frame_children: Vec::new(),
            presentation: None,
            interposed: None,
        };
        tree.refresh_frame_interest(widget);
        tree
    }

    /// Reconciles the current tree with the provided [`Widget`].
    ///
    /// If the tag of the [`Widget`] matches the tag of the [`Tree`], then the
    /// [`Widget`] proceeds with the reconciliation (i.e. [`Widget::diff`] is called).
    ///
    /// Otherwise, the whole [`Tree`] is recreated.
    ///
    /// [`Widget::diff`]: crate::Widget::diff
    pub fn diff<'a, Message, Theme, Renderer>(
        &mut self,
        mut new: impl BorrowMut<dyn Widget<Message, Theme, Renderer> + 'a>,
    ) where
        Renderer: crate::Renderer,
    {
        if self.tag != new.borrow().tag() {
            *self = Self::new(new.borrow());
        }

        new.borrow_mut().diff(self);
        self.refresh_frame_children();
        self.refresh_frame_interest(new.borrow());
    }

    /// Returns whether this widget or one of its descendants needs frame events.
    pub fn has_frame_interest(&self) -> bool {
        self.frame_interest
    }

    /// Counts nodes on the currently cached frame routes.
    ///
    /// This walks only registered listener paths, not the full widget tree. It is
    /// intended for diagnostics and mechanism-count performance gates.
    #[doc(hidden)]
    pub fn frame_route_size(&self) -> usize {
        if !self.frame_interest {
            return 0;
        }

        1 + self
            .frame_children
            .iter()
            .map(|&index| self.children[index].frame_route_size())
            .sum::<usize>()
    }

    /// Counts listener leaves on the currently cached frame routes.
    #[doc(hidden)]
    pub fn frame_listener_count(&self) -> usize {
        if !self.frame_interest {
            0
        } else if self.frame_children.is_empty() {
            1
        } else {
            self.frame_children
                .iter()
                .map(|&index| self.children[index].frame_listener_count())
                .sum()
        }
    }

    /// Recomputes cached frame interest after a widget has changed its state.
    pub fn refresh_frame_interest<'a, Message, Theme, Renderer>(
        &mut self,
        widget: impl Borrow<dyn Widget<Message, Theme, Renderer> + 'a>,
    ) where
        Renderer: crate::Renderer,
    {
        self.frame_interest = widget.borrow().frame_interest(self);
    }

    /// Marks this node as needing frame events without consulting its widget
    /// (enzICED).
    ///
    /// A widget whose `layout` changed something the next frame must answer
    /// (a scrollable whose content grew) calls this on its own tree, since
    /// its cached interest was computed by the preceding `diff`. The mark
    /// reaches the routes once [`propagate_frame_interest`] runs.
    ///
    /// [`propagate_frame_interest`]: Self::propagate_frame_interest
    #[doc(hidden)]
    pub fn raise_frame_interest(&mut self) {
        self.frame_interest = true;
    }

    /// Adds every descendant with frame interest to its parent's routes, and
    /// marks those parents interested (enzICED).
    ///
    /// Widget-free and additive: it never removes a route, and it dispatches
    /// nothing. Returns whether this node now has frame interest.
    #[doc(hidden)]
    pub fn propagate_frame_interest(&mut self) -> bool {
        let mut any = false;

        for (index, child) in self.children.iter_mut().enumerate() {
            if child.propagate_frame_interest() {
                any = true;

                if let Err(position) = self.frame_children.binary_search(&index) {
                    self.frame_children.insert(position, index);
                }
            }
        }

        if any {
            self.frame_interest = true;
        }

        self.frame_interest
    }

    /// Rebuilds the direct-child routing index during reconciliation or a full event pass.
    pub fn refresh_frame_children(&mut self) {
        self.frame_children.clear();
        self.frame_children.extend(
            self.children
                .iter()
                .enumerate()
                .filter_map(|(index, child)| child.has_frame_interest().then_some(index)),
        );
    }

    /// Reconciles one direct child in the routing index after its state changes.
    pub fn refresh_frame_child(&mut self, index: usize) {
        let interested = self
            .children
            .get(index)
            .is_some_and(Tree::has_frame_interest);

        match self.frame_children.binary_search(&index) {
            Ok(position) if !interested => {
                let _ = self.frame_children.remove(position);
            }
            Err(position) if interested => {
                self.frame_children.insert(position, index);
            }
            Ok(_) | Err(_) => {}
        }
    }

    /// Reconciles the children of the tree with the provided list of widgets.
    pub fn diff_children<'a, Message, Theme, Renderer>(
        &mut self,
        new_children: &mut [impl BorrowMut<dyn Widget<Message, Theme, Renderer> + 'a>],
    ) where
        Renderer: crate::Renderer,
    {
        diff_children(&mut self.children, new_children);
        // enzICED: the frame-interest routing index follows the children.
        self.refresh_frame_children();
    }

    /// Reconciles the children of the tree with the provided list of widgets using custom
    /// logic both for diffing and creating new widget state.
    pub fn diff_children_custom<T>(
        &mut self,
        new_children: &mut [T],
        diff: impl Fn(&mut Tree, &mut T),
        new_state: impl Fn(&T) -> Self,
    ) {
        diff_children_custom(&mut self.children, new_children, diff, new_state);
        // enzICED: the frame-interest routing index follows the children.
        self.refresh_frame_children();
    }
}

/// Reconciles the children of the tree with the provided list of widgets.
pub fn diff_children<'a, Message, Theme, Renderer>(
    old_children: &mut Vec<Tree>,
    new_children: &mut [impl BorrowMut<dyn Widget<Message, Theme, Renderer> + 'a>],
) where
    Renderer: crate::Renderer,
{
    diff_children_custom(
        old_children,
        new_children,
        |tree, widget| tree.diff(widget.borrow_mut()),
        |widget| Tree::new(widget.borrow()),
    );
}

/// Reconciles the children of the tree with the provided list of widgets using custom
/// logic both for diffing and creating new widget state.
pub fn diff_children_custom<T>(
    old_children: &mut Vec<Tree>,
    new_children: &mut [T],
    diff: impl Fn(&mut Tree, &mut T),
    new_state: impl Fn(&T) -> Tree,
) {
    if old_children.len() > new_children.len() {
        old_children.truncate(new_children.len());
    }

    if old_children.len() < new_children.len() {
        old_children.extend(new_children[old_children.len()..].iter().map(new_state));
    }

    for (child_state, new) in old_children.iter_mut().zip(new_children.iter_mut()) {
        diff(child_state, new);
    }
}

/// Reconciles the `current_children` with the provided list of widgets using
/// custom logic both for diffing and creating new widget state.
///
/// The algorithm will try to minimize the impact of diffing by querying the
/// `maybe_changed` closure.
pub fn diff_children_custom_with_search<T>(
    current_children: &mut Vec<Tree>,
    new_children: &mut [T],
    diff: impl Fn(&mut Tree, &mut T),
    maybe_changed: impl Fn(usize) -> bool,
    new_state: impl Fn(&T) -> Tree,
) {
    if new_children.is_empty() {
        current_children.clear();
        return;
    }

    if current_children.is_empty() {
        current_children.extend(new_children.iter().map(new_state));

        // TODO: Merge loop with extend logic (?)
        for (child_state, new) in current_children.iter_mut().zip(new_children.iter_mut()) {
            diff(child_state, new);
        }

        return;
    }

    let first_maybe_changed = maybe_changed(0);
    let last_maybe_changed = maybe_changed(current_children.len() - 1);

    if current_children.len() > new_children.len() {
        if !first_maybe_changed && last_maybe_changed {
            current_children.truncate(new_children.len());
        } else {
            let difference_index = if first_maybe_changed {
                0
            } else {
                (1..current_children.len())
                    .find(|&i| maybe_changed(i))
                    .unwrap_or(0)
            };

            let _ = current_children.splice(
                difference_index..difference_index + (current_children.len() - new_children.len()),
                std::iter::empty(),
            );
        }
    }

    if current_children.len() < new_children.len() {
        let first_maybe_changed = maybe_changed(0);
        let last_maybe_changed = maybe_changed(current_children.len() - 1);

        if !first_maybe_changed && last_maybe_changed {
            current_children.extend(new_children[current_children.len()..].iter().map(new_state));
        } else {
            let difference_index = if first_maybe_changed {
                0
            } else {
                (1..current_children.len())
                    .find(|&i| maybe_changed(i))
                    .unwrap_or(0)
            };

            let _ = current_children.splice(
                difference_index..difference_index,
                new_children[difference_index
                    ..difference_index + (new_children.len() - current_children.len())]
                    .iter()
                    .map(new_state),
            );
        }
    }

    // TODO: Merge loop with extend logic (?)
    for (child_state, new) in current_children.iter_mut().zip(new_children.iter_mut()) {
        diff(child_state, new);
    }
}

/// The identifier of some widget state.
#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct Tag(any::TypeId);

impl Tag {
    /// Creates a [`Tag`] for a state of type `T`.
    pub fn of<T>() -> Self
    where
        T: 'static,
    {
        Self(any::TypeId::of::<T>())
    }

    /// Creates a [`Tag`] for a stateless widget.
    pub fn stateless() -> Self {
        Self::of::<()>()
    }
}

/// Widget-local gesture ownership that can be revoked without synthesizing input events.
pub trait Interactive: Any {
    /// Clears pointer, keyboard and queued activation ownership, preserving focus and document data.
    /// Implementations must not publish actions or invoke application callbacks.
    fn cancel_interaction(&mut self);
}

/// The internal [`State`] of a widget.
pub enum State {
    /// No meaningful internal state.
    None,

    /// Some meaningful internal state.
    Some(Box<dyn Any>),

    /// State whose pending gestures must be revoked when presented geometry loses ownership.
    Interactive(Box<dyn Interactive>),
}

impl State {
    /// Stores gesture-aware state with the same downcast type as ordinary widget state.
    pub fn interactive<T: Interactive>(state: T) -> Self {
        Self::Interactive(Box::new(state))
    }

    /// Creates a new [`State`].
    pub fn new<T>(state: T) -> Self
    where
        T: 'static,
    {
        State::Some(Box::new(state))
    }

    /// Downcasts the [`State`] to `T` and returns a reference to it.
    ///
    /// # Panics
    /// This method will panic if the downcast fails or the [`State`] is [`State::None`].
    pub fn downcast_ref<T>(&self) -> &T
    where
        T: 'static,
    {
        match self {
            State::None => panic!("Downcast on stateless state"),
            State::Some(state) => state.downcast_ref().expect("Downcast widget state"),
            State::Interactive(state) => {
                let value: &dyn Any = state.as_ref();
                value
                    .downcast_ref()
                    .expect("Downcast interactive widget state")
            }
        }
    }

    /// Downcasts the [`State`] to `T` and returns a mutable reference to it.
    ///
    /// # Panics
    /// This method will panic if the downcast fails or the [`State`] is [`State::None`].
    pub fn downcast_mut<T>(&mut self) -> &mut T
    where
        T: 'static,
    {
        match self {
            State::None => panic!("Downcast on stateless state"),
            State::Some(state) => state.downcast_mut().expect("Downcast widget state"),
            State::Interactive(state) => {
                let value: &mut dyn Any = state.as_mut();
                value
                    .downcast_mut()
                    .expect("Downcast interactive widget state")
            }
        }
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "State::None"),
            Self::Some(_) => write!(f, "State::Some"),
            Self::Interactive(_) => write!(f, "State::Interactive"),
        }
    }
}
