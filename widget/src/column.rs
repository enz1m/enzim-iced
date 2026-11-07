// Modified by enzUI contributors. See FORK.md.
//! Distribute content vertically.
use crate::core::alignment::{self, Alignment};
use crate::core::layout;
use crate::core::mouse;
use crate::core::overlay;
use crate::core::renderer;
use crate::core::widget::{Operation, Tree};
use crate::core::{
    Element, Event, Layout, Length, Padding, Pixels, Rectangle, Shell, Size, Vector, Widget,
};

/// A container that distributes its contents vertically.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } }
/// # pub type State = ();
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// use iced::widget::{button, column};
///
/// #[derive(Debug, Clone)]
/// enum Message {
///     // ...
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     column![
///         "I am on top!",
///         button("I am in the center!"),
///         "I am below.",
///     ].into()
/// }
/// ```
pub struct Column<'a, Message, Theme = crate::Theme, Renderer = crate::Renderer> {
    spacing: f32,
    padding: Padding,
    width: Length,
    height: Length,
    align: Alignment,
    clip: bool,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
}

impl<'a, Message, Theme, Renderer> Column<'a, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    /// Creates an empty [`Column`].
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    /// Creates a [`Column`] with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(Vec::with_capacity(capacity))
    }

    /// Creates a [`Column`] with the given elements.
    pub fn with_children(
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        let iterator = children.into_iter();

        Self::with_capacity(iterator.size_hint().0).extend(iterator)
    }

    /// Creates a [`Column`] from an already allocated [`Vec`].
    pub fn from_vec(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Fit,
            height: Length::Fit,
            align: Alignment::Start,
            clip: false,
            children,
        }
    }

    /// Sets the vertical spacing _between_ elements.
    ///
    /// Custom margins per element do not exist in iced. You should use this
    /// method instead! While less flexible, it helps you keep spacing between
    /// elements consistent.
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into().0;
        self
    }

    /// Sets the [`Padding`] of the [`Column`].
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the width of the [`Column`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Column`].
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the horizontal alignment of the contents of the [`Column`] .
    pub fn align_x(mut self, align: impl Into<alignment::Horizontal>) -> Self {
        self.align = Alignment::from(align.into());
        self
    }

    /// Sets whether the contents of the [`Column`] should be clipped on
    /// overflow.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    /// Adds an element to the [`Column`].
    pub fn push(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        let child = child.into();

        if !child.as_widget().is_void() {
            self.children.push(child);
        }

        self
    }

    /// Extends the [`Column`] with the given children.
    pub fn extend(
        self,
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        children.into_iter().fold(self, Self::push)
    }

    /// Turns the [`Column`] into a [`Wrapping`] column.
    ///
    /// The original alignment of the [`Column`] is preserved per column wrapped.
    pub fn wrap(self) -> Wrapping<'a, Message, Theme, Renderer> {
        Wrapping {
            column: self,
            horizontal_spacing: None,
            align_y: alignment::Vertical::Top,
        }
    }
}

impl<Message, Renderer> Default for Column<'_, Message, Renderer>
where
    Renderer: crate::core::Renderer,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, Message, Theme, Renderer: crate::core::Renderer>
    FromIterator<Element<'a, Message, Theme, Renderer>> for Column<'a, Message, Theme, Renderer>
{
    fn from_iter<T: IntoIterator<Item = Element<'a, Message, Theme, Renderer>>>(iter: T) -> Self {
        Self::with_children(iter)
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Column<'_, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.children);

        if self.width.is_fit() || self.height.is_fit() {
            for child in &self.children {
                let size = child.as_widget().size();

                self.width = self.width.cross(size.width);
                self.height = self.height.stack(size.height);
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            limits,
            self.width,
            self.height,
            self.padding,
            self.spacing,
            self.align,
            &mut self.children,
            &mut tree.children,
        )
        .with_drawing_cull(self.clip)
        .with_child_drawing_culls()
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
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for (child, (layout, tree)) in self
            .children
            .iter_mut()
            .zip(layout.iter_mut(&mut tree.children))
        {
            child
                .as_widget_mut()
                .update(tree, event, layout, cursor, renderer, shell, viewport);
            tree.refresh_frame_interest(child.as_widget());
        }
        tree.refresh_frame_children();
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
        let Tree {
            children,
            frame_children,
            ..
        } = tree;
        let mut route = 0;

        while route < frame_children.len() {
            let index = frame_children[route];
            let child = &mut self.children[index];
            let child_layout = layout.child(children, index);
            let child_tree = &mut children[index];

            child.as_widget_mut().update_frame(
                child_tree,
                event,
                child_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
            child_tree.refresh_frame_interest(child.as_widget());

            if child_tree.has_frame_interest() {
                route += 1;
            } else {
                let _ = frame_children.remove(route);
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
            .zip(layout.iter(&tree.children))
            .map(|(child, (layout, tree))| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
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
        layout.draw_culled(tree, renderer, viewport, |renderer, viewport| {
            for (child, (layout, tree)) in self.children.iter().zip(layout.iter(&tree.children)) {
                layout.draw_culled(tree, renderer, viewport, |renderer, viewport| {
                    child
                        .as_widget()
                        .draw(tree, renderer, theme, style, layout, cursor, viewport);
                });
            }
        });
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

impl<'a, Message, Theme, Renderer> From<Column<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: crate::core::Renderer + 'a,
{
    fn from(column: Column<'a, Message, Theme, Renderer>) -> Self {
        Self::new(column)
    }
}

/// A [`Column`] that wraps its contents.
///
/// Create a [`Column`] first, and then call [`Column::wrap`] to
/// obtain a [`Column`] that wraps its contents.
///
/// The original alignment of the [`Column`] is preserved per column wrapped.
#[allow(missing_debug_implementations)]
pub struct Wrapping<'a, Message, Theme = crate::Theme, Renderer = crate::Renderer> {
    column: Column<'a, Message, Theme, Renderer>,
    horizontal_spacing: Option<f32>,
    align_y: alignment::Vertical,
}

impl<Message, Theme, Renderer> Wrapping<'_, Message, Theme, Renderer> {
    /// Sets the horizontal spacing _between_ columns.
    pub fn horizontal_spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.horizontal_spacing = Some(amount.into().0);
        self
    }

    /// Sets the vertical alignment of the wrapping [`Column`].
    pub fn align_y(mut self, align_y: impl Into<alignment::Vertical>) -> Self {
        self.align_y = align_y.into();
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Wrapping<'_, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    fn diff(&mut self, tree: &mut Tree) {
        self.column.diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.column.size()
    }

    fn frame_interest(&self, tree: &Tree) -> bool {
        self.column.frame_interest(tree)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        layout::flow::resolve(
            renderer,
            limits,
            layout::flow::Rule {
                axis: layout::flow::Axis::Vertical,
                sizing: Size::new(self.column.width, self.column.height),
                padding: self.column.padding,
                gaps: [
                    self.column.spacing,
                    self.horizontal_spacing.unwrap_or(self.column.spacing),
                ],
                cross_alignment: self.column.align,
                main_alignment: self.align_y.into(),
            },
            &mut self.column.children,
            &mut tree.children,
        )
        .with_drawing_cull(self.column.clip)
        .with_child_drawing_culls()
        .apply(tree);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.column
            .operate(tree, layout, viewport, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.column
            .update(tree, event, layout, cursor, renderer, shell, viewport);
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
        self.column
            .update_frame(tree, event, layout, cursor, renderer, shell, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.column
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        self.column
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
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
        self.column
            .overlay(tree, layout, renderer, viewport, translation, window)
    }
}

impl<'a, Message, Theme, Renderer> From<Wrapping<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: crate::core::Renderer + 'a,
{
    fn from(column: Wrapping<'a, Message, Theme, Renderer>) -> Self {
        Self::new(column)
    }
}

#[cfg(test)]
mod frame_routing_tests {
    use super::Column;
    use crate::core::layout;
    use crate::core::mouse;
    use crate::core::renderer;
    use crate::core::shell;
    use crate::core::time::Instant;
    use crate::core::widget::Tree;
    use crate::core::window;
    use crate::core::{Element, Event, Layout, Length, Rectangle, Shell, Size, Widget};
    use iced_runtime::user_interface::{self, UserInterface};

    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Probe {
        active: Rc<Cell<bool>>,
        frame_updates: Rc<Cell<usize>>,
        all_updates: Rc<Cell<usize>>,
        interest_queries: Rc<Cell<usize>>,
    }

    impl Widget<(), (), ()> for Probe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }

        fn frame_interest(&self, _tree: &Tree) -> bool {
            self.interest_queries.set(self.interest_queries.get() + 1);
            self.active.get()
        }

        fn layout(&mut self, tree: &mut Tree, _renderer: &(), _limits: &layout::Limits) {
            layout::Node::new(Size::ZERO).apply(tree);
        }

        fn update(
            &mut self,
            _tree: &mut Tree,
            event: &Event,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _renderer: &(),
            shell: &mut Shell<'_, ()>,
            _viewport: &Rectangle,
        ) {
            self.all_updates.set(self.all_updates.get() + 1);

            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                self.frame_updates.set(self.frame_updates.get() + 1);

                if self.active.get() {
                    shell.request_redraw();
                }
            }
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut (),
            _theme: &(),
            _style: &renderer::Style,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    struct WakeProbe {
        active: Rc<Cell<bool>>,
        frames: Rc<Cell<usize>>,
    }

    impl Widget<(), (), ()> for WakeProbe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }

        fn frame_interest(&self, _tree: &Tree) -> bool {
            self.active.get()
        }

        fn layout(&mut self, tree: &mut Tree, _renderer: &(), _limits: &layout::Limits) {
            layout::Node::new(Size::ZERO).apply(tree);
        }

        fn update(
            &mut self,
            _tree: &mut Tree,
            event: &Event,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _renderer: &(),
            shell: &mut Shell<'_, ()>,
            _viewport: &Rectangle,
        ) {
            match event {
                Event::Window(window::Event::Focused) => {
                    self.active.set(true);
                    shell.request_redraw();
                }
                Event::Window(window::Event::RedrawRequested(_)) => {
                    self.frames.set(self.frames.get() + 1);
                }
                _ => {}
            }
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut (),
            _theme: &(),
            _style: &renderer::Style,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    struct OneShotProbe {
        active: Rc<Cell<bool>>,
        frames: Rc<Cell<usize>>,
    }

    impl Widget<(), (), ()> for OneShotProbe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }

        fn frame_interest(&self, _tree: &Tree) -> bool {
            self.active.get()
        }

        fn layout(&mut self, tree: &mut Tree, _renderer: &(), _limits: &layout::Limits) {
            layout::Node::new(Size::ZERO).apply(tree);
        }

        fn update(
            &mut self,
            _tree: &mut Tree,
            event: &Event,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _renderer: &(),
            _shell: &mut Shell<'_, ()>,
            _viewport: &Rectangle,
        ) {
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                self.frames.set(self.frames.get() + 1);
                self.active.set(false);
            }
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut (),
            _theme: &(),
            _style: &renderer::Style,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    struct FallbackProbe(Rc<Cell<usize>>);

    impl Widget<(), (), ()> for FallbackProbe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }

        fn layout(&mut self, tree: &mut Tree, _renderer: &(), _limits: &layout::Limits) {
            layout::Node::new(Size::ZERO).apply(tree);
        }

        fn update(
            &mut self,
            _tree: &mut Tree,
            event: &Event,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _renderer: &(),
            _shell: &mut Shell<'_, ()>,
            _viewport: &Rectangle,
        ) {
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                self.0.set(self.0.get() + 1);
            }
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut (),
            _theme: &(),
            _style: &renderer::Style,
            _layout: Layout,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    fn explicit_probe(
        active: Rc<Cell<bool>>,
        frame_updates: Rc<Cell<usize>>,
        all_updates: Rc<Cell<usize>>,
    ) -> Element<'static, (), (), ()> {
        explicit_probe_counted(active, frame_updates, all_updates, Rc::new(Cell::new(0)))
    }

    fn explicit_probe_counted(
        active: Rc<Cell<bool>>,
        frame_updates: Rc<Cell<usize>>,
        all_updates: Rc<Cell<usize>>,
        interest_queries: Rc<Cell<usize>>,
    ) -> Element<'static, (), (), ()> {
        Element::new(Probe {
            active,
            frame_updates,
            all_updates,
            interest_queries,
        })
    }

    fn frame(ui: &mut UserInterface<'_, (), (), ()>, renderer: &mut ()) -> window::RedrawRequest {
        dispatch(
            ui,
            renderer,
            Event::Window(window::Event::RedrawRequested(Instant::now())),
        )
    }

    fn dispatch(
        ui: &mut UserInterface<'_, (), (), ()>,
        renderer: &mut (),
        event: Event,
    ) -> window::RedrawRequest {
        let mut messages = crate::core::shell::Bus::new();
        let (state, statuses) = ui.update(
            &window::Headless,
            &shell::Waker::noop(),
            &[event],
            mouse::Cursor::Unavailable,
            renderer,
            &mut messages,
        );

        assert_eq!(statuses.len(), 1);
        match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => panic!("probe UI unexpectedly became outdated"),
        }
    }

    fn build(
        root: impl Into<Element<'static, (), (), ()>>,
        cache: user_interface::Cache,
        renderer: &mut (),
    ) -> UserInterface<'static, (), (), ()> {
        UserInterface::build(root, Size::new(100.0, 100.0), cache, renderer)
    }

    #[test]
    fn one_listener_skips_ten_thousand_static_siblings_for_one_thousand_frames() {
        let active = Rc::new(Cell::new(true));
        let listener_frames = Rc::new(Cell::new(0));
        let static_frames = Rc::new(Cell::new(0));
        let all_updates = Rc::new(Cell::new(0));
        let mut children = Vec::with_capacity(10_001);

        for _ in 0..10_000 {
            children.push(explicit_probe(
                Rc::new(Cell::new(false)),
                static_frames.clone(),
                all_updates.clone(),
            ));
        }
        children.push(explicit_probe(
            active,
            listener_frames.clone(),
            all_updates.clone(),
        ));

        let mut renderer = ();
        let mut ui = build(
            Column::with_children(children),
            user_interface::Cache::new(),
            &mut renderer,
        );

        for _ in 0..1_000 {
            assert_eq!(
                frame(&mut ui, &mut renderer),
                window::RedrawRequest::NextFrame
            );
        }

        assert_eq!(listener_frames.get(), 1_000);
        assert_eq!(
            static_frames.get(),
            0,
            "10,000 static sibling branches must receive zero ticks per frame"
        );
        assert_eq!(all_updates.get(), 1_000);
        assert_eq!(ui.frame_route_counts(), (2, 1));
    }

    #[test]
    fn routing_count_matrix_is_independent_of_static_sibling_count() {
        const FRAMES: usize = 1_000;

        for static_siblings in [100, 1_000, 10_000] {
            for active_listeners in [1, 10, 100] {
                let static_frames = Rc::new(Cell::new(0));
                let listener_frames = Rc::new(Cell::new(0));
                let all_updates = Rc::new(Cell::new(0));
                let static_interest_queries = Rc::new(Cell::new(0));
                let listener_interest_queries = Rc::new(Cell::new(0));
                let mut children = Vec::with_capacity(static_siblings + active_listeners);

                children.extend((0..static_siblings).map(|_| {
                    explicit_probe_counted(
                        Rc::new(Cell::new(false)),
                        static_frames.clone(),
                        all_updates.clone(),
                        static_interest_queries.clone(),
                    )
                }));
                children.extend((0..active_listeners).map(|_| {
                    explicit_probe_counted(
                        Rc::new(Cell::new(true)),
                        listener_frames.clone(),
                        all_updates.clone(),
                        listener_interest_queries.clone(),
                    )
                }));

                let mut renderer = ();
                let mut ui = build(
                    Column::with_children(children),
                    user_interface::Cache::new(),
                    &mut renderer,
                );

                assert!(static_siblings >= 100, "benchmark floor must not shrink");
                assert_eq!(
                    ui.frame_route_counts(),
                    (active_listeners + 1, active_listeners),
                    "route counts must be derived from the reconciled tree"
                );
                let registration_queries =
                    static_interest_queries.get() + listener_interest_queries.get();
                assert!(
                    registration_queries >= static_siblings + active_listeners,
                    "reconciliation must inspect every newly registered widget"
                );

                for _ in 0..FRAMES {
                    assert_eq!(
                        frame(&mut ui, &mut renderer),
                        window::RedrawRequest::NextFrame
                    );
                }

                assert_eq!(static_frames.get(), 0);
                assert_eq!(listener_frames.get(), active_listeners * FRAMES);
                assert_eq!(all_updates.get(), active_listeners * FRAMES);
                assert_eq!(static_interest_queries.get(), static_siblings * 2);
                assert_eq!(
                    listener_interest_queries.get(),
                    active_listeners * (FRAMES + 2)
                );
            }
        }
    }

    #[test]
    fn shared_and_disjoint_listener_paths_receive_each_frame_once() {
        let shared_left_frames = Rc::new(Cell::new(0));
        let shared_right_frames = Rc::new(Cell::new(0));
        let disjoint_frames = Rc::new(Cell::new(0));
        let updates = Rc::new(Cell::new(0));
        let listener = |frames: Rc<Cell<usize>>| {
            explicit_probe(Rc::new(Cell::new(true)), frames, updates.clone())
        };
        let shared: Element<'static, (), (), ()> = Column::with_children([
            listener(shared_left_frames.clone()),
            listener(shared_right_frames.clone()),
        ])
        .into();
        let disjoint: Element<'static, (), (), ()> =
            Column::with_children([listener(disjoint_frames.clone())]).into();

        let mut renderer = ();
        let mut ui = build(
            Column::with_children([shared, disjoint]),
            user_interface::Cache::new(),
            &mut renderer,
        );

        assert_eq!(
            frame(&mut ui, &mut renderer),
            window::RedrawRequest::NextFrame
        );
        assert_eq!(
            shared_left_frames.get(),
            1,
            "listener path root/shared[0] missed its presented-frame tick"
        );
        assert_eq!(
            shared_right_frames.get(),
            1,
            "listener path root/shared[1] missed its presented-frame tick"
        );
        assert_eq!(
            disjoint_frames.get(),
            1,
            "listener path root/disjoint[0] missed its presented-frame tick"
        );
        assert_eq!(updates.get(), 3);
        assert_eq!(ui.frame_route_counts(), (6, 3));
    }

    #[test]
    fn moving_stopping_and_removing_listener_reconciles_cached_paths() {
        let active = Rc::new(Cell::new(true));
        let frames = Rc::new(Cell::new(0));
        let updates = Rc::new(Cell::new(0));
        let static_probe = || {
            explicit_probe(
                Rc::new(Cell::new(false)),
                Rc::new(Cell::new(0)),
                Rc::new(Cell::new(0)),
            )
        };
        let listener = || explicit_probe(active.clone(), frames.clone(), updates.clone());
        let mut renderer = ();

        let left: Element<'static, (), (), ()> = Column::with_children([listener()]).into();
        let right: Element<'static, (), (), ()> = Column::with_children([static_probe()]).into();
        let mut ui = build(
            Column::with_children([left, right]),
            user_interface::Cache::new(),
            &mut renderer,
        );
        assert_eq!(ui.frame_route_counts(), (3, 1));
        assert_eq!(
            frame(&mut ui, &mut renderer),
            window::RedrawRequest::NextFrame
        );
        assert_eq!(frames.get(), 1);

        let cache = ui.into_cache();
        let left: Element<'static, (), (), ()> = Column::with_children([static_probe()]).into();
        let right: Element<'static, (), (), ()> = Column::with_children([listener()]).into();
        let mut ui = build(Column::with_children([left, right]), cache, &mut renderer);
        assert_eq!(ui.frame_route_counts(), (3, 1));
        assert_eq!(
            frame(&mut ui, &mut renderer),
            window::RedrawRequest::NextFrame
        );
        assert_eq!(frames.get(), 2);

        active.set(false);
        let cache = ui.into_cache();
        let left: Element<'static, (), (), ()> = Column::with_children([static_probe()]).into();
        let right: Element<'static, (), (), ()> = Column::with_children([listener()]).into();
        let mut ui = build(Column::with_children([left, right]), cache, &mut renderer);
        assert_eq!(ui.frame_route_counts(), (0, 0));
        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert_eq!(frames.get(), 2);

        let cache = ui.into_cache();
        let mut ui = build(
            Column::with_children([static_probe()]),
            cache,
            &mut renderer,
        );
        assert_eq!(ui.frame_route_counts(), (0, 0));
        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert_eq!(frames.get(), 2);
    }

    #[test]
    fn listener_started_by_a_non_frame_event_joins_the_next_frame() {
        let active = Rc::new(Cell::new(false));
        let frames = Rc::new(Cell::new(0));
        let root: Column<'static, (), (), ()> = Column::with_children([Element::new(WakeProbe {
            active: active.clone(),
            frames: frames.clone(),
        })]);
        let mut renderer = ();
        let mut ui = build(root, user_interface::Cache::new(), &mut renderer);

        assert_eq!(ui.frame_route_counts(), (0, 0));
        assert_eq!(
            dispatch(
                &mut ui,
                &mut renderer,
                Event::Window(window::Event::Focused),
            ),
            window::RedrawRequest::NextFrame
        );
        assert!(active.get());
        assert_eq!(ui.frame_route_counts(), (2, 1));

        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert_eq!(frames.get(), 1);
    }

    #[test]
    fn listener_that_stops_during_a_frame_is_removed_without_a_rebuild() {
        let active = Rc::new(Cell::new(true));
        let frames = Rc::new(Cell::new(0));
        let root: Column<'static, (), (), ()> =
            Column::with_children([Element::new(OneShotProbe {
                active: active.clone(),
                frames: frames.clone(),
            })]);
        let mut renderer = ();
        let mut ui = build(root, user_interface::Cache::new(), &mut renderer);

        assert_eq!(ui.frame_route_counts(), (2, 1));
        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert!(!active.get());
        assert_eq!(frames.get(), 1);

        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert_eq!(
            frames.get(),
            1,
            "stopped listener path root/one-shot must not receive another tick"
        );
        assert_eq!(ui.frame_route_counts(), (0, 0));
    }

    #[test]
    fn non_frame_window_events_still_reach_every_sibling() {
        let updates = Rc::new(Cell::new(0));
        let mut children = Vec::with_capacity(101);

        children.extend((0..100).map(|_| {
            explicit_probe(
                Rc::new(Cell::new(false)),
                Rc::new(Cell::new(0)),
                updates.clone(),
            )
        }));
        children.push(explicit_probe(
            Rc::new(Cell::new(true)),
            Rc::new(Cell::new(0)),
            updates.clone(),
        ));

        let mut renderer = ();
        let mut ui = build(
            Column::with_children(children),
            user_interface::Cache::new(),
            &mut renderer,
        );

        assert_eq!(
            dispatch(
                &mut ui,
                &mut renderer,
                Event::Window(window::Event::Focused),
            ),
            window::RedrawRequest::Wait
        );
        assert_eq!(updates.get(), 101);
    }

    #[test]
    fn custom_widget_without_interest_contract_uses_conservative_fallback() {
        let frames = Rc::new(Cell::new(0));
        let root: Column<'static, (), (), ()> =
            Column::with_children([Element::new(FallbackProbe(frames.clone()))]);
        let mut renderer = ();
        let mut ui = build(root, user_interface::Cache::new(), &mut renderer);

        assert_eq!(ui.frame_route_counts(), (2, 1));
        assert_eq!(frame(&mut ui, &mut renderer), window::RedrawRequest::Wait);
        assert_eq!(frames.get(), 1);
    }
}
