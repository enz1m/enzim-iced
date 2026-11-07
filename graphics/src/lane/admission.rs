// Modified by enzUI contributors. See FORK.md.
//! Aggregate backend-lifetime admission; native/GPU byte accounting is a separate contract.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

/// Counts backends being constructed, running, or still inside destruction.
#[derive(Debug)]
pub struct Admission {
    limit: usize,
    occupied: AtomicUsize,
}

impl Admission {
    /// Zero capacity disables backend construction.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            occupied: AtomicUsize::new(0),
        })
    }

    /// Includes quarantined backends until destruction completes normally.
    pub fn occupied(&self) -> usize {
        self.occupied.load(Ordering::Acquire)
    }

    pub(super) fn reserve(self: &Arc<Self>) -> Option<Permit> {
        let _ = self
            .occupied
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.limit).then(|| count + 1)
            })
            .ok()?;
        Some(Permit(Arc::clone(self)))
    }
}

/// Sixteen backend lifetimes shared by compositor factories and direct lane construction.
pub fn global() -> Arc<Admission> {
    static POOL: OnceLock<Arc<Admission>> = OnceLock::new();
    Arc::clone(POOL.get_or_init(|| Admission::new(16)))
}

pub(super) struct Permit(Arc<Admission>);
impl Drop for Permit {
    fn drop(&mut self) {
        // Unwinding cannot attest successful backend destruction; retain admission fail-closed.
        if !std::thread::panicking() {
            let _ = self.0.occupied.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(super) struct OwnedBackend<B> {
    // Field destruction order keeps admission occupied throughout backend destruction.
    pub backend: B,
    pub _permit: Permit,
}
