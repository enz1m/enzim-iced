// Modified by enzUI contributors. See FORK.md.
//! A renderer-owned worker for animation and declared layout over retained scenes.
//!
//! The worker owns its surface while running; reclaim is an explicit ownership
//! handshake. It samples tracks and replays owned data without invoking widget
//! methods. Backend resource locks remain a separate isolation constraint.
//! Publication is bounded and coalesced within ownership epochs. Active receives
//! do not wait for publisher locks; each frame drains at most one mailbox capacity.
pub mod admission;
mod mailbox;
mod sample_stamp;
#[cfg(test)]
mod tests;

pub use mailbox::Statistics as PublicationStatistics;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use crate::Viewport;
use crate::compositor::{PresentationQuality, SurfaceError};
use crate::core::renderer::{Recording, Track, TrackAnimation};
use crate::core::{Color, time};

/// How long a [`Lane::reclaim`] waits for the worker to hand the surface back
/// before giving up and reporting the lane as poisoned.
const RECLAIM_TIMEOUT: time::Duration = time::Duration::from_secs(2);

/// The most animation-only frames the lane keeps in its trace ring.
///
/// The trace is diagnostic, so it is deliberately **bounded**: an animation
/// that runs for an hour must not grow the process. Overwriting the oldest
/// record is the documented eviction.
const TRACE_CAPACITY: usize = 4096;

/// The backend half of a lane: the operations the worker performs on the
/// surface and renderer it owns.
///
/// It exists so the worker loop can live here, in backend-agnostic code, while
/// the `Send` obligations stay with the concrete backend that spawns it. A
/// backend with no thread-safe surface (software/`softbuffer`) simply never
/// implements it, and [`crate::Compositor::spawn_animation_lane`] keeps
/// returning the surface unchanged.
///
/// # Why the renderer is not an associated type
///
/// The backend **owns** its renderer, rather than the lane owning it beside the
/// backend. That is what keeps this trait object-safe over a single associated
/// type, and object safety is not a style preference here: without it the
/// *shipped* compositor cannot have a lane at all.
///
/// The default `iced` compositor is
/// `iced_renderer::fallback::Compositor<wgpu, tiny_skia>`, whose surface is an
/// enum over both backends. It can only wire a lane by *adapting* the primary
/// backend's surface type into that enum, which means holding the primary
/// backend as `Box<dyn Backend<Surface = _>>`. With a second associated type
/// for the renderer that box is unnameable, so `spawn_animation_lane` silently
/// stayed `None` for every real application while the wgpu implementation right
/// beside it looked complete (#2219 review finding B1).
pub trait Backend: Send + 'static {
    /// The surface this backend presents to.
    ///
    /// This matches [`crate::Compositor::Surface`], which carries the same
    /// bound so that the runtime -- generic over the compositor -- can actually
    /// call `spawn_animation_lane` (#2219 finding B1).
    type Surface: Send + 'static;

    /// Reconfigures the surface after a resize or scale change.
    fn configure(&mut self, surface: &mut Self::Surface, width: u32, height: u32);

    /// Replays `scene` with `tracks` applied, entering no widget.
    ///
    /// Returns `false` when the recording cannot be replayed by this backend's
    /// renderer, which stops the lane rather than presenting a wrong frame.
    fn replay(&mut self, viewport: &Viewport, scene: &Recording, tracks: &[Track]) -> bool;

    /// Releases scene-derived backend resources after their owner retires.
    fn retire_resources(&mut self) {}

    /// The specific owned-program refusal behind a failed replay, when available.
    fn replay_refusal(&self) -> Option<crate::core::renderer::presentation::Error> {
        None
    }

    /// Captures bounded layout and native-input data before surface submission.
    /// Budget refusal may preserve paint without an input sample; other refusals stop the lane.
    fn layout_sample(
        &mut self,
    ) -> Result<
        Option<Arc<crate::core::renderer::presentation::Sample>>,
        crate::core::renderer::presentation::Error,
    > {
        Ok(None)
    }

    /// Presents what the backend's renderer holds to the surface.
    ///
    /// `on_pre_present` runs **after** the frame is acquired and submitted and
    /// **immediately before** the surface commit. That ordering is the whole
    /// point of the parameter: a `wp_presentation` feedback request must reach
    /// the connection's request stream before the commit it describes, and it
    /// must not be issued at all when acquisition failed (enzICED #2219).
    /// It is therefore never called on the error paths.
    fn present(
        &mut self,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background: Color,
        on_pre_present: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError>;

    /// The strongest milestone [`present`](Self::present) establishes.
    ///
    /// This is the *static* capability of the backend, and it stays
    /// [`PresentationQuality::SurfacePresented`] even when compositor feedback
    /// is wired up: whether a given frame was actually shown is an
    /// asynchronous, per-frame answer that arrives later, and promoting the
    /// static method would mislabel every missing and discarded one.
    fn presentation_quality(&self) -> PresentationQuality;

    /// Turns the backend's own per-present timing on or off (enzICED #2250).
    ///
    /// The worker calls this exactly when a stage ledger is installed, so a
    /// default build takes no timestamps at all inside `present`. A backend
    /// that ignores it simply reports no spans, which the ledger records as
    /// missing rather than inventing.
    fn measure_stages(&mut self, _enabled: bool) {}

    /// The backend-internal spans of the last [`present`](Self::present).
    ///
    /// Only the backend can separate *waiting for a swapchain image* from
    /// *preparing and encoding commands* from *handing them to the queue* from
    /// *requesting presentation*; from the worker all of it is one opaque
    /// call, and a lane row that reports their sum cannot distinguish a
    /// compositor-paced frame from a slow one (enzICED #2219/#2250).
    ///
    /// The default is `None`, which is deliberately fail-closed: a backend that
    /// has not been taught to time itself yields frames the stage ledger marks
    /// `Missing(Acquire)`/`Missing(Prepare)`/... and the verdict refuses,
    /// rather than frames that silently look complete.
    fn take_present_stages(&mut self) -> Option<PresentStages> {
        None
    }
}

/// The backend-internal spans of one lane present (enzICED #2219/#2250).
///
/// #2250 split what used to be a single `submit` span: real wgpu preparation
/// and command encoding happened inside a span *labelled* `Submit`, so nothing
/// could tell an expensive frame from a stalled queue. The four spans below are
/// each measured at their own boundary and are sequential by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentStages {
    /// Acquiring the next swapchain image.
    pub acquire: (time::Instant, time::Instant),
    /// Preparing primitives and encoding the frame's commands.
    pub prepare: (time::Instant, time::Instant),
    /// Handing the encoded commands to the GPU queue.
    pub submit: (time::Instant, time::Instant),
    /// Requesting presentation: feedback registration plus the surface commit.
    pub present: (time::Instant, time::Instant),
}

/// A boxed [`Backend`] is a [`Backend`], so a compositor that must *adapt*
/// another compositor's backend can hold it behind a pointer (enzICED #2219).
impl<S: Send + 'static> Backend for Box<dyn Backend<Surface = S>> {
    type Surface = S;

    fn configure(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        (**self).configure(surface, width, height);
    }

    fn replay(&mut self, viewport: &Viewport, scene: &Recording, tracks: &[Track]) -> bool {
        (**self).replay(viewport, scene, tracks)
    }

    fn retire_resources(&mut self) {
        (**self).retire_resources();
    }

    fn replay_refusal(&self) -> Option<crate::core::renderer::presentation::Error> {
        (**self).replay_refusal()
    }

    fn layout_sample(
        &mut self,
    ) -> Result<
        Option<Arc<crate::core::renderer::presentation::Sample>>,
        crate::core::renderer::presentation::Error,
    > {
        (**self).layout_sample()
    }

    fn present(
        &mut self,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background: Color,
        on_pre_present: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError> {
        (**self).present(surface, viewport, background, on_pre_present)
    }

    fn presentation_quality(&self) -> PresentationQuality {
        (**self).presentation_quality()
    }

    fn measure_stages(&mut self, enabled: bool) {
        (**self).measure_stages(enabled);
    }

    fn take_present_stages(&mut self) -> Option<PresentStages> {
        (**self).take_present_stages()
    }
}

/// What the runtime must do with a window's surface for one redraw
/// (enzICED #2219).
///
/// The decision lives here, as a pure function of observable conditions,
/// because the bug it prevents is invisible in the event loop: a runtime that
/// draws and presents unconditionally reclaims the surface on every frame, so
/// the lane exists, compiles, is spawned -- and never owns anything. That is
/// not a slow animation; it is no isolation at all, and nothing in the type
/// system objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The lane owns the surface and this frame changes only track values: the
    /// UI thread must neither draw nor present. Presenting here would reclaim
    /// the surface and destroy the isolation it just established.
    LaneAnimates,
    /// The lane owns the surface and the UI thread recorded a replacement
    /// scene: commit it and let the lane present it, without an ownership
    /// round trip.
    LaneCommits,
    /// Transfers the surface and admitted recording for a live travel. Owned-layout
    /// recordings may start before UI submission; transform-only recordings may not.
    HandOver,
    /// The UI thread presents and keeps the surface.
    UiPresents,
}

/// Everything [`plan`] is allowed to look at.
#[derive(Debug, Clone, Copy)]
pub struct Conditions {
    /// A lane exists for this window and has not been disabled.
    pub lane_available: bool,
    /// The lane is presenting right now.
    pub lane_running: bool,
    /// This frame changes nothing but published track values.
    pub animation_only: bool,
    /// A committed scene matching the current viewport exists.
    pub scene_replayable: bool,
    /// At least one published animation is usable and still travelling.
    pub live_animation: bool,
}

/// Decides who presents one redraw.
pub fn plan(conditions: Conditions) -> Plan {
    let Conditions {
        lane_available,
        lane_running,
        animation_only,
        scene_replayable,
        live_animation,
    } = conditions;

    // The lane cannot be running without a committed scene of its OWN, so the
    // UI thread's copy is irrelevant here (enzICED #2219).
    //
    // Requiring `scene_replayable` was a real isolation defect: whenever the UI
    // thread's `window.scene` lapsed for one frame -- a draw that declined to
    // record, an invalidation -- this fell through to the same-thread branch,
    // which RECLAIMS the surface and stops a healthy lane mid-travel. It was
    // measured: the lane's own trace reported `Stop::Reclaimed` after 4 frames
    // with 145 ms of travel still in hand, and the animation then waited for
    // the blocked UI thread, producing exactly the stall the issue forbids.
    if lane_running && animation_only {
        return Plan::LaneAnimates;
    }

    if lane_running && live_animation {
        // The travel is live and the lane owns the surface. The UI thread may
        // publish a replacement scene, but it must NOT present: `present_frame`
        // goes through `Window::surface_mut`, which reclaims the surface, so a
        // single UI-thread present mid-travel stops the lane and the animation
        // then waits for whatever the UI thread does next (#2219).
        //
        // This frame is NOT animation-only -- the first branch took those --
        // so the UI thread has content the lane's committed scene does not
        // show. If it could be recorded, the lane takes it as an atomic
        // replacement and keeps the cadence.
        //
        // If it could NOT be recorded (an editor subtree, a raw buffer whose
        // producer went away, a renderer with no retained path), the ONLY copy
        // of the current content is in the UI thread's renderer. Skipping the
        // present here would leave the lane's older scene on screen -- stale
        // content presented confidently, which is the failure mode a retained
        // scene must never have. The UI thread therefore presents and takes the
        // surface back; losing the lane is the cost of staying truthful
        // (#2219 review finding 4).
        return if scene_replayable {
            Plan::LaneCommits
        } else {
            Plan::UiPresents
        };
    }

    // The hand over happens only *after* a frame the UI thread itself
    // presented, so the lane never starts from a scene that was never on
    // screen.
    if lane_available && !lane_running && scene_replayable && live_animation {
        return Plan::HandOver;
    }

    Plan::UiPresents
}

/// Why the lane stopped owning the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The UI thread asked for the surface back.
    Reclaimed,
    /// Every animation finished; the lane has presented the settled value.
    Settled,
    /// The scene could not be replayed by the lane's renderer.
    ReplayRefused,
    /// An owned program required unsupported geometry or invalid driver data.
    Presentation(crate::core::renderer::presentation::Error),
    /// The surface is not visible; ownership returns without repaint polling.
    Occluded,
    /// The surface reported an error the lane must not paper over.
    Surface(SurfaceError),
    /// The lane is shutting down.
    Shutdown,
}

/// What the worker did while it owned the surface.
#[derive(Debug, Clone)]
pub struct Report {
    /// Why it stopped.
    pub stop: Stop,
    /// Frames the lane presented during this ownership window.
    pub presented: u64,
    /// The last error the surface reported, if any.
    pub error: Option<SurfaceError>,
    /// The tracks the lane presented last, so the UI thread can resume from the
    /// value the user actually saw rather than from its own stale sample.
    pub last_tracks: Vec<Track>,
}

/// One presented animation-only frame, as the lane recorded it.
#[derive(Debug, Clone)]
pub struct Frame {
    /// When the lane finished presenting.
    pub at: time::Instant,
    /// How long acquire + replay + submit + present took.
    pub present: time::Duration,
    /// The tracks the lane sampled for this frame.
    pub tracks: Vec<Track>,
    /// The milestone this frame actually reached.
    pub quality: PresentationQuality,
    /// The scene generation this frame replayed, so a replacement scene is
    /// visible in the trace instead of being invisible mid-travel.
    pub scene: u64,
}

/// Process-wide lane telemetry, for a harness that must prove the lane is
/// *actually* present in a shipped binary (enzICED #2219).
///
/// A gate that can only observe the UI thread cannot tell a real render lane
/// from a same-thread replay cache: both leave the same trace in an
/// application-side log. Review finding B1 was exactly that -- a live run whose
/// log contained no lane marker at all, so "the lane presented this" was an
/// inference rather than a measurement.
///
/// These are five atomics for the whole process, updated on paths that already
/// synchronise, so they are cheap and, being counters, **bounded**.
pub mod telemetry {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Whether lane instrumentation is on. **Off by default** (enzICED #2219
    /// review finding 7).
    ///
    /// Instrumentation a shipped application cannot turn off is not
    /// instrumentation, it is overhead: with this `false` the worker performs
    /// no counter writes, no per-frame `Vec<Track>` clone, no mutex acquisition
    /// and no ring push -- one relaxed load per presented frame is the entire
    /// default cost.
    static ENABLED: AtomicBool = AtomicBool::new(false);

    /// Turns lane instrumentation on for this process.
    ///
    /// A harness calls this (or sets `ICED_LANE_TRACE`) before the first frame.
    /// There is deliberately no way to turn it back off: a counter that can be
    /// stopped mid-run is a counter a gate cannot trust.
    pub fn enable() {
        ENABLED.store(true, Ordering::Relaxed);
    }

    /// Whether lane instrumentation is on.
    pub fn is_enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// Turns instrumentation on when `ICED_LANE_TRACE` names a trace file, so
    /// the opt-in is a single decision rather than two that can disagree.
    pub(super) fn enable_from_env() {
        if std::env::var_os("ICED_LANE_TRACE").is_some() {
            enable();
        }
    }

    /// Adds `delta` to `counter`, but only while instrumentation is on.
    pub(super) fn bump(counter: &AtomicU64, delta: u64) {
        if is_enabled() {
            let _ = counter.fetch_add(delta, Ordering::Relaxed);
        }
    }

    /// Lanes created by a compositor.
    pub(super) static SPAWNED: AtomicU64 = AtomicU64::new(0);
    /// Times a lane took ownership of a surface.
    pub(super) static STARTED: AtomicU64 = AtomicU64::new(0);
    /// Frames presented **by lane workers**, never by the UI thread.
    pub(super) static PRESENTED: AtomicU64 = AtomicU64::new(0);
    /// Atomic scene replacements applied while a lane kept presenting.
    pub(super) static COMMITS: AtomicU64 = AtomicU64::new(0);
    /// Times a lane stopped and returned the surface.
    pub(super) static STOPPED: AtomicU64 = AtomicU64::new(0);

    /// One immutable reading of the counters above.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Snapshot {
        /// Lanes created by a compositor.
        pub spawned: u64,
        /// Times a lane took ownership of a surface.
        pub started: u64,
        /// Frames presented by lane workers.
        pub presented: u64,
        /// Atomic scene replacements applied mid-travel.
        pub commits: u64,
        /// Times a lane stopped and returned the surface.
        pub stopped: u64,
    }

    /// Reads the process-wide lane counters.
    ///
    /// `spawned == 0` is the honest statement "this build has no lane": it is
    /// what a software-only backend reports, and what the whole process
    /// reported before the shipped `fallback::Compositor` forwarded one.
    ///
    /// Every counter reads zero while [`is_enabled`] is `false`, which is the
    /// documented default: a reader that has not opted in must not mistake an
    /// un-instrumented process for an idle one, so a gate asserts
    /// [`is_enabled`] first.
    pub fn snapshot() -> Snapshot {
        Snapshot {
            spawned: SPAWNED.load(Ordering::Relaxed),
            started: STARTED.load(Ordering::Relaxed),
            presented: PRESENTED.load(Ordering::Relaxed),
            commits: COMMITS.load(Ordering::Relaxed),
            stopped: STOPPED.load(Ordering::Relaxed),
        }
    }
}

/// Counters the UI thread can read without a handshake.
#[derive(Debug, Default)]
struct Counters {
    presented: AtomicU64,
    replays: AtomicU64,
    replay_refusals: AtomicU64,
    /// Animation frames not presented because nothing visible changed
    /// (enzUI-ICED #3084).
    invisible_skips: AtomicU64,
    /// Frames sampled at a predicted presentation instant rather than at
    /// wake (enzUI-ICED #3082).
    paced_frames: AtomicU64,
    layout_snapshot_budget_skips: AtomicU64,
    surface_errors: AtomicU64,
    scene_commits: AtomicU64,
    /// Track-generation rows built for presentation observers.
    generation_rows: AtomicU64,
    /// Non-empty generation buffers allocated for presentation observers.
    generation_allocations: AtomicU64,
    // An immutable origin lets one atomic publish the complete sample timestamp without a UI lock.
    clock_origin: std::sync::OnceLock<time::Instant>,
    last_presented: sample_stamp::SampleStamp,
    running: AtomicBool,
    /// Set by the worker when it stopped owning the surface *on its own*
    /// (settled, replay refused, surface error) rather than because the UI
    /// thread reclaimed it.
    ///
    /// It exists so a lane can never park silently: the UI thread learns the
    /// lane is done without a handshake, and the paired wake hook brings the
    /// event loop back even when nothing else would.
    stopped: AtomicBool,
}

/// Vblanks past the first possible one a lane frame is expected to land on:
/// the lane submits one frame ahead under FIFO (enzUI-ICED #3082).
pub const LANE_PRESENT_LEAD: u32 = 1;

/// The instant a lane frame samples its motion at (enzUI-ICED #3660).
///
/// A recording is the layout at one instant. Moving it along a curve to
/// another instant moves it rigidly, so whatever that layout would have
/// reflowed (a trailing edge, a wrapped row, a close button) is off by the
/// distance travelled meanwhile: the whole layout visibly shifts. So while the
/// UI thread repaints every frame of a travel, a fresh commit is shown at
/// exactly the instant it was laid out for, and a frame with no fresh commit
/// holds the last one, however late the next is: a slow or stalled UI thread
/// costs repeated frames, never a frame whose layout disagrees with itself.
/// Whoever drives such a travel keeps it from jumping when the UI thread
/// catches up (the app shell delays its clock by a stall). Motion the UI
/// thread does not repaint is sampled for the vblank, as the curves alone
/// describe it.
#[derive(Debug, Default)]
struct CommitClock {
    /// The last sample instant, which never goes back.
    last: Option<time::Instant>,
    /// Whether the UI thread repaints every frame of what is moving.
    repainting: bool,
}

impl CommitClock {
    /// For a lane taking over a scene the UI thread presented, laid out for
    /// `presented`.
    fn handed_over(presented: Option<time::Instant>, repainting: bool) -> Self {
        Self {
            last: presented,
            repainting,
        }
    }

    /// The sample instant for a frame shown at `vblank`; `fresh` is the
    /// instant the commit it shows was laid out for, if the lane has not
    /// shown it yet.
    fn sample_for(&mut self, vblank: time::Instant, fresh: Option<time::Instant>) -> time::Instant {
        let at = match fresh {
            Some(laid_out) => laid_out,
            None if self.repainting => self.last.unwrap_or(vblank),
            None => vblank,
        };
        let at = self.last.map_or(at, |last| at.max(last));
        self.last = Some(at);
        at
    }
}

/// The pacing of a skipped frame before any presented interval is known.
const DEFAULT_FRAME_INTERVAL: time::Duration = time::Duration::from_micros(16_667);
/// Bounds on a measured presented interval used to pace skipped frames.
const MIN_FRAME_INTERVAL: time::Duration = time::Duration::from_millis(4);
const MAX_FRAME_INTERVAL: time::Duration = time::Duration::from_millis(50);

/// Changes below these are invisible (enzUI-ICED #3084): 0.3 physical pixels
/// of translation, clip or scale (at the viewport's larger extent), and one
/// 8-bit colour level of opacity.
const INVISIBLE_PIXELS: f32 = 0.3;
const INVISIBLE_OPACITY: f32 = 1.0 / 255.0;

/// Whether `next` would paint the same picture as `presented`.
fn tracks_visually_equal(next: &[Track], presented: &[Track], viewport: &Viewport) -> bool {
    if next.len() != presented.len() {
        return false;
    }
    let scale = viewport.scale_factor();
    let size = viewport.logical_size();
    let extent = size.width.max(size.height);
    let px = |logical: f32| (logical * scale).abs() < INVISIBLE_PIXELS;
    next.iter().all(|track| {
        let Some(old) = presented.iter().find(|old| old.node == track.node) else {
            return false;
        };
        let clip_same = match (track.clip, old.clip) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                px(a.x - b.x) && px(a.y - b.y) && px(a.width - b.width) && px(a.height - b.height)
            }
            _ => false,
        };
        px(track.translation.x - old.translation.x)
            && px(track.translation.y - old.translation.y)
            && (track.opacity - old.opacity).abs() < INVISIBLE_OPACITY
            && px((track.scale - old.scale) * extent)
            && px((track.rotation - old.rotation) * extent)
            && px(track.scale_origin.x - old.scale_origin.x)
            && px(track.scale_origin.y - old.scale_origin.y)
            && clip_same
    })
}

/// The UI-thread wake hook, shared with the worker.
type Wake = std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>;

/// The compositor-feedback hook, shared with the worker (enzICED #2219).
type Observer = std::sync::OnceLock<Arc<dyn PresentationObserver>>;

/// Slot for the per-frame stage ledger the runtime may install (enzICED #2219).
type Stages = std::sync::OnceLock<Arc<crate::presentation::StageLedger>>;

/// One frame about to be committed, offered to a [`PresentationObserver`] at
/// the pre-commit seam (enzICED #2219).
///
/// The lane supplies only what the lane knows. Window, surface generation and
/// ownership generation belong to the runtime, which is why the observer -- not
/// the lane -- assembles the full `FrameKey`.
#[derive(Debug)]
pub struct PendingSubmission<'a> {
    /// Owned sampled allocations; absence never grants presented-input ownership.
    pub layout: Option<Arc<crate::core::renderer::presentation::Sample>>,
    /// Which side is committing.
    pub owner: crate::presentation::FrameOwner,
    /// The generation of the scene being replayed.
    pub scene_generation: u64,
    /// The instant the tracks below were sampled from the presentation clock.
    ///
    /// This -- not callback-arrival time -- is what a retarget resumes from,
    /// so it travels with the identity rather than being recomputed later.
    pub sampled_at: time::Instant,
    /// The exact `(node, animation generation)` pairs this frame sampled.
    ///
    /// A sampled track with no live animation behind it carries generation
    /// `0`, explicitly, so "static" and "missing" can never be confused.
    pub tracks: &'a [crate::presentation::TrackGeneration],
}

/// Requests compositor presentation feedback for a frame about to be committed
/// (enzICED #2219).
///
/// Implemented by the runtime, which owns the Wayland connection and the
/// window/surface/ownership generations. It is called on whichever thread is
/// committing -- the UI thread or the lane worker -- from inside the backend's
/// pre-commit slot, so an implementation must be cheap and must not block on
/// the UI thread.
pub trait PresentationObserver: Send + Sync + 'static {
    /// Called after acquire+submit and immediately before the surface commit.
    ///
    /// It is **not** called when frame acquisition failed, so a failed frame
    /// never leaves a pending identity behind that a later callback could
    /// wrongly join to. Returns the registered full key when feedback was
    /// actually requested; instrumentation uses it to join sampled values to
    /// callback-confirmed presentation instead of treating submission as
    /// visibility.
    fn pre_commit(
        &self,
        submission: PendingSubmission<'_>,
    ) -> Option<crate::presentation::FrameKey>;

    /// Whether this observer actually reads `PendingSubmission::tracks`.
    ///
    /// Collecting the `(node, animation generation)` pairs is `O(tracks *
    /// animations)` plus one allocation on every ACTIVE frame, and the runtime
    /// installs an observer unconditionally -- including on X11 and on a
    /// Wayland compositor with no `wp_presentation` global, where the observer
    /// only counts frames and returns before reading a track. Such an observer
    /// returns `false` and is handed an empty slice, so the join identity work
    /// is paid only where a feedback callback can actually consume it
    /// (enzICED #2219).
    ///
    /// The default is `true`: an observer that forgets to answer keeps the
    /// full identity rather than silently losing it.
    fn needs_track_generations(&self) -> bool {
        true
    }

    /// The vblank grid this surface presents on, from the compositor's last
    /// presentation and its stated (or, when unstated, measured) refresh
    /// (enzUI-ICED #3082).
    ///
    /// The lane samples motion at the grid instant a frame is predicted to be
    /// shown, not at the instant it woke. `None` (the default, and the answer
    /// without `wp_presentation` feedback) keeps sampling at wake.
    fn presentation_grid(&self) -> Option<crate::presentation::PresentationGrid> {
        None
    }
}

#[derive(Clone)]
struct RevocableRecording(Arc<std::sync::Mutex<Option<Recording>>>);

impl RevocableRecording {
    fn new(recording: Recording) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(recording))))
    }

    fn snapshot(&self) -> Option<Recording> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn retire(&self) {
        let _ = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

enum Request<S> {
    /// Hand the surface over and start presenting.
    Start {
        surface: S,
        viewport: Viewport,
        background: Color,
    },
    /// Replace the committed scene atomically, without restarting live tracks.
    Commit {
        scene: RevocableRecording,
        /// Producer-assigned generation of these exact scene bytes.
        ///
        /// This must travel with the commit instead of being incremented by
        /// the worker on receipt: queued/replaced commits need to retain the
        /// identity the UI thread assigned when it recorded the scene.
        generation: u64,
        viewport: Viewport,
        background: Color,
        tracks: Vec<Track>,
        animations: Vec<TrackAnimation>,
        /// The instant the UI thread laid this scene out and drew it for.
        sampled_at: Option<time::Instant>,
        /// Whether the UI thread has presented this scene itself.
        presented: bool,
        /// Whether the UI thread repaints every frame of what is moving, so
        /// fresh commits follow this one.
        repainting: bool,
    },
    /// Update the tracks/animations of the committed scene.
    Animate {
        tracks: Vec<Track>,
        animations: Vec<TrackAnimation>,
    },
    /// Drops every retained revision before later work in this mailbox epoch.
    RetireScene,
    /// The surface geometry moved; reconfigure before the next present.
    Configure {
        width: u32,
        height: u32,
        viewport: Viewport,
    },
    /// Hand the surface back.
    Reclaim,
    /// Stop the worker.
    Shutdown,
}

impl<S> mailbox::Message for Request<S> {
    fn barrier(&self) -> bool {
        matches!(self, Self::Start { .. } | Self::Reclaim | Self::Shutdown)
    }

    fn supersedes(&self, previous: &Self) -> bool {
        matches!(
            (self, previous),
            (
                Self::Commit { .. },
                Self::Commit { .. } | Self::Animate { .. }
            ) | (Self::Animate { .. }, Self::Animate { .. })
                | (
                    Self::RetireScene,
                    Self::Commit { .. } | Self::Animate { .. } | Self::RetireScene
                )
                | (Self::Configure { .. }, Self::Configure { .. })
        )
    }

    fn revision(&self) -> Option<u64> {
        match self {
            Self::Commit { generation, .. } => Some(*generation),
            _ => None,
        }
    }
}

fn publish_stop(counters: &Counters, self_stopped: bool, reply: impl FnOnce()) {
    // The receiver may restart the lane as soon as the surface reply arrives.
    if self_stopped {
        counters.stopped.store(true, Ordering::Release);
    }
    reply();
}

enum Reply<S> {
    Surface { surface: S, report: Report },
    Stopped,
}

/// Whether the worker and its backend actually finished, not merely whether a wait expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    /// The worker was joined after completion.
    Joined,
    /// Completion is unconfirmed; the backend may still own native/GPU resources.
    Quarantined,
}

/// Surface handback and worker retirement are separate ownership facts.
#[derive(Debug)]
pub struct Shutdown<S> {
    /// Reclaimed surface, if any; quarantine does not authorize replacement of its backend.
    pub surface: Option<S>,
    /// Only `Joined` confirms that the worker's backend finished destruction.
    pub retirement: Retirement,
}

/// A handle to a running animation lane.
pub struct Lane<S> {
    to_worker: mailbox::Sender<Request<S>>,
    from_worker: mpsc::Receiver<Reply<S>>,
    worker: Option<std::thread::JoinHandle<()>>,
    counters: Arc<Counters>,
    trace: Arc<std::sync::Mutex<std::collections::VecDeque<Frame>>>,
    wake: Arc<Wake>,
    observer: Arc<Observer>,
    stages: Arc<Stages>,
    active_scene: Option<RevocableRecording>,
    quality: PresentationQuality,
    running: bool,
    poisoned: bool,
}

impl<S> std::fmt::Debug for Lane<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lane")
            .field("running", &self.running)
            .field("poisoned", &self.poisoned)
            .field("presented", &self.presented())
            .field("quality", &self.quality)
            .finish()
    }
}

/// Spawns a lane worker driven by `backend`, which owns its own renderer.
///
/// A new lane is idle; [`Lane::start`] separately transfers the surface.
/// # Errors
/// Returns the unchanged backend when process-wide admission is exhausted.
/// Use [`try_spawn`] to reserve capacity before constructing a backend.
pub fn spawn<B: Backend>(backend: B) -> Result<Lane<B::Surface>, B> {
    let Some(permit) = admission::global().reserve() else {
        return Err(backend);
    };
    Ok(spawn_reserved(backend, permit))
}

/// Reserves aggregate capacity before invoking a backend constructor.
/// Refusal never invokes the factory; failed construction releases its reservation.
pub fn try_spawn<B: Backend>(
    pool: &Arc<admission::Admission>,
    factory: impl FnOnce() -> Option<B>,
) -> Option<Lane<B::Surface>> {
    let permit = pool.reserve()?;
    let backend = factory()?;
    Some(spawn_reserved(backend, permit))
}

fn spawn_reserved<B: Backend>(backend: B, permit: admission::Permit) -> Lane<B::Surface> {
    let (to_worker, requests) = mailbox::channel::<Request<B::Surface>>();
    let (replies, from_worker) = mpsc::sync_channel::<Reply<B::Surface>>(2);

    telemetry::enable_from_env();
    telemetry::bump(&telemetry::SPAWNED, 1);
    let counters = Arc::new(Counters::default());
    let _ = counters.clock_origin.set(time::Instant::now());
    let trace = Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::<Frame>::with_capacity(64),
    ));
    let wake: Arc<Wake> = Arc::new(std::sync::OnceLock::new());
    let observer: Arc<Observer> = Arc::new(std::sync::OnceLock::new());
    let stages: Arc<Stages> = Arc::new(std::sync::OnceLock::new());
    let quality = backend.presentation_quality();

    let worker_counters = counters.clone();
    let worker_trace = trace.clone();
    let worker_wake = wake.clone();
    let worker_observer = observer.clone();
    let worker_stages = stages.clone();

    let ownership = admission::OwnedBackend {
        backend,
        _permit: permit,
    };
    let worker = std::thread::Builder::new()
        .name("iced-animation-lane".into())
        .spawn(move || {
            let mut ownership=ownership;
            let backend=&mut ownership.backend;
            let counters = worker_counters;
            let trace = worker_trace;
            let wake = worker_wake;
            let observer = worker_observer;
            let stages = worker_stages;
            // Opt-in, append-only evidence written by the thread that actually
            // presented, so a harness never has to infer lane cadence from the
            // UI thread's timeline. Off unless `ICED_LANE_TRACE` names a file.
            let mut log = std::env::var_os("ICED_LANE_TRACE").and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            });

            // The lane's whole retained state, in one place: what it presents
            // (`scene`), how it is driven (`animations`/`tracks`), and where
            // (`viewport`/`background`/`pending_configure`).
            struct State {
                scene: Option<RevocableRecording>,
                generation: u64,
                animations: Vec<TrackAnimation>,
                tracks: Vec<Track>,
                viewport: Option<Viewport>,
                background: Color,
                pending_configure: Option<(u32, u32)>,
                // The instant the committed scene was laid out for, until the
                // lane has presented it once.
                fresh_sample: Option<time::Instant>,
                // The instant a scene the UI thread presented before handing
                // the surface over was laid out for.
                handed_over: Option<time::Instant>,
                // Whether the UI thread repaints every frame of what moves.
                repainting: bool,
            }

            let lane_started = time::Instant::now();

            let mut state = State {
                scene: None,
                generation: 0,
                animations: Vec::new(),
                tracks: Vec::new(),
                viewport: None,
                background: Color::TRANSPARENT,
                pending_configure: None,
                fresh_sample: None,
                handed_over: None,
                repainting: false,
            };

            'idle: loop {
                // Idle: the lane owns no surface, so it blocks. It costs a
                // parked thread and nothing else.
                let mut surface = match requests.recv() {
                    Ok(Request::Start {
                        surface,
                        viewport: next,
                        background: next_background,
                    }) => {
                        state.viewport = Some(next);
                        state.background = next_background;
                        surface
                    }
                    Ok(Request::Commit {
                        scene: next,
                        generation: next_generation,
                        viewport: next_viewport,
                        background: next_background,
                        tracks: next_tracks,
                        animations: next_animations,
                        sampled_at,
                        presented,
                        repainting,
                    }) => {
                        state.repainting = repainting;
                        if presented {
                            state.fresh_sample = None;
                            state.handed_over = sampled_at;
                        } else {
                            state.fresh_sample = sampled_at;
                        }
                        if let Some(previous) = state.scene.replace(next) {
                            previous.retire();
                        }
                        state.generation = next_generation;
                        let _ = counters.scene_commits.fetch_add(1, Ordering::Relaxed);
                        telemetry::bump(&telemetry::COMMITS, 1);
                        state.viewport = Some(next_viewport);
                        state.background = next_background;
                        state.tracks = next_tracks;
                        merge_animations(&mut state.animations, next_animations);
                        continue 'idle;
                    }
                    Ok(Request::Animate {
                        tracks: next_tracks,
                        animations: next_animations,
                    }) => {
                        state.repainting = false;
                        state.tracks = next_tracks;
                        merge_animations(&mut state.animations, next_animations);
                        continue 'idle;
                    }
                    Ok(Request::RetireScene) => {
                        if let Some(scene) = state.scene.take() {
                            scene.retire();
                        }
                        backend.retire_resources();
                        state.generation = 0;
                        state.tracks.clear();
                        state.animations.clear();
                        continue 'idle;
                    }
                    Ok(Request::Configure {
                        width,
                        height,
                        viewport: next,
                    }) => {
                        state.pending_configure = Some((width, height));
                        state.viewport = Some(next);
                        continue 'idle;
                    }
                    Ok(Request::Reclaim) => continue 'idle,
                    Ok(Request::Shutdown) | Err(_) => {
                        backend.retire_resources();
                        let _ = replies.send(Reply::Stopped);
                        return;
                    }
                };

                counters.running.store(true, Ordering::Release);
                telemetry::bump(&telemetry::STARTED, 1);

                // enzICED #2250: the backend takes per-present timestamps only
                // while a ledger is installed. A default build asks for none,
                // so observation costs nothing when it is disabled.
                backend.measure_stages(stages.get().is_some());

                let mut presented = 0_u64;
                // enzUI-ICED #3084: whether anything other than the sampled
                // tracks changed since the last presented frame (a new scene,
                // a configure), and how long presented frames take to arrive,
                // which paces a skipped frame.
                let mut dirty_since_present = true;
                let mut last_presented_at: Option<time::Instant> = None;
                let mut frame_interval = DEFAULT_FRAME_INTERVAL;
                let mut last_tracks = state.tracks.clone();
                // enzUI-ICED #3082: motion is sampled for the vblank the frame
                // is predicted to land on, not the instant this thread woke.
                let mut pacer = crate::presentation::FramePacer::new();
                // enzUI-ICED #3660: fresh commits are shown at the instant
                // they were laid out for (see `CommitClock`).
                let mut clock = CommitClock::handed_over(state.handed_over.take(), state.repainting);
                let mut sampled = Vec::with_capacity(state.tracks.len().max(state.animations.len()));
                let mut error = None;
                let mut retired = false;
                let stop = 'running: loop {
                    // enzUI-ICED #3660: on a known vblank grid the lane starts
                    // a frame half a period before the vblank its previous
                    // frame lands on. It keeps one frame queued instead of
                    // filling the swapchain and blocking in acquire, so a
                    // reclaim is answered at once and the UI thread taking the
                    // surface back finds a free image. A request ends the
                    // wait; after the drain, it resumes until the frame is due.
                    // A first frame after a hand over the UI thread keeps
                    // repainting waits the same way for the vblank the
                    // presented frame is followed by, so the UI thread's next
                    // commit can reach it instead of the lane holding the
                    // presented one (enzUI-ICED #3660).
                    let due = observer
                        .get()
                        .and_then(|listener| listener.presentation_grid())
                        .and_then(|grid| {
                            let target = pacer.grid_target().or_else(|| {
                                (presented == 0 && clock.repainting)
                                    .then(|| grid.next_after(time::Instant::now()))
                            })?;
                            target.checked_sub(grid.period / 2)
                        });
                    if let Some(due) = due {
                        let _ = requests.wait_until(due);
                    }

                    // A continuously publishing UI must not postpone the next presentation.
                    for _ in 0..mailbox::CAPACITY {
                        match requests.try_recv() {
                            Ok(Request::Start { .. }) => {
                                // The UI thread cannot hand over a surface it
                                // does not have; ignore rather than leak.
                            }
                            Ok(Request::Commit {
                                scene: next,
                                generation: next_generation,
                                viewport: next_viewport,
                                background: next_background,
                                tracks: next_tracks,
                                animations: next_animations,
                                sampled_at,
                                presented,
                                repainting,
                            }) => {
                                state.fresh_sample = (!presented).then_some(sampled_at).flatten();
                                clock.repainting = repainting;
                                // An atomic replacement: the new scene takes
                                // effect on the NEXT presented frame, and live
                                // animations are merged by generation, so a
                                // content revision during travel does not
                                // restart or jump the track. Keep the producer's
                                // generation: receipt order is not scene identity.
                                if let Some(previous) = state.scene.replace(next) {
                                    previous.retire();
                                }
                                state.generation = next_generation;
                                let _ = counters.scene_commits.fetch_add(1, Ordering::Relaxed);
                                telemetry::bump(&telemetry::COMMITS, 1);
                                state.viewport = Some(next_viewport);
                                state.background = next_background;
                                state.tracks = next_tracks;
                                merge_animations(&mut state.animations, next_animations);
                                dirty_since_present = true;
                            }
                            Ok(Request::Animate {
                                tracks: next_tracks,
                                animations: next_animations,
                            }) => {
                                clock.repainting = false;
                                state.tracks = next_tracks;
                                merge_animations(&mut state.animations, next_animations);
                            }
                            Ok(Request::RetireScene) => {
                                if let Some(scene) = state.scene.take() {
                                    scene.retire();
                                }
                                backend.retire_resources();
                                retired = true;
                                state.generation = 0;
                                state.tracks.clear();
                                state.animations.clear();
                                break 'running Stop::Reclaimed;
                            }
                            Ok(Request::Configure {
                                width,
                                height,
                                viewport: next,
                            }) => {
                                state.pending_configure = Some((width, height));
                                state.viewport = Some(next);
                                dirty_since_present = true;
                            }
                            Ok(Request::Reclaim) => break 'running Stop::Reclaimed,
                            Ok(Request::Shutdown) => {
                                backend.retire_resources();
                                break 'running Stop::Shutdown;
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                backend.retire_resources();
                                break 'running Stop::Shutdown;
                            }
                        }
                    }

                    if due.is_some_and(|due| time::Instant::now() < due) {
                        continue 'running;
                    }

                    let Some(viewport) = state.viewport.clone() else {
                        break 'running Stop::Reclaimed;
                    };

                    if let Some((width, height)) = state.pending_configure.take() {
                        backend.configure(&mut surface, width, height);
                    }

                    let Some(scene) = state.scene.as_ref().and_then(RevocableRecording::snapshot)
                    else {
                        break 'running Stop::Settled;
                    };

                    if scene.docked_tenure().is_some_and(|tenure| !tenure.activate()) {
                        break 'running Stop::Presentation(
                            crate::core::renderer::presentation::Error::DockedOwnershipRequired,
                        );
                    }

                    // Sample the presentation clock. This is the line the
                    // "sampling moved back to the UI frame path" sabotage
                    // removes: without it the lane can only present values the
                    // UI thread computed, and a blocked UI thread freezes the
                    // animation.
                    let wake = time::Instant::now();
                    // enzUI-ICED #3082: with the compositor's vblank grid, the
                    // sample is the instant this frame is predicted to be
                    // shown, so a linear motion advances by one refresh's worth
                    // per presented frame however the wake jitters. Without a
                    // grid it is `wake`, as before.
                    let grid = observer
                        .get()
                        .and_then(|listener| listener.presentation_grid());
                    let vblank = pacer.target(wake, grid, LANE_PRESENT_LEAD);
                    let period = grid.map_or(frame_interval, |grid| grid.period);
                    let now = clock.sample_for(vblank, state.fresh_sample.take());
                    sampled.clear();
                    let mut still_running = false;
                    // enzICED #2219: `Animate` is exactly the clock sample plus
                    // track advance -- the only work an animation-only frame
                    // does that the UI thread used to do. It is timed on its
                    // own so it can never be folded into `Prepare`.
                    let animate_started = wake;

                    for animation in &state.animations {
                        if let Some(track) = animation.sample(now) {
                            sampled.push(track);
                            still_running |= !animation.is_finished(now);
                        }
                    }

                    // Tracks with no animation (a settled node, or an effect
                    // that publishes sampled values only) keep their last
                    // published value so the picture stays complete.
                    for track in &state.tracks {
                        if !sampled.iter().any(|existing| existing.node == track.node) {
                            sampled.push(*track);
                        }
                    }

                    let animate_ended = time::Instant::now();
                    let started = animate_ended;

                    // enzUI-ICED #3084: a frame whose tracks moved less than a
                    // visible amount since the last PRESENTED frame is not
                    // replayed or presented; the drift accumulates against that
                    // frame, so a slow track still presents once it adds up.
                    // Never the settled frame (`still_running` is false for it),
                    // never after a scene or size change.
                    if presented > 0
                        && still_running
                        && !dirty_since_present
                        && tracks_visually_equal(&sampled, &last_tracks, &viewport)
                    {
                        let _ = counters.invisible_skips.fetch_add(1, Ordering::Relaxed);
                        drop(scene);
                        // Pace like a presented frame would have: FIFO no
                        // longer blocks us, so without this the lane spins.
                        // A request ends the wait early. One vblank on a known
                        // grid: the interval between presents grows with every
                        // skipped frame, and a frame held for a late commit
                        // (enzUI-ICED #3660) must not make the next wait longer.
                        let _ = requests.wait_until(time::Instant::now() + period);
                        continue 'running;
                    }

                    if !backend.replay(&viewport, &scene, &sampled) {
                        let _ = counters.replay_refusals.fetch_add(1, Ordering::Relaxed);
                        break 'running backend.replay_refusal().map_or(Stop::ReplayRefused, Stop::Presentation);
                    }
                    let (trace_track, trace_owned_layout) = if telemetry::is_enabled() {
                        (
                            scene
                                .presentation()
                                .and_then(|program| {
                                    sampled.iter().position(|track| {
                                        program.drives_responsive(track.node.0)
                                    })
                                })
                                .or_else(|| (!sampled.is_empty()).then_some(0)),
                            scene.presentation().is_some_and(|program| {
                                program
                                    .layout_drivers()
                                    .any(|node| program.drives_responsive(node))
                            }),
                        )
                    } else {
                        (None, false)
                    };
                    drop(scene);

                    let _ = counters.replays.fetch_add(1, Ordering::Relaxed);
                    let prepare_ended = time::Instant::now();

                    // The pre-commit seam. It runs after acquire+submit and
                    // immediately before the surface commit, and the backend
                    // does not call it at all when acquisition failed -- so a
                    // frame that was never committed never leaves a pending
                    // identity behind (enzICED #2219).
                    //
                    // The generations come from the ANIMATIONS that produced
                    // each sampled value, which only this thread knows; a track
                    // with no animation behind it is generation 0, explicitly.
                    //
                    // This is `O(tracks * animations)` plus an allocation, and
                    // it runs on every ACTIVE frame -- so it is paid ONLY when
                    // an observer is installed AND that observer will actually
                    // read the slice. The runtime installs an observer even
                    // where no `wp_presentation` global exists (X11, or a
                    // compositor without the protocol); there the observer only
                    // counts the frame and returns before touching a track, and
                    // `Vec::new()` allocates nothing (enzICED #2219).
                    let listener = observer.get();
                    // The stage ledger binds the same generations, so it is a
                    // second reason to derive them (#2250).
                    let wants_generations = stages.get().is_some()
                        || listener.is_some_and(|listener| listener.needs_track_generations());
                    let sampled_generations: Vec<crate::presentation::TrackGeneration> =
                        if wants_generations {
                            let rows: Vec<_> = sampled
                                .iter()
                                .map(|track| crate::presentation::TrackGeneration {
                                    node: track.node,
                                    generation: state
                                        .animations
                                        .iter()
                                        .find(|animation| animation.node == track.node)
                                        .map_or(0, |animation| animation.generation),
                                })
                                .collect();
                            let _ = counters
                                .generation_rows
                                .fetch_add(rows.len() as u64, Ordering::Relaxed);
                            if !rows.is_empty() {
                                let _ = counters
                                    .generation_allocations
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            rows
                        } else {
                            Vec::new()
                        };

                    let layout = if listener.is_some_and(|observer| observer.needs_track_generations()) {
                        match backend.layout_sample() {
                            Ok(value) => value,
                            Err(crate::core::renderer::presentation::Error::Budget) => {
                                // Valid replay may continue without an input lease while feedback retains the bounded snapshots.
                                let _ = counters.layout_snapshot_budget_skips.fetch_add(1, Ordering::Relaxed);
                                None
                            }
                            Err(error) => break 'running Stop::Presentation(error),
                        }
                    } else { None };
                    let mut submitted_key = None;
                    let mut on_pre_present = || {
                        if let Some(observer) = listener {
                            submitted_key = observer.pre_commit(PendingSubmission {
                                layout: layout.clone(),
                                owner: crate::presentation::FrameOwner::Lane,
                                scene_generation: state.generation,
                                sampled_at: now,
                                tracks: &sampled_generations,
                            });
                        }
                    };

                    let present_outcome = backend.present(
                        &mut surface,
                        &viewport,
                        state.background,
                        &mut on_pre_present,
                    );

                    // enzICED #2219: seal the render-lane stages against the
                    // EXACT key the pre-commit hook registered. A frame the
                    // sink could not identify (no protocol, refused request)
                    // seals with `None`, which discards the spans rather than
                    // letting them drift onto the next frame.
                    if let Some(ledger) = stages.get() {
                        ledger.record(
                            crate::presentation::FrameOwner::Lane,
                            crate::presentation::FrameStage::Animate,
                            animate_started,
                            animate_ended,
                        );
                        ledger.record(
                            crate::presentation::FrameOwner::Lane,
                            crate::presentation::FrameStage::Replay,
                            started,
                            prepare_ended,
                        );

                        // Acquire/Prepare/Submit/Present are backend-internal.
                        // A backend that cannot report them leaves them
                        // missing, and the ledger's verdict refuses the run.
                        if let Some(spans) = backend.take_present_stages() {
                            for (stage, span) in [
                                (crate::presentation::FrameStage::Acquire, spans.acquire),
                                (crate::presentation::FrameStage::Prepare, spans.prepare),
                                (crate::presentation::FrameStage::Submit, spans.submit),
                                (crate::presentation::FrameStage::Present, spans.present),
                            ] {
                                ledger.record(
                                    crate::presentation::FrameOwner::Lane,
                                    stage,
                                    span.0,
                                    span.1,
                                );
                            }
                        }

                        // The row binds the same `(scene, track)` identity the
                        // compositor callback joins against, so a stage row and
                        // a feedback row can never describe different frames
                        // (#2250).
                        let _ = ledger.seal_with_tracks(
                            crate::presentation::FrameOwner::Lane,
                            submitted_key,
                            &sampled_generations,
                        );
                    }

                    match present_outcome {
                        Ok(()) => {
                            last_tracks.clone_from(&sampled);
                            pacer.presented(vblank);
                            if vblank != wake {
                                let _ = counters.paced_frames.fetch_add(1, Ordering::Relaxed);
                            }
                            presented += 1;
                            dirty_since_present = false;
                            let arrived = time::Instant::now();
                            if let Some(previous) = last_presented_at.replace(arrived) {
                                frame_interval = arrived
                                    .saturating_duration_since(previous)
                                    .clamp(MIN_FRAME_INTERVAL, MAX_FRAME_INTERVAL);
                            }
                            let _ = counters.presented.fetch_add(1, Ordering::Relaxed);
                            telemetry::bump(&telemetry::PRESENTED, 1);

                            let presented_at = time::Instant::now();

                            if let Some(origin) = counters.clock_origin.get() {
                                counters.last_presented.publish(vblank.saturating_duration_since(*origin));
                            }

                            // Everything below is instrumentation, and it is
                            // skipped entirely unless a harness opted in: no
                            // track clone, no mutex, no ring push on a default
                            // build (#2219 review finding 7).
                            if telemetry::is_enabled() {
                            let frame = Frame {
                                at: presented_at,
                                present: started.elapsed(),
                                tracks: sampled.clone(),
                                quality,
                                scene: state.generation,
                            };

                            if let Some(log) = log.as_mut() {
                                use std::io::Write as _;

                                let track = trace_track.and_then(|index| frame.tracks.get(index));
                                let submitted_sequence = submitted_key
                                    .map_or_else(|| String::from("null"), |key| key.submitted_sequence.to_string());
                                let surface_generation = submitted_key
                                    .map_or_else(|| String::from("null"), |key| key.surface_generation.to_string());
                                let ownership_generation = submitted_key
                                    .map_or_else(|| String::from("null"), |key| key.ownership_generation.to_string());
                                let window = submitted_key
                                    .map_or_else(|| String::from("null"), |key| key.window_id.to_string());
                                let _ = writeln!(
                                    log,
                                    // `clip_w`/`clip_h` are recorded as null when
                                    // the sampled track carries no clip, never
                                    // omitted: a gate that cannot tell "no clip"
                                    // from "not measured" fails open on every
                                    // clip-travelled effect (#2219).
                                    "{{\"record\":\"lane_frame\",\"thread\":\"render-lane\",\"present_ms\":{:.3},\"elapsed_ms\":{:.3},\"scene\":{},\"window\":{},\"surface_generation\":{},\"ownership_generation\":{},\"submitted_sequence\":{},\"milestone\":\"{:?}\",\"node\":{},\"dx\":{:.3},\"dy\":{:.3},\"opacity\":{:.4},\"scale\":{:.5},\"scale_origin_x\":{:.3},\"scale_origin_y\":{:.3},\"clip_w\":{},\"clip_h\":{},\"tracks\":{},\"owned_layout\":{},\"dx_exact\":{},\"dy_exact\":{},\"wake_ms\":{:.3},\"sampled_ms\":{:.3}}}",
                                    frame.present.as_secs_f64() * 1000.0,
                                    frame
                                        .at
                                        .saturating_duration_since(lane_started)
                                        .as_secs_f64()
                                        * 1000.0,
                                    frame.scene,
                                    window,
                                    surface_generation,
                                    ownership_generation,
                                    submitted_sequence,
                                    frame.quality,
                                    track.map_or(0, |track| track.node.0),
                                    track.map_or(0.0, |track| f64::from(track.translation.x)),
                                    track.map_or(0.0, |track| f64::from(track.translation.y)),
                                    track.map_or(1.0, |track| f64::from(track.opacity)),
                                    track.map_or(1.0, |track| f64::from(track.scale)),
                                    track.map_or(0.0, |track| f64::from(track.scale_origin.x)),
                                    track.map_or(0.0, |track| f64::from(track.scale_origin.y)),
                                    track.and_then(|track| track.clip).map_or_else(
                                        || String::from("null"),
                                        |clip| format!("{:.3}", clip.width),
                                    ),
                                    track.and_then(|track| track.clip).map_or_else(
                                        || String::from("null"),
                                        |clip| format!("{:.3}", clip.height),
                                    ),
                                    frame.tracks.len(),
                                    trace_owned_layout,
                                    track.map_or(0.0, |track| track.translation.x),
                                    track.map_or(0.0, |track| track.translation.y),
                                    // enzUI-ICED #3082: when the frame woke and
                                    // the instant its motion was sampled for,
                                    // on the same origin as `elapsed_ms`.
                                    wake.saturating_duration_since(lane_started).as_secs_f64() * 1000.0,
                                    now.saturating_duration_since(lane_started).as_secs_f64() * 1000.0,
                                );

                                // The curve the lane is sampling, so the trace
                                // explains a settle instead of merely showing a
                                // gap (#2219 B6).
                                for animation in &state.animations {
                                    let _ = writeln!(
                                        log,
                                        "{{\"record\":\"lane_curve\",\"node\":{},\"generation\":{},\"duration_ms\":{:.3},\"remaining_ms\":{:.3},\"finished\":{}}}",
                                        animation.node.0,
                                        animation.generation,
                                        animation.duration.as_secs_f64() * 1000.0,
                                        animation
                                            .duration
                                            .saturating_sub(now.saturating_duration_since(animation.start))
                                            .as_secs_f64()
                                            * 1000.0,
                                        animation.is_finished(now),
                                    );
                                }
                            }

                            if let Ok(mut trace) = trace.lock() {
                                if trace.len() == TRACE_CAPACITY {
                                    let _ = trace.pop_front();
                                }
                                trace.push_back(frame);
                            }
                            }
                        }
                        Err(SurfaceError::Occluded) => break 'running Stop::Occluded,
                        Err(surface_error) => {
                            let _ = counters.surface_errors.fetch_add(1, Ordering::Relaxed);
                            error = Some(surface_error.clone());
                            break 'running Stop::Surface(surface_error);
                        }
                    }

                    if !still_running {
                        // Settled: the last value has been presented. Ownership
                        // goes back so the UI thread can commit final layout.
                        break 'running Stop::Settled;
                    }
                };

                state.tracks = last_tracks;

                if matches!(stop, Stop::Settled | Stop::Occluded | Stop::ReplayRefused | Stop::Presentation(_) | Stop::Surface(_)) {
                    state.animations.clear();
                }

                if let Some(log) = log.as_mut() {
                    use std::io::Write as _;
                    let _ = writeln!(
                        log,
                        "{{\"record\":\"lane_stop\",\"reason\":\"{stop:?}\",\"presented\":{presented}}}"
                    );
                }

                counters.running.store(false, Ordering::Release);
                telemetry::bump(&telemetry::STOPPED, 1);

                let shutting_down = stop == Stop::Shutdown;
                // Asynchronous retirement and backend self-stops must wake the UI;
                // a synchronous reclaim already has a waiting receiver.
                let self_stopped = retired
                    || matches!(
                        stop,
                        Stop::Settled
                            | Stop::Occluded
                            | Stop::ReplayRefused
                            | Stop::Presentation(_)
                            | Stop::Surface(_)
                    );
                let last_tracks = state.tracks.clone();

                publish_stop(&counters, self_stopped, || {
                    let _ = replies.send(Reply::Surface {
                        surface,
                        report: Report {
                            stop,
                            presented,
                            error,
                            last_tracks,
                        },
                    });
                });

                if self_stopped
                    && let Some(wake) = wake.get()
                {
                    wake();
                }

                if shutting_down {
                    return;
                }
            }
        })
        .expect("spawn the iced animation lane");

    Lane {
        to_worker,
        from_worker,
        worker: Some(worker),
        counters,
        trace,
        wake,
        observer,
        stages,
        active_scene: None,
        quality,
        running: false,
        poisoned: false,
    }
}

/// Merges freshly published animations into the lane's set.
///
/// A re-publication of the same `generation` is a *continuation*: it is
/// ignored, so a UI thread that keeps republishing an unchanged travel cannot
/// restart or jitter a track the lane is already presenting. A new generation
/// is a retarget and replaces it.
fn merge_animations(current: &mut Vec<TrackAnimation>, published: Vec<TrackAnimation>) {
    for animation in published {
        match current
            .iter_mut()
            .find(|existing| existing.node == animation.node)
        {
            Some(existing) => {
                // The same travel republished from its first instant changes
                // nothing; a travel whose clock moved (the app shell delays a
                // docked travel by a stall, enzUI-ICED #3660) takes the new
                // timing, which the commit's layout was laid out on.
                if animation.generation != existing.generation
                    || animation.start != existing.start
                    || animation.duration != existing.duration
                {
                    *existing = animation;
                }
            }
            None => current.push(animation),
        }
    }
}

impl<S> Lane<S> {
    /// Whether the worker is presenting this window right now.
    ///
    /// A lane that stopped on its own is **not** running even though it still
    /// physically holds the surface until the next
    /// [`reclaim`](Self::reclaim): treating it as running would let the UI
    /// thread keep skipping presents for a picture nobody advances.
    pub fn is_running(&self) -> bool {
        self.holds_surface() && !self.has_stopped()
    }

    /// Whether the worker still physically holds the surface.
    ///
    /// This is the ownership question, and it is the one
    /// [`reclaim`](Self::reclaim) must ask: a self-stopped lane holds the
    /// surface until it is taken back.
    pub fn holds_surface(&self) -> bool {
        self.running && !self.poisoned
    }

    /// Whether a self-stopped worker still holds the surface for reclamation.
    pub fn has_stopped(&self) -> bool {
        self.holds_surface() && self.counters.stopped.load(Ordering::Acquire)
    }

    /// Installs the hook the worker calls when it stops on its own
    /// (enzICED #2219).
    ///
    /// The runtime passes its window waker here, which is what turns a settled
    /// or failed lane into an ordinary UI frame instead of a parked animation.
    /// Only the first call takes effect.
    pub fn on_stop(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.wake.set(Arc::new(wake));
    }

    /// Installs the per-frame stage ledger the worker seals every presented
    /// frame into (enzICED #2219).
    ///
    /// Without one the lane records nothing and costs nothing. Only the first
    /// call takes effect.
    pub fn record_stages(&self, ledger: Arc<crate::presentation::StageLedger>) {
        let _ = self.stages.set(ledger);
    }

    /// Whether a per-frame stage ledger is installed on this lane.
    pub fn records_stages(&self) -> bool {
        self.stages.get().is_some()
    }

    /// Installs the compositor-feedback hook the worker calls immediately
    /// before every surface commit (enzICED #2219).
    ///
    /// Without one, the lane commits exactly as before and every frame stays
    /// honestly [`PresentationQuality::SurfacePresented`]. Only the first call
    /// takes effect, so a re-handed-over lane cannot end up with two sinks
    /// competing for the same identities.
    pub fn observe_presentation(&self, observer: Arc<dyn PresentationObserver>) {
        let _ = self.observer.set(observer);
    }

    /// Whether a compositor-feedback observer is installed.
    pub fn observes_presentation(&self) -> bool {
        self.observer.get().is_some()
    }

    /// Whether the lane can still be used.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// The strongest milestone this lane's `present` establishes.
    pub fn presentation_quality(&self) -> PresentationQuality {
        self.quality
    }

    /// Animation-only frames the lane has presented since it was spawned.
    pub fn presented(&self) -> u64 {
        self.counters.presented.load(Ordering::Relaxed)
    }

    /// Scene replays the lane has performed.
    pub fn replays(&self) -> u64 {
        self.counters.replays.load(Ordering::Relaxed)
    }

    /// Returns bounded publication queue occupancy and refusal counters.
    pub fn publication_statistics(&self) -> PublicationStatistics {
        self.to_worker.statistics()
    }

    /// Successful replays whose snapshot hit its budget, including any later failed surface acquisition.
    /// The skipped snapshot supplies no projected-input authority for that replay.
    pub fn layout_snapshot_budget_skips(&self) -> u64 {
        self.counters
            .layout_snapshot_budget_skips
            .load(Ordering::Relaxed)
    }

    /// Animation frames the lane did not present because their sampled
    /// tracks differed from the last presented ones by less than a visible
    /// amount (enzUI-ICED #3084).
    pub fn invisible_skips(&self) -> u64 {
        self.counters.invisible_skips.load(Ordering::Relaxed)
    }

    /// Presented frames whose motion was sampled at the predicted
    /// presentation instant from the compositor's vblank grid, rather than at
    /// the instant the lane woke (enzUI-ICED #3082).
    pub fn paced_frames(&self) -> u64 {
        self.counters.paced_frames.load(Ordering::Relaxed)
    }

    /// Replay refusals that return ownership instead of presenting an invalid scene.
    pub fn replay_refusals(&self) -> u64 {
        self.counters.replay_refusals.load(Ordering::Relaxed)
    }

    /// Atomic scene commits the lane has taken.
    pub fn scene_commits(&self) -> u64 {
        self.counters.scene_commits.load(Ordering::Relaxed)
    }

    /// Deterministic presentation-identity work: `(rows, allocations)`.
    ///
    /// An observer that returns `false` from
    /// [`PresentationObserver::needs_track_generations`] must leave both at
    /// zero even while animated frames are committed.
    pub fn presentation_identity_work(&self) -> (u64, u64) {
        (
            self.counters.generation_rows.load(Ordering::Relaxed),
            self.counters.generation_allocations.load(Ordering::Relaxed),
        )
    }

    /// The sample instant of the last successful backend commit, not its completion time.
    ///
    /// This does not substitute for compositor-confirmed feedback. It remains
    /// available without taking a lock shared with the presentation worker.
    pub fn last_presented_at(&self) -> Option<time::Instant> {
        self.counters
            .clock_origin
            .get()?
            .checked_add(self.counters.last_presented.elapsed()?)
    }

    /// Takes the recorded frame trace, leaving the ring empty.
    pub fn drain_trace(&self) -> Vec<Frame> {
        match self.trace.lock() {
            Ok(mut trace) => trace.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Commits a scene to the lane atomically.
    ///
    /// `generation` is assigned by the producer when these scene bytes are
    /// recorded. Safe to call while the lane is running: the replacement takes
    /// effect on the next presented frame and live animations continue from
    /// where they are.
    pub fn commit(
        &mut self,
        scene: Recording,
        generation: u64,
        viewport: Viewport,
        background: Color,
        tracks: Vec<Track>,
        animations: Vec<TrackAnimation>,
        sampled_at: Option<time::Instant>,
        presented: bool,
        repainting: bool,
    ) {
        let scene = RevocableRecording::new(scene);
        if let Some(previous) = self.active_scene.replace(scene.clone()) {
            previous.retire();
        }
        self.send(Request::Commit {
            scene,
            generation,
            viewport,
            background,
            tracks,
            animations,
            sampled_at,
            presented,
            repainting,
        });
    }

    /// Updates the tracks/animations without replacing the scene.
    pub fn animate(&mut self, tracks: Vec<Track>, animations: Vec<TrackAnimation>) {
        self.send(Request::Animate { tracks, animations });
    }

    /// Retires queued and installed scene revisions without destroying the reusable lane.
    pub fn retire_scene(&mut self) {
        if let Some(scene) = self.active_scene.take() {
            scene.retire();
        }
        self.send(Request::RetireScene);
    }

    /// Tells the lane the surface geometry changed.
    pub fn configure(&mut self, width: u32, height: u32, viewport: Viewport) {
        self.send(Request::Configure {
            width,
            height,
            viewport,
        });
    }

    /// Hands the surface to the lane and lets it present.
    ///
    /// Returns the surface back when the lane cannot take it (it is poisoned,
    /// or the worker is gone), so the caller can never lose a surface by
    /// starting a dead lane.
    pub fn start(&mut self, surface: S, viewport: Viewport, background: Color) -> Option<S> {
        if self.poisoned || self.running {
            return Some(surface);
        }

        self.counters.stopped.store(false, Ordering::Release);

        match self.to_worker.send(Request::Start {
            surface,
            viewport,
            background,
        }) {
            Ok(()) => {
                self.running = true;
                None
            }
            Err(mpsc::SendError(Request::Start { surface, .. })) => {
                self.poisoned = true;
                Some(surface)
            }
            Err(_) => {
                self.poisoned = true;
                None
            }
        }
    }

    /// Takes the surface back from the lane, blocking until the worker has
    /// finished the frame it is presenting.
    ///
    /// Returns `None` when the lane does not hold the surface.
    pub fn reclaim(&mut self) -> Option<(S, Report)> {
        if !self.holds_surface() {
            return None;
        }

        self.counters.stopped.store(false, Ordering::Release);
        self.send(Request::Reclaim);

        match self.from_worker.recv_timeout(RECLAIM_TIMEOUT) {
            Ok(Reply::Surface { surface, report }) => {
                self.running = false;
                Some((surface, report))
            }
            Ok(Reply::Stopped) => {
                // The worker exited without returning the surface: it was
                // dropped with the thread. Nothing else may assume it exists.
                self.running = false;
                self.poisoned = true;
                None
            }
            Err(_) => {
                self.running = false;
                self.poisoned = true;
                None
            }
        }
    }

    /// Returns a reclaimed surface only after confirmed worker retirement.
    /// Use `shutdown_checked` to distinguish absence from quarantined ownership.
    pub fn shutdown(self) -> Option<S> {
        let result = self.shutdown_checked();
        match result.retirement {
            Retirement::Joined => result.surface,
            Retirement::Quarantined => None,
        }
    }

    /// Requests retirement with bounded protocol waits; quarantine is not resource reclamation.
    pub fn shutdown_checked(mut self) -> Shutdown<S> {
        self.retire_scene();
        let surface = self.reclaim().map(|(surface, _)| surface);
        let retirement = self.retire_worker();
        Shutdown {
            surface,
            retirement,
        }
    }

    fn retire_worker(&mut self) -> Retirement {
        let _ = self.to_worker.send(Request::Shutdown);
        self.to_worker.close();
        let Some(worker) = self.worker.take() else {
            return Retirement::Joined;
        };
        let deadline = time::Instant::now() + RECLAIM_TIMEOUT;
        while !worker.is_finished() {
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.from_worker.recv_timeout(remaining) {
                Ok(Reply::Surface { .. }) => {}
                Ok(Reply::Stopped) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // The reply can precede backend destruction; only thread completion releases that ownership.
                    while !worker.is_finished() {
                        let remaining = deadline.saturating_duration_since(time::Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        std::thread::park_timeout(remaining.min(time::Duration::from_millis(1)));
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if worker.is_finished() && worker.join().is_ok() {
            Retirement::Joined
        } else {
            self.poisoned = true;
            Retirement::Quarantined
        }
    }

    fn send(&mut self, request: Request<S>) {
        if self.poisoned {
            return;
        }

        if self.to_worker.send(request).is_err() {
            self.poisoned = true;
            self.to_worker.close();
        }
    }
}

impl<S> Drop for Lane<S> {
    fn drop(&mut self) {
        if let Some(scene) = self.active_scene.take() {
            scene.retire();
        }
        let _ = self.retire_worker();
    }
}
