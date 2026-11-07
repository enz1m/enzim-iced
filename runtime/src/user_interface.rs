// Modified by enzUI contributors. See FORK.md.
//! Implement your own event loop to drive a user interface.
use crate::core::event::{self, Event};
use crate::core::layout;
use crate::core::mouse;
use crate::core::overlay;
use crate::core::renderer;
use crate::core::shell;
use crate::core::touch;
use crate::core::widget;
use crate::core::window;
use crate::core::{
    Clipboard, Element, InputMethod, Layout, Rectangle, Shell, Size, Vector, Window,
};

/// Presented geometry could not be reconciled with the native widget state.
///
/// Restore native layout before routing input through the rejected geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("presented geometry could not be reconciled with native widget state")]
pub struct PresentedLayoutRefused;

/// A set of interactive graphical elements with a specific [`Layout`].
///
/// It can be updated and drawn.
///
/// Iced tries to avoid dictating how to write your event loop. You are in
/// charge of using this type in your system in any way you want.
///
/// # Example
/// The [`integration`] example uses a [`UserInterface`] to integrate Iced in an
/// existing graphical application.
///
/// [`integration`]: https://github.com/iced-rs/iced/tree/master/examples/integration
pub struct UserInterface<'a, Message, Theme, Renderer> {
    root: Element<'a, Message, Theme, Renderer>,
    state: widget::Tree,
    overlay: Option<mouse::Interaction>,
    /// The overlays are drawn into a popup surface by
    /// [`draw_overlay`](Self::draw_overlay), not by [`draw`](Self::draw)
    /// (enzICED).
    overlay_detached: bool,
    bounds: Size,
    /// Whether one presented frame still owes an unpruned dispatch because the
    /// geometry moved under the cached frame routes (enzICED #2231).
    ///
    /// Cached frame interest is recomputed from widget state during
    /// [`widget::Tree::diff`], which runs *before* layout and never again for
    /// this build. A widget that cached "not interested" because it was
    /// offscreen (a viewport-gated animation) is therefore invisible to
    /// targeted routing after a resize relaid it into view: the frame is
    /// pruned at its ancestors, the widget never sees a fresh viewport, and it
    /// can never ask for another frame. One unpruned frame gives it that
    /// viewport, and the ordinary refresh as the pass descends puts the
    /// subtree back on -- or off -- the route.
    geometry_route: bool,
    /// The [`layout_fingerprint`] of the laid out `state` (enzICED).
    fingerprint: u64,
    material_dirty: bool,
    /// Every node that has published a track since the last draw, sorted.
    ///
    /// A replay paints a node without a track as it was recorded. A node that
    /// stops publishing (it settled, or its widget only publishes while it
    /// moves) was recorded somewhere else, so a frame that loses one of these
    /// is drawn instead of replayed.
    tracked_since_draw: Vec<crate::core::renderer::NodeId>,
    /// The nodes the last update published tracks for, which the next draw
    /// records at those poses: a frame drawn for some other widget still
    /// records a moving node mid-travel (enzUI-ICED #3660).
    tracked_for_draw: Vec<crate::core::renderer::NodeId>,
    /// The cursor before the next update's first event, if the caller knows
    /// it (enzICED). See [`UserInterface::set_batch_cursor_origin`].
    batch_cursor_origin: Option<mouse::Cursor>,
}

/// The cursor each event of a batch saw when it arrived (enzICED).
///
/// Replays the batch's pointer events forward from `origin`, the cursor
/// before the first event. Without an origin, or when the replay does not end
/// at `last` (the window's cursor after the batch), every event gets `last`,
/// which is upstream behaviour. Allocation-free: an update must not cost more
/// than iced's own per-event floor.
#[derive(Debug, Clone, Copy)]
struct EventCursors {
    origin: Option<mouse::Cursor>,
    last: mouse::Cursor,
}

impl EventCursors {
    fn new(origin: Option<mouse::Cursor>, events: &[Event], last: mouse::Cursor) -> Self {
        let origin =
            origin.filter(|&origin| events.iter().fold(origin, step).position() == last.position());
        Self { origin, last }
    }

    /// Each event's cursor, in order.
    fn iter<'e>(self, events: &'e [Event]) -> impl Iterator<Item = mouse::Cursor> + 'e {
        let mut current = self.origin;
        let last = self.last;
        events.iter().map(move |event| match current.as_mut() {
            Some(cursor) => {
                *cursor = step(*cursor, event);
                *cursor
            }
            None => last,
        })
    }
}

/// The cursor after `event`, starting from `current`.
fn step(current: mouse::Cursor, event: &Event) -> mouse::Cursor {
    match event {
        Event::Mouse(mouse::Event::CursorMoved { position }) => mouse::Cursor::Available(*position),
        Event::Mouse(mouse::Event::CursorLeft) => mouse::Cursor::Unavailable,
        Event::Touch(
            touch::Event::FingerPressed { position, .. }
            | touch::Event::FingerMoved { position, .. }
            | touch::Event::FingerLifted { position, .. }
            | touch::Event::FingerLost { position, .. },
        ) => mouse::Cursor::Available(*position),
        _ => current,
    }
}

/// Identifies nonempty keyboard/IME-only batches directed to the live focus owner.
/// Mixed batches retain positional validation so dispatch order is unchanged.
pub fn focus_directed_input(events: &[Event]) -> bool {
    !events.is_empty()
        && events
            .iter()
            .all(|event| matches!(event, Event::Keyboard(_) | Event::InputMethod(_)))
}

struct NativeReconciliation {
    root: crate::core::renderer::NodeId,
    projected: bool,
    expected: std::collections::BTreeSet<crate::core::renderer::NodeId>,
    requested: std::collections::BTreeSet<crate::core::renderer::NodeId>,
    expected_scrolls: std::collections::BTreeSet<crate::core::renderer::NodeId>,
    requested_scrolls: std::collections::BTreeSet<crate::core::renderer::NodeId>,
    completed: usize,
    failed: bool,
}

impl widget::Operation for NativeReconciliation {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
        visit(self);
    }
    fn is_native_reconciliation(&self) -> bool {
        true
    }
    fn projection_applied(&mut self, root: crate::core::renderer::NodeId) {
        if root == self.root {
            self.projected = true;
        } else {
            self.failed = true;
        }
    }
    fn needs_editor_reconciliation(&self, node: crate::core::renderer::NodeId) -> bool {
        self.requested.contains(&node)
    }
    fn editor_reconciled(&mut self, node: crate::core::renderer::NodeId, success: bool) {
        if !self.expected.remove(&node) || !success {
            self.failed = true;
        }
        self.completed += 1;
    }
    fn needs_scroll_reconciliation(&self, node: crate::core::renderer::NodeId) -> bool {
        self.requested_scrolls.contains(&node)
    }
    fn scroll_reconciled(&mut self, node: crate::core::renderer::NodeId, success: bool) {
        if !self.expected_scrolls.remove(&node) || !success {
            self.failed = true;
        }
        self.completed += 1;
    }
}

impl<'a, Message, Theme, Renderer> UserInterface<'a, Message, Theme, Renderer>
where
    Renderer: crate::core::Renderer,
{
    /// Shifts exactly one matching widget clock group before an owned recording starts.
    ///
    /// Both passes use existing layout and renderer state. Missing, duplicate, mismatched
    /// or unshiftable owners refuse before mutation; no application actions are dispatched.
    pub fn rebase_docked_clocks(
        &mut self,
        renderer: &Renderer,
        animations: &[crate::core::renderer::TrackAnimation],
        delay: crate::core::time::Duration,
    ) -> bool {
        if animations.is_empty()
            || animations.len() > 8
            || animations.iter().enumerate().any(|(index, animation)| {
                animations[..index]
                    .iter()
                    .any(|other| other.node == animation.node)
            })
            || animations.iter().any(|animation| !animation.is_usable())
        {
            return false;
        }
        struct Rebase<'a> {
            animations: &'a [crate::core::renderer::TrackAnimation],
            delay: crate::core::time::Duration,
            apply: bool,
            claims: usize,
            success: bool,
        }
        impl widget::Operation for Rebase<'_> {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
                visit(self);
            }
            fn is_docked_clock_rebase(&self) -> bool {
                true
            }
            fn docked_clock(&mut self, clock: &mut dyn widget::operation::DockedClock) {
                if clock.owns_animations(self.animations) && clock.can_shift(self.delay) {
                    self.claims = self.claims.saturating_add(1);
                    if self.apply {
                        self.success &= self.claims == 1 && clock.shift(self.delay);
                    }
                }
            }
        }
        let mut operation = Rebase {
            animations,
            delay,
            apply: false,
            claims: 0,
            success: true,
        };
        let layout = Layout::new(self.state.size);
        self.root.as_widget_mut().operate(
            &mut self.state,
            layout,
            &Rectangle::with_size(self.bounds),
            renderer,
            &mut operation,
        );
        if operation.claims != 1 {
            return false;
        }
        operation.apply = true;
        operation.claims = 0;
        let layout = Layout::new(self.state.size);
        self.root.as_widget_mut().operate(
            &mut self.state,
            layout,
            &Rectangle::with_size(self.bounds),
            renderer,
            &mut operation,
        );
        operation.claims == 1 && operation.success
    }

    /// Builds a user interface for an [`Element`].
    ///
    /// It is able to avoid expensive computations when using a [`Cache`]
    /// obtained from a previous instance of a [`UserInterface`].
    ///
    /// # Example
    /// Imagine we want to build a [`UserInterface`] for
    /// [the counter example that we previously wrote](index.html#usage). Here
    /// is naive way to set up our application loop:
    ///
    /// ```no_run
    /// # mod iced_wgpu {
    /// #     pub type Renderer = ();
    /// # }
    /// #
    /// # pub struct Counter;
    /// #
    /// # impl Counter {
    /// #     pub fn new() -> Self { Counter }
    /// #     pub fn view(&self) -> iced_core::Element<(), (), Renderer> { unimplemented!() }
    /// #     pub fn update(&mut self, _: ()) {}
    /// # }
    /// use iced_runtime::core::shell;
    /// use iced_runtime::core::window;
    /// use iced_runtime::core::Size;
    /// use iced_runtime::user_interface::{self, UserInterface};
    /// use iced_wgpu::Renderer;
    ///
    /// // Initialization
    /// let mut counter = Counter::new();
    /// let mut cache = user_interface::Cache::new();
    /// let mut renderer = Renderer::default();
    /// let mut window = window::Headless; // This should be a proper window, like a `winit` one
    /// let mut waker = shell::Waker::noop();
    /// let mut window_size = Size::new(1024.0, 768.0);
    ///
    /// // Application loop
    /// loop {
    ///     // Process system events here...
    ///
    ///     // Build the user interface
    ///     let user_interface = UserInterface::build(
    ///         counter.view(),
    ///         window_size,
    ///         cache,
    ///         &mut renderer,
    ///     );
    ///
    ///     // Update and draw the user interface here...
    ///     // ...
    ///
    ///     // Obtain the cache for the next iteration
    ///     cache = user_interface.into_cache();
    /// }
    /// ```
    pub fn build<E: Into<Element<'a, Message, Theme, Renderer>>>(
        root: E,
        bounds: Size,
        cache: Cache,
        renderer: &mut Renderer,
    ) -> Self {
        let mut root = root.into();

        let Cache {
            mut state,
            bounds: last_bounds,
            geometry_route,
            fingerprint: last_fingerprint,
        } = cache;
        state.diff(root.as_widget_mut());

        root.as_widget_mut().layout(
            &mut state,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );

        // A widget whose geometry changed in this layout (a scrollable whose
        // content grew) answers on the next redraw, but the frame interest
        // cached by `diff` predates the layout. Such a widget raises its own
        // interest; propagating it adds the routes that reach it, without
        // opening the frame to the rest of the tree (enzICED).
        let fingerprint = layout_fingerprint(&state);
        if last_fingerprint != Some(fingerprint) {
            let _ = state.propagate_frame_interest();
        }

        UserInterface {
            root,
            state,
            overlay: None,
            overlay_detached: false,
            bounds,
            geometry_route: geometry_route || owes_geometry_route(last_bounds, bounds),
            fingerprint,
            material_dirty: true,
            tracked_since_draw: Vec::new(),
            tracked_for_draw: Vec::new(),
            batch_cursor_origin: None,
        }
    }

    /// Builds a user interface while attributing tree diff and layout to a window.
    ///
    /// This is available only with the `performance-observer` feature. When
    /// collection is disabled, each span is a cold atomic branch and allocates
    /// nothing.
    #[cfg(feature = "performance-observer")]
    pub fn build_observed<E: Into<Element<'a, Message, Theme, Renderer>>>(
        root: E,
        bounds: Size,
        cache: Cache,
        renderer: &mut Renderer,
        window: crate::performance::WindowId,
    ) -> Self {
        let mut root = root.into();
        let Cache {
            mut state,
            bounds: last_bounds,
            geometry_route,
            fingerprint: last_fingerprint,
        } = cache;

        {
            let _span =
                crate::performance::begin_stage(window, crate::performance::Stage::TreeDiff);
            state.diff(root.as_widget_mut());
        }

        {
            let _span = crate::performance::begin_stage(window, crate::performance::Stage::Layout);
            root.as_widget_mut().layout(
                &mut state,
                renderer,
                &layout::Limits::new(Size::ZERO, bounds),
            );
        }

        // See `build`: a layout that moved anything propagates the interest
        // it raised, without opening the frame to the rest of the tree.
        let fingerprint = layout_fingerprint(&state);
        if last_fingerprint != Some(fingerprint) {
            let _ = state.propagate_frame_interest();
        }

        UserInterface {
            root,
            state,
            overlay: None,
            overlay_detached: false,
            bounds,
            geometry_route: geometry_route || owes_geometry_route(last_bounds, bounds),
            fingerprint,
            material_dirty: true,
            tracked_since_draw: Vec::new(),
            tracked_for_draw: Vec::new(),
            batch_cursor_origin: None,
        }
    }

    /// Updates the [`UserInterface`] by processing each provided [`Event`].
    ///
    /// It returns __messages__ that may have been produced as a result of user
    /// interactions. You should feed these to your __update logic__.
    ///
    /// # Example
    /// Let's allow our [counter](index.html#usage) to change state by
    /// completing [the previous example](#example):
    ///
    /// ```no_run
    /// # mod iced_wgpu {
    /// #     pub type Renderer = ();
    /// # }
    /// #
    /// # pub struct Counter;
    /// #
    /// # impl Counter {
    /// #     pub fn new() -> Self { Counter }
    /// #     pub fn view(&self) -> iced_core::Element<(), (), Renderer> { unimplemented!() }
    /// #     pub fn update(&mut self, _: ()) {}
    /// # }
    /// use iced_runtime::core::mouse;
    /// use iced_runtime::core::shell;
    /// use iced_runtime::core::window;
    /// use iced_runtime::core::Size;
    /// use iced_runtime::user_interface::{self, UserInterface};
    /// use iced_wgpu::Renderer;
    ///
    /// let mut counter = Counter::new();
    /// let mut cache = user_interface::Cache::new();
    /// let mut renderer = Renderer::default();
    /// let mut window = window::Headless; // This should be a proper window, like a `winit` one
    /// let mut waker = shell::Waker::noop();
    /// let mut window_size = Size::new(1024.0, 768.0);
    /// let mut cursor = mouse::Cursor::default();
    ///
    /// // Initialize our event storage
    /// let mut events = Vec::new();
    /// let mut messages = shell::Bus::new();
    ///
    /// loop {
    ///     // Obtain system events...
    ///
    ///     let mut user_interface = UserInterface::build(
    ///         counter.view(),
    ///         window_size,
    ///         cache,
    ///         &mut renderer,
    ///     );
    ///
    ///     // Update the user interface
    ///     let (state, event_statuses) = user_interface.update(
    ///         &window,
    ///         &waker,
    ///         &events,
    ///         cursor,
    ///         &mut renderer,
    ///         &mut messages
    ///     );
    ///
    ///     cache = user_interface.into_cache();
    ///
    ///     // Process the produced messages
    ///     for (message, _receipt) in messages.drain() {
    ///         counter.update(message);
    ///     }
    /// }
    /// ```
    pub fn update(
        &mut self,
        window: &dyn Window,
        waker: &shell::Waker,
        events: &[Event],
        cursor: mouse::Cursor,
        renderer: &mut Renderer,
        messages: &mut shell::Bus<Message>,
    ) -> (State, Vec<event::Status>) {
        let mut ignored_window_requests = Vec::new();
        self.update_with_window_state(
            window,
            waker,
            events,
            cursor,
            renderer,
            messages,
            window::RuntimeState::default(),
            &mut ignored_window_requests,
        )
    }

    /// Sets the cursor that was current before the first event of the next
    /// update (enzICED).
    ///
    /// The cursor passed to an update is the one after its last event. When a
    /// busy frame batches a click with later pointer motion or a pointer
    /// leave, hit-testing the press there sends the click to another row or
    /// to nothing. With an origin, each event is dispatched with the cursor
    /// as it stood when that event arrived. An origin whose replay does not
    /// end at the passed cursor is ignored.
    pub fn set_batch_cursor_origin(&mut self, origin: mouse::Cursor) {
        self.batch_cursor_origin = Some(origin);
    }

    /// Updates this interface with authoritative state for its current native
    /// window and collects policy-free requests addressed to that same window.
    ///
    /// Window requests deliberately carry no runtime id. A multi-window host
    /// must bind the returned requests to the id associated with this specific
    /// `UserInterface` invocation.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_window_state(
        &mut self,
        window: &dyn Window,
        waker: &shell::Waker,
        events: &[Event],
        cursor: mouse::Cursor,
        renderer: &mut Renderer,
        messages: &mut shell::Bus<Message>,
        window_state: window::RuntimeState,
        window_requests: &mut Vec<window::Request>,
    ) -> (State, Vec<event::Status>) {
        self.update_with_layout_owner(
            window,
            waker,
            events,
            cursor,
            renderer,
            messages,
            window_state,
            window_requests,
            None,
        )
    }

    /// Dispatches frame events with the runtime-verified current surface owner's layout program.
    /// Non-frame events never receive this ownership context; native input needs its own lease.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_layout_owner(
        &mut self,
        window: &dyn Window,
        waker: &shell::Waker,
        events: &[Event],
        cursor: mouse::Cursor,
        renderer: &mut Renderer,
        messages: &mut shell::Bus<Message>,
        window_state: window::RuntimeState,
        window_requests: &mut Vec<window::Request>,
        layout_owner: Option<&std::sync::Arc<renderer::presentation::Program>>,
    ) -> (State, Vec<event::Status>) {
        let mut outdated = false;
        let mut redraw_request = window::RedrawRequest::Wait;
        let mut input_method = InputMethod::Disabled;
        let mut input_method_surrounding = None;
        let mut clipboard = Clipboard::new();
        let mut has_layout_changed = false;
        let mut change = Change::None;
        let mut redraw_deadline: Option<crate::core::time::Instant> = None;
        // Compositor tracks published this update, and whether every shell that
        // published one asked for nothing else (enzICED #2219). It starts true
        // and is ANDed down, so a single event that wants a rebuild disqualifies
        // the whole frame -- silence is never taken for consent.
        let mut tracks: Vec<renderer::Track> = Vec::new();
        let mut animations: Vec<renderer::TrackAnimation> = Vec::new();
        let mut is_animation_only = !self.material_dirty;
        // Whether every event asked for nothing but content redraws, and
        // whether any did (enzUI-ICED #3442).
        let mut content_only = true;
        let mut repainting = false;
        let mut content_requested = false;
        let mut requires_layout_ownership = false;
        // Summed across every dispatched event, so the runtime can report WHY
        // a frame was not animation-only (enzICED #2219 B8).
        let mut redraw_requesters = 0usize;
        let mut layout = Layout::new(self.state.size);
        let viewport = Rectangle::with_size(self.bounds);
        let event_cursors = EventCursors::new(self.batch_cursor_origin.take(), events, cursor);

        let overlay = self.root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );

        let mut maybe_overlay = (!overlay.is_empty()).then(|| overlay::Group::new(overlay));

        let (last_base, bases, overlay_statuses, overlay_interaction) = if maybe_overlay.is_some() {
            let mut overlay = maybe_overlay.as_mut().unwrap();
            let mut event_statuses = Vec::new();

            for (event, event_cursor) in events.iter().zip(event_cursors.iter(events)) {
                let mut shell =
                    Shell::new_with_window_state(window, waker.clone(), messages, window_state);
                if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                    shell.set_layout_owner(layout_owner.cloned());
                }

                overlay.update(event, event_cursor, renderer, &mut shell);

                let invalidation = shell.invalidation();
                change.record(invalidation);

                event_statuses.push(shell.event_status());
                redraw_request = redraw_request.min(shell.redraw_request());
                if let Some(at) = shell.redraw_deadline() {
                    redraw_deadline = Some(redraw_deadline.map_or(at, |current| current.min(at)));
                }
                if !input_method.is_enabled() && shell.input_method().is_enabled() {
                    input_method_surrounding = shell.input_method_surrounding().cloned();
                }
                input_method.merge(shell.input_method());
                clipboard.merge(shell.clipboard_mut());
                window_requests.extend(shell.drain_window_requests());

                // Collect tracks before layout validation; the caller alone decides whether replay is safe.
                let frame_was_animation_only = shell.is_animation_only();
                repainting |= shell.is_repainting();
                content_only &= shell.is_quiet();
                content_requested |= shell.content_requests() > 0;
                requires_layout_ownership |= shell.requires_layout_ownership();
                redraw_requesters = redraw_requesters.saturating_add(shell.redraw_requesters());
                for track in shell.take_tracks() {
                    if let Some(existing) = tracks
                        .iter_mut()
                        .find(|existing: &&mut renderer::Track| existing.node == track.node)
                    {
                        *existing = track;
                    } else {
                        tracks.push(track);
                    }
                }
                for animation in shell.take_animations() {
                    if let Some(existing) =
                        animations
                            .iter_mut()
                            .find(|existing: &&mut renderer::TrackAnimation| {
                                existing.node == animation.node
                            })
                    {
                        *existing = animation;
                    } else {
                        animations.push(animation);
                    }
                }

                is_animation_only = is_animation_only && frame_was_animation_only;

                if let shell::Invalidation::Layout(_) | shell::Invalidation::Overlay = invalidation
                {
                    drop(maybe_overlay);

                    // A relayout invalidates cached frame routes: dispatch once
                    // without pruning. Recreated overlays only repaint; the base
                    // tree's routes still hold (enzICED).
                    if let shell::Invalidation::Layout(_) = invalidation {
                        self.geometry_route = true;
                    }
                    has_layout_changed = true;

                    if let shell::Invalidation::Layout(diff) = invalidation {
                        match diff {
                            shell::Diff::Perform => {
                                self.root.as_widget_mut().diff(&mut self.state);
                            }
                            shell::Diff::Skip => {}
                        }

                        self.root.as_widget_mut().layout(
                            &mut self.state,
                            renderer,
                            &layout::Limits::new(Size::ZERO, self.bounds),
                        );

                        layout = Layout::new(self.state.size);
                    }

                    maybe_overlay = {
                        let overlay = self.root.as_widget_mut().overlay(
                            &mut self.state,
                            layout,
                            renderer,
                            &viewport,
                            Vector::ZERO,
                            self.bounds,
                        );

                        (!overlay.is_empty()).then(|| overlay::Group::new(overlay))
                    };

                    if maybe_overlay.is_none() {
                        event_statuses.resize(events.len(), event::Status::Ignored);
                        break;
                    }

                    overlay = maybe_overlay.as_mut().unwrap();
                }

                if invalidation == shell::Invalidation::Widgets {
                    outdated = true;
                }
            }

            let mut levitate = |cursor: mouse::Cursor| {
                let interaction = maybe_overlay
                    .as_mut()
                    .and_then(|overlay| {
                        cursor.position().map(|cursor_position| {
                            overlay.mouse_interaction(
                                mouse::Cursor::Available(cursor_position),
                                renderer,
                            )
                        })
                    })
                    .unwrap_or_default();

                if interaction == mouse::Interaction::None {
                    (cursor, mouse::Interaction::None)
                } else {
                    // Upstream: the cursor levitates above an interactive
                    // overlay rather than becoming unavailable.
                    (cursor.levitate(), interaction)
                }
            };
            let (base_cursor, interaction) = levitate(cursor);
            let base_cursors = event_cursors
                .iter(events)
                .map(|event_cursor| {
                    if event_cursor == cursor {
                        base_cursor
                    } else {
                        levitate(event_cursor).0
                    }
                })
                .collect::<Vec<_>>();
            self.overlay = maybe_overlay.as_ref().map(|_| interaction);

            (base_cursor, Some(base_cursors), event_statuses, interaction)
        } else {
            self.overlay = None;

            (
                cursor,
                None,
                vec![event::Status::Ignored; events.len()],
                mouse::Interaction::None,
            )
        };

        drop(maybe_overlay);

        // Under an overlay each event's base cursor was levitated above; else
        // it is the replayed cursor itself, with no list built for it.
        // Pulled inside the map, so the statuses keep an exact size hint and
        // collect into one right-sized allocation.
        let mut replay = event_cursors.iter(events);
        let mut levitated = bases.map(Vec::into_iter);
        let mut next_base = move || {
            match levitated.as_mut() {
                Some(listed) => listed.next(),
                None => replay.next(),
            }
            .unwrap_or(cursor)
        };
        let event_statuses = events
            .iter()
            .zip(overlay_statuses)
            .map(|(event, overlay_status)| {
                let base_cursor = next_base();
                // A mouse press an overlay took still reaches the layer below, levitating, so
                // a control there holding focus drops it and nothing there activates. Touch
                // presses carry their own position and stay with the overlay.
                let replay_below = matches!(overlay_status, event::Status::Captured)
                    && matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)));
                if matches!(overlay_status, event::Status::Captured) && !replay_below {
                    return overlay_status;
                }
                let base_cursor = if replay_below {
                    base_cursor.levitate()
                } else {
                    base_cursor
                };

                let mut shell =
                    Shell::new_with_window_state(window, waker.clone(), messages, window_state);
                if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                    shell.set_layout_owner(layout_owner.cloned());
                }

                if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                    if self.geometry_route {
                        // Newly exposed descendants need one unpruned frame to refresh cached frame interest.
                        self.geometry_route = false;

                        self.root.as_widget_mut().update(
                            &mut self.state,
                            event,
                            layout,
                            base_cursor,
                            renderer,
                            &mut shell,
                            &viewport,
                        );
                    } else if self.state.has_frame_interest() {
                        self.root.as_widget_mut().update_frame(
                            &mut self.state,
                            event,
                            layout,
                            base_cursor,
                            renderer,
                            &mut shell,
                            &viewport,
                        );
                    }
                } else {
                    self.root.as_widget_mut().update(
                        &mut self.state,
                        event,
                        layout,
                        base_cursor,
                        renderer,
                        &mut shell,
                        &viewport,
                    );
                }

                self.state.refresh_frame_interest(self.root.as_widget());

                if shell.event_status() == event::Status::Captured {
                    self.overlay = None;
                }

                redraw_request = redraw_request.min(shell.redraw_request());
                if let Some(at) = shell.redraw_deadline() {
                    redraw_deadline = Some(redraw_deadline.map_or(at, |current| current.min(at)));
                }
                if !input_method.is_enabled() && shell.input_method().is_enabled() {
                    input_method_surrounding = shell.input_method_surrounding().cloned();
                }
                input_method.merge(shell.input_method());
                clipboard.merge(shell.clipboard_mut());
                window_requests.extend(shell.drain_window_requests());

                // Collect tracks before layout validation; the caller alone decides whether replay is safe.
                let frame_was_animation_only = shell.is_animation_only();
                repainting |= shell.is_repainting();
                content_only &= shell.is_quiet();
                content_requested |= shell.content_requests() > 0;
                requires_layout_ownership |= shell.requires_layout_ownership();
                redraw_requesters = redraw_requesters.saturating_add(shell.redraw_requesters());
                for track in shell.take_tracks() {
                    if let Some(existing) = tracks
                        .iter_mut()
                        .find(|existing: &&mut renderer::Track| existing.node == track.node)
                    {
                        *existing = track;
                    } else {
                        tracks.push(track);
                    }
                }
                for animation in shell.take_animations() {
                    if let Some(existing) =
                        animations
                            .iter_mut()
                            .find(|existing: &&mut renderer::TrackAnimation| {
                                existing.node == animation.node
                            })
                    {
                        *existing = animation;
                    } else {
                        animations.push(animation);
                    }
                }

                is_animation_only = is_animation_only && frame_was_animation_only;

                let invalidation = shell.invalidation();
                change.record(invalidation);

                if let shell::Invalidation::Layout(_) | shell::Invalidation::Overlay = invalidation
                {
                    has_layout_changed = true;

                    // Same-sized layouts can expose siblings absent from the
                    // cached frame routes; recreated overlays leave the base
                    // tree's routes intact (enzICED).
                    if let shell::Invalidation::Layout(_) = invalidation {
                        self.geometry_route = true;
                    }

                    if let shell::Invalidation::Layout(diff) = invalidation {
                        match diff {
                            shell::Diff::Perform => {
                                self.root.as_widget_mut().diff(&mut self.state);
                            }
                            shell::Diff::Skip => {}
                        }

                        self.root.as_widget_mut().layout(
                            &mut self.state,
                            renderer,
                            &layout::Limits::new(Size::ZERO, self.bounds),
                        );

                        layout = Layout::new(self.state.size);
                    }

                    let overlay = self.root.as_widget_mut().overlay(
                        &mut self.state,
                        layout,
                        renderer,
                        &viewport,
                        Vector::ZERO,
                        self.bounds,
                    );

                    if !overlay.is_empty() {
                        let mut overlay = overlay::Group::new(overlay);

                        self.overlay = Some(overlay.mouse_interaction(cursor, renderer));
                    }
                }

                if invalidation == shell::Invalidation::Widgets {
                    outdated = true;
                }

                shell.event_status().merge(overlay_status)
            })
            .collect();

        let mouse_interaction = if overlay_interaction == mouse::Interaction::None {
            self.root.as_widget().mouse_interaction(
                &self.state,
                layout,
                last_base,
                &viewport,
                renderer,
            )
        } else {
            overlay_interaction
        };

        // enzICED: the view reads its window's decorations, so a change
        // rebuilds it.
        outdated |= events
            .iter()
            .any(|event| matches!(event, Event::Window(window::Event::DecorationsChanged(_))));

        let is_animation_only = is_animation_only
            && self
                .tracked_since_draw
                .iter()
                .all(|node| tracks.iter().any(|track| track.node == *node));
        if is_animation_only {
            for track in &tracks {
                if let Err(index) = self.tracked_since_draw.binary_search(&track.node) {
                    self.tracked_since_draw.insert(index, track.node);
                }
            }
        }
        self.tracked_for_draw.clear();
        self.tracked_for_draw
            .extend(tracks.iter().map(|track| track.node));
        self.tracked_for_draw.sort_unstable();
        self.tracked_for_draw.dedup();

        self.material_dirty |= outdated || has_layout_changed || !is_animation_only;

        (
            if outdated {
                State::Outdated
            } else {
                State::Updated {
                    mouse_interaction,
                    redraw_request,
                    redraw_deadline,
                    input_method,
                    input_method_surrounding,
                    clipboard,
                    has_layout_changed,
                    change,
                    tracks,
                    animations,
                    requires_layout_ownership,
                    is_animation_only: is_animation_only && !has_layout_changed,
                    is_content_only: content_requested && content_only && !has_layout_changed,
                    is_repainting: repainting,
                    redraw_requesters,
                }
            },
            event_statuses,
        )
    }

    /// Draws the [`UserInterface`] with the provided [`Renderer`].
    ///
    /// It returns the current [`mouse::Interaction`]. You should update the
    /// icon of the mouse cursor accordingly in your system.
    ///
    /// [`Renderer`]: crate::core::Renderer
    ///
    /// # Example
    /// We can finally draw our [counter](index.html#usage) by
    /// [completing the last example](#example-1):
    ///
    /// ```no_run
    /// # mod iced_wgpu {
    /// #     pub type Renderer = ();
    /// #     pub type Theme = ();
    /// # }
    /// #
    /// # pub struct Counter;
    /// #
    /// # impl Counter {
    /// #     pub fn new() -> Self { Counter }
    /// #     pub fn view(&self) -> Element<(), (), Renderer> { unimplemented!() }
    /// #     pub fn update(&mut self, _: ()) {}
    /// # }
    /// use iced_runtime::core::mouse;
    /// use iced_runtime::core::renderer;
    /// use iced_runtime::core::shell;
    /// use iced_runtime::core::window;
    /// use iced_runtime::core::{Element, Size};
    /// use iced_runtime::user_interface::{self, UserInterface};
    /// use iced_wgpu::{Renderer, Theme};
    ///
    /// let mut counter = Counter::new();
    /// let mut cache = user_interface::Cache::new();
    /// let mut renderer = Renderer::default();
    /// let mut window = window::Headless; // This should be a proper window, like a `winit` one
    /// let mut waker = shell::Waker::noop();
    /// let mut window_size = Size::new(1024.0, 768.0);
    /// let mut cursor = mouse::Cursor::default();
    /// let mut events = Vec::new();
    /// let mut messages = shell::Bus::new();
    /// let mut theme = Theme::default();
    ///
    /// loop {
    ///     // Obtain system events...
    ///
    ///     let mut user_interface = UserInterface::build(
    ///         counter.view(),
    ///         window_size,
    ///         cache,
    ///         &mut renderer,
    ///     );
    ///
    ///     // Update the user interface
    ///     let event_statuses = user_interface.update(
    ///         &window,
    ///         &waker,
    ///         &events,
    ///         cursor,
    ///         &mut renderer,
    ///         &mut messages
    ///     );
    ///
    ///     // Draw the user interface
    ///     let mouse_interaction = user_interface.draw(&mut renderer, &theme, &renderer::Style::default(), cursor);
    ///
    ///     cache = user_interface.into_cache();
    ///
    ///     for (message, _receipt) in messages.drain() {
    ///         counter.update(message);
    ///     }
    ///
    ///     // Update mouse cursor icon...
    ///     // Flush rendering operations...
    /// }
    /// ```
    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    ) {
        let viewport = Rectangle::with_size(self.bounds);
        renderer.reset(viewport);

        self.draw_inner(renderer, theme, style, cursor, viewport);
        self.material_dirty = false;
        self.tracked_since_draw = std::mem::take(&mut self.tracked_for_draw);
    }

    /// Draws as [`draw`](Self::draw) does, for comparison only: the frame
    /// still counts as replayed, so what the next update decides about
    /// replaying is unchanged.
    pub fn draw_for_comparison(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    ) {
        let viewport = Rectangle::with_size(self.bounds);
        renderer.reset(viewport);
        self.draw_inner(renderer, theme, style, cursor, viewport);
    }

    /// The body of [`draw`](Self::draw), without the renderer reset, so a
    /// retained capture can bracket exactly the same painting (enzICED #2219).
    fn draw_inner(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
        viewport: Rectangle,
    ) {
        let layout = Layout::new(self.state.size);
        let base_cursor = match &self.overlay {
            None | Some(mouse::Interaction::None) => cursor,
            _ => cursor.levitate(),
        };

        self.root.as_widget().draw(
            &self.state,
            renderer,
            theme,
            style,
            layout,
            base_cursor,
            &viewport,
        );

        let Self {
            overlay,
            root,
            overlay_detached,
            ..
        } = self;

        if overlay.is_none() || *overlay_detached {
            return;
        }

        let overlay = root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );

        if !overlay.is_empty() {
            let mut overlay = overlay::Group::new(overlay);

            overlay.draw(renderer, theme, style, cursor);
        }
    }

    /// The smallest rectangle holding every open overlay that reports its
    /// layout, in window coordinates, or `None` without one (enzICED).
    ///
    /// The runtime compares it with the window to decide whether the
    /// overlays need a popup surface of their own.
    pub fn overlay_bounds(&mut self, renderer: &Renderer) -> Option<Rectangle> {
        let _ = self.overlay?;
        let layout = Layout::new(self.state.size);
        let viewport = Rectangle::with_size(self.bounds);
        let overlay = self.root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );
        if overlay.is_empty() {
            return None;
        }
        overlay::Group::new(overlay).bounds(renderer)
    }

    /// The area the overlays draw in, the rectangle the first anchored one
    /// is placed against, in window coordinates, and whether any of them
    /// takes the input grab (enzICED); `None` without overlays that report an
    /// area.
    pub fn overlay_placement(
        &mut self,
        renderer: &Renderer,
    ) -> Option<(Rectangle, Option<Rectangle>, bool)> {
        let _ = self.overlay?;
        let layout = Layout::new(self.state.size);
        let viewport = Rectangle::with_size(self.bounds);
        let overlay = self.root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );
        if overlay.is_empty() {
            return None;
        }
        let mut group = overlay::Group::new(overlay);
        let area = group.bounds(renderer)?;
        Some((area, group.anchor(renderer), group.grabs(renderer)))
    }

    /// Whether [`draw`](Self::draw) leaves the overlays out because
    /// [`draw_overlay`](Self::draw_overlay) paints them into a surface of
    /// their own (enzICED).
    pub fn set_overlay_detached(&mut self, detached: bool) {
        if self.overlay_detached != detached {
            self.overlay_detached = detached;
            self.material_dirty = true;
        }
    }

    /// Draws only the overlays, as they would appear in `area` of the window,
    /// onto a renderer for a surface of `area`'s size (enzICED).
    ///
    /// `area` is in window coordinates and may extend past the window. The
    /// renderer is reset to `area`'s size; overlay content keeps its window
    /// position relative to `area`'s origin.
    pub fn draw_overlay(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
        area: Rectangle,
    ) {
        renderer.reset(Rectangle::with_size(area.size()));
        if self.overlay.is_none() {
            return;
        }
        let layout = Layout::new(self.state.size);
        let viewport = Rectangle::with_size(self.bounds);
        let overlay = self.root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );
        if overlay.is_empty() {
            return;
        }
        let mut overlay = overlay::Group::new(overlay);
        renderer.with_translation(Vector::new(-area.x, -area.y), |renderer| {
            overlay.draw(renderer, theme, style, cursor);
        });
    }

    /// Draws the [`UserInterface`] and captures the result as a retained scene
    /// (enzICED #2219).
    ///
    /// This is an ordinary [`draw`](Self::draw) that also keeps a copy, so the
    /// recording frame costs no extra pass. The returned recording can then be
    /// replayed by [`replay_retained`] on later frames -- including from a
    /// thread that does not own this `UserInterface` -- so an animation-only
    /// frame performs no application `view` rebuild, no `Tree::diff`, no widget
    /// `layout` and no widget `draw`.
    ///
    /// Returns `None` when the renderer cannot retain paint, in which case the
    /// frame was still drawn normally and the caller must keep drawing.
    ///
    /// Widgets inside the tree attribute their paint to a scene node with
    /// [`Renderer::mark_recording`], which is what a track later addresses.
    ///
    /// [`Renderer::mark_recording`]: crate::core::Renderer::mark_recording
    pub fn draw_retained(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    ) -> Option<renderer::Recording>
    where
        Renderer: crate::core::Renderer,
    {
        let viewport = Rectangle::with_size(self.bounds);

        // `draw` resets the renderer itself; the recording scope has to open
        // after that reset or it would be discarded with the previous frame.
        renderer.reset(viewport);

        if !renderer.start_recording(viewport) {
            // No retained path on this backend. Draw normally and say so,
            // rather than returning an empty scene the caller might replay.
            self.draw(renderer, theme, style, cursor);
            return None;
        }

        self.draw_inner(renderer, theme, style, cursor, viewport);
        self.material_dirty = false;
        self.tracked_since_draw = std::mem::take(&mut self.tracked_for_draw);

        renderer.finish_recording()
    }

    /// Records a replacement while an earlier scene retains its published tenure.
    ///
    /// Success requires complete docked ownership and surface-owner activation between
    /// frames before replay. Unsupported backends fail before drawing; capture refusals
    /// can occur after drawing. Either refusal requires quiescence before ordinary
    /// recording or UI presentation.
    pub fn draw_staged_retained(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    ) -> Result<renderer::Recording, renderer::presentation::Error>
    where
        Renderer: crate::core::Renderer,
    {
        let viewport = Rectangle::with_size(self.bounds);
        renderer.reset(viewport);
        if !renderer.start_staged_recording(viewport) {
            return Err(renderer::presentation::Error::UnsupportedBackend);
        }
        self.draw_inner(renderer, theme, style, cursor, viewport);
        match renderer.finish_recording() {
            Some(recording)
                if recording.presentation().is_some() && recording.docked_tenure().is_some() =>
            {
                self.material_dirty = false;
                self.tracked_since_draw = std::mem::take(&mut self.tracked_for_draw);
                Ok(recording)
            }
            _ => Err(renderer
                .presentation_refusal()
                .unwrap_or(renderer::presentation::Error::DockedOwnershipRequired)),
        }
    }

    /// Returns `(nodes on listener paths, listener leaves)` for frame-routing diagnostics.
    #[doc(hidden)]
    pub fn frame_route_counts(&self) -> (usize, usize) {
        (
            self.state.frame_route_size(),
            self.state.frame_listener_count(),
        )
    }

    /// Applies a programmatic widget command to the live native layout.
    ///
    /// Clears temporary presentation geometry without authorizing positional input.
    /// Existing activation latches survive; resulting material must be drawn before replay.
    pub fn operate_native(
        &mut self,
        renderer: &mut Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.prepare_native_layout(renderer);
        self.operate(renderer, operation);
    }

    /// Restores live native geometry without cancelling focus or armed commands.
    ///
    /// Revokes temporary presentation geometry and invalidates retained material.
    /// This does not authorize positional events against an unconfirmed frame.
    pub fn prepare_native_layout(&mut self, renderer: &mut Renderer) {
        renderer.clear_presented_layout();
        self.root.as_widget_mut().layout(
            &mut self.state,
            renderer,
            &layout::Limits::new(Size::ZERO, self.bounds),
        );
        self.overlay = None;
        self.material_dirty = true;
    }

    /// Traverses widgets using the current layout without invalidating retained material.
    ///
    /// Use [`Self::operate_native`] for native state commands. This traversal also
    /// serves semantic collection over temporary presentation geometry.
    pub fn operate(&mut self, renderer: &Renderer, operation: &mut dyn widget::Operation) {
        let viewport = Rectangle::with_size(self.bounds);
        let layout = Layout::new(self.state.size);

        // enzICED #2231: an operation reaches straight into widget state --
        // `scroll_to` moves content, focus moves a caret -- without any event
        // dispatch, so nothing refreshes the cached frame routes afterwards
        // (this method does not, and the shell answers by requesting a raw
        // redraw, not by rebuilding). A widget that cached "not interested"
        // while offscreen would stay pruned behind ancestors that never
        // re-examined it. Owe one unpruned frame, on the same terms as a
        // resize or a reflow; it is cleared as that frame is taken.
        self.geometry_route = true;

        operation.traverse(&mut |operation| {
            self.root.as_widget_mut().operate(
                &mut self.state,
                layout,
                &viewport,
                renderer,
                operation,
            );
        });

        let overlay = self.root.as_widget_mut().overlay(
            &mut self.state,
            layout,
            renderer,
            &viewport,
            Vector::ZERO,
            self.bounds,
        );

        if !overlay.is_empty() {
            let mut overlay = overlay::Group::new(overlay);

            overlay.operate(renderer, operation);

            // enzICED: an operation establishes the overlay, as before the
            // flattened-overlay merge, so a `draw` that follows it (without an
            // intervening `update`) paints the overlays the operation reached.
            // The cursor interaction of the last `update` is kept.
            if self.overlay.is_none() {
                self.overlay = Some(mouse::Interaction::None);
            }
        }
    }

    /// Relayouts and returns a new  [`UserInterface`] using the provided
    /// bounds.
    pub fn relayout(self, bounds: Size, renderer: &mut Renderer) -> Self {
        let cache = Cache {
            state: self.state,
            bounds: Some(self.bounds),
            geometry_route: self.geometry_route,
            fingerprint: Some(self.fingerprint),
        };

        Self::build(self.root, bounds, cache, renderer)
    }

    /// Cancels pending gestures without publishing actions or changing focus/document state.
    pub fn cancel_interactions(&mut self) {
        let _ = self.state.cancel_interactions();
        self.overlay = None;
    }

    /// Reconciles declared native editor and scroll targets under the exact projected layout domain.
    /// Missing/duplicate adapters or source disagreement refuse before input dispatch. This does
    /// not invoke widget layout, overlays, application actions, or a highlighter.
    ///
    /// # Errors
    /// Returns [`PresentedLayoutRefused`] for unsupported properties, invalid projection,
    /// or missing, duplicate or disagreeing native adapters.
    pub fn reconcile_presented_layout(
        &mut self,
        renderer: &Renderer,
    ) -> Result<usize, PresentedLayoutRefused> {
        self.reconcile_presented(renderer, true)
    }

    /// Projects confirmed geometry for synchronous semantic collection without editor mutation.
    /// This does not authorize input; the caller must revoke the candidate after collection.
    ///
    /// # Errors
    /// Returns [`PresentedLayoutRefused`] if the captured domain cannot be projected.
    pub fn reconcile_presented_geometry(
        &mut self,
        renderer: &Renderer,
    ) -> Result<usize, PresentedLayoutRefused> {
        self.reconcile_presented(renderer, false)
    }

    fn reconcile_presented(
        &mut self,
        renderer: &Renderer,
        editors: bool,
    ) -> Result<usize, PresentedLayoutRefused> {
        let Some(sample) = renderer.presented_layout().cloned() else {
            return Ok(0);
        };
        if !sample.geometry_reconcilable() {
            return Err(PresentedLayoutRefused);
        }
        let root = crate::core::renderer::NodeId(sample.root_node().ok_or(PresentedLayoutRefused)?);
        // The projection is laid over the tree for the operation below and
        // kept only if every native adapter reconciles against it.
        let native = if self.state.presentation_node() == Some(root) {
            let native = layout::Node::snapshot(&self.state);
            sample
                .project_layout(&native)
                .map_err(|_| PresentedLayoutRefused)?
                .apply(&mut self.state);
            Some(native)
        } else {
            None
        };
        let mut operation = NativeReconciliation {
            root,
            projected: native.is_some(),
            expected: sample
                .editor_nodes()
                .filter(|_| editors)
                .map(crate::core::renderer::NodeId)
                .collect(),
            requested: sample
                .editor_nodes()
                .filter(|_| editors)
                .map(crate::core::renderer::NodeId)
                .collect(),
            expected_scrolls: sample
                .scroll_nodes()
                .filter(|_| editors)
                .map(crate::core::renderer::NodeId)
                .collect(),
            requested_scrolls: sample
                .scroll_nodes()
                .filter(|_| editors)
                .map(crate::core::renderer::NodeId)
                .collect(),
            completed: 0,
            failed: false,
        };
        let layout = Layout::new(self.state.size);
        self.root.as_widget_mut().operate(
            &mut self.state,
            layout,
            &Rectangle::with_size(self.bounds),
            renderer,
            &mut operation,
        );
        if !operation.projected
            || operation.failed
            || !operation.expected.is_empty()
            || !operation.expected_scrolls.is_empty()
        {
            if let Some(native) = native {
                native.apply(&mut self.state);
            }
            return Err(PresentedLayoutRefused);
        }
        self.overlay = None;
        self.geometry_route = true;
        Ok(operation.completed)
    }

    /// Rebuilds native geometry once after a rejected presented-layout lease.
    pub fn reconcile_native_layout(&mut self, renderer: &Renderer) {
        self.cancel_interactions();
        self.root.as_widget_mut().layout(
            &mut self.state,
            renderer,
            &layout::Limits::new(Size::ZERO, self.bounds),
        );
        self.geometry_route = true;
    }

    /// Relayouts an observed user interface while preserving window attribution.
    #[cfg(feature = "performance-observer")]
    pub fn relayout_observed(
        self,
        bounds: Size,
        renderer: &mut Renderer,
        window: crate::performance::WindowId,
    ) -> Self {
        let cache = Cache {
            state: self.state,
            bounds: Some(self.bounds),
            geometry_route: self.geometry_route,
            fingerprint: Some(self.fingerprint),
        };

        Self::build_observed(self.root, bounds, cache, renderer, window)
    }

    /// Extract the [`Cache`] of the [`UserInterface`], consuming it in the
    /// process.
    pub fn into_cache(self) -> Cache {
        Cache {
            state: self.state,
            bounds: Some(self.bounds),
            geometry_route: self.geometry_route,
            fingerprint: Some(self.fingerprint),
        }
    }
}

/// Replays a scene captured by [`UserInterface::draw_retained`], applying each
/// compositor track to the node it names (enzICED #2219).
///
/// This function deliberately takes **no** [`UserInterface`]: an animation-only
/// frame needs the committed scene and the tracks, and nothing else. That is
/// the isolation claim in the type system -- a caller holding only a
/// `Recording` and a `&mut Renderer` cannot rebuild a view, diff a tree, lay
/// out a widget or draw one, because it has none of them to hand. It is what
/// lets a render lane keep presenting while the UI thread is busy elsewhere.
///
/// Returns `false` when the recording cannot be replayed -- a foreign backend
/// payload, or a renderer with no retained path -- in which case the caller
/// must fall back to a full [`UserInterface::draw`].
pub fn replay_retained<Renderer>(
    renderer: &mut Renderer,
    bounds: Size,
    scene: &renderer::Recording,
    tracks: &[renderer::Track],
) -> bool
where
    Renderer: crate::core::Renderer,
{
    let viewport = Rectangle::with_size(bounds);
    renderer.reset(viewport);
    renderer.replay_tracks(scene, tracks)
}

/// A hash of every widget's geometry in a laid out tree: equal fingerprints
/// mean nothing moved or resized (enzICED). It walks the tree's geometry
/// only; no widget is consulted.
fn layout_fingerprint(tree: &widget::Tree) -> u64 {
    // FxHash's word mix: the fingerprint is taken over the whole tree on
    // every rebuild, so it must be cheap, and only has to tell two layouts
    // of the same tree apart.
    fn mix(hash: u64, word: u64) -> u64 {
        (hash.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95)
    }

    fn walk(tree: &widget::Tree, hash: u64) -> u64 {
        let mut hash = mix(
            hash,
            u64::from(tree.translation.x.to_bits()) << 32 | u64::from(tree.translation.y.to_bits()),
        );
        hash = mix(
            hash,
            u64::from(tree.size.width.to_bits()) << 32 | u64::from(tree.size.height.to_bits()),
        );
        hash = mix(hash, tree.children.len() as u64);
        tree.children
            .iter()
            .fold(hash, |hash, child| walk(child, hash))
    }

    walk(tree, 0)
}

/// Whether a rebuild at `bounds` moved the geometry under the cached frame
/// routes and so owes one unpruned frame (enzICED #2231).
///
/// Only a *change* arms the route. A first build has no cached interest to
/// invalidate (every fresh `Tree` starts interested), and rebuilding at the
/// same size -- what an application does on every single frame -- leaves
/// targeted routing exactly as it was, so there is no recurring fan-out.
fn owes_geometry_route(last_bounds: Option<Size>, bounds: Size) -> bool {
    matches!(last_bounds, Some(last) if last != bounds)
}

/// Reusable data of a specific [`UserInterface`].
#[derive(Debug)]
pub struct Cache {
    state: widget::Tree,
    /// The bounds the cached tree was last laid out with, if it ever was.
    ///
    /// Kept so a rebuild can tell a resize from the ordinary same-size rebuild
    /// that happens every frame (enzICED #2231).
    bounds: Option<Size>,
    /// Whether a geometry transition still owes one unpruned frame.
    ///
    /// Carried through the cache because the redraw that answers a resize may
    /// arrive in a later `UserInterface` instance than the one that observed
    /// the new bounds.
    geometry_route: bool,
    /// The [`layout_fingerprint`] of the cached layout, if it was ever laid
    /// out (enzICED).
    fingerprint: Option<u64>,
}

impl Cache {
    /// Creates an empty [`Cache`].
    ///
    /// You should use this to initialize a [`Cache`] before building your first
    /// [`UserInterface`].
    pub fn new() -> Cache {
        Cache {
            state: widget::Tree::empty(),
            bounds: None,
            geometry_route: false,
            fingerprint: None,
        }
    }
}

impl Default for Cache {
    fn default() -> Cache {
        Cache::new()
    }
}

/// The change experienced by a [`UserInterface`] while being updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing was invalidated: the [`UserInterface`] is unchanged.
    None,
    /// Only the application overlays were recreated.
    Overlay,
    /// The application layout was recomputed.
    Layout,
}

impl Change {
    /// Accumulates the [`Change`] a [`shell::Invalidation`] causes; a
    /// layout change subsumes an overlay one.
    fn record(&mut self, invalidation: shell::Invalidation) {
        match invalidation {
            shell::Invalidation::Layout(_) => *self = Change::Layout,
            shell::Invalidation::Overlay if *self == Change::None => *self = Change::Overlay,
            _ => {}
        }
    }
}

/// The resulting state after updating a [`UserInterface`].
#[derive(Debug)]
pub enum State {
    /// The [`UserInterface`] is outdated and needs to be rebuilt.
    Outdated,

    /// The [`UserInterface`] is up-to-date and can be reused without
    /// rebuilding.
    Updated {
        /// The current [`mouse::Interaction`] of the user interface.
        mouse_interaction: mouse::Interaction,
        /// The [`window::RedrawRequest`] describing when a redraw should be performed.
        redraw_request: window::RedrawRequest,
        /// Earliest widget timer, preserved when a presentation lane supplies moving frames.
        redraw_deadline: Option<crate::core::time::Instant>,
        /// The current [`InputMethod`] strategy of the user interface.
        input_method: InputMethod,
        /// The text around the cursor of the field whose input method this
        /// is, when the field told it.
        input_method_surrounding: Option<crate::core::input_method::Surrounding>,
        /// The set of [`Clipboard`] requests that the user interface has produced.
        clipboard: Clipboard,
        /// Whether the layout of the [`UserInterface`] has changed.
        has_layout_changed: bool,
        /// The [`Change`] that the [`UserInterface`] experienced (upstream).
        change: Change,
        /// The compositor animation tracks published during this update
        /// (enzICED #2219).
        tracks: Vec<renderer::Track>,
        /// The renderer-owned animations published during this update, which a
        /// render lane can keep sampling without the UI thread (enzICED #2219).
        animations: Vec<renderer::TrackAnimation>,
        /// If true, a transform-only cached scene cannot serve this update.
        requires_layout_ownership: bool,
        /// Whether updates have changed only tracks since the current material was drawn.
        ///
        /// Rebuilt content, native commands, widget damage, and layout changes keep
        /// this false until drawing. Replay still requires usable tracks and a
        /// valid committed scene.
        is_animation_only: bool,
        /// Whether this update asked only for content redraws
        /// ([`Shell::request_content_redraw`]): the last frame can be
        /// presented again with its primitives prepared, entering no widget
        /// (enzUI-ICED #3442).
        ///
        /// [`Shell::request_content_redraw`]: crate::core::Shell::request_content_redraw
        is_content_only: bool,
        /// Whether a widget's paint changed in a way no track carries
        /// ([`Shell::request_redraw`]), so the frames after this one are
        /// drawn too (enzUI-ICED #3660).
        ///
        /// [`Shell::request_redraw`]: crate::core::Shell::request_redraw
        is_repainting: bool,
        /// How many widgets asked for the next frame during this update
        /// (enzICED #2219).
        ///
        /// Reported so a refusal to serve an animation-only frame is
        /// *explained* rather than an unexplained `false`: more requesters than
        /// published tracks means some other widget -- a caret, a spinner --
        /// wants to paint, and replaying the retained scene would freeze it
        /// (review finding B8).
        redraw_requesters: usize,
    },
}

impl State {
    /// Returns whether the layout of the [`UserInterface`] has changed.
    pub fn has_layout_changed(&self) -> bool {
        match self {
            State::Outdated => true,
            State::Updated {
                has_layout_changed, ..
            } => *has_layout_changed,
        }
    }

    /// Returns whether processing requested any newly drawn frame.
    pub fn invalidated(&self) -> bool {
        match self {
            State::Outdated => true,
            State::Updated {
                redraw_request,
                has_layout_changed,
                ..
            } => *has_layout_changed || *redraw_request != window::RedrawRequest::Wait,
        }
    }

    /// Returns the redraw requested while processing the update.
    pub fn redraw_request(&self) -> window::RedrawRequest {
        match self {
            State::Outdated => window::RedrawRequest::NextFrame,
            State::Updated { redraw_request, .. } => *redraw_request,
        }
    }
}
