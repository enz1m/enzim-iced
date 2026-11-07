// Modified by enzUI contributors. See FORK.md.
//! Kinetic touchpad scrolling (enzICED).
//!
//! A touchpad reports pixel scroll deltas while fingers move and an axis
//! stop when they lift. The scroll then continues with the speed it had,
//! slowing exponentially, as GTK and Qt do: the runtime feeds the window
//! synthetic pixel scrolls on later frames until the speed is negligible, the
//! user touches the touchpad again, or any other input arrives.

use crate::core::Vector;
use crate::core::time::{Duration, Instant};

use std::collections::VecDeque;

/// How long the motion takes to lose about two thirds of its speed.
const TIME_CONSTANT: f32 = 0.325;
/// Below this speed, in logical pixels per second, the motion stops.
const STOP_SPEED: f32 = 20.0;
/// A release slower than this does not start motion at all.
const FLING_SPEED: f32 = 100.0;
/// Only the scrolling of this last stretch decides the speed.
const SAMPLE_WINDOW: Duration = Duration::from_millis(100);
/// The fastest motion, so a stray huge delta cannot throw the view away.
const MAX_SPEED: f32 = 8000.0;

/// The momentum of one window's touchpad scrolling.
#[derive(Debug, Default)]
pub(crate) struct Kinetic {
    samples: VecDeque<(Instant, Vector)>,
    velocity: Option<Vector>,
    last_tick: Option<Instant>,
}

impl Kinetic {
    /// A finger scroll of `delta` logical pixels at `now`.
    pub(crate) fn scroll(&mut self, delta: Vector, now: Instant) {
        self.velocity = None;
        self.samples.push_back((now, delta));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > SAMPLE_WINDOW)
        {
            let _ = self.samples.pop_front();
        }
    }

    /// The fingers lifted at `now`; returns whether motion starts.
    pub(crate) fn release(&mut self, now: Instant) -> bool {
        let samples = std::mem::take(&mut self.samples);
        let recent: Vec<_> = samples
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) <= SAMPLE_WINDOW)
            .collect();
        let (Some(first), Some(last)) = (recent.first(), recent.last()) else {
            return false;
        };
        let span = last.0.saturating_duration_since(first.0).as_secs_f32();
        if recent.len() < 2 || span <= 0.0 {
            return false;
        }
        // The first sample's delta happened before the span began.
        let distance = recent
            .iter()
            .skip(1)
            .fold(Vector::ZERO, |sum, (_, delta)| sum + *delta);
        let mut velocity = distance * (1.0 / span);
        let speed = velocity.x.hypot(velocity.y);
        if speed < FLING_SPEED {
            return false;
        }
        if speed > MAX_SPEED {
            velocity *= MAX_SPEED / speed;
        }
        self.velocity = Some(velocity);
        self.last_tick = Some(now);
        true
    }

    /// Ends any motion: the user touched or pressed something.
    pub(crate) fn stop(&mut self) {
        self.samples.clear();
        self.velocity = None;
        self.last_tick = None;
    }

    pub(crate) fn is_moving(&self) -> bool {
        self.velocity.is_some()
    }

    /// The distance to scroll for the time since the last tick, or `None`
    /// when not moving. The motion stops by itself once it is slow enough.
    pub(crate) fn tick(&mut self, now: Instant) -> Option<Vector> {
        let velocity = self.velocity?;
        let last = self.last_tick.replace(now).unwrap_or(now);
        let dt = now.saturating_duration_since(last).as_secs_f32();
        let decay = (-dt / TIME_CONSTANT).exp();
        // The exact distance an exponentially slowing motion covers in `dt`.
        let distance = velocity * (TIME_CONSTANT * (1.0 - decay));
        let velocity = velocity * decay;
        if velocity.x.hypot(velocity.y) < STOP_SPEED {
            self.velocity = None;
            self.last_tick = None;
        } else {
            self.velocity = Some(velocity);
        }
        Some(distance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fling(kinetic: &mut Kinetic, start: Instant, per_frame: f32) -> Instant {
        let mut now = start;
        for _ in 0..6 {
            kinetic.scroll(Vector::new(0.0, per_frame), now);
            now += Duration::from_millis(16);
        }
        now - Duration::from_millis(16)
    }

    #[test]
    fn a_fast_release_keeps_scrolling_and_slows_down() {
        let mut kinetic = Kinetic::default();
        let start = Instant::now();
        let released = fling(&mut kinetic, start, 20.0);
        assert!(kinetic.release(released), "20 px per 16 ms is a fling");

        let mut now = released;
        let mut steps = Vec::new();
        while let Some(step) = kinetic.tick(now + Duration::from_millis(16)) {
            now += Duration::from_millis(16);
            steps.push(step.y);
            assert!(steps.len() < 1000, "the motion ends");
        }
        assert!(
            steps[1] > 15.0,
            "it continues near the release speed: {steps:?}"
        );
        assert!(
            steps.windows(2).all(|pair| pair[1] < pair[0]),
            "it slows down"
        );
        let total: f32 = steps.iter().sum();
        // v * tau, where v = 20 px / 16 ms = 1250 px/s.
        assert!(
            (total - 1250.0 * TIME_CONSTANT).abs() < 25.0,
            "covered {total}"
        );
    }

    #[test]
    fn a_slow_release_or_a_new_touch_does_not_move() {
        let mut kinetic = Kinetic::default();
        let start = Instant::now();
        let released = fling(&mut kinetic, start, 1.0);
        assert!(
            !kinetic.release(released),
            "1 px per frame is a careful scroll"
        );

        let released = fling(&mut kinetic, start, 20.0);
        assert!(kinetic.release(released));
        kinetic.stop();
        assert_eq!(kinetic.tick(released + Duration::from_millis(16)), None);

        let released = fling(&mut kinetic, start, 20.0);
        assert!(kinetic.release(released));
        kinetic.scroll(Vector::new(0.0, 1.0), released + Duration::from_millis(5));
        assert!(!kinetic.is_moving(), "touching again stops the motion");
    }
}
