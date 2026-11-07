// Modified by enzUI contributors. See FORK.md.
//! Text-input status paint and idle frame routing.

use iced_runtime::UserInterface;
use iced_runtime::core::renderer::Headless as _;
use iced_runtime::core::shell::Waker;
use iced_runtime::core::widget::{Id, Operation, operation};
use iced_runtime::core::{
    Background, Border, Color, Element, Event, Length, Point, Size, Theme, mouse, window,
};
use iced_runtime::user_interface::{Cache, State};
use iced_widget::{container, text_input};
use std::time::{Duration, Instant};

type Renderer = iced_tiny_skia::Renderer;
const SIZE: Size = Size::new(320.0, 80.0);
const ACTIVE: Color = Color::from_rgb(1.0, 0.0, 0.0);
const HOVER: Color = Color::from_rgb(0.0, 0.0, 1.0);
const FOCUSED: Color = Color::from_rgb(1.0, 1.0, 0.0);
const FOCUSED_HOVER: Color = Color::from_rgb(0.0, 1.0, 0.0);
const DISABLED: Color = Color::from_rgb(0.5, 0.5, 0.5);

fn view(enabled: bool) -> Element<'static, (), Theme, Renderer> {
    let input = text_input::TextInput::new("", "sample")
        .id("input")
        .on_input_maybe(enabled.then_some(|_| ()))
        .style(|_, status| text_input::Style {
            background: Background::Color(Color::WHITE),
            border: Border {
                width: 2.0,
                radius: 6.0.into(),
                color: match status {
                    text_input::Status::Active => ACTIVE,
                    text_input::Status::Hovered => HOVER,
                    text_input::Status::Focused { is_hovered: false } => FOCUSED,
                    text_input::Status::Focused { is_hovered: true } => FOCUSED_HOVER,
                    text_input::Status::Disabled => DISABLED,
                },
            },
            icon: Color::BLACK,
            placeholder: Color::BLACK,
            value: Color::BLACK,
            selection: Color::BLACK,
        });
    container(input)
        .width(Length::Fixed(280.0))
        .padding(8.0)
        .into()
}

struct Host {
    ui: Option<UserInterface<'static, (), Theme, Renderer>>,
    renderer: Renderer,
    cursor: mouse::Cursor,
    pixels: Vec<u8>,
    now: Instant,
}
impl Host {
    fn new(enabled: bool) -> Self {
        let mut renderer = Renderer::new(iced_runtime::core::renderer::Settings::default());
        let ui = UserInterface::build(view(enabled), SIZE, Cache::default(), &mut renderer);
        Self {
            ui: Some(ui),
            renderer,
            cursor: mouse::Cursor::Unavailable,
            pixels: Vec::new(),
            now: Instant::now(),
        }
    }
    fn dispatch(&mut self, event: Event) -> window::RedrawRequest {
        let mut messages = iced_runtime::core::shell::Bus::new();
        let (state, _) = self.ui.as_mut().unwrap().update(
            &window::Headless,
            &Waker::noop(),
            &[event],
            self.cursor,
            &mut self.renderer,
            &mut messages,
        );
        assert!(messages.is_empty(), "no application update");
        let State::Updated { redraw_request, .. } = state else {
            panic!("input must not require a view rebuild")
        };
        redraw_request
    }
    fn paint(&mut self) -> Vec<u8> {
        self.ui.as_mut().unwrap().draw(
            &mut self.renderer,
            &Theme::Dark,
            &iced_runtime::core::renderer::Style::default(),
            self.cursor,
        );
        self.renderer.screenshot(
            Size::new(SIZE.width as u32, SIZE.height as u32),
            1.0,
            Color::BLACK,
        )
    }
    fn frame(&mut self) {
        self.now = Instant::now().max(self.now + Duration::from_millis(16));
        let _ = self.dispatch(Event::Window(window::Event::RedrawRequested(self.now)));
        self.pixels = self.paint();
        let control = self.paint();
        assert!(self.pixels == control, "same-input paint control");
    }
    fn move_to(&mut self, point: Point) {
        self.cursor = mouse::Cursor::Available(point);
        let request = self.dispatch(Event::Mouse(mouse::Event::CursorMoved { position: point }));
        if request == window::RedrawRequest::NextFrame {
            self.frame();
        }
    }
    fn operate(&mut self, op: &mut dyn Operation) {
        self.ui.as_mut().unwrap().operate(&self.renderer, op);
    }
    fn routes(&self) -> usize {
        self.ui.as_ref().unwrap().frame_route_counts().0
    }
    fn border(&self, color: Color) {
        let rgba = color.into_rgba8();
        let count = self
            .pixels
            .chunks_exact(4)
            .filter(|p| p.iter().zip(rgba).all(|(a, b)| a.abs_diff(b) < 8))
            .count();
        assert!(
            (500..1400).contains(&count),
            "expected {color:?} border, got {count} matching pixels"
        );
    }
    fn rebuild(&mut self, enabled: bool) {
        let cache = self.ui.take().unwrap().into_cache();
        self.ui = Some(UserInterface::build(
            view(enabled),
            SIZE,
            cache,
            &mut self.renderer,
        ));
    }
}

#[test]
fn idle_first_frame_resolves_enabled_and_disabled_styles_then_leaves_the_route() {
    for (enabled, color) in [(true, ACTIVE), (false, DISABLED)] {
        let mut host = Host::new(enabled);
        let initial_routes = host.routes();
        host.frame();
        host.border(color);
        assert!(initial_routes > 0, "first status needs a frame");
        assert_eq!(host.routes(), 0);
        let original = host.pixels.clone();
        for _ in 0..3 {
            host.frame();
            assert_eq!(host.routes(), 0);
        }
        assert!(host.pixels == original);
    }
}

#[test]
fn idle_hover_matches_initial_hover_and_restores_active_paint() {
    let mut cold = Host::new(true);
    cold.operate(&mut operation::focusable::unfocus::<()>());
    cold.cursor = mouse::Cursor::Available(Point::new(40.0, 20.0));
    cold.frame();
    cold.border(HOVER);

    let mut warm = Host::new(true);
    warm.operate(&mut operation::focusable::unfocus::<()>());
    warm.frame();
    warm.border(ACTIVE);
    assert_eq!(warm.routes(), 0);
    let original = warm.pixels.clone();
    for _ in 0..3 {
        warm.move_to(Point::new(40.0, 20.0));
        warm.border(HOVER);
        assert!(
            warm.pixels == cold.pixels,
            "hover timing must not change paint"
        );
        assert_eq!(warm.routes(), 0);
        warm.move_to(Point::new(310.0, 70.0));
        warm.border(ACTIVE);
        assert!(warm.pixels == original);
        assert_eq!(warm.routes(), 0);
    }
}

#[test]
fn idle_focus_operations_and_nonblinking_selection_keep_the_correct_style() {
    let mut host = Host::new(true);
    host.frame();
    host.operate(&mut operation::focusable::focus::<()>(Id::new("input")));
    host.frame();
    host.border(FOCUSED);
    assert!(host.routes() > 0, "focused caret is a frame listener");
    host.operate(&mut operation::text_input::select_all::<()>(Id::new(
        "input",
    )));
    host.frame();
    host.border(FOCUSED);
    assert_eq!(host.routes(), 0, "selection suppresses caret blinking");
    host.move_to(Point::new(40.0, 20.0));
    host.border(FOCUSED_HOVER);
    assert_eq!(host.routes(), 0);
    host.move_to(Point::new(310.0, 70.0));
    host.border(FOCUSED);
    host.operate(&mut operation::focusable::unfocus::<()>());
    host.frame();
    host.border(ACTIVE);
    assert_eq!(host.routes(), 0);
}

#[test]
fn idle_same_size_rebuild_refreshes_enabled_status_without_a_pointer_event() {
    let mut host = Host::new(true);
    host.frame();
    host.move_to(Point::new(40.0, 20.0));
    host.border(HOVER);
    for (enabled, expected) in [(false, DISABLED), (true, HOVER)] {
        host.rebuild(enabled);
        assert!(host.routes() > 0);
        host.frame();
        host.border(expected);
        assert_eq!(host.routes(), 0);
    }
}

#[test]
fn a_rebuilt_focused_input_asks_only_for_its_blink_deadline() {
    let mut host = Host::new(true);
    host.frame();
    host.operate(&mut operation::focusable::focus::<()>(Id::new("input")));
    host.frame();
    host.frame();
    for _ in 0..3 {
        host.rebuild(true);
        host.now += Duration::from_millis(16);
        let request = host.dispatch(Event::Window(window::Event::RedrawRequested(host.now)));
        assert!(
            matches!(request, window::RedrawRequest::At(_)),
            "a rebuilt focused input inside one blink phase must schedule its \
             blink, not demand another frame: {request:?}"
        );
        host.pixels = host.paint();
    }
}

/// After ten seconds without caret activity a focused input stops blinking:
/// it asks for no more frames, and its caret is painted solid rather than in
/// whichever phase the blink last showed.
#[test]
fn an_idle_focused_input_stops_blinking_after_ten_seconds() {
    let mut host = Host::new(true);
    host.frame();
    host.operate(&mut operation::focusable::focus::<()>(Id::new("input")));
    let focused_at = Instant::now();

    // The caret's hidden half-cycle.
    let request = host.dispatch(Event::Window(window::Event::RedrawRequested(
        focused_at + Duration::from_millis(700),
    )));
    assert_ne!(
        request,
        window::RedrawRequest::Wait,
        "a blinking caret keeps asking for frames"
    );
    let hidden = host.paint();

    // Settling shows the hidden caret again, which takes one frame; after
    // that nothing is asked for.
    let settled_at = focused_at + Duration::from_secs(11);
    let _ = host.dispatch(Event::Window(window::Event::RedrawRequested(settled_at)));
    let request = host.dispatch(Event::Window(window::Event::RedrawRequested(
        settled_at + Duration::from_millis(16),
    )));
    assert_eq!(
        request,
        window::RedrawRequest::Wait,
        "a settled caret asks for no frame"
    );
    let solid = host.paint();
    let request = host.dispatch(Event::Window(window::Event::RedrawRequested(
        settled_at + Duration::from_millis(500),
    )));
    assert_eq!(request, window::RedrawRequest::Wait);
    assert!(host.paint() == solid, "a settled caret no longer changes");
    assert!(solid != hidden, "the settled caret is painted, not hidden");
}
