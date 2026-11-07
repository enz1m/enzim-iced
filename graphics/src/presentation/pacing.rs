// Modified by enzUI contributors. See FORK.md.
//! Sampling motion for the moment a frame is shown (enzUI-ICED #3082).
//!
//! A frame's motion used to be evaluated at the instant its thread woke. With
//! FIFO and several frames in flight, the gap between that wake and the
//! vblank the frame lands on varies from frame to frame. The curve is smooth,
//! but its samples are unevenly spaced in presentation time, and the result
//! reads as micro-stutter.
//!
//! [`PresentationGrid`] is the vblank grid a surface presents on. The runtime
//! derives it from the last `wp_presentation.presented` timestamp plus the
//! compositor's `refresh`. [`FramePacer`] turns a wake instant into the grid
//! instant the frame is predicted to be presented at. It advances exactly one
//! refresh per presented frame in steady state, so a linear motion moves by a
//! constant step per shown frame.

use std::time::{Duration, Instant};

/// The vblank grid of one surface: a presented instant and the refresh
/// period after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationGrid {
    /// A presentation the compositor reported, in this process's clock.
    pub anchor: Instant,
    /// The compositor-stated refresh period. Never a guessed constant.
    pub period: Duration,
}

impl PresentationGrid {
    /// A grid, or `None` for a zero period (the protocol's "unknown").
    pub fn new(anchor: Instant, period: Duration) -> Option<Self> {
        (!period.is_zero()).then_some(Self { anchor, period })
    }

    /// The first grid instant strictly after `instant`.
    pub fn next_after(&self, instant: Instant) -> Instant {
        let period = self.period.as_nanos();
        let steps = match instant.checked_duration_since(self.anchor) {
            Some(elapsed) => elapsed.as_nanos() / period + 1,
            // An anchor in the future (a clock-conversion rounding edge) is
            // itself the next vblank.
            None => 0,
        };
        self.anchor + nanos(steps.saturating_mul(period))
    }
}

/// How far ahead of the predicted target a paced sample may run before it is
/// pulled back to the grid, in periods.
///
/// Steady state never approaches it. It only bounds the case where frames are
/// presented faster than one per vblank, which would otherwise let the
/// one-period-per-frame rule run arbitrarily far ahead of the screen.
const MAX_LEAD_PERIODS: u32 = 3;

/// How long no frame may have been presented before a grid is adopted by a
/// pacer that was sampling at wake: longer than any frame interval, so the
/// adoption happens between travels, never within one.
pub const ADOPT_AFTER_IDLE: Duration = Duration::from_millis(100);

/// Predicts the presentation instant of each frame one surface presents.
#[derive(Debug, Clone, Default)]
pub struct FramePacer {
    last_target: Option<Instant>,
    /// Whether `last_target` was the grid's prediction. A wake-sampled frame
    /// is not, so a grid that appears mid-travel waits for an idle gap.
    last_on_grid: bool,
    /// Whether the target most recently returned was on the grid, committed
    /// by [`presented`](Self::presented).
    pending_on_grid: bool,
}

impl FramePacer {
    /// A pacer with no presented frame yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The instant to sample motion at for a frame woken at `wake`.
    ///
    /// `lead` is how many vblanks after the first possible one the frame is
    /// expected to land. The lane passes one because it submits a frame ahead
    /// under FIFO.
    ///
    /// The target is the grid instant `lead` periods after the first vblank
    /// following `wake`, and never less than one period after the previous
    /// target. Under FIFO every presented frame occupies its own vblank, so a
    /// wake that jitters early still advances the motion by exactly one
    /// period. A missed vblank shows up as a later wake and advances by two.
    ///
    /// Without a grid (no protocol, no stated refresh, a clock that cannot be
    /// converted), this is `wake`: the old behaviour, never a guessed refresh.
    ///
    /// A grid that becomes known while frames are being sampled at wake (the
    /// first feedback of a session arrives mid-travel) is adopted only after
    /// an idle gap of [`ADOPT_AFTER_IDLE`]. Adopting it during continuous
    /// motion would leap the motion ahead by the whole latency the grid
    /// removes, two frames' worth on a FIFO lane. Measured live, that is a
    /// 100 px jump of a panel slide, and a gradual catch-up only turns it into
    /// several uneven steps. Between travels nothing moves, so the shift is
    /// invisible there.
    ///
    /// The target is not committed. Call [`presented`](Self::presented) once
    /// the frame was actually submitted. A frame that is skipped does not
    /// consume a vblank.
    pub fn target(&mut self, wake: Instant, grid: Option<PresentationGrid>, lead: u32) -> Instant {
        let Some(grid) = grid else {
            self.pending_on_grid = false;
            return wake;
        };
        let predicted = grid.next_after(wake) + grid.period * lead;
        self.pending_on_grid = true;
        let Some(previous) = self.last_target else {
            return predicted;
        };
        if !self.last_on_grid {
            if wake.saturating_duration_since(previous) < ADOPT_AFTER_IDLE {
                self.pending_on_grid = false;
                return wake;
            }
            return predicted;
        }
        let stepped = previous + grid.period;
        if stepped > predicted + grid.period * MAX_LEAD_PERIODS {
            // Frames are presented faster than the grid (a non-FIFO mode, a
            // compositor that releases early). Hold the sample rather than
            // run further ahead, and never step backwards: motion sampled
            // out of order would visibly reverse.
            previous
        } else {
            stepped.max(predicted)
        }
    }

    /// Records that the frame sampled at `target` was submitted.
    pub fn presented(&mut self, target: Instant) {
        self.last_target = Some(target);
        self.last_on_grid = self.pending_on_grid;
    }

    /// The grid instant the last presented frame was sampled for, when it was
    /// on the grid.
    pub fn grid_target(&self) -> Option<Instant> {
        self.last_target.filter(|_| self.last_on_grid)
    }

    /// Forgets the presented history, for example after a surface change.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The presentation period observed in consecutive presentation timestamps
/// (nanoseconds, ascending), for a compositor that states no refresh.
///
/// The lower quartile of the most recent intervals up to 100 ms. Every
/// interval is a whole number of vblanks, so the short ones are the period
/// and the rest are frames the surface did not submit: an app that presents
/// a pair of frames a second, 46 ms apart, on a 144 Hz output has a median
/// of 46 ms. Longer gaps are idle time, not vblanks. Fewer than four
/// intervals is no evidence.
///
/// A virtual output whose timer jitters around its vblanks reports intervals
/// that are not whole multiples of the shortest one (8.0 ms, 4.2 ms, 8.0 ms
/// for a 6.1 ms period). Then the shortest interval is no vblank at all, and
/// the period is the lower quartile of consecutive pairs' means instead.
pub fn measured_period(timestamps_ns: &[u64]) -> Option<Duration> {
    const RECENT: usize = 17;
    const IDLE_NS: u64 = 100_000_000;
    let recent = &timestamps_ns[timestamps_ns.len().saturating_sub(RECENT)..];
    let interval = |a: u64, b: u64| Some(b.saturating_sub(a)).filter(|i| (1..=IDLE_NS).contains(i));
    let mut intervals: Vec<u64> = recent
        .windows(2)
        .filter_map(|pair| interval(pair[0], pair[1]))
        .collect();
    if intervals.len() < 4 {
        return None;
    }
    intervals.sort_unstable();
    let shortest = intervals[intervals.len() / 4];
    let whole = intervals.iter().all(|interval| {
        let ratio = *interval as f64 / shortest as f64;
        (ratio - ratio.round()).abs() < 0.05
    });
    if whole {
        return Some(Duration::from_nanos(shortest));
    }
    let mut pairs: Vec<u64> = recent
        .windows(3)
        .filter_map(|triple| {
            Some((interval(triple[0], triple[1])? + interval(triple[1], triple[2])?) / 2)
        })
        .collect();
    if pairs.len() < 4 {
        return Some(Duration::from_nanos(shortest));
    }
    pairs.sort_unstable();
    Some(Duration::from_nanos(pairs[pairs.len() / 4]))
}

fn nanos(value: u128) -> Duration {
    Duration::from_nanos(u64::try_from(value).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_micros(16_667);

    fn grid(anchor: Instant) -> Option<PresentationGrid> {
        PresentationGrid::new(anchor, PERIOD)
    }

    #[test]
    fn a_zero_refresh_is_no_grid() {
        assert_eq!(PresentationGrid::new(Instant::now(), Duration::ZERO), None);
    }

    #[test]
    fn next_after_is_strictly_later_and_on_the_grid() {
        let anchor = Instant::now();
        let grid = grid(anchor).unwrap();
        assert_eq!(grid.next_after(anchor), anchor + PERIOD);
        assert_eq!(grid.next_after(anchor + PERIOD / 2), anchor + PERIOD);
        assert_eq!(grid.next_after(anchor + PERIOD), anchor + PERIOD * 2);
        assert_eq!(
            grid.next_after(anchor + PERIOD * 7 + PERIOD / 3),
            anchor + PERIOD * 8
        );
    }

    #[test]
    fn without_a_grid_the_wake_is_the_sample() {
        let mut pacer = FramePacer::new();
        let wake = Instant::now();
        assert_eq!(pacer.target(wake, None, 1), wake);
    }

    /// The #3082 defect: wakes jitter within each vblank, so wake-sampled
    /// motion advances by uneven steps. Paced samples advance by exactly one
    /// period per presented frame.
    ///
    /// Sabotage: return `wake` from `target` and the steps follow the jitter.
    #[test]
    fn jittered_wakes_sample_evenly_spaced_instants() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        // Each wake lands somewhere between 5 % and 95 % of its vblank.
        let jitter = [0.05, 0.9, 0.3, 0.95, 0.1, 0.6, 0.4, 0.85, 0.05, 0.7];
        let mut targets = Vec::new();
        for (frame, fraction) in jitter.iter().enumerate() {
            let wake = anchor + PERIOD * frame as u32 + PERIOD.mul_f64(*fraction);
            let target = pacer.target(wake, grid(anchor), 1);
            pacer.presented(target);
            targets.push(target);
        }
        for pair in targets.windows(2) {
            assert_eq!(pair[1] - pair[0], PERIOD, "targets {targets:?}");
        }
        assert_eq!(
            targets[0],
            anchor + PERIOD * 2,
            "one vblank of lead past the next"
        );
    }

    /// An early wake, still inside the vblank the previous frame was aimed
    /// at, still advances one period, because FIFO shows it one vblank later.
    #[test]
    fn a_frame_never_samples_the_previous_frames_vblank() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        let first = pacer.target(anchor + PERIOD / 2, grid(anchor), 0);
        pacer.presented(first);
        let second = pacer.target(anchor + PERIOD / 2 + PERIOD / 10, grid(anchor), 0);
        assert_eq!(second - first, PERIOD);
    }

    /// A missed vblank is real: the frame is shown two periods later, and
    /// its motion must be where it will be then.
    #[test]
    fn a_missed_vblank_advances_two_periods() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        let first = pacer.target(anchor + PERIOD / 4, grid(anchor), 0);
        pacer.presented(first);
        let late = pacer.target(anchor + PERIOD * 2 + PERIOD / 4, grid(anchor), 0);
        assert_eq!(late - first, PERIOD * 2);
    }

    /// A skipped frame (`presented` not called) does not consume a vblank.
    #[test]
    fn a_skipped_frame_is_not_a_step() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        let first = pacer.target(anchor + PERIOD / 4, grid(anchor), 0);
        pacer.presented(first);
        let skipped = pacer.target(anchor + PERIOD + PERIOD / 4, grid(anchor), 0);
        let next = pacer.target(anchor + PERIOD + PERIOD / 2, grid(anchor), 0);
        assert_eq!(skipped, next);
        assert_eq!(next - first, PERIOD);
    }

    fn stamp(seconds: u64, nanoseconds: u32, refresh_ns: u32) -> super::super::PresentationStamp {
        super::super::PresentationStamp {
            clock_id: 1,
            seconds,
            nanoseconds,
            refresh_ns,
            sequence: 0,
            flags_raw: 0,
            output_ids: std::sync::Arc::from(Vec::new()),
        }
    }

    /// A compositor timestamp 5 ms old on its own clock anchors the grid
    /// 5 ms before `now` in this process's clock.
    ///
    /// Sabotage: anchor at `now` and the grid is off by the stamp's age.
    #[test]
    fn a_stamp_anchors_the_grid_by_its_age_on_its_own_clock() {
        let now = Instant::now();
        let grid = stamp(100, 0, 16_666_667)
            .grid(
                now,
                Duration::from_secs(100) + Duration::from_millis(5),
                None,
            )
            .expect("a grid");
        assert_eq!(grid.anchor, now - Duration::from_millis(5));
        assert_eq!(grid.period, Duration::from_nanos(16_666_667));
    }

    #[test]
    fn a_stamp_without_a_period_ahead_of_the_clock_or_stale_and_measured_is_no_grid() {
        let now = Instant::now();
        let measured = Some(Duration::from_nanos(16_180_000));
        assert_eq!(
            stamp(100, 0, 0).grid(now, Duration::from_secs(100), None),
            None
        );
        assert_eq!(
            stamp(100, 0, 16_666_667).grid(now, Duration::from_secs(99), None),
            None
        );
        assert_eq!(
            stamp(100, 0, 0).grid(
                now,
                Duration::from_secs(100) + super::super::GRID_MAX_AGE * 2,
                measured
            ),
            None,
            "a measured period goes stale"
        );
    }

    /// The #3085 finding, measured on a native GNOME session at 144 Hz: a
    /// travel started after more than a second of idle was never paced (0 %
    /// of its frames, against 100 % after 0.5 s). Its first frame found only
    /// a stale stamp, sampled at wake, and a pacer does not adopt a grid
    /// mid-travel. With a stated refresh the old stamp still gives the phase,
    /// so the first frame after any idle is on the grid.
    ///
    /// Sabotage: refuse a stamp older than `GRID_MAX_AGE` whatever its
    /// period, and the idle travel samples at wake throughout.
    #[test]
    fn a_stated_refresh_paces_a_travel_that_starts_after_a_long_idle() {
        let now = Instant::now();
        let idle = Duration::from_secs(90);
        let grid = stamp(100, 0, 6_944_483).grid(now, Duration::from_secs(100) + idle, None);
        let grid = grid.expect("a stated refresh anchors the grid after an idle");
        assert_eq!(grid.anchor, now - idle);

        let mut pacer = FramePacer::new();
        let mut previous = None;
        for frame in 0..8_u32 {
            let wake = now + grid.period * frame + grid.period / 3;
            let target = pacer.target(wake, Some(grid), 1);
            pacer.presented(target);
            assert!(
                target > wake,
                "frame {frame} must be paced, not sampled at wake"
            );
            if let Some(previous) = previous.replace(target) {
                assert_eq!(
                    target - previous,
                    grid.period,
                    "frame {frame} steps one period"
                );
            }
        }
    }

    /// A compositor that states no refresh (wlroots headless, nested
    /// sessions) still paces on the cadence it was observed presenting at.
    /// A stated refresh wins over the measurement.
    #[test]
    fn an_unstated_refresh_falls_back_to_the_measured_cadence() {
        let now = Instant::now();
        let measured = Some(Duration::from_nanos(16_180_000));
        let grid = stamp(100, 0, 0)
            .grid(now, Duration::from_secs(100), measured)
            .expect("a measured grid");
        assert_eq!(grid.period, Duration::from_nanos(16_180_000));
        let stated = stamp(100, 0, 16_666_667)
            .grid(now, Duration::from_secs(100), measured)
            .unwrap();
        assert_eq!(stated.period, Duration::from_nanos(16_666_667));
    }

    /// Idle gaps, the odd missed vblank and frames the surface skipped are
    /// not the period.
    ///
    /// Sabotage: take the median — the skipped frames outvote the vblanks.
    #[test]
    fn the_measured_period_is_the_short_recent_interval() {
        let mut stamps = vec![0_u64];
        for interval in [
            16_180_000,
            16_170_000,
            32_360_000,
            48_540_000,
            900_000_000,
            48_540_000,
            16_180_000,
        ] {
            stamps.push(stamps.last().unwrap() + interval);
        }
        assert_eq!(
            measured_period(&stamps),
            Some(Duration::from_nanos(16_180_000))
        );
        assert_eq!(
            measured_period(&stamps[..4]),
            None,
            "three intervals are not evidence"
        );
        assert_eq!(measured_period(&[]), None);
    }

    /// A headless output's timestamps alternate 8.0 ms and 4.2 ms around a
    /// 6.1 ms period. Taking the short interval paced a UI thread at 240 Hz.
    ///
    /// Sabotage: return the lower-quartile interval unconditionally.
    #[test]
    fn jittered_timestamps_measure_the_mean_of_their_pairs() {
        let mut stamps = vec![0_u64];
        for index in 0..16 {
            let interval = if index % 2 == 0 { 8_000_000 } else { 4_200_000 };
            stamps.push(stamps.last().unwrap() + interval);
        }
        assert_eq!(
            measured_period(&stamps),
            Some(Duration::from_nanos(6_100_000))
        );
    }

    /// Task Manager measured live in a nested 144 Hz session: a short burst at
    /// launch, then two frames 46 ms apart once a second. A 46 ms period made
    /// the sidebar slide step 46 ms of motion per 7 ms frame, then hold one
    /// sample for 13 frames.
    ///
    /// Sabotage: compute the period from the recent window alone and it is
    /// 46 ms.
    #[test]
    fn a_surface_that_presents_now_and_then_keeps_its_fastest_period() {
        use super::super::{FrameKey, FrameOwner, Outcome, PresentationSink, SubmittedFrame};

        let sink = PresentationSink::new(4);
        let mut at_ns = 100_000_000_000_u64;
        let mut sequence = 0;
        let mut present = |at_ns: u64| {
            sequence += 1;
            let key = FrameKey {
                window_id: 1,
                surface_generation: 1,
                ownership_generation: 1,
                scene_generation: 0,
                submitted_sequence: sequence,
            };
            let frame = SubmittedFrame::new(key, Instant::now(), FrameOwner::Ui, Vec::new());
            assert!(sink.register(frame.clone()));
            let mut stamp = stamp(at_ns / 1_000_000_000, (at_ns % 1_000_000_000) as u32, 0);
            stamp.output_ids = std::sync::Arc::from([6]);
            let _ = sink
                .complete_exact(&frame, Outcome::Presented(stamp))
                .unwrap();
        };
        for _ in 0..5 {
            at_ns += 6_944_000;
            present(at_ns);
        }
        for _ in 0..30 {
            at_ns += 1_000_000_000;
            present(at_ns);
            at_ns += 46_000_000;
            present(at_ns);
        }
        let grid = sink
            .grid(Instant::now(), Duration::from_nanos(at_ns))
            .expect("a measured grid");
        assert_eq!(grid.period, Duration::from_nanos(6_944_000));
    }

    /// The live #3082 finding: the first feedback of a session arrived three
    /// frames into a travel, and adopting the grid at once leaped the slide
    /// 100 px ahead in one frame. The travel keeps sampling at wake, and the
    /// grid is adopted by the next travel, after an idle gap.
    ///
    /// Sabotage: adopt `predicted` directly after a wake-sampled frame.
    #[test]
    fn a_grid_that_appears_mid_travel_waits_for_the_next_travel() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        let mut steps = Vec::new();
        let mut previous = None;
        for frame in 0..10_u32 {
            let wake = anchor + PERIOD * frame + PERIOD / 10;
            let grid = if frame < 3 { None } else { grid(anchor) };
            let target = pacer.target(wake, grid, 1);
            pacer.presented(target);
            assert_eq!(target, wake, "frame {frame} stays on its travel's clock");
            if let Some(previous) = previous.replace(target) {
                steps.push(target - previous);
            }
        }
        assert!(steps.iter().all(|step| *step == PERIOD), "{steps:?}");

        let wake = anchor + PERIOD * 9 + PERIOD / 10 + ADOPT_AFTER_IDLE + PERIOD;
        let next_travel = pacer.target(wake, grid(anchor), 1);
        assert_eq!(next_travel, grid(anchor).unwrap().next_after(wake) + PERIOD);
    }

    /// If frames present faster than one per vblank, the pacer neither runs
    /// away into the future nor steps backwards: it holds.
    ///
    /// Sabotage: return `predicted` at the cap and the sample reverses.
    #[test]
    fn the_lead_is_bounded_and_never_reverses() {
        let anchor = Instant::now();
        let mut pacer = FramePacer::new();
        let wake = anchor + PERIOD / 4;
        let mut previous = None;
        for _ in 0..20 {
            let target = pacer.target(wake, grid(anchor), 0);
            pacer.presented(target);
            assert!(
                previous.is_none_or(|previous| target >= previous),
                "stepped backwards"
            );
            previous = Some(target);
            assert!(
                target <= grid(anchor).unwrap().next_after(wake) + PERIOD * MAX_LEAD_PERIODS,
                "ran ahead to {:?}",
                target - anchor
            );
        }
    }
}
