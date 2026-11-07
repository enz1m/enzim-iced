// Modified by enzUI contributors. See FORK.md.
//! Live Wayland `wp_presentation` feedback for iced windows (enzICED #2219).
//!
//! `SurfaceTexture::present()` returning proves a *submission*. Whether the
//! user ever saw that frame is an asynchronous, per-frame answer that only the
//! compositor can give, and it may be **`discarded`**. Everything above this
//! module -- lane cadence, reversal anchors, the evidence gate -- was
//! previously reading a submission timestamp and calling it a visible frame.
//!
//! # Why a guest queue on winit's own connection
//!
//! Wayland object IDs are connection-local, so a second `connect_to_env()`
//! cannot issue feedback for winit's `wl_surface` and may raise a protocol
//! error. Winit exposes no usable Wayland API (`WindowExtWayland` is empty at
//! the pinned revision) but it does expose raw display/surface pointers, and
//! `wayland-backend` documents `Backend::from_foreign_display` for exactly
//! this. So this module makes its event queue on the display's
//! [`GuestConnection`](crate::guest::GuestConnection), a second *queue* on the
//! same `wl_display`, never a second connection, dispatched on the guest's
//! reader thread.
//!
//! A dedicated queue is also required rather than merely convenient: feedback
//! dispatched on winit's main queue would be deferred for the whole of the
//! deterministic 100 ms UI-thread stall the issue's control depends on.
//!
//! # Ownership rules this module obeys
//!
//! 1. The [`winit::event_loop::OwnedDisplayHandle`] outlives the foreign
//!    backend.
//! 2. The `Arc<winit::window::Window>` outlives every reconstructed
//!    `WlSurface` proxy.
//! 3. Winit remains the sole owner of its `WlSurface`: this module never
//!    manages or destroys it.
//! 4. One guest backend per display, shared by every window on it.
//! 5. Both raw handles are confirmed to be Wayland handles before any `unsafe`.

use crate::graphics::lane::{PendingSubmission, PresentationObserver};
use crate::graphics::presentation::{
    FrameKey, FrameOwner, Outcome, PresentationSink, Resolved, SubmittedFrame,
};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
// Only the `performance-observer` stage bridge needs it (enzICED #2219).
#[cfg(feature = "performance-observer")]
use std::time::Instant;

/// Output identities accumulated for one `wp_presentation_feedback` object.
///
/// `sync_output` events for different in-flight feedback objects may interleave,
/// so this storage is deliberately per-object and cloneable into that object's
/// Wayland user data. IDs are sorted and deduplicated before the terminal
/// `presented` event consumes them.
#[derive(Debug, Clone, Default)]
pub struct FeedbackOutputs(Arc<Mutex<Vec<u32>>>);

impl FeedbackOutputs {
    /// Records one `wl_output` protocol ID reported for this feedback object.
    pub fn note(&self, output_id: u32) {
        if let Ok(mut outputs) = self.0.lock() {
            outputs.push(output_id);
            outputs.sort_unstable();
            outputs.dedup();
        }
    }

    /// Takes an immutable snapshot for the presentation record.
    pub fn snapshot(&self) -> Arc<[u32]> {
        self.0
            .lock()
            .map(|outputs| Arc::from(outputs.as_slice()))
            .unwrap_or_else(|_| Arc::from(&[][..]))
    }
}

/// Per-window presentation state shared between the committing threads and the
/// protocol dispatcher.
///
/// The UI thread and the lane worker both commit for the same window, so the
/// submitted-sequence counter is one atomic shared by both: a sequence is
/// allocated *inside* the pre-commit hook, after acquisition succeeded, so a
/// failed acquisition consumes nothing and leaves no pending identity.
#[derive(Debug)]
pub struct WindowPresentation {
    window_id: u64,
    surface_generation: AtomicU64,
    ownership_generation: AtomicU64,
    sequence: AtomicU64,
    sink: Arc<PresentationSink>,
    /// The protocol request side. `None` on a build or session with no
    /// `wp_presentation`, in which case every frame stays `SurfacePresented`.
    requester: Mutex<Option<Arc<dyn FeedbackRequester>>>,
}

/// Issues one `wp_presentation.feedback` request for a registered frame.
///
/// Behind a trait so the request/commit ORDERING can be proven without a
/// compositor: the live implementation talks to Wayland, the gate's records
/// the exact call sequence. Both go through the same
/// [`WindowPresentation::pre_commit`] body, so the gate exercises the shipped
/// ordering rather than a copy of it.
pub trait FeedbackRequester: std::fmt::Debug + Send + Sync + 'static {
    /// Requests feedback for the exact immutable submitted frame. Returns
    /// `false` when the request could not be issued at all (dead proxy, lost
    /// connection), which must downgrade the frame to `SurfacePresented`
    /// rather than leave it pending forever.
    fn request(&self, frame: SubmittedFrame) -> bool;

    /// Cancels callbacks owned by a retired window without waiting for native completion.
    fn cancel(&self, _window_id: u64) {}
}

impl WindowPresentation {
    /// Creates per-window state. `requester` is `None` when the session has no
    /// `wp_presentation` global.
    pub fn new(window_id: u64, requester: Option<Arc<dyn FeedbackRequester>>) -> Self {
        Self {
            window_id,
            // Generation 0 is "never configured". A real surface starts at 1 so
            // a frame committed before the first configure cannot share an
            // identity with one committed after it.
            surface_generation: AtomicU64::new(1),
            ownership_generation: AtomicU64::new(1),
            sequence: AtomicU64::new(0),
            sink: Arc::new(PresentationSink::default()),
            requester: Mutex::new(requester),
        }
    }

    /// The sink holding pending and resolved frames for this window.
    pub fn sink(&self) -> &Arc<PresentationSink> {
        &self.sink
    }

    /// Whether compositor feedback can be requested at all for this window.
    pub fn has_protocol(&self) -> bool {
        self.requester
            .lock()
            .is_ok_and(|requester| requester.is_some())
    }

    /// The surface generation currently in force.
    pub fn surface_generation(&self) -> u64 {
        self.surface_generation.load(Ordering::Acquire)
    }

    /// The ownership generation currently in force.
    pub fn ownership_generation(&self) -> u64 {
        self.ownership_generation.load(Ordering::Acquire)
    }

    /// Records a resize, scale change, surface loss or recreation.
    ///
    /// Feedback still in flight for the previous surface remains honest
    /// telemetry but can no longer steer this window.
    pub fn invalidate_surface(&self) -> u64 {
        self.surface_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Records a **successful** UI <-> lane transfer.
    ///
    /// Only a successful one: a refused hand over changes nothing, so a frame
    /// committed either side of a refusal keeps the same ownership generation
    /// and stays joinable.
    pub fn transfer_ownership(&self) -> u64 {
        self.ownership_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Returns the last allocated pre-commit sequence, including refused registrations or requests.
    pub fn submitted_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    /// The pre-commit body, shared by the UI thread and the lane worker.
    ///
    /// Order is load-bearing and is asserted by the ordering gate:
    /// allocate sequence -> register identity -> request feedback -> return,
    /// and only then does the caller commit. A registration the sink refuses
    /// (at capacity, or a duplicate identity) issues **no** request, because a
    /// request with no pending identity is a callback that can only be joined
    /// wrongly or not at all.
    pub fn pre_commit(&self, submission: PendingSubmission<'_>) -> Option<FrameKey> {
        // Keep the requester lock through registration and request. Teardown
        // takes the same lock before aborting pending identities, so an
        // already-running lane callback cannot clone a requester, resume after
        // shutdown, and reintroduce pending feedback.
        let Ok(requester) = self.requester.lock() else {
            self.sink.record_unfeedbacked();
            return None;
        };
        let Some(requester) = requester.as_ref() else {
            // No protocol. Count it, so the evidence can say how many frames
            // are honestly `SurfacePresented` rather than leaving a silent gap.
            self.sink.record_unfeedbacked();
            return None;
        };

        let key = FrameKey {
            window_id: self.window_id,
            surface_generation: self.surface_generation.load(Ordering::Acquire),
            ownership_generation: self.ownership_generation.load(Ordering::Acquire),
            scene_generation: submission.scene_generation,
            submitted_sequence: self.sequence.fetch_add(1, Ordering::AcqRel) + 1,
        };

        let tracks = submission.tracks.to_vec();

        let mut frame = SubmittedFrame::new(key, submission.sampled_at, submission.owner, tracks);
        frame.layout = submission.layout;

        if !self.sink.register(frame.clone()) {
            return None;
        }

        if !requester.request(frame) {
            // The proxy was inert or the connection died between registering
            // and requesting. Resolve it now rather than leaving a pending
            // identity a later callback could be misjoined to.
            let _ = self.sink.complete(key, Outcome::Aborted);
            None
        } else {
            Some(key)
        }
    }

    /// Joins a completed feedback event and returns the immutable submission
    /// the callback resolved.
    ///
    /// The returned record carries the owner and exact sampled track
    /// generations that were registered before commit. The Wayland dispatcher
    /// uses it for the presentation trace; it must never reconstruct those
    /// fields from callback arrival order.
    pub fn complete(&self, frame: &SubmittedFrame, outcome: Outcome) -> Option<Resolved> {
        self.sink.complete_exact(frame, outcome).ok()
    }

    /// Key-only completion used by deterministic protocol-ordering tests.
    /// Production Wayland callbacks use [`Self::complete`] and therefore also
    /// verify owner, sampled instant, and all track generations.
    pub fn complete_key(&self, key: FrameKey, outcome: Outcome) -> Option<Resolved> {
        self.sink.complete(key, outcome).ok()
    }

    /// Drops the requester, aborts pending feedback and releases the last presented layout.
    /// Late registrations are refused even while external sink handles remain alive.
    pub fn shutdown(&self) {
        let requester = self
            .requester
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(requester) = requester {
            requester.cancel(self.window_id);
        }
        let _ = self.sink.close();
    }

    /// The instant of the last frame the compositor confirmed it SHOWED, for
    /// the surface, owner, scene generation **and track generations** currently
    /// in force (enzICED #2219).
    ///
    /// This is `sampled_at` of that frame -- the value that was on screen --
    /// not the arrival time of its callback. A discarded frame never appears
    /// here, which is what stops a reversal anchoring on a picture nobody saw.
    ///
    /// `active` is the `(node, generation)` set the effects have published to
    /// the lane for the travel that is running now. A retarget changes those
    /// generations and nothing else, so without them a callback for the frame
    /// *before* the retarget would anchor the travel *after* it.
    pub fn last_presented_at(
        &self,
        scene_generation: u64,
        active: &[crate::graphics::presentation::TrackGeneration],
    ) -> Option<std::time::Instant> {
        self.sink
            .active_anchor(
                self.surface_generation(),
                self.ownership_generation(),
                // A callback for the scene replaced while this travel was in
                // flight remains valid telemetry, but it must never steer the
                // current picture backwards.
                scene_generation,
                active,
            )
            .map(|(frame, _)| frame.sampled_at)
    }

    /// Returns allocation data only for the exact currently confirmed scene and travel.
    ///
    /// Semantic collection requires source-validated geometry projection; native input
    /// additionally requires every declared editor and scroll adapter to reconcile.
    pub fn presented_layout(
        &self,
        scene_generation: u64,
        active: &[crate::graphics::presentation::TrackGeneration],
    ) -> Option<(FrameKey, Arc<crate::core::renderer::presentation::Sample>)> {
        let (frame, _) = self.sink.active_anchor(
            self.surface_generation(),
            self.ownership_generation(),
            scene_generation,
            active,
        )?;
        if frame.key.window_id != self.window_id {
            return None;
        }
        Some((frame.key, frame.layout?))
    }

    /// Compositor-stated refresh period, or `None`.
    pub fn refresh(&self) -> Option<std::time::Duration> {
        self.sink.refresh()
    }

    /// The vblank grid of this window's surface, from its newest presented
    /// frame (enzUI-ICED #3082), or `None` without the protocol, a stated
    /// refresh, or a readable presentation clock.
    ///
    /// `ICED_DISABLE_PRESENTATION_PACING` (presence, any value) turns the grid
    /// off, so every frame samples at wake as before: a field rollback and
    /// the A/B arm of the live cadence measurement.
    pub fn presentation_grid(&self) -> Option<crate::graphics::presentation::PresentationGrid> {
        static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DISABLED.get_or_init(|| std::env::var_os("ICED_DISABLE_PRESENTATION_PACING").is_some())
        {
            return None;
        }
        let clock_id = self.sink.last_clock_id()?;
        let now = std::time::Instant::now();
        let clock_now = presentation_clock_now(clock_id)?;
        self.sink.grid(now, clock_now)
    }

    /// Drains joined results for the evidence stream.
    pub fn drain(&self) -> Vec<Resolved> {
        self.sink.drain()
    }
}

/// Adapts [`WindowPresentation`] to the lane's pre-commit hook.
#[derive(Debug)]
pub struct LaneObserver(Arc<WindowPresentation>);

impl LaneObserver {
    /// Wraps per-window presentation state as a lane observer.
    pub fn new(presentation: Arc<WindowPresentation>) -> Self {
        Self(presentation)
    }
}

impl PresentationObserver for LaneObserver {
    fn pre_commit(&self, submission: PendingSubmission<'_>) -> Option<FrameKey> {
        self.0.pre_commit(submission)
    }

    /// The observer is installed for every window, including X11 and Wayland
    /// compositors with no `wp_presentation` global. Without the protocol,
    /// `pre_commit` only counts the unfeedbacked frame and returns before it
    /// reads a single track, so the lane must not pay for the join identity
    /// there (enzICED #2219).
    fn needs_track_generations(&self) -> bool {
        self.0.has_protocol()
    }

    fn presentation_grid(&self) -> Option<crate::graphics::presentation::PresentationGrid> {
        self.0.presentation_grid()
    }
}

/// Reads the POSIX clock `wp_presentation` timestamps are in, so a stamp's
/// age can carry it into `Instant` (enzUI-ICED #3082). Only the clocks a
/// compositor may choose are read; any other id is `None`.
#[cfg(target_os = "linux")]
fn presentation_clock_now(clock_id: u32) -> Option<std::time::Duration> {
    use rustix::time::{ClockId, clock_gettime};

    let clock = match clock_id {
        0 => ClockId::Realtime,
        1 => ClockId::Monotonic,
        4 => ClockId::MonotonicRaw,
        7 => ClockId::Boottime,
        _ => return None,
    };
    let now = clock_gettime(clock);
    Some(std::time::Duration::new(
        u64::try_from(now.tv_sec).ok()?,
        u32::try_from(now.tv_nsec).ok()?,
    ))
}

#[cfg(not(target_os = "linux"))]
fn presentation_clock_now(_clock_id: u32) -> Option<std::time::Duration> {
    None
}

/// Records renderer identity from the compositor instance itself.
///
/// This is opt-in evidence and avoids scraping logging text, which can be
/// disabled or reformatted independently of the backend actually selected.
pub fn trace_information(information: &crate::graphics::compositor::Information) {
    use std::io::Write as _;

    let Some(path) = std::env::var_os("ICED_PRESENTATION_TRACE") else {
        return;
    };
    let Ok(mut trace) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(
        trace,
        "{{\"record\":\"graphics\",\"adapter\":{:?},\"backend\":{:?},\"device\":{:?}}}",
        information.adapter, information.backend, information.device,
    );
}

/// One UI-presented frame's wake timing and why it was not retained, as an
/// `ICED_PRESENTATION_TRACE` record (enzUI-ICED #3396).
///
/// It joins the `presentation` records on `window` and `submitted_sequence`.
/// `requested_to_wake_us` runs from the first redraw request after the
/// previous frame to the event-loop wake that served it, and
/// `wake_to_submit_us` from that wake to the moment the frame was handed to the
/// compositor. `refusal` is the renderer's latest capture or program refusal,
/// or `null` when the frame raised none.
pub fn ui_frame_record(
    key: &FrameKey,
    requested: Option<std::time::Instant>,
    wake: Option<std::time::Instant>,
    submitted: std::time::Instant,
    refusal: Option<crate::core::renderer::presentation::Error>,
) -> String {
    let micros = |from: Option<std::time::Instant>, to: Option<std::time::Instant>| match (from, to)
    {
        (Some(from), Some(to)) => to.saturating_duration_since(from).as_micros().to_string(),
        _ => "null".to_owned(),
    };
    let refusal = refusal.map_or_else(|| "null".to_owned(), |error| format!("\"{error:?}\""));

    format!(
        "{{\"record\":\"ui_frame\",\"window\":{},\"surface_generation\":{},\"scene_generation\":{},\"submitted_sequence\":{},\"requested_to_wake_us\":{},\"wake_to_submit_us\":{},\"refusal\":{}}}",
        key.window_id,
        key.surface_generation,
        key.scene_generation,
        key.submitted_sequence,
        micros(requested, wake),
        micros(wake, Some(submitted)),
        refusal,
    )
}

/// Appends [`ui_frame_record`] to `ICED_PRESENTATION_TRACE`, when it is set.
pub fn trace_ui_frame(record: impl FnOnce() -> String) {
    use std::io::Write as _;

    let Some(path) = std::env::var_os("ICED_PRESENTATION_TRACE") else {
        return;
    };
    if let Ok(mut trace) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(trace, "{}", record());
    }
}

/// Builds the UI thread's own pre-commit submission for a frame it is about to
/// commit itself.
///
/// The UI path has no `TrackAnimation` list of its own, so its sampled tracks
/// are recorded with the generations they were published with, and a static
/// frame records an empty track list -- not a fabricated one.
pub fn ui_submission(
    scene_generation: u64,
    sampled_at: std::time::Instant,
    tracks: &[crate::graphics::presentation::TrackGeneration],
) -> PendingSubmission<'_> {
    PendingSubmission {
        layout: None,
        owner: FrameOwner::Ui,
        scene_generation,
        sampled_at,
        tracks,
    }
}

// ---------------------------------------------------------------------------
// Live Wayland implementation
// ---------------------------------------------------------------------------

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "wayland"
))]
mod wayland {
    use super::{FeedbackOutputs, FeedbackRequester, Outcome, SubmittedFrame, WindowPresentation};
    use crate::graphics::presentation::PresentationStamp;
    use crate::guest::{GuestConnection, Pump};

    use std::collections::HashMap;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, Weak};

    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle,
        backend::ObjectId,
        globals::{GlobalList, GlobalListContents, registry_queue_init},
    };
    use wayland_protocols::wp::presentation_time::client::{
        wp_presentation::{self, WpPresentation},
        wp_presentation_feedback::{self, WpPresentationFeedback},
    };

    /// The per-display hub: one queue on the display's guest connection,
    /// dispatched on its reader thread, and one bound `wp_presentation`.
    #[derive(Debug)]
    pub struct PresentationHub {
        connection: Connection,
        presentation: WpPresentation,
        handle: QueueHandle<HubState>,
        state: Arc<HubShared>,
        pump: Pump,
        _guest: Arc<GuestConnection>,
        // Shared with the requesters so none outlives the display.
        _display: Arc<winit::event_loop::OwnedDisplayHandle>,
    }

    const MAX_PRESENTATION_WINDOWS: usize = 64;
    const MAX_PENDING_FEEDBACKS: usize = 256;
    const MAX_PENDING_FEEDBACK_BYTES: usize = 64 * 1024 * 1024;
    const RETAINED_LAYOUT_CHARGE: usize = 8 * 1024 * 1024;

    /// State the dispatcher thread mutates and the committing threads read.
    #[derive(Debug)]
    pub struct HubShared {
        /// Per-window sinks, so a callback reaches the right window's join.
        windows: Mutex<HashMap<u64, Arc<WindowPresentation>>>,
        callbacks: Arc<FeedbackCallbacks>,
        retired: AtomicBool,
        /// The compositor's clock id, learned from `wp_presentation.clock_id`.
        clock_id: Mutex<Option<u32>>,
    }

    impl Default for HubShared {
        fn default() -> Self {
            Self {
                windows: Mutex::new(HashMap::new()),
                callbacks: Arc::new(FeedbackCallbacks::new(
                    MAX_PENDING_FEEDBACKS,
                    MAX_PENDING_FEEDBACK_BYTES,
                )),
                retired: AtomicBool::new(false),
                clock_id: Mutex::new(None),
            }
        }
    }

    impl HubShared {
        fn register_window(&self, window_id: u64, presentation: Arc<WindowPresentation>) -> bool {
            if self.retired.load(Ordering::Acquire) {
                return false;
            }
            let previous = {
                let mut windows = self
                    .windows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if self.retired.load(Ordering::Acquire)
                    || (!windows.contains_key(&window_id)
                        && windows.len() >= MAX_PRESENTATION_WINDOWS)
                {
                    return false;
                }
                windows.insert(window_id, presentation)
            };
            if let Some(previous) = previous {
                let _ = self.callbacks.cancel_window(window_id);
                previous.shutdown();
            }
            true
        }

        fn unregister_window(&self, window_id: u64) -> bool {
            let removed = self
                .windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&window_id);
            let found = removed.is_some();
            if let Some(presentation) = removed {
                presentation.shutdown();
            }
            let _ = self.callbacks.cancel_window(window_id);
            found
        }

        fn retire(&self) {
            self.retired.store(true, Ordering::Release);
            let _ = self.callbacks.close();
            let windows = std::mem::take(
                &mut *self
                    .windows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            for presentation in windows.into_values() {
                presentation.shutdown();
            }
        }
    }

    /// Dispatcher-visible state.
    #[derive(Debug)]
    pub struct HubState {
        shared: Arc<HubShared>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct FeedbackToken(u64);

    type FeedbackCallback = Box<dyn FnOnce(Outcome) + Send>;

    struct PendingFeedback {
        window_id: u64,
        retained_bytes: usize,
        callback: FeedbackCallback,
    }

    struct FeedbackCallbackState {
        closed: bool,
        next: u64,
        retained_bytes: usize,
        pending: HashMap<FeedbackToken, PendingFeedback>,
    }

    struct FeedbackCallbacks {
        capacity: usize,
        byte_capacity: usize,
        state: Mutex<FeedbackCallbackState>,
    }

    impl std::fmt::Debug for FeedbackCallbacks {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            formatter
                .debug_struct("FeedbackCallbacks")
                .field("capacity", &self.capacity)
                .field("byte_capacity", &self.byte_capacity)
                .field("pending", &state.pending.len())
                .field("retained_bytes", &state.retained_bytes)
                .finish()
        }
    }

    impl FeedbackCallbacks {
        fn new(capacity: usize, byte_capacity: usize) -> Self {
            Self {
                capacity: capacity.max(1),
                byte_capacity,
                state: Mutex::new(FeedbackCallbackState {
                    closed: false,
                    next: 1,
                    retained_bytes: 0,
                    pending: HashMap::new(),
                }),
            }
        }

        fn register(
            &self,
            window_id: u64,
            retained_bytes: usize,
            callback: impl FnOnce(Outcome) + Send + 'static,
        ) -> Option<FeedbackToken> {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let next_bytes = state.retained_bytes.checked_add(retained_bytes)?;
            if state.closed
                || state.pending.len() >= self.capacity
                || next_bytes > self.byte_capacity
            {
                return None;
            }
            let token = FeedbackToken(state.next);
            state.next = state.next.checked_add(1)?;
            state.retained_bytes = next_bytes;
            let replaced = state.pending.insert(
                token,
                PendingFeedback {
                    window_id,
                    retained_bytes,
                    callback: Box::new(callback),
                },
            );
            debug_assert!(replaced.is_none());
            Some(token)
        }

        fn complete(&self, token: FeedbackToken, outcome: Outcome) -> bool {
            let callback = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(pending) = state.pending.remove(&token) else {
                    return false;
                };
                state.retained_bytes = state.retained_bytes.saturating_sub(pending.retained_bytes);
                pending.callback
            };
            let _ = catch_unwind(AssertUnwindSafe(|| callback(outcome)));
            true
        }

        fn cancel(&self, token: FeedbackToken) -> bool {
            let callback = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(pending) = state.pending.remove(&token) else {
                    return false;
                };
                state.retained_bytes = state.retained_bytes.saturating_sub(pending.retained_bytes);
                pending.callback
            };
            drop(callback);
            true
        }

        fn cancel_window(&self, window_id: u64) -> usize {
            let callbacks = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let tokens = state
                    .pending
                    .iter()
                    .filter_map(|(token, pending)| {
                        (pending.window_id == window_id).then_some(*token)
                    })
                    .collect::<Vec<_>>();
                let mut callbacks = Vec::with_capacity(tokens.len());
                for token in tokens {
                    if let Some(pending) = state.pending.remove(&token) {
                        state.retained_bytes =
                            state.retained_bytes.saturating_sub(pending.retained_bytes);
                        callbacks.push(pending.callback);
                    }
                }
                callbacks
            };
            let count = callbacks.len();
            drop(callbacks);
            count
        }

        fn close(&self) -> usize {
            let callbacks = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.closed = true;
                state.retained_bytes = 0;
                std::mem::take(&mut state.pending)
            };
            let count = callbacks.len();
            drop(callbacks);
            count
        }

        #[cfg(test)]
        fn stats(&self) -> (usize, usize, bool) {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.pending.len(), state.retained_bytes, state.closed)
        }
    }

    fn feedback_retained_bytes(frame: &SubmittedFrame) -> usize {
        std::mem::size_of::<SubmittedFrame>()
            .saturating_add(frame.tracks.len().saturating_mul(std::mem::size_of::<
                crate::graphics::presentation::TrackGeneration,
            >()))
            .saturating_add(
                usize::from(frame.layout.is_some()).saturating_mul(RETAINED_LAYOUT_CHARGE),
            )
    }

    #[derive(Debug)]
    struct FeedbackLease {
        token: FeedbackToken,
        callbacks: Weak<FeedbackCallbacks>,
    }

    impl Drop for FeedbackLease {
        fn drop(&mut self) {
            if let Some(callbacks) = self.callbacks.upgrade() {
                let _ = callbacks.complete(self.token, Outcome::Aborted);
            }
        }
    }

    /// The bounded token one feedback object carries; retained frame ownership stays in the hub.
    #[derive(Debug, Clone)]
    pub struct FeedbackData {
        lease: Arc<FeedbackLease>,
        /// `sync_output` may arrive zero or more times before `presented` for
        /// this exact feedback object. Keep the set on the object itself: a
        /// display-global accumulator would attach outputs to the wrong frame
        /// as soon as multiple feedback objects are in flight.
        output_ids: FeedbackOutputs,
    }

    impl PresentationHub {
        /// Reconstitutes a guest queue on winit's display and binds
        /// `wp_presentation`.
        ///
        /// Returns `None` -- never an error the caller might ignore -- when the
        /// session is not Wayland or the compositor does not advertise the
        /// protocol. Every frame then stays honestly `SurfacePresented`.
        pub fn new(display: winit::event_loop::OwnedDisplayHandle) -> Option<Arc<Self>> {
            let guest = GuestConnection::of(&display)?;
            let connection = guest.connection().clone();

            let (globals, queue): (GlobalList, EventQueue<HubState>) =
                registry_queue_init(&connection).ok()?;
            let handle = queue.handle();

            let state = Arc::new(HubShared::default());

            // Version 1..=2; a compositor without the global returns Err and we
            // downgrade honestly rather than pretending.
            let presentation: WpPresentation = globals.bind(&handle, 1..=2, ()).ok()?;

            // The queue is dispatched on the guest's reader thread right after
            // each read, so feedback lands while the UI thread is busy.
            let mut queue = queue;
            let mut hub_state = HubState {
                shared: Arc::clone(&state),
            };
            let pump = guest.pump(move || queue.dispatch_pending(&mut hub_state).is_ok());

            Some(Arc::new(Self {
                connection,
                presentation,
                handle,
                state,
                pump,
                _guest: guest,
                _display: Arc::new(display),
            }))
        }

        /// Registers bounded presentation state for one live window.
        pub fn register(&self, window_id: u64, presentation: Arc<WindowPresentation>) -> bool {
            self.state.register_window(window_id, presentation)
        }

        /// Forgets a window and aborts every frame it still had in flight, so
        /// teardown leaves no submission unresolved.
        pub fn unregister(&self, window_id: u64) {
            let _ = self.state.unregister_window(window_id);
        }

        /// Builds the requester a window installs.
        pub fn requester(self: &Arc<Self>, surface: WlSurface) -> Arc<dyn FeedbackRequester> {
            Arc::new(HubRequester {
                hub: Arc::downgrade(self),
                surface,
                _display: Arc::clone(&self._display),
            })
        }

        /// Reconstructs a `WlSurface` proxy for a winit window on the guest
        /// connection.
        ///
        /// Rule 3: the returned proxy is a *view*. It is never `destroy()`ed
        /// and never `manage_object`ed; winit stays the owner. Rule 2 is the
        /// caller's: it must keep the `Arc<Window>` alive at least as long.
        pub fn surface_for(&self, window: &winit::window::Window) -> Option<WlSurface> {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            let ptr = match window.window_handle().ok()?.as_raw() {
                RawWindowHandle::Wayland(handle) => handle.surface.as_ptr(),
                _ => return None,
            };

            // SAFETY: `ptr` is winit's live `wl_surface` for a window the
            // caller keeps alive, and the interface is checked against the
            // proxy's own class by `ObjectId::from_ptr`, which returns
            // `InvalidId` on a mismatch rather than misinterpreting memory.
            #[allow(unsafe_code)]
            let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), ptr.cast()) }.ok()?;

            WlSurface::from_id(&self.connection, id).ok()
        }

        /// Retires registrations and callbacks, then stops dispatching the
        /// hub's queue.
        pub fn shutdown(&self) {
            self.state.retire();
            self.pump.remove();
        }
    }

    impl Drop for PresentationHub {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    #[derive(Debug)]
    struct HubRequester {
        hub: Weak<PresentationHub>,
        surface: WlSurface,
        _display: Arc<winit::event_loop::OwnedDisplayHandle>,
    }

    impl FeedbackRequester for HubRequester {
        fn request(&self, frame: SubmittedFrame) -> bool {
            let Some(hub) = self.hub.upgrade() else {
                return false;
            };
            if !self.surface.is_alive() {
                return false;
            }

            let window_id = frame.key.window_id;
            let shared = Arc::downgrade(&hub.state);
            let retained_bytes = feedback_retained_bytes(&frame);
            let Some(token) =
                hub.state
                    .callbacks
                    .register(window_id, retained_bytes, move |outcome| {
                        let Some(shared) = shared.upgrade() else {
                            return;
                        };
                        let presentation = shared
                            .windows
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get(&window_id)
                            .cloned();
                        if let Some(presentation) = presentation
                            && let Some(resolved) = presentation.complete(&frame, outcome)
                        {
                            trace_resolved(&resolved);
                        }
                    })
            else {
                return false;
            };
            let callbacks = Arc::downgrade(&hub.state.callbacks);
            let feedback = hub.presentation.feedback(
                &self.surface,
                &hub.handle,
                FeedbackData {
                    lease: Arc::new(FeedbackLease { token, callbacks }),
                    output_ids: FeedbackOutputs::default(),
                },
            );

            // `feedback` returns an inert proxy rather than a Result when the
            // constructor failed, so this is checked instead of assumed.
            if !feedback.is_alive() {
                let _ = hub.state.callbacks.cancel(token);
                return false;
            }

            // The request must be in the stream BEFORE the caller commits.
            if hub.connection.flush().is_err() {
                let _ = hub.state.callbacks.cancel(token);
                false
            } else {
                true
            }
        }

        fn cancel(&self, window_id: u64) {
            if let Some(hub) = self.hub.upgrade() {
                let _ = hub.state.callbacks.cancel_window(window_id);
            }
        }
    }

    impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, GlobalListContents> for HubState {
        fn event(
            _state: &mut Self,
            _proxy: &wayland_client::protocol::wl_registry::WlRegistry,
            _event: wayland_client::protocol::wl_registry::Event,
            _data: &GlobalListContents,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            // The hub binds exactly one global and cares about no others. A
            // compositor that removes `wp_presentation` mid-session simply
            // stops answering, and pending frames abort on teardown.
        }
    }

    impl Dispatch<WpPresentation, ()> for HubState {
        fn event(
            state: &mut Self,
            _proxy: &WpPresentation,
            event: wp_presentation::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            if let wp_presentation::Event::ClockId { clk_id } = event
                && let Ok(mut clock) = state.shared.clock_id.lock()
            {
                *clock = Some(clk_id);
            }
        }
    }

    impl Dispatch<WpPresentationFeedback, FeedbackData> for HubState {
        fn event(
            state: &mut Self,
            _proxy: &WpPresentationFeedback,
            event: wp_presentation_feedback::Event,
            data: &FeedbackData,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            use wp_presentation_feedback::Event;

            let clock_id = state
                .shared
                .clock_id
                .lock()
                .ok()
                .and_then(|clock| *clock)
                .unwrap_or(0);

            let outcome = match event {
                Event::SyncOutput { output } => {
                    // `sync_output` belongs to THIS feedback object. Several
                    // feedbacks may be in flight and callbacks may be
                    // interleaved, so accumulate on `FeedbackData`, never in a
                    // queue-global scratch slot.
                    data.output_ids.note(output.id().protocol_id());
                    return;
                }
                Event::Presented {
                    tv_sec_hi,
                    tv_sec_lo,
                    tv_nsec,
                    refresh,
                    seq_hi,
                    seq_lo,
                    flags,
                } => {
                    let output_ids = data.output_ids.snapshot();

                    Outcome::Presented(PresentationStamp {
                        clock_id,
                        seconds: (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                        nanoseconds: tv_nsec,
                        refresh_ns: refresh,
                        sequence: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                        // Unknown future bits are PRESERVED, not rejected.
                        flags_raw: flags.into(),
                        output_ids,
                    })
                }
                Event::Discarded => Outcome::Discarded,
                _ => return,
            };

            let _ = state.shared.callbacks.complete(data.lease.token, outcome);
        }
    }

    /// Writes one callback-derived record for live evidence. This is opt-in and
    /// runs only on the dedicated Wayland dispatcher thread. In particular, a
    /// `SurfaceTexture::present()` return can never write this record.
    fn trace_resolved(resolved: &crate::graphics::presentation::Resolved) {
        use crate::graphics::presentation::{Outcome, Quality};
        use std::io::Write as _;

        let Some(path) = std::env::var_os("ICED_PRESENTATION_TRACE") else {
            return;
        };
        let Ok(mut trace) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        else {
            return;
        };

        let frame = &resolved.frame;
        let tracks = frame
            .tracks
            .iter()
            .map(|track| format!("[{},{}]", track.node.0, track.generation))
            .collect::<Vec<_>>()
            .join(",");

        match (&resolved.quality, &resolved.outcome) {
            (Quality::CompositorPresented, Outcome::Presented(stamp)) => {
                let outputs = stamp
                    .output_ids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let _ = writeln!(
                    trace,
                    "{{\"record\":\"presentation\",\"milestone\":\"CompositorPresented\",\"window\":{},\"surface_generation\":{},\"ownership_generation\":{},\"scene_generation\":{},\"submitted_sequence\":{},\"owner\":\"{:?}\",\"tracks\":[{}],\"clock_id\":{},\"seconds\":{},\"nanoseconds\":{},\"refresh_ns\":{},\"compositor_sequence\":{},\"flags_raw\":{},\"output_ids\":[{}]}}",
                    frame.key.window_id,
                    frame.key.surface_generation,
                    frame.key.ownership_generation,
                    frame.key.scene_generation,
                    frame.key.submitted_sequence,
                    frame.owner,
                    tracks,
                    stamp.clock_id,
                    stamp.seconds,
                    stamp.nanoseconds,
                    stamp.refresh_ns,
                    stamp.sequence,
                    stamp.flags_raw,
                    outputs,
                );
            }
            (_, Outcome::Discarded) => {
                let _ = writeln!(
                    trace,
                    "{{\"record\":\"presentation\",\"milestone\":\"Discarded\",\"window\":{},\"surface_generation\":{},\"ownership_generation\":{},\"scene_generation\":{},\"submitted_sequence\":{},\"owner\":\"{:?}\",\"tracks\":[{}]}}",
                    frame.key.window_id,
                    frame.key.surface_generation,
                    frame.key.ownership_generation,
                    frame.key.scene_generation,
                    frame.key.submitted_sequence,
                    frame.owner,
                    tracks,
                );
            }
            _ => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicUsize;

        struct DropProbe(Arc<AtomicUsize>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                let _ = self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        #[test]
        fn presentation_reclamation_cancels_callback_and_drops_capture_once() {
            let callbacks = FeedbackCallbacks::new(2, 16);
            let drops = Arc::new(AtomicUsize::new(0));
            let probe = DropProbe(Arc::clone(&drops));
            let token = callbacks
                .register(7, 8, move |_| drop(probe))
                .expect("callback should be admitted");

            assert_eq!(callbacks.stats(), (1, 8, false));
            assert!(callbacks.cancel(token));
            assert!(!callbacks.cancel(token));
            assert_eq!(callbacks.stats(), (0, 0, false));
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn presentation_reclamation_feedback_data_drop_cancels_once() {
            let callbacks = Arc::new(FeedbackCallbacks::new(1, 8));
            let drops = Arc::new(AtomicUsize::new(0));
            let completions = Arc::new(AtomicUsize::new(0));
            let probe = DropProbe(Arc::clone(&drops));
            let observed = Arc::clone(&completions);
            let token = callbacks
                .register(7, 8, move |outcome| {
                    assert!(matches!(outcome, Outcome::Aborted));
                    let _ = observed.fetch_add(1, Ordering::SeqCst);
                    drop(probe);
                })
                .expect("callback should be admitted");
            let data = FeedbackData {
                lease: Arc::new(FeedbackLease {
                    token,
                    callbacks: Arc::downgrade(&callbacks),
                }),
                output_ids: FeedbackOutputs::default(),
            };
            let clone = data.clone();

            drop(data);
            assert_eq!(callbacks.stats(), (1, 8, false));
            drop(clone);
            assert_eq!(callbacks.stats(), (0, 0, false));
            assert_eq!(completions.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn presentation_reclamation_contains_callback_panic() {
            let callbacks = FeedbackCallbacks::new(2, 16);
            let completed = Arc::new(AtomicUsize::new(0));
            let later = Arc::clone(&completed);
            let first = callbacks
                .register(1, 4, |_| panic!("injected feedback callback panic"))
                .expect("first callback should be admitted");
            let second = callbacks
                .register(1, 4, move |_| {
                    let _ = later.fetch_add(1, Ordering::SeqCst);
                })
                .expect("second callback should be admitted");

            assert!(callbacks.complete(first, Outcome::Aborted));
            assert!(callbacks.complete(second, Outcome::Aborted));
            assert_eq!(completed.load(Ordering::SeqCst), 1);
            assert_eq!(callbacks.stats(), (0, 0, false));
        }

        #[test]
        fn presentation_reclamation_late_completion_cannot_resurrect_retired_window() {
            let callbacks = FeedbackCallbacks::new(2, 16);
            let completed = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&completed);
            let token = callbacks
                .register(9, 8, move |_| {
                    let _ = observed.fetch_add(1, Ordering::SeqCst);
                })
                .expect("callback should be admitted");

            assert_eq!(callbacks.cancel_window(9), 1);
            assert!(!callbacks.complete(token, Outcome::Aborted));
            assert_eq!(completed.load(Ordering::SeqCst), 0);
            assert_eq!(callbacks.stats(), (0, 0, false));
        }

        #[test]
        fn presentation_reclamation_two_windows_are_isolated() {
            let callbacks = FeedbackCallbacks::new(2, 16);
            let first_drops = Arc::new(AtomicUsize::new(0));
            let second_calls = Arc::new(AtomicUsize::new(0));
            let first_probe = DropProbe(Arc::clone(&first_drops));
            let second = Arc::clone(&second_calls);
            let first_token = callbacks
                .register(1, 8, move |_| drop(first_probe))
                .expect("first window should be admitted");
            let second_token = callbacks
                .register(2, 8, move |_| {
                    let _ = second.fetch_add(1, Ordering::SeqCst);
                })
                .expect("second window should be admitted");

            assert_eq!(callbacks.cancel_window(1), 1);
            assert!(!callbacks.complete(first_token, Outcome::Aborted));
            assert!(callbacks.complete(second_token, Outcome::Aborted));
            assert_eq!(first_drops.load(Ordering::SeqCst), 1);
            assert_eq!(second_calls.load(Ordering::SeqCst), 1);
            assert_eq!(callbacks.stats(), (0, 0, false));
        }

        #[test]
        fn presentation_reclamation_bounds_pending_count_and_bytes() {
            let callbacks = FeedbackCallbacks::new(1, 8);
            let first = callbacks
                .register(1, 8, |_| {})
                .expect("first callback should fit both limits");
            assert!(callbacks.register(2, 0, |_| {}).is_none());
            assert!(callbacks.cancel(first));
            assert!(callbacks.register(2, 9, |_| {}).is_none());
            assert_eq!(callbacks.stats(), (0, 0, false));
        }

        #[test]
        fn presentation_reclamation_hub_registration_retires_exactly_once() {
            let shared = HubShared::default();
            let presentation = Arc::new(WindowPresentation::new(31, None));
            let weak = Arc::downgrade(&presentation);

            assert!(shared.register_window(31, Arc::clone(&presentation)));
            drop(presentation);
            assert!(weak.upgrade().is_some());
            assert!(shared.unregister_window(31));
            assert!(weak.upgrade().is_none());
            assert!(!shared.unregister_window(31));

            shared.retire();
            let retired = Arc::new(WindowPresentation::new(32, None));
            assert!(!shared.register_window(32, Arc::clone(&retired)));
            assert_eq!(
                shared
                    .windows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
                0
            );
        }

        #[test]
        fn presentation_reclamation_bounds_window_registrations() {
            let shared = HubShared::default();
            let first = Arc::new(WindowPresentation::new(0, None));
            let first_weak = Arc::downgrade(&first);
            assert!(shared.register_window(0, Arc::clone(&first)));
            drop(first);

            let capacity = u64::try_from(MAX_PRESENTATION_WINDOWS)
                .expect("presentation window capacity should fit u64");
            for window_id in 1..capacity {
                let presentation = Arc::new(WindowPresentation::new(window_id, None));
                assert!(shared.register_window(window_id, presentation));
            }

            let overflow_id = capacity;
            let overflow = Arc::new(WindowPresentation::new(overflow_id, None));
            assert!(!shared.register_window(overflow_id, Arc::clone(&overflow)));
            assert_eq!(Arc::strong_count(&overflow), 1);

            let replacement = Arc::new(WindowPresentation::new(0, None));
            let replacement_weak = Arc::downgrade(&replacement);
            assert!(shared.register_window(0, replacement));
            assert!(first_weak.upgrade().is_none());

            shared.retire();
            assert!(replacement_weak.upgrade().is_none());
        }
    }
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "wayland"
))]
pub use wayland::PresentationHub;

/// Stand-in on platforms with no Wayland: there is no hub, so every frame
/// stays `SurfacePresented` and nothing pretends otherwise.
#[cfg(not(all(
    unix,
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "wayland"
)))]
#[derive(Debug)]
pub struct PresentationHub;

#[cfg(not(all(
    unix,
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "wayland"
)))]
impl PresentationHub {
    /// Always `None` off Wayland.
    pub fn new(_display: winit::event_loop::OwnedDisplayHandle) -> Option<Arc<Self>> {
        None
    }

    /// Refuses registration because no presentation protocol exists.
    pub fn register(&self, _window_id: u64, _presentation: Arc<WindowPresentation>) -> bool {
        false
    }

    /// No-op off Wayland.
    pub fn unregister(&self, _window_id: u64) {}

    /// No-op off Wayland.
    pub fn shutdown(&self) {}
}

/// Forwards `iced_runtime` stage spans into per-window stage ledgers
/// (enzICED #2219).
///
/// Gated on `performance-observer` because that is the feature that produces
/// the spans at all: without it `iced_runtime` compiles no stage timing, so a
/// bridge would forward nothing while looking wired.
#[cfg(feature = "performance-observer")]
///
/// `View`, `Diff`, `Layout` and `SceneRecord` are measured deep inside
/// `iced_runtime`, which knows nothing about frame identity. Rather than
/// duplicating those spans, this bridge subscribes to the runtime's existing
/// per-span hook and files each one against the ledger of the window that
/// produced it, as UI-thread work. `Input` and `Update` are recorded directly
/// by the event loop, which owns those spans.
#[derive(Debug, Default)]
pub struct StageBridge {
    ledgers: Mutex<std::collections::HashMap<u64, Arc<crate::graphics::presentation::StageLedger>>>,
}

#[cfg(feature = "performance-observer")]
impl StageBridge {
    /// Creates an empty bridge.
    pub fn new() -> Self {
        Self::default()
    }

    /// Routes spans of `window_id` into `ledger`.
    pub fn register(
        &self,
        window_id: u64,
        ledger: Arc<crate::graphics::presentation::StageLedger>,
    ) {
        if let Ok(mut ledgers) = self.ledgers.lock() {
            let _ = ledgers.insert(window_id, ledger);
        }
    }

    /// Stops routing spans of `window_id`.
    pub fn unregister(&self, window_id: u64) {
        if let Ok(mut ledgers) = self.ledgers.lock() {
            let _ = ledgers.remove(&window_id);
        }
    }

    /// The [`FrameStage`] a runtime stage maps to, if any.
    ///
    /// `GpuSubmit` is deliberately unmapped: it is the *UI* thread's own submit
    /// and is not one of the four render-lane stages. Mapping it would file UI
    /// work under a lane stage -- the misattribution the ledger exists to
    /// catch.
    pub fn frame_stage(
        stage: crate::runtime::performance::Stage,
    ) -> Option<crate::graphics::presentation::FrameStage> {
        use crate::graphics::presentation::FrameStage;
        use crate::runtime::performance::Stage;

        match stage {
            Stage::ViewRebuild => Some(FrameStage::View),
            Stage::TreeDiff => Some(FrameStage::Diff),
            Stage::Layout => Some(FrameStage::Layout),
            Stage::DrawPrepare => Some(FrameStage::SceneRecord),
            Stage::GpuSubmit => None,
        }
    }
}

#[cfg(feature = "performance-observer")]
impl crate::runtime::performance::SpanObserver for StageBridge {
    fn span(
        &self,
        window: crate::runtime::performance::WindowId,
        stage: crate::runtime::performance::Stage,
        start: Instant,
        end: Instant,
    ) {
        let Some(stage) = Self::frame_stage(stage) else {
            return;
        };

        let Ok(ledgers) = self.ledgers.lock() else {
            return;
        };

        if let Some(ledger) = ledgers.get(&window.0) {
            ledger.record(
                crate::graphics::presentation::FrameOwner::Ui,
                stage,
                start,
                end,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Live installation of the stage bridge (enzICED #2250)
// ---------------------------------------------------------------------------

/// The default number of sealed rows a live ledger retains per window.
#[cfg(feature = "performance-observer")]
const STAGE_CAPACITY: usize = 4096;

/// Owns the live UI-thread stage instrumentation for a whole process
/// (enzICED #2250).
///
/// #2219 shipped `Window::record_stages` and [`StageBridge`], but nothing in
/// the real winit path ever installed either: the only caller was a test that
/// drove `StageBridge::span` by hand, so a live run produced no joined stage
/// evidence at all while the gate stayed green. This type is that missing
/// caller. Installing it:
///
/// 1. enables `iced_runtime`'s span collection, which is what makes
///    `View`/`Diff`/`Layout`/`SceneRecord` spans exist at all;
/// 2. registers the bridge as the runtime's **process-wide** span observer, so
///    every span the runtime already emits is forwarded without a second
///    measurement;
/// 3. hands `window::Manager::insert` a ledger for every window it creates,
///    which is also given to that window's animation lane.
///
/// Nothing is installed by default. Without an installation the accessors below
/// return `None`, `Window::stages()` is `None`, the lane is told not to measure,
/// and the whole path costs one relaxed atomic load per created window.
#[cfg(feature = "performance-observer")]
#[derive(Debug)]
pub struct StageRecorder {
    bridge: Arc<StageBridge>,
    ledgers: Mutex<std::collections::HashMap<u64, Arc<crate::graphics::presentation::StageLedger>>>,
    capacity: usize,
    attached: AtomicU64,
}

#[cfg(feature = "performance-observer")]
static RECORDER: std::sync::OnceLock<Mutex<Option<Arc<StageRecorder>>>> =
    std::sync::OnceLock::new();

#[cfg(feature = "performance-observer")]
fn recorder_slot() -> &'static Mutex<Option<Arc<StageRecorder>>> {
    RECORDER.get_or_init(|| Mutex::new(None))
}

#[cfg(feature = "performance-observer")]
impl StageRecorder {
    /// Installs live stage recording, replacing any previous installation.
    ///
    /// Returns the recorder so a harness can read the ledgers afterwards. The
    /// runtime's span observer is set to this recorder's bridge here, and
    /// nowhere else: a caller that forgot this step gets ledgers with no
    /// `view`/`diff`/`layout`/`scene_record` spans, which the ledger reports as
    /// missing rather than silently accepting.
    pub fn install(capacity: usize) -> Arc<Self> {
        let recorder = Arc::new(Self {
            bridge: Arc::new(StageBridge::new()),
            ledgers: Mutex::new(std::collections::HashMap::new()),
            capacity: capacity.max(1),
            attached: AtomicU64::new(0),
        });

        // The spans only exist while the runtime is collecting.
        if !crate::runtime::performance::is_enabled() {
            crate::runtime::performance::enable(crate::runtime::performance::Config {
                trace_capacity: 8192,
                max_windows: 16,
            });
        }

        crate::runtime::performance::observe_spans(recorder.bridge.clone());

        if let Ok(mut slot) = recorder_slot().lock() {
            *slot = Some(recorder.clone());
        }

        recorder
    }

    /// Removes live stage recording and the runtime span observer.
    pub fn uninstall() {
        crate::runtime::performance::stop_observing_spans();

        if let Ok(mut slot) = recorder_slot().lock() {
            *slot = None;
        }
    }

    /// The installed recorder, if any.
    pub fn installed() -> Option<Arc<Self>> {
        recorder_slot().lock().ok()?.clone()
    }

    /// The bridge this recorder installed as the runtime span observer.
    pub fn bridge(&self) -> &Arc<StageBridge> {
        &self.bridge
    }

    /// Creates and registers the ledger for `window_id`.
    ///
    /// Called by `window::Manager::insert` for every window it creates, which
    /// is what makes the installation *live* rather than a test affordance.
    pub fn attach(&self, window_id: u64) -> Arc<crate::graphics::presentation::StageLedger> {
        let ledger = Arc::new(crate::graphics::presentation::StageLedger::new(
            self.capacity,
        ));

        self.bridge.register(window_id, ledger.clone());

        if let Ok(mut ledgers) = self.ledgers.lock() {
            let _ = ledgers.insert(window_id, ledger.clone());
        }

        let _ = self.attached.fetch_add(1, Ordering::Relaxed);

        ledger
    }

    /// Stops recording `window_id` and drops its ledger registration.
    pub fn detach(&self, window_id: u64) {
        self.bridge.unregister(window_id);

        if let Ok(mut ledgers) = self.ledgers.lock() {
            let _ = ledgers.remove(&window_id);
        }
    }

    /// The ledger of `window_id`, if it was attached.
    pub fn ledger(
        &self,
        window_id: u64,
    ) -> Option<Arc<crate::graphics::presentation::StageLedger>> {
        self.ledgers.lock().ok()?.get(&window_id).cloned()
    }

    /// Every attached `(window_id, ledger)` pair.
    pub fn ledgers(&self) -> Vec<(u64, Arc<crate::graphics::presentation::StageLedger>)> {
        self.ledgers
            .lock()
            .map(|ledgers| {
                let mut rows: Vec<_> = ledgers
                    .iter()
                    .map(|(id, ledger)| (*id, ledger.clone()))
                    .collect();
                rows.sort_by_key(|(id, _)| *id);
                rows
            })
            .unwrap_or_default()
    }

    /// How many windows this recorder has attached a ledger to.
    ///
    /// A live run that never reached `window::Manager::insert` reads `0`, which
    /// is what makes "the observer was never installed on a real window" a loud
    /// state instead of an empty ledger.
    pub fn attached(&self) -> u64 {
        self.attached.load(Ordering::Relaxed)
    }
}

/// Attaches the installed recorder's ledger to a freshly created window
/// (enzICED #2250).
///
/// This is the single seam `window::Manager::insert` calls. `None` -- the
/// default -- means no stage recording, and the window then pays nothing.
///
/// `ICED_STAGE_LEDGER` installs a recorder on first use without any code
/// change, so a shipped binary can be asked for joined stage evidence in the
/// field. Its value, when it parses, is the per-window row capacity.
#[cfg(feature = "performance-observer")]
pub fn attach_stage_ledger(
    window_id: u64,
) -> Option<Arc<crate::graphics::presentation::StageLedger>> {
    let recorder = match StageRecorder::installed() {
        Some(recorder) => recorder,
        None => {
            let capacity = std::env::var("ICED_STAGE_LEDGER").ok()?;

            StageRecorder::install(capacity.parse().unwrap_or(STAGE_CAPACITY))
        }
    };

    Some(recorder.attach(window_id))
}

/// Releases a closed window's stage ledger (enzICED #2250).
#[cfg(feature = "performance-observer")]
pub fn detach_stage_ledger(window_id: u64) {
    if let Some(recorder) = StageRecorder::installed() {
        recorder.detach(window_id);
    }
}

/// Without `performance-observer` the runtime compiles no stage spans, so there
/// is nothing to forward and no ledger is ever attached (enzICED #2250).
#[cfg(not(feature = "performance-observer"))]
pub fn attach_stage_ledger(
    _window_id: u64,
) -> Option<Arc<crate::graphics::presentation::StageLedger>> {
    None
}

/// No-op without `performance-observer` (enzICED #2250).
#[cfg(not(feature = "performance-observer"))]
pub fn detach_stage_ledger(_window_id: u64) {}

#[cfg(test)]
mod ui_frame_tests {
    use super::*;
    use crate::core::renderer::presentation::Error;
    use std::time::{Duration, Instant};

    fn key() -> FrameKey {
        FrameKey {
            window_id: 7,
            surface_generation: 2,
            ownership_generation: 1,
            scene_generation: 0,
            submitted_sequence: 41,
        }
    }

    /// The record carries the frame's wake timing and the refusal that kept it
    /// on the UI thread, joinable to its `presentation` record (#3396).
    #[test]
    fn a_ui_frame_record_reports_wake_timing_and_the_refusal() {
        let requested = Instant::now();
        let wake = requested + Duration::from_micros(3_250);
        let submitted = wake + Duration::from_micros(5_500);
        let record = ui_frame_record(
            &key(),
            Some(requested),
            Some(wake),
            submitted,
            Some(Error::OpaquePaint),
        );

        assert_eq!(
            record,
            "{\"record\":\"ui_frame\",\"window\":7,\"surface_generation\":2,\
             \"scene_generation\":0,\"submitted_sequence\":41,\
             \"requested_to_wake_us\":3250,\"wake_to_submit_us\":5500,\
             \"refusal\":\"OpaquePaint\"}"
        );
    }

    /// Unknown timing and no refusal are explicit nulls, not zeros.
    #[test]
    fn missing_timing_and_no_refusal_are_null() {
        let record = ui_frame_record(&key(), None, None, Instant::now(), None);

        assert!(record.contains("\"requested_to_wake_us\":null"), "{record}");
        assert!(record.contains("\"wake_to_submit_us\":null"), "{record}");
        assert!(record.ends_with("\"refusal\":null}"), "{record}");
    }
}
