// Modified by enzUI contributors. See FORK.md.
//! Compositor presentation feedback: exact per-frame identity and join
//! (enzICED #2219).
//!
//! A `SurfaceTexture::present()` that returned is a **submission**, not a
//! visible frame. Wayland's `wp_presentation` answers each committed frame
//! asynchronously and out of order, and a frame may be *discarded* rather than
//! shown. This module owns the part of that contract that is independent of
//! any protocol binding: what identifies a submitted frame, how a late
//! callback is matched back to it, and what a match is allowed to advance.
//!
//! It is deliberately backend-free so the join can be exercised exactly --
//! several frames in flight, callbacks delivered out of submission order,
//! each identity dimension swapped independently -- without a compositor.
//! `iced_winit::presentation` builds the real Wayland hub on top of it.
//!
//! # What this refuses to do
//!
//! * Join by arrival order. FIFO is wrong the moment two frames are in
//!   flight, and it is *silently* wrong: it always produces an answer.
//! * Let a `Discarded` frame advance anything. A discarded frame was never
//!   seen, so it is not cadence, not latency and not a presented count.
//! * Invent a compositor milestone when the protocol is absent. A commit with
//!   no feedback stays [`Quality::SurfacePresented`] forever.

mod layout_lease;
pub mod pacing;
pub mod watch;
pub use layout_lease::{
    LayoutLease, LayoutLeaseContext, LayoutLeaseError, LayoutLeaseSlot, NativeInputRecovery,
};
pub use pacing::{FramePacer, PresentationGrid};

use crate::core::renderer::NodeId;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Which side of the handoff submitted a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameOwner {
    /// The application/UI thread presented it.
    Ui,
    /// The render lane worker presented it.
    Lane,
}

/// The immutable identity of one committed frame (enzICED #2219).
///
/// Every dimension is part of the join key. Dropping any one of them lets a
/// callback for a *different* frame update the active lane: a stale surface
/// after a resize, a frame from the previous owner after a handoff, or a
/// replaced scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameKey {
    /// `core::window::Id::as_u64()` of the window that committed.
    pub window_id: u64,
    /// Bumped on every resize/scale reconfigure, surface loss and recreate.
    pub surface_generation: u64,
    /// Bumped only on a *successful* UI <-> lane transfer.
    pub ownership_generation: u64,
    /// Bumped only when a newly recorded scene is installed.
    pub scene_generation: u64,
    /// Per-window monotonic counter, allocated inside the pre-commit hook.
    pub submitted_sequence: u64,
}

/// The generation of one track sampled for a submitted frame.
///
/// Kept as a per-node list rather than collapsed into a scalar: two different
/// track sets can trivially sum or hash alike, and the whole point of the join
/// is that it cannot be accidentally satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackGeneration {
    /// The scene node the track addresses.
    pub node: NodeId,
    /// `TrackAnimation::generation`, or `0` for a sampled track with no live
    /// animation behind it.
    pub generation: u64,
}

/// A frame that has been committed and is awaiting compositor feedback.
#[derive(Debug, Clone)]
pub struct SubmittedFrame {
    /// Allocations sampled for this exact submission; not a hit-test or accessibility lease.
    pub layout: Option<Arc<crate::core::renderer::presentation::Sample>>,
    /// Its immutable identity.
    pub key: FrameKey,
    /// When the tracks were sampled -- the anchor a reversal resumes from.
    pub sampled_at: std::time::Instant,
    /// The exact track generations sampled for this frame, sorted by node.
    pub tracks: Arc<[TrackGeneration]>,
    /// Who committed it.
    pub owner: FrameOwner,
}

impl SubmittedFrame {
    /// Builds a submitted frame, normalising the track list so the join is
    /// order-independent.
    pub fn new(
        key: FrameKey,
        sampled_at: std::time::Instant,
        owner: FrameOwner,
        mut tracks: Vec<TrackGeneration>,
    ) -> Self {
        tracks.sort_by_key(|track| track.node.0);

        Self {
            layout: None,
            key,
            sampled_at,
            tracks: tracks.into(),
            owner,
        }
    }
}

/// The compositor's raw answer for a presented frame.
///
/// Every field is the protocol's own value, preserved verbatim. A local
/// `Instant` is derived separately and only when the clock conversion
/// succeeds; it is never substituted for a missing protocol value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationStamp {
    /// `wp_presentation.clock_id`, the POSIX clock the timestamp is in.
    pub clock_id: u32,
    /// Presentation timestamp, seconds part (hi<<32 | lo).
    pub seconds: u64,
    /// Presentation timestamp, nanoseconds part.
    pub nanoseconds: u32,
    /// Refresh period in nanoseconds; `0` means "unknown", never 16_666_667.
    pub refresh_ns: u32,
    /// Compositor frame sequence (hi<<32 | lo). Output-specific.
    pub sequence: u64,
    /// Raw `wp_presentation_feedback.kind` bits, unknown bits preserved.
    pub flags_raw: u32,
    /// Protocol IDs of the `wl_output`s this frame was presented on.
    pub output_ids: Arc<[u32]>,
}

impl PresentationStamp {
    /// The refresh period, or `None` when the compositor did not state one.
    ///
    /// There is deliberately no fallback constant here: a hard-coded 16.67 ms
    /// is exactly the assumption #2219 forbids.
    pub fn refresh(&self) -> Option<std::time::Duration> {
        (self.refresh_ns > 0).then(|| std::time::Duration::from_nanos(u64::from(self.refresh_ns)))
    }

    /// The timestamp in nanoseconds of `clock_id`'s clock.
    pub fn timestamp_ns(&self) -> u64 {
        self.seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(self.nanoseconds))
    }

    /// The vblank grid this presentation anchors (enzUI-ICED #3082).
    ///
    /// `clock_now` is `clock_id`'s clock read at `now`. The stamp's age on
    /// that clock carries it into this process's `Instant`. The period is the
    /// stated refresh, else `measured` (the observed presentation cadence; a
    /// headless or nested compositor states `0`).
    ///
    /// `None` when there is no period or the stamp is ahead of the clock read
    /// (a different clock).
    ///
    /// A stated refresh anchors the grid at any age. The compositor presents
    /// on that period whether or not this surface submitted, so the phase an
    /// old stamp gives is still the phase of the next vblank (period rounding
    /// drifts it by nanoseconds per second). Refusing it after an idle second
    /// left the first frame of every travel sampled at wake, and a pacer never
    /// adopts a grid mid-travel, so an animation started from idle, the usual
    /// case, was never paced (enzUI-ICED #3085). If the mode or output did
    /// change, the first frame is off by at most one period and the stamp it
    /// produces corrects the next. Only a `measured` period goes stale:
    /// without a stated refresh the stamp must be at most [`GRID_MAX_AGE`]
    /// old.
    pub fn grid(
        &self,
        now: std::time::Instant,
        clock_now: std::time::Duration,
        measured: Option<std::time::Duration>,
    ) -> Option<PresentationGrid> {
        let age = u64::try_from(clock_now.as_nanos())
            .ok()?
            .checked_sub(self.timestamp_ns())
            .map(std::time::Duration::from_nanos)?;
        let period = match self.refresh() {
            Some(stated) => stated,
            None if age <= GRID_MAX_AGE => measured?,
            None => return None,
        };
        PresentationGrid::new(now.checked_sub(age)?, period)
    }
}

/// How old a presentation may be and still anchor the vblank grid with a
/// measured period (enzUI-ICED #3082). A stated refresh has no age limit.
pub const GRID_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(1);

/// How one committed frame ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The compositor showed it.
    Presented(PresentationStamp),
    /// The compositor threw it away; it was never visible.
    Discarded,
    /// The feedback object died without an answer (surface or connection
    /// gone). Not a presentation and not a discard.
    Aborted,
}

/// The honest milestone a frame reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// `SurfaceTexture::present()` returned. Says nothing about visibility.
    SurfacePresented,
    /// A matching `wp_presentation_feedback.presented` arrived.
    CompositorPresented,
}

/// A joined feedback result.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The frame it belongs to, proven by exact key equality.
    pub frame: SubmittedFrame,
    /// What the compositor said.
    pub outcome: Outcome,
    /// The milestone this frame reached.
    pub quality: Quality,
}

/// Why a feedback event could not be joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// No pending frame carries this exact key.
    Unknown,
    /// The numeric key existed, but owner/sample/track identity differed.
    IdentityMismatch,
    /// A frame with this key was already resolved; a protocol event must
    /// complete exactly once.
    AlreadyResolved,
}

/// Bounded pending-frame registry and exact join (enzICED #2219).
///
/// Shared between the commit site (UI thread or lane worker) and the protocol
/// dispatcher thread, so it locks internally and never blocks on the UI.
#[derive(Debug)]
pub struct PresentationSink {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    closed: bool,
    geometry_waker: Option<std::task::Waker>,
    pending: VecDeque<SubmittedFrame>,
    resolved: VecDeque<Resolved>,
    capacity: usize,
    /// Frames committed while the pending map was full, or while the protocol
    /// was unavailable. They stay `SurfacePresented` forever.
    unfeedbacked: u64,
    /// While true, no new feedback may be requested: the map is saturated and
    /// evicting a pending identity would let its callback later attach to a
    /// different frame.
    saturated: bool,
    presented: u64,
    discarded: u64,
    aborted: u64,
    /// The last frame that was actually SEEN, and its stamp. Never advanced by
    /// a discard.
    last_presented: Option<(SubmittedFrame, PresentationStamp)>,
    /// The output set the current cadence series belongs to. Sequence numbers
    /// are output-specific, so a change resets the series rather than
    /// producing a nonsense delta.
    cadence_outputs: Option<Arc<[u32]>>,
    /// Presentation timestamps of consecutive presented frames on one output
    /// set, in nanoseconds of the compositor clock.
    cadence: Vec<u64>,
    /// The shortest period measured on this output set. A surface that has
    /// gone back to presenting a frame now and then keeps the period its last
    /// continuous animation showed.
    measured_period: Option<std::time::Duration>,
}

impl Default for PresentationSink {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// How many frames may be in flight before feedback requests are suspended.
///
/// A compositor that never answers must not grow this without bound; a cap
/// that silently evicted the oldest entry would let its late callback join a
/// *different* frame, which is the failure mode this whole module exists to
/// prevent. So the cap suspends requests instead.
pub const DEFAULT_CAPACITY: usize = 64;

impl PresentationSink {
    /// Creates an empty sink with the given in-flight capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                closed: false,
                geometry_waker: None,
                pending: VecDeque::new(),
                resolved: VecDeque::new(),
                capacity: capacity.max(1),
                unfeedbacked: 0,
                saturated: false,
                presented: 0,
                discarded: 0,
                aborted: 0,
                last_presented: None,
                cadence_outputs: None,
                cadence: Vec::new(),
                measured_period: None,
            }),
        }
    }

    /// Replaces the wake recipient for newly confirmed owned-layout frames.
    /// Waking occurs outside the sink lock and grants no geometry or input authority.
    pub fn set_geometry_waker(&self, waker: std::task::Waker) {
        let previous = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            inner.geometry_waker.replace(waker)
        };
        drop(previous);
    }

    fn resolve_and_wake(
        &self,
        mut inner: std::sync::MutexGuard<'_, Inner>,
        index: usize,
        outcome: Outcome,
    ) -> Result<Resolved, JoinError> {
        let result = Self::resolve_at(&mut inner, index, outcome);
        let wake = result
            .as_ref()
            .ok()
            .filter(|resolved| {
                resolved.quality == Quality::CompositorPresented
                    && resolved.frame.owner == FrameOwner::Lane
                    && resolved.frame.layout.is_some()
                    && inner
                        .last_presented
                        .as_ref()
                        .is_some_and(|(frame, _)| frame.key == resolved.frame.key)
            })
            .and_then(|_| inner.geometry_waker.clone());
        drop(inner);
        if let Some(waker) = wake {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake()));
        }
        result
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a frame that is about to be committed, returning whether
    /// feedback may be requested for it.
    ///
    /// `false` means the caller must **not** issue a protocol request and must
    /// record the frame honestly as [`Quality::SurfacePresented`].
    pub fn register(&self, frame: SubmittedFrame) -> bool {
        let mut inner = self.lock();

        if inner.closed {
            inner.unfeedbacked += 1;
            return false;
        }
        if inner.pending.len() >= inner.capacity {
            inner.saturated = true;
            inner.unfeedbacked += 1;
            return false;
        }

        // An identity must be unique while in flight, or two callbacks could
        // both match and the second would silently overwrite the first.
        if inner.pending.iter().any(|pending| pending.key == frame.key) {
            inner.unfeedbacked += 1;
            return false;
        }

        inner.pending.push_back(frame);
        true
    }

    /// Records a commit that could not request feedback at all -- no
    /// `wp_presentation` global, a dead proxy, or a non-Wayland backend.
    pub fn record_unfeedbacked(&self) {
        self.lock().unfeedbacked += 1;
    }

    /// Joins a completed feedback event to its exact pending frame.
    ///
    /// The whole [`FrameKey`] must match. There is no positional fallback and
    /// no nearest match: an event that names no pending frame is reported as
    /// [`JoinError::Unknown`] and changes nothing.
    pub fn complete(&self, key: FrameKey, outcome: Outcome) -> Result<Resolved, JoinError> {
        let inner = self.lock();

        let Some(index) = inner.pending.iter().position(|pending| pending.key == key) else {
            let already = inner
                .resolved
                .iter()
                .any(|resolved| resolved.frame.key == key);

            return Err(if already {
                JoinError::AlreadyResolved
            } else {
                JoinError::Unknown
            });
        };

        self.resolve_and_wake(inner, index, outcome)
    }

    /// Joins a callback carrying the exact immutable submission installed as
    /// that feedback object's user data.
    ///
    /// In addition to the numeric key, owner, sampled instant, and the complete
    /// sorted node/generation list must match. This is the live Wayland path;
    /// [`complete`](Self::complete) remains available for deterministic tests
    /// that exercise individual key dimensions.
    pub fn complete_exact(
        &self,
        submitted: &SubmittedFrame,
        outcome: Outcome,
    ) -> Result<Resolved, JoinError> {
        let inner = self.lock();
        let Some(index) = inner
            .pending
            .iter()
            .position(|pending| pending.key == submitted.key)
        else {
            let already = inner
                .resolved
                .iter()
                .any(|resolved| resolved.frame.key == submitted.key);
            return Err(if already {
                JoinError::AlreadyResolved
            } else {
                JoinError::Unknown
            });
        };
        let pending = &inner.pending[index];
        let same_layout = match (&pending.layout, &submitted.layout) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same_layout
            || pending.owner != submitted.owner
            || pending.sampled_at != submitted.sampled_at
            || pending.tracks.as_ref() != submitted.tracks.as_ref()
        {
            return Err(JoinError::IdentityMismatch);
        }
        self.resolve_and_wake(inner, index, outcome)
    }

    fn resolve_at(
        inner: &mut Inner,
        index: usize,
        outcome: Outcome,
    ) -> Result<Resolved, JoinError> {
        let frame = inner.pending.remove(index).expect("indexed pending frame");

        if inner.pending.len() < inner.capacity {
            inner.saturated = false;
        }

        let quality = match &outcome {
            Outcome::Presented(stamp) => {
                inner.presented += 1;
                inner.note_cadence(&frame, stamp);
                Quality::CompositorPresented
            }
            // A discarded frame was never on screen. It advances no counter
            // that means "the user saw this", no cadence sample, and no
            // reversal anchor. Collapsing it into the presented path is the
            // sabotage the discard gate reverses.
            Outcome::Discarded => {
                inner.discarded += 1;
                Quality::SurfacePresented
            }
            Outcome::Aborted => {
                inner.aborted += 1;
                Quality::SurfacePresented
            }
        };

        let resolved = Resolved {
            frame,
            outcome,
            quality,
        };

        watch::publish(&resolved);
        let mut telemetry = resolved.clone();
        telemetry.frame.layout = None;
        inner.resolved.push_back(telemetry);
        while inner.resolved.len() > inner.capacity {
            let _ = inner.resolved.pop_front();
        }

        Ok(resolved)
    }

    /// Drains joined telemetry without allocation snapshots; the latest confirmed frame retains its snapshot.
    pub fn drain(&self) -> Vec<Resolved> {
        self.lock().resolved.drain(..).collect()
    }

    /// The last frame the compositor actually showed, with its raw stamp.
    ///
    /// This is what a retarget or reversal anchors on: the value that was on
    /// screen, sampled at `frame.sampled_at`, not the arrival time of the
    /// callback and not this thread's own stale sample.
    pub fn last_presented(&self) -> Option<(SubmittedFrame, PresentationStamp)> {
        self.lock().last_presented.clone()
    }

    /// The last presented frame, but only if it still belongs to the surface,
    /// ownership, and scene generation currently in force **and** presented the
    /// exact track generations that are active now (enzICED #2219).
    ///
    /// Older feedback stays valid *telemetry*; it must not steer a lane that
    /// has since been resized, handed over, or given a replacement scene.
    ///
    /// # Why the track generations are part of the question
    ///
    /// A retarget replaces the travel *without* touching any of the three
    /// numeric generations above: the surface is the same, the owner is the
    /// same, and a retarget that publishes only a new curve records no new
    /// scene. A frame submitted just before the retarget can therefore still be
    /// presented *after* it -- it is genuinely the newest thing the user saw --
    /// and anchoring the replacement travel at that frame's `sampled_at`
    /// resumes the NEW animation from the OLD one's instant, which jumps or
    /// reverses the picture. The generation list is the only dimension that
    /// distinguishes them, so steering is bound to it and nothing else can
    /// stand in for it.
    ///
    /// `active` is the `(node, generation)` set the effects have published for
    /// the travel running now. Every one of those nodes must appear in the
    /// presented frame **at the same generation**, and an empty `active` never
    /// anchors: there is no travel to resume, so there is nothing an anchor
    /// could mean. That makes the rule fail-closed -- an anchor that cannot be
    /// proven to belong to the active travel is refused, and the caller keeps
    /// its own last sample instead of adopting a foreign one.
    ///
    /// It is deliberately **not** whole-set equality. The lane samples its own
    /// retained state, so a presented frame legitimately carries nodes the UI
    /// thread is no longer publishing -- a static track kept at its last value,
    /// or an animation the lane still holds for a sibling. Demanding set
    /// equality would refuse those frames too, which does not make anything
    /// safer: it silently disables the anchor altogether and puts every
    /// reversal back on the UI thread's stale sample (#1753/#2226). Extra
    /// presented nodes are therefore ignored; a *disagreeing* generation on a
    /// node the travel owns is not.
    pub fn active_anchor(
        &self,
        surface_generation: u64,
        ownership_generation: u64,
        scene_generation: u64,
        active: &[TrackGeneration],
    ) -> Option<(SubmittedFrame, PresentationStamp)> {
        self.lock().last_presented.clone().filter(|(frame, _)| {
            frame.key.surface_generation == surface_generation
                && frame.key.ownership_generation == ownership_generation
                && frame.key.scene_generation == scene_generation
                && presented_the_active_travel(frame.tracks.as_ref(), active)
        })
    }

    /// The vblank grid anchored at the newest presented frame, or `None`
    /// (see [`PresentationStamp::grid`]).
    pub fn grid(
        &self,
        now: std::time::Instant,
        clock_now: std::time::Duration,
    ) -> Option<PresentationGrid> {
        let inner = self.lock();
        let measured = inner.measured_period;
        inner
            .last_presented
            .as_ref()
            .and_then(|(_, stamp)| stamp.grid(now, clock_now, measured))
    }

    /// The `wp_presentation` clock of the newest presented frame, which
    /// [`grid`](Self::grid)'s `clock_now` must be read from.
    pub fn last_clock_id(&self) -> Option<u32> {
        self.lock()
            .last_presented
            .as_ref()
            .map(|(_, stamp)| stamp.clock_id)
    }

    /// Compositor-derived refresh period, or `None` when unstated.
    pub fn refresh(&self) -> Option<std::time::Duration> {
        self.lock()
            .last_presented
            .as_ref()
            .and_then(|(_, stamp)| stamp.refresh())
    }

    /// Intervals between consecutive presented frames on one unchanged output
    /// set, in nanoseconds of the compositor's own clock.
    pub fn cadence_intervals_ns(&self) -> Vec<u64> {
        let inner = self.lock();

        inner
            .cadence
            .windows(2)
            .map(|pair| pair[1].saturating_sub(pair[0]))
            .collect()
    }

    /// A snapshot of the sink's counters.
    pub fn stats(&self) -> Stats {
        let inner = self.lock();

        Stats {
            pending: inner.pending.len(),
            presented: inner.presented,
            discarded: inner.discarded,
            aborted: inner.aborted,
            unfeedbacked: inner.unfeedbacked,
            saturated: inner.saturated,
        }
    }

    /// Permanently closes this sink, aborting bounded pending work and releasing the last layout.
    ///
    /// Late registrations are refused; late callbacks cannot recreate the retired anchor.
    pub fn close(&self) -> usize {
        let mut inner = self.lock();
        inner.closed = true;
        let waker = inner.geometry_waker.take();
        let count = inner.pending.len();
        while !inner.pending.is_empty() {
            let _ = Self::resolve_at(&mut inner, 0, Outcome::Aborted);
        }
        inner.last_presented = None;
        inner.cadence.clear();
        inner.cadence_outputs = None;
        inner.measured_period = None;
        drop(inner);
        drop(waker);
        count
    }

    /// Completes every still-pending frame as [`Outcome::Aborted`].
    ///
    /// Called on teardown so no submission is left unresolved -- the live gate
    /// requires every lane submission to end as presented, discarded or
    /// aborted, never as silence.
    pub fn abort_pending(&self) -> usize {
        let keys: Vec<FrameKey> = self.lock().pending.iter().map(|frame| frame.key).collect();

        let mut aborted = 0;

        for key in keys {
            if self.complete(key, Outcome::Aborted).is_ok() {
                aborted += 1;
            }
        }

        aborted
    }
}

impl Inner {
    fn note_cadence(&mut self, frame: &SubmittedFrame, stamp: &PresentationStamp) {
        let timestamp = stamp
            .seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(stamp.nanoseconds));

        // Callbacks may arrive out of submission order. Cadence is ordered by
        // the compositor's timestamp, never callback arrival, and a late
        // callback from an older output set must not reset the active series.
        let newest_timestamp = self.last_presented.as_ref().map(|(_, previous)| {
            previous
                .seconds
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::from(previous.nanoseconds))
        });
        let is_newest = newest_timestamp.is_none_or(|previous| {
            timestamp > previous
                || (timestamp == previous
                    && self.last_presented.as_ref().is_none_or(|(previous, _)| {
                        frame.key.submitted_sequence > previous.key.submitted_sequence
                    }))
        });
        let same_outputs = self
            .cadence_outputs
            .as_ref()
            .is_some_and(|outputs| outputs.as_ref() == stamp.output_ids.as_ref());

        if same_outputs {
            let position = self
                .cadence
                .partition_point(|existing| *existing <= timestamp);
            self.cadence.insert(position, timestamp);
        } else if is_newest {
            self.cadence.clear();
            self.cadence.push(timestamp);
            self.cadence_outputs = Some(Arc::clone(&stamp.output_ids));
            self.measured_period = None;
        }

        if self.cadence.len() > 4096 {
            let _ = self.cadence.remove(0);
        }

        if let Some(period) = pacing::measured_period(&self.cadence) {
            self.measured_period = Some(
                self.measured_period
                    .map_or(period, |fastest| fastest.min(period)),
            );
        }

        if is_newest {
            self.last_presented = Some((frame.clone(), stamp.clone()));
        }
    }
}

/// Whether a presented frame showed the travel that is active now
/// (enzICED #2219).
///
/// One direction only: every node the *active* travel owns must have been
/// presented at that node's current generation. A frame that also presented
/// other nodes still qualifies -- see [`PresentationSink::active_anchor`] for
/// why that asymmetry is the safe one.
///
/// An empty `active` is never a match. "No travel is running" is not "any frame
/// will do": returning `true` there would let the last frame of a finished
/// travel steer whatever starts next.
pub fn presented_the_active_travel(
    presented: &[TrackGeneration],
    active: &[TrackGeneration],
) -> bool {
    if active.is_empty() {
        return false;
    }

    active.iter().all(|wanted| {
        presented
            .iter()
            .any(|shown| shown.node == wanted.node && shown.generation == wanted.generation)
    })
}

/// Counters describing a [`PresentationSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Frames committed and still awaiting an answer.
    pub pending: usize,
    /// Frames the compositor confirmed it showed.
    pub presented: u64,
    /// Frames the compositor threw away.
    pub discarded: u64,
    /// Frames whose feedback object died unanswered.
    pub aborted: u64,
    /// Commits that could not request feedback and remain `SurfacePresented`.
    pub unfeedbacked: u64,
    /// Whether the in-flight cap is currently suspending requests.
    pub saturated: bool,
}

/// The ordered protocol/commit calls one frame made, for the ordering gate.
///
/// The feedback request must reach the connection's request stream *before*
/// the surface commit, or the compositor may present the frame before it has
/// anything to report against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStep {
    /// `wp_presentation.feedback(surface)` was issued.
    RequestedFeedback(u64),
    /// `pre_present_notify()` + `SurfaceTexture::present()`.
    Committed(u64),
    /// Surface acquisition failed; nothing was committed.
    AcquisitionFailed,
}

// ---------------------------------------------------------------------------
// Per-frame stage ledger (enzICED #2219, frame-scoped by #2250)
// ---------------------------------------------------------------------------

/// Which thread a [`FrameStage`] belongs to.
///
/// A stage is owned by exactly one lane. This is the dimension that makes
/// *misattribution* -- a UI-thread span filed as render-lane work, which is the
/// single cheapest way to fake the isolation claim -- a detectable error rather
/// than a plausible number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageLane {
    /// The application/UI thread.
    Ui,
    /// The render-lane worker.
    Lane,
}

impl FrameOwner {
    /// The stage lane this owner records into.
    pub fn stage_lane(self) -> StageLane {
        match self {
            Self::Ui => StageLane::Ui,
            Self::Lane => StageLane::Lane,
        }
    }
}

/// The number of distinct [`FrameStage`]s.
pub const STAGE_COUNT: usize = 13;

/// One timed stage of exactly one committed frame (enzICED #2219/#2250).
///
/// The issue requires the UI pipeline and the render lane to be measured
/// *separately*, per frame, and joined to the exact frame identity. A single
/// aggregate histogram cannot answer "did this animation-only frame rebuild a
/// view?", and a lane row that reports only a total cannot distinguish waiting
/// for a swapchain image ([`Acquire`](Self::Acquire)) from encoding GPU work
/// ([`Prepare`](Self::Prepare)) from handing it to the queue
/// ([`Submit`](Self::Submit)).
///
/// # Cardinality is part of the truth (#2250)
///
/// A presented frame is *not* one pass over each of these. Between two
/// presented frames the event loop may drain **zero** input batches (an
/// animation tick, a timer wake) or **several** (a burst of motion events),
/// and it may rebuild the view zero, one or many times. Recording those as
/// once-per-frame stages made a no-input frame report `Missing(Input)` and a
/// two-batch frame report `Duplicate(Input)`, so ordinary frames were
/// indistinguishable from broken instrumentation. Stages whose real
/// cardinality is `0..n` are therefore [batched](Self::is_batched) and carry
/// every occurrence; the rest occur exactly once and stay required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FrameStage {
    /// UI: event-queue drain and input dispatch. Batched: `0..n` per frame.
    Input,
    /// UI: the redraw pass's own message/update work. Once per frame.
    Update,
    /// UI: application `view` rebuild. Batched: `0..n` per frame.
    View,
    /// UI: `Tree::diff` against retained state. Batched: `0..n` per frame.
    Diff,
    /// UI: widget layout. Batched: `0..n` per frame.
    Layout,
    /// UI: recording the retained scene from widget draw. Once per frame.
    SceneRecord,
    /// UI: taking the surface back from the render lane before presenting.
    /// Once per UI-presented frame.
    Reclaim,
    /// Lane: sampling the presentation clock and advancing tracks.
    Animate,
    /// Lane: replaying the retained scene into renderer primitives.
    ///
    /// This is the lane's own CPU work over the recording. It was previously
    /// called `Prepare`, which left the *backend's* preparation and encoding
    /// with no name of its own (#2250).
    Replay,
    /// Acquiring the next swapchain image, on whichever side presents.
    Acquire,
    /// Backend preparation and command encoding, on whichever side presents.
    ///
    /// For wgpu this is `Renderer::prepare` + `Renderer::render` into a command
    /// encoder plus the staging belt finish -- everything up to, and not
    /// including, the queue submission.
    Prepare,
    /// Handing the encoded commands to the GPU queue, on whichever side
    /// presents.
    Submit,
    /// The presentation request -- feedback registration and the surface
    /// present/commit itself -- on whichever side presents.
    Present,
}

impl FrameStage {
    /// Every stage, in pipeline order.
    pub const ALL: [Self; STAGE_COUNT] = [
        Self::Input,
        Self::Update,
        Self::View,
        Self::Diff,
        Self::Layout,
        Self::SceneRecord,
        Self::Reclaim,
        Self::Animate,
        Self::Replay,
        Self::Acquire,
        Self::Prepare,
        Self::Submit,
        Self::Present,
    ];

    /// The stages a UI-thread frame may report: its own work, then the
    /// surface reclaim and the backend's four spans when it presents.
    pub const UI: [Self; 11] = [
        Self::Input,
        Self::Update,
        Self::View,
        Self::Diff,
        Self::Layout,
        Self::SceneRecord,
        Self::Reclaim,
        Self::Acquire,
        Self::Prepare,
        Self::Submit,
        Self::Present,
    ];

    /// The six stages a render-lane frame reports.
    pub const LANE: [Self; 6] = [
        Self::Animate,
        Self::Replay,
        Self::Acquire,
        Self::Prepare,
        Self::Submit,
        Self::Present,
    ];

    /// The UI stages a presented frame must show **at least once**.
    ///
    /// `Input`, `View`, `Diff` and `Layout` are deliberately absent: their real
    /// cardinality is `0..n` (see [`FrameStage`]), so requiring them would make
    /// a legitimate no-input, no-rebuild frame unusable. `Update` and
    /// `SceneRecord` are required but *not* limited to one occurrence: a live
    /// run measured redraw passes that end without a UI present (the render
    /// lane presents that frame instead), so two update/record passes really do
    /// precede one UI-presented frame. Calling the second one a `Duplicate`
    /// made ordinary lane-served runs unusable (#2250).
    pub const UI_REQUIRED: [Self; 2] = [Self::Update, Self::SceneRecord];

    /// The render-lane stages that occur exactly once per presented frame.
    pub const LANE_REQUIRED: [Self; 6] = Self::LANE;

    /// The stages that legitimately occur zero or many times per frame.
    ///
    /// All of them are UI-thread stages: the render lane runs each of its own
    /// stages exactly once per present, so a second `Acquire` or `Submit` in
    /// one lane frame remains a `Duplicate` error.
    pub const BATCHED: [Self; 6] = [
        Self::Input,
        Self::Update,
        Self::View,
        Self::Diff,
        Self::Layout,
        Self::SceneRecord,
    ];

    /// `(outer, inner)` pairs whose nesting is intentional (#2250).
    ///
    /// The event loop times `Input` and `Update` around passes that themselves
    /// rebuild the view, so `View`/`Diff`/`Layout` really do run *inside* them.
    /// Reporting that as an overlap error refused every ordinary frame; erasing
    /// the outer span instead would delete the only measurement of the pass as
    /// a whole. So the containment is declared, and then *checked*: an inner
    /// span that merely straddles its declared parent is a
    /// [`StageError::NotNested`], and any overlap between two stages that are
    /// not declared here is still a [`StageError::Overlap`].
    pub const NESTING: [(Self, Self); 6] = [
        (Self::Update, Self::View),
        (Self::Update, Self::Diff),
        (Self::Update, Self::Layout),
        (Self::Input, Self::View),
        (Self::Input, Self::Diff),
        (Self::Input, Self::Layout),
    ];

    /// Position in [`Self::ALL`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|stage| *stage == self)
            .unwrap_or_default()
    }

    /// The lane that owns this stage. The backend stages (`Acquire` to
    /// `Present`) belong to the render lane and are also reported by a
    /// UI-presented frame; see [`Self::all_for`].
    pub fn lane(self) -> StageLane {
        match self {
            Self::Input
            | Self::Update
            | Self::View
            | Self::Diff
            | Self::Layout
            | Self::SceneRecord
            | Self::Reclaim => StageLane::Ui,
            Self::Animate
            | Self::Replay
            | Self::Acquire
            | Self::Prepare
            | Self::Submit
            | Self::Present => StageLane::Lane,
        }
    }

    /// Whether this stage legitimately occurs zero or many times per frame.
    pub fn is_batched(self) -> bool {
        matches!(
            self,
            Self::Input | Self::Update | Self::View | Self::Diff | Self::Layout | Self::SceneRecord
        )
    }

    /// Whether `inner` running inside `self` is intentional.
    pub fn nests(self, inner: Self) -> bool {
        Self::NESTING
            .iter()
            .any(|(outer, nested)| *outer == self && *nested == inner)
    }

    /// Stable machine-readable name used by the evidence rows.
    pub fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Update => "update",
            Self::View => "view",
            Self::Diff => "diff",
            Self::Layout => "layout",
            Self::SceneRecord => "scene_record",
            Self::Reclaim => "reclaim",
            Self::Animate => "animate",
            Self::Replay => "replay",
            Self::Acquire => "acquire",
            Self::Prepare => "prepare",
            Self::Submit => "submit",
            Self::Present => "present",
        }
    }

    /// Every stage `owner` may record.
    pub fn all_for(owner: FrameOwner) -> &'static [Self] {
        match owner.stage_lane() {
            StageLane::Ui => &Self::UI,
            StageLane::Lane => &Self::LANE,
        }
    }

    /// The stages required at least once of a frame committed by `owner`.
    ///
    /// A [batched](Self::is_batched) required stage must occur one or more
    /// times; every other required stage must occur exactly once.
    pub fn required_for(owner: FrameOwner) -> &'static [Self] {
        match owner.stage_lane() {
            StageLane::Ui => &Self::UI_REQUIRED,
            StageLane::Lane => &Self::LANE_REQUIRED,
        }
    }
}

/// A half-open `[start, end)` span in nanoseconds from a ledger's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StageSpan {
    /// Start, nanoseconds from [`StageLedger::origin`].
    pub start_ns: u64,
    /// End, nanoseconds from [`StageLedger::origin`].
    pub end_ns: u64,
}

impl StageSpan {
    /// Duration in nanoseconds. Saturating, never negative.
    pub fn duration_ns(self) -> u64 {
        self.end_ns.saturating_sub(self.start_ns)
    }

    /// Whether two spans share any instant.
    ///
    /// Two stages of the same frame and lane are sequential by construction
    /// unless their nesting is declared in [`FrameStage::NESTING`]. An
    /// undeclared overlap therefore means one measured interval was filed under
    /// two names -- the "combined stages" failure -- and is an error, not a
    /// tolerance.
    pub fn overlaps(self, other: Self) -> bool {
        self.start_ns < other.end_ns && other.start_ns < self.end_ns
    }

    /// Whether `other` lies entirely within `self`.
    pub fn contains(self, other: Self) -> bool {
        self.start_ns <= other.start_ns && other.end_ns <= self.end_ns
    }
}

/// Why a frame's stage record cannot be trusted.
///
/// Every variant is fatal for the frame that carries it: the evidence gate
/// scores a frame as usable only when it has no errors and no missing stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    /// A stage was recorded by the wrong lane.
    Misattributed {
        /// The stage that was offered.
        stage: FrameStage,
        /// The owner that offered it.
        owner: FrameOwner,
    },
    /// A once-per-frame stage was recorded twice for one frame.
    Duplicate(FrameStage),
    /// Two stages of one frame cover overlapping time without declared nesting.
    Overlap(FrameStage, FrameStage),
    /// A required stage was never recorded.
    Missing(FrameStage),
    /// A stage ended before it started.
    Inverted(FrameStage),
    /// A stage span started before the frame it was filed against did (#2250).
    ///
    /// This is the cross-frame attribution failure: an input batch, or any
    /// other span, that belongs to an already-sealed frame being counted again
    /// as part of the next one.
    CrossFrame(FrameStage),
    /// A declared-nested stage only partially overlaps its parent (#2250).
    NotNested {
        /// The declared outer stage.
        outer: FrameStage,
        /// The stage that should have been contained by it.
        inner: FrameStage,
    },
}

impl StageError {
    /// Stable machine-readable rendering for the evidence rows.
    pub fn to_json(self) -> String {
        match self {
            Self::Misattributed { stage, owner } => format!(
                "{{\"error\":\"misattributed\",\"stage\":\"{}\",\"owner\":\"{:?}\"}}",
                stage.name(),
                owner
            ),
            Self::Duplicate(stage) => {
                format!("{{\"error\":\"duplicate\",\"stage\":\"{}\"}}", stage.name())
            }
            Self::Overlap(a, b) => format!(
                "{{\"error\":\"overlap\",\"stage\":\"{}\",\"other\":\"{}\"}}",
                a.name(),
                b.name()
            ),
            Self::Missing(stage) => {
                format!("{{\"error\":\"missing\",\"stage\":\"{}\"}}", stage.name())
            }
            Self::Inverted(stage) => {
                format!("{{\"error\":\"inverted\",\"stage\":\"{}\"}}", stage.name())
            }
            Self::CrossFrame(stage) => {
                format!(
                    "{{\"error\":\"cross_frame\",\"stage\":\"{}\"}}",
                    stage.name()
                )
            }
            Self::NotNested { outer, inner } => format!(
                "{{\"error\":\"not_nested\",\"outer\":\"{}\",\"inner\":\"{}\"}}",
                outer.name(),
                inner.name()
            ),
        }
    }
}

/// The sealed per-stage record of one committed frame, bound to its exact
/// [`FrameKey`] (enzICED #2219/#2250).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStages {
    /// The immutable identity of the frame these spans belong to.
    pub key: FrameKey,
    /// Which side committed it.
    pub owner: FrameOwner,
    /// Spans of the once-per-frame stages, indexed by [`FrameStage::index`];
    /// `None` means *not measured*, which is never the same as a zero-duration
    /// stage.
    pub spans: [Option<StageSpan>; STAGE_COUNT],
    /// Every occurrence of each [batched](FrameStage::is_batched) stage, in the
    /// order it was recorded. Empty means the stage genuinely did not run this
    /// frame, which is an ordinary frame -- not missing instrumentation.
    pub batches: [Vec<StageSpan>; STAGE_COUNT],
    /// The exact track generations this frame sampled, sorted by node.
    ///
    /// Bound here, and not merely in the presentation sink, so a stage row
    /// carries the same `(scene, track)` identity a compositor callback joins
    /// against: a row is evidence about one frame's content or it is evidence
    /// about nothing.
    pub tracks: Arc<[TrackGeneration]>,
    /// Nesting pairs that were observed and accepted for this frame.
    pub nested: Vec<(FrameStage, FrameStage)>,
    /// Everything that makes this row unusable as evidence.
    pub errors: Vec<StageError>,
}

impl FrameStages {
    /// The span recorded for a once-per-frame `stage`, or the **first**
    /// occurrence of a batched one.
    ///
    /// Use [`batches`](Self::batches) for a batched stage: collapsing several
    /// input batches into one interval is exactly the fabrication #2250 exists
    /// to prevent.
    pub fn span(&self, stage: FrameStage) -> Option<StageSpan> {
        if stage.is_batched() {
            self.batches[stage.index()].first().copied()
        } else {
            self.spans[stage.index()]
        }
    }

    /// Every occurrence of `stage` in this frame.
    pub fn batches(&self, stage: FrameStage) -> &[StageSpan] {
        &self.batches[stage.index()]
    }

    /// How many times `stage` ran during this frame.
    pub fn occurrences(&self, stage: FrameStage) -> usize {
        if stage.is_batched() {
            self.batches[stage.index()].len()
        } else {
            usize::from(self.spans[stage.index()].is_some())
        }
    }

    /// Total measured time of `stage` across all its occurrences.
    pub fn total_ns(&self, stage: FrameStage) -> u64 {
        if stage.is_batched() {
            self.batches[stage.index()]
                .iter()
                .map(|span| span.duration_ns())
                .sum()
        } else {
            self.spans[stage.index()].map_or(0, StageSpan::duration_ns)
        }
    }

    /// Whether this row may be used as evidence.
    ///
    /// Fail-closed: a row is usable only when every once-per-frame stage
    /// required of its owner is present *and* nothing was duplicated,
    /// overlapped, inverted, misnested, filed by the wrong lane or carried in
    /// from another frame.
    pub fn is_usable(&self) -> bool {
        self.errors.is_empty()
            && FrameStage::required_for(self.owner)
                .iter()
                .all(|stage| self.occurrences(*stage) > 0)
    }

    /// One JSON evidence row.
    ///
    /// A missing once-per-frame stage is emitted as `null`, never omitted and
    /// never zero: a consumer that cannot tell "not measured" from "took no
    /// time" fails open on exactly the frames this issue is about. A batched
    /// stage is emitted as its list of occurrences, so `[]` (did not run) and
    /// `null` (not instrumented for this owner) stay distinct.
    pub fn to_json(&self) -> String {
        let span_json = |span: &StageSpan| {
            format!(
                "{{\"start_ns\":{},\"end_ns\":{},\"duration_ns\":{}}}",
                span.start_ns,
                span.end_ns,
                span.duration_ns()
            )
        };

        let owned = FrameStage::all_for(self.owner);

        let stages = FrameStage::ALL
            .iter()
            .map(|stage| {
                if !owned.contains(stage) {
                    return format!("\"{}\":null", stage.name());
                }

                if stage.is_batched() {
                    let batches = self.batches[stage.index()]
                        .iter()
                        .map(span_json)
                        .collect::<Vec<_>>()
                        .join(",");

                    format!("\"{}\":[{}]", stage.name(), batches)
                } else {
                    match self.spans[stage.index()] {
                        Some(span) => format!("\"{}\":{}", stage.name(), span_json(&span)),
                        None => format!("\"{}\":null", stage.name()),
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(",");

        let errors = self
            .errors
            .iter()
            .map(|error| error.to_json())
            .collect::<Vec<_>>()
            .join(",");

        let nested = self
            .nested
            .iter()
            .map(|(outer, inner)| {
                format!(
                    "{{\"outer\":\"{}\",\"inner\":\"{}\"}}",
                    outer.name(),
                    inner.name()
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let tracks = self
            .tracks
            .iter()
            .map(|track| {
                format!(
                    "{{\"node\":{},\"generation\":{}}}",
                    track.node.0, track.generation
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        format!(
            "{{\"record\":\"frame_stages\",\"window\":{},\"surface_generation\":{},\
\"ownership_generation\":{},\"scene_generation\":{},\"submitted_sequence\":{},\
\"owner\":\"{:?}\",\"lane\":\"{:?}\",\"usable\":{},\"tracks\":[{}],\"stages\":{{{}}},\
\"nested\":[{}],\"errors\":[{}]}}",
            self.key.window_id,
            self.key.surface_generation,
            self.key.ownership_generation,
            self.key.scene_generation,
            self.key.submitted_sequence,
            self.owner,
            self.owner.stage_lane(),
            self.is_usable(),
            tracks,
            stages,
            nested,
            errors
        )
    }
}

/// Counters describing a [`StageLedger`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageStats {
    /// Frames sealed against a real [`FrameKey`].
    pub sealed: u64,
    /// Sealed frames with every required stage and no error.
    pub usable: u64,
    /// Sealed frames missing at least one required stage.
    pub incomplete: u64,
    /// Stages offered by the wrong lane.
    pub misattributed: u64,
    /// Once-per-frame stages offered twice for one frame.
    pub duplicated: u64,
    /// Frames whose stages covered overlapping time without declared nesting.
    pub overlapping: u64,
    /// Frames with a declared-nested stage that only partially overlapped its
    /// parent.
    pub unnested: u64,
    /// Spans rejected because they started before the frame they were offered
    /// to (#2250).
    pub cross_frame: u64,
    /// Frames whose spans were discarded because no key was registered.
    ///
    /// Unjoinable spans are dropped, never carried into the next frame: a
    /// stage attributed to a neighbouring frame is worse than a missing one.
    pub unjoinable: u64,
    /// Sealed rows evicted by the bounded ring.
    pub dropped: u64,
    /// Total batched-stage occurrences observed across all sealed frames.
    pub batch_occurrences: u64,
}

#[derive(Debug, Default)]
struct PendingStages {
    spans: [Option<StageSpan>; STAGE_COUNT],
    batches: [Vec<StageSpan>; STAGE_COUNT],
    errors: Vec<StageError>,
    touched: bool,
}

#[derive(Debug)]
struct LedgerInner {
    origin: std::time::Instant,
    capacity: usize,
    pending: [PendingStages; 2],
    /// Nanoseconds from `origin` at which each lane's current frame began.
    /// Advanced by every seal, so a span from an already-sealed frame cannot
    /// be filed against the next one (#2250).
    frame_start_ns: [u64; 2],
    sealed: VecDeque<FrameStages>,
    stats: StageStats,
}

fn lane_slot(lane: StageLane) -> usize {
    match lane {
        StageLane::Ui => 0,
        StageLane::Lane => 1,
    }
}

/// A bounded, thread-safe ledger of per-frame stage spans (enzICED #2219).
///
/// Each lane accumulates the stages of the frame it is currently building, and
/// [`seal`](Self::seal) binds them to the [`FrameKey`] that the presentation
/// sink registered inside the pre-commit hook. That is the only moment at which
/// the identity exists, which is why the pre-commit return value is load-bearing
/// on the UI path too and may not simply be discarded.
///
/// Sealing is also the **frame boundary**: the accumulator is taken *and* the
/// lane's frame start is advanced, so a span measured before the seal can never
/// be attributed to the frame after it (#2250).
#[derive(Debug)]
pub struct StageLedger {
    inner: Mutex<LedgerInner>,
}

impl StageLedger {
    /// Creates a ledger retaining at most `capacity` sealed rows.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(LedgerInner {
                origin: std::time::Instant::now(),
                capacity: capacity.max(1),
                pending: [PendingStages::default(), PendingStages::default()],
                frame_start_ns: [0, 0],
                sealed: VecDeque::with_capacity(capacity.max(1)),
                stats: StageStats::default(),
            }),
        }
    }

    /// The monotonic origin every span in this ledger is measured from.
    pub fn origin(&self) -> Option<std::time::Instant> {
        self.inner.lock().ok().map(|inner| inner.origin)
    }

    /// Records one stage span for the frame `owner` is currently building.
    ///
    /// Rejects, and *records the rejection*, when the stage belongs to the
    /// other lane, when a once-per-frame stage has already been recorded for
    /// this frame, or when the span began before this frame did.
    pub fn record(
        &self,
        owner: FrameOwner,
        stage: FrameStage,
        start: std::time::Instant,
        end: std::time::Instant,
    ) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };

        let origin = inner.origin;
        let slot = lane_slot(owner.stage_lane());

        if !FrameStage::all_for(owner).contains(&stage) {
            inner.stats.misattributed = inner.stats.misattributed.saturating_add(1);
            inner.pending[slot].touched = true;
            inner.pending[slot]
                .errors
                .push(StageError::Misattributed { stage, owner });
            return;
        }

        let span = StageSpan {
            start_ns: start.saturating_duration_since(origin).as_nanos() as u64,
            end_ns: end.saturating_duration_since(origin).as_nanos() as u64,
        };

        let frame_start_ns = inner.frame_start_ns[slot];
        let pending = &mut inner.pending[slot];
        pending.touched = true;

        if end < start {
            pending.errors.push(StageError::Inverted(stage));
            return;
        }

        // #2250: a span that began before this frame did belongs to an
        // already-sealed frame. Counting it here is cross-frame attribution --
        // "this frame handled input" when the input was another frame's -- so
        // it is refused and named, never absorbed.
        if span.start_ns < frame_start_ns {
            pending.errors.push(StageError::CrossFrame(stage));
            inner.stats.cross_frame = inner.stats.cross_frame.saturating_add(1);
            return;
        }

        if stage.is_batched() {
            pending.batches[stage.index()].push(span);
            return;
        }

        if pending.spans[stage.index()].is_some() {
            pending.errors.push(StageError::Duplicate(stage));
            let duplicated = inner.stats.duplicated.saturating_add(1);
            inner.stats.duplicated = duplicated;
            return;
        }

        pending.spans[stage.index()] = Some(span);
    }

    /// Binds the stages accumulated by `owner` to `key` and retains the row.
    ///
    /// See [`seal_with_tracks`](Self::seal_with_tracks); the frame samples no
    /// tracks.
    pub fn seal(&self, owner: FrameOwner, key: Option<FrameKey>) -> Option<FrameStages> {
        self.seal_with_tracks(owner, key, &[])
    }

    /// Binds the stages accumulated by `owner` to `key` and `tracks`.
    ///
    /// `key` is `None` when the pre-commit hook could not register an identity
    /// (no protocol, sink at capacity, request refused). Those spans are
    /// *discarded* and counted, because a stage row that cannot be joined to a
    /// frame is not weaker evidence -- it is evidence of a different frame.
    ///
    /// Either way the lane's frame boundary advances: the next frame starts
    /// here (#2250).
    pub fn seal_with_tracks(
        &self,
        owner: FrameOwner,
        key: Option<FrameKey>,
        tracks: &[TrackGeneration],
    ) -> Option<FrameStages> {
        let Ok(mut inner) = self.inner.lock() else {
            return None;
        };

        let origin = inner.origin;
        let slot = lane_slot(owner.stage_lane());
        let pending = std::mem::take(&mut inner.pending[slot]);

        // The frame boundary. Everything measured from now on belongs to the
        // next frame, and anything that claims to predate this instant will be
        // refused as `CrossFrame`.
        inner.frame_start_ns[slot] = std::time::Instant::now()
            .saturating_duration_since(origin)
            .as_nanos() as u64;

        let Some(key) = key else {
            if pending.touched {
                inner.stats.unjoinable = inner.stats.unjoinable.saturating_add(1);
            }
            return None;
        };

        let mut errors = pending.errors;
        let mut nested = Vec::new();

        for stage in FrameStage::required_for(owner) {
            let recorded = if stage.is_batched() {
                !pending.batches[stage.index()].is_empty()
            } else {
                pending.spans[stage.index()].is_some()
            };

            if !recorded {
                errors.push(StageError::Missing(*stage));
            }
        }

        let owned = FrameStage::all_for(owner);
        let mut overlapped = false;
        let mut misnested = false;

        let occurrences = |stage: FrameStage| -> Vec<StageSpan> {
            if stage.is_batched() {
                pending.batches[stage.index()].clone()
            } else {
                pending.spans[stage.index()].into_iter().collect()
            }
        };

        for (position, first) in owned.iter().enumerate() {
            let firsts = occurrences(*first);

            // Two occurrences of the SAME batched stage can never overlap:
            // the event loop does not dispatch two input batches at once.
            for (index, a) in firsts.iter().enumerate() {
                for b in &firsts[index + 1..] {
                    if a.overlaps(*b) {
                        errors.push(StageError::Overlap(*first, *first));
                        overlapped = true;
                    }
                }
            }

            for second in &owned[position + 1..] {
                let seconds = occurrences(*second);

                for a in &firsts {
                    for b in &seconds {
                        if !a.overlaps(*b) {
                            continue;
                        }

                        if first.nests(*second) {
                            if a.contains(*b) {
                                nested.push((*first, *second));
                            } else {
                                errors.push(StageError::NotNested {
                                    outer: *first,
                                    inner: *second,
                                });
                                misnested = true;
                            }
                        } else if second.nests(*first) {
                            if b.contains(*a) {
                                nested.push((*second, *first));
                            } else {
                                errors.push(StageError::NotNested {
                                    outer: *second,
                                    inner: *first,
                                });
                                misnested = true;
                            }
                        } else {
                            errors.push(StageError::Overlap(*first, *second));
                            overlapped = true;
                        }
                    }
                }
            }
        }

        if overlapped {
            inner.stats.overlapping = inner.stats.overlapping.saturating_add(1);
        }

        if misnested {
            inner.stats.unnested = inner.stats.unnested.saturating_add(1);
        }

        nested.sort_unstable();
        nested.dedup();

        let batch_occurrences: u64 = FrameStage::BATCHED
            .iter()
            .map(|stage| pending.batches[stage.index()].len() as u64)
            .sum();
        inner.stats.batch_occurrences = inner
            .stats
            .batch_occurrences
            .saturating_add(batch_occurrences);

        let mut sorted_tracks = tracks.to_vec();
        sorted_tracks.sort_by_key(|track| track.node.0);

        let row = FrameStages {
            key,
            owner,
            spans: pending.spans,
            batches: pending.batches,
            tracks: sorted_tracks.into(),
            nested,
            errors,
        };

        inner.stats.sealed = inner.stats.sealed.saturating_add(1);

        if row.is_usable() {
            inner.stats.usable = inner.stats.usable.saturating_add(1);
        } else if row
            .errors
            .iter()
            .any(|error| matches!(error, StageError::Missing(_)))
        {
            inner.stats.incomplete = inner.stats.incomplete.saturating_add(1);
        }

        let capacity = inner.capacity;

        if inner.sealed.len() == capacity {
            let _ = inner.sealed.pop_front();
            inner.stats.dropped = inner.stats.dropped.saturating_add(1);
        }

        inner.sealed.push_back(row.clone());

        Some(row)
    }

    /// The retained sealed rows, oldest first.
    pub fn frames(&self) -> Vec<FrameStages> {
        self.inner
            .lock()
            .map(|inner| inner.sealed.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Current counters.
    pub fn stats(&self) -> StageStats {
        self.inner
            .lock()
            .map(|inner| inner.stats)
            .unwrap_or_default()
    }

    /// Fail-closed acceptance of this ledger as #2219/#2250 evidence.
    ///
    /// Returns every reason the stage evidence is not acceptable. An empty
    /// vector is the only pass, and a ledger that recorded nothing at all fails
    /// rather than passing vacuously.
    pub fn verdict(&self) -> Vec<String> {
        let stats = self.stats();
        let mut failures = Vec::new();

        if stats.sealed == 0 {
            failures.push(String::from(
                "no frame was sealed: stage evidence is absent, not clean",
            ));
        }

        if stats.usable != stats.sealed {
            failures.push(format!(
                "{} of {} sealed frames are unusable",
                stats.sealed.saturating_sub(stats.usable),
                stats.sealed
            ));
        }

        for (count, label) in [
            (stats.incomplete, "frames missing a required stage"),
            (stats.misattributed, "stages recorded by the wrong lane"),
            (stats.duplicated, "stages recorded twice"),
            (stats.overlapping, "frames with overlapping stage spans"),
            (
                stats.unnested,
                "frames with a stage that straddles its parent",
            ),
            (stats.cross_frame, "stages attributed to the wrong frame"),
            (stats.unjoinable, "frames whose stages could not be joined"),
            (stats.dropped, "sealed rows evicted by the bounded ring"),
        ] {
            if count > 0 {
                failures.push(format!("{count} {label}"));
            }
        }

        failures
    }
}

impl Default for StageLedger {
    fn default() -> Self {
        Self::new(4096)
    }
}

#[cfg(test)]
mod stage_tests {
    use super::{FrameKey, FrameOwner, FrameStage, StageError, StageLedger};
    use std::time::{Duration, Instant};

    fn key(sequence: u64) -> FrameKey {
        FrameKey {
            window_id: 1,
            surface_generation: 0,
            ownership_generation: 0,
            scene_generation: 0,
            submitted_sequence: sequence,
        }
    }

    /// A UI-presented frame carries its surface reclaim and the backend's
    /// four spans (#3660); the render lane does not reclaim.
    #[test]
    fn a_ui_frame_reports_its_reclaim_and_backend_stages() {
        let ledger = StageLedger::new(8);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        for (index, stage) in [
            FrameStage::Update,
            FrameStage::SceneRecord,
            FrameStage::Reclaim,
            FrameStage::Acquire,
            FrameStage::Prepare,
            FrameStage::Submit,
            FrameStage::Present,
        ]
        .into_iter()
        .enumerate()
        {
            let index = index as u64;
            ledger.record(FrameOwner::Ui, stage, at(index * 2 + 1), at(index * 2 + 2));
        }
        let frame = ledger
            .seal(FrameOwner::Ui, Some(key(1)))
            .expect("a sealed row");
        assert!(frame.is_usable(), "{:?}", frame.errors);
        for stage in [
            FrameStage::Reclaim,
            FrameStage::Acquire,
            FrameStage::Prepare,
            FrameStage::Submit,
            FrameStage::Present,
        ] {
            assert_eq!(frame.total_ns(stage), 1_000_000, "{stage:?}");
        }

        ledger.record(FrameOwner::Lane, FrameStage::Reclaim, at(20), at(21));
        let lane = ledger
            .seal(FrameOwner::Lane, Some(key(2)))
            .expect("a sealed row");
        assert!(
            lane.errors.iter().any(|error| matches!(
                error,
                StageError::Misattributed {
                    stage: FrameStage::Reclaim,
                    ..
                }
            )),
            "{:?}",
            lane.errors
        );
    }
}
