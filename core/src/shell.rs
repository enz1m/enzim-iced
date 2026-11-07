// Modified by enzUI contributors. See FORK.md.
//! Communicate with the iced runtime from widgets.
use crate::clipboard;
use crate::event;
use crate::window;
use crate::{Clipboard, InputMethod, Window};

use std::rc::{self, Rc};
use std::sync::Arc;
use std::vec;

/// A connection to the state of a shell.
///
/// A [`Widget`] can leverage a [`Shell`] to trigger changes in an application,
/// like publishing messages or invalidating the current layout.
///
/// [`Widget`]: crate::Widget
#[derive(Debug)]
pub struct Shell<'a, Message> {
    window: &'a dyn Window,
    bus: &'a mut Bus<Message>,
    waker: Waker,
    event_status: event::Status,
    redraw_request: window::RedrawRequest,
    redraw_deadline: Option<crate::time::Instant>,
    input_method: InputMethod,
    /// The text around the cursor of the field whose input method won.
    input_method_surrounding: Option<crate::input_method::Surrounding>,
    invalidation: Invalidation,
    clipboard: Clipboard,
    window_state: window::RuntimeState,
    window_requests: Vec<window::Request>,
    tracks: Vec<crate::renderer::Track>,
    animations: Vec<crate::renderer::TrackAnimation>,
    /// A widget asked for the next frame to be drawn
    /// ([`request_redraw`](Self::request_redraw)), as opposed to the next
    /// frame of motion its tracks carry (enzUI-ICED #3660).
    repaint: bool,
    /// Widgets asked for the next frame of motion their published tracks
    /// carry ([`request_track_frame`](Self::request_track_frame)).
    track_frames: usize,
    requires_layout_ownership: bool,
    layout_owner: Option<Arc<crate::renderer::presentation::Program>>,
    /// How many widgets asked for the next frame during this update
    /// (enzICED #2219).
    ///
    /// Counted, not merely flagged, because "an animation-only frame" is not
    /// "somebody published a track": it is "*nothing else* wants to paint".
    /// A caret, a spinner or a progress bar repaints itself from
    /// `RedrawRequested` without invalidating layout or widgets, so a shell
    /// that only checked tracks and invalidation would replay a retained scene
    /// over it and freeze it for the whole travel (#2219 review finding B8).
    redraw_requesters: usize,
    /// How many widgets asked for a frame because only their custom
    /// primitive's content changed (enzICED, enzUI-ICED #3442).
    content_requests: usize,
}

/// The most severe invalidation requested by a [`Shell`] while
/// processing an event.
///
/// Variants are ordered by severity and [`Shell::invalidate`]
/// accumulates the maximum: a higher variant subsumes the lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invalidation {
    /// Nothing needs to be rebuilt.
    None,
    /// The application overlays need to be recreated.
    Overlay,
    /// The application layout needs to be recomputed, following the
    /// given [`Diff`] strategy.
    Layout(Diff),
    /// The entire application widget tree needs to be rebuilt and
    /// relaid out.
    Widgets,
}

impl<'a, Message> Shell<'a, Message> {
    /// Creates a new [`Shell`] with the provided buffer of messages.
    pub fn new(window: &'a dyn Window, waker: Waker, bus: &'a mut Bus<Message>) -> Self {
        Self {
            window,
            bus,
            waker,
            event_status: event::Status::Ignored,
            redraw_request: window::RedrawRequest::Wait,
            redraw_deadline: None,
            tracks: Vec::new(),
            animations: Vec::new(),
            repaint: false,
            track_frames: 0,
            requires_layout_ownership: false,
            layout_owner: None,
            redraw_requesters: 0,
            content_requests: 0,
            invalidation: Invalidation::None,
            input_method: InputMethod::Disabled,
            input_method_surrounding: None,
            clipboard: Clipboard::new(),
            window_state: window::RuntimeState::default(),
            window_requests: Vec::new(),
        }
    }

    /// Creates a [`Shell`] with authoritative state for the window currently
    /// dispatching the widget event.
    pub fn new_with_window_state(
        window: &'a dyn Window,
        waker: Waker,
        bus: &'a mut Bus<Message>,
        window_state: window::RuntimeState,
    ) -> Self {
        Self {
            window_state,
            ..Self::new(window, waker, bus)
        }
    }

    /// Creates a new [`Shell`] from the current one with the given list of local messages.
    pub fn local<'b, A>(&self, bus: &'b mut Bus<A>) -> Shell<'b, A>
    where
        'a: 'b,
    {
        let mut local =
            Shell::new_with_window_state(self.window, self.waker.clone(), bus, self.window_state);
        local.layout_owner = self.layout_owner.clone();
        local
    }

    /// Supplies the current surface owner's layout program for frame-only dispatch.
    /// The runtime must verify live recording tenure; this grants no input or accessibility lease.
    pub fn set_layout_owner(
        &mut self,
        program: Option<Arc<crate::renderer::presentation::Program>>,
    ) {
        self.layout_owner = program;
    }

    /// Whether the runtime's active presentation program owns this reservation driver.
    pub fn presentation_owns_layout(&self, driver: crate::renderer::NodeId) -> bool {
        self.layout_owner
            .as_ref()
            .is_some_and(|program| program.drives_responsive(driver.0))
    }

    /// Whether the active surface program owns this exact caret clock and allocation.
    /// A changed focus epoch or a different leaf still requires native repaint.
    pub fn presentation_owns_blink(
        &self,
        node: crate::renderer::NodeId,
        blink: crate::layout::input::Blink,
    ) -> bool {
        self.layout_owner
            .as_ref()
            .is_some_and(|program| program.input_blink(node.0) == Some(blink))
    }

    /// Returns authoritative native state for the current window.
    pub fn window_state(&self) -> window::RuntimeState {
        self.window_state
    }

    /// Requests a policy-free native operation on the exact window currently
    /// dispatching this widget event.
    pub fn request_window(&mut self, request: window::Request) {
        self.window_requests.push(request);
    }

    /// Drains native current-window requests produced by this shell.
    pub fn drain_window_requests(&mut self) -> impl Iterator<Item = window::Request> + '_ {
        self.window_requests.drain(..)
    }

    /// Returns the [`Window`] of the [`Shell`].
    pub fn window(&self) -> &'a dyn Window {
        self.window
    }

    /// Returns the [`Waker`] of the [`Shell`].
    pub fn waker(&self) -> &Waker {
        &self.waker
    }

    /// Returns true if the [`Shell`] contains no published messages
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bus.messages.is_empty()
    }

    /// Publish the given `Message` for an application to process it.
    pub fn publish(&mut self, message: Message) {
        let _ = self.publish_and_track(message);
    }

    /// Publish the given `Message` for an application to process it.
    ///
    /// The returned [`Tracking`] can be used to determine if the message
    /// was processed.
    pub fn publish_and_track(&mut self, message: Message) -> Tracking {
        self.bus.push(message)
    }

    /// Forwards the given `Message` and fulfills the given [`Receipt`]
    /// once processed.
    pub fn forward(&mut self, message: Message, receipt: Receipt) {
        self.bus.forward(message, receipt);
    }

    /// Marks the current event as captured. Prevents "event bubbling".
    ///
    /// A widget should capture an event when no ancestor should
    /// handle it.
    pub fn capture_event(&mut self) {
        self.event_status = event::Status::Captured;
    }

    /// Returns the current [`event::Status`] of the [`Shell`].
    #[must_use]
    pub fn event_status(&self) -> event::Status {
        self.event_status
    }

    /// Returns whether the current event has been captured.
    #[must_use]
    pub fn is_event_captured(&self) -> bool {
        self.event_status == event::Status::Captured
    }

    /// Requests a new frame to be drawn as soon as possible.
    ///
    /// The frame is drawn: its widgets are laid out where layout asks for it
    /// and drawn again. This is what a widget asks for whenever what it
    /// paints changes, including a widget that moves, resizes or reflows
    /// content itself from `update` (a custom slide that changes its
    /// children's widths, a panel that lays out a child again each frame).
    /// Tracks other widgets publish never stand in for it: a frame they
    /// alone would move is still drawn (enzUI-ICED #3660).
    ///
    /// A widget whose motion is wholly carried by the tracks it publishes
    /// asks for [`request_track_frame`](Self::request_track_frame) instead.
    #[track_caller]
    pub fn request_redraw(&mut self) {
        wake_trace::note(window::RedrawRequest::NextFrame);
        self.redraw_request = window::RedrawRequest::NextFrame;
        self.redraw_requesters = self.redraw_requesters.saturating_add(1);
        self.repaint = true;
    }

    /// Requests the next frame of motion that the tracks published in this
    /// update fully describe ([`publish_track`](Self::publish_track),
    /// [`publish_track_animation`](Self::publish_track_animation)): a
    /// translation, scale, rotation, opacity or clip of a recorded node whose
    /// paint and layout are otherwise unchanged.
    ///
    /// When nothing else in the update asks for a draw, the runtime serves
    /// the frame by moving the last recorded scene along those tracks (on the
    /// render lane, off the UI thread). Ask for it only in an update that
    /// publishes the tracks; a widget whose node is not recorded yet, or whose
    /// layout or paint changed, asks for [`request_redraw`](Self::request_redraw).
    /// An update that publishes no track at all is drawn either way.
    #[track_caller]
    pub fn request_track_frame(&mut self) {
        wake_trace::note(window::RedrawRequest::NextFrame);
        self.redraw_request = window::RedrawRequest::NextFrame;
        self.track_frames = self.track_frames.saturating_add(1);
    }

    /// Whether a widget asked for the next frame to be drawn
    /// ([`request_redraw`](Self::request_redraw)) during this update, so
    /// the UI thread draws the frames after this one too.
    #[must_use]
    pub fn is_repainting(&self) -> bool {
        self.repaint
    }

    /// Requests a new frame because only what a custom primitive draws has
    /// changed: its bounds, and everything else the widget drew, are as they
    /// were (enzUI-ICED #3442).
    ///
    /// The primitive must read its content when it is prepared, not when it
    /// is drawn. When nothing else in the window asked to paint, the runtime
    /// then presents the last frame again with only its primitives prepared,
    /// entering no widget's `draw`; otherwise this is [`request_redraw`].
    ///
    /// [`request_redraw`]: Self::request_redraw
    #[track_caller]
    pub fn request_content_redraw(&mut self) {
        wake_trace::note(window::RedrawRequest::NextFrame);
        self.redraw_request = window::RedrawRequest::NextFrame;
        self.content_requests = self.content_requests.saturating_add(1);
    }

    /// Whether this update asked only for content redraws: no message, no
    /// invalidation, no track and no other paint request (enzUI-ICED #3442).
    #[must_use]
    pub fn is_content_only(&self) -> bool {
        self.content_requests > 0 && self.is_quiet()
    }

    /// Whether this update asked for nothing at all besides content redraws.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.redraw_requesters == 0
            && self.track_frames == 0
            && self.tracks.is_empty()
            && self.bus.messages.is_empty()
            && self.invalidation == Invalidation::None
    }

    /// How many content redraws this update asked for (enzUI-ICED #3442).
    #[must_use]
    pub fn content_requests(&self) -> usize {
        self.content_requests
    }

    /// Requests a new frame to be drawn at the given [`window::RedrawRequest`].
    ///
    /// A request for the next frame is [`request_redraw`](Self::request_redraw);
    /// a later one only schedules that frame and does not keep this one from
    /// being served along tracks.
    #[track_caller]
    pub fn request_redraw_at(&mut self, redraw_request: impl Into<window::RedrawRequest>) {
        let redraw_request = redraw_request.into();
        wake_trace::note(redraw_request);
        if redraw_request == window::RedrawRequest::NextFrame {
            self.request_redraw();
        } else {
            self.redraw_request = self.redraw_request.min(redraw_request);
        }
    }

    /// Schedules a future update without declaring that the current pixels changed.
    /// Call `request_redraw` separately when the current event changes paint.
    /// The deadline survives handover to an independent presentation lane.
    #[track_caller]
    pub fn schedule_redraw_at(&mut self, at: crate::time::Instant) {
        wake_trace::note(window::RedrawRequest::At(at));
        self.redraw_deadline = Some(self.redraw_deadline.map_or(at, |current| current.min(at)));
        self.redraw_request = self.redraw_request.min(window::RedrawRequest::At(at));
    }

    /// The earliest scheduled update, independent of immediate animation requests.
    pub fn redraw_deadline(&self) -> Option<crate::time::Instant> {
        self.redraw_deadline
    }

    /// Returns the request a redraw should happen, if any.
    #[must_use]
    pub fn redraw_request(&self) -> window::RedrawRequest {
        self.redraw_request
    }

    /// Replaces the redraw request of the [`Shell`]; without conflict resolution.
    ///
    /// This is useful if you want to overwrite the redraw request to a previous value.
    /// Since it's a fairly advanced use case and should rarely be used, it is a static
    /// method.
    pub fn replace_redraw_request(shell: &mut Self, redraw_request: window::RedrawRequest) {
        shell.redraw_request = redraw_request;
    }

    /// Requests the runtime to read the clipboard contents expecting the given [`clipboard::Kind`].
    ///
    /// The runtime will produce a [`clipboard::Event::Read`] when the contents have been read.
    pub fn read_clipboard(&mut self, kind: clipboard::Kind) {
        self.clipboard.reads.push(kind);
    }

    /// Requests the runtime to read the primary selection expecting the given
    /// [`clipboard::Kind`].
    ///
    /// The runtime will produce a [`clipboard::Event::ReadPrimary`] when the
    /// contents have been read.
    pub fn read_primary(&mut self, kind: clipboard::Kind) {
        self.clipboard.primary_reads.push(kind);
    }

    /// Requests the runtime to offer the given [`clipboard::Content`] as the
    /// primary selection (the text a middle click pastes).
    ///
    /// The runtime will produce a [`clipboard::Event::WrittenPrimary`] when the
    /// selection has been claimed.
    pub fn write_primary(&mut self, content: impl Into<clipboard::Content>) {
        self.clipboard.primary_write = Some(content.into());
    }

    /// Requests the runtime to write the given [`clipboard::Content`] to the clipboard.
    ///
    /// The runtime will produce a [`clipboard::Event::Written`] when the contents have been written.
    pub fn write_clipboard(&mut self, content: impl Into<clipboard::Content>) {
        self.clipboard.write = Some(content.into());
    }

    /// Returns the [`Clipboard`] requests of the [`Shell`], mutably.
    pub fn clipboard_mut(&mut self) -> &mut Clipboard {
        &mut self.clipboard
    }

    /// Requests the current [`InputMethod`] strategy.
    ///
    /// __Important__: This request will only be honored by the
    /// [`Shell`] only during a [`window::Event::RedrawRequested`].
    pub fn request_input_method<T: AsRef<str>>(&mut self, ime: &InputMethod<T>) {
        self.input_method.merge(ime);
    }

    /// Requests an [`InputMethod`] together with the text around its
    /// cursor; the text goes with the request only if this request's input
    /// method is the one the [`Shell`] keeps.
    pub fn request_input_method_with_surrounding<T: AsRef<str>>(
        &mut self,
        ime: &InputMethod<T>,
        surrounding: crate::input_method::Surrounding,
    ) {
        let wins = !self.input_method.is_enabled() && matches!(ime, InputMethod::Enabled { .. });
        self.input_method.merge(ime);
        if wins {
            self.input_method_surrounding = Some(surrounding);
        }
    }

    /// The text around the cursor of the current [`InputMethod`], if its
    /// field told it.
    #[must_use]
    pub fn input_method_surrounding(&self) -> Option<&crate::input_method::Surrounding> {
        self.input_method_surrounding.as_ref()
    }

    /// Returns the current [`InputMethod`] strategy.
    #[must_use]
    pub fn input_method(&self) -> &InputMethod {
        &self.input_method
    }

    /// Returns the current [`InputMethod`] strategy.
    #[must_use]
    pub fn input_method_mut(&mut self) -> &mut InputMethod {
        &mut self.input_method
    }

    /// Invalidates the current application overlay.
    ///
    /// The shell will recreate the application overlays.
    pub fn invalidate_overlay(&mut self) {
        self.invalidate(Invalidation::Overlay);
    }

    /// Invalidates the current application layout.
    ///
    /// The shell will relayout the application widgets.
    pub fn invalidate_layout(&mut self) {
        self.invalidate_layout_with(Diff::Skip);
    }

    /// Invalidates the current application layout with the following [`Diff`] strategy.
    pub fn invalidate_layout_with(&mut self, diff: Diff) {
        self.invalidate(Invalidation::Layout(diff));
    }

    /// Invalidates the current application widgets.
    ///
    /// The shell will rebuild and relayout the widget tree.
    pub fn invalidate_widgets(&mut self) {
        self.invalidate(Invalidation::Widgets);
    }

    /// Sets the [`Invalidation`] of the [`Shell`].
    pub fn invalidate(&mut self, invalidation: Invalidation) {
        self.invalidation = self.invalidation.max(invalidation);
    }

    /// Returns the [`Invalidation`] of the [`Shell`].
    pub fn invalidation(&self) -> Invalidation {
        self.invalidation
    }

    /// Runs `f` and returns what it invalidated on its own, whatever was
    /// invalidated before; the shell keeps the larger of the two.
    ///
    /// A container uses this to tell its child's invalidation from a
    /// sibling's (enzUI-ICED #3245).
    pub fn scoped_invalidation<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> (R, Invalidation) {
        let before = std::mem::replace(&mut self.invalidation, Invalidation::None);
        let result = f(self);
        let own = self.invalidation;
        self.invalidation = before.max(own);
        (result, own)
    }

    /// Publishes a compositor animation track for this frame (enzICED #2219).
    ///
    /// This is how a migrated motion effect says *compose me, do not lay me out
    /// again*. A widget that publishes a track for its scene node -- and
    /// invalidates nothing -- lets the runtime serve the frame from the last
    /// committed scene, so no `view` runs, no tree is diffed, and no widget is
    /// laid out or drawn.
    ///
    /// Publishing a track is a **request, not a promise**. The runtime falls
    /// back to a full draw whenever it cannot honour it: no committed scene,
    /// a renderer with no retained path, a resize, a theme or font change, or
    /// any other invalidation in the same frame. An effect must therefore stay
    /// correct when its track is ignored, which is what keeps the fallback
    /// honest rather than a second code path nobody exercises.
    pub fn publish_track(&mut self, track: crate::renderer::Track) {
        self.record_track(track);
    }

    /// Publishes layout drivers that require owned-layout replay.
    ///
    /// A nonempty group requires layout ownership; an empty group changes
    /// nothing.
    pub fn publish_docked_tracks(
        &mut self,
        tracks: impl IntoIterator<Item = crate::renderer::Track>,
    ) {
        for track in tracks {
            self.requires_layout_ownership = true;
            self.record_track(track);
        }
    }

    /// Whether this update requires an owned layout program instead of transform-only cached paint.
    pub fn requires_layout_ownership(&self) -> bool {
        self.requires_layout_ownership
    }

    fn record_track(&mut self, track: crate::renderer::Track) {
        // Last publication for a node wins: an effect that retargets mid-flight
        // publishes again from the value it is presenting at, and the frame
        // must use the newer one.
        if let Some(existing) = self
            .tracks
            .iter_mut()
            .find(|existing| existing.node == track.node)
        {
            *existing = track;
        } else {
            self.tracks.push(track);
        }
    }

    /// Publishes a renderer-owned animation for this frame (enzICED #2219).
    ///
    /// Unlike [`publish_track`](Self::publish_track), which is one sampled
    /// instant, this hands the render lane the whole travel, so the lane can
    /// keep advancing the node from the presentation clock while the UI thread
    /// is doing something else entirely.
    ///
    /// The effect must **also** publish the sampled track for the current
    /// frame: a runtime with no lane (a software fallback, a backend with no
    /// retained path) still has to present the value this frame.
    pub fn publish_track_animation(&mut self, animation: crate::renderer::TrackAnimation) {
        if let Some(existing) = self
            .animations
            .iter_mut()
            .find(|existing| existing.node == animation.node)
        {
            *existing = animation;
        } else {
            self.animations.push(animation);
        }
    }

    /// The renderer-owned animations published during this update
    /// (enzICED #2219).
    pub fn animations(&self) -> &[crate::renderer::TrackAnimation] {
        &self.animations
    }

    /// Takes the published animations, leaving none behind.
    pub fn take_animations(&mut self) -> Vec<crate::renderer::TrackAnimation> {
        std::mem::take(&mut self.animations)
    }

    /// The compositor tracks published during this update (enzICED #2219).
    pub fn tracks(&self) -> &[crate::renderer::Track] {
        &self.tracks
    }

    /// Takes the published tracks, leaving none behind.
    pub fn take_tracks(&mut self) -> Vec<crate::renderer::Track> {
        std::mem::take(&mut self.tracks)
    }

    /// Whether this frame can be served by replaying the last committed scene
    /// (enzICED #2219).
    ///
    /// True only when at least one track was published and **nothing** asked
    /// for a draw: no message (a message runs `update` and then `view`), no
    /// layout or widget invalidation, and no
    /// [`request_redraw`](Self::request_redraw). Frames asked for with
    /// [`request_track_frame`](Self::request_track_frame) are the ones the
    /// tracks serve. Anything else and the caller must draw.
    ///
    /// A redraw request is never covered by tracks: a widget that lays its
    /// content out again each frame, beside one that publishes a track, must
    /// still have its frame drawn, or the frame shows the last layout moved
    /// rigidly along the tracks (enzUI-ICED #3660).
    pub fn is_animation_only(&self) -> bool {
        !self.tracks.is_empty()
            && self.bus.messages.is_empty()
            // Upstream's single `Invalidation`: any rebuild, including the
            // overlay recreation our pre-merge flags could not express.
            && self.invalidation == Invalidation::None
            && self.redraw_requesters == 0
    }

    /// How many widgets requested the next frame during this update
    /// (enzICED #2219).
    ///
    /// Exposed so the runtime and the live gate can report *why* a frame was
    /// not animation-only, instead of reporting an unexplained `false`.
    #[must_use]
    pub fn redraw_requesters(&self) -> usize {
        self.redraw_requesters
    }

    /// Merges the current [`Shell`] with another one by applying the given
    /// function to the messages of the latter.
    ///
    /// This method is useful for composition.
    pub fn merge<B>(&mut self, mut other: Shell<'_, B>, f: impl Fn(B) -> Message) {
        for (message, receipt) in other.bus.drain() {
            self.bus.forward(f(message), receipt);
        }

        self.invalidation = self.invalidation.max(other.invalidation);
        self.redraw_request = self.redraw_request.min(other.redraw_request);
        if let Some(at) = other.redraw_deadline {
            self.schedule_redraw_at(at);
        }
        self.event_status = self.event_status.merge(other.event_status);
        // Requesters ADD: a merged child that wants to paint is one more
        // reason this frame is not animation-only (#2219 B8).
        self.redraw_requesters = self
            .redraw_requesters
            .saturating_add(other.redraw_requesters);
        self.content_requests = self.content_requests.saturating_add(other.content_requests);

        self.repaint |= other.repaint;
        self.track_frames = self.track_frames.saturating_add(other.track_frames);
        self.requires_layout_ownership |= other.requires_layout_ownership;

        for track in other.tracks.drain(..) {
            self.record_track(track);
        }

        for animation in other.animations.drain(..) {
            self.publish_track_animation(animation);
        }

        if !self.input_method.is_enabled() && other.input_method.is_enabled() {
            self.input_method_surrounding = other.input_method_surrounding.take();
        }
        self.input_method.merge(&other.input_method);
        self.clipboard.merge(&mut other.clipboard);
        self.window_requests.append(&mut other.window_requests);
    }
}

/// A waker can be used to wake up the iced runtime and, consequently, trigger
/// wake events concurrently from widget logic.
#[derive(Clone)]
pub struct Waker {
    wake: Arc<dyn Fn() + Send + Sync + 'static>,
}

impl Waker {
    /// Creates a new [`Waker`] with the given `wake` function.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
        }
    }

    /// Creates a new [`Waker`] that does nothing.
    pub fn noop() -> Self {
        Self::new(|| {})
    }

    /// Wakes up the iced runtime as soon as possible.
    ///
    /// You normally want to call this concurrently (e.g. from a different thread).
    pub fn wake(&self) {
        (self.wake)();
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waker").finish()
    }
}

/// The diffing strategy to follow when invalidating some layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Diff {
    /// Skips the diffing step.
    Skip,
    /// Performs diffing again before layouting.
    Perform,
}

/// A channel of messages published by a [`Shell`].
///
/// enzICED: messages and their receipts are stored side by side, so a [`Bus`]
/// reads as a slice of its pending messages (`Deref<Target = [T]>`).
#[derive(Debug)]
pub struct Bus<T> {
    messages: Vec<T>,
    receipts: Vec<Receipt>,
}

impl<T> Bus<T> {
    /// Creates an empty [`Bus`].
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            receipts: Vec::new(),
        }
    }

    /// The messages pending in the [`Bus`], in order, without consuming
    /// them (enzICED: inspection and test convenience).
    pub fn messages(&self) -> &[T] {
        &self.messages
    }

    /// Drops every pending message, marking each as processed (enzICED).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.receipts.clear();
    }

    /// Removes and returns the message at `index`, marking it as processed
    /// (enzICED: test convenience).
    pub fn remove(&mut self, index: usize) -> T {
        let _ = self.receipts.remove(index);
        self.messages.remove(index)
    }

    /// Drains the messages, marking each as processed (enzICED: the
    /// receipt-free counterpart of [`drain`](Self::drain)).
    pub fn drain_messages(&mut self) -> impl Iterator<Item = T> + '_ {
        self.receipts.clear();
        self.messages.drain(..)
    }

    /// Pushes a new message to the [`Bus`].
    ///
    /// The returned [`Tracking`] can be used to determine if the message
    /// was processed.
    pub fn push(&mut self, message: T) -> Tracking {
        let receipt = Receipt::new();
        let tracking = receipt.tracking();

        self.forward(message, receipt);

        tracking
    }

    /// Forward a new message to the [`Bus`] with the given [`Receipt`].
    pub fn forward(&mut self, message: T, receipt: Receipt) {
        self.messages.push(message);
        self.receipts.push(receipt);
    }

    /// Drains the [`Bus`].
    pub fn drain(&mut self) -> impl Iterator<Item = (T, Receipt)> {
        self.messages.drain(..).zip(self.receipts.drain(..))
    }
}

impl<T> Default for Bus<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// The pending messages, in order (enzICED).
impl<T> std::ops::Deref for Bus<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.messages
    }
}

impl<T> IntoIterator for Bus<T> {
    type Item = T;
    type IntoIter = vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.messages.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Bus<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    /// Borrows the pending messages in order (enzICED).
    fn into_iter(self) -> Self::IntoIter {
        self.messages.iter()
    }
}

/// A [`Bus`] of fresh, untracked messages (enzICED: test convenience).
impl<T> FromIterator<T> for Bus<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut bus = Self::new();

        for message in iter {
            let _ = bus.push(message);
        }

        bus
    }
}

/// Appends fresh, untracked messages (enzICED: test convenience).
impl<T> Extend<T> for Bus<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for message in iter {
            let _ = self.push(message);
        }
    }
}

/// Compares the pending messages, in order (enzICED: test convenience).
impl<T: PartialEq> PartialEq for Bus<T> {
    fn eq(&self, other: &Self) -> bool {
        self.messages == other.messages
    }
}

/// Compares the pending messages, in order (enzICED: test convenience).
impl<T: PartialEq> PartialEq<Vec<T>> for Bus<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.messages == *other
    }
}

/// Proof that a message has been received.
#[derive(Debug)]
pub struct Receipt(Rc<()>);

impl Receipt {
    fn new() -> Self {
        Self(Rc::new(()))
    }

    fn tracking(&self) -> Tracking {
        Tracking(Rc::downgrade(&self.0))
    }
}

/// A message tracking returned by [`Shell::publish`].
#[derive(Debug, Clone)]
pub struct Tracking(rc::Weak<()>);

impl Tracking {
    /// Returns `true` if the message of this [`Tracking`] has been processed
    /// by `update` logic.
    pub fn is_processed(&self) -> bool {
        self.0.strong_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{NodeId, Track};

    /// A frame whose motion the published tracks carry is served along them;
    /// a redraw any widget asks for is drawn, whatever tracks other widgets
    /// published, so a widget laid out again beside a track publisher has its
    /// layout shown (enzUI-ICED #3660). A redraw scheduled for later does not
    /// keep this frame from being served.
    ///
    /// Sabotage: let published tracks cover redraw requests -- the merged
    /// redraw is served as an animation-only frame.
    #[test]
    fn a_redraw_is_never_covered_by_other_widgets_tracks() {
        let window = crate::window::Headless;
        let mut bus = Bus::new();
        let mut shell: Shell<'_, ()> = Shell::new(&window, Waker::noop(), &mut bus);
        shell.publish_track(Track::identity(NodeId(7)));
        shell.publish_track(Track::identity(NodeId(8)));
        shell.request_track_frame();
        shell.request_redraw_at(window::RedrawRequest::At(
            crate::time::Instant::now() + crate::time::Duration::from_secs(1),
        ));
        assert!(
            shell.is_animation_only(),
            "control: motion the tracks carry is served along them"
        );
        assert!(!shell.is_repainting());

        let mut bus = Bus::new();
        let mut child: Shell<'_, ()> = Shell::new(&window, Waker::noop(), &mut bus);
        child.request_redraw();
        shell.merge(child, |()| ());
        assert!(shell.is_repainting(), "a merged redraw is kept");
        assert!(
            !shell.is_animation_only(),
            "a redraw is drawn whatever tracks were published"
        );
    }
}

/// Where the UI thread's next frame was asked for, recorded only when
/// `ICED_SPAN_TRACE` is set, so a trace can name the widget that keeps a
/// window waking.
pub mod wake_trace {
    use crate::window::RedrawRequest;
    use std::cell::Cell;
    use std::panic::Location;
    use std::sync::OnceLock;

    type Noted = (RedrawRequest, &'static Location<'static>);

    thread_local! {
        static EARLIEST: Cell<Option<Noted>> = const { Cell::new(None) };
    }

    /// Whether requests are being recorded.
    pub fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var_os("ICED_SPAN_TRACE").is_some())
    }

    /// Notes a request made by the caller's caller, keeping the earliest.
    #[track_caller]
    pub(super) fn note(request: RedrawRequest) {
        if !enabled() {
            return;
        }
        let at = Location::caller();
        EARLIEST.with(|earliest| {
            if earliest.get().is_none_or(|(noted, _)| request < noted) {
                earliest.set(Some((request, at)));
            }
        });
    }

    /// The earliest request noted on this thread since the last take, and
    /// the source line that made it.
    pub fn take() -> Option<Noted> {
        EARLIEST.with(Cell::take)
    }
}
