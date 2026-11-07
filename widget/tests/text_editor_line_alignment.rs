// Modified by enzUI contributors. See FORK.md.
//! Per-line alignment across an element rebuild.
//!
//! The editor lives in the caller's `Content` and in the widget tree cache,
//! while the element that states the alignment is rebuilt every frame, so an
//! alignment installed by one element is observed through the next one.

use iced_runtime::UserInterface;
use iced_runtime::core::event::Event;
use iced_runtime::core::renderer::Headless as _;
use iced_runtime::core::shell::Waker;
use iced_runtime::core::text::Alignment;
use iced_runtime::core::window::{self, Headless};
use iced_runtime::core::{Color, Element, Font, Point, Size, Theme, mouse};
use iced_runtime::user_interface::Cache;

use iced_tiny_skia::graphics::text::editor::instrument;
use iced_widget::text_editor;

type Renderer = iced_tiny_skia::Renderer;

#[derive(Debug, Clone)]
enum Msg {
    Edit(text_editor::Action),
}

const SIZE: Size = Size::new(400.0, 120.0);
const TEXT: &str = "align me";

// Enough lines that a per-layout scan is unmistakable in the counter.
const LINES: usize = 64;

// Well past the end of a left-aligned run of this length in a 400px column,
// and inside a centred one.
const PROBE: Point = Point::new(200.0, 12.0);

fn content() -> text_editor::Content<Renderer> {
    text_editor::Content::<Renderer>::with_text(TEXT)
}

fn many_lines() -> text_editor::Content<Renderer> {
    let text: Vec<&str> = (0..LINES).map(|_| TEXT).collect();

    text_editor::Content::<Renderer>::with_text(&text.join("\n"))
}

// Lines the editor walked to install or release an alignment during `layout`.
fn lines_visited_by(
    content: &text_editor::Content<Renderer>,
    alignment: Option<Alignment>,
    cache: Cache,
) -> (u64, Cache) {
    let before = instrument::counters();
    let mut renderer = new_renderer();
    let ui = UserInterface::build(view(content, alignment), SIZE, cache, &mut renderer);

    (
        instrument::counters().since(before).alignment_lines_visited,
        ui.into_cache(),
    )
}

fn view(
    content: &text_editor::Content<Renderer>,
    alignment: Option<Alignment>,
) -> Element<'_, Msg, Theme, Renderer> {
    let editor = text_editor::TextEditor::new(content)
        .font(Font::MONOSPACE)
        .on_action(Msg::Edit);

    match alignment {
        Some(alignment) => editor.line_alignment(move |_index| alignment).into(),
        None => editor.into(),
    }
}

fn new_renderer() -> Renderer {
    Renderer::new(iced_runtime::core::renderer::Settings::default())
}

fn draw(
    ui: &mut UserInterface<'_, Msg, Theme, Renderer>,
    renderer: &mut Renderer,
    cursor: mouse::Cursor,
) {
    ui.draw(
        renderer,
        &Theme::Dark,
        &iced_runtime::core::renderer::Style::default(),
        cursor,
    );
}

fn frame(
    content: &text_editor::Content<Renderer>,
    alignment: Option<Alignment>,
    cache: Cache,
) -> (Vec<u8>, Cache) {
    let mut renderer = new_renderer();
    let mut ui = UserInterface::build(view(content, alignment), SIZE, cache, &mut renderer);
    draw(&mut ui, &mut renderer, mouse::Cursor::Unavailable);
    let pixels = renderer.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::BLACK,
    );
    (pixels, ui.into_cache())
}

fn click_column(
    content: &mut text_editor::Content<Renderer>,
    alignment: Option<Alignment>,
    cache: Cache,
) -> (usize, Cache) {
    let mut renderer = new_renderer();
    let cursor = mouse::Cursor::Available(PROBE);
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let cache = {
        let mut ui = UserInterface::build(view(content, alignment), SIZE, cache, &mut renderer);
        draw(&mut ui, &mut renderer, cursor);
        // A press within 300ms and 6px of the previous one is a double click,
        // which selects a word instead of answering where the point landed.
        let elsewhere = mouse::Cursor::Available(Point::new(PROBE.x - 60.0, PROBE.y));
        let _ = ui.update(
            &Headless,
            &Waker::noop(),
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            elsewhere,
            &mut renderer,
            &mut iced_runtime::core::shell::Bus::new(),
        );
        let _ = ui.update(
            &Headless,
            &Waker::noop(),
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            cursor,
            &mut renderer,
            &mut messages,
        );
        ui.into_cache()
    };

    for message in messages.drain_messages() {
        let Msg::Edit(action) = message;
        let _ = content.perform(action);
    }

    (content.cursor().position.index, cache)
}

// The editor paints its caret and its selection only while focused.
fn focus(content: &text_editor::Content<Renderer>, cache: Cache) -> Cache {
    let mut renderer = new_renderer();
    let cursor = mouse::Cursor::Available(Point::new(20.0, 12.0));
    let mut ui = UserInterface::build(view(content, None), SIZE, cache, &mut renderer);
    draw(&mut ui, &mut renderer, cursor);

    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    for event in [
        Event::Window(window::Event::Focused),
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        let _ = ui.update(
            &Headless,
            &Waker::noop(),
            &[event],
            cursor,
            &mut renderer,
            &mut messages,
        );
    }

    ui.into_cache()
}

// The retained editor, not the element, holds the alignment, so only an
// element that states its absence can take it back.
#[test]
fn a_rebuild_without_the_alignment_returns_the_glyphs_and_selection_to_the_reading_edge() {
    let mut content = content();
    let cache = focus(&content, Cache::default());
    let _ = content.perform(text_editor::Action::SelectAll);

    let (reading_edge, cache) = frame(&content, None, cache);
    let (centred, cache) = frame(&content, Some(Alignment::Center), cache);
    assert_ne!(
        reading_edge, centred,
        "centring must paint somewhere else, or the frames below prove nothing"
    );

    let (rebuilt, _) = frame(&content, None, cache);
    assert_eq!(
        rebuilt, reading_edge,
        "an element rebuilt without an alignment must paint what it painted \
         before one was ever asked for"
    );
}

#[test]
fn a_rebuild_without_the_alignment_returns_the_hit_test_to_the_reading_edge() {
    let mut content = content();
    let cache = focus(&content, Cache::default());

    let (reading_edge, cache) = click_column(&mut content, None, cache);
    assert_eq!(
        reading_edge,
        TEXT.len(),
        "a click past the end of a left-aligned run lands at the end of the line"
    );

    let (centred, cache) = click_column(&mut content, Some(Alignment::Center), cache);
    assert!(
        centred < TEXT.len(),
        "the same click must land inside a centred run: {centred} of {}",
        TEXT.len()
    );

    let (rebuilt, _) = click_column(&mut content, None, cache);
    assert_eq!(
        rebuilt, reading_edge,
        "an element rebuilt without an alignment must hit-test where it did \
         before one was ever asked for"
    );
}

#[test]
fn a_never_aligned_editor_walks_no_lines_and_a_release_walks_them_once() {
    let content = many_lines();
    assert_eq!(
        content.line_count(),
        LINES,
        "the fixture must hold the lines a stale scan would walk"
    );

    let mut cache = Cache::default();
    for layout in 0..3 {
        let (visited, next) = lines_visited_by(&content, None, cache);
        cache = next;
        assert_eq!(
            visited, 0,
            "layout {layout} of an editor that never took an alignment must \
             not ask its lines about one"
        );
    }

    let (installing, cache) = lines_visited_by(&content, Some(Alignment::Center), cache);

    assert_eq!(
        installing, LINES as u64,
        "installing an alignment states every line"
    );

    let (releasing, mut cache) = lines_visited_by(&content, None, cache);
    assert_eq!(
        releasing, LINES as u64,
        "releasing it walks the lines it had aligned, once"
    );

    for layout in 0..3 {
        let (visited, next) = lines_visited_by(&content, None, cache);
        cache = next;
        assert_eq!(
            visited, 0,
            "layout {layout} after the release must be back to no work"
        );
    }
}
