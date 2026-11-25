use std::time::Instant;

/// Paces the frames that program updates alone ask a window for.
///
/// A frame that changes nothing commits nothing, so no frame callback paces
/// the next one. Updates arriving faster than the display refreshes would
/// otherwise rebuild and draw the window once per update. After such a frame,
/// the next update frame waits for the following vblank. Frames requested by
/// input or by anything else are never delayed and never start a delay.
#[derive(Debug, Default)]
pub(crate) struct UpdatePacing {
    pending_update_only: bool,
    drawing_update_only: bool,
    unchanged_until: Option<Instant>,
}

impl UpdatePacing {
    /// Records a full frame request with a cause other than a program update.
    pub fn other_requested(&mut self) {
        self.pending_update_only = false;
    }

    /// Records a full frame request owed to a program update and returns the
    /// vblank the frame must wait for, if any.
    pub fn update_requested(&mut self, frame_pending: bool, now: Instant) -> Option<Instant> {
        if !frame_pending {
            self.pending_update_only = true;
        }

        self.unchanged_until.filter(|until| now < *until)
    }

    /// Records the start of a frame; `full` is false for a content-only frame.
    pub fn begin_frame(&mut self, full: bool) {
        self.drawing_update_only = std::mem::take(&mut self.pending_update_only) && full;
    }

    /// Records whether the frame changed anything on screen.
    pub fn presented(&mut self, unchanged: bool, next_vblank: impl FnOnce() -> Instant) {
        self.unchanged_until = (unchanged && self.drawing_update_only).then(next_vblank);
    }
}

#[cfg(test)]
mod tests {
    use super::UpdatePacing;
    use std::time::{Duration, Instant};

    const PERIOD: Duration = Duration::from_micros(16_667);

    fn draw(pacing: &mut UpdatePacing, unchanged: bool, at: Instant) {
        pacing.begin_frame(true);
        pacing.presented(unchanged, || at + PERIOD);
    }

    #[test]
    fn unchanged_update_frame_delays_the_next_update_to_the_vblank() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        assert_eq!(pacing.update_requested(false, start), None);
        draw(&mut pacing, true, start);

        let soon = start + Duration::from_millis(2);
        assert_eq!(pacing.update_requested(false, soon), Some(start + PERIOD));
        assert_eq!(pacing.update_requested(false, start + PERIOD), None);
    }

    #[test]
    fn changed_update_frame_leaves_pacing_to_frame_callbacks() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        let _ = pacing.update_requested(false, start);
        draw(&mut pacing, false, start);

        assert_eq!(pacing.update_requested(false, start), None);
    }

    #[test]
    fn unchanged_input_frame_does_not_delay_an_update() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        pacing.other_requested();
        draw(&mut pacing, true, start);

        assert_eq!(pacing.update_requested(false, start), None);
    }

    #[test]
    fn update_joining_a_pending_input_frame_does_not_start_a_delay() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        pacing.other_requested();
        let _ = pacing.update_requested(true, start);
        draw(&mut pacing, true, start);

        assert_eq!(pacing.update_requested(false, start), None);
    }

    #[test]
    fn input_after_an_update_request_makes_the_frame_unpaced() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        let _ = pacing.update_requested(false, start);
        pacing.other_requested();
        draw(&mut pacing, true, start);

        assert_eq!(pacing.update_requested(false, start), None);
    }

    #[test]
    fn content_only_frame_does_not_start_a_delay() {
        let start = Instant::now();
        let mut pacing = UpdatePacing::default();

        let _ = pacing.update_requested(false, start);
        pacing.begin_frame(false);
        pacing.presented(true, || start + PERIOD);

        assert_eq!(pacing.update_requested(false, start), None);
    }
}
