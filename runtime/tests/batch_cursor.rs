// Modified by enzUI contributors. See FORK.md.
//! A batched event is dispatched with the cursor it arrived with (enzICED).
//!
//! Upstream hands every event of a batch the window's cursor after the last
//! event, so a click batched with later motion or a pointer leave is
//! hit-tested somewhere it never happened.
use iced_runtime::core;
use iced_runtime::core::layout;
use iced_runtime::core::mouse::{self, Cursor};
use iced_runtime::core::renderer;
use iced_runtime::core::shell;
use iced_runtime::core::widget;
use iced_runtime::core::window;
use iced_runtime::core::{Element, Event, Length, Point, Rectangle, Shell, Size};
use iced_runtime::user_interface::{Cache, UserInterface};

use std::cell::Cell;
use std::rc::Rc;

/// Records the cursor it is handed with a left press.
struct PressProbe(Rc<Cell<Option<Cursor>>>);

impl widget::Widget<(), core::Theme, ()> for PressProbe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, tree: &mut widget::Tree, _renderer: &(), limits: &layout::Limits) {
        layout::Node::new(limits.max()).apply(tree);
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        _renderer: &mut (),
        _theme: &core::Theme,
        _style: &renderer::Style,
        _layout: layout::Layout,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn update(
        &mut self,
        _tree: &mut widget::Tree,
        event: &Event,
        _layout: layout::Layout,
        cursor: Cursor,
        _renderer: &(),
        _shell: &mut Shell<'_, ()>,
        _viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
            self.0.set(Some(cursor));
        }
    }
}

fn press_cursor(origin: Option<Cursor>, events: &[Event], last: Cursor) -> Option<Cursor> {
    let seen = Rc::new(Cell::new(None));
    let mut ui = UserInterface::build(
        Element::new(PressProbe(seen.clone())),
        Size::new(100.0, 100.0),
        Cache::new(),
        &mut (),
    );
    if let Some(origin) = origin {
        ui.set_batch_cursor_origin(origin);
    }
    let _ = ui.update(
        &window::Headless,
        &shell::Waker::noop(),
        events,
        last,
        &mut (),
        &mut shell::Bus::new(),
    );
    seen.get()
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

const AT: Cursor = Cursor::Available(Point::new(10.0, 10.0));

#[test]
fn a_press_batched_with_a_pointer_leave_sees_where_it_was_pressed() {
    let events = [press(), release(), Event::Mouse(mouse::Event::CursorLeft)];

    assert_eq!(
        press_cursor(Some(AT), &events, Cursor::Unavailable),
        Some(AT)
    );
    // Control: without an origin the press sees the cursor after the batch.
    assert_eq!(
        press_cursor(None, &events, Cursor::Unavailable),
        Some(Cursor::Unavailable)
    );
}

#[test]
fn a_press_batched_before_motion_sees_the_point_before_the_motion() {
    let to = Point::new(80.0, 80.0);
    let events = [
        press(),
        release(),
        Event::Mouse(mouse::Event::CursorMoved { position: to }),
    ];

    assert_eq!(
        press_cursor(Some(AT), &events, Cursor::Available(to)),
        Some(AT)
    );
}

/// An origin whose replay disagrees with the window's cursor is not trusted.
#[test]
fn an_origin_that_does_not_replay_to_the_last_cursor_is_ignored() {
    let last = Cursor::Available(Point::new(50.0, 50.0));

    assert_eq!(press_cursor(Some(AT), &[press()], last), Some(last));
}
