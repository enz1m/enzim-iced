// Modified by enzUI contributors. See FORK.md.
use crate::core::{self, Size};
use crate::graphics::Shell;
use crate::image::atlas::{self, Atlas};

#[cfg(all(feature = "image", not(target_arch = "wasm32")))]
use worker::Worker;

#[cfg(feature = "image")]
use std::collections::HashMap;
use std::sync::Arc;

/// Retention policy for renderer-owned image and SVG cache entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCachePolicy {
    /// Number of complete missed frames tolerated before an entry is evicted.
    /// Values below one are normalized to one.
    pub grace_frames: u64,
    /// Maximum bytes of missed, evictable entries retained during the grace window.
    /// The budget is applied independently to raster and rasterized SVG entries.
    pub stale_byte_budget: u64,
}

impl ImageCachePolicy {
    pub const fn new(grace_frames: u64, stale_byte_budget: u64) -> Self {
        Self {
            grace_frames,
            stale_byte_budget,
        }
    }

    fn normalized(self) -> Self {
        Self {
            grace_frames: self.grace_frames.max(1),
            ..self
        }
    }
}

impl Default for ImageCachePolicy {
    /// Entries survive 600 missed frames, bounded by 64 MiB of stale bytes, so
    /// an image on a page the user left for a few seconds is still uploaded
    /// when they return instead of being decoded again.
    fn default() -> Self {
        Self::new(600, 64 * 1024 * 1024)
    }
}

/// A snapshot of renderer-owned image cache and atlas usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageCacheStatistics {
    pub frame: u64,
    pub grace_frames: u64,
    pub stale_byte_budget: u64,
    /// CPU source bytes charged while native cache, pending uploads or retained scenes own them.
    pub raster_source_bytes: usize,
    pub raster_entries: usize,
    pub raster_bytes: u64,
    pub raster_stale_entries: usize,
    pub raster_stale_bytes: u64,
    pub raster_uploads: u64,
    pub raster_evictions: u64,
    /// Decoder/upload jobs whose terminal result has not been consumed.
    pub pending_jobs: usize,
    /// User callbacks retained by pending jobs.
    pub pending_callbacks: usize,
    /// Inline callback bytes retained by pending jobs.
    pub pending_callback_bytes: usize,
    pub svg_entries: usize,
    pub svg_raster_entries: usize,
    pub svg_bytes: u64,
    pub svg_stale_entries: usize,
    pub svg_stale_bytes: u64,
    pub svg_uploads: u64,
    pub svg_evictions: u64,
    pub atlas_layers: usize,
    pub atlas_live_layers: usize,
    pub atlas_empty_layers: usize,
    pub atlas_logical_layers: usize,
    pub atlas_allocations: usize,
    pub atlas_occupied_bytes: u64,
    pub atlas_allocated_bytes: u64,
    pub atlas_uploads: u64,
    pub atlas_removals: u64,
    /// Times the backing array texture was reallocated to grow (#1513).
    pub atlas_growths: u32,
    /// Bytes copied from an outgoing array texture into a larger replacement.
    pub atlas_bytes_copied: u64,
    /// Bytes staged into the atlas by image uploads (padded).
    pub atlas_bytes_uploaded: u64,
}

pub struct Cache {
    atlas: Atlas,
    #[cfg(feature = "image")]
    raster: Raster,
    #[cfg(feature = "svg")]
    vector: crate::image::vector::Cache,
    #[cfg(all(feature = "image", not(target_arch = "wasm32")))]
    worker: Option<Worker>,
    policy: ImageCachePolicy,
    frame: u64,
}

impl Cache {
    pub fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        backend: wgpu::Backend,
        layout: wgpu::BindGroupLayout,
        _shell: &Shell,
    ) -> Self {
        #[cfg(feature = "image")]
        let raster_cache = crate::image::raster::Cache::default();
        #[cfg(all(feature = "image", not(target_arch = "wasm32")))]
        let worker = Worker::new(
            device,
            _queue,
            backend,
            layout.clone(),
            _shell,
            raster_cache.source_budget(),
        )
        .map_err(|error| {
            log::error!("failed to spawn image worker: {error}");
        })
        .ok();

        Self {
            atlas: Atlas::new(device, backend, layout),
            #[cfg(feature = "image")]
            raster: Raster {
                cache: raster_cache,
                pending: HashMap::new(),
                pending_callbacks: 0,
                pending_callback_bytes: 0,
                belt: wgpu::util::StagingBelt::new(device.clone(), 2 * 1024 * 1024),
                sync_uploads: 0,
            },
            #[cfg(feature = "svg")]
            vector: crate::image::vector::Cache::default(),
            #[cfg(all(feature = "image", not(target_arch = "wasm32")))]
            worker,
            policy: ImageCachePolicy::default(),
            frame: 0,
        }
    }

    pub fn set_policy(&mut self, policy: ImageCachePolicy) {
        self.policy = policy.normalized();
    }

    pub(crate) fn policy(&self) -> ImageCachePolicy {
        self.policy
    }

    /// Whether an image is still being decoded or uploaded: it appears
    /// later without a new recording (enzUI-ICED #3666).
    pub(crate) fn has_pending(&self) -> bool {
        #[cfg(feature = "image")]
        {
            !self.raster.pending.is_empty()
        }
        #[cfg(not(feature = "image"))]
        {
            false
        }
    }

    pub fn statistics(&self) -> ImageCacheStatistics {
        let atlas = self.atlas.statistics();
        let mut statistics = ImageCacheStatistics {
            frame: self.frame,
            grace_frames: self.policy.grace_frames,
            stale_byte_budget: self.policy.stale_byte_budget,
            atlas_layers: atlas.layers,
            atlas_live_layers: atlas.live_layers,
            atlas_empty_layers: atlas.empty_layers,
            atlas_logical_layers: atlas.logical_layers,
            atlas_allocations: atlas.allocations,
            atlas_occupied_bytes: atlas.occupied_bytes,
            atlas_allocated_bytes: atlas.allocated_bytes,
            atlas_uploads: atlas.uploads,
            atlas_removals: atlas.removals,
            atlas_growths: atlas.growths,
            atlas_bytes_copied: atlas.bytes_copied,
            atlas_bytes_uploaded: atlas.bytes_uploaded,
            ..ImageCacheStatistics::default()
        };

        #[cfg(feature = "image")]
        {
            self.raster.cache.add_statistics(&mut statistics);
            statistics.raster_uploads = statistics
                .raster_uploads
                .saturating_add(self.raster.sync_uploads);
            statistics.pending_jobs = self.raster.pending.len();
            statistics.pending_callbacks = self.raster.pending_callbacks;
            statistics.pending_callback_bytes = self.raster.pending_callback_bytes;
        }

        #[cfg(feature = "svg")]
        self.vector.add_statistics(&mut statistics);

        statistics
    }

    #[cfg(feature = "image")]
    pub(crate) fn disable_source_capture(&mut self) {
        self.raster.cache.source_budget().disable();
    }

    #[cfg(feature = "svg")]
    pub(crate) fn vector_identity(&self, handle: &core::svg::Handle) -> Option<usize> {
        self.vector.identity(handle)
    }

    #[cfg(feature = "svg")]
    pub(crate) fn vector_source(
        &mut self,
        handle: &core::svg::Handle,
    ) -> Option<Arc<crate::image::vector::source::Source>> {
        self.vector.source(handle)
    }
    #[cfg(feature = "svg")]
    pub(crate) fn install_vector_source(&mut self, source: &crate::image::vector::source::Source) {
        self.vector.install_source(source, &mut self.atlas);
    }

    #[cfg(feature = "image")]
    /// Whether a frame drawing `handle` now shows nothing for it: an image
    /// that is not loaded yet (its prepare only starts loading it), or one
    /// that failed to load (enzUI-ICED #3660).
    #[cfg(feature = "image")]
    pub(crate) fn raster_shows_nothing(&mut self, handle: &core::image::Handle) -> bool {
        use crate::image::raster::Memory;
        self.receive();
        if !self.raster.cache.contains(handle) {
            #[cfg(not(target_arch = "wasm32"))]
            let loads_later = self.worker.is_some();
            #[cfg(target_arch = "wasm32")]
            let loads_later = false;
            if loads_later && !matches!(handle, core::image::Handle::Rgba { .. }) {
                return true;
            }
            // RGBA handles, and every handle without a worker, load at once:
            // load it as the frame's prepare would.
            self.raster.cache.insert(handle, Memory::load(handle));
        }
        matches!(self.raster.cache.get_mut(handle), Some(Memory::Error(_)))
    }

    pub(crate) fn raster_source(
        &mut self,
        handle: &core::image::Handle,
    ) -> Option<Arc<crate::image::raster::source::Source>> {
        self.receive();
        if !self.raster.cache.contains(handle)
            && let core::image::Handle::Rgba { .. } = handle
        {
            self.raster
                .cache
                .insert(handle, crate::image::raster::Memory::load(handle));
        }

        self.raster.cache.source(handle)
    }

    #[cfg(feature = "image")]
    pub fn allocate_image(
        &mut self,
        handle: &core::image::Handle,
        callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>) + Send + 'static,
    ) {
        use crate::image::raster::Memory;

        let callback = PendingCallback::new(callback);

        if self.raster.pending.contains_key(&handle.id()) {
            if let Err(callback) = self.raster.push_callback(handle.id(), callback) {
                let _ = callback.call(Err(core::image::Error::OutOfMemory));
            }
            return;
        }

        if let Some(Memory::Device {
            allocation, entry, ..
        }) = self.raster.cache.get_mut(handle)
        {
            if let Some(allocation) = allocation
                .as_ref()
                .and_then(core::image::Allocation::upgrade)
            {
                let _ = callback.call(Ok(allocation));
                return;
            }

            #[allow(unsafe_code)]
            let new = unsafe { core::image::allocate(handle, entry.size()) };
            *allocation = Some(new.downgrade());
            let _ = callback.call(Ok(new));

            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(worker) = self.worker.as_ref() else {
                let _ = callback.call(Err(core::image::Error::Unsupported));
                return;
            };

            match worker.load(handle, true) {
                Ok(completion) => {
                    self.raster.insert_pending(handle, completion, callback);
                }
                Err(error) => {
                    let _ = callback.call(Err(error.into_image_error()));
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        let _ = callback.call(Err(core::image::Error::Unsupported));
    }

    #[cfg(feature = "image")]
    pub fn load_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: &core::image::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.load_image_with(device, queue, handle, true)
    }

    /// [`Self::load_image`] for an image a retained scene draws next
    ///: the upload is submitted ahead of that frame on the
    /// same queue, which orders it, so the caller does not wait for the GPU.
    /// Waiting stalled a replay behind the frame in flight, once per image.
    #[cfg(feature = "image")]
    pub(crate) fn load_scene_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: &core::image::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.load_image_with(device, queue, handle, false)
    }

    #[cfg(feature = "image")]
    fn load_image_with(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: &core::image::Handle,
        wait: bool,
    ) -> Result<core::image::Allocation, core::image::Error> {
        use crate::image::raster::Memory;

        if !self.raster.cache.contains(handle) {
            self.raster.cache.insert(handle, Memory::load(handle));
        }

        match self.raster.cache.get_mut(handle).unwrap() {
            Memory::Host(image) => {
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("raster image upload"),
                });

                let entry = self.atlas.upload(
                    device,
                    &mut encoder,
                    &mut self.raster.belt,
                    image.width(),
                    image.height(),
                    image,
                );

                self.raster.belt.finish();
                let submission = queue.submit([encoder.finish()]);
                self.raster.belt.recall();

                let Some(entry) = entry else {
                    return Err(core::image::Error::OutOfMemory);
                };

                if wait {
                    let _ = device.poll(wgpu::PollType::Wait {
                        submission_index: Some(submission),
                        timeout: None,
                    });
                }

                #[allow(unsafe_code)]
                let allocation = unsafe {
                    core::image::allocate(handle, Size::new(image.width(), image.height()))
                };

                self.raster.cache.insert(
                    handle,
                    Memory::Device {
                        entry,
                        bind_group: None,
                        allocation: Some(allocation.downgrade()),
                    },
                );

                Ok(allocation)
            }
            Memory::Device {
                entry, allocation, ..
            } => {
                if let Some(allocation) = allocation
                    .as_ref()
                    .and_then(core::image::Allocation::upgrade)
                {
                    return Ok(allocation);
                }

                #[allow(unsafe_code)]
                let new = unsafe { core::image::allocate(handle, entry.size()) };

                *allocation = Some(new.downgrade());

                Ok(new)
            }
            Memory::Error(error) => Err(error.clone()),
        }
    }

    #[cfg(feature = "image")]
    pub fn measure_image(&mut self, handle: &core::image::Handle) -> Option<Size<u32>> {
        self.receive();

        let image = load_image(
            &mut self.raster.cache,
            &mut self.raster.pending,
            #[cfg(not(target_arch = "wasm32"))]
            self.worker.as_ref(),
            handle,
        )?;

        Some(image.dimensions())
    }

    #[cfg(feature = "svg")]
    pub fn measure_svg(&mut self, handle: &core::svg::Handle) -> Size<u32> {
        // TODO: Concurrency
        self.vector.load(handle).viewport_dimensions()
    }

    #[cfg(feature = "image")]
    pub fn upload_raster(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        handle: &core::image::Handle,
    ) -> Option<(&atlas::Entry, &Arc<wgpu::BindGroup>)> {
        use crate::image::raster::Memory;

        self.receive();

        let Raster {
            cache,
            pending,
            sync_uploads,
            ..
        } = &mut self.raster;
        let memory = load_image(
            cache,
            pending,
            #[cfg(not(target_arch = "wasm32"))]
            self.worker.as_ref(),
            handle,
        )?;

        let image = match memory {
            Memory::Device {
                entry, bind_group, ..
            } => {
                return Some((
                    entry,
                    bind_group.as_ref().unwrap_or(self.atlas.bind_group()),
                ));
            }
            Memory::Host(image) => image.clone(),
            Memory::Error(_) => return None,
        };

        const MAX_SYNC_SIZE: usize = 2 * 1024 * 1024;

        #[cfg(not(target_arch = "wasm32"))]
        let worker_can_admit = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.can_admit_bytes(image.len()));
        #[cfg(target_arch = "wasm32")]
        let worker_can_admit = false;

        // TODO: Concurrent Wasm support
        if image.len() < MAX_SYNC_SIZE || !worker_can_admit {
            let entry =
                self.atlas
                    .upload(device, encoder, belt, image.width(), image.height(), &image)?;

            *memory = Memory::Device {
                entry,
                bind_group: None,
                allocation: None,
            };
            *sync_uploads = sync_uploads.saturating_add(1);

            if let Memory::Device { entry, .. } = memory {
                return Some((entry, self.atlas.bind_group()));
            }
        }

        if !pending.contains_key(&handle.id()) {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(worker) = self.worker.as_ref() {
                match worker.upload(image) {
                    Ok(completion) => {
                        let replaced = pending.insert(
                            handle.id(),
                            Pending {
                                handle: handle.clone(),
                                callbacks: Vec::new(),
                                callback_bytes: 0,
                                completion,
                            },
                        );
                        debug_assert!(replaced.is_none());
                    }
                    Err(_error) => {}
                }
            }
        }

        None
    }

    /// A raster of an SVG at `size` for a recording the render lane replays,
    /// when the lane cannot rasterize the SVG itself.
    #[cfg(all(feature = "svg", feature = "image"))]
    pub(crate) fn frozen_vector(
        &mut self,
        handle: &core::svg::Handle,
        color: Option<core::Color>,
        size: Size<u32>,
    ) -> Option<Arc<crate::image::raster::source::Source>> {
        let budget = self.raster.cache.source_budget();
        self.vector.freeze(handle, color, size, &budget)
    }

    #[cfg(feature = "svg")]
    pub fn upload_vector(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        handle: &core::svg::Handle,
        color: Option<core::Color>,
        size: Size<u32>,
    ) -> Option<(&atlas::Entry, &Arc<wgpu::BindGroup>)> {
        // TODO: Concurrency
        self.vector
            .upload(device, encoder, belt, handle, color, size, &mut self.atlas)
            .map(|entry| (entry, self.atlas.bind_group()))
    }

    pub fn mark_used(
        &mut self,
        images: &[crate::image::Image],
        scale: f32,
        visible: core::Rectangle,
    ) {
        for image in images {
            let clip = match image {
                crate::image::Image::Raster { clip_bounds, .. }
                | crate::image::Image::Vector { clip_bounds, .. } => clip_bounds,
            };
            if image
                .bounds()
                .intersection(clip)
                .and_then(|bounds| bounds.intersection(&visible))
                .is_none()
            {
                continue;
            }
            #[cfg(feature = "image")]
            if let crate::image::Image::Raster { image, .. } = image {
                self.raster.cache.mark_used(&image.handle);
            }
            #[cfg(feature = "svg")]
            if let crate::image::Image::Vector { svg, bounds, .. } = image {
                let bounds = crate::nudge::round(*bounds * scale);
                if bounds.width >= 1.0 && bounds.height >= 1.0 {
                    self.vector.mark_used(
                        &svg.handle,
                        svg.color,
                        Size::new(bounds.width as u32, bounds.height as u32),
                    );
                }
            }
        }
        #[cfg(not(feature = "svg"))]
        let _ = scale;
    }

    pub fn trim(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        #[cfg(feature = "image")]
        {
            self.receive();
            self.raster
                .cache
                .trim(self.frame, self.policy, &mut self.atlas, drop);
        }

        #[cfg(feature = "svg")]
        self.vector.trim(self.frame, self.policy, &mut self.atlas); // TODO: Concurrency

        self.atlas.shrink(device, encoder);
        self.frame = self.frame.saturating_add(1);
    }

    #[cfg(feature = "image")]
    pub fn receive(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(worker) = self.worker.as_ref() {
                worker.drain_notifications();
            }

            let completed = self
                .raster
                .pending
                .iter()
                .filter_map(|(id, pending)| pending.completion.try_take().map(|work| (*id, work)))
                .collect::<Vec<_>>();

            for (id, work) in completed {
                use crate::image::raster::Memory;

                let Some(pending) = self.raster.remove_pending(id) else {
                    drop(work);
                    continue;
                };

                let Pending {
                    handle, callbacks, ..
                } = pending;

                match work {
                    worker::Work::Upload {
                        entry,
                        bind_group,
                        source,
                    } => {
                        self.raster.cache.insert_source(&handle, source);

                        let allocation = if callbacks.is_empty() {
                            None
                        } else {
                            // SAFETY: `Work::Upload` carries a completed in-memory allocation
                            // paired with this pending request's `handle`.
                            #[allow(unsafe_code)]
                            let allocation =
                                unsafe { core::image::allocate(&handle, entry.size()) };
                            let reference = allocation.downgrade();

                            self.raster.cache.insert(
                                &handle,
                                Memory::Device {
                                    entry,
                                    bind_group: Some(bind_group),
                                    allocation: Some(reference),
                                },
                            );
                            dispatch_callbacks(callbacks, Ok(allocation));
                            continue;
                        };

                        self.raster.cache.insert(
                            &handle,
                            Memory::Device {
                                entry,
                                bind_group: Some(bind_group),
                                allocation,
                            },
                        );
                    }
                    worker::Work::Error(error) => {
                        self.raster
                            .cache
                            .insert(&handle, Memory::Error(error.clone()));
                        dispatch_callbacks(callbacks, Err(error));
                    }
                }
            }
        }
    }
}

#[cfg(all(feature = "image", not(target_arch = "wasm32")))]
impl Drop for Cache {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.cancel();
        }
        self.raster.cancel_pending();

        if let Some(worker) = self.worker.as_mut() {
            let _ = worker.quit();
        }
    }
}

#[cfg(feature = "image")]
const MAX_PENDING_CALLBACKS: usize = 1_024;
#[cfg(feature = "image")]
const MAX_CALLBACKS_PER_IMAGE: usize = 64;
#[cfg(feature = "image")]
const MAX_PENDING_CALLBACK_BYTES: usize = 1024 * 1024;

#[cfg(feature = "image")]
struct Raster {
    cache: crate::image::raster::Cache,
    pending: HashMap<core::image::Id, Pending>,
    pending_callbacks: usize,
    pending_callback_bytes: usize,
    belt: wgpu::util::StagingBelt,
    sync_uploads: u64,
}

#[cfg(feature = "image")]
type PendingCallback =
    crate::graphics::resource::Callback<Result<core::image::Allocation, core::image::Error>>;

#[cfg(feature = "image")]
struct Pending {
    handle: core::image::Handle,
    callbacks: Vec<PendingCallback>,
    callback_bytes: usize,
    #[cfg(not(target_arch = "wasm32"))]
    completion: worker::Completion,
}

#[cfg(feature = "image")]
impl Raster {
    fn push_callback(
        &mut self,
        id: core::image::Id,
        callback: PendingCallback,
    ) -> Result<(), PendingCallback> {
        let Some(pending) = self.pending.get_mut(&id) else {
            return Err(callback);
        };

        let next_callbacks = self.pending_callbacks.saturating_add(1);
        let next_bytes = self
            .pending_callback_bytes
            .saturating_add(callback.retained_bytes());

        if !callback_within_limits(pending.callbacks.len(), next_callbacks, next_bytes) {
            return Err(callback);
        }

        pending.callback_bytes = pending
            .callback_bytes
            .saturating_add(callback.retained_bytes());
        pending.callbacks.push(callback);
        self.pending_callbacks = next_callbacks;
        self.pending_callback_bytes = next_bytes;

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn insert_pending(
        &mut self,
        handle: &core::image::Handle,
        completion: worker::Completion,
        callback: PendingCallback,
    ) {
        let id = handle.id();
        let pending = Pending {
            handle: handle.clone(),
            callbacks: Vec::new(),
            callback_bytes: 0,
            completion,
        };
        let replaced = self.pending.insert(id, pending);
        debug_assert!(replaced.is_none());

        if let Err(callback) = self.push_callback(id, callback) {
            if let Some(pending) = self.remove_pending(id) {
                pending.completion.cancel();
            }
            let _ = callback.call(Err(core::image::Error::OutOfMemory));
        }
    }

    fn remove_pending(&mut self, id: core::image::Id) -> Option<Pending> {
        let pending = self.pending.remove(&id)?;
        self.pending_callbacks = self
            .pending_callbacks
            .saturating_sub(pending.callbacks.len());
        self.pending_callback_bytes = self
            .pending_callback_bytes
            .saturating_sub(pending.callback_bytes);
        Some(pending)
    }

    fn cancel_pending(&mut self) {
        for (_, pending) in self.pending.drain() {
            #[cfg(not(target_arch = "wasm32"))]
            pending.completion.cancel();
            drop(pending.callbacks);
        }
        self.pending_callbacks = 0;
        self.pending_callback_bytes = 0;
    }
}

#[cfg(feature = "image")]
fn callback_within_limits(
    callbacks_for_image: usize,
    total_callbacks: usize,
    total_bytes: usize,
) -> bool {
    callbacks_for_image < MAX_CALLBACKS_PER_IMAGE
        && total_callbacks <= MAX_PENDING_CALLBACKS
        && total_bytes <= MAX_PENDING_CALLBACK_BYTES
}

#[cfg(feature = "image")]
fn dispatch_callbacks(
    callbacks: Vec<PendingCallback>,
    result: Result<core::image::Allocation, core::image::Error>,
) {
    for callback in callbacks {
        let _ = callback.call(result.clone());
    }
}

#[cfg(feature = "image")]
fn load_image<'a>(
    cache: &'a mut crate::image::raster::Cache,
    pending: &mut HashMap<core::image::Id, Pending>,
    #[cfg(not(target_arch = "wasm32"))] worker: Option<&Worker>,
    handle: &core::image::Handle,
) -> Option<&'a mut crate::image::raster::Memory> {
    use crate::image::raster::Memory;

    if !cache.contains(handle) {
        if cfg!(target_arch = "wasm32") {
            // TODO: Concurrent support for Wasm
            cache.insert(handle, Memory::load(handle));
        } else if let core::image::Handle::Rgba { .. } = handle {
            // Load RGBA handles synchronously, since it's very cheap
            cache.insert(handle, Memory::load(handle));
        } else if !pending.contains_key(&handle.id()) {
            #[cfg(not(target_arch = "wasm32"))]
            match worker {
                Some(worker) => match worker.load(handle, false) {
                    Ok(completion) => {
                        let replaced = pending.insert(
                            handle.id(),
                            Pending {
                                handle: handle.clone(),
                                callbacks: Vec::new(),
                                callback_bytes: 0,
                                completion,
                            },
                        );
                        debug_assert!(replaced.is_none());
                    }
                    Err(error) => cache.insert(handle, Memory::Error(error.into_image_error())),
                },
                None => cache.insert(handle, Memory::load(handle)),
            }
        }
    }

    cache.get_mut(handle)
}

#[cfg(all(feature = "image", not(target_arch = "wasm32")))]
mod worker {
    use crate::core::Bytes;
    use crate::core::image;
    use crate::graphics::Shell;
    use crate::image::atlas::{self, Atlas};
    use crate::image::raster;

    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, Weak, mpsc};
    use std::thread;
    use std::time::Duration;

    const MAX_PENDING_JOBS: usize = 256;
    const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;
    const COMPLETION_NOTIFICATIONS: usize = 32;
    const GPU_POLL_SLICE: Duration = Duration::from_millis(50);
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

    pub struct Worker {
        jobs: Option<mpsc::SyncSender<Job>>,
        notifications: mpsc::Receiver<()>,
        completions: CompletionScope<Work>,
        stop: Arc<AtomicBool>,
        done: mpsc::Receiver<()>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Worker {
        pub fn new(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            backend: wgpu::Backend,
            texture_layout: wgpu::BindGroupLayout,
            shell: &Shell,
            source_budget: raster::source::Budget,
        ) -> io::Result<Self> {
            let (jobs_sender, jobs_receiver) = mpsc::sync_channel(MAX_PENDING_JOBS);
            let (notification_sender, notification_receiver) =
                mpsc::sync_channel(COMPLETION_NOTIFICATIONS);
            let (done_sender, done_receiver) = mpsc::sync_channel(1);
            let stop = Arc::new(AtomicBool::new(false));
            let completions =
                CompletionScope::new(notification_sender, MAX_PENDING_JOBS, MAX_PENDING_BYTES);

            let instance = Instance {
                device: device.clone(),
                queue: queue.clone(),
                backend,
                texture_layout,
                shell: shell.clone(),
                belt: wgpu::util::StagingBelt::new(device.clone(), 4 * 1024 * 1024),
                jobs: jobs_receiver,
                stop: Arc::clone(&stop),
                source_budget,
            };

            let handle = spawn_fallible(
                thread::Builder::new().name(String::from("iced image worker")),
                move || {
                    let _ = catch_unwind(AssertUnwindSafe(|| instance.run()));
                    let _ = done_sender.try_send(());
                },
                thread::Builder::spawn,
            )?;

            Ok(Self {
                jobs: Some(jobs_sender),
                notifications: notification_receiver,
                completions,
                stop,
                done: done_receiver,
                handle: Some(handle),
            })
        }

        pub fn load(
            &self,
            handle: &image::Handle,
            is_allocation: bool,
        ) -> Result<Completion, SubmitError> {
            let retained_bytes = match handle {
                image::Handle::Bytes(_, bytes) => bytes.len(),
                image::Handle::Rgba { pixels, .. } => pixels.len(),
                image::Handle::Path(_, path) => path.as_os_str().len(),
            };

            self.submit(retained_bytes, |terminal| Job::Load {
                handle: handle.clone(),
                is_allocation,
                terminal,
            })
        }

        pub fn upload(&self, image: raster::Image) -> Result<Completion, SubmitError> {
            let retained_bytes = image.len();

            self.submit(retained_bytes, |terminal| Job::Upload {
                width: image.width(),
                height: image.height(),
                rgba: image.into_raw(),
                terminal,
            })
        }

        pub fn can_admit_bytes(&self, retained_bytes: usize) -> bool {
            !self.stop.load(Ordering::Acquire) && retained_bytes <= MAX_PENDING_BYTES
        }

        fn submit(
            &self,
            retained_bytes: usize,
            make_job: impl FnOnce(Terminal<Work>) -> Job,
        ) -> Result<Completion, SubmitError> {
            if self.stop.load(Ordering::Acquire) {
                return Err(SubmitError::Stopped);
            }

            let (completion, terminal) = self
                .completions
                .admit(retained_bytes, Work::Error(image::Error::Unsupported))?;
            let job = make_job(terminal);
            let Some(jobs) = self.jobs.as_ref() else {
                job.reject();
                return Err(SubmitError::Stopped);
            };

            match jobs.try_send(job) {
                Ok(()) => Ok(completion),
                Err(mpsc::TrySendError::Full(job)) => {
                    job.reject();
                    Err(SubmitError::Full)
                }
                Err(mpsc::TrySendError::Disconnected(job)) => {
                    job.reject();
                    Err(SubmitError::Stopped)
                }
            }
        }

        pub fn drain_notifications(&self) {
            while self.notifications.try_recv().is_ok() {}
        }

        pub fn cancel(&self) {
            self.completions.cancel();
            self.stop.store(true, Ordering::Release);
        }

        pub fn quit(&mut self) -> bool {
            self.cancel();
            bounded_shutdown(
                &mut self.jobs,
                &self.stop,
                &self.done,
                &mut self.handle,
                SHUTDOWN_TIMEOUT,
            )
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.quit();
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SubmitError {
        Full,
        TooLarge,
        Stopped,
    }

    impl SubmitError {
        pub fn into_image_error(self) -> image::Error {
            match self {
                Self::Stopped => image::Error::Unsupported,
                Self::Full | Self::TooLarge => image::Error::OutOfMemory,
            }
        }
    }

    pub type Completion = Arc<CompletionSlot<Work>>;

    pub struct Instance {
        source_budget: raster::source::Budget,
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: wgpu::Backend,
        texture_layout: wgpu::BindGroupLayout,
        shell: Shell,
        belt: wgpu::util::StagingBelt,
        jobs: mpsc::Receiver<Job>,
        stop: Arc<AtomicBool>,
    }

    enum Job {
        Load {
            handle: image::Handle,
            is_allocation: bool,
            terminal: Terminal<Work>,
        },
        Upload {
            rgba: Bytes,
            width: u32,
            height: u32,
            terminal: Terminal<Work>,
        },
    }

    impl Job {
        fn reservation(&self) -> Arc<Reservation> {
            match self {
                Self::Load { terminal, .. } | Self::Upload { terminal, .. } => {
                    Arc::clone(&terminal._reservation)
                }
            }
        }

        fn reject(self) {
            match self {
                Self::Load { terminal, .. } | Self::Upload { terminal, .. } => {
                    terminal.cancel();
                }
            }
        }
    }

    /// Blocks until a job arrives. `None` once the worker stops: the jobs
    /// sender is dropped on shutdown, which wakes the wait, and a job that
    /// arrives after a stop is rejected rather than run. An idle worker never
    /// wakes, so it costs nothing between images.
    fn next_job(jobs: &mpsc::Receiver<Job>, stop: &AtomicBool) -> Option<Job> {
        let job = jobs.recv().ok()?;
        if stop.load(Ordering::Acquire) {
            job.reject();
            return None;
        }
        Some(job)
    }

    fn process_job(job: Job, run: impl FnOnce(Job)) {
        // GPU completion can precede handler return while its input bytes are still live.
        let reservation = job.reservation();
        run(job);
        drop(reservation);
    }

    pub enum Work {
        Upload {
            entry: atlas::Entry,
            bind_group: Arc<wgpu::BindGroup>,
            source: Option<Arc<raster::source::Source>>,
        },
        Error(image::Error),
    }

    impl Instance {
        fn run(mut self) {
            loop {
                let Some(job) = next_job(&self.jobs, &self.stop) else {
                    return;
                };

                process_job(job, |job| match job {
                    Job::Load {
                        handle,
                        is_allocation,
                        terminal,
                    } => match crate::graphics::image::load(&handle) {
                        Ok(image) => self.upload(
                            image.width(),
                            image.height(),
                            image.into_raw(),
                            if is_allocation {
                                Shell::tick
                            } else {
                                Shell::invalidate_layout
                            },
                            terminal,
                        ),
                        Err(error) => {
                            let _ = terminal.finish(Work::Error(error));
                        }
                    },
                    Job::Upload {
                        rgba,
                        width,
                        height,
                        terminal,
                    } => self.upload(width, height, rgba, Shell::request_redraw, terminal),
                });
            }
        }

        fn upload(
            &mut self,
            width: u32,
            height: u32,
            rgba: Bytes,
            callback: fn(&Shell),
            terminal: Terminal<Work>,
        ) {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("raster image upload"),
                });

            let mut atlas = Atlas::with_size(
                &self.device,
                self.backend,
                self.texture_layout.clone(),
                width.max(height),
            );

            let Some(entry) = atlas.upload(
                &self.device,
                &mut encoder,
                &mut self.belt,
                width,
                height,
                &rgba,
            ) else {
                let _ = terminal.finish(Work::Error(image::Error::OutOfMemory));
                return;
            };

            let source = self.source_budget.capture(width, height, &rgba);
            let shell = self.shell.clone();

            self.belt.finish();
            let submission = self.queue.submit([encoder.finish()]);
            self.belt.recall();

            let bind_group = atlas.bind_group().clone();

            self.queue.on_submitted_work_done(move || {
                let _ = finish_with_callback(
                    terminal,
                    Work::Upload {
                        entry,
                        bind_group,
                        source,
                    },
                    || callback(&shell),
                );
            });

            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_POLL_SLICE),
            });
        }
    }

    fn finish_with_callback<T>(terminal: Terminal<T>, value: T, callback: impl FnOnce()) -> bool {
        if terminal.finish(value) {
            let _ = catch_unwind(AssertUnwindSafe(callback));
            true
        } else {
            false
        }
    }

    fn spawn_fallible<F, S>(
        builder: thread::Builder,
        run: F,
        spawn: S,
    ) -> io::Result<thread::JoinHandle<()>>
    where
        F: FnOnce() + Send + 'static,
        S: FnOnce(thread::Builder, F) -> io::Result<thread::JoinHandle<()>>,
    {
        spawn(builder, run)
    }

    fn bounded_shutdown(
        jobs: &mut Option<mpsc::SyncSender<Job>>,
        stop: &AtomicBool,
        done: &mpsc::Receiver<()>,
        handle: &mut Option<thread::JoinHandle<()>>,
        timeout: Duration,
    ) -> bool {
        stop.store(true, Ordering::Release);
        drop(jobs.take());

        let Some(handle) = handle.take() else {
            return true;
        };
        let acknowledged = done.recv_timeout(timeout).is_ok();

        drop(handle);
        acknowledged
    }

    #[cfg(test)]
    struct ExitAck(mpsc::SyncSender<()>);

    #[cfg(test)]
    impl Drop for ExitAck {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }

    #[derive(Clone)]
    struct AdmissionBudget {
        inner: Arc<BudgetInner>,
    }

    struct BudgetInner {
        limits: Usage,
        usage: Mutex<Usage>,
    }

    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    struct Usage {
        jobs: usize,
        bytes: usize,
    }

    impl AdmissionBudget {
        fn new(jobs: usize, bytes: usize) -> Self {
            Self {
                inner: Arc::new(BudgetInner {
                    limits: Usage { jobs, bytes },
                    usage: Mutex::new(Usage::default()),
                }),
            }
        }

        fn reserve(&self, bytes: usize) -> Result<Reservation, SubmitError> {
            let mut usage = lock(&self.inner.usage);
            let Some(jobs) = usage.jobs.checked_add(1) else {
                return Err(SubmitError::Full);
            };
            let Some(total_bytes) = usage.bytes.checked_add(bytes) else {
                return Err(SubmitError::TooLarge);
            };

            if jobs > self.inner.limits.jobs {
                return Err(SubmitError::Full);
            }
            if total_bytes > self.inner.limits.bytes {
                return Err(SubmitError::TooLarge);
            }

            usage.jobs = jobs;
            usage.bytes = total_bytes;

            Ok(Reservation {
                budget: self.clone(),
                bytes,
            })
        }

        #[cfg(test)]
        fn usage(&self) -> (usize, usize) {
            let usage = lock(&self.inner.usage);
            (usage.jobs, usage.bytes)
        }
    }

    struct Reservation {
        budget: AdmissionBudget,
        bytes: usize,
    }

    impl Drop for Reservation {
        fn drop(&mut self) {
            let mut usage = lock(&self.budget.inner.usage);
            usage.jobs = usage.jobs.saturating_sub(1);
            usage.bytes = usage.bytes.saturating_sub(self.bytes);
        }
    }

    struct CompletionScopeInner {
        generation: u64,
        active: AtomicBool,
        notifications: mpsc::SyncSender<()>,
        budget: AdmissionBudget,
    }

    struct CompletionScope<T> {
        inner: Arc<CompletionScopeInner>,
        _result: std::marker::PhantomData<fn() -> T>,
    }

    impl<T> CompletionScope<T> {
        fn new(notifications: mpsc::SyncSender<()>, max_jobs: usize, max_bytes: usize) -> Self {
            static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

            Self {
                inner: Arc::new(CompletionScopeInner {
                    generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
                    active: AtomicBool::new(true),
                    notifications,
                    budget: AdmissionBudget::new(max_jobs, max_bytes),
                }),
                _result: std::marker::PhantomData,
            }
        }

        fn admit(
            &self,
            retained_bytes: usize,
            fallback: T,
        ) -> Result<(Arc<CompletionSlot<T>>, Terminal<T>), SubmitError> {
            let reservation = Arc::new(self.inner.budget.reserve(retained_bytes)?);
            let slot = Arc::new(CompletionSlot {
                generation: self.inner.generation,
                scope: Arc::downgrade(&self.inner),
                state: Mutex::new(CompletionState::Pending(Arc::clone(&reservation))),
            });
            let terminal = Terminal {
                slot: Arc::clone(&slot),
                fallback: Some(fallback),
                _reservation: reservation,
            };

            Ok((slot, terminal))
        }

        fn cancel(&self) {
            self.inner.active.store(false, Ordering::Release);
        }
    }

    impl<T> Clone for CompletionScope<T> {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
                _result: std::marker::PhantomData,
            }
        }
    }

    enum CompletionState<T> {
        Pending(Arc<Reservation>),
        Ready(T, Arc<Reservation>),
        Cancelled,
    }

    pub struct CompletionSlot<T> {
        generation: u64,
        scope: Weak<CompletionScopeInner>,
        state: Mutex<CompletionState<T>>,
    }

    impl<T> CompletionSlot<T> {
        fn complete(&self, value: T) -> bool {
            let Some(scope) = self.scope.upgrade() else {
                drop(value);
                self.cancel();
                return false;
            };

            if !scope.active.load(Ordering::Acquire) || scope.generation != self.generation {
                drop(value);
                self.cancel();
                return false;
            }

            let mut state = lock(&self.state);
            let previous = std::mem::replace(&mut *state, CompletionState::Cancelled);
            match previous {
                CompletionState::Pending(reservation) => {
                    *state = CompletionState::Ready(value, reservation);
                    drop(state);
                    let _ = scope.notifications.try_send(());
                    true
                }
                previous => {
                    *state = previous;
                    drop(value);
                    false
                }
            }
        }

        pub fn try_take(&self) -> Option<T> {
            let mut state = lock(&self.state);
            let previous = std::mem::replace(&mut *state, CompletionState::Cancelled);
            match previous {
                CompletionState::Ready(value, _reservation) => Some(value),
                previous => {
                    *state = previous;
                    None
                }
            }
        }

        pub fn cancel(&self) {
            let mut state = lock(&self.state);
            *state = CompletionState::Cancelled;
        }
    }

    struct Terminal<T> {
        slot: Arc<CompletionSlot<T>>,
        fallback: Option<T>,
        _reservation: Arc<Reservation>,
    }

    impl<T> Terminal<T> {
        fn finish(mut self, value: T) -> bool {
            self.fallback = None;
            self.slot.complete(value)
        }

        fn cancel(mut self) {
            self.fallback = None;
            self.slot.cancel();
        }
    }

    impl<T> Drop for Terminal<T> {
        fn drop(&mut self) {
            if let Some(fallback) = self.fallback.take() {
                let _ = self.slot.complete(fallback);
            }
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Condvar;
        use std::sync::atomic::AtomicUsize;
        use std::time::Instant;

        #[derive(Clone)]
        struct DropProbe(Arc<AtomicUsize>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                let _ = self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        #[test]
        fn image_worker_full_result_queue_is_nonblocking_and_reclaims() {
            let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 2, 32);
            let drops = Arc::new(AtomicUsize::new(0));
            let (first, first_terminal) = scope
                .admit(8, DropProbe(Arc::clone(&drops)))
                .expect("first completion should be admitted");
            let (second, second_terminal) = scope
                .admit(8, DropProbe(Arc::clone(&drops)))
                .expect("second completion should be admitted");

            let _ = first_terminal.finish(DropProbe(Arc::clone(&drops)));
            let _ = second_terminal.finish(DropProbe(Arc::clone(&drops)));

            assert!(wake_receiver.try_recv().is_ok());
            assert!(wake_receiver.try_recv().is_err());
            assert!(first.try_take().is_some());
            assert!(second.try_take().is_some());
            assert_eq!(scope.inner.budget.usage(), (0, 0));
            assert_eq!(drops.load(Ordering::SeqCst), 4);
        }

        #[test]
        fn image_worker_job_count_and_bytes_are_capped_and_released() {
            let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 1, 8);
            let (slot, terminal) = scope
                .admit(8, ())
                .expect("first job should fit both limits");

            assert!(matches!(scope.admit(0, ()), Err(SubmitError::Full)));
            terminal.cancel();
            slot.cancel();
            assert_eq!(scope.inner.budget.usage(), (0, 0));
            assert!(matches!(scope.admit(9, ()), Err(SubmitError::TooLarge)));
        }

        #[test]
        fn image_worker_pending_callback_count_and_bytes_are_capped() {
            assert!(super::super::callback_within_limits(
                super::super::MAX_CALLBACKS_PER_IMAGE - 1,
                super::super::MAX_PENDING_CALLBACKS,
                super::super::MAX_PENDING_CALLBACK_BYTES,
            ));
            assert!(!super::super::callback_within_limits(
                super::super::MAX_CALLBACKS_PER_IMAGE,
                super::super::MAX_PENDING_CALLBACKS,
                super::super::MAX_PENDING_CALLBACK_BYTES,
            ));
            assert!(!super::super::callback_within_limits(
                0,
                super::super::MAX_PENDING_CALLBACKS + 1,
                super::super::MAX_PENDING_CALLBACK_BYTES,
            ));
            assert!(!super::super::callback_within_limits(
                0,
                1,
                super::super::MAX_PENDING_CALLBACK_BYTES + 1,
            ));
        }

        #[test]
        fn image_worker_callback_panic_does_not_skip_later_callbacks() {
            let calls = Arc::new(AtomicUsize::new(0));
            let later_calls = Arc::clone(&calls);
            let callbacks = vec![
                super::super::PendingCallback::new(|_| panic!("injected callback panic")),
                super::super::PendingCallback::new(move |_| {
                    let _ = later_calls.fetch_add(1, Ordering::SeqCst);
                }),
            ];

            super::super::dispatch_callbacks(callbacks, Err(image::Error::OutOfMemory));

            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn resource_reclamation_callback_panic_drops_capture_and_runs_later_callback() {
            let drops = Arc::new(AtomicUsize::new(0));
            let calls = Arc::new(AtomicUsize::new(0));
            let first_probe = DropProbe(Arc::clone(&drops));
            let second_probe = DropProbe(Arc::clone(&drops));
            let later_calls = Arc::clone(&calls);
            let callbacks = vec![
                super::super::PendingCallback::new(move |_| {
                    drop(first_probe);
                    panic!("injected retained-resource callback panic");
                }),
                super::super::PendingCallback::new(move |_| {
                    drop(second_probe);
                    let _ = later_calls.fetch_add(1, Ordering::SeqCst);
                }),
            ];

            super::super::dispatch_callbacks(callbacks, Err(image::Error::OutOfMemory));

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
        }

        #[test]
        fn image_worker_upload_failure_is_one_terminal_result() {
            let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 1, 16);
            let (slot, terminal) = scope
                .admit(16, Err::<(), _>(image::Error::OutOfMemory))
                .expect("upload should be admitted");

            let _ = terminal.finish(Err(image::Error::OutOfMemory));
            assert!(matches!(
                slot.try_take(),
                Some(Err(image::Error::OutOfMemory))
            ));
            assert!(slot.try_take().is_none());
            assert_eq!(scope.inner.budget.usage(), (0, 0));
        }

        #[test]
        fn image_worker_dropped_admitted_job_gets_one_terminal_result() {
            let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 1, 4);
            let (slot, terminal) = scope.admit(4, 41_u8).expect("job should be admitted");

            drop(terminal);
            assert_eq!(slot.try_take(), Some(41));
            assert!(slot.try_take().is_none());
            assert_eq!(scope.inner.budget.usage(), (0, 0));
        }

        #[test]
        fn image_worker_spawn_failure_is_reported_without_running() {
            let ran = Arc::new(AtomicBool::new(false));
            let ran_in_thread = Arc::clone(&ran);
            let result = spawn_fallible(
                thread::Builder::new(),
                move || ran_in_thread.store(true, Ordering::SeqCst),
                |_builder, _run| Err(io::Error::other("injected spawn failure")),
            );

            assert!(result.is_err());
            assert!(!ran.load(Ordering::SeqCst));
        }

        #[test]
        fn image_worker_blocked_shutdown_is_bounded_and_late_result_reclaims() {
            let (jobs_sender, jobs_receiver) = mpsc::sync_channel::<Job>(1);
            let (done_sender, done_receiver) = mpsc::sync_channel(1);
            let stop = Arc::new(AtomicBool::new(false));
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            let thread_gate = Arc::clone(&gate);
            let (released_sender, released_receiver) = mpsc::sync_channel(1);
            let drops = Arc::new(AtomicUsize::new(0));
            let thread_drops = Arc::clone(&drops);

            let handle = thread::spawn(move || {
                let _done = ExitAck(done_sender);
                let _jobs = jobs_receiver;
                let _probe = DropProbe(thread_drops);
                let (mutex, condition) = &*thread_gate;
                let mut released = lock(mutex);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !*released {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return;
                    }
                    let waited = condition
                        .wait_timeout(released, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    released = waited.0;
                }
                let _ = released_sender.try_send(());
            });
            let mut jobs = Some(jobs_sender);
            let mut handle = Some(handle);

            let started = Instant::now();
            assert!(!bounded_shutdown(
                &mut jobs,
                &stop,
                &done_receiver,
                &mut handle,
                Duration::from_millis(20),
            ));
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_eq!(drops.load(Ordering::SeqCst), 0);

            let (mutex, condition) = &*gate;
            *lock(mutex) = true;
            condition.notify_all();
            released_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("blocked worker should release after the gate opens");

            let deadline = Instant::now() + Duration::from_secs(1);
            while drops.load(Ordering::SeqCst) != 1 && Instant::now() < deadline {
                thread::yield_now();
            }
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn image_worker_cancellation_keeps_live_job_and_callback_input_charged() {
            let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 1, 8);
            let (slot, terminal) = scope
                .admit(8, Work::Error(image::Error::Unsupported))
                .unwrap();
            let job = Job::Upload {
                rgba: Bytes::from(vec![0; 8]),
                width: 2,
                height: 1,
                terminal,
            };
            process_job(job, |job| {
                let Job::Upload { terminal, rgba, .. } = job else {
                    panic!("upload fixture")
                };
                slot.cancel();
                assert_eq!(scope.inner.budget.usage(), (1, 8));
                assert!(matches!(
                    scope.admit(1, Work::Error(image::Error::Unsupported)),
                    Err(SubmitError::Full)
                ));
                drop(terminal);
                assert_eq!(scope.inner.budget.usage(), (1, 8));
                assert_eq!(rgba.len(), 8);
                drop(rgba);
            });
            assert_eq!(scope.inner.budget.usage(), (0, 0));
        }

        /// An idle worker sleeps until a job arrives: the waiting thread's
        /// voluntary context switches over an idle stretch stay at the one
        /// wake for the job, where a polling wait would add one per poll. A
        /// job that arrives after a stop is rejected (its charge released),
        /// and a dropped sender ends the wait.
        #[cfg(target_os = "linux")]
        #[test]
        fn an_idle_image_worker_sleeps_until_a_job_or_shutdown() {
            fn switches() -> u64 {
                std::fs::read_to_string("/proc/thread-self/status")
                    .expect("thread status")
                    .lines()
                    .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
                    .and_then(|count| count.trim().parse().ok())
                    .expect("voluntary_ctxt_switches")
            }
            let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
            let scope = CompletionScope::new(wake_sender, 2, 16);
            let job = |scope: &CompletionScope<Work>| {
                let (_slot, terminal) = scope
                    .admit(8, Work::Error(image::Error::Unsupported))
                    .unwrap();
                Job::Upload {
                    rgba: Bytes::from(vec![0; 8]),
                    width: 2,
                    height: 1,
                    terminal,
                }
            };
            let (jobs, receiver) = mpsc::sync_channel(2);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                let before = switches();
                let first = next_job(&receiver, &worker_stop).is_some();
                let idle_wakes = switches() - before;
                let after_stop = next_job(&receiver, &worker_stop).is_none();
                (idle_wakes, first, after_stop)
            });

            thread::sleep(Duration::from_millis(300));
            jobs.send(job(&scope)).unwrap();
            thread::sleep(Duration::from_millis(50));
            stop.store(true, Ordering::Release);
            jobs.send(job(&scope)).unwrap();
            let (idle_wakes, first, after_stop) = worker.join().unwrap();

            assert!(first, "a job ends the idle wait");
            assert!(
                idle_wakes <= 3,
                "an idle worker woke {idle_wakes} times in 300 ms"
            );
            assert!(after_stop, "a job after a stop is not run");
            assert_eq!(
                scope.inner.budget.usage(),
                (0, 0),
                "the rejected job's charge is released"
            );

            let (jobs, receiver) = mpsc::sync_channel::<Job>(1);
            let waiter =
                thread::spawn(move || next_job(&receiver, &AtomicBool::new(false)).is_none());
            thread::sleep(Duration::from_millis(20));
            drop(jobs);
            assert!(waiter.join().unwrap(), "a dropped sender ends the wait");
        }

        #[test]
        fn resource_reclamation_cancelled_late_completion_does_not_notify_window() {
            for cancel_slot in [false, true] {
                let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
                let scope = CompletionScope::new(wake_sender, 1, 8);
                let drops = Arc::new(AtomicUsize::new(0));
                let notifications = Arc::new(AtomicUsize::new(0));
                let (slot, terminal) = scope
                    .admit(8, DropProbe(Arc::clone(&drops)))
                    .expect("completion should be admitted");

                scope.cancel();
                if cancel_slot {
                    slot.cancel();
                }
                let calls = Arc::clone(&notifications);
                assert!(!finish_with_callback(
                    terminal,
                    DropProbe(Arc::clone(&drops)),
                    move || {
                        let _ = calls.fetch_add(1, Ordering::SeqCst);
                    },
                ));

                assert_eq!(notifications.load(Ordering::SeqCst), 0);
                assert_eq!(drops.load(Ordering::SeqCst), 2);
                assert_eq!(scope.inner.budget.usage(), (0, 0));
            }
        }

        #[test]
        fn image_worker_cancelled_generation_drops_late_result_exactly_once() {
            for cancel_slot in [false, true] {
                let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
                let scope = CompletionScope::new(wake_sender, 1, 8);
                let drops = Arc::new(AtomicUsize::new(0));
                let (slot, terminal) = scope
                    .admit(8, DropProbe(Arc::clone(&drops)))
                    .expect("completion should be admitted");

                scope.cancel();
                if cancel_slot {
                    slot.cancel();
                }
                assert_eq!(scope.inner.budget.usage(), (1, 8));
                let _ = terminal.finish(DropProbe(Arc::clone(&drops)));

                assert!(slot.try_take().is_none());
                assert_eq!(scope.inner.budget.usage(), (0, 0));
                assert_eq!(drops.load(Ordering::SeqCst), 2);
            }
        }
    }
}
