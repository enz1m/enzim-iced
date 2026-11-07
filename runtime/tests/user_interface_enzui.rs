// Modified by enzUI contributors. See FORK.md.
//! enzICED deviations of the [`UserInterface`] from upstream, each pinned
//! against the upstream behaviour it replaces.
use iced_runtime::core;
use iced_runtime::core::layout;
use iced_runtime::core::layout::Measure as _;
use iced_runtime::core::overlay;
use iced_runtime::core::renderer;
use iced_runtime::core::shell;
use iced_runtime::core::widget;
use iced_runtime::core::window;
use iced_runtime::core::{Element, Event, Length, Rectangle, Shell, Size};
use iced_runtime::user_interface::{Cache, UserInterface};

use std::cell::Cell;
use std::rc::Rc;

type Renderer = ();

const SIZE: Size = Size::new(100.0, 100.0);

#[derive(Default)]
struct Counts {
    overlay_draws: Cell<u32>,
    updates: Cell<u32>,
    frames: Cell<u32>,
    child_frames: Cell<u32>,
}

/// A leaf that owns an overlay and counts how its events arrive.
struct Root {
    counts: Rc<Counts>,
    invalidate_overlay: bool,
}

impl widget::Widget<(), core::Theme, Renderer> for Root {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, tree: &mut widget::Tree, _renderer: &Renderer, limits: &layout::Limits) {
        layout::Node::new(limits.max()).apply(tree);
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn frame_interest(&self, _tree: &widget::Tree) -> bool {
        true
    }

    fn update(
        &mut self,
        _tree: &mut widget::Tree,
        _event: &Event,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _renderer: &Renderer,
        _shell: &mut Shell<'_, ()>,
        _viewport: &Rectangle,
    ) {
        self.counts.updates.set(self.counts.updates.get() + 1);
    }

    fn update_frame(
        &mut self,
        _tree: &mut widget::Tree,
        _event: &Event,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, ()>,
        _viewport: &Rectangle,
    ) {
        self.counts.frames.set(self.counts.frames.get() + 1);

        if self.invalidate_overlay {
            shell.invalidate_overlay();
        }
    }

    fn overlay<'a>(
        &'a mut self,
        _tree: &'a mut widget::Tree,
        _layout: layout::Layout,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        _translation: core::Vector,
        _window: Size,
    ) -> Vec<overlay::Element<'a, (), core::Theme, Renderer>> {
        vec![overlay::Element::new(Box::new(CountingOverlay(
            self.counts.clone(),
        )))]
    }
}

struct CountingOverlay(Rc<Counts>);

impl core::Overlay<(), core::Theme, Renderer> for CountingOverlay {
    fn draw(
        &self,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _cursor: core::mouse::Cursor,
    ) {
        self.0.overlay_draws.set(self.0.overlay_draws.get() + 1);
    }
}

fn frame() -> Event {
    Event::Window(window::Event::RedrawRequested(core::time::Instant::now()))
}

fn update(ui: &mut UserInterface<'_, (), core::Theme, Renderer>, event: Event) {
    let _ = ui.update(
        &window::Headless,
        &shell::Waker::noop(),
        &[event],
        core::mouse::Cursor::Unavailable,
        &mut (),
        &mut shell::Bus::new(),
    );
}

fn draw(ui: &mut UserInterface<'_, (), core::Theme, Renderer>) {
    ui.draw(
        &mut (),
        &core::Theme::Light,
        &renderer::Style::default(),
        core::mouse::Cursor::Unavailable,
    );
}

/// An operation establishes the overlays it reached, so a `draw` that
/// follows it without an intervening `update` paints them. Upstream skips
/// overlays until an `update` has run.
///
/// Sabotage: drop the `self.overlay = Some(..)` after `overlay.operate` and
/// the draw below paints no overlay.
#[test]
fn an_operation_establishes_the_overlays_a_following_draw_paints() {
    let counts = Rc::new(Counts::default());
    let root = Root {
        counts: counts.clone(),
        invalidate_overlay: false,
    };
    let mut ui = UserInterface::build(Element::new(root), SIZE, Cache::default(), &mut ());

    struct Noop;
    impl widget::Operation for Noop {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn widget::Operation)) {
            operate(self);
        }
    }
    ui.operate(&(), &mut Noop);
    draw(&mut ui);

    assert_eq!(counts.overlay_draws.get(), 1);
}

/// Recreating the overlays leaves the base tree's frame routes intact, so
/// it must not cost the next frame an unpruned `update` walk of the whole
/// tree. Upstream treats an overlay invalidation like a relayout.
///
/// Sabotage: set `geometry_route` for `Invalidation::Overlay` again and the
/// third frame below arrives through `update`.
#[test]
fn an_overlay_invalidation_keeps_the_frame_routes() {
    let counts = Rc::new(Counts::default());
    let root = Root {
        counts: counts.clone(),
        invalidate_overlay: true,
    };
    let mut ui = UserInterface::build(Element::new(root), SIZE, Cache::default(), &mut ());

    // Every frame invalidates the overlays; every following frame must
    // still be routed.
    update(&mut ui, frame());
    update(&mut ui, frame());
    update(&mut ui, frame());
    assert_eq!(
        (counts.updates.get(), counts.frames.get()),
        (0, 3),
        "frames after an overlay invalidation still take `update_frame`"
    );
}

/// A child that only learns in `layout` that it needs the next frame.
struct Child {
    counts: Rc<Counts>,
    height: f32,
}

impl widget::Widget<(), core::Theme, Renderer> for Child {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(self.height))
    }

    fn layout(&mut self, tree: &mut widget::Tree, _renderer: &Renderer, limits: &layout::Limits) {
        if self.height > 10.0 {
            tree.raise_frame_interest();
        }

        layout::Node::new(Size::new(limits.max().width, self.height)).apply(tree);
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn frame_interest(&self, _tree: &widget::Tree) -> bool {
        false
    }

    fn update_frame(
        &mut self,
        _tree: &mut widget::Tree,
        _event: &Event,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _renderer: &Renderer,
        _shell: &mut Shell<'_, ()>,
        _viewport: &Rectangle,
    ) {
        self.counts
            .child_frames
            .set(self.counts.child_frames.get() + 1);
    }
}

/// Routes frames only to its interested children.
struct Parent {
    child: Element<'static, (), core::Theme, Renderer>,
}

impl widget::Widget<(), core::Theme, Renderer> for Parent {
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.child));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, tree: &mut widget::Tree, renderer: &Renderer, limits: &layout::Limits) {
        let child =
            self.child
                .as_widget_mut()
                .measure_snapshot(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(limits.max(), vec![child]).apply(tree);
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn frame_interest(&self, tree: &widget::Tree) -> bool {
        !tree.frame_children.is_empty()
    }

    fn update_frame(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: layout::Layout,
        cursor: core::mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, ()>,
        viewport: &Rectangle,
    ) {
        if tree.frame_children.contains(&0) {
            let child_layout = layout.child(&tree.children, 0);
            self.child.as_widget_mut().update_frame(
                &mut tree.children[0],
                event,
                child_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
        }
    }
}

fn parent(counts: &Rc<Counts>, height: f32) -> Element<'static, (), core::Theme, Renderer> {
    Element::new(Parent {
        child: Element::new(Child {
            counts: counts.clone(),
            height,
        }),
    })
}

/// A rebuild whose layout raised a descendant's frame interest reaches that
/// descendant on the next frame, through the routes alone. Upstream has no
/// frame routes; the fork used to open the whole frame unpruned instead.
///
/// Sabotage: skip `propagate_frame_interest` in `UserInterface::build` and
/// the child never sees the frame.
#[test]
fn interest_raised_in_layout_reaches_the_routes_after_a_rebuild() {
    let counts = Rc::new(Counts::default());
    let mut ui = UserInterface::build(parent(&counts, 10.0), SIZE, Cache::default(), &mut ());
    // Spend the fresh interface's unpruned frame.
    update(&mut ui, frame());

    let cache = ui.into_cache();
    let mut ui = UserInterface::build(parent(&counts, 20.0), SIZE, cache, &mut ());
    update(&mut ui, frame());

    assert_eq!(counts.child_frames.get(), 1);
}

/// A leaf whose overlay reports its laid-out node, past the window's edge.
struct PlacedRoot {
    counts: Rc<Counts>,
}

impl widget::Widget<(), core::Theme, Renderer> for PlacedRoot {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, tree: &mut widget::Tree, _renderer: &Renderer, limits: &layout::Limits) {
        layout::Node::new(limits.max()).apply(tree);
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _layout: layout::Layout,
        _cursor: core::mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn overlay<'a>(
        &'a mut self,
        _tree: &'a mut widget::Tree,
        _layout: layout::Layout,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        _translation: core::Vector,
        _window: Size,
    ) -> Vec<overlay::Element<'a, (), core::Theme, Renderer>> {
        vec![overlay::Element::new(Box::new(PlacedOverlay {
            counts: self.counts.clone(),
            node: layout::Node::new(Size::new(60.0, 70.0)).move_to(core::Point::new(80.0, 70.0)),
        }))]
    }
}

struct PlacedOverlay {
    counts: Rc<Counts>,
    node: layout::Node,
}

impl core::Overlay<(), core::Theme, Renderer> for PlacedOverlay {
    fn draw(
        &self,
        _renderer: &mut Renderer,
        _theme: &core::Theme,
        _style: &renderer::Style,
        _cursor: core::mouse::Cursor,
    ) {
        self.counts
            .overlay_draws
            .set(self.counts.overlay_draws.get() + 1);
    }

    fn node(&self) -> Option<&layout::Node> {
        Some(&self.node)
    }
}

/// The runtime reads where the open overlays are, and can paint them into a
/// surface of their own instead of the window.
///
/// Sabotage: ignore `overlay_detached` in `draw_inner` and the detached
/// window draw still paints the overlay; skip the node in `Group::bounds` and
/// no bounds are found.
#[test]
fn detached_overlays_paint_only_through_draw_overlay() {
    let counts = Rc::new(Counts::default());
    let root = PlacedRoot {
        counts: counts.clone(),
    };
    let mut ui = UserInterface::build(Element::new(root), SIZE, Cache::default(), &mut ());
    update(&mut ui, frame());

    assert_eq!(
        ui.overlay_bounds(&()),
        Some(Rectangle::new(
            core::Point::new(80.0, 70.0),
            Size::new(60.0, 70.0)
        ))
    );

    draw(&mut ui);
    assert_eq!(
        counts.overlay_draws.get(),
        1,
        "attached: the window paints it"
    );

    ui.set_overlay_detached(true);
    draw(&mut ui);
    assert_eq!(
        counts.overlay_draws.get(),
        1,
        "detached: the window does not"
    );

    ui.draw_overlay(
        &mut (),
        &core::Theme::Light,
        &renderer::Style::default(),
        core::mouse::Cursor::Unavailable,
        Rectangle::new(core::Point::new(56.0, 46.0), Size::new(108.0, 118.0)),
    );
    assert_eq!(counts.overlay_draws.get(), 2, "the popup paints it");
}

/// A change in how the desktop decorates the window rebuilds the view, which
/// reads it.
///
/// Sabotage: drop the `DecorationsChanged` check in `update` and the state
/// stays `Updated`.
#[test]
fn a_decoration_change_rebuilds_the_view() {
    let counts = Rc::new(Counts::default());
    let root = PlacedRoot { counts };
    let mut ui = UserInterface::build(Element::new(root), SIZE, Cache::default(), &mut ());
    let (state, _) = ui.update(
        &window::Headless,
        &shell::Waker::noop(),
        &[Event::Window(window::Event::DecorationsChanged(
            window::Decorations::default(),
        ))],
        core::mouse::Cursor::Unavailable,
        &mut (),
        &mut shell::Bus::new(),
    );
    assert!(matches!(
        state,
        iced_runtime::user_interface::State::Outdated
    ));
}
