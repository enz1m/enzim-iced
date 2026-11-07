// Modified by enzUI contributors. See FORK.md.
//! enzICED deviations of the `scrollable` widget from upstream, each pinned
//! against the upstream behaviour it replaces.
use iced_runtime::UserInterface;
use iced_runtime::core::layout::Measure as _;
use iced_runtime::core::shell::{Bus, Waker};
use iced_runtime::core::widget::operation::scrollable::{AbsoluteOffset, scroll_to};
use iced_runtime::core::widget::operation::{Animation, Scrollable as ScrollableState};
use iced_runtime::core::widget::{Id, Operation};
use iced_runtime::core::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector};
use iced_runtime::core::{mouse, window};
use iced_runtime::user_interface::{Cache, State};
use iced_widget::scrollable::{Direction, Scrollbar};
use iced_widget::{column, scrollable, space};

use std::time::Instant;

type Renderer = iced_tiny_skia::Renderer;

const SIZE: Size = Size::new(200.0, 200.0);
const ID: Id = Id::new("scrollable");

/// A 200 px viewport over 3000 px of content, with a thin thumb painted in
/// the middle of a wide scrollbar band.
fn view() -> Element<'static, (), Theme, Renderer> {
    scrollable(space().height(3000))
        .id(ID)
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(Direction::Vertical(
            Scrollbar::default().width(15).scroller_width(5),
        ))
        .into()
}

struct Host {
    ui: UserInterface<'static, (), Theme, Renderer>,
    renderer: Renderer,
}

impl Host {
    fn new(view: Element<'static, (), Theme, Renderer>) -> Self {
        let mut renderer = Renderer::new(iced_runtime::core::renderer::Settings::default());
        let ui = UserInterface::build(view, SIZE, Cache::default(), &mut renderer);
        let mut host = Self { ui, renderer };
        let _ = host.event(redraw(), mouse::Cursor::Unavailable);
        host
    }

    fn event(&mut self, event: Event, cursor: mouse::Cursor) -> window::RedrawRequest {
        let (state, _) = self.ui.update(
            &window::Headless,
            &Waker::noop(),
            &[event],
            cursor,
            &mut self.renderer,
            &mut Bus::<()>::new(),
        );

        match state {
            State::Updated { redraw_request, .. } => redraw_request,
            State::Outdated => window::RedrawRequest::NextFrame,
        }
    }

    fn scroll_to(&mut self, y: f32) {
        self.ui.operate(
            &self.renderer,
            &mut scroll_to::<()>(
                ID,
                AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
                Animation::Instant,
            ),
        );
    }

    /// The translation an operation observes.
    fn translation(&mut self) -> Vector {
        struct Probe(Option<Vector>);

        impl Operation for Probe {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }

            fn scrollable(
                &mut self,
                _id: Option<&Id>,
                _bounds: Rectangle,
                _content: Size,
                translation: Vector,
                _state: &mut dyn ScrollableState,
            ) {
                self.0 = Some(translation);
            }
        }

        let mut probe = Probe(None);
        self.ui.operate(&self.renderer, &mut probe);
        probe.0.expect("the scrollable is operated")
    }
}

fn redraw() -> Event {
    Event::Window(window::Event::RedrawRequested(Instant::now()))
}

/// Upstream reports the last presented translation to operations too, so a
/// `scroll_to` followed by a reveal or an accessibility pass in the same
/// frame read the offset from before the scroll.
///
/// Sabotage: report `state.last_translation` in `operate` and this reads 0.
#[test]
fn an_operation_observes_the_live_translation() {
    let mut host = Host::new(view());
    host.scroll_to(300.0);

    assert_eq!(host.translation(), Vector::new(0.0, 300.0));
}

/// Scrolls its child by `scroll_to` from inside its own `update`, after the
/// child handled the event, the way a reveal does.
struct ScrollOnKey {
    child: Element<'static, (), Theme, Renderer>,
}

impl iced_runtime::core::Widget<(), Theme, Renderer> for ScrollOnKey {
    fn diff(&mut self, tree: &mut iced_runtime::core::widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.child));
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut iced_runtime::core::widget::Tree,
        renderer: &Renderer,
        limits: &iced_runtime::core::layout::Limits,
    ) {
        let child =
            self.child
                .as_widget_mut()
                .measure_snapshot(&mut tree.children[0], renderer, limits);
        iced_runtime::core::layout::Node::with_children(child.size(), vec![child]).apply(tree);
    }

    fn draw(
        &self,
        tree: &iced_runtime::core::widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced_runtime::core::renderer::Style,
        layout: iced_runtime::core::Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.child.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children(&tree.children).next().unwrap(),
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut iced_runtime::core::widget::Tree,
        layout: iced_runtime::core::Layout,
        viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let child_layout = layout.children(&tree.children).next().unwrap();
        self.child.as_widget_mut().operate(
            &mut tree.children[0],
            child_layout,
            viewport,
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut iced_runtime::core::widget::Tree,
        event: &Event,
        layout: iced_runtime::core::Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut iced_runtime::core::Shell<'_, ()>,
        viewport: &Rectangle,
    ) {
        let child = layout.children(&tree.children).next().unwrap();
        self.child.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            shell,
            viewport,
        );

        if let Event::Keyboard(iced_runtime::core::keyboard::Event::KeyPressed { .. }) = event {
            let mut op = scroll_to::<()>(
                ID,
                AbsoluteOffset {
                    x: None,
                    y: Some(300.0),
                },
                Animation::Instant,
            );
            self.child.as_widget_mut().operate(
                &mut tree.children[0],
                child,
                &child.bounds(),
                renderer,
                &mut op,
            );
        }
    }
}

fn key() -> Event {
    use iced_runtime::core::keyboard::{self, key};

    Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(key::Named::End),
        modified_key: keyboard::Key::Named(key::Named::End),
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::default(),
        text: None,
        repeat: false,
    })
}

/// An offset a widget moves during its own `update`, after the scrollable
/// has handled the event, is only presented by the next `RedrawRequested`.
/// The scrollable must ask for that frame on the next event it sees;
/// upstream waits for an unrelated one.
///
/// Sabotage: drop the final `translation != last_translation` request and
/// the cursor move below returns `Wait`.
#[test]
fn an_unpresented_offset_requests_the_frame_that_presents_it() {
    let mut host = Host::new(Element::new(ScrollOnKey { child: view() }));
    let cursor = mouse::Cursor::Available(Point::new(50.0, 50.0));
    let _ = host.event(
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 50.0),
        }),
        cursor,
    );
    let _ = host.event(redraw(), cursor);

    let _ = host.event(key(), cursor);
    let request = host.event(
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 51.0),
        }),
        cursor,
    );

    assert_eq!(request, window::RedrawRequest::NextFrame);

    let _ = host.event(redraw(), cursor);
    let settled = host.event(
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 52.0),
        }),
        cursor,
    );
    assert_eq!(
        settled,
        window::RedrawRequest::Wait,
        "once presented, the offset owes no further frame"
    );
}

/// A thin thumb in a wide band is grabbed by a press level with it anywhere
/// across the band. Upstream hit-tests the painted thumb only, so the same
/// press is a rail press that pages the content.
///
/// Sabotage: hit-test `scroller.bounds.contains(cursor)` again and the press
/// below pages the offset away from 0.
#[test]
fn a_press_level_with_the_thumb_grabs_it_across_the_band() {
    let mut host = Host::new(view());
    // The band spans the rightmost 15 px; the 5 px thumb is centred in it and
    // starts at the top. x = 198 is inside the band, outside the thumb.
    let press = mouse::Cursor::Available(Point::new(198.0, 4.0));
    let _ = host.event(
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(198.0, 4.0),
        }),
        press,
    );
    let _ = host.event(
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        press,
    );
    let _ = host.event(redraw(), press);

    assert_eq!(
        host.translation(),
        Vector::ZERO,
        "grabbing the thumb where it rests must not page the content"
    );
}

/// A `Fill` child along a scroll axis hugs its intrinsic extent, as it did
/// before the upstream merge; upstream grows it to at least the viewport.
///
/// Sabotage: stop forcing compression on the scroll axes in
/// `layout::scroll::resolve` and the column below fills the 200 px viewport.
#[test]
fn a_fill_child_hugs_its_content_along_the_scroll_axis() {
    let content = column![space().height(40)].height(Length::Fill);
    let mut host = Host::new(
        scrollable(content)
            .id(ID)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    );

    struct Probe(Option<Size>);

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            _bounds: Rectangle,
            content: Size,
            _translation: Vector,
            _state: &mut dyn ScrollableState,
        ) {
            self.0 = Some(content);
        }
    }

    let mut probe = Probe(None);
    host.ui.operate(&host.renderer, &mut probe);

    assert_eq!(probe.0.map(|size| size.height), Some(40.0));
}
