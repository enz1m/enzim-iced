// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #1802 — late-latch drag coalescing.
//!
//! The text editor defers `Action::Drag` publication to `RedrawRequested`,
//! overwriting a `pending_drag: Option<Point>` on every `CursorMoved`
//! during an active drag.  This guarantees:
//!
//! - At most one selection update per painted frame (bounded cost); the
//!   default publishes `Action::Drag`, while the opt-in direct owner mutates
//!   `Content` before paint and publishes only a cursor notification.
//! - The painted frame uses the current redraw-time window cursor, not a stale
//!   pending endpoint (including when raw cursor state advances before the
//!   corresponding widget-event batch is dispatched).
//! - `ButtonReleased` flushes any pending endpoint before ending the drag,
//!   so the final position is never lost.
//! - Nothing pending, nothing published: a click that never moved and every
//!   frame after the release flush publish no drag at all, so the redraw-time
//!   refresh can never invent one.
//! - Focus loss mid-drag ends the drag: the latched endpoint is dropped rather
//!   than flushed by the next frame, and later button-free motion cannot
//!   re-arm it.
//!
//! Tests exercise the real `UserInterface::update` dispatch path, including
//! a `scrollable` parent that translates the cursor.

use std::time::Instant;

use iced_runtime::UserInterface;
use iced_runtime::core::event::Event;
use iced_runtime::core::shell::Waker;
use iced_runtime::core::window::{self, Headless};
use iced_runtime::core::{Element, Font, Point, Size, Theme, mouse};
use iced_runtime::user_interface;

use iced_widget::scrollable::AbsoluteOffset;
use iced_widget::{scrollable, text_editor};

type Renderer = iced_tiny_skia::Renderer;

#[derive(Debug, Clone)]
enum Msg {
    Edit(text_editor::Action),
    DragUpdate(text_editor::Cursor),
}

const SIZE: Size = Size::new(400.0, 200.0);
const SCROLL_OFFSET_Y: f32 = 50.0;

fn seed_content() -> text_editor::Content<Renderer> {
    let mut s = String::new();
    for i in 0..200 {
        s.push_str(&format!("line{i:03}\n"));
    }
    text_editor::Content::<Renderer>::with_text(&s)
}

fn scrolled_view(content: &text_editor::Content<Renderer>) -> Element<'_, Msg, Theme, Renderer> {
    scrollable::Scrollable::with_direction(
        text_editor::TextEditor::new(content)
            .font(Font::MONOSPACE)
            .on_action(Msg::Edit),
        scrollable::Direction::Vertical(scrollable::Scrollbar::new()),
    )
    .initial_offset(AbsoluteOffset {
        x: 0.0,
        y: SCROLL_OFFSET_Y,
    })
    .into()
}

fn plain_view(content: &text_editor::Content<Renderer>) -> Element<'_, Msg, Theme, Renderer> {
    text_editor::TextEditor::new(content)
        .font(Font::MONOSPACE)
        .on_action(Msg::Edit)
        .into()
}

fn direct_drag_view(content: &text_editor::Content<Renderer>) -> Element<'_, Msg, Theme, Renderer> {
    text_editor::TextEditor::new(content)
        .font(Font::MONOSPACE)
        .on_action(Msg::Edit)
        .on_drag_update(Msg::DragUpdate)
        .into()
}

/// Extract all `Action::Drag` endpoints from collected messages.
fn drag_endpoints(messages: &iced_runtime::core::shell::Bus<Msg>) -> Vec<Point> {
    messages
        .messages()
        .iter()
        .filter_map(|m| match m {
            Msg::Edit(text_editor::Action::Drag(p)) => Some(*p),
            Msg::DragUpdate(_) | Msg::Edit(_) => None,
        })
        .collect()
}

fn drag_updates(messages: &iced_runtime::core::shell::Bus<Msg>) -> Vec<text_editor::Cursor> {
    messages
        .messages()
        .iter()
        .filter_map(|m| match m {
            Msg::DragUpdate(cursor) => Some(*cursor),
            Msg::Edit(_) => None,
        })
        .collect()
}

fn new_renderer() -> Renderer {
    Renderer::new(iced_runtime::core::renderer::Settings::default())
}

fn cursor_train(count: usize, base: Point) -> Vec<Event> {
    (0..count)
        .map(|i| {
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(base.x + i as f32, base.y),
            })
        })
        .collect()
}

fn redraw_event() -> Event {
    Event::Window(window::Event::RedrawRequested(Instant::now()))
}

/// Set up: initial draw, press, redraw after press.  Returns the live UI
/// and renderer so callers can drive further batches.
fn setup_drag(
    view: Element<'_, Msg, Theme, Renderer>,
    press_at: Point,
) -> (UserInterface<'_, Msg, Theme, Renderer>, Renderer) {
    let mut renderer = new_renderer();
    let waker = Waker::noop();
    let window = Headless;
    let mut ui = UserInterface::build(view, SIZE, user_interface::Cache::default(), &mut renderer);

    // First draw so scrollable layout consumes initial_offset.
    ui.draw(
        &mut renderer,
        &Theme::Dark,
        &iced_runtime::core::renderer::Style::default(),
        mouse::Cursor::Unavailable,
    );

    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    let press_cursor = mouse::Cursor::Available(press_at);

    // Press to arm drag_click.
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))],
        press_cursor,
        &mut renderer,
        &mut messages,
    );

    // Redraw after press (mirrors iced_winit).
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        press_cursor,
        &mut renderer,
        &mut messages,
    );

    (ui, renderer)
}

// -----------------------------------------------------------------------
// Test 1: two input batches, NO interposed RedrawRequested — the LATEST
//         endpoint must be the one published when redraw finally fires.
//
// This is the scenario the previous flag-based design failed: batch 2's
// newer endpoint was dropped because `drag_pending` was still true.
//
// Sabotage: publish `Action::Drag` directly in `Update::Drag` instead of
// storing to `pending_drag`.  Both batch endpoints would be published (two
// messages instead of one), and the first/older endpoint would be the one
// that reaches `Editor::perform` first — trailing selection.
// -----------------------------------------------------------------------
#[test]
fn two_batches_without_redraw_publishes_latest_endpoint() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // Batch 1: cursor moves to endpoint A.
    let endpoint_a = Point::new(120.0, 40.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(5, Point::new(90.0, 40.0)),
        mouse::Cursor::Available(endpoint_a),
        &mut renderer,
        &mut messages,
    );

    // NO RedrawRequested here — simulates frame-callback throttling.

    // Batch 2: cursor moves further to endpoint B.
    let endpoint_b = Point::new(200.0, 60.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(3, Point::new(130.0, 50.0)),
        mouse::Cursor::Available(endpoint_b),
        &mut renderer,
        &mut messages,
    );

    // Still no drag published — only pending_drag overwritten.
    let drags_before_redraw = drag_endpoints(&messages);
    assert!(
        drags_before_redraw.is_empty(),
        "no Action::Drag should be published before RedrawRequested; \
         got {drags_before_redraw:?}",
    );

    // Now the redraw fires, flushing the latest endpoint.
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(endpoint_b),
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(
        drags.len(),
        1,
        "exactly one Action::Drag per redraw — got {drags:?}",
    );

    // The published endpoint must be the LATEST (B), not the older (A).
    // This is the pointer-to-presented-endpoint consequence: the painted
    // frame must show where the pointer IS, not where it WAS.
    let published = drags[0];
    // `position_in(bounds)` subtracts bounds origin + padding, so the
    // exact value depends on layout.  We check that it was derived from
    // endpoint_b's cursor, not endpoint_a's.  Since both cursors differ
    // by 80px horizontally and 20px vertically, comparing with a generous
    // tolerance proves which one won.
    let distance_to_a =
        ((published.x - (endpoint_a.x)).powi(2) + (published.y - (endpoint_a.y)).powi(2)).sqrt();
    let distance_to_b =
        ((published.x - (endpoint_b.x)).powi(2) + (published.y - (endpoint_b.y)).powi(2)).sqrt();
    assert!(
        distance_to_b < distance_to_a,
        "published endpoint {published:?} must be closer to latest \
         endpoint_b {endpoint_b:?} (dist {distance_to_b:.1}) than to \
         stale endpoint_a {endpoint_a:?} (dist {distance_to_a:.1})",
    );
}

// -----------------------------------------------------------------------
// Test 2: winit has already accepted endpoint B into its window cursor, but
//         the widget's last dispatched CursorMoved only latched endpoint A.
//         RedrawRequested must sample the current cursor again instead of
//         painting the stale pending waypoint.
//
// This is the ordering captured by the hold=0 #1802 product evidence: a
// redraw can overtake the AboutToWait dispatch of the newest CursorMoved.
// The PNG then retains exactly the prior waypoint even though winit's window
// cursor is already newer. This gate crosses the real UserInterface dispatch
// seam that supplies that current cursor to the widget.
//
// Sabotage: remove the redraw-time cursor refresh and publish only
// `pending_drag.take()`. The assertion sees endpoint A instead of B.
// -----------------------------------------------------------------------
#[test]
fn redraw_uses_current_cursor_instead_of_stale_pending_waypoint() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // The widget has only dispatched endpoint A so far.
    let endpoint_a = Point::new(120.0, 40.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, endpoint_a),
        mouse::Cursor::Available(endpoint_a),
        &mut renderer,
        &mut messages,
    );
    assert!(
        drag_endpoints(&messages).is_empty(),
        "drag must still be pending"
    );

    // Model the winit ordering directly: raw CursorMoved(B) has updated the
    // window cursor, while its widget event has not passed AboutToWait yet;
    // RedrawRequested is dispatched with B as UserInterface's current cursor.
    let endpoint_b = Point::new(210.0, 40.0);
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(endpoint_b),
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(drags.len(), 1, "one frame must publish one drag: {drags:?}");

    let published = drags[0];
    let distance_to_a = (published.x - endpoint_a.x).abs();
    let distance_to_b = (published.x - endpoint_b.x).abs();
    assert!(
        distance_to_b < distance_to_a,
        "RedrawRequested published stale endpoint A: published={published:?}, \
         pending={endpoint_a:?}, current_window_cursor={endpoint_b:?}",
    );

    // The refresh must not clamp or discard an endpoint beyond the widget.
    // That coordinate is the signal the graphics editor uses to reveal and
    // autoscroll at an edge.
    messages.clear();
    let endpoint_c = Point::new(220.0, 100.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, endpoint_c),
        mouse::Cursor::Available(endpoint_c),
        &mut renderer,
        &mut messages,
    );
    let outside = Point::new(SIZE.width + 30.0, SIZE.height + 20.0);
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(outside),
        &mut renderer,
        &mut messages,
    );
    let outside_drags = drag_endpoints(&messages);
    assert_eq!(outside_drags.len(), 1, "outside redraw: {outside_drags:?}");
    assert!(
        outside_drags[0].x > SIZE.width && outside_drags[0].y > SIZE.height,
        "redraw-time refresh must preserve the out-of-bounds edge endpoint \
         for autoscroll: {outside_drags:?}",
    );
}

// -----------------------------------------------------------------------
// Opt-in direct drag: the redraw must mutate Content before the application
// consumes any message. This is the product consequence of the low-latency
// path: paint can observe the newest selection in this same frame, while the
// callback is only a cursor/status notification and cannot cause a second
// perform.
//
// Sabotage: remove `perform_pointer_action(Action::Drag(position))` from the
// redraw branch. The notification remains, but `selection_range` is still
// absent and this test reddens.
// -----------------------------------------------------------------------
#[test]
fn direct_drag_updates_content_before_notification_is_consumed() {
    let mut content = seed_content();
    // Seed the anchor through the same editor action the application performs
    // for the press. The UserInterface below owns only a shared borrow, which
    // is precisely what the widget's narrow interior-mutable drag seam exists
    // to update.
    let _ = content.perform(text_editor::Action::Click(
        Point::new(40.0, 20.0),
        mouse::click::Kind::Single,
    ));
    let before = content.cursor();

    let view = direct_drag_view(&content);
    let press = Point::new(40.0, 20.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    let endpoint = Point::new(240.0, 60.0);

    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(8, Point::new(80.0, 30.0)),
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );
    assert!(drag_endpoints(&messages).is_empty());
    assert!(drag_updates(&messages).is_empty());

    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );

    assert!(
        drag_endpoints(&messages).is_empty(),
        "direct mode must not publish an Action::Drag for the app to perform twice"
    );
    let updates = drag_updates(&messages);
    assert_eq!(updates.len(), 1, "one redraw produces one notification");

    let after = content.cursor();
    assert_ne!(
        after.position, before.position,
        "the widget did not move the cursor"
    );
    assert_eq!(
        updates[0], after,
        "notification must describe the applied selection"
    );
    assert!(
        content.selection_range().is_some(),
        "the same-frame drag must establish a real logical selection"
    );

    // Release-before-redraw is the other flush site. It must use the same
    // direct owner, preserving the final endpoint without handing the app an
    // Action::Drag to replay.
    messages.clear();
    let release_at = Point::new(300.0, 80.0);
    let _ = ui.update(
        &window,
        &waker,
        &[
            Event::Mouse(mouse::Event::CursorMoved {
                position: release_at,
            }),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(release_at),
        &mut renderer,
        &mut messages,
    );
    assert!(drag_endpoints(&messages).is_empty());
    assert_eq!(drag_updates(&messages).len(), 1);
    assert_ne!(content.cursor().position, after.position);
    drop(ui);
}

// -----------------------------------------------------------------------
// Test 3: CursorMoved + ButtonReleased in one batch — the pending endpoint
//         must be flushed by Release so the final position is not lost.
//
// Sabotage: remove the `pending_drag.take()` flush from `Update::Release`.
// The final drag position would be lost — `drag_endpoints` would be empty
// and the selection would freeze at whatever the previous redraw painted.
// -----------------------------------------------------------------------
#[test]
fn release_flushes_pending_endpoint() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // One batch: CursorMoved(s) then ButtonReleased, all with the same
    // latest cursor (as UserInterface::update supplies).
    let final_endpoint = Point::new(250.0, 80.0);
    let final_cursor = mouse::Cursor::Available(final_endpoint);

    let mut batch: Vec<Event> = cursor_train(4, Point::new(100.0, 50.0));
    batch.push(Event::Mouse(mouse::Event::ButtonReleased(
        mouse::Button::Left,
    )));

    let _ = ui.update(
        &window,
        &waker,
        &batch,
        final_cursor,
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(
        drags.len(),
        1,
        "Release must flush the pending endpoint — got {drags:?}",
    );

    // Verify it came from the final cursor, not some earlier position.
    // The endpoint is in widget-local coords so won't match exactly, but
    // it must be meaningfully nonzero (not the press origin).
    assert!(
        drags[0].x > 10.0 && drags[0].y > 10.0,
        "flushed endpoint {drags:?} must reflect the final cursor position, \
         not the press origin",
    );
}

// -----------------------------------------------------------------------
// Test 4: scrolled editor — cursor translation must not break the latch.
//
// A `scrollable` translates `cursor` by `+translation` before dispatching
// to the child.  The latch uses only `cursor.position_in(bounds)`, where
// both operands are translated consistently.
//
// Sabotage: gate `pending_drag` storage on
// `cursor.position() == Some(event_position)` (raw-position equality).
// Under nonzero scroll offset the two never match, so pending_drag is
// never set and no drag is published — this assertion fires.
// -----------------------------------------------------------------------
#[test]
fn scrolled_editor_late_latch_survives_translation() {
    let content = seed_content();
    let view = scrolled_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let endpoint = Point::new(200.0, 80.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(6, Point::new(90.0, 40.0)),
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );

    // Nothing published yet — verify the latch stored.
    assert!(
        drag_endpoints(&messages).is_empty(),
        "no drag before redraw under scrollable",
    );

    // Redraw flushes.
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(
        drags.len(),
        1,
        "under a scrollable with nonzero offset, the late-latch must still \
         produce exactly one drag on redraw — got {drags:?}",
    );
}

// -----------------------------------------------------------------------
// Test 5: stationary edge point across two frames — autoscroll.
//
// After a redraw publishes the latched endpoint, a subsequent batch at the
// SAME local point must re-latch and the next redraw must re-publish.
// `graphics::text::Editor::perform` keeps out-of-bounds Drag on the
// `shape_until_cursor` path so autoscroll continues.
//
// Sabotage: prevent `pending_drag` from being set when the value equals
// the previously published point (add a `last_published` equality check).
// The second redraw would produce no drag and autoscroll would stop.
// -----------------------------------------------------------------------
#[test]
fn same_edge_point_republishes_across_frames() {
    let content = seed_content();
    let view = scrolled_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let edge = Point::new(180.0, 195.0); // near bottom edge
    let edge_cursor = mouse::Cursor::Available(edge);

    // Frame 1: drag to edge.
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(3, edge),
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        edge_cursor,
        &mut renderer,
        &mut messages,
    );

    let after_frame_1 = drag_endpoints(&messages).len();
    assert_eq!(after_frame_1, 1, "frame 1 must produce one drag");

    // Frame 2: pointer stationary at the same edge (autoscroll scenario).
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, edge),
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        edge_cursor,
        &mut renderer,
        &mut messages,
    );

    let total = drag_endpoints(&messages).len();
    assert_eq!(
        total, 2,
        "a second frame at the SAME edge point must republish so \
         autoscroll continues — got {total} total drags",
    );

    // Both published endpoints must be the same local point (from the
    // same cursor), proving the latch does not suppress repeated values.
    let endpoints = drag_endpoints(&messages);
    assert_eq!(
        endpoints[0], endpoints[1],
        "both frames must publish the same edge endpoint for autoscroll",
    );
}

// -----------------------------------------------------------------------
// Test 6: N CursorMoved in one batch with plain editor — at most one
//         Action::Drag, and only on RedrawRequested (not in the batch).
//
// Sabotage: publish `Action::Drag` directly from `Update::Drag` (remove
// the late-latch) — N messages would appear in the batch.
// -----------------------------------------------------------------------
#[test]
fn plain_editor_batch_produces_zero_drags_until_redraw() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let batch_end = Point::new(180.0, 40.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(10, Point::new(90.0, 40.0)),
        mouse::Cursor::Available(batch_end),
        &mut renderer,
        &mut messages,
    );

    assert!(
        drag_endpoints(&messages).is_empty(),
        "no drag in the input batch — got {:?}",
        drag_endpoints(&messages),
    );

    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(batch_end),
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(
        drags.len(),
        1,
        "exactly one Action::Drag on RedrawRequested — got {drags:?}",
    );
}

// -----------------------------------------------------------------------
// Test 7: a click that never moved.  The button is down and the hardware
//         cursor has since advanced, but no `CursorMoved` was dispatched,
//         so nothing is pending and the redraw must publish NOTHING.
//
// This is the negative control for the redraw-time cursor refresh: without
// the `pending_drag.is_some()` precondition, every frame after a plain
// caret click (blink included) would drag the selection to wherever the
// pointer happens to be.
//
// Sabotage: drop the `pending_drag.is_some()` precondition from the
// `RedrawRequested` refresh.  A drag is then published here.
// -----------------------------------------------------------------------
#[test]
fn click_without_motion_publishes_no_drag_on_redraw() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // The pointer has moved in the compositor, but the widget has been given
    // no `CursorMoved` event, so it holds no endpoint.
    let moved_away = Point::new(300.0, 150.0);
    for _ in 0..3 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            mouse::Cursor::Available(moved_away),
            &mut renderer,
            &mut messages,
        );
    }

    assert!(
        drag_endpoints(&messages).is_empty(),
        "a click with no dispatched motion must publish no drag — got {:?}",
        drag_endpoints(&messages),
    );
}

// -----------------------------------------------------------------------
// Test 8: after the release flush, later frames must publish nothing even
//         though the cursor keeps moving.  The drag is over.
//
// Sabotage: drop the `pending_drag.is_some()` precondition from the
// `RedrawRequested` refresh.  Post-release frames then keep publishing
// `Action::Drag`, extending a selection with no button held.
// -----------------------------------------------------------------------
#[test]
fn post_release_redraws_publish_no_drag() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // Drag, then release in the same batch: the release flush publishes the
    // final endpoint exactly once.
    let last = Point::new(200.0, 60.0);
    let mut batch = cursor_train(2, last);
    batch.push(Event::Mouse(mouse::Event::ButtonReleased(
        mouse::Button::Left,
    )));

    let _ = ui.update(
        &window,
        &waker,
        &batch,
        mouse::Cursor::Available(last),
        &mut renderer,
        &mut messages,
    );
    assert_eq!(
        drag_endpoints(&messages).len(),
        1,
        "release must flush exactly once — got {:?}",
        drag_endpoints(&messages),
    );

    // Now keep the pointer moving across frames with the button up.
    messages.clear();
    for step in 0..3 {
        let hover = Point::new(220.0 + step as f32 * 20.0, 70.0);
        let _ = ui.update(
            &window,
            &waker,
            &cursor_train(2, hover),
            mouse::Cursor::Available(hover),
            &mut renderer,
            &mut messages,
        );
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            mouse::Cursor::Available(hover),
            &mut renderer,
            &mut messages,
        );
    }

    assert!(
        drag_endpoints(&messages).is_empty(),
        "no drag may be published after release — got {:?}",
        drag_endpoints(&messages),
    );
}

// -----------------------------------------------------------------------
// Test 9: focus loss during an active drag.  The window lost focus while an
//         endpoint was latched and before any `ButtonReleased` arrived —
//         what an alt-tab, a compositor grab or a popup does mid-drag.
//         The drag is over:
//
//   * the latched endpoint must NOT be refreshed and published by the next
//     `RedrawRequested`, and
//   * later pointer motion must NOT re-arm the drag, because no button is
//     held any more.
//
// The positive control drives the identical event sequence WITHOUT the
// `Unfocused` event and asserts drags ARE published, so this absence claim
// cannot pass merely because the batches were incapable of producing one.
//
// Sabotage: remove `state.pending_drag = None; state.drag_click = None;`
// from the `Window::Unfocused` arm.  The redraw after unfocus publishes the
// latched endpoint and the later hover frames keep publishing drags with no
// button held.
// -----------------------------------------------------------------------
#[test]
fn unfocus_during_drag_publishes_no_drag_afterwards() {
    fn drive(unfocus: bool) -> Vec<Point> {
        let content = seed_content();
        let view = plain_view(&content);
        let press = Point::new(80.0, 40.0);

        let (mut ui, mut renderer) = setup_drag(view, press);
        let waker = Waker::noop();
        let window = Headless;
        let mut messages: iced_runtime::core::shell::Bus<Msg> =
            iced_runtime::core::shell::Bus::new();

        // Latch an endpoint: a live drag with nothing published yet.
        let latched = Point::new(150.0, 50.0);
        let _ = ui.update(
            &window,
            &waker,
            &cursor_train(2, latched),
            mouse::Cursor::Available(latched),
            &mut renderer,
            &mut messages,
        );
        assert!(
            drag_endpoints(&messages).is_empty(),
            "the endpoint must still be pending before the frame — got {:?}",
            drag_endpoints(&messages),
        );

        if unfocus {
            // No `ButtonReleased`: the window simply lost focus.
            let _ = ui.update(
                &window,
                &waker,
                &[Event::Window(window::Event::Unfocused)],
                mouse::Cursor::Available(latched),
                &mut renderer,
                &mut messages,
            );
        }

        // Measure only what happens from here on.
        messages.clear();

        // The frame that would flush the latched endpoint.
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            mouse::Cursor::Available(latched),
            &mut renderer,
            &mut messages,
        );

        // Then pointer motion with no button held.
        for step in 0..3 {
            let hover = Point::new(200.0 + step as f32 * 20.0, 70.0);
            let _ = ui.update(
                &window,
                &waker,
                &cursor_train(2, hover),
                mouse::Cursor::Available(hover),
                &mut renderer,
                &mut messages,
            );
            let _ = ui.update(
                &window,
                &waker,
                &[redraw_event()],
                mouse::Cursor::Available(hover),
                &mut renderer,
                &mut messages,
            );
        }

        drag_endpoints(&messages)
    }

    // Positive control: identical events, focus retained. The drag is alive,
    // so the flush frame and every later moved frame publish.
    let focused = drive(false);
    assert!(
        focused.len() >= 4,
        "control: a live drag must publish the flush frame and each later \
         moved frame — got {focused:?}",
    );

    let unfocused = drive(true);
    assert!(
        unfocused.is_empty(),
        "focus loss must end the drag: no endpoint may be flushed and no \
         later motion may re-arm it — got {unfocused:?}",
    );
}

// -----------------------------------------------------------------------
// Test 10: STATIONARY out-of-bounds cursor produces autoscroll continuation.
//
// A pointer sits past the editor's inner text bounds and never moves again;
// no further `CursorMoved` is dispatched. Every subsequent `RedrawRequested`
// with a live single-button drag must re-arm the pending endpoint from the
// current cursor and publish another `Action::Drag`, so cosmic-text's
// `shape_until_scroll` advances the viewport on every frame the pointer
// stays past the edge. Without this, autoscroll advances exactly once and
// stops -- the "stationary edge autoscroll" gap the reviewer flagged.
//
// Sabotage: remove the re-arm branch (the `pending_drag.is_none() &&
// matches!(state.drag_click, ...)` block) -- the second and later redraws
// then publish nothing and the vec![1,1,1] assertion reddens.
// -----------------------------------------------------------------------
#[test]
fn stationary_out_of_bounds_cursor_republishes_drag_for_autoscroll() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let edge = Point::new(120.0, SIZE.height + 40.0);
    let edge_cursor = mouse::Cursor::Available(edge);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, edge),
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    assert!(
        drag_endpoints(&messages).is_empty(),
        "the initial CursorMoved must not publish before the redraw"
    );

    let mut per_frame = Vec::new();
    for _ in 0..3 {
        messages.clear();
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            edge_cursor,
            &mut renderer,
            &mut messages,
        );
        per_frame.push(drag_endpoints(&messages).len());
    }

    assert_eq!(
        per_frame,
        vec![1, 1, 1],
        "stationary OOB pointer must publish one Action::Drag per frame -- \
         got {per_frame:?}"
    );

    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))],
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    messages.clear();
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    assert!(
        drag_endpoints(&messages).is_empty(),
        "post-release redraw must not re-arm autoscroll -- got {:?}",
        drag_endpoints(&messages),
    );
}

/// The direct-owner branch must request the follow-up frame that lets a
/// stationary out-of-bounds pointer continue autoscrolling. Driving another
/// redraw by hand proves only that the re-arm branch works; this assertion
/// observes the real `UserInterface` result that schedules that next frame.
///
/// Sabotage: remove `shell.request_redraw()` from the out-of-bounds flush. The
/// drag notification still arrives, but the request becomes the later caret
/// blink deadline instead of `NextFrame`, and this gate reddens.
#[test]
fn direct_out_of_bounds_drag_flush_requests_the_next_frame() {
    let content = seed_content();
    let view = direct_drag_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    let edge = Point::new(120.0, SIZE.height + 40.0);
    let edge_cursor = mouse::Cursor::Available(edge);

    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, edge),
        edge_cursor,
        &mut renderer,
        &mut messages,
    );
    messages.clear();

    let (state, _) = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        edge_cursor,
        &mut renderer,
        &mut messages,
    );

    assert_eq!(
        drag_updates(&messages).len(),
        1,
        "direct mode must perform and notify exactly once on this frame"
    );
    assert!(
        drag_endpoints(&messages).is_empty(),
        "direct mode must not publish an Action::Drag for the app to replay"
    );
    assert_eq!(
        state.redraw_request(),
        window::RedrawRequest::NextFrame,
        "stationary edge continuation must schedule the next frame immediately"
    );
}

// -----------------------------------------------------------------------
// Test 11: STATIONARY in-bounds cursor after a flush must NOT re-arm.
//
// The re-arm branch is scoped to out-of-bounds only. An in-bounds click
// that never moved further must publish exactly one flush and then stop,
// so caret blink frames do not extend the selection every 500 ms.
//
// Sabotage: drop the OOB precondition on the re-arm branch -- every
// redraw then republishes at the current in-bounds cursor and this
// reddens.
// -----------------------------------------------------------------------
#[test]
fn stationary_in_bounds_cursor_does_not_republish_drag() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(80.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let inside = Point::new(200.0, 100.0);
    let inside_cursor = mouse::Cursor::Available(inside);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, inside),
        inside_cursor,
        &mut renderer,
        &mut messages,
    );
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        inside_cursor,
        &mut renderer,
        &mut messages,
    );
    let after_first = drag_endpoints(&messages).len();
    assert_eq!(after_first, 1, "first redraw flushes the pending endpoint");

    for _ in 0..2 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            inside_cursor,
            &mut renderer,
            &mut messages,
        );
    }
    let total = drag_endpoints(&messages).len();
    assert_eq!(
        total, 1,
        "in-bounds stationary cursor must publish exactly once -- got \
         {total} total drags across three redraws",
    );
}

// -----------------------------------------------------------------------
// Test 12: DIRECT drag mode with an active drag whose window loses focus.
//
// This mirrors the default-mode `unfocus_during_drag_publishes_no_drag_
// afterwards`, but proves the SAME cancellation applies when the widget
// owns the drag: no `EditorDragUpdated` may be flushed after focus loss,
// and later pointer motion may not re-arm the direct path.
//
// Sabotage: remove `state.pending_drag = None; state.drag_click = None;`
// from the `Window::Unfocused` arm. The direct-mode redraw after unfocus
// would then flush the latched endpoint AND perform the drag against
// `Content`; the assertion below sees at least one `DragUpdate` and
// reddens.
// -----------------------------------------------------------------------
#[test]
fn direct_drag_focus_loss_ends_drag_without_flushing() {
    fn drive(unfocus: bool) -> Vec<text_editor::Cursor> {
        let mut content = seed_content();
        let _ = content.perform(text_editor::Action::Click(
            Point::new(40.0, 20.0),
            mouse::click::Kind::Single,
        ));

        let view = direct_drag_view(&content);
        let press = Point::new(40.0, 20.0);
        let (mut ui, mut renderer) = setup_drag(view, press);
        let waker = Waker::noop();
        let window = Headless;
        let mut messages: iced_runtime::core::shell::Bus<Msg> =
            iced_runtime::core::shell::Bus::new();

        let latched = Point::new(180.0, 50.0);
        let _ = ui.update(
            &window,
            &waker,
            &cursor_train(2, latched),
            mouse::Cursor::Available(latched),
            &mut renderer,
            &mut messages,
        );

        if unfocus {
            let _ = ui.update(
                &window,
                &waker,
                &[Event::Window(window::Event::Unfocused)],
                mouse::Cursor::Available(latched),
                &mut renderer,
                &mut messages,
            );
        }

        messages.clear();

        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            mouse::Cursor::Available(latched),
            &mut renderer,
            &mut messages,
        );
        for step in 0..3 {
            let hover = Point::new(220.0 + step as f32 * 20.0, 70.0);
            let _ = ui.update(
                &window,
                &waker,
                &cursor_train(2, hover),
                mouse::Cursor::Available(hover),
                &mut renderer,
                &mut messages,
            );
            let _ = ui.update(
                &window,
                &waker,
                &[redraw_event()],
                mouse::Cursor::Available(hover),
                &mut renderer,
                &mut messages,
            );
        }

        drop(ui);
        drag_updates(&messages)
    }

    let focused_updates = drive(false);
    assert!(
        focused_updates.len() >= 4,
        "control: live direct drag must notify on the flush frame and each \
         later moved frame -- got {focused_updates:?}"
    );

    let unfocused_updates = drive(true);
    assert!(
        unfocused_updates.is_empty(),
        "focus loss must end the direct drag: no notification may be \
         flushed and no later motion may re-arm it -- got \
         {unfocused_updates:?}"
    );
}

// -----------------------------------------------------------------------
// Test 13: DIRECT drag under a scrolled parent. Same coordinate seam as
// `scrolled_editor_late_latch_survives_translation`, but the widget owns
// the drag. `Content` must have moved before the app consumes the
// `DragUpdate` notification, even at nonzero scroll offset.
//
// Sabotage: change `perform_pointer_action` to publish an action instead
// of mutating `Content`. The final selection check reddens because
// `Content` is still at the press position.
// -----------------------------------------------------------------------
#[test]
fn direct_drag_scrolled_translation_survives_and_mutates_content() {
    let mut content = seed_content();
    let _ = content.perform(text_editor::Action::Click(
        Point::new(40.0, 20.0),
        mouse::click::Kind::Single,
    ));
    let before = content.cursor();

    let view = scrollable::Scrollable::with_direction(
        text_editor::TextEditor::new(&content)
            .font(Font::MONOSPACE)
            .on_action(Msg::Edit)
            .on_drag_update(Msg::DragUpdate),
        scrollable::Direction::Vertical(scrollable::Scrollbar::new()),
    )
    .initial_offset(AbsoluteOffset {
        x: 0.0,
        y: SCROLL_OFFSET_Y,
    })
    .into();

    let press = Point::new(80.0, 40.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let endpoint = Point::new(200.0, 80.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(6, Point::new(90.0, 40.0)),
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(endpoint),
        &mut renderer,
        &mut messages,
    );

    let updates = drag_updates(&messages);
    assert_eq!(
        updates.len(),
        1,
        "one redraw under a scrollable must produce one notification: \
         {updates:?}"
    );
    drop(ui);
    let after = content.cursor();
    assert_ne!(
        after.position, before.position,
        "direct drag under a scrolled parent must mutate `Content` before \
         the notification reaches the app"
    );
    assert_eq!(
        updates[0], after,
        "notification cursor must equal `Content`'s cursor after the \
         same-frame perform"
    );
    assert!(
        content.selection_range().is_some(),
        "a live selection must exist after a same-frame drag"
    );
}

// -----------------------------------------------------------------------
// Test: held-edge autoscroll continuation (#1802).
//
// A single-button drag whose pointer sits stationary PAST the editor's inner
// bottom must advance the viewport and extend the selection cursor on each
// redraw — bounded per frame — and stop exactly at document end. This is the
// stationary-edge stall proven in the product: before the fix, a Drag whose
// `y` is beyond the laid-out viewport hit-clamps to the last VISIBLE row, so
// `shape_until_cursor` reveals nothing and the viewport never advances.
//
// Uses the DIRECT (`on_drag_update`) path so the drag is performed inside the
// widget update — the exact path the Text app runs — and the consequence is
// read from the public `scroll_metrics()`/`cursor()` API, not a counter.
//
// Sabotage (in graphics/src/text/editor.rs `Action::Drag`): perform the Drag
// at the raw past-bottom `position` without the bounded overshoot scroll. The
// hit re-clamps to the last visible row, so `first_line`/`offset.y` never
// advance and `cursor.position.line` stalls — both assertions below redden.
#[test]
fn held_past_bottom_edge_autoscrolls_and_extends_selection() {
    let content = seed_content(); // 200 lines
    let view = direct_drag_view(&content);
    // Press well inside, near the bottom of the 400x200 editor.
    let press = Point::new(50.0, 170.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let start = content.scroll_metrics();
    let start_first_line = start.first_line;

    // The pointer now sits stationary PAST the inner bottom edge (y beyond the
    // 200px viewport). CursorMoved past bounds yields no Drag on its own
    // (`position_in` is None there); the RedrawRequested re-arm is what must
    // keep autoscrolling.
    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );

    // Drive a handful of held redraws and record the per-frame advance.
    let mut prev_first_line = start_first_line;
    let mut advances = Vec::new();
    for _ in 0..8 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            past_bottom,
            &mut renderer,
            &mut messages,
        );
        let m = content.scroll_metrics();
        advances.push(m.first_line as i64 - prev_first_line as i64);
        prev_first_line = m.first_line;
    }

    let mid = content.scroll_metrics();
    assert!(
        mid.first_line > start_first_line,
        "held past-bottom drag must advance the viewport: first_line \
         {} -> {} (per-frame advances {:?})",
        start_first_line,
        mid.first_line,
        advances,
    );
    // Bounded per frame: never jump more than the MAX_AUTOSCROLL_LINES cap,
    // and while not yet at EOF at least one line per held frame.
    for a in &advances {
        assert!(
            (0..=4).contains(a),
            "per-frame autoscroll must be bounded to [0,4] lines, got {a} \
             (advances {advances:?})",
        );
    }
    assert!(
        advances.iter().any(|a| *a >= 1),
        "at least one held frame must scroll while not at EOF: {advances:?}",
    );

    // The selection cursor followed the viewport down (selection extended),
    // not stuck at the press line.
    let cur = content.cursor();
    assert!(
        cur.position.line > start_first_line + 1,
        "selection cursor must extend past the originally visible window: \
         line {} (start_first_line {})",
        cur.position.line,
        start_first_line,
    );
    assert!(
        content.selection_range().is_some(),
        "a live multi-line selection must exist during edge autoscroll",
    );

    // Document-end clamp: drive far more redraws than there are lines and
    // confirm the viewport stops at EOF without overrunning the buffer.
    for _ in 0..400 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            past_bottom,
            &mut renderer,
            &mut messages,
        );
    }
    let end = content.scroll_metrics();
    let line_count = content.line_count();
    assert!(
        end.first_line >= mid.first_line,
        "viewport must not scroll backwards while held past the bottom: \
         {} -> {}",
        mid.first_line,
        end.first_line,
    );
    assert!(
        end.first_line < line_count,
        "document-end clamp: first_line {} must stay within the {} lines",
        end.first_line,
        line_count,
    );
    let end_cursor = content.cursor();
    assert!(
        end_cursor.position.line <= line_count.saturating_sub(1),
        "cursor must clamp at the last line, got {} of {} lines",
        end_cursor.position.line,
        line_count,
    );
    // Holding at EOF must be idle: one more redraw scrolls nothing further.
    let before_idle = content.scroll_metrics().first_line;
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        past_bottom,
        &mut renderer,
        &mut messages,
    );
    assert_eq!(
        content.scroll_metrics().first_line,
        before_idle,
        "at EOF the held drag must be idle (document-end clamp)",
    );

    drop(ui);
}

// -----------------------------------------------------------------------
// Test: an IN-BOUNDS held drag must NOT autoscroll (negative control).
//
// The overshoot path must trigger only past the inner edge; a pointer parked
// inside the viewport, redrawn repeatedly, must leave the viewport where it
// is. This guards the fix from degenerating into "always scroll".
// -----------------------------------------------------------------------
#[test]
fn held_in_bounds_drag_does_not_autoscroll() {
    let content = seed_content();
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let start_first_line = content.scroll_metrics().first_line;

    // Pointer parked WELL inside the 400x200 viewport.
    let inside = mouse::Cursor::Available(Point::new(120.0, 90.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(120.0, 90.0),
        })],
        inside,
        &mut renderer,
        &mut messages,
    );
    for _ in 0..12 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            inside,
            &mut renderer,
            &mut messages,
        );
    }

    assert_eq!(
        content.scroll_metrics().first_line,
        start_first_line,
        "an in-bounds held drag must not autoscroll the viewport",
    );
    drop(ui);
}

// -----------------------------------------------------------------------
// Test: held-edge autoscroll PAST THE TOP — reverse selection extension and
// the document-start clamp (#1802).
//
// The bottom-edge test above only exercises `dir = +1`. The `position.y < 0.0`
// branch owns three separate behaviours that nothing else covers: the upward
// bounded scroll, the `hit_y = row_reveal_margin * 0.5` clamp onto the freshly
// revealed FIRST visible row, and cosmic-text's document-start clamp.
//
// The gesture is one continuous held drag: press mid-viewport, hold past the
// bottom to travel down (setup, asserted loudly so a broken setup cannot make
// the real assertions vacuous), then hold past the top without releasing. The
// selection endpoint must walk back up, cross ABOVE the anchor (a truly
// reversed selection, not a shrinking one), land on line 0 and stay there.
//
// `Update::Click` only PUBLISHES `Action::Click`; the direct drag path never
// performs it on `Content`, so an untouched fixture would anchor the selection
// at line 0 and "drag up" could only ever shrink it back to nothing. The
// anchor is therefore seeded mid-document through the public `move_to` API
// (cursor only, no selection) so the reversal is a real crossing.
//
// Sabotage (in graphics/src/text/editor.rs `Action::Drag`): flip the upward
// branch to `(0.0, 0.0_f32)` — the down phase still passes, the viewport stops
// reversing and `first_line` never returns to 0. Sabotaging `hit_y` for
// `dir < 0` to `position.y` instead re-clamps the hit to the OLD first visible
// row, so the cursor stops following and the crossing assertion reddens.
#[test]
fn held_past_top_edge_reverses_selection_and_clamps_at_bof() {
    const ANCHOR_LINE: usize = 40;

    let mut content = seed_content(); // 200 lines
    content.move_to(text_editor::Cursor {
        position: text_editor::Position {
            line: ANCHOR_LINE,
            index: 0,
        },
        selection: None,
    });
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 170.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // The first Drag anchors the selection at wherever the cursor already is;
    // every later Drag moves only the far endpoint, so this is the line the
    // endpoint must cross to prove reversal.
    let anchor_line = content.cursor().position.line;
    assert_eq!(
        anchor_line, ANCHOR_LINE,
        "setup precondition: the seeded anchor must survive the press (a press \
         that started performing Click on Content would silently move it)",
    );

    // --- setup phase: travel DOWN so there is somewhere to scroll back up ---
    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );
    for _ in 0..30 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            past_bottom,
            &mut renderer,
            &mut messages,
        );
    }
    let descended = content.scroll_metrics().first_line;
    assert!(
        descended > 0,
        "setup precondition: the down phase must leave the viewport scrolled \
         (first_line {descended}); with no travelled distance the upward \
         assertions below would be vacuous",
    );
    let descended_cursor = content.cursor().position.line;
    assert!(
        descended_cursor > anchor_line,
        "setup precondition: the endpoint must be BELOW the anchor before \
         reversing (cursor {descended_cursor}, anchor {anchor_line})",
    );

    // --- reverse phase: same held drag, pointer now past the INNER TOP ---
    // Local y = 2 - padding.top(5) = -3, i.e. three pixels above the text
    // area, so `overshoot` is under one row and the step is the minimum one.
    let past_top = mouse::Cursor::Available(Point::new(50.0, 2.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 2.0),
        })],
        past_top,
        &mut renderer,
        &mut messages,
    );

    let mut prev = descended;
    let mut reversals = Vec::new();
    for _ in 0..8 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            past_top,
            &mut renderer,
            &mut messages,
        );
        let now = content.scroll_metrics().first_line;
        reversals.push(prev as i64 - now as i64);
        prev = now;
    }
    assert!(
        reversals.iter().all(|d| *d >= 0),
        "a drag held past the top must never scroll further DOWN: {reversals:?}",
    );
    assert!(
        reversals.iter().all(|d| *d <= 4),
        "upward autoscroll must respect the same per-frame cap: {reversals:?}",
    );
    assert!(
        content.scroll_metrics().first_line < descended,
        "held past-top drag must reverse the viewport: {} -> {} ({reversals:?})",
        descended,
        content.scroll_metrics().first_line,
    );
    assert!(
        content.cursor().position.line < descended_cursor,
        "the selection endpoint must follow the reversing viewport: {} -> {}",
        descended_cursor,
        content.cursor().position.line,
    );

    // --- document-start clamp: hold long past BOF ---
    for _ in 0..400 {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            past_top,
            &mut renderer,
            &mut messages,
        );
    }
    assert_eq!(
        content.scroll_metrics().first_line,
        0,
        "document-start clamp: a drag held past the top must settle at line 0",
    );
    let bof_cursor = content.cursor().position.line;
    assert_eq!(
        bof_cursor, 0,
        "at BOF the endpoint must sit on the first line, got {bof_cursor}",
    );
    assert!(
        bof_cursor < anchor_line,
        "reverse extension: the endpoint must end ABOVE the press anchor \
         (endpoint {bof_cursor}, anchor {anchor_line}) — a selection that only \
         shrank back to the anchor never exercised the reversed direction",
    );
    assert!(
        content.selection_range().is_some(),
        "a live reversed selection must exist at BOF",
    );

    // Holding at BOF must be idle: one more redraw scrolls nothing further.
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        past_top,
        &mut renderer,
        &mut messages,
    );
    assert_eq!(
        content.scroll_metrics().first_line,
        0,
        "at BOF the held drag must be idle (document-start clamp)",
    );

    drop(ui);
}

/// Drive one held past-bottom drag at `cursor_y` (window coordinates) for
/// `frames` redraws and return the per-frame `first_line` advance.
fn held_bottom_edge_advances(cursor_y: f32, frames: usize) -> Vec<i64> {
    let content = seed_content();
    let view = direct_drag_view(&content);
    let (mut ui, mut renderer) = setup_drag(view, Point::new(50.0, 170.0));
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let held = mouse::Cursor::Available(Point::new(50.0, cursor_y));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, cursor_y),
        })],
        held,
        &mut renderer,
        &mut messages,
    );

    let mut prev = content.scroll_metrics().first_line;
    let mut advances = Vec::new();
    for _ in 0..frames {
        let _ = ui.update(
            &window,
            &waker,
            &[redraw_event()],
            held,
            &mut renderer,
            &mut messages,
        );
        let now = content.scroll_metrics().first_line;
        advances.push(now as i64 - prev as i64);
        prev = now;
    }
    drop(ui);
    advances
}

// -----------------------------------------------------------------------
// Test: the autoscroll step GROWS with overshoot depth and SATURATES at the
// documented cap (#1802).
//
// `lines = (1 + floor(overshoot / row_height)).min(MAX_AUTOSCROLL_LINES)`. The
// bottom-edge test above holds the pointer far outside, so it saturates on
// every frame and would pass unchanged against a hard-coded one-line step or
// against no cap at all. This discriminates all three:
//
//   * shallow (1 px past the inner bottom, i.e. well under one row) must step
//     exactly ONE line per frame — reddens if the step is hard-coded to the cap
//     or scaled by raw pixels instead of rows;
//   * deep (205 px past, ~9 rows for the 16 px monospace default) must step
//     exactly FOUR — reddens if the step is hard-coded to one line, and reddens
//     if `MAX_AUTOSCROLL_LINES` is raised or removed;
//   * and deep must exceed shallow, which is the growth claim itself.
//
// The 200-line fixture is far longer than 8 frames x 4 lines, so neither run
// reaches the EOF clamp and every frame reports its true unclamped step.
//
// Sabotage: replace `lines` with the constant `1.0` (deep reddens, shallow
// still green); replace it with `MAX_AUTOSCROLL_LINES` (shallow reddens, deep
// still green); raise the cap to 8.0 (deep reddens at 8, shallow still green).
#[test]
fn held_edge_autoscroll_step_grows_with_overshoot_and_saturates() {
    // Inner text bottom is 200 - padding.y(10) = 190 in editor-local space,
    // which is window y = 195. One pixel past that is a sub-row overshoot.
    let shallow = held_bottom_edge_advances(196.0, 8);
    // 400 - 5 = 395 local, i.e. 205 px (~9 rows) past the inner bottom, which
    // asks for 10 lines and must be clamped to the cap.
    let deep = held_bottom_edge_advances(400.0, 8);

    assert!(
        shallow.iter().all(|a| *a == 1),
        "a sub-row overshoot must advance exactly one line per frame, got \
         {shallow:?}",
    );
    assert!(
        deep.iter().all(|a| *a == 4),
        "an overshoot asking for ~10 lines must saturate at the \
         MAX_AUTOSCROLL_LINES cap of 4 per frame, got {deep:?}",
    );
    assert!(
        deep.iter().sum::<i64>() > shallow.iter().sum::<i64>(),
        "the autoscroll step must GROW with overshoot depth: deep {deep:?} vs \
         shallow {shallow:?}",
    );
}

// -----------------------------------------------------------------------
// enzUI-ICED #1862 — held-edge autoscroll loop termination at BOF/EOF and
// suppression on horizontal-only overflow.
//
// Parent #1802 keeps the autoscroll loop alive across frames; that gate only
// proves that `first_line` stops advancing. This gate proves the redraw loop
// itself terminates once cosmic-text's document clamp is reached, and that
// horizontal-only overshoot (which this editor does not autoscroll) never
// starts the loop in the first place.
//
// All assertions run through the real `UserInterface::update` dispatch and
// read `state.redraw_request()` — the exact signal `iced_winit` schedules
// the next frame on.

use iced_runtime::core::window::RedrawRequest;

/// Advance one `UserInterface::update` pass at `held` (a `RedrawRequested`)
/// and return the resulting `RedrawRequest`.
fn redraw_and_get_request(
    ui: &mut UserInterface<'_, Msg, Theme, Renderer>,
    renderer: &mut Renderer,
    held: mouse::Cursor,
    messages: &mut iced_runtime::core::shell::Bus<Msg>,
) -> RedrawRequest {
    let waker = Waker::noop();
    let window = Headless;
    let (state, _) = ui.update(&window, &waker, &[redraw_event()], held, renderer, messages);
    state.redraw_request()
}

/// Direct mode. Hold past the bottom edge on a document long enough that
/// the viewport can advance repeatedly, then keep holding once cosmic-text's
/// EOF clamp has settled. The FINAL settling frames may still schedule
/// `NextFrame`, but every LATER redraw at the same clamped state must not —
/// otherwise the widget runs the 60 fps redraw / re-arm / shape loop forever
/// with no visible progress.
///
/// Sabotage: remove the advance gate (unconditionally
/// `shell.request_redraw()` in the OOB flush block). Every post-clamp frame
/// then returns `NextFrame` and the `!= NextFrame` assertion reddens.
#[test]
fn held_past_bottom_at_eof_stops_scheduling_next_frame() {
    let content = seed_content(); // 200 lines
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 170.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );

    // Drive well past the length of the 200-line document (each frame caps
    // at MAX_AUTOSCROLL_LINES = 4 lines, so 400 frames is `4x` the document
    // — cosmic-text is clamped at EOF long before the loop ends).
    let mut requests = Vec::with_capacity(400);
    for _ in 0..400 {
        requests.push(redraw_and_get_request(
            &mut ui,
            &mut renderer,
            past_bottom,
            &mut messages,
        ));
    }

    // At EOF the scroll offset and cursor are frozen. Take the tail of the
    // run — long enough to have crossed EOF many times over — and assert
    // `NextFrame` is absent.
    let tail = &requests[requests.len() - 20..];
    assert!(
        tail.iter().all(|r| *r != RedrawRequest::NextFrame),
        "held past EOF must stop requesting the next frame: {tail:?}",
    );

    // The idle request is the caret blink deadline (`At(_)`) or `Wait`, not
    // `NextFrame`. At least one of the tail frames must be `At(_)` — the
    // caret is still blinking. That excludes a bug where the widget stopped
    // scheduling anything at all.
    assert!(
        tail.iter().any(|r| matches!(r, RedrawRequest::At(_))),
        "the caret blink deadline must still schedule (`At(_)`); the widget \
         must not fall silent at EOF: {tail:?}",
    );

    drop(ui);
}

/// Direct mode, symmetric BOF proof. Reversed selection setup mirrors
/// `held_past_top_edge_reverses_selection_and_clamps_at_bof`: seed an anchor
/// mid-document so the reverse phase has somewhere to travel, then hold past
/// the top edge and confirm the redraw loop terminates once the viewport is
/// clamped at line 0.
///
/// Sabotage: same as EOF — remove the advance gate. Tail frames revert to
/// `NextFrame`.
#[test]
fn held_past_top_at_bof_stops_scheduling_next_frame() {
    let mut content = seed_content();
    content.move_to(text_editor::Cursor {
        position: text_editor::Position { line: 40, index: 0 },
        selection: None,
    });
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 170.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // Descend first so BOF is a real destination.
    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );
    for _ in 0..30 {
        let _ = redraw_and_get_request(&mut ui, &mut renderer, past_bottom, &mut messages);
    }

    // Reverse: hold past the inner top edge until BOF clamps.
    let past_top = mouse::Cursor::Available(Point::new(50.0, 2.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 2.0),
        })],
        past_top,
        &mut renderer,
        &mut messages,
    );

    let mut requests = Vec::with_capacity(400);
    for _ in 0..400 {
        requests.push(redraw_and_get_request(
            &mut ui,
            &mut renderer,
            past_top,
            &mut messages,
        ));
    }

    let tail = &requests[requests.len() - 20..];
    assert!(
        tail.iter().all(|r| *r != RedrawRequest::NextFrame),
        "held past BOF must stop requesting the next frame: {tail:?}",
    );
    assert!(
        tail.iter().any(|r| matches!(r, RedrawRequest::At(_))),
        "caret blink deadline must still schedule at BOF: {tail:?}",
    );

    drop(ui);
}

/// After the BOF/EOF clamp stops the loop, moving the still-held pointer
/// back toward the OPPOSITE edge must resume continuation. A fresh
/// `CursorMoved` re-arms `pending_drag` and the flush must once again
/// schedule `NextFrame` when the drag advances.
///
/// Sabotage: keep the snapshot from a previous edge visit around — drop
/// `state.edge_autoscroll_last = None` on the in-bounds flush branch. A
/// reversal that transitions through an in-bounds pending endpoint would
/// then compare against stale state on the far edge and misclassify
/// advance/no-advance.
#[test]
fn reversal_after_edge_clamp_resumes_continuation() {
    let mut content = seed_content();
    // Anchor mid-document so a reversal past the top has somewhere to go.
    content.move_to(text_editor::Cursor {
        position: text_editor::Position { line: 60, index: 0 },
        selection: None,
    });
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 170.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // Descend to EOF and let the loop settle.
    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );
    for _ in 0..400 {
        let _ = redraw_and_get_request(&mut ui, &mut renderer, past_bottom, &mut messages);
    }
    // Sanity: we are indeed clamped (not scheduling next frame at bottom).
    let settled = redraw_and_get_request(&mut ui, &mut renderer, past_bottom, &mut messages);
    assert_ne!(
        settled,
        RedrawRequest::NextFrame,
        "precondition: EOF must have clamped the loop before reversal",
    );

    // Move the still-held cursor back toward the OPPOSITE edge.
    let past_top = mouse::Cursor::Available(Point::new(50.0, 2.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 2.0),
        })],
        past_top,
        &mut renderer,
        &mut messages,
    );
    let after_reversal = redraw_and_get_request(&mut ui, &mut renderer, past_top, &mut messages);
    assert_eq!(
        after_reversal,
        RedrawRequest::NextFrame,
        "moving the still-held cursor toward the opposite edge must resume \
         held-edge continuation",
    );

    drop(ui);
}

/// Horizontal-only overshoot (pointer past the LEFT or RIGHT inner edge but
/// vertically inside the editor) must not start the vertical autoscroll
/// redraw loop. This editor has no horizontal held-edge autoscroll, so
/// scheduling `NextFrame` for a horizontal-only overshoot is pure spin.
///
/// Sabotage: revert the re-arm predicate to the pre-#1862
/// `local.x < 0 || local.y < 0 || local.x > W || local.y > H`. The
/// horizontal-only frames then re-arm every redraw and request `NextFrame`,
/// and this gate reddens.
#[test]
fn horizontal_only_overshoot_does_not_schedule_next_frame() {
    let content = seed_content();
    let view = direct_drag_view(&content);
    // Press inside; the flush frame arms drag_click as Single so the
    // re-arm block is a live candidate.
    let press = Point::new(80.0, 100.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    // Pointer past the RIGHT edge, vertically INSIDE the viewport.
    let past_right = mouse::Cursor::Available(Point::new(SIZE.width + 40.0, 100.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(SIZE.width + 40.0, 100.0),
        })],
        past_right,
        &mut renderer,
        &mut messages,
    );
    // First redraw flushes the pending endpoint (a real drag point outside
    // the widget). What must NOT follow is a re-armed autoscroll redraw loop.
    let _flush = redraw_and_get_request(&mut ui, &mut renderer, past_right, &mut messages);
    messages.clear();

    // Now the pending is empty and the pointer is stationary. Every
    // further redraw must NOT re-arm and must NOT return `NextFrame`.
    let mut later = Vec::with_capacity(20);
    for _ in 0..20 {
        later.push(redraw_and_get_request(
            &mut ui,
            &mut renderer,
            past_right,
            &mut messages,
        ));
    }
    assert!(
        later.iter().all(|r| *r != RedrawRequest::NextFrame),
        "horizontal-only overshoot must not spin the vertical autoscroll \
         redraw loop: {later:?}",
    );

    // And no per-frame drag notifications leak out of the stationary tail.
    let drags_after_flush = drag_updates(&messages);
    assert!(
        drags_after_flush.is_empty(),
        "horizontal-only stationary pointer must not publish a per-frame \
         drag stream: {drags_after_flush:?}",
    );

    drop(ui);
}

/// The held-edge continuation must survive one no-op-perform frame. Direct
/// mode's `advanced_this_frame` is the first advance signal; when that is
/// false (e.g. the perform found no change this frame yet may on the next),
/// `advanced_since_last_flush` must still allow one more chance by seeing
/// the previous snapshot differ from this frame's pre-perform state. This
/// guards against the naive design that only checked in-frame advance and
/// would stop the loop one frame too early during a legal transient.
///
/// Sabotage: drop the `advanced_since_last_flush` branch. The reversal test
/// stays green (direct-mode advance catches it), but a mid-scroll fixture
/// where post-perform equals pre-perform for a single settling frame would
/// stop prematurely.
#[test]
fn held_edge_continuation_survives_first_no_advance_frame() {
    // Drive one deep held past-bottom drag and verify that the widget
    // scheduled `NextFrame` on every frame BEFORE the viewport reached
    // EOF. If the advance gate is over-aggressive it will cut the loop
    // short before the document has finished scrolling.
    let content = seed_content();
    let view = direct_drag_view(&content);
    let press = Point::new(50.0, 170.0);
    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let past_bottom = mouse::Cursor::Available(Point::new(50.0, 320.0));
    let _ = ui.update(
        &window,
        &waker,
        &[Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 320.0),
        })],
        past_bottom,
        &mut renderer,
        &mut messages,
    );

    // While the viewport is still moving, `NextFrame` must be scheduled.
    // The 200-line fixture scrolls up to 4 lines per frame, so 20 frames
    // is well within the pre-EOF advance window.
    let mut last_first_line = content.scroll_metrics().first_line;
    for i in 0..20 {
        let request = redraw_and_get_request(&mut ui, &mut renderer, past_bottom, &mut messages);
        let first_line = content.scroll_metrics().first_line;
        assert!(
            first_line > last_first_line,
            "frame {i}: continuation must keep scrolling before EOF \
             ({last_first_line} -> {first_line})",
        );
        assert_eq!(
            request,
            RedrawRequest::NextFrame,
            "frame {i}: continuation must schedule NextFrame while the \
             viewport is still advancing (first_line {first_line})",
        );
        last_first_line = first_line;
    }

    drop(ui);
}

// -----------------------------------------------------------------------
// Upstream iced #3455: a drag keeps extending the selection when the pointer
// leaves the editor sideways.
//
// A `CursorMoved` past the editor's right edge (vertically inside) used to
// resolve through `cursor.position_in(bounds)`, which is `None` outside the
// widget, so the move was dropped. The vertical re-arm on redraw does not
// cover horizontal overshoot (#1862), so the selection froze where the
// pointer left, instead of reaching the end of the line.
//
// Sabotage: resolve the drag with `position_in(bounds)` again — no drag is
// published.
// -----------------------------------------------------------------------
#[test]
fn a_drag_past_the_side_edge_still_extends_the_selection() {
    let content = seed_content();
    let view = plain_view(&content);
    let press = Point::new(20.0, 40.0);

    let (mut ui, mut renderer) = setup_drag(view, press);
    let waker = Waker::noop();
    let window = Headless;
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();

    let outside = Point::new(SIZE.width + 50.0, 40.0);
    let _ = ui.update(
        &window,
        &waker,
        &cursor_train(1, outside),
        mouse::Cursor::Available(outside),
        &mut renderer,
        &mut messages,
    );
    let _ = ui.update(
        &window,
        &waker,
        &[redraw_event()],
        mouse::Cursor::Available(outside),
        &mut renderer,
        &mut messages,
    );

    let drags = drag_endpoints(&messages);
    assert_eq!(drags.len(), 1, "the outside move is a drag — got {drags:?}");
    assert!(
        drags[0].x > SIZE.width,
        "the endpoint is where the pointer is, past the edge — got {drags:?}"
    );
}

// -----------------------------------------------------------------------
// Upstream iced #3442: touch drives the editor like the left button. A
// finger pressed on the text and moved publishes a click, then exactly one
// drag at the next redraw, like the mouse path.
//
// Sabotage: drop the touch-to-mouse mapping in `Update::from_event` — the
// finger publishes nothing.
// -----------------------------------------------------------------------
#[test]
fn a_finger_selects_like_the_left_button() {
    use iced_runtime::core::touch;

    let content = seed_content();
    let mut renderer = new_renderer();
    let waker = Waker::noop();
    let window = Headless;
    let mut ui = UserInterface::build(
        plain_view(&content),
        SIZE,
        user_interface::Cache::default(),
        &mut renderer,
    );
    let mut messages: iced_runtime::core::shell::Bus<Msg> = iced_runtime::core::shell::Bus::new();
    let finger = touch::Finger(1);
    let press = Point::new(20.0, 40.0);
    let moved = Point::new(120.0, 40.0);

    let _ = ui.update(
        &window,
        &waker,
        &[Event::Touch(touch::Event::FingerPressed {
            id: finger,
            position: press,
        })],
        mouse::Cursor::Available(press),
        &mut renderer,
        &mut messages,
    );
    assert!(
        messages
            .messages()
            .iter()
            .any(|message| matches!(message, Msg::Edit(text_editor::Action::Click(_, _)))),
        "the press is a click — got {messages:?}"
    );

    for batch in [
        vec![Event::Touch(touch::Event::FingerMoved {
            id: finger,
            position: moved,
        })],
        vec![redraw_event()],
    ] {
        let _ = ui.update(
            &window,
            &waker,
            &batch,
            mouse::Cursor::Available(moved),
            &mut renderer,
            &mut messages,
        );
    }

    let drags = drag_endpoints(&messages);
    assert_eq!(drags.len(), 1, "one drag at the redraw — got {drags:?}");
}
