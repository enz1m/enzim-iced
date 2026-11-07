// Modified by enzUI contributors. See FORK.md.
//! A `tokio` backend.

/// A `tokio` executor.
pub type Executor = tokio::runtime::Runtime;

/// The most worker threads the executor starts unless `TOKIO_WORKER_THREADS`
/// asks for a number. A desktop application's tasks are mostly waiting on
/// I/O, and every idle worker still costs a thread and its allocator arena,
/// so the default of one per CPU only adds memory on a many-core machine.
pub const MAX_DEFAULT_WORKERS: usize = 4;

/// The worker count for a runtime: `TOKIO_WORKER_THREADS` when set (tokio
/// reads it itself), otherwise the CPU count capped at
/// [`MAX_DEFAULT_WORKERS`].
fn default_workers() -> Option<usize> {
    if std::env::var_os("TOKIO_WORKER_THREADS").is_some() {
        return None;
    }
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    Some(cpus.min(MAX_DEFAULT_WORKERS))
}

impl crate::Executor for Executor {
    fn new() -> Result<Self, futures::io::Error> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        let _ = builder.enable_all();
        if let Some(workers) = default_workers() {
            let _ = builder.worker_threads(workers);
        }
        builder.build()
    }

    #[allow(clippy::let_underscore_future)]
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let _ = tokio::runtime::Runtime::spawn(self, future);
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = tokio::runtime::Runtime::enter(self);
        f()
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.block_on(future)
    }
}

pub mod time {
    //! Listen and react to time.
    use crate::MaybeSend;
    use crate::core::time::{Duration, Instant};
    use crate::subscription::Subscription;

    use futures::stream;

    /// Returns a [`Subscription`] that produces messages at a set interval.
    ///
    /// The first message is produced after a `duration`, and then continues to
    /// produce more messages every `duration` after that.
    pub fn every(duration: Duration) -> Subscription<Instant> {
        Subscription::run_with(duration, |duration| {
            use futures::stream::StreamExt;

            let start = tokio::time::Instant::now() + *duration;

            let mut interval = tokio::time::interval_at(start, *duration);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let stream = {
                futures::stream::unfold(interval, |mut interval| async move {
                    Some((interval.tick().await, interval))
                })
            };

            stream.map(tokio::time::Instant::into_std).boxed()
        })
    }

    /// Returns a [`Subscription`] that runs the given async function at a
    /// set interval; producing the result of the function as output.
    pub fn repeat<F, T>(f: fn() -> F, interval: Duration) -> Subscription<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static,
    {
        Subscription::run_with((f, interval), |(f, interval)| {
            let f = *f;
            let interval = *interval;

            stream::unfold(0, move |i| async move {
                if i > 0 {
                    tokio::time::sleep(interval).await;
                }

                Some((f().await, i + 1))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `TOKIO_WORKER_THREADS`, the executor starts one worker per CPU
    /// up to [`MAX_DEFAULT_WORKERS`], not one per CPU.
    #[test]
    fn the_default_executor_caps_its_workers() {
        if std::env::var_os("TOKIO_WORKER_THREADS").is_some() {
            eprintln!("skipped: TOKIO_WORKER_THREADS is set");
            return;
        }
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        // Through the trait, as iced builds it: `Runtime` has an inherent
        // `new` of its own.
        let executor = <Executor as crate::Executor>::new().expect("runtime");
        assert_eq!(
            executor.metrics().num_workers(),
            cpus.min(MAX_DEFAULT_WORKERS)
        );
    }
}
