// Modified by enzUI contributors. See FORK.md.
pub(crate) mod source;
use crate::core::Size;
use crate::core::image;
use crate::graphics;
use crate::image::atlas::{self, Atlas};
use crate::image::cache::{ImageCachePolicy, ImageCacheStatistics};

use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::{Arc, Weak};

pub type Image = graphics::image::Buffer;

/// Entry in cache corresponding to an image handle
#[derive(Debug)]
pub enum Memory {
    /// Image data on host
    Host(Image),
    /// Storage entry
    Device {
        entry: atlas::Entry,
        bind_group: Option<Arc<wgpu::BindGroup>>,
        allocation: Option<Weak<image::Memory>>,
    },
    Error(image::Error),
}

impl Memory {
    pub fn load(handle: &image::Handle) -> Self {
        match graphics::image::load(handle) {
            Ok(image) => Self::Host(image),
            Err(error) => Self::Error(error),
        }
    }

    pub fn dimensions(&self) -> Size<u32> {
        match self {
            Memory::Host(image) => {
                let (width, height) = image.dimensions();

                Size::new(width, height)
            }
            Memory::Device { entry, .. } => entry.size(),
            Memory::Error(_) => Size::new(1, 1),
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            Memory::Host(image) => image.len() as u64,
            Memory::Device { entry, .. } => {
                let size = entry.size();
                u64::from(size.width) * u64::from(size.height) * 4
            }
            Memory::Error(_) => 0,
        }
    }

    fn has_active_allocation(&self) -> bool {
        matches!(
            self,
            Memory::Device { allocation: Some(allocation), .. }
                if allocation.strong_count() > 0
        )
    }
}

#[derive(Debug, Default)]
pub struct Cache {
    sources: FxHashMap<image::Id, Arc<source::Source>>,
    /// The handle each captured source was frozen from.
    /// A frozen handle has the same pixels as its original, so a retained
    /// scene replayed with it reuses the original's upload instead of
    /// decoding and uploading the image again.
    aliases: FxHashMap<image::Id, image::Id>,
    budget: source::Budget,
    map: FxHashMap<image::Id, Memory>,
    last_used: FxHashMap<image::Id, u64>,
    frame: u64,
    uploads: u64,
    evictions: u64,
}

impl Cache {
    pub(crate) fn source_budget(&self) -> source::Budget {
        self.budget.clone()
    }
    pub(crate) fn source(&self, handle: &image::Handle) -> Option<Arc<source::Source>> {
        self.sources.get(&handle.id()).cloned()
    }
    pub(crate) fn insert_source(
        &mut self,
        handle: &image::Handle,
        source: Option<Arc<source::Source>>,
    ) {
        if let Some(source) = source {
            let _ = self.sources.insert(handle.id(), source);
        } else {
            let _ = self.sources.remove(&handle.id());
        }
    }
    /// The entry `id` reads: its own, else the original it was frozen from.
    fn resolve(&self, id: image::Id) -> image::Id {
        match self.aliases.get(&id) {
            Some(original) if !self.map.contains_key(&id) && self.map.contains_key(original) => {
                *original
            }
            _ => id,
        }
    }

    pub fn mark_used(&mut self, handle: &image::Handle) {
        let id = self.resolve(handle.id());
        if self.map.contains_key(&id) {
            let _ = self.last_used.insert(id, self.frame);
        }
    }

    pub fn get_mut(&mut self, handle: &image::Handle) -> Option<&mut Memory> {
        let id = self.resolve(handle.id());
        let _ = self.last_used.insert(id, self.frame);

        self.map.get_mut(&id)
    }

    pub fn insert(&mut self, handle: &image::Handle, memory: Memory) {
        match &memory {
            Memory::Host(image) => {
                let _ = self.sources.remove(&handle.id());
                let source = self.budget.capture(image.width(), image.height(), image);
                if let Some(source) = &source {
                    let _ = self.aliases.insert(source.handle.id(), handle.id());
                }
                self.insert_source(handle, source);
            }
            Memory::Error(_) => {
                let _ = self.sources.remove(&handle.id());
            }
            Memory::Device { .. } => {}
        }
        if matches!(memory, Memory::Device { .. })
            && !matches!(self.map.get(&handle.id()), Some(Memory::Device { .. }))
        {
            self.uploads = self.uploads.saturating_add(1);
        }

        let _ = self.map.insert(handle.id(), memory);
        let _ = self.last_used.insert(handle.id(), self.frame);
    }

    pub fn contains(&self, handle: &image::Handle) -> bool {
        self.map.contains_key(&self.resolve(handle.id()))
    }

    pub fn trim(
        &mut self,
        frame: u64,
        policy: ImageCachePolicy,
        atlas: &mut Atlas,
        on_drop: impl Fn(Arc<wgpu::BindGroup>),
    ) {
        self.frame = frame;

        let mut removals = FxHashSet::default();
        let mut stale = self
            .map
            .iter()
            .filter_map(|(id, memory)| {
                let age = frame.saturating_sub(*self.last_used.get(id).unwrap_or(&frame));

                if age == 0 || memory.has_active_allocation() {
                    return None;
                }

                if age > policy.grace_frames {
                    let _ = removals.insert(*id);
                }

                Some((*id, age, memory.bytes()))
            })
            .collect::<Vec<_>>();

        let mut retained_stale_bytes = stale
            .iter()
            .filter(|(id, _, _)| !removals.contains(id))
            .map(|(_, _, bytes)| *bytes)
            .sum::<u64>();

        if retained_stale_bytes > policy.stale_byte_budget {
            stale.sort_unstable_by_key(|(_, age, _)| std::cmp::Reverse(*age));

            for (id, _, bytes) in stale {
                if retained_stale_bytes <= policy.stale_byte_budget {
                    break;
                }

                if removals.insert(id) {
                    retained_stale_bytes = retained_stale_bytes.saturating_sub(bytes);
                }
            }
        }

        if !removals.is_empty() {
            self.aliases.retain(|frozen, original| {
                !removals.contains(frozen) && !removals.contains(original)
            });
        }

        for id in removals {
            let Some(mut memory) = self.map.remove(&id) else {
                continue;
            };

            let _ = self.last_used.remove(&id);
            let _ = self.sources.remove(&id);
            self.evictions = self.evictions.saturating_add(1);
            log::debug!("Dropping image allocation: {id:?}");

            if let Memory::Device {
                entry, bind_group, ..
            } = &mut memory
            {
                if let Some(bind_group) = bind_group.take() {
                    on_drop(bind_group);
                } else {
                    atlas.remove(entry);
                }
            }
        }

        self.frame = frame.saturating_add(1);
    }

    pub fn add_statistics(&self, statistics: &mut ImageCacheStatistics) {
        statistics.raster_source_bytes = self.budget.bytes();
        statistics.raster_entries = self.map.len();
        statistics.raster_bytes = self.map.values().map(Memory::bytes).sum();
        statistics.raster_stale_entries = self
            .last_used
            .values()
            .filter(|last_used| last_used.saturating_add(1) < self.frame)
            .count();
        statistics.raster_stale_bytes = self
            .map
            .iter()
            .filter(|(id, _)| {
                self.last_used
                    .get(id)
                    .is_some_and(|last| last.saturating_add(1) < self.frame)
            })
            .map(|(_, memory)| memory.bytes())
            .sum();
        statistics.raster_uploads = self.uploads;
        statistics.raster_evictions = self.evictions;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frozen_handle_reads_the_original_entry() {
        let original = image::Handle::from_rgba(2, 2, vec![255; 16]);
        let mut cache = Cache::default();
        cache.insert(&original, Memory::load(&original));
        let frozen = cache
            .source(&original)
            .expect("captured source")
            .handle
            .clone();
        assert_ne!(frozen.id(), original.id());

        // A replay asks for the frozen handle; the original's entry answers,
        // so nothing is decoded or uploaded again.
        assert!(cache.contains(&frozen));
        assert!(matches!(cache.get_mut(&frozen), Some(Memory::Host(_))));
        assert_eq!(cache.map.len(), 1);

        // Once the original is gone, the frozen handle stands on its own.
        let _ = cache.map.remove(&original.id());
        assert!(!cache.contains(&frozen));
    }
}
