// Modified by enzUI contributors. See FORK.md.
//! Display content on top of other content.
use crate::core::layout;
use crate::core::layout::Measure as _;
use crate::core::mouse;
use crate::core::overlay;
use crate::core::renderer;
use crate::core::widget::{Operation, Tree};
use crate::core::{Element, Event, Layout, Length, Rectangle, Shell, Size, Vector, Widget};

/// A container that displays children on top of each other.
///
/// The first [`Element`] dictates the intrinsic [`Size`] of a [`Stack`] and
/// will be displayed as the base layer. Every consecutive [`Element`] will be
/// rendered on top; on its own layer.
///
/// You can use [`push_under`](Self::push_under) to push an [`Element`] under
/// the current [`Stack`] without affecting its intrinsic [`Size`].
///
/// Keep in mind that too much layering will normally produce bad UX as well as
/// introduce certain rendering overhead. Use this widget sparingly!
pub struct Stack<'a, Message, Theme = crate::Theme, Renderer = crate::Renderer> {
    width: Length,
    height: Length,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    clip: bool,
    base_layer: usize,
}

impl<'a, Message, Theme, Renderer> Stack<'a, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    /// Creates an empty [`Stack`].
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    /// Creates a [`Stack`] with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(Vec::with_capacity(capacity))
    }

    /// Creates a [`Stack`] with the given elements.
    pub fn with_children(
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        let iterator = children.into_iter();

        Self::with_capacity(iterator.size_hint().0).extend(iterator)
    }

    /// Creates a [`Stack`] from an already allocated [`Vec`].
    pub fn from_vec(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            width: Length::Fit,
            height: Length::Fit,
            children,
            clip: false,
            base_layer: 0,
        }
    }

    /// Sets the width of the [`Stack`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Stack`].
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Adds an element on top of the [`Stack`].
    pub fn push(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        let child = child.into();

        if !child.as_widget().is_void() {
            self.children.push(child);
        }

        self
    }

    /// Adds an element under the [`Stack`].
    pub fn push_under(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.children.insert(0, child.into());
        self.base_layer += 1;
        self
    }

    /// Extends the [`Stack`] with the given children.
    pub fn extend(
        self,
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        children.into_iter().fold(self, Self::push)
    }

    /// Sets whether the [`Stack`] should clip overflowing content.
    ///
    /// It has a slight performance overhead during presentation.
    ///
    /// By default, it is set to `false`.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }
}

impl<Message, Renderer> Default for Stack<'_, Message, Renderer>
where
    Renderer: crate::core::Renderer,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Stack<'a, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.children);

        if let Some(base) = self.children.get(self.base_layer) {
            let size = base.as_widget().size();

            self.width = self.width.cross(size.width);
            self.height = self.height.cross(size.height);
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        let sizing = Size::new(self.width, self.height);
        let children = self
            .children
            .iter()
            .map(|child| child.as_widget().size())
            .collect();
        let node = match layout::overlay::try_resolve(
            limits,
            sizing,
            self.base_layer,
            self.children.len(),
            |index, limits| {
                Ok::<_, std::convert::Infallible>(self.children[index].as_widget_mut().measure(
                    &mut tree.children[index],
                    renderer,
                    limits,
                ))
            },
        ) {
            Ok(node) => node,
            Err(error) => match error {},
        };
        node.with_overlay_presentation(*limits, sizing, self.base_layer, children)
            .apply(tree);
    }

    fn frame_interest(&self, tree: &Tree) -> bool {
        !tree.frame_children.is_empty()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds(), viewport);
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(layout.iter_mut(&mut tree.children))
                .for_each(|(child, (layout, state))| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, viewport, renderer, operation);
                });
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        mut cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.children.is_empty() {
            return;
        }

        let is_over = cursor.is_over(layout.bounds());
        let end = self.children.len() - 1;

        for index in (0..self.children.len()).rev() {
            let reverse_position = self.children.len() - index - 1;
            let child = &mut self.children[index];
            let child_layout = layout.child(&tree.children, index);
            let child_tree = &mut tree.children[index];

            child.as_widget_mut().update(
                child_tree,
                event,
                child_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
            child_tree.refresh_frame_interest(child.as_widget());

            let interaction = if reverse_position < end && is_over && !cursor.is_levitating() {
                child.as_widget().mouse_interaction(
                    child_tree,
                    child_layout,
                    cursor,
                    viewport,
                    renderer,
                )
            } else {
                mouse::Interaction::None
            };

            tree.refresh_frame_child(index);

            if shell.is_event_captured() {
                if matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                        | Event::Touch(crate::core::touch::Event::FingerPressed { .. })
                ) {
                    // Skipped layers must release focus without receiving the captured press.
                    let mut unfocus = crate::core::widget::operation::focusable::unfocus::<()>();
                    for skipped in 0..index {
                        let skipped_layout = layout.child(&tree.children, skipped);
                        self.children[skipped].as_widget_mut().operate(
                            &mut tree.children[skipped],
                            skipped_layout,
                            viewport,
                            renderer,
                            &mut unfocus,
                        );
                    }
                    shell.request_redraw();
                }
                return;
            }

            if interaction != mouse::Interaction::None {
                cursor = cursor.levitate();
            }
        }
    }

    fn update_frame(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // A layer without frame interest can still shield the cursor for layers below it.
        let shielding_layer = if cursor.is_over(layout.bounds()) && cursor.is_over(*viewport) {
            self.children
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, child)| {
                    let child_layout = layout.child(&tree.children, index);
                    (child.as_widget().mouse_interaction(
                        &tree.children[index],
                        child_layout,
                        cursor,
                        viewport,
                        renderer,
                    ) != mouse::Interaction::None)
                        .then_some(index)
                })
        } else {
            None
        };
        let mut route = tree.frame_children.len();

        while route > 0 {
            route -= 1;
            let index = tree.frame_children[route];
            let child = &mut self.children[index];
            let child_layout = layout.child(&tree.children, index);
            let child_tree = &mut tree.children[index];

            child.as_widget_mut().update_frame(
                child_tree,
                event,
                child_layout,
                if shielding_layer.is_some_and(|shield| index < shield) {
                    cursor.levitate()
                } else {
                    cursor
                },
                renderer,
                shell,
                viewport,
            );
            child_tree.refresh_frame_interest(child.as_widget());

            if !child_tree.has_frame_interest() {
                let _ = tree.frame_children.remove(route);
            }

            if shell.is_event_captured() {
                return;
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .rev()
            .zip(layout.iter(&tree.children).rev())
            .map(|(child, (layout, tree))| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .find(|&interaction| interaction != mouse::Interaction::None)
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(clipped_viewport) = layout.bounds().intersection(viewport) {
            let viewport = if self.clip {
                &clipped_viewport
            } else {
                viewport
            };

            let layers_under = if cursor.is_over(layout.bounds()) {
                self.children
                    .iter()
                    .rev()
                    .zip(layout.iter(&tree.children).rev())
                    .position(|(layer, (layout, tree))| {
                        let interaction = layer
                            .as_widget()
                            .mouse_interaction(tree, layout, cursor, viewport, renderer);

                        interaction != mouse::Interaction::None
                    })
                    .map(|i| self.children.len() - i - 1)
                    .unwrap_or_default()
            } else {
                0
            };

            let mut layers = self
                .children
                .iter()
                .zip(layout.iter(&tree.children))
                .enumerate();

            let layers = layers.by_ref();

            let mut draw_layer =
                |i, layer: &Element<'a, Message, Theme, Renderer>, tree, layout, cursor| {
                    if i > 0 {
                        let draw = |renderer: &mut Renderer| {
                            layer
                                .as_widget()
                                .draw(tree, renderer, theme, style, layout, cursor, viewport);
                        };
                        if self.clip {
                            renderer.with_layer(*viewport, draw);
                        } else {
                            renderer.with_layout_layer(
                                renderer::LayoutLayer::Inherited,
                                *viewport,
                                None,
                                draw,
                            );
                        }
                    } else {
                        layer
                            .as_widget()
                            .draw(tree, renderer, theme, style, layout, cursor, viewport);
                    }
                };

            for (i, (layer, (layout, tree))) in layers.take(layers_under) {
                draw_layer(i, layer, tree, layout, mouse::Cursor::Unavailable);
            }

            for (i, (layer, (layout, tree))) in layers {
                draw_layer(i, layer, tree, layout, cursor);
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
        window: Size,
    ) -> Vec<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
            window,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Stack<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: crate::core::Renderer + 'a,
{
    fn from(stack: Stack<'a, Message, Theme, Renderer>) -> Self {
        Self::new(stack)
    }
}
