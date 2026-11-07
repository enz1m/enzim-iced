// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::core::renderer::{NodeId, Scale};
use crate::core::{Size, Vector};
use std::sync::atomic::AtomicUsize;

struct Stub {
    attempts: Arc<AtomicUsize>,
    retirements: Arc<AtomicUsize>,
    successful_first: Option<(mpsc::Sender<Vec<Track>>, mpsc::Receiver<()>)>,
    sampled: Vec<Track>,
}

impl Backend for Stub {
    type Surface = u64;
    fn configure(&mut self, _: &mut u64, _: u32, _: u32) {}
    fn replay(&mut self, _: &Viewport, _: &Recording, tracks: &[Track]) -> bool {
        self.sampled = tracks.to_vec();
        true
    }
    fn present(
        &mut self,
        _: &mut u64,
        _: &Viewport,
        _: Color,
        before: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError> {
        let _ = self.attempts.fetch_add(1, Ordering::Relaxed);
        if let Some((entered, release)) = self.successful_first.take() {
            before();
            let _ = entered.send(self.sampled.clone());
            release
                .recv_timeout(time::Duration::from_secs(2))
                .expect("test owner must release the frame");
            Ok(())
        } else {
            Err(SurfaceError::Occluded)
        }
    }
    fn retire_resources(&mut self) {
        let _ = self.retirements.fetch_add(1, Ordering::SeqCst);
    }
    fn presentation_quality(&self) -> PresentationQuality {
        PresentationQuality::Submitted
    }
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        let _ = self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Observer(mpsc::Sender<time::Instant>);
impl PresentationObserver for Observer {
    fn pre_commit(
        &self,
        submission: PendingSubmission<'_>,
    ) -> Option<crate::presentation::FrameKey> {
        let _ = self.0.send(submission.sampled_at);
        None
    }
    fn needs_track_generations(&self) -> bool {
        false
    }
}

fn viewport() -> Viewport {
    Viewport::with_physical_size(Size::new(320, 200), Scale::default())
}

fn animation(generation: u64, fixed: Option<f32>) -> TrackAnimation {
    TrackAnimation {
        node: NodeId(900),
        generation,
        start: time::Instant::now(),
        duration: time::Duration::from_secs(3600),
        keyframes: vec![
            (
                0.0,
                Track {
                    translation: Vector::new(fixed.unwrap_or(0.0), 0.0),
                    ..Track::identity(NodeId(900))
                },
            ),
            (
                1.0,
                Track {
                    translation: Vector::new(fixed.unwrap_or(100.0), 0.0),
                    ..Track::identity(NodeId(900))
                },
            ),
        ],
    }
}

#[test]
fn occluded_surface_returns_once_instead_of_polling() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut lane = spawn(Stub {
        attempts: attempts.clone(),
        retirements: Arc::new(AtomicUsize::new(0)),
        successful_first: None,
        sampled: vec![],
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        vec![],
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    assert!(lane.start(71, viewport(), Color::BLACK).is_none());
    let stopped = woke.recv_timeout(time::Duration::from_secs(2));
    assert!(stopped.is_ok());
    let (surface, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(surface, 71);
    assert_eq!(report.stop, Stop::Occluded);
    assert_eq!(report.presented, 0);
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert!(!lane.is_poisoned());
}

#[test]
fn scene_reclamation_lane_retire_drops_installed_recording_before_shutdown() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut lane = spawn(Stub {
        attempts,
        retirements: Arc::new(AtomicUsize::new(0)),
        successful_first: None,
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(DropProbe(Arc::clone(&drops))),
        1,
        viewport(),
        Color::BLACK,
        Vec::new(),
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    lane.retire_scene();
    assert!(lane.start(81, viewport(), Color::BLACK).is_none());
    woke.recv_timeout(time::Duration::from_secs(2))
        .expect("retired lane should return its surface");
    let (surface, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(surface, 81);
    assert_eq!(report.stop, Stop::Settled);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn scene_reclamation_blocked_present_releases_recording_before_completion() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let (entered, committed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut lane = spawn(Stub {
        attempts,
        retirements: Arc::new(AtomicUsize::new(0)),
        successful_first: Some((entered, released)),
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(DropProbe(Arc::clone(&drops))),
        1,
        viewport(),
        Color::BLACK,
        Vec::new(),
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    assert!(lane.start(82, viewport(), Color::BLACK).is_none());
    let _ = committed
        .recv_timeout(time::Duration::from_secs(2))
        .expect("backend should enter present");

    lane.retire_scene();
    assert_eq!(drops.load(Ordering::SeqCst), 1);

    let _ = release.send(());
    woke.recv_timeout(time::Duration::from_secs(2))
        .expect("retired lane should stop after late completion");
    let (surface, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(surface, 82);
    assert_eq!(report.stop, Stop::Reclaimed);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn resource_reclamation_late_present_runs_backend_retirement_once() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let retirements = Arc::new(AtomicUsize::new(0));
    let (entered, committed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut lane = spawn(Stub {
        attempts,
        retirements: Arc::clone(&retirements),
        successful_first: Some((entered, released)),
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        Vec::new(),
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    assert!(lane.start(83, viewport(), Color::BLACK).is_none());
    let _ = committed
        .recv_timeout(time::Duration::from_secs(2))
        .expect("backend should enter present");

    lane.retire_scene();
    assert_eq!(retirements.load(Ordering::SeqCst), 0);
    let _ = release.send(());
    woke.recv_timeout(time::Duration::from_secs(2))
        .expect("late present should reach retirement");
    assert_eq!(retirements.load(Ordering::SeqCst), 1);
    let (_, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(report.stop, Stop::Reclaimed);
}

#[test]
fn report_and_stamp_preserve_the_last_successful_sample_not_a_failed_revision() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let (entered, committed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut lane = spawn(Stub {
        attempts: attempts.clone(),
        retirements: Arc::new(AtomicUsize::new(0)),
        successful_first: Some((entered, released)),
        sampled: vec![],
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    let (observed, samples) = mpsc::channel();
    lane.observe_presentation(Arc::new(Observer(observed)));
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        vec![],
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    assert!(lane.start(72, viewport(), Color::BLACK).is_none());
    let first = committed.recv_timeout(time::Duration::from_secs(2));
    lane.animate(vec![], vec![animation(2, Some(5000.0))]);
    let _ = release.send(());
    let stopped = woke.recv_timeout(time::Duration::from_secs(2));
    assert!(stopped.is_ok());
    let (_, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(report.stop, Stop::Occluded);
    assert_eq!(report.presented, 1);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    assert_eq!(report.last_tracks, first.unwrap());
    assert_eq!(
        lane.last_presented_at(),
        Some(samples.recv_timeout(time::Duration::from_secs(2)).unwrap())
    );
}

#[test]
fn lane_stop_publication_precedes_reply_and_cannot_resurrect_after_restart() {
    let counters = Arc::new(Counters::default());
    let worker_counters = counters.clone();
    let (reply, returned) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        publish_stop(&worker_counters, true, || {
            reply.send(()).unwrap();
            resumed.recv_timeout(time::Duration::from_secs(2)).unwrap();
        });
        done.send(()).unwrap();
    });
    let received = returned.recv_timeout(time::Duration::from_secs(2));
    let stopped_at_reply = counters.stopped.load(Ordering::Acquire);
    counters.stopped.store(false, Ordering::Release);
    let _ = resume.send(());
    let finished = completed.recv_timeout(time::Duration::from_secs(2));
    let deadline = time::Instant::now() + time::Duration::from_secs(2);
    while !worker.is_finished() && time::Instant::now() < deadline {
        std::thread::sleep(time::Duration::from_millis(1));
    }
    assert!(worker.is_finished(), "stop publisher did not exit");
    worker.join().unwrap();
    assert!(received.is_ok() && finished.is_ok());
    assert!(
        stopped_at_reply,
        "stop state must be published before surface return"
    );
    assert!(
        !counters.stopped.load(Ordering::Acquire),
        "previous stop must not overwrite the next tenure"
    );
}

#[test]
fn lane_stop_publication_idle_lane_ignores_stale_stop_state() {
    let mut lane = spawn(Stub {
        attempts: Arc::new(AtomicUsize::new(0)),
        retirements: Arc::new(AtomicUsize::new(0)),
        successful_first: None,
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    lane.counters.stopped.store(true, Ordering::Release);
    assert!(
        !lane.has_stopped(),
        "idle stop state must not trigger surface recovery"
    );
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        vec![],
        vec![animation(1, None)],
        None,
        false,
        false,
    );
    assert!(lane.start(71, viewport(), Color::BLACK).is_none());
    woke.recv_timeout(time::Duration::from_secs(2)).unwrap();
    assert!(lane.has_stopped());
    let (surface, report) = lane.reclaim().unwrap();
    assert_eq!(surface, 71);
    assert_eq!(report.stop, Stop::Occluded);
    lane.counters.stopped.store(true, Ordering::Release);
    assert!(!lane.has_stopped());
    assert_eq!(lane.shutdown_checked().retirement, Retirement::Joined);
}

// ---- enzUI-ICED #3084: invisible animation frames are not presented ----

/// A backend that presents every frame after a short FIFO-like block and
/// keeps the tracks of the last presented one.
struct Paced {
    presents: Arc<AtomicUsize>,
    last: Arc<std::sync::Mutex<Vec<Track>>>,
    sampled: Vec<Track>,
}

impl Backend for Paced {
    type Surface = u64;
    fn configure(&mut self, _: &mut u64, _: u32, _: u32) {}
    fn replay(&mut self, _: &Viewport, _: &Recording, tracks: &[Track]) -> bool {
        self.sampled = tracks.to_vec();
        true
    }
    fn present(
        &mut self,
        _: &mut u64,
        _: &Viewport,
        _: Color,
        before: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError> {
        before();
        std::thread::sleep(time::Duration::from_millis(4));
        let _ = self.presents.fetch_add(1, Ordering::Relaxed);
        *self.last.lock().unwrap() = self.sampled.clone();
        Ok(())
    }
    fn retire_resources(&mut self) {}
    fn presentation_quality(&self) -> PresentationQuality {
        PresentationQuality::Submitted
    }
}

fn glide(to: f32, duration_ms: u64) -> TrackAnimation {
    TrackAnimation {
        node: NodeId(900),
        generation: 1,
        start: time::Instant::now(),
        duration: time::Duration::from_millis(duration_ms),
        keyframes: vec![
            (0.0, Track::identity(NodeId(900))),
            (
                1.0,
                Track {
                    translation: Vector::new(to, 0.0),
                    ..Track::identity(NodeId(900))
                },
            ),
        ],
    }
}

/// Run one animation to its settle on a paced backend.
fn run(animation: TrackAnimation) -> (usize, u64, Vec<Track>) {
    let presents = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut lane = spawn(Paced {
        presents: presents.clone(),
        last: last.clone(),
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    let (wake, woke) = mpsc::channel();
    lane.on_stop(move || {
        let _ = wake.send(());
    });
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        vec![],
        vec![animation],
        None,
        false,
        false,
    );
    assert!(lane.start(90, viewport(), Color::BLACK).is_none());
    woke.recv_timeout(time::Duration::from_secs(5))
        .expect("the animation settles");
    let (_surface, report) = lane.reclaim().expect("surface must be returned");
    assert_eq!(report.stop, Stop::Settled);
    let skips = lane.invisible_skips();
    let presented = presents.load(Ordering::Relaxed);
    let last = last.lock().unwrap().clone();
    (presented, skips, last)
}

/// A glide too small to see presents its first frame and its settled frame,
/// and nothing between.
///
/// Sabotage: drop the skip branch — every paced frame is presented.
#[test]
fn an_invisible_glide_presents_only_its_first_and_settled_frames() {
    let (presented, skips, last) = run(glide(0.2, 300));
    assert_eq!(presented, 2, "first and settled frames only");
    assert!(skips > 5, "the frames between were skipped, got {skips}");
    assert_eq!(last[0].translation.x, 0.2, "the settled frame is exact");
}

/// A visible glide is never skipped.
///
/// Sabotage: raise the threshold past a frame's motion — frames vanish.
#[test]
fn a_visible_glide_presents_every_frame() {
    let (presented, skips, last) = run(glide(400.0, 200));
    assert!(presented > 10, "{presented}");
    assert_eq!(skips, 0);
    assert_eq!(last[0].translation.x, 400.0);
}

/// A slow glide presents whenever the drift since the last presented frame
/// becomes visible, so it still moves rather than jumping at the end.
///
/// Sabotage: compare against the previous SAMPLED frame instead of the last
/// presented one — sub-threshold steps never add up and only the ends show.
#[test]
fn a_slow_glide_presents_as_its_drift_becomes_visible() {
    let (presented, skips, last) = run(glide(3.0, 500));
    assert!(skips > 0);
    assert!(
        presented >= 5,
        "the drift presented along the way, got {presented}"
    );
    assert_eq!(last[0].translation.x, 3.0);
}

// ---- enzUI-ICED #3082: motion is sampled at the predicted presentation ----

const VBLANK: time::Duration = time::Duration::from_millis(20);

/// A FIFO-like backend on a fixed vblank grid: `present` releases at the next
/// vblank plus a varying delay, so the lane wakes at a different point of each
/// vblank. Records the translation it presented.
struct Vblank {
    anchor: time::Instant,
    frame: usize,
    presented: mpsc::Sender<f32>,
    sampled: Vec<Track>,
}

impl Backend for Vblank {
    type Surface = u64;
    fn configure(&mut self, _: &mut u64, _: u32, _: u32) {}
    fn replay(&mut self, _: &Viewport, _: &Recording, tracks: &[Track]) -> bool {
        self.sampled = tracks.to_vec();
        true
    }
    fn present(
        &mut self,
        _: &mut u64,
        _: &Viewport,
        _: Color,
        before: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError> {
        before();
        let grid = crate::presentation::PresentationGrid::new(self.anchor, VBLANK).unwrap();
        let jitter = [1, 12, 4, 15, 2, 9][self.frame % 6];
        self.frame += 1;
        let release = grid.next_after(time::Instant::now()) + time::Duration::from_millis(jitter);
        std::thread::sleep(release.saturating_duration_since(time::Instant::now()));
        let _ = self.presented.send(self.sampled[0].translation.x);
        Ok(())
    }
    fn retire_resources(&mut self) {}
    fn presentation_quality(&self) -> PresentationQuality {
        PresentationQuality::Submitted
    }
}

struct Grid(time::Instant);
impl PresentationObserver for Grid {
    fn pre_commit(&self, _: PendingSubmission<'_>) -> Option<crate::presentation::FrameKey> {
        None
    }
    fn needs_track_generations(&self) -> bool {
        false
    }
    fn presentation_grid(&self) -> Option<crate::presentation::PresentationGrid> {
        crate::presentation::PresentationGrid::new(self.0, VBLANK)
    }
}

/// Per-frame displacement of a linear 0.5 px/ms travel on the jittered
/// vblank backend, with or without the compositor's grid.
fn displacements(with_grid: bool) -> (Vec<f32>, u64) {
    let anchor = time::Instant::now();
    let (presented, frames) = mpsc::channel();
    let mut lane = spawn(Vblank {
        anchor,
        frame: 0,
        presented,
        sampled: Vec::new(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    if with_grid {
        lane.observe_presentation(Arc::new(Grid(anchor)));
    }
    lane.commit(
        Recording::new(()),
        1,
        viewport(),
        Color::BLACK,
        vec![],
        vec![glide(1000.0, 2000)],
        None,
        false,
        false,
    );
    assert!(lane.start(90, viewport(), Color::BLACK).is_none());
    let values: Vec<f32> = (0..16)
        .map(|_| {
            frames
                .recv_timeout(time::Duration::from_secs(2))
                .expect("a presented frame")
        })
        .collect();
    let paced = lane.paced_frames();
    let _ = lane.reclaim();
    (
        values.windows(2).map(|pair| pair[1] - pair[0]).collect(),
        paced,
    )
}

/// With the grid, every presented frame advances the travel by one vblank's
/// worth (0.5 px/ms × 20 ms), however late in its vblank the lane woke.
///
/// Sabotage: sample at `wake` in the lane loop — the steps follow the
/// release jitter (between about 3 and 17 px).
#[test]
fn a_linear_travel_advances_by_one_vblank_per_presented_frame() {
    let (steps, paced) = displacements(true);
    assert!(paced >= 15, "frames were paced: {paced}");
    for step in &steps[1..] {
        assert!((step - 10.0).abs() < 0.05, "uneven steps: {steps:?}");
    }
}

/// The control: without a grid the lane samples at wake, and the same
/// backend shows uneven steps. This proves the backend's jitter is real.
#[test]
fn without_a_grid_the_steps_follow_the_wake_jitter() {
    let (steps, paced) = displacements(false);
    assert_eq!(paced, 0);
    let spread = steps.iter().copied().fold(f32::MIN, f32::max)
        - steps.iter().copied().fold(f32::MAX, f32::min);
    assert!(spread > 3.0, "the control backend jitters: {steps:?}");
}

// ---- enzUI-ICED #3660: the lane keeps one frame queued on a grid ----

/// A FIFO swapchain of three images on a 20 ms grid: a present returns at
/// once while an image is free and otherwise blocks until the oldest queued
/// frame is shown, like an acquire on a full swapchain.
struct Swapchain {
    anchor: time::Instant,
    queued: std::collections::VecDeque<time::Instant>,
    blocked: Arc<std::sync::Mutex<time::Duration>>,
}

impl Backend for Swapchain {
    type Surface = u64;
    fn configure(&mut self, _: &mut u64, _: u32, _: u32) {}
    fn replay(&mut self, _: &Viewport, _: &Recording, _: &[Track]) -> bool {
        true
    }
    fn present(
        &mut self,
        _: &mut u64,
        _: &Viewport,
        _: Color,
        before: &mut dyn FnMut(),
    ) -> Result<(), SurfaceError> {
        let grid = crate::presentation::PresentationGrid::new(self.anchor, VBLANK).unwrap();
        let now = time::Instant::now();
        while self.queued.front().is_some_and(|shown| *shown <= now) {
            let _ = self.queued.pop_front();
        }
        if self.queued.len() == 3 {
            let shown = self.queued.pop_front().unwrap();
            let wait = shown.saturating_duration_since(now);
            std::thread::sleep(wait);
            *self.blocked.lock().unwrap() += wait;
        }
        before();
        let next = grid.next_after(time::Instant::now());
        let shown = self
            .queued
            .back()
            .map_or(next, |last| next.max(*last + VBLANK));
        self.queued.push_back(shown);
        Ok(())
    }
    fn retire_resources(&mut self) {}
    fn presentation_quality(&self) -> PresentationQuality {
        PresentationQuality::Submitted
    }
}

/// On a grid the lane never fills the swapchain: no present blocks, and a
/// reclaim mid-travel returns within a fraction of a vblank.
///
/// Sabotage: drop the grid wait at the top of the lane loop -- the lane runs
/// three frames ahead, every present blocks for about a vblank, and a
/// reclaim waits for the blocked present.
#[test]
fn on_a_grid_the_lane_never_blocks_in_present_and_reclaims_at_once() {
    let anchor = time::Instant::now();
    let blocked = Arc::new(std::sync::Mutex::new(time::Duration::ZERO));
    let mut lane = spawn(Swapchain {
        anchor,
        queued: std::collections::VecDeque::new(),
        blocked: blocked.clone(),
    })
    .unwrap_or_else(|_| panic!("backend admission unavailable"));
    lane.observe_presentation(Arc::new(Grid(anchor)));
    let mut slowest = time::Duration::ZERO;
    for _ in 0..4 {
        lane.commit(
            Recording::new(()),
            1,
            viewport(),
            Color::BLACK,
            vec![],
            vec![glide(1000.0, 5000)],
            None,
            false,
            false,
        );
        assert!(lane.start(90, viewport(), Color::BLACK).is_none());
        std::thread::sleep(time::Duration::from_millis(230));
        let asked = time::Instant::now();
        let _ = lane.reclaim().expect("surface must be returned");
        slowest = slowest.max(asked.elapsed());
    }
    let presented = lane.presented();
    let blocked = *blocked.lock().unwrap();
    assert!(presented >= 30, "the lane kept presenting: {presented}");
    assert!(
        blocked < VBLANK,
        "presents blocked for {blocked:?} over {presented} frames"
    );
    assert!(
        slowest < time::Duration::from_millis(4),
        "a reclaim took {slowest:?}"
    );
}

const PERIOD: time::Duration = time::Duration::from_micros(16_667);

/// While the UI thread repaints a travel, a fresh commit is shown at the
/// instant it was laid out for, however late it reaches its vblank, so its
/// layout is not carried rigidly along the curves (enzUI-ICED #3660).
///
/// Sabotage: sample every frame at its vblank -- each commit is moved by the
/// lag between its layout and its vblank.
#[test]
fn a_fresh_commit_is_shown_at_the_instant_it_was_laid_out_for() {
    let start = time::Instant::now();
    let mut clock = CommitClock::handed_over(Some(start), true);
    for frame in 2..10_u32 {
        let vblank = start + PERIOD * frame;
        // Commits reach their vblank one or two periods after their layout.
        let laid_out = vblank - PERIOD * (1 + frame % 2);
        let laid_out = laid_out.max(start + PERIOD * (frame - 1));
        assert_eq!(clock.sample_for(vblank, Some(laid_out)), laid_out);
    }
}

/// A frame with no fresh commit holds the last one however late the next
/// is: a UI thread stalled mid-travel (a page being built) repeats frames
/// rather than having its layout moved rigidly, which shifts everything that
/// reflows. Samples never go back.
///
/// Sabotage: carry a missing commit on along the curves after a while -- a
/// long stall moves the held layout.
#[test]
fn a_late_commit_holds_however_late() {
    let start = time::Instant::now();
    let mut clock = CommitClock::handed_over(Some(start), true);
    let v = |frame: u32| start + PERIOD * frame;
    assert_eq!(clock.sample_for(v(2), Some(v(2))), v(2));
    for frame in 3..40 {
        assert_eq!(
            clock.sample_for(v(frame), None),
            v(2),
            "frame {frame} holds"
        );
    }
    assert_eq!(
        clock.sample_for(v(40), Some(v(1))),
        v(2),
        "a commit older than what is shown does not step back"
    );
    assert_eq!(clock.sample_for(v(41), Some(v(41))), v(41));
}

/// The lane's first frames after taking over a scene the UI thread presented
/// hold that scene until the UI thread's next commit instead of moving it
/// rigidly. Motion the UI thread does not repaint is sampled for the vblank.
///
/// Sabotage: sample the first frame at its vblank -- it moves the presented
/// layout.
#[test]
fn a_hand_over_holds_the_presented_scene() {
    let start = time::Instant::now();
    let mut clock = CommitClock::handed_over(Some(start), true);
    let first = start + PERIOD * (1 + LANE_PRESENT_LEAD);
    assert_eq!(clock.sample_for(first, None), start);

    let mut quiet = CommitClock::handed_over(Some(start), false);
    assert_eq!(quiet.sample_for(first, None), first);
}

/// A travel whose clock moved (the app shell delays a docked travel by a
/// stall) is republished under the same generation with a later start; the
/// lane takes the new timing, which the commit's layout was laid out on. The
/// same travel republished unchanged changes nothing (enzUI-ICED #3660).
///
/// Sabotage: merge by generation alone -- the lane keeps sampling the old
/// timing, and a fresh commit is shown moved by the difference.
#[test]
fn a_re_timed_travel_replaces_the_curve_of_the_same_generation() {
    let first = glide(100.0, 200);
    let mut current = vec![first.clone()];
    merge_animations(&mut current, vec![first.clone()]);
    assert_eq!(current[0].start, first.start, "an unchanged travel stays");

    let delayed = TrackAnimation {
        start: first.start + time::Duration::from_millis(57),
        ..first.clone()
    };
    merge_animations(&mut current, vec![delayed.clone()]);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].start, delayed.start, "the later timing is taken");
}
