// Modified by enzUI contributors. See FORK.md.
//! Widget-level raster tests for `TextInput` inline-preedit composition.
//!
//! Each test drives a real `UserInterface` on an `iced_tiny_skia` headless
//! renderer and reads back through `rightmost_ink` and `raster_delta`. Every
//! non-trivial gate is paired with a positive control so a helper returning
//! an empty raster cannot silently pass.

use std::ops::Range;
use std::time::Instant;

use iced_runtime::UserInterface;
use iced_runtime::core::InputMethod;
use iced_runtime::core::event::Event;
use iced_runtime::core::input_method;
use iced_runtime::core::renderer::Headless as _;
use iced_runtime::core::shell::Waker;
use iced_runtime::core::window::{self, Headless};
use iced_runtime::core::{Color, Element, Font, Length, Point, Rectangle, Size, Theme, mouse};
use iced_runtime::user_interface::{Cache, State as UiState};

use iced_widget::text_input;

type Renderer = iced_tiny_skia::Renderer;
type Msg = String;

// Wide enough to fit both the longest placeholder and the longest composed
// content in these tests without clipping.
const SIZE: Size = Size::new(600.0, 60.0);
const CLICK_INSIDE: Point = Point::new(20.0, 20.0);
// Narrower than the clip-gate composition, so a leak lands on the backdrop
// past this width.
const NARROW_WIDTH: u32 = 120;

fn new_renderer() -> Renderer {
    Renderer::new(iced_runtime::core::renderer::Settings::default())
}

fn view<'a>(placeholder: &'a str, value: &'a str) -> Element<'a, Msg, Theme, Renderer> {
    text_input::TextInput::new(placeholder, value)
        .font(Font::MONOSPACE)
        .on_input(Msg::from)
        .into()
}

// Width-constrained view for the clip gate; a leak past `NARROW_WIDTH`
// lands on the surrounding screenshot backdrop.
fn view_narrow<'a>(placeholder: &'a str, value: &'a str) -> Element<'a, Msg, Theme, Renderer> {
    text_input::TextInput::new(placeholder, value)
        .font(Font::MONOSPACE)
        .width(Length::Fixed(NARROW_WIDTH as f32))
        .on_input(Msg::from)
        .into()
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

fn dispatch(
    ui: &mut UserInterface<'_, Msg, Theme, Renderer>,
    renderer: &mut Renderer,
    events: &[Event],
    cursor: mouse::Cursor,
) {
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    let _ = ui.update(
        &Headless,
        &Waker::noop(),
        events,
        cursor,
        renderer,
        &mut messages,
    );
}

// `FocusMode::AlreadyFocused` skips the focus click so a chained frame
// does not promote the interaction to `mouse::Click::Kind::Double` and
// perturb the selection under test.
fn frame_with_events(
    placeholder: &str,
    value: &str,
    cache: Cache,
    focus: FocusMode,
    events: &[Event],
) -> (Vec<u8>, Cache) {
    let mut renderer = new_renderer();
    let cursor = mouse::Cursor::Available(CLICK_INSIDE);
    let cache = {
        let mut ui = UserInterface::build(view(placeholder, value), SIZE, cache, &mut renderer);
        draw(&mut ui, &mut renderer, cursor);
        if matches!(focus, FocusMode::WithFocusClick) {
            dispatch(
                &mut ui,
                &mut renderer,
                &[
                    Event::Window(window::Event::Focused),
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                cursor,
            );
        }
        dispatch(&mut ui, &mut renderer, events, cursor);
        ui.into_cache()
    };
    let mut renderer = new_renderer();
    let mut ui = UserInterface::build(view(placeholder, value), SIZE, cache, &mut renderer);
    draw(&mut ui, &mut renderer, cursor);
    let pixels = renderer.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::BLACK,
    );
    (pixels, ui.into_cache())
}

#[derive(Clone, Copy)]
enum FocusMode {
    WithFocusClick,
    AlreadyFocused,
}

// `frame_with_events` variant that builds `view_narrow` so a wide
// composition exercises the clip decision.
fn narrow_frame_with_events(
    placeholder: &str,
    value: &str,
    cache: Cache,
    focus: FocusMode,
    events: &[Event],
) -> (Vec<u8>, Cache) {
    let mut renderer = new_renderer();
    let cursor = mouse::Cursor::Available(CLICK_INSIDE);
    let cache = {
        let mut ui =
            UserInterface::build(view_narrow(placeholder, value), SIZE, cache, &mut renderer);
        draw(&mut ui, &mut renderer, cursor);
        if matches!(focus, FocusMode::WithFocusClick) {
            dispatch(
                &mut ui,
                &mut renderer,
                &[
                    Event::Window(window::Event::Focused),
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                cursor,
            );
        }
        dispatch(&mut ui, &mut renderer, events, cursor);
        ui.into_cache()
    };
    let mut renderer = new_renderer();
    let mut ui = UserInterface::build(view_narrow(placeholder, value), SIZE, cache, &mut renderer);
    draw(&mut ui, &mut renderer, cursor);
    let pixels = renderer.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::BLACK,
    );
    (pixels, ui.into_cache())
}

fn preedit_events(content: &str, selection: Option<Range<usize>>) -> Vec<Event> {
    vec![
        Event::InputMethod(input_method::Event::Opened),
        Event::InputMethod(input_method::Event::Preedit(content.to_owned(), selection)),
    ]
}

// Drives one Focused-click + `events` + `RedrawRequested` batch through a
// real `UserInterface::update` and returns the `InputMethod::Enabled.cursor`
// rectangle the widget advertised to the runtime.
fn ime_cursor_rectangle(placeholder: &str, value: &str, events: &[Event]) -> Rectangle {
    let mut renderer = new_renderer();
    let cursor = mouse::Cursor::Available(CLICK_INSIDE);
    let mut ui = UserInterface::build(
        view(placeholder, value),
        SIZE,
        Cache::default(),
        &mut renderer,
    );
    draw(&mut ui, &mut renderer, cursor);
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
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
    let mut batch: Vec<Event> = events.to_vec();
    batch.push(Event::Window(
        window::Event::RedrawRequested(Instant::now()),
    ));
    let (state, _statuses) = ui.update(
        &Headless,
        &Waker::noop(),
        &batch,
        cursor,
        &mut renderer,
        &mut messages,
    );
    match state {
        UiState::Updated { input_method, .. } => match input_method {
            InputMethod::Enabled { cursor, .. } => cursor,
            InputMethod::Disabled => panic!(
                "widget refused to enable IME on a focused, redrawn input; \
                 without an `Enabled` payload the composition-aware caret \
                 assertion cannot fire"
            ),
        },
        UiState::Outdated => {
            panic!("UserInterface reported Outdated after Focused + RedrawRequested")
        }
    }
}

// Uses the widget's own `Drag::Select` route rather than a synthetic
// Ctrl+A key event, which would need `SmolStr`, `Location`, `text` and
// `repeat` fields that vary between winit revisions.
#[test]
fn input_method_cursor_moves_to_the_composition_visual_caret() {
    // Model value stays empty and its `Cursor::Index` therefore stays at 0.
    // Only the preedit selection differs across the three probes, so any
    // horizontal movement of the reported IME cursor is attributable to the
    // composition-aware caret path.
    let base = ime_cursor_rectangle("", "", &[]);
    let caret_at_end = ime_cursor_rectangle("", "", &preedit_events("abc", Some(3..3)));
    // Composition caret at the start reads the preedit's `selection.start`,
    // which is a byte offset inside the preedit content. With the composition
    // absent it collapses to the model cursor at 0, so this must equal `base`.
    let caret_at_start = ime_cursor_rectangle("", "", &preedit_events("abc", Some(0..0)));

    assert!(
        caret_at_end.x > base.x,
        "the composition-aware IME cursor must sit to the right of the empty \
         value's cursor when the preedit caret is at the end of `abc`; \
         base.x = {}, composed.x = {}. If they are equal the widget is still \
         measuring against `state.value.raw()` at the model cursor and would \
         advertise the wrong candidate rectangle to the compositor",
        base.x,
        caret_at_end.x
    );
    assert_eq!(
        caret_at_start.x, base.x,
        "a composition with the preedit caret at position 0 must advertise the \
         same IME cursor x as the pure empty-value frame (base.x = {}, \
         caret_at_start.x = {}); a difference would mean the widget shifted \
         the caret by more than the preedit head advance",
        base.x, caret_at_start.x
    );

    // UTF-8 discrimination: caret at byte 6 of `日本語` sits between the second
    // and third grapheme; caret at byte 9 sits after the third. Same glyphs,
    // different `selection.start`, so the reported IME cursor x must move,
    // and by strictly more than one grapheme's worth of the preceding `abc`.
    let utf8_mid = ime_cursor_rectangle("", "", &preedit_events("日本語", Some(6..6)));
    let utf8_end = ime_cursor_rectangle("", "", &preedit_events("日本語", Some(9..9)));
    assert!(
        utf8_end.x > utf8_mid.x,
        "moving the preedit caret from byte 6 to byte 9 of `日本語` must move \
         the IME cursor right (mid.x = {}, end.x = {}). If they are equal the \
         widget is not walking the preedit byte offset into grapheme steps \
         against the composition paragraph",
        utf8_mid.x,
        utf8_end.x
    );

    // Positive control: repeating the same probe must return an equal x.
    // A drift here would mean the harness is not holding other state constant
    // and the `caret_at_end > base` inequality could be measuring frame
    // noise instead of composition placement.
    let utf8_mid_again = ime_cursor_rectangle("", "", &preedit_events("日本語", Some(6..6)));
    assert_eq!(
        utf8_mid.x, utf8_mid_again.x,
        "two runs with the same preedit selection must report the same IME \
         cursor x (first = {}, repeat = {})",
        utf8_mid.x, utf8_mid_again.x
    );
}

fn drag_select_events() -> Vec<Event> {
    let left = Point::new(4.0, 20.0);
    let right = Point::new(SIZE.width - 4.0, 20.0);
    vec![
        Event::Mouse(mouse::Event::CursorMoved { position: left }),
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::CursorMoved { position: right }),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]
}

// Stays inside the painted field: `TextInput` is `Length::Shrink`
// vertically, so rows past the field are `Color::BLACK` backdrop and
// would register as ink under `rightmost_ink`.
const TEXT_ROW: Range<u32> = 15..25;
// Stays inside the field's border + `DEFAULT_PADDING` chrome so the
// right-edge border pixel does not swallow every rightmost-ink reading.
const SCAN_X: Range<u32> = 6..594;

// Compares against the field's own fill (not the `Color::BLACK` backdrop)
// so a Dark theme's near-black background does not register as ink on
// every pixel; `TOLERANCE` absorbs border and anti-aliasing bleed.
fn rightmost_ink(pixels: &[u8]) -> Option<u32> {
    const TOLERANCE: u32 = 24; // per-channel byte distance, sum across RGB.
    let fill = field_fill_sample(pixels);
    let width = SIZE.width as u32;
    let stride = width as usize * 4;
    let mut rightmost = None;
    for y in TEXT_ROW.clone() {
        let row = &pixels[(y as usize) * stride..(y as usize + 1) * stride];
        for x in SCAN_X.clone().rev() {
            let i = (x as usize) * 4;
            let dr = row[i].abs_diff(fill[0]) as u32;
            let dg = row[i + 1].abs_diff(fill[1]) as u32;
            let db = row[i + 2].abs_diff(fill[2]) as u32;
            if dr + dg + db > TOLERANCE {
                rightmost = Some(rightmost.map_or(x, |r: u32| r.max(x)));
                break;
            }
        }
    }
    rightmost
}

// Samples inside the field padding, above the glyph baseline, so the
// reading is never contaminated by a glyph, caret or border pixel.
fn field_fill_sample(pixels: &[u8]) -> [u8; 4] {
    let x = SIZE.width as u32 / 2;
    let y = TEXT_ROW.start.saturating_sub(4).max(2);
    let stride = SIZE.width as usize * 4;
    let i = (y as usize) * stride + (x as usize) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn raster_delta(a: &[u8], b: &[u8]) -> u64 {
    assert_eq!(a.len(), b.len(), "raster sizes must agree");
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum()
}

#[test]
fn empty_value_preedit_suppresses_placeholder_past_the_composition() {
    let placeholder = "A very long placeholder text";
    let (baseline, _) = frame_with_events(
        placeholder,
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &[],
    );
    let (composed, _) = frame_with_events(
        placeholder,
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("A", Some(1..1)),
    );

    let baseline_end =
        rightmost_ink(&baseline).expect("placeholder must be visible in the baseline frame");
    let composed_end = rightmost_ink(&composed).expect("composition must paint some ink");

    assert!(
        composed_end + 40 < baseline_end,
        "with the fix the composed row is much narrower than the placeholder row \
         (baseline rightmost inked x = {baseline_end}, composed = {composed_end}); \
         the pre-fix overlay left placeholder pixels showing to the right of the \
         composition"
    );

    // Positive control: empty preedit must equal the pure placeholder frame.
    let (empty_preedit, _) = frame_with_events(
        placeholder,
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("", None),
    );
    assert_eq!(
        empty_preedit, baseline,
        "an empty preedit is not composing; the raster must match the pure \
         placeholder frame or the test cannot distinguish suppression from \
         a rendering coincidence"
    );
}

#[test]
fn selection_is_visually_replaced_by_the_preedit() {
    let value = "10+20";
    let select_all = drag_select_events();
    let (baseline, cache) = frame_with_events(
        "",
        value,
        Cache::default(),
        FocusMode::WithFocusClick,
        &select_all,
    );

    // Chained frame reuses the cache carrying the selection, so it only
    // opens a preedit on top and no extra click perturbs it.
    let composing = preedit_events("4+5", Some(3..3));
    let (composed, _) = frame_with_events("", value, cache, FocusMode::AlreadyFocused, &composing);

    let baseline_end = rightmost_ink(&baseline).expect("baseline value must be visible");
    let composed_end = rightmost_ink(&composed).expect("composed value must be visible");
    assert!(
        composed_end + 8 < baseline_end,
        "composed `4+5` must occupy fewer pixels than selected `10+20` \
         (baseline rightmost x = {baseline_end}, composed = {composed_end}); \
         the pre-fix raster kept `20` visible past the composition"
    );

    // Positive control: a same-length composition must land near the
    // baseline rightmost pixel; a large gap would mean the shorter-composed
    // check is measuring something other than composition width.
    let mut same_length = select_all.clone();
    same_length.extend(preedit_events("10+20", Some(5..5)));
    let (same_length_composed, _) = frame_with_events(
        "",
        value,
        Cache::default(),
        FocusMode::WithFocusClick,
        &same_length,
    );
    let same_length_end =
        rightmost_ink(&same_length_composed).expect("same-length composition must be visible");
    assert!(
        same_length_end.abs_diff(baseline_end) < 15,
        "a same-length composition must occupy roughly the same rightmost x as \
         the baseline value (baseline = {baseline_end}, same-length composed = \
         {same_length_end}); a large gap here would mean the test is measuring \
         something other than composition span"
    );
}

#[test]
fn insertion_at_caret_lengthens_the_visible_row() {
    // With no explicit selection the caret defaults to the end of the
    // value, so the composition is inserted after `ab`.
    let (baseline, cache) =
        frame_with_events("", "ab", Cache::default(), FocusMode::WithFocusClick, &[]);
    let (composed, _) = frame_with_events(
        "",
        "ab",
        cache,
        FocusMode::AlreadyFocused,
        &preedit_events("XYZ", Some(3..3)),
    );

    let baseline_end = rightmost_ink(&baseline).expect("baseline row must be inked");
    let composed_end = rightmost_ink(&composed).expect("composed row must be inked");
    assert!(
        composed_end > baseline_end,
        "inserting `XYZ` after `ab` must extend the inked row to the right \
         (baseline rightmost x = {baseline_end}, composed = {composed_end}); \
         if it does not, the composition is being drawn on top of the value \
         instead of alongside it"
    );

    // Positive control: an empty preedit must not extend the row.
    let (empty_preedit, _) = frame_with_events(
        "",
        "ab",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("", None),
    );
    let empty_end = rightmost_ink(&empty_preedit)
        .expect("baseline value must remain visible under empty preedit");
    assert_eq!(
        empty_end, baseline_end,
        "an empty preedit must not extend the row past the plain value \
         (baseline = {baseline_end}, empty-preedit = {empty_end})"
    );
}

#[test]
fn cancellation_restores_the_pre_composition_raster() {
    let value = "hi";
    let (baseline, cache) =
        frame_with_events("", value, Cache::default(), FocusMode::WithFocusClick, &[]);
    let (composed, cache) = frame_with_events(
        "",
        value,
        cache,
        FocusMode::AlreadyFocused,
        &preedit_events("X", Some(1..1)),
    );
    assert_ne!(
        composed, baseline,
        "composition must move some pixels off the baseline before we can \
         assert that cancellation puts them back"
    );
    let (after_close, _) = frame_with_events(
        "",
        value,
        cache,
        FocusMode::AlreadyFocused,
        &[Event::InputMethod(input_method::Event::Closed)],
    );
    assert_eq!(
        after_close, baseline,
        "Closed must clear the composition and restore the pre-composition \
         raster; a residual composition here means the widget kept painting \
         from stale preedit state after the compositor cancelled"
    );
}

#[test]
fn utf8_preedit_caret_sits_between_the_expected_graphemes() {
    // Byte 6 is the grapheme boundary between the 2nd and 3rd character
    // of `日本語`; two frames share every glyph and differ only in caret
    // position, so any raster delta must come from the caret.
    let (caret_mid, _) = frame_with_events(
        "",
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("日本語", Some(6..6)),
    );
    let (caret_end, _) = frame_with_events(
        "",
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("日本語", Some(9..9)),
    );
    let delta = raster_delta(&caret_mid, &caret_end);
    assert!(
        delta > 0,
        "moving the preedit caret from byte 6 to byte 9 must move at least \
         some pixels; raster delta = {delta}"
    );

    // Positive control: identical selection produces identical rasters.
    let (caret_end_again, _) = frame_with_events(
        "",
        "",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("日本語", Some(9..9)),
    );
    assert_eq!(
        caret_end, caret_end_again,
        "two frames captured under the same preedit selection must be \
         pixel-identical; a nonzero delta here means the harness is not \
         holding other state constant"
    );
}

#[test]
fn losing_focus_stops_the_field_from_showing_the_composition() {
    let (composed, cache) = frame_with_events(
        "",
        "hi",
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events("X", Some(1..1)),
    );
    let (baseline, _) =
        frame_with_events("", "hi", Cache::default(), FocusMode::WithFocusClick, &[]);
    assert_ne!(
        composed, baseline,
        "composed and pre-composition frames must differ before we can \
         claim focus loss reverts them"
    );
    let (after_unfocus, _) = frame_with_events(
        "",
        "hi",
        cache,
        FocusMode::AlreadyFocused,
        &[Event::Window(window::Event::Unfocused)],
    );

    // Compared by `rightmost_ink` rather than raster equality because a
    // focused frame also paints a blinking caret at its own x.
    let composed_end = rightmost_ink(&composed).expect("composed row must be inked");
    let unfocused_end = rightmost_ink(&after_unfocus).expect("value must remain visible");
    assert!(
        unfocused_end + 4 < composed_end,
        "an unfocused field must not display the composition \
         (composed rightmost x = {composed_end}, unfocused = {unfocused_end}); \
         a value close to `composed_end` means preedit pixels bled into a \
         field that no longer holds focus"
    );
}

#[test]
fn input_method_request_carries_no_preedit_when_composing_inline() {
    // `UserInterface` does not expose the widget's `input_method` request
    // to a headless harness, so the routing contract is enforced by a
    // source-level scan of `TextInput::input_method`.
    let source = include_str!("../src/text_input.rs");
    assert!(
        source.contains("preedit: None,"),
        "TextInput::input_method must return `preedit: None` while the \
         widget composites inline; a forwarded preedit reintroduces the \
         over-the-spot overlay #2901 was written to remove"
    );
    assert!(
        !source.contains("preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref)"),
        "the pre-#2901 forwarding line must not return -- it re-enables the \
         double-painting overlay this issue removed"
    );
}

// Fixed-width field so any leaked glyph past `NARROW_WIDTH` lands on the
// `Color::BLACK` backdrop, where it is unambiguously distinguishable from
// any field pixel.
#[test]
fn long_composition_is_clipped_inside_the_field_chrome() {
    let placeholder = "";
    let value = "";
    let long = "A".repeat(26);
    // Caret at position 0 keeps the horizontal scroll offset at 0, so the
    // composition starts from the left padding and any pixel past the
    // right border is a leak the clip should have suppressed.
    let (composed, _) = narrow_frame_with_events(
        placeholder,
        value,
        Cache::default(),
        FocusMode::WithFocusClick,
        &preedit_events(&long, Some(0..0)),
    );

    // Anything past the field's right border reads directly against the
    // black backdrop, so a nonzero delta from black is a leaked glyph.
    let stride = SIZE.width as usize * 4;
    let mut leaked: Vec<(u32, u32)> = Vec::new();
    for y in TEXT_ROW.clone() {
        let row = &composed[(y as usize) * stride..(y as usize + 1) * stride];
        for x in (NARROW_WIDTH + 4)..(SIZE.width as u32 - 4) {
            let i = (x as usize) * 4;
            let dr = row[i] as u32;
            let dg = row[i + 1] as u32;
            let db = row[i + 2] as u32;
            if dr + dg + db > 24 {
                leaked.push((x, y));
                if leaked.len() >= 5 {
                    break;
                }
            }
        }
        if leaked.len() >= 5 {
            break;
        }
    }
    assert!(
        leaked.is_empty(),
        "composition wider than the field must be clipped by \
         with_layer(text_bounds, ..); leaked composition pixels past \
         NARROW_WIDTH={NARROW_WIDTH}: first samples = {:?}",
        &leaked[..leaked.len().min(5)]
    );

    // Positive control: assert composition ink exists inside the field,
    // so the empty-past-border check is not passing on a black raster.
    let mut inside_ink = 0usize;
    let fill = field_fill_sample(&composed);
    for y in TEXT_ROW.clone() {
        let row = &composed[(y as usize) * stride..(y as usize + 1) * stride];
        for x in 6..(NARROW_WIDTH - 6) {
            let i = (x as usize) * 4;
            let dr = (row[i] as i32 - fill[0] as i32).abs();
            let dg = (row[i + 1] as i32 - fill[1] as i32).abs();
            let db = (row[i + 2] as i32 - fill[2] as i32).abs();
            if dr + dg + db > 24 {
                inside_ink += 1;
            }
        }
    }
    assert!(
        inside_ink > 20,
        "the composition must actually be painted inside the narrow field \
         (found {inside_ink} inked pixels); a value near zero here means \
         the composition never reached the frame, so the empty-past-border \
         assertion would pass trivially"
    );
}
