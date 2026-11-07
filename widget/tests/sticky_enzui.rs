// Modified by enzUI contributors. See FORK.md.
//! enzICED deviations of `sticky`, pinned.

use iced_runtime::UserInterface;
use iced_runtime::core::shell::{Bus, Waker};
use iced_runtime::core::widget::operation::Animation;
use iced_runtime::core::widget::operation::scrollable::{AbsoluteOffset, scroll_to};
use iced_runtime::core::widget::{Id, Operation};
use iced_runtime::core::{Element, Event, Length, Rectangle, Size, Theme};
use iced_runtime::core::{mouse, window};
use iced_runtime::user_interface::Cache;
use iced_widget::{column, scrollable, space, sticky, text};

use std::time::Instant;

type Renderer = iced_tiny_skia::Renderer;

const ID: Id = Id::new("scrollable");

/// A 200 px viewport over a 1000 px section headed by a sticky "Header",
/// with a "Row" inside the section and an "After" past it.
fn view() -> Element<'static, (), Theme, Renderer> {
    scrollable(column![
        column![
            sticky(text("Header").height(20)),
            space().height(500),
            text("Row").height(20),
            space().height(480),
        ],
        text("After").height(20),
        space().height(3000),
    ])
    .id(ID)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Every text an operation pass visits, in visiting order, with its bounds.
fn texts(
    ui: &mut UserInterface<'_, (), Theme, Renderer>,
    renderer: &Renderer,
) -> Vec<(String, Rectangle)> {
    struct Texts(Vec<(String, Rectangle)>);

    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, text: &str) {
            self.0.push((text.to_owned(), bounds));
        }
    }

    let mut texts = Texts(Vec::new());
    ui.operate(renderer, &mut texts);
    texts.0
}

fn redraw(ui: &mut UserInterface<'_, (), Theme, Renderer>, renderer: &mut Renderer) {
    let _ = ui.update(
        &window::Headless,
        &Waker::noop(),
        &[Event::Window(
            window::Event::RedrawRequested(Instant::now()),
        )],
        mouse::Cursor::Unavailable,
        renderer,
        &mut Bus::<()>::new(),
    );
}

/// Upstream hands stuck contents to an overlay, so an operation pass visits
/// them after every other widget of the window: a screen reader read the
/// section's heading after its rows, outside the section. Here they are
/// operated in place, at the position they are stuck at.
///
/// Sabotage: return early from `Sticky::operate` while stuck (and restore
/// `Overlay::operate`) and the header is visited last.
#[test]
fn stuck_contents_are_operated_in_place_at_their_stuck_position() {
    let mut renderer = Renderer::new(iced_runtime::core::renderer::Settings::default());
    let mut ui = UserInterface::build(
        view(),
        Size::new(200.0, 200.0),
        Cache::default(),
        &mut renderer,
    );
    redraw(&mut ui, &mut renderer);

    let unstuck = texts(&mut ui, &renderer);
    assert_eq!(
        unstuck
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>(),
        ["Header", "Row", "After"],
    );
    assert_eq!(
        unstuck[0].1.y, 0.0,
        "an unstuck header sits where it was laid out"
    );

    ui.operate(
        &renderer,
        &mut scroll_to::<()>(
            ID,
            AbsoluteOffset {
                x: None,
                y: Some(300.0),
            },
            Animation::Instant,
        ),
    );
    redraw(&mut ui, &mut renderer);
    let cache = ui.into_cache();
    let mut ui = UserInterface::build(view(), Size::new(200.0, 200.0), cache, &mut renderer);
    redraw(&mut ui, &mut renderer);

    let stuck = texts(&mut ui, &renderer);
    assert_eq!(
        stuck
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>(),
        ["Header", "Row", "After"],
        "a stuck header must be visited once, before the rows of its section"
    );
    assert_eq!(
        stuck[0].1.y, 300.0,
        "a stuck header is operated at the viewport's top edge, in content space"
    );
}
