// Modified by enzUI contributors. See FORK.md.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default)]
pub(super) struct SampleStamp(AtomicU64);

impl SampleStamp {
    pub(super) fn publish(&self, elapsed: Duration) {
        let value = u64::try_from(elapsed.as_nanos())
            .ok()
            .and_then(|value| value.checked_add(1));
        self.0.store(value.unwrap_or(0), Ordering::Release);
    }

    pub(super) fn elapsed(&self) -> Option<Duration> {
        self.0
            .load(Ordering::Acquire)
            .checked_sub(1)
            .map(Duration::from_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_unpublished_zero_and_unrepresentable_samples() {
        let stamp = SampleStamp::default();
        assert_eq!(stamp.elapsed(), None);
        stamp.publish(Duration::ZERO);
        assert_eq!(stamp.elapsed(), Some(Duration::ZERO));
        stamp.publish(Duration::from_nanos(12_345));
        assert_eq!(stamp.elapsed(), Some(Duration::from_nanos(12_345)));
        stamp.publish(Duration::MAX);
        assert_eq!(stamp.elapsed(), None);
    }

    #[test]
    fn concurrent_reader_sees_monotonic_complete_samples() {
        let stamp = SampleStamp::default();
        std::thread::scope(|scope| {
            let _ = scope.spawn(|| {
                for sample in 0..100_000 {
                    stamp.publish(Duration::from_nanos(sample));
                }
            });
            let mut last = Duration::ZERO;
            for _ in 0..100_000 {
                if let Some(sample) = stamp.elapsed() {
                    assert!(sample >= last);
                    last = sample;
                }
            }
        });
        assert_eq!(stamp.elapsed(), Some(Duration::from_nanos(99_999)));
    }
}
