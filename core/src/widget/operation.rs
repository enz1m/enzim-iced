// Modified by enzUI contributors. See FORK.md.
//! Query or update internal widget state.
pub mod focusable;
pub mod scrollable;
pub mod text_input;

pub use focusable::Focusable;
pub use scrollable::Scrollable;
pub use text_input::TextInput;

use crate::widget::Id;
use crate::{Point, Rectangle, Size, Vector};

use std::any::Any;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// A widget-owned clock group for an explicitly declared docked travel.
/// Validation must be side-effect free. A refused shift must leave every clock unchanged.
pub trait DockedClock {
    /// Whether every requested node/generation belongs to this one clock group.
    fn owns_animations(&self, animations: &[crate::renderer::TrackAnimation]) -> bool;
    /// Whether the entire group's clock and cached frame instants can be shifted atomically.
    fn can_shift(&self, delay: crate::time::Duration) -> bool;
    /// Applies a previously validated shift without layout, highlighting or application actions.
    fn shift(&mut self, delay: crate::time::Duration) -> bool;
}

/// Renderer-owned shaped text geometry exposed during widget operations.
///
/// This deliberately carries behavior rather than font metrics: consumers can
/// hit-test and obtain exact glyph rectangles without knowing the renderer's
/// concrete paragraph type.
pub trait SelectableTextLayout {
    /// Hit-test a paragraph-local point, returning a source byte offset.
    fn hit_test(&self, point: Point) -> Option<usize>;

    /// Return paragraph-local rectangles for glyphs overlapping `range`, whose
    /// endpoints are source byte offsets.
    fn range_bounds(&self, range: Range<usize>) -> Vec<Rectangle>;

    /// Return every grapheme's bounds from one walk of the layout, or `None`
    /// when unavailable. See [`crate::text::Paragraph::grapheme_bounds`].
    fn grapheme_bounds(&self) -> Option<Vec<crate::text::paragraph::GraphemeBounds>> {
        None
    }
}

impl<P: crate::text::Paragraph> SelectableTextLayout for P {
    fn hit_test(&self, point: Point) -> Option<usize> {
        // Route through the global source-offset API (#1816): renderers with
        // hard-line storage override this to add preceding line + line-ending
        // bytes, so a hit on any line after the first resolves to the correct
        // document offset instead of a line-local one.
        crate::text::Paragraph::hit_test_source_offset(self, point)
    }

    fn range_bounds(&self, range: Range<usize>) -> Vec<Rectangle> {
        crate::text::Paragraph::range_bounds(self, range)
    }

    fn grapheme_bounds(&self) -> Option<Vec<crate::text::paragraph::GraphemeBounds>> {
        crate::text::Paragraph::grapheme_bounds(self)
    }
}

/// A piece of logic that can traverse the widget tree of an application in
/// order to query or update some widget state.
pub trait Operation<T = ()>: Send {
    /// Requests further traversal of the widget tree to keep operating.
    ///
    /// The provided `operate` closure may be called by an [`Operation`]
    /// to return control to the widget tree and keep traversing it. If
    /// the closure is not called, the children of the widget asking for
    /// traversal will be skipped.
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>));

    /// Whether this pass exclusively reconciles displayed geometry without user actions.
    fn is_native_reconciliation(&self) -> bool {
        false
    }

    /// Whether this pass only synchronizes a declared docked animation clock.
    fn is_docked_clock_rebase(&self) -> bool {
        false
    }

    /// Visits a clock group without invoking widget layout or user actions.
    fn docked_clock(&mut self, _clock: &mut dyn DockedClock) {}

    /// Reports that the exact captured layout domain was projected before native reconciliation.
    fn projection_applied(&mut self, _root: crate::renderer::NodeId) {}

    /// Selects an editor by its exact retained allocation identity, not by equal bounds.
    fn needs_editor_reconciliation(&self, _node: crate::renderer::NodeId) -> bool {
        false
    }

    /// Reports success or refusal for one requested native editor; missing reports must refuse.
    fn editor_reconciled(&mut self, _node: crate::renderer::NodeId, _success: bool) {}

    /// Selects a scroll owner by its exact retained allocation identity.
    fn needs_scroll_reconciliation(&self, _node: crate::renderer::NodeId) -> bool {
        false
    }

    /// Reports one native scroll validation; missing or duplicate reports must refuse input.
    fn scroll_reconciled(&mut self, _node: crate::renderer::NodeId, _success: bool) {}

    /// Operates on a widget that contains other widgets.
    fn container(&mut self, _id: Option<&Id>, _bounds: Rectangle, _viewport: &Rectangle) {}

    /// Operates on a widget that can be scrolled.
    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content: Size,
        _translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
    }

    /// Operates on a widget that can be focused.
    fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, _state: &mut dyn Focusable) {}

    /// Operates on a widget that has text input.
    fn text_input(&mut self, _id: Option<&Id>, _bounds: Rectangle, _state: &mut dyn TextInput) {}

    /// Operates on a widget that contains some text.
    fn text(&mut self, _id: Option<&Id>, _bounds: Rectangle, _text: &str) {}

    /// Operates on text together with the renderer's shaped geometry.
    ///
    /// The default preserves compatibility by forwarding to [`Self::text`].
    /// Selection-aware operations override this to consume exact hit testing
    /// and range rectangles.
    fn selectable_text(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        text: &str,
        _layout: &dyn SelectableTextLayout,
    ) {
        self.text(id, bounds, text);
    }

    /// Operates on a custom widget with some state.
    fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, _state: &mut dyn Any) {}

    /// Finishes the [`Operation`] and returns its [`Outcome`].
    fn finish(&self) -> Outcome<T> {
        Outcome::None
    }
}

impl<T, O> Operation<O> for Box<T>
where
    T: Operation<O> + ?Sized,
{
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<O>)) {
        self.as_mut().traverse(operate);
    }

    fn is_native_reconciliation(&self) -> bool {
        self.as_ref().is_native_reconciliation()
    }
    fn is_docked_clock_rebase(&self) -> bool {
        self.as_ref().is_docked_clock_rebase()
    }
    fn docked_clock(&mut self, clock: &mut dyn DockedClock) {
        self.as_mut().docked_clock(clock);
    }

    fn projection_applied(&mut self, root: crate::renderer::NodeId) {
        self.as_mut().projection_applied(root);
    }

    fn needs_editor_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
        self.as_ref().needs_editor_reconciliation(node)
    }

    fn editor_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
        self.as_mut().editor_reconciled(node, success);
    }

    fn needs_scroll_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
        self.as_ref().needs_scroll_reconciliation(node)
    }

    fn scroll_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
        self.as_mut().scroll_reconciled(node, success);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle, viewport: &Rectangle) {
        self.as_mut().container(id, bounds, viewport);
    }

    fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        self.as_mut().focusable(id, bounds, state);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content: Size,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        self.as_mut()
            .scrollable(id, bounds, content, translation, state);
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
        self.as_mut().text_input(id, bounds, state);
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.as_mut().text(id, bounds, text);
    }

    fn selectable_text(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        text: &str,
        layout: &dyn SelectableTextLayout,
    ) {
        self.as_mut().selectable_text(id, bounds, text, layout);
    }

    fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        self.as_mut().custom(id, bounds, state);
    }

    fn finish(&self) -> Outcome<O> {
        self.as_ref().finish()
    }
}

/// The result of an [`Operation`].
pub enum Outcome<T> {
    /// The [`Operation`] produced no result.
    None,

    /// The [`Operation`] produced some result.
    Some(T),

    /// The [`Operation`] needs to be followed by another [`Operation`].
    Chain(Box<dyn Operation<T>>),
}

impl<T> fmt::Debug for Outcome<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "Outcome::None"),
            Self::Some(output) => write!(f, "Outcome::Some({output:?})"),
            Self::Chain(_) => write!(f, "Outcome::Chain(...)"),
        }
    }
}

/// The animation to apply to a state change, as requested by an
/// [`Operation`].
///
/// Some widgets can apply a state change either immediately or with an
/// animation; operations on those widgets take an [`Animation`] to select
/// which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Animation {
    /// Use the default animation of the widget.
    #[default]
    Auto,

    /// Apply the change immediately.
    Instant,

    /// Animate the change towards the new state.
    Smooth,
}

/// Wraps the [`Operation`] in a black box, erasing its returning type.
pub fn black_box<'a, T, O>(operation: &'a mut dyn Operation<T>) -> impl Operation<O> + 'a
where
    T: 'a,
{
    struct BlackBox<'a, T> {
        operation: &'a mut dyn Operation<T>,
    }

    impl<T, O> Operation<O> for BlackBox<'_, T> {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<O>))
        where
            Self: Sized,
        {
            self.operation.traverse(&mut |operation| {
                operate(&mut BlackBox { operation });
            });
        }

        fn is_native_reconciliation(&self) -> bool {
            self.operation.is_native_reconciliation()
        }
        fn is_docked_clock_rebase(&self) -> bool {
            self.operation.is_docked_clock_rebase()
        }
        fn docked_clock(&mut self, clock: &mut dyn DockedClock) {
            self.operation.docked_clock(clock);
        }
        fn projection_applied(&mut self, root: crate::renderer::NodeId) {
            self.operation.projection_applied(root);
        }
        fn needs_editor_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_editor_reconciliation(node)
        }
        fn editor_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.editor_reconciled(node, success);
        }

        fn needs_scroll_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_scroll_reconciliation(node)
        }
        fn scroll_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.scroll_reconciled(node, success);
        }

        fn container(&mut self, id: Option<&Id>, bounds: Rectangle, viewport: &Rectangle) {
            self.operation.container(id, bounds, viewport);
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
            self.operation.focusable(id, bounds, state);
        }

        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            content: Size,
            translation: Vector,
            state: &mut dyn Scrollable,
        ) {
            self.operation
                .scrollable(id, bounds, content, translation, state);
        }

        fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
            self.operation.text_input(id, bounds, state);
        }

        fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
            self.operation.text(id, bounds, text);
        }

        fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
            self.operation.custom(id, bounds, state);
        }

        fn finish(&self) -> Outcome<O> {
            Outcome::None
        }
    }

    BlackBox { operation }
}

/// Maps the output of an [`Operation`] using the given function.
pub fn map<A, B>(
    operation: impl Operation<A>,
    f: impl Fn(A) -> B + Send + Sync + 'static,
) -> impl Operation<B>
where
    A: 'static,
    B: 'static,
{
    struct Map<O, A, B> {
        operation: O,
        f: Arc<dyn Fn(A) -> B + Send + Sync>,
    }

    impl<O, A, B> Operation<B> for Map<O, A, B>
    where
        O: Operation<A>,
        A: 'static,
        B: 'static,
    {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<B>)) {
            struct MapRef<'a, A> {
                operation: &'a mut dyn Operation<A>,
            }

            impl<A, B> Operation<B> for MapRef<'_, A> {
                fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<B>)) {
                    self.operation.traverse(&mut |operation| {
                        operate(&mut MapRef { operation });
                    });
                }

                fn is_native_reconciliation(&self) -> bool {
                    self.operation.is_native_reconciliation()
                }
                fn is_docked_clock_rebase(&self) -> bool {
                    self.operation.is_docked_clock_rebase()
                }
                fn docked_clock(&mut self, clock: &mut dyn DockedClock) {
                    self.operation.docked_clock(clock);
                }
                fn projection_applied(&mut self, root: crate::renderer::NodeId) {
                    self.operation.projection_applied(root);
                }
                fn needs_editor_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
                    self.operation.needs_editor_reconciliation(node)
                }
                fn editor_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
                    self.operation.editor_reconciled(node, success);
                }

                fn needs_scroll_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
                    self.operation.needs_scroll_reconciliation(node)
                }
                fn scroll_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
                    self.operation.scroll_reconciled(node, success);
                }

                fn container(&mut self, id: Option<&Id>, bounds: Rectangle, viewport: &Rectangle) {
                    let Self { operation, .. } = self;

                    operation.container(id, bounds, viewport);
                }

                fn scrollable(
                    &mut self,
                    id: Option<&Id>,
                    bounds: Rectangle,
                    content: Size,
                    translation: Vector,
                    state: &mut dyn Scrollable,
                ) {
                    self.operation
                        .scrollable(id, bounds, content, translation, state);
                }

                fn focusable(
                    &mut self,
                    id: Option<&Id>,
                    bounds: Rectangle,
                    state: &mut dyn Focusable,
                ) {
                    self.operation.focusable(id, bounds, state);
                }

                fn text_input(
                    &mut self,
                    id: Option<&Id>,
                    bounds: Rectangle,
                    state: &mut dyn TextInput,
                ) {
                    self.operation.text_input(id, bounds, state);
                }

                fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
                    self.operation.text(id, bounds, text);
                }

                fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
                    self.operation.custom(id, bounds, state);
                }
            }

            self.operation.traverse(&mut |operation| {
                operate(&mut MapRef { operation });
            });
        }

        fn is_native_reconciliation(&self) -> bool {
            self.operation.is_native_reconciliation()
        }
        fn is_docked_clock_rebase(&self) -> bool {
            self.operation.is_docked_clock_rebase()
        }
        fn docked_clock(&mut self, clock: &mut dyn DockedClock) {
            self.operation.docked_clock(clock);
        }
        fn projection_applied(&mut self, root: crate::renderer::NodeId) {
            self.operation.projection_applied(root);
        }
        fn needs_editor_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_editor_reconciliation(node)
        }
        fn editor_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.editor_reconciled(node, success);
        }

        fn needs_scroll_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_scroll_reconciliation(node)
        }
        fn scroll_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.scroll_reconciled(node, success);
        }

        fn container(&mut self, id: Option<&Id>, bounds: Rectangle, viewport: &Rectangle) {
            self.operation.container(id, bounds, viewport);
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
            self.operation.focusable(id, bounds, state);
        }

        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            content: Size,
            translation: Vector,
            state: &mut dyn Scrollable,
        ) {
            self.operation
                .scrollable(id, bounds, content, translation, state);
        }

        fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
            self.operation.text_input(id, bounds, state);
        }

        fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
            self.operation.text(id, bounds, text);
        }

        fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
            self.operation.custom(id, bounds, state);
        }

        fn finish(&self) -> Outcome<B> {
            match self.operation.finish() {
                Outcome::None => Outcome::None,
                Outcome::Some(output) => Outcome::Some((self.f)(output)),
                Outcome::Chain(next) => Outcome::Chain(Box::new(Map {
                    operation: next,
                    f: self.f.clone(),
                })),
            }
        }
    }

    Map {
        operation,
        f: Arc::new(f),
    }
}

/// Chains the output of an [`Operation`] with the provided function to
/// build a new [`Operation`].
pub fn then<A, B, O>(
    operation: impl Operation<A> + 'static,
    next: impl Fn(A) -> O + Send + Sync + 'static,
) -> impl Operation<B>
where
    A: 'static,
    B: 'static,
    O: Operation<B> + 'static,
{
    struct Chain<T, O, A> {
        operation: T,
        next: Arc<dyn Fn(A) -> O + Send + Sync>,
    }

    impl<T, O, A, B> Operation<B> for Chain<T, O, A>
    where
        T: Operation<A> + 'static,
        O: Operation<B> + 'static,
        A: 'static,
        B: 'static,
    {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<B>)) {
            self.operation.traverse(&mut |operation| {
                operate(&mut black_box(operation));
            });
        }

        fn is_native_reconciliation(&self) -> bool {
            self.operation.is_native_reconciliation()
        }
        fn is_docked_clock_rebase(&self) -> bool {
            self.operation.is_docked_clock_rebase()
        }
        fn docked_clock(&mut self, clock: &mut dyn DockedClock) {
            self.operation.docked_clock(clock);
        }
        fn projection_applied(&mut self, root: crate::renderer::NodeId) {
            self.operation.projection_applied(root);
        }
        fn needs_editor_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_editor_reconciliation(node)
        }
        fn editor_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.editor_reconciled(node, success);
        }

        fn needs_scroll_reconciliation(&self, node: crate::renderer::NodeId) -> bool {
            self.operation.needs_scroll_reconciliation(node)
        }
        fn scroll_reconciled(&mut self, node: crate::renderer::NodeId, success: bool) {
            self.operation.scroll_reconciled(node, success);
        }

        fn container(&mut self, id: Option<&Id>, bounds: Rectangle, viewport: &Rectangle) {
            self.operation.container(id, bounds, viewport);
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
            self.operation.focusable(id, bounds, state);
        }

        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            content: Size,
            translation: crate::Vector,
            state: &mut dyn Scrollable,
        ) {
            self.operation
                .scrollable(id, bounds, content, translation, state);
        }

        fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
            self.operation.text_input(id, bounds, state);
        }

        fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
            self.operation.text(id, bounds, text);
        }

        fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
            self.operation.custom(id, bounds, state);
        }

        fn finish(&self) -> Outcome<B> {
            match self.operation.finish() {
                Outcome::None => Outcome::None,
                Outcome::Some(value) => Outcome::Chain(Box::new((self.next)(value))),
                Outcome::Chain(next) => Outcome::Chain(Box::new(Chain {
                    operation: next,
                    next: self.next.clone(),
                })),
            }
        }
    }

    Chain {
        operation,
        next: Arc::new(next),
    }
}

/// Produces an [`Operation`] that applies the given [`Operation`] to the
/// children of a container with the given [`Id`].
pub fn scope<T: 'static>(target: Id, operation: impl Operation<T> + 'static) -> impl Operation<T> {
    struct ScopedOperation<Message> {
        target: Id,
        current: Option<Id>,
        operation: Box<dyn Operation<Message>>,
    }

    impl<Message: 'static> Operation<Message> for ScopedOperation<Message> {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Message>)) {
            if self.current.as_ref() == Some(&self.target) {
                self.operation.as_mut().traverse(operate);
            } else {
                operate(self);
            }

            self.current = None;
        }

        fn container(&mut self, id: Option<&Id>, _bounds: Rectangle, _viewport: &Rectangle) {
            self.current = id.cloned();
        }

        fn finish(&self) -> Outcome<Message> {
            match self.operation.finish() {
                Outcome::Chain(next) => Outcome::Chain(Box::new(ScopedOperation {
                    target: self.target.clone(),
                    current: None,
                    operation: next,
                })),
                outcome => outcome,
            }
        }
    }

    ScopedOperation {
        target,
        current: None,
        operation: Box::new(operation),
    }
}
