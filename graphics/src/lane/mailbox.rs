// Modified by enzUI contributors. See FORK.md.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, TryLockError, mpsc};

pub(super) const CAPACITY: usize = 8;

pub(super) trait Message {
    fn barrier(&self) -> bool;
    fn supersedes(&self, previous: &Self) -> bool;
    fn revision(&self) -> Option<u64> {
        None
    }
}

/// Bounded publication traffic for one lane, including nonblocking-reader contention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Statistics {
    /// Pending messages, never greater than the mailbox capacity.
    pub queued: u64,
    /// Largest observed pending population.
    pub high_water: u64,
    /// Obsolete updates removed before the worker consumed them.
    pub superseded: u64,
    /// Scene revisions rejected because a newer revision was already accepted.
    pub stale: u64,
    /// Sends refused because ownership barriers filled the bounded queue.
    pub full: u64,
    /// Active receives that skipped a publisher-held lock instead of waiting.
    pub contended: u64,
}

#[derive(Default)]
struct Counters {
    queued: AtomicU64,
    high_water: AtomicU64,
    superseded: AtomicU64,
    stale: AtomicU64,
    full: AtomicU64,
    contended: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> Statistics {
        Statistics {
            queued: self.queued.load(Ordering::Relaxed),
            high_water: self.high_water.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            stale: self.stale.load(Ordering::Relaxed),
            full: self.full.load(Ordering::Relaxed),
            contended: self.contended.load(Ordering::Relaxed),
        }
    }
}

struct State<T> {
    queue: VecDeque<T>,
    revision: Option<u64>,
    closed: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
    counters: Counters,
}

pub(super) struct Sender<T>(Arc<Shared<T>>);
pub(super) struct Receiver<T>(Arc<Shared<T>>);

pub(super) fn channel<T: Message>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::with_capacity(CAPACITY),
            revision: None,
            closed: false,
        }),
        ready: Condvar::new(),
        counters: Counters::default(),
    });
    (Sender(shared.clone()), Receiver(shared))
}

impl<T: Message> Sender<T> {
    pub(super) fn send(&self, message: T) -> Result<(), mpsc::SendError<T>> {
        let Ok(mut state) = self.0.state.lock() else {
            return Err(mpsc::SendError(message));
        };
        if state.closed {
            return Err(mpsc::SendError(message));
        }
        if message
            .revision()
            .zip(state.revision)
            .is_some_and(|(next, current)| next < current)
        {
            let _ = self.0.counters.stale.fetch_add(1, Ordering::Relaxed);
            drop(state);
            drop(message);
            return Ok(());
        }
        let mut retired = Vec::new();
        if !message.barrier() {
            let start = state
                .queue
                .iter()
                .rposition(Message::barrier)
                .map_or(0, |index| index + 1);
            for index in (start..state.queue.len()).rev() {
                if message.supersedes(&state.queue[index]) {
                    retired.push(state.queue.remove(index).expect("in-bounds pending update"));
                }
            }
        }
        if state.queue.len() == CAPACITY {
            let _ = self.0.counters.full.fetch_add(1, Ordering::Relaxed);
            drop(state);
            drop(retired);
            return Err(mpsc::SendError(message));
        }
        if let Some(revision) = message.revision() {
            state.revision = Some(revision);
        }
        state.queue.push_back(message);
        let count = state.queue.len() as u64;
        self.0.counters.queued.store(count, Ordering::Relaxed);
        let _ = self
            .0
            .counters
            .high_water
            .fetch_max(count, Ordering::Relaxed);
        let _ = self
            .0
            .counters
            .superseded
            .fetch_add(retired.len() as u64, Ordering::Relaxed);
        drop(state);
        self.0.ready.notify_one();
        // Releasing a scene can destroy renderer resources; never do that under the publication lock.
        drop(retired);
        Ok(())
    }

    pub(super) fn statistics(&self) -> Statistics {
        self.0.counters.snapshot()
    }
}

impl<T> Sender<T> {
    pub(super) fn close(&self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.closed = true;
        }
        self.0.ready.notify_all();
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T> Receiver<T> {
    pub(super) fn recv(&self) -> Result<T, mpsc::RecvError> {
        let mut state = self.0.state.lock().map_err(|_| mpsc::RecvError)?;
        loop {
            if let Some(value) = state.queue.pop_front() {
                self.0
                    .counters
                    .queued
                    .store(state.queue.len() as u64, Ordering::Relaxed);
                return Ok(value);
            }
            if state.closed {
                return Err(mpsc::RecvError);
            }
            state = self.0.ready.wait(state).map_err(|_| mpsc::RecvError)?;
        }
    }

    /// Waits until a message is pending, the channel closes, or `deadline`
    /// passes, without taking the message. True when the caller should drain.
    pub(super) fn wait_until(&self, deadline: std::time::Instant) -> bool {
        let Ok(mut state) = self.0.state.lock() else {
            return true;
        };
        loop {
            if !state.queue.is_empty() || state.closed {
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = match self.0.ready.wait_timeout(state, remaining) {
                Ok((state, _)) => state,
                Err(_) => return true,
            };
        }
    }

    pub(super) fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let mut state = match self.0.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                let _ = self.0.counters.contended.fetch_add(1, Ordering::Relaxed);
                return Err(mpsc::TryRecvError::Empty);
            }
            Err(TryLockError::Poisoned(_)) => return Err(mpsc::TryRecvError::Disconnected),
        };
        if let Some(value) = state.queue.pop_front() {
            self.0
                .counters
                .queued
                .store(state.queue.len() as u64, Ordering::Relaxed);
            Ok(value)
        } else if state.closed {
            Err(mpsc::TryRecvError::Disconnected)
        } else {
            Err(mpsc::TryRecvError::Empty)
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.closed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Debug)]
    enum Update {
        Scene(u64, Option<Arc<AtomicUsize>>),
        Animate(u64),
        Configure(u64),
        Barrier(u64),
    }

    impl Drop for Update {
        fn drop(&mut self) {
            if let Self::Scene(_, Some(count)) = self {
                let _ = count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    impl Message for Update {
        fn barrier(&self) -> bool {
            matches!(self, Self::Barrier(_))
        }
        fn supersedes(&self, old: &Self) -> bool {
            matches!(
                (self, old),
                (Self::Scene(..), Self::Scene(..) | Self::Animate(_))
                    | (Self::Animate(_), Self::Animate(_))
                    | (Self::Configure(_), Self::Configure(_))
            )
        }
        fn revision(&self) -> Option<u64> {
            if let Self::Scene(revision, _) = self {
                Some(*revision)
            } else {
                None
            }
        }
    }

    #[test]
    fn scene_flood_releases_superseded_resources_and_keeps_latest() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = channel();
        for revision in 1..=10_000 {
            sender
                .send(Update::Scene(revision, Some(drops.clone())))
                .unwrap();
            sender.send(Update::Animate(revision)).unwrap();
        }
        assert_eq!(sender.statistics().queued, 2);
        assert_eq!(sender.statistics().high_water, 2);
        assert_eq!(drops.load(Ordering::Relaxed), 9_999);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Update::Scene(10_000, _)
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Update::Animate(10_000)
        ));
        assert_eq!(drops.load(Ordering::Relaxed), 10_000);
    }

    #[test]
    fn ownership_barriers_prevent_cross_epoch_coalescing() {
        let (sender, receiver) = channel();
        sender.send(Update::Scene(1, None)).unwrap();
        sender.send(Update::Barrier(1)).unwrap();
        sender.send(Update::Configure(1)).unwrap();
        sender.send(Update::Scene(2, None)).unwrap();
        sender.send(Update::Configure(2)).unwrap();
        sender.send(Update::Barrier(2)).unwrap();
        assert!(matches!(receiver.recv().unwrap(), Update::Scene(1, _)));
        assert!(matches!(receiver.recv().unwrap(), Update::Barrier(1)));
        assert!(matches!(receiver.recv().unwrap(), Update::Scene(2, _)));
        assert!(matches!(receiver.recv().unwrap(), Update::Configure(2)));
        assert!(matches!(receiver.recv().unwrap(), Update::Barrier(2)));
    }

    #[test]
    fn stale_scene_cannot_replace_a_newer_revision() {
        let (sender, receiver) = channel();
        sender.send(Update::Scene(7, None)).unwrap();
        sender.send(Update::Scene(6, None)).unwrap();
        assert_eq!(sender.statistics().stale, 1);
        assert!(matches!(receiver.recv().unwrap(), Update::Scene(7, _)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn full_control_queue_refuses_instead_of_growing_or_losing_ownership() {
        let (sender, receiver) = channel();
        for id in 0..CAPACITY {
            sender.send(Update::Barrier(id as u64)).unwrap();
        }
        assert!(matches!(
            sender.send(Update::Barrier(99)),
            Err(mpsc::SendError(Update::Barrier(99)))
        ));
        assert_eq!(sender.statistics().full, 1);
        assert_eq!(sender.statistics().high_water, CAPACITY as u64);
        sender.close();
        for id in 0..CAPACITY {
            assert!(
                matches!(receiver.recv().unwrap(), Update::Barrier(value) if value == id as u64)
            );
        }
        assert!(receiver.recv().is_err());
    }

    #[test]
    fn active_reader_does_not_wait_for_a_suspended_publisher() {
        let (sender, receiver) = channel::<Update>();
        let held = sender.0.state.lock().unwrap();
        let (done, completion) = mpsc::channel();
        std::thread::scope(|scope| {
            let _ = scope.spawn(|| {
                done.send(receiver.try_recv()).unwrap();
            });
            let result = completion.recv_timeout(Duration::from_millis(100));
            drop(held);
            assert!(matches!(result.unwrap(), Err(mpsc::TryRecvError::Empty)));
        });
        assert_eq!(sender.statistics().contended, 1);
    }

    #[test]
    fn replacement_is_visible_while_superseded_resource_destruction_is_blocked() {
        #[derive(Debug)]
        struct Payload {
            revision: u64,
            hold: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
        }
        impl Message for Payload {
            fn barrier(&self) -> bool {
                false
            }
            fn supersedes(&self, _: &Self) -> bool {
                true
            }
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                if let Some((entered, release)) = self.hold.take() {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
            }
        }
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (sender, receiver) = channel();
        sender
            .send(Payload {
                revision: 1,
                hold: Some((entered, released)),
            })
            .unwrap();
        std::thread::scope(|scope| {
            let _ = scope.spawn(|| {
                sender
                    .send(Payload {
                        revision: 2,
                        hold: None,
                    })
                    .unwrap();
            });
            let retiring = observed.recv_timeout(Duration::from_secs(2));
            let replacement = receiver.try_recv();
            let _ = release.send(());
            assert!(retiring.is_ok());
            assert_eq!(replacement.unwrap().revision, 2);
        });
    }

    #[test]
    fn idle_reader_wakes_when_publisher_is_dropped() {
        let (sender, receiver) = channel::<Update>();
        let thread = std::thread::spawn(move || receiver.recv());
        drop(sender);
        assert!(thread.join().unwrap().is_err());
    }
}
