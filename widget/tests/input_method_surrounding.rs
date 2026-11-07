// Modified by enzUI contributors. See FORK.md.
//! A focused text field tells the input method the text around its cursor
//! (enzUI-ICED #3680); a secure field tells it nothing.

use std::time::Instant;

use iced_runtime::UserInterface;
use iced_runtime::core::event::Event;
use iced_runtime::core::input_method::Surrounding;
use iced_runtime::core::shell::Waker;
use iced_runtime::core::window::{self, Headless};
use iced_runtime::core::{Element, Font, Point, Size, Theme, mouse};
use iced_runtime::user_interface::{Cache, State as UiState};

use iced_widget::{text_editor, text_input};

type Renderer = iced_tiny_skia::Renderer;
type Msg = String;

const SIZE: Size = Size::new(600.0, 60.0);
// Past the end of the value, so the click leaves the cursor at its end.
const CLICK: Point = Point::new(560.0, 20.0);

fn surrounding(value: &str, secure: bool) -> Option<Surrounding> {
    told(
        text_input::TextInput::new("", value)
            .font(Font::MONOSPACE)
            .secure(secure)
            .on_input(Msg::from)
            .into(),
    )
}

/// What a focused, redrawn `view` tells the input method.
fn told(view: Element<'_, Msg, Theme, Renderer>) -> Option<Surrounding> {
    let mut renderer = Renderer::new(iced_runtime::core::renderer::Settings::default());
    let cursor = mouse::Cursor::Available(CLICK);
    let mut ui = UserInterface::build(view, SIZE, Cache::default(), &mut renderer);
    ui.draw(
        &mut renderer,
        &Theme::Dark,
        &iced_runtime::core::renderer::Style::default(),
        cursor,
    );
    let mut messages = iced_runtime::core::shell::Bus::<Msg>::new();
    let _ = ui.update(
        &Headless,
        &Waker::noop(),
        &[
            Event::Window(window::Event::Focused),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        cursor,
        &mut renderer,
        &mut messages,
    );
    let (state, _) = ui.update(
        &Headless,
        &Waker::noop(),
        &[Event::Window(
            window::Event::RedrawRequested(Instant::now()),
        )],
        cursor,
        &mut renderer,
        &mut messages,
    );
    match state {
        UiState::Updated {
            input_method,
            input_method_surrounding,
            ..
        } => {
            assert!(
                input_method.is_enabled(),
                "a focused field enables the input method"
            );
            input_method_surrounding
        }
        UiState::Outdated => panic!("the user interface was outdated after one redraw"),
    }
}

// Sabotage: return `None` from `TextInput::surrounding`, or drop the
// `is_secure` check there.
#[test]
fn a_focused_field_tells_the_input_method_its_text_but_a_secure_one_does_not() {
    let told = surrounding("so my fellow Américans.", false).expect("text around the cursor");
    assert_eq!(told.text, "so my fellow Américans.");
    assert_eq!(
        told.cursor,
        told.text.len(),
        "the click left the cursor at the end"
    );
    assert_eq!(told.anchor, told.cursor);
    assert_eq!(surrounding("hunter2", true), None);
}

// Sabotage: return `None` from `TextEditor::surrounding`.
#[test]
fn a_focused_editor_tells_the_input_method_the_lines_around_its_cursor() {
    let mut content = text_editor::Content::<Renderer>::with_text("first line.\nsecond");
    let _ = content.perform(text_editor::Action::Move(text_editor::Motion::End));
    let told = told(
        text_editor::TextEditor::new(&content)
            .font(Font::MONOSPACE)
            .on_action(|_| String::new())
            .into(),
    )
    .expect("text around the cursor");
    assert_eq!(told.text, "first line.\nsecond");
    assert_eq!(
        told.cursor,
        "first line.".len(),
        "the cursor at the first line's end"
    );
    assert_eq!(told.anchor, told.cursor);
}
