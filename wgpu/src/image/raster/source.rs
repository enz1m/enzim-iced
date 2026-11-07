// Modified by enzUI contributors. See FORK.md.
use crate::core::{Bytes, image};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub(crate) const MAX_SOURCE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct Budget {
    used: Arc<AtomicUsize>,
    enabled: Arc<AtomicBool>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }
}
impl Budget {
    pub(crate) fn bytes(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
    pub(crate) fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }
    pub(crate) fn capture(&self, width: u32, height: u32, pixels: &[u8]) -> Option<Arc<Source>> {
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        let bytes = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if width == 0
            || height == 0
            || width > 16_384
            || height > 16_384
            || bytes != pixels.len()
            || bytes > MAX_SOURCE_BYTES
        {
            return None;
        }
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= MAX_SOURCE_BYTES)
            })
            .ok()?;
        let lease = Lease {
            budget: self.clone(),
            bytes,
        };
        let mut data = Vec::new();
        data.try_reserve_exact(bytes).ok()?;
        data.extend_from_slice(pixels);
        let pixels = Bytes::from_owner(LeasedPixels {
            data,
            _lease: lease,
        });
        Some(Arc::new(Source {
            handle: image::Handle::from_rgba(width, height, pixels),
            bytes,
        }))
    }
}

#[derive(Debug)]
struct Lease {
    budget: Budget,
    bytes: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct LeasedPixels {
    data: Vec<u8>,
    _lease: Lease,
}
impl AsRef<[u8]> for LeasedPixels {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Debug)]
pub(crate) struct Source {
    pub(crate) handle: image::Handle,
    bytes: usize,
}
impl Source {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn size(&self) -> crate::core::Size<u32> {
        match &self.handle {
            image::Handle::Rgba { width, height, .. } => crate::core::Size::new(*width, *height),
            _ => unreachable!(),
        }
    }
}
