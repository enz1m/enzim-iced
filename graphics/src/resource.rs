// Modified by enzUI contributors. See FORK.md.
//! Aggregate admission for renderer-owned font and media snapshots.
//!
//! Admission objects are process policy; [`Permit`] values are the only
//! per-scene ownership and release their count and bytes with the final clone.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock};

/// Maximum copied font faces retained across presentation snapshots.
pub const MAX_RETAINED_FONT_RESOURCES: usize = 1_024;
/// Maximum copied font bytes retained across presentation snapshots.
pub const MAX_RETAINED_FONT_BYTES: usize = 256 * 1024 * 1024;
/// Maximum media sources retained across presentation snapshots.
pub const MAX_RETAINED_MEDIA_RESOURCES: usize = 4_096;
/// Maximum media source bytes retained across presentation snapshots.
pub const MAX_RETAINED_MEDIA_BYTES: usize = 256 * 1024 * 1024;

/// Count and retained-byte usage at one admission boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Retained resource units.
    pub resources: usize,
    /// Retained source bytes.
    pub bytes: usize,
}

/// A count and byte ceiling for one resource class.
#[derive(Debug)]
pub struct Admission {
    limits: Usage,
    usage: Mutex<Usage>,
}

/// The limit that refused a resource reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The count ceiling would be exceeded.
    Count,
    /// The byte ceiling would be exceeded or arithmetic overflowed.
    Bytes,
}

impl Admission {
    /// Creates an independent admission boundary with exact inclusive ceilings.
    pub fn new(resources: usize, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            limits: Usage { resources, bytes },
            usage: Mutex::new(Usage::default()),
        })
    }

    /// Reserves count and bytes atomically until the final returned permit is dropped.
    ///
    /// # Errors
    /// Returns the first count or byte limit that the reservation would exceed.
    pub fn reserve(self: &Arc<Self>, resources: usize, bytes: usize) -> Result<Permit, Refusal> {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_resources = usage
            .resources
            .checked_add(resources)
            .ok_or(Refusal::Count)?;
        let next_bytes = usage.bytes.checked_add(bytes).ok_or(Refusal::Bytes)?;
        if next_resources > self.limits.resources {
            return Err(Refusal::Count);
        }
        if next_bytes > self.limits.bytes {
            return Err(Refusal::Bytes);
        }
        let reserved = Usage { resources, bytes };
        usage.resources = next_resources;
        usage.bytes = next_bytes;
        drop(usage);
        Ok(Permit {
            lease: Arc::new(Lease {
                admission: Arc::clone(self),
                reserved,
            }),
        })
    }

    /// Returns an atomic snapshot of current count and byte usage.
    pub fn usage(&self) -> Usage {
        *self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A cloneable reservation released exactly once with its final clone.
#[derive(Clone)]
pub struct Permit {
    lease: Arc<Lease>,
}

impl Permit {
    /// Returns whether two handles keep the same reservation alive.
    pub fn same_reservation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
    }
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Permit")
            .field("reserved", &self.lease.reserved)
            .finish_non_exhaustive()
    }
}

struct Lease {
    admission: Arc<Admission>,
    reserved: Usage,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut usage = self
            .admission
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.resources = usage
            .resources
            .checked_sub(self.reserved.resources)
            .expect("resource permit count must be released exactly once");
        usage.bytes = usage
            .bytes
            .checked_sub(self.reserved.bytes)
            .expect("resource permit bytes must be released exactly once");
    }
}

/// A one-shot callback whose panic cannot escape its resource owner.
pub struct Callback<T> {
    callback: Box<dyn FnOnce(T) + Send>,
    retained_bytes: usize,
}

impl<T> Callback<T> {
    /// Boxes a callback and records its inline retained size for admission.
    pub fn new(callback: impl FnOnce(T) + Send + 'static) -> Self {
        let retained_bytes =
            std::mem::size_of_val(&callback).saturating_add(std::mem::size_of::<Self>());
        Self {
            callback: Box::new(callback),
            retained_bytes,
        }
    }

    /// Returns bytes charged while the callback remains pending.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Invokes the callback once and reports whether it returned without panic.
    pub fn call(self, value: T) -> bool {
        catch_unwind(AssertUnwindSafe(|| (self.callback)(value))).is_ok()
    }
}

/// Returns the permanent process policy for copied font snapshots.
///
/// The `OnceLock` retains only limits and counters. Snapshot bytes remain owned
/// by permits and are released when their last scene or renderer clone retires.
pub fn font_admission() -> &'static Arc<Admission> {
    static POLICY: OnceLock<Arc<Admission>> = OnceLock::new();
    POLICY.get_or_init(|| Admission::new(MAX_RETAINED_FONT_RESOURCES, MAX_RETAINED_FONT_BYTES))
}

/// Returns the permanent process policy for retained raster and vector sources.
///
/// The `OnceLock` retains only limits and counters. Source ownership remains in
/// permits held by scenes and renderer claims.
pub fn media_admission() -> &'static Arc<Admission> {
    static POLICY: OnceLock<Arc<Admission>> = OnceLock::new();
    POLICY.get_or_init(|| Admission::new(MAX_RETAINED_MEDIA_RESOURCES, MAX_RETAINED_MEDIA_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Owned {
        _probe: DropProbe,
        _permit: Permit,
    }

    #[test]
    fn resource_reclamation_count_and_byte_floors_refuse_without_leaking() {
        let admission = Admission::new(2, 8);
        let first = admission
            .reserve(1, 4)
            .expect("first reservation should fit");
        let second = admission
            .reserve(1, 4)
            .expect("second reservation should fit");

        assert_eq!(
            admission.usage(),
            Usage {
                resources: 2,
                bytes: 8
            }
        );
        assert!(matches!(admission.reserve(1, 0), Err(Refusal::Count)));
        assert!(matches!(admission.reserve(0, 1), Err(Refusal::Bytes)));
        assert_eq!(
            admission.usage(),
            Usage {
                resources: 2,
                bytes: 8
            }
        );

        drop(first);
        drop(second);
        assert_eq!(admission.usage(), Usage::default());
    }

    #[test]
    fn resource_reclamation_final_permit_clone_releases_exactly_once() {
        let admission = Admission::new(1, 8);
        let first = admission.reserve(1, 8).expect("reservation should fit");
        let second = first.clone();

        drop(first);
        assert_eq!(
            admission.usage(),
            Usage {
                resources: 1,
                bytes: 8
            }
        );
        drop(second);
        assert_eq!(admission.usage(), Usage::default());
    }

    #[test]
    fn resource_reclamation_two_windows_retire_independently() {
        let admission = Admission::new(2, 12);
        let drops = Arc::new(AtomicUsize::new(0));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(2);
        let (done_sender, done_receiver) = mpsc::sync_channel(2);
        let mut releases = Vec::new();
        let mut workers = Vec::new();

        for window in 0..2 {
            let admission = Arc::clone(&admission);
            let drops = Arc::clone(&drops);
            let ready = ready_sender.clone();
            let done = done_sender.clone();
            let (release, released) = mpsc::sync_channel(1);
            releases.push(release);
            workers.push(std::thread::spawn(move || {
                let owned = Owned {
                    _permit: admission.reserve(1, 6).expect("window should fit"),
                    _probe: DropProbe(drops),
                };
                let _ = ready.try_send(window);
                let _ = released.recv_timeout(Duration::from_secs(2));
                drop(owned);
                let _ = done.try_send(window);
            }));
        }

        let mut ready = [
            ready_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("first window should start"),
            ready_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("second window should start"),
        ];
        ready.sort_unstable();
        assert_eq!(ready, [0, 1]);
        assert_eq!(
            admission.usage(),
            Usage {
                resources: 2,
                bytes: 12
            }
        );

        releases[0].send(()).expect("first window should retire");
        assert_eq!(done_receiver.recv_timeout(Duration::from_secs(2)), Ok(0));
        assert_eq!(
            admission.usage(),
            Usage {
                resources: 1,
                bytes: 6
            }
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        releases[1].send(()).expect("second window should retire");
        assert_eq!(done_receiver.recv_timeout(Duration::from_secs(2)), Ok(1));
        assert_eq!(admission.usage(), Usage::default());
        assert_eq!(drops.load(Ordering::SeqCst), 2);

        for worker in workers {
            worker.join().expect("acknowledged worker should exit");
        }
    }

    #[test]
    fn resource_reclamation_retired_callback_drops_capture_without_invocation() {
        let admission = Admission::new(1, 8);
        let drops = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let owned = Owned {
            _permit: admission.reserve(1, 8).expect("callback should fit"),
            _probe: DropProbe(Arc::clone(&drops)),
        };
        let observed = Arc::clone(&calls);
        let callback = Callback::new(move |()| {
            drop(owned);
            let _ = observed.fetch_add(1, Ordering::SeqCst);
        });

        drop(callback);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(admission.usage(), Usage::default());
    }

    #[test]
    fn resource_reclamation_callback_panic_releases_and_allows_later_callback() {
        let admission = Admission::new(2, 16);
        let drops = Arc::new(AtomicUsize::new(0));
        let later_calls = Arc::new(AtomicUsize::new(0));
        let first = Owned {
            _permit: admission.reserve(1, 8).expect("first callback should fit"),
            _probe: DropProbe(Arc::clone(&drops)),
        };
        let second = Owned {
            _permit: admission.reserve(1, 8).expect("second callback should fit"),
            _probe: DropProbe(Arc::clone(&drops)),
        };
        let later = Arc::clone(&later_calls);
        let panic = Callback::new(move |owned| {
            drop(owned);
            panic!("injected retained-resource callback panic");
        });
        let succeeds = Callback::new(move |owned| {
            drop(owned);
            let _ = later.fetch_add(1, Ordering::SeqCst);
        });

        assert!(!panic.call(first));
        assert!(succeeds.call(second));
        assert_eq!(later_calls.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(admission.usage(), Usage::default());
    }
}
