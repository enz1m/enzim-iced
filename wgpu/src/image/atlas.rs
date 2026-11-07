// Modified by enzUI contributors. See FORK.md.
pub mod entry;

mod allocation;
mod allocator;
mod layer;

pub use allocation::Allocation;
pub use entry::Entry;
pub use layer::Layer;

use allocator::Allocator;

pub const DEFAULT_SIZE: u32 = 2048;
pub const MAX_SIZE: u32 = 2048;

use crate::core::Size;
use crate::graphics::color;

use std::sync::Arc;

/// Bytes held by one atlas layer of `size` x `size` RGBA8 texels.
pub fn layer_bytes(size: u32) -> u64 {
    u64::from(size) * u64::from(size) * 4
}

/// Default bound on the **peak transient** footprint of the atlas array, in
/// bytes: during a growth the outgoing texture and its replacement are alive at
/// the same time, so the peak is what a budget has to bound.
///
/// Override with `ICED_IMAGE_ATLAS_BUDGET_MB`.
pub const DEFAULT_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

fn budget_bytes() -> u64 {
    static BUDGET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

    *BUDGET.get_or_init(|| {
        std::env::var("ICED_IMAGE_ATLAS_BUDGET_MB")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|mb| *mb > 0)
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(DEFAULT_BUDGET_BYTES)
    })
}

/// The largest number of layers the atlas may ever hold.
///
/// Bounded by the adapter's `max_texture_array_layers` and by `budget`, which
/// bounds the **peak** footprint: capacity doubles, so a growth from `n` to
/// `2n` layers is transiently `3n` layers, i.e. `1.5x` the new capacity. The
/// resident bound is therefore `budget / (1.5 * layer_bytes)`.
///
/// At least one layer is always allowed: an atlas that can hold nothing would
/// turn a tight budget into "no images at all".
pub fn max_layers_for(max_texture_array_layers: u32, size: u32, budget: u64) -> usize {
    let per_layer = layer_bytes(size).max(1);
    let affordable = (budget / per_layer).saturating_mul(2) / 3;

    (affordable.min(u64::from(max_texture_array_layers)).max(1) as usize).min(u32::MAX as usize)
}

/// The physical array depth to allocate so that `required` logical layers fit.
///
/// Capacity grows **geometrically** from `current` and is never shrunk to the
/// exact logical depth here, so a sequence of single-layer arrivals reallocates
/// `O(log n)` times instead of once per arrival. Returns `None` when `required`
/// cannot be represented within `max_layers` on this backend.
pub fn next_capacity(
    required: usize,
    current: u32,
    max_layers: usize,
    backend: wgpu::Backend,
) -> Option<u32> {
    if required > max_layers {
        return None;
    }

    let max = max_layers as u64;
    let mut capacity = u64::from(current).max(1);

    while capacity < required as u64 {
        capacity = capacity.saturating_mul(2);
    }

    capacity = capacity.min(max);

    // On the GL backend a depth that is a multiple of 6 makes wgpu take the
    // texture for a `GL_TEXTURE_CUBE_MAP` / `GL_TEXTURE_CUBE_ARRAY` rather than
    // a `GL_TEXTURE_2D_ARRAY`, so step over those depths.
    // https://github.com/gfx-rs/wgpu/blob/004e3efe84a320d9331371ed31fa50baa2414911/wgpu-hal/src/gles/mod.rs#L371
    if backend == wgpu::Backend::Gl && capacity.is_multiple_of(6) {
        if capacity < max {
            capacity += 1;
        } else if capacity > required as u64 {
            capacity -= 1;
        } else {
            return None;
        }
    }

    Some(capacity as u32)
}

#[derive(Debug)]
pub struct Atlas {
    size: u32,
    backend: wgpu::Backend,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    texture_bind_group: Arc<wgpu::BindGroup>,
    texture_layout: wgpu::BindGroupLayout,
    layers: Vec<Layer>,
    // `texture_layers` is the *physical* depth of the backing array texture,
    // tracked separately from `layers.len()` (the logical layer count) so that
    // capacity can grow geometrically without a reallocation on every arrival
    // (enzUI-ICED #1513).
    texture_layers: usize,
    max_layers: usize,
    growths: u32,
    bytes_copied: u64,
    bytes_uploaded: u64,
    occupied_bytes: u64,
    uploads: u64,
    removals: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Statistics {
    /// Physical layers the backing array texture actually has (capacity).
    pub layers: usize,
    pub live_layers: usize,
    pub empty_layers: usize,
    /// Logical layers the allocator is tracking (`<= layers`).
    pub logical_layers: usize,
    pub allocations: usize,
    pub occupied_bytes: u64,
    pub allocated_bytes: u64,
    pub uploads: u64,
    pub removals: u64,
    /// Times the backing array texture was reallocated to grow (#1513).
    pub growths: u32,
    /// Bytes copied from an outgoing array texture into a larger replacement.
    pub bytes_copied: u64,
    /// Bytes staged into the atlas by image uploads (padded).
    pub bytes_uploaded: u64,
}

impl Atlas {
    pub fn new(
        device: &wgpu::Device,
        backend: wgpu::Backend,
        texture_layout: wgpu::BindGroupLayout,
    ) -> Self {
        Self::with_size(device, backend, texture_layout, DEFAULT_SIZE)
    }

    pub fn with_size(
        device: &wgpu::Device,
        backend: wgpu::Backend,
        texture_layout: wgpu::BindGroupLayout,
        size: u32,
    ) -> Self {
        Self::with_size_and_budget(device, backend, texture_layout, size, budget_bytes())
    }

    /// Same as [`Atlas::with_size`], with an explicit peak-footprint budget in
    /// bytes instead of the process-wide one (#1513).
    pub fn with_size_and_budget(
        device: &wgpu::Device,
        backend: wgpu::Backend,
        texture_layout: wgpu::BindGroupLayout,
        size: u32,
        budget: u64,
    ) -> Self {
        let size = size.min(MAX_SIZE);
        let max_layers = max_layers_for(device.limits().max_texture_array_layers, size, budget);

        let layers = match backend {
            // On the GL backend we start with 2 layers, to help wgpu figure
            // out that this texture is `GL_TEXTURE_2D_ARRAY` rather than `GL_TEXTURE_2D`
            // https://github.com/gfx-rs/wgpu/blob/004e3efe84a320d9331371ed31fa50baa2414911/wgpu-hal/src/gles/mod.rs#L371
            wgpu::Backend::Gl => vec![Layer::Empty, Layer::Empty],
            _ => vec![Layer::Empty],
        };

        let mut layers = layers;
        layers.truncate(max_layers.max(1));

        let extent = wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers.len() as u32,
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu::image texture atlas"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu::image texture atlas bind group"),
            layout: &texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            }],
        });

        Atlas {
            size,
            backend,
            texture,
            texture_view,
            texture_bind_group: Arc::new(texture_bind_group),
            texture_layout,
            texture_layers: layers.len(),
            max_layers,
            growths: 0,
            bytes_copied: 0,
            bytes_uploaded: 0,
            layers,
            occupied_bytes: 0,
            uploads: 0,
            removals: 0,
        }
    }

    pub fn bind_group(&self) -> &Arc<wgpu::BindGroup> {
        &self.texture_bind_group
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Option<Entry> {
        let entry = {
            let current_size = self.layers.len();
            let entry = self.allocate(width, height)?;

            // We grow the internal texture after allocating if necessary. The
            // layers that existed before this allocation are the only ones with
            // content worth copying into the replacement (#1513).
            self.grow(current_size, device, encoder, self.backend);

            if self.layers.len() > self.texture_layers {
                // Growth was refused (past the array-layer limit or the
                // budget). Undo the allocation instead of uploading into a
                // layer the texture does not have. `release_entry` frees only
                // the allocator regions, never the occupancy counters, because
                // those are added below and were not added for this entry.
                self.release_entry(&entry);
                self.shrink_to_capacity();

                return None;
            }

            entry
        };

        log::debug!("Allocated atlas entry: {entry:?}");

        match &entry {
            Entry::Contiguous(allocation) => {
                self.upload_allocation(pixels, width, 0, allocation, encoder, belt);
            }
            Entry::Fragmented { fragments, .. } => {
                for fragment in fragments {
                    let (x, y) = fragment.position;
                    let offset = 4 * (y * width + x) as usize;

                    self.upload_allocation(
                        pixels,
                        width,
                        offset,
                        &fragment.allocation,
                        encoder,
                        belt,
                    );
                }
            }
        }

        self.occupied_bytes = self
            .occupied_bytes
            .saturating_add(u64::from(width) * u64::from(height) * 4);
        self.uploads = self.uploads.saturating_add(1);

        if log::log_enabled!(log::Level::Debug) {
            log::debug!(
                "Atlas layers: {} (busy: {}, allocations: {})",
                self.layers.len(),
                self.layers.iter().filter(|layer| !layer.is_empty()).count(),
                self.layers.iter().map(Layer::allocations).sum::<usize>(),
            );
        }

        Some(entry)
    }

    pub fn remove(&mut self, entry: &Entry) {
        log::debug!("Removing atlas entry: {entry:?}");

        match entry {
            Entry::Contiguous(allocation) => {
                self.deallocate(allocation);
            }
            Entry::Fragmented { fragments, .. } => {
                for fragment in fragments {
                    self.deallocate(&fragment.allocation);
                }
            }
        }

        let size = entry.size();
        self.occupied_bytes = self
            .occupied_bytes
            .saturating_sub(u64::from(size.width) * u64::from(size.height) * 4);
        self.removals = self.removals.saturating_add(1);
    }

    pub fn shrink(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let minimum_layers = if self.backend == wgpu::Backend::Gl {
            2
        } else {
            1
        };
        let retained_layers = self
            .layers
            .iter()
            .rposition(|layer| !layer.is_empty())
            .map_or(minimum_layers, |last| (last + 1).max(minimum_layers));

        if retained_layers >= self.layers.len() {
            return;
        }

        let texture_layers = match self.backend {
            wgpu::Backend::Gl if retained_layers.is_multiple_of(6) => retained_layers + 1,
            _ => retained_layers,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu::image texture atlas"),
            size: wgpu::Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers: texture_layers as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        for (layer, _) in self
            .layers
            .iter()
            .take(retained_layers)
            .enumerate()
            .filter(|(_, layer)| !layer.is_empty())
        {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::default(),
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::default(),
                },
                wgpu::Extent3d {
                    width: self.size,
                    height: self.size,
                    depth_or_array_layers: 1,
                },
            );
        }

        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let texture_bind_group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu::image texture atlas bind group"),
            layout: &self.texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            }],
        }));

        self.layers.truncate(retained_layers);
        self.texture_layers = texture_layers;
        self.texture = texture;
        self.texture_view = texture_view;
        self.texture_bind_group = texture_bind_group;
    }

    pub fn statistics(&self) -> Statistics {
        let live_layers = self.layers.iter().filter(|layer| !layer.is_empty()).count();

        Statistics {
            layers: self.texture_layers,
            live_layers,
            empty_layers: self.texture_layers - live_layers,
            logical_layers: self.layers.len(),
            allocations: self.layers.iter().map(Layer::allocations).sum(),
            occupied_bytes: self.occupied_bytes,
            allocated_bytes: u64::from(self.size)
                * u64::from(self.size)
                * self.texture_layers as u64
                * 4,
            uploads: self.uploads,
            removals: self.removals,
            growths: self.growths,
            bytes_copied: self.bytes_copied,
            bytes_uploaded: self.bytes_uploaded,
        }
    }

    /// Whether one more logical layer can be backed by a texture on this
    /// backend, within the array-layer limit and the budget (#1513).
    fn can_push_layer(&self) -> bool {
        next_capacity(
            self.layers.len() + 1,
            self.texture_layers as u32,
            self.max_layers,
            self.backend,
        )
        .is_some()
    }

    /// Drops trailing empty logical layers the current texture cannot back.
    ///
    /// A refused allocation can leave layers behind it — a fragmented image
    /// that took two pages before the third was refused, say. They must go, or
    /// the next growth would try to copy content out of a layer the outgoing
    /// texture never had (a real `TextureOverrun`, observed before the fix).
    fn shrink_to_capacity(&mut self) {
        while self.layers.len() > self.texture_layers
            && self.layers.last().is_some_and(Layer::is_empty)
        {
            let _ = self.layers.pop();
        }
    }

    /// Frees the allocator regions an entry holds without touching the
    /// occupancy counters — for rolling back an allocation whose upload was
    /// refused and therefore never counted (#1513).
    fn release_entry(&mut self, entry: &Entry) {
        match entry {
            Entry::Contiguous(allocation) => self.deallocate(allocation),
            Entry::Fragmented { fragments, .. } => {
                for fragment in fragments {
                    self.deallocate(&fragment.allocation);
                }
            }
        }
    }

    fn allocate(&mut self, width: u32, height: u32) -> Option<Entry> {
        let allocated = self.allocate_inner(width, height);

        if allocated.is_none() {
            // A partial failure can leave trailing empty logical layers the
            // texture cannot back; drop them before returning.
            self.shrink_to_capacity();
        }

        allocated
    }

    fn allocate_inner(&mut self, width: u32, height: u32) -> Option<Entry> {
        // Allocate one layer if texture fits perfectly
        if width == self.size && height == self.size {
            let mut empty_layers = self
                .layers
                .iter_mut()
                .enumerate()
                .filter(|(_, layer)| layer.is_empty());

            if let Some((i, layer)) = empty_layers.next() {
                *layer = Layer::Full;

                return Some(Entry::Contiguous(Allocation::Full {
                    layer: i,
                    size: self.size,
                }));
            }

            if !self.can_push_layer() {
                return None;
            }

            self.layers.push(Layer::Full);

            return Some(Entry::Contiguous(Allocation::Full {
                layer: self.layers.len() - 1,
                size: self.size,
            }));
        }

        // Split big textures across multiple layers
        if width > self.size || height > self.size {
            let mut fragments: Vec<entry::Fragment> = Vec::new();
            let mut y = 0;

            while y < height {
                let height = std::cmp::min(height - y, self.size);
                let mut x = 0;

                while x < width {
                    let width = std::cmp::min(width - x, self.size);

                    let Some(allocation) = self.allocate_inner(width, height) else {
                        // Roll back the fragments we already took, so a refused
                        // allocation does not leak the layers it half-filled.
                        for fragment in &fragments {
                            self.deallocate(&fragment.allocation);
                        }

                        return None;
                    };

                    if let Entry::Contiguous(allocation) = allocation {
                        fragments.push(entry::Fragment {
                            position: (x, y),
                            allocation,
                        });
                    }

                    x += width;
                }

                y += height;
            }

            return Some(Entry::Fragmented {
                size: Size::new(width, height),
                fragments,
            });
        }

        // Try allocating on an existing layer
        for (i, layer) in self.layers.iter_mut().enumerate() {
            match layer {
                Layer::Empty => {
                    let mut allocator = Allocator::new(self.size);

                    if let Some(region) = allocator.allocate(width, height) {
                        *layer = Layer::Busy(allocator);

                        return Some(Entry::Contiguous(Allocation::Partial {
                            region,
                            layer: i,
                            atlas_size: self.size,
                        }));
                    }
                }
                Layer::Busy(allocator) => {
                    if let Some(region) = allocator.allocate(width, height) {
                        return Some(Entry::Contiguous(Allocation::Partial {
                            region,
                            layer: i,
                            atlas_size: self.size,
                        }));
                    }
                }
                Layer::Full => {}
            }
        }

        // Create new layer with atlas allocator
        if !self.can_push_layer() {
            return None;
        }

        let mut allocator = Allocator::new(self.size);

        if let Some(region) = allocator.allocate(width, height) {
            self.layers.push(Layer::Busy(allocator));

            return Some(Entry::Contiguous(Allocation::Partial {
                region,
                layer: self.layers.len() - 1,
                atlas_size: self.size,
            }));
        }

        // We ran out of memory (?)
        None
    }

    fn deallocate(&mut self, allocation: &Allocation) {
        log::debug!("Deallocating atlas: {allocation:?}");

        match allocation {
            Allocation::Full { layer, .. } => {
                self.layers[*layer] = Layer::Empty;
            }
            Allocation::Partial { layer, region, .. } => {
                let layer = &mut self.layers[*layer];

                if let Layer::Busy(allocator) = layer {
                    allocator.deallocate(region);

                    if allocator.is_empty() {
                        *layer = Layer::Empty;
                    }
                }
            }
        }
    }

    fn upload_allocation(
        &mut self,
        pixels: &[u8],
        image_width: u32,
        offset: usize,
        allocation: &Allocation,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
    ) {
        let (x, y) = allocation.position();
        let Size { width, height } = allocation.size();
        let layer = allocation.layer();
        let padding = allocation.padding();

        // It is a webgpu requirement that:
        //   BufferCopyView.layout.bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT == 0
        // So we calculate bytes_per_row by rounding width up to the next
        // multiple of wgpu::COPY_BYTES_PER_ROW_ALIGNMENT.
        let bytes_per_row = (4 * (width + padding.width * 2))
            .next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            as usize;
        let total_bytes = bytes_per_row * (height + padding.height * 2) as usize;

        self.bytes_uploaded = self.bytes_uploaded.saturating_add(total_bytes as u64);

        let buffer_slice = belt.allocate(
            wgpu::BufferSize::new(total_bytes as u64).unwrap(),
            wgpu::BufferSize::new(8 * 4).unwrap(),
        );

        const PIXEL: usize = 4;

        let mut fragment = buffer_slice.get_mapped_range_mut();
        let w = width as usize;
        let h = height as usize;
        let pad_w = padding.width as usize;
        let pad_h = padding.height as usize;
        let stride = PIXEL * w;

        // Copy image rows
        for row in 0..h {
            let src = offset + row * PIXEL * image_width as usize;
            let dst = (row + pad_h) * bytes_per_row;

            fragment
                .slice(dst + PIXEL * pad_w..dst + PIXEL * pad_w + stride)
                .copy_from_slice(&pixels[src..src + stride]);

            // Add padding to the sides, if needed
            for i in 0..pad_w {
                fragment
                    .slice(dst + PIXEL * i..dst + PIXEL * (i + 1))
                    .copy_from_slice(&pixels[src..src + PIXEL]);

                fragment
                    .slice(
                        dst + stride + PIXEL * (pad_w + i)..dst + stride + PIXEL * (pad_w + i + 1),
                    )
                    .copy_from_slice(&pixels[src + stride - PIXEL..src + stride]);
            }
        }

        // Add padding on top and bottom
        for row in 0..pad_h {
            let dst_top = row * bytes_per_row;
            let dst_bottom = (pad_h + h + row) * bytes_per_row;
            let src_top = offset;
            let src_bottom = offset + (h - 1) * PIXEL * image_width as usize;

            // Top
            fragment
                .slice(dst_top + PIXEL * pad_w..dst_top + PIXEL * (pad_w + w))
                .copy_from_slice(&pixels[src_top..src_top + PIXEL * w]);

            // Bottom
            fragment
                .slice(dst_bottom + PIXEL * pad_w..dst_bottom + PIXEL * (pad_w + w))
                .copy_from_slice(&pixels[src_bottom..src_bottom + PIXEL * w]);

            // Corners
            for i in 0..pad_w {
                // Top left
                fragment
                    .slice(dst_top + PIXEL * i..dst_top + PIXEL * (i + 1))
                    .copy_from_slice(&pixels[offset..offset + PIXEL]);

                // Top right
                fragment
                    .slice(dst_top + PIXEL * (w + pad_w + i)..dst_top + PIXEL * (w + pad_w + i + 1))
                    .copy_from_slice(&pixels[offset + PIXEL * (w - 1)..offset + PIXEL * w]);

                // Bottom left
                fragment
                    .slice(dst_bottom + PIXEL * i..dst_bottom + PIXEL * (i + 1))
                    .copy_from_slice(&pixels[src_bottom..src_bottom + PIXEL]);

                // Bottom right
                fragment
                    .slice(
                        dst_bottom + PIXEL * (w + pad_w + i)
                            ..dst_bottom + PIXEL * (w + pad_w + i + 1),
                    )
                    .copy_from_slice(&pixels[src_bottom + PIXEL * (w - 1)..src_bottom + PIXEL * w]);
            }
        }

        // Copy actual image
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: buffer_slice.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: buffer_slice.offset(),
                    bytes_per_row: Some(bytes_per_row as u32),
                    rows_per_image: Some(height + padding.height * 2),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: x - padding.width,
                    y: y - padding.height,
                    z: layer as u32,
                },
                aspect: wgpu::TextureAspect::default(),
            },
            wgpu::Extent3d {
                width: width + padding.width * 2,
                height: height + padding.height * 2,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Reallocates the backing array texture if the logical layers no longer
    /// fit in it, copying the `occupied_before` layers that already hold
    /// content into the replacement.
    ///
    /// Capacity is **geometric**, not exact: it is only ever grown here, so a
    /// run of single-layer arrivals reallocates `O(log n)` times and copies
    /// each layer `O(log n)` times, instead of reallocating and copying every
    /// live layer on every arrival (enzUI-ICED #1513). Reclamation of the
    /// excess capacity is left to `shrink`, which #1512 runs on settled
    /// frames.
    fn grow(
        &mut self,
        occupied_before: usize,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        backend: wgpu::Backend,
    ) {
        if self.layers.len() <= self.texture_layers {
            // The current texture already has capacity for the logical layers.
            return;
        }

        let Some(depth_or_array_layers) = next_capacity(
            self.layers.len(),
            self.texture_layers as u32,
            self.max_layers,
            backend,
        ) else {
            log::warn!(
                "Image atlas cannot grow past {} layers ({} requested); refusing allocation",
                self.texture_layers,
                self.layers.len(),
            );

            // Leave `texture_layers` unchanged; `upload` detects the shortfall
            // and rolls the allocation back.
            return;
        };

        let new_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu::image texture atlas"),
            size: wgpu::Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let mut bytes_copied = 0;

        // Never read past the outgoing texture: it only has `texture_layers`
        // layers, however many logical layers the allocator is now tracking.
        let copies = occupied_before.min(self.texture_layers);

        for (i, layer) in self.layers.iter_mut().take(copies).enumerate() {
            if layer.is_empty() {
                continue;
            }

            bytes_copied += layer_bytes(self.size);

            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: i as u32,
                    },
                    aspect: wgpu::TextureAspect::default(),
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &new_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: i as u32,
                    },
                    aspect: wgpu::TextureAspect::default(),
                },
                wgpu::Extent3d {
                    width: self.size,
                    height: self.size,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.texture_layers = depth_or_array_layers as usize;
        self.growths += 1;
        self.bytes_copied = self.bytes_copied.saturating_add(bytes_copied);

        log::debug!(
            "Grew image atlas to {} layers of capacity ({} logical, growth #{}, {bytes_copied} bytes copied)",
            self.texture_layers,
            self.layers.len(),
            self.growths,
        );

        self.texture = new_texture;
        self.texture_view = self.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        self.texture_bind_group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu::image texture atlas bind group"),
            layout: &self.texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&self.texture_view),
            }],
        }));
    }
}

#[cfg(test)]
mod tests {
    //! enzUI-ICED #1513 — the atlas used to reallocate its array texture to the
    //! *exact* logical depth after every allocation, copying every occupied
    //! layer each time: N sequential single-layer arrivals cost N-1
    //! reallocations and 1+2+...+(N-1) whole-layer copies.
    //!
    //! Capacity is now geometric and tracked separately from the logical layer
    //! count. The policy tests below need no GPU; the behaviour tests drive a
    //! real `wgpu` device and **skip with a printed reason** when no adapter is
    //! available, rather than fabricating a claim.

    use super::*;

    /// A tiny atlas: one layer is 64x64x4 = 16 KiB, so a 32-layer run costs
    /// half a megabyte instead of half a gigabyte, and `64 * 4` is exactly
    /// `COPY_BYTES_PER_ROW_ALIGNMENT`, so read-back needs no row padding.
    const SIZE: u32 = 64;

    const LIMIT: u32 = 256;

    /// The peak budget that makes [`max_layers_for`] allow exactly `layers`
    /// layers of [`SIZE`]: it allows `floor(budget / layer_bytes) * 2 / 3`.
    fn budget_for(layers: u64) -> u64 {
        let budget = layer_bytes(SIZE) * (3 * layers + 1) / 2;

        assert_eq!(
            max_layers_for(LIMIT, SIZE, budget) as u64,
            layers,
            "the test budget must buy exactly {layers} layers"
        );

        budget
    }

    // ---------------------------------------------------------------- policy

    #[test]
    fn sequential_arrivals_reallocate_logarithmically() {
        let mut capacity = 1;
        let mut growths = 0;
        let mut copies = 0;

        for required in 1..=32usize {
            if required > capacity as usize {
                copies += u64::from(capacity); // every occupied layer is copied
                capacity = next_capacity(required, capacity, LIMIT as usize, wgpu::Backend::Vulkan)
                    .expect("capacity within limits");
                growths += 1;
            }
        }

        assert_eq!(capacity, 32, "capacity must still cover 32 layers");
        assert!(
            growths <= 6,
            "32 sequential arrivals must reallocate O(log n) times, got {growths}"
        );
        assert!(
            copies <= 2 * 32,
            "copied layers must stay amortized O(n), got {copies}"
        );
    }

    #[test]
    fn capacity_is_bounded_by_array_layers_and_budget() {
        // The adapter limit binds when the budget is generous...
        assert_eq!(max_layers_for(8, 2048, u64::MAX / 4), 8);

        // ...and the budget binds when the adapter is generous. 1 GiB of peak
        // footprint at 16 MiB per 2048 layer = 42 resident layers (the peak of
        // a doubling growth is 1.5x the new capacity).
        assert_eq!(max_layers_for(256, 2048, DEFAULT_BUDGET_BYTES), 42);

        // A budget too small for even one layer still allows one.
        assert_eq!(max_layers_for(256, 2048, 1), 1);

        // Beyond the bound, growth is refused instead of validated away.
        assert_eq!(
            next_capacity(9, 8, 8, wgpu::Backend::Vulkan),
            None,
            "a request past the layer bound must be refused"
        );
        assert_eq!(
            next_capacity(8, 4, 8, wgpu::Backend::Vulkan),
            Some(8),
            "the bound itself must remain reachable"
        );
    }

    #[test]
    fn gl_capacity_never_lands_on_a_cubemap_depth() {
        // wgpu's GLES backend reads a depth that is a multiple of 6 as a
        // cubemap, so no GL capacity may ever be one.
        let mut capacity = 2;

        for required in 1..=64usize {
            if required > capacity as usize {
                capacity = next_capacity(required, capacity, 300, wgpu::Backend::Gl)
                    .expect("capacity within limits");
            }

            assert!(
                !capacity.is_multiple_of(6),
                "GL capacity {capacity} for {required} layers is a cubemap depth"
            );
            assert!(capacity as usize >= required);
        }

        // Forced onto the bound: 6 layers with a 6-layer limit cannot be
        // represented on GL, and is refused rather than mis-typed.
        assert_eq!(next_capacity(5, 4, 6, wgpu::Backend::Gl), Some(5));
        assert_eq!(next_capacity(6, 4, 6, wgpu::Backend::Gl), None);
    }

    // ------------------------------------------------------------------ gpu

    struct Gpu {
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: wgpu::Backend,
        layout: wgpu::BindGroupLayout,
    }

    fn gpu_with(backends: wgpu::Backends) -> Option<Gpu> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends;

        let instance = wgpu::Instance::new(descriptor);

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::None,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;

        let backend = adapter.get_info().backend;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("iced_wgpu::image::atlas test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .ok()?;

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iced_wgpu::image texture atlas layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            }],
        });

        Some(Gpu {
            device,
            queue,
            backend,
            layout,
        })
    }

    fn gpu() -> Option<Gpu> {
        gpu_with(wgpu::Backends::all())
    }

    fn new_atlas(gpu: &Gpu, budget: u64) -> Atlas {
        Atlas::with_size_and_budget(&gpu.device, gpu.backend, gpu.layout.clone(), SIZE, budget)
    }

    /// Uploads a `width` x `height` image filled with `fill`, submitting the
    /// work exactly as the cache does (one arrival, one submission).
    fn upload(
        gpu: &Gpu,
        atlas: &mut Atlas,
        width: u32,
        height: u32,
        fill: [u8; 4],
    ) -> Option<Entry> {
        let pixels: Vec<u8> = (0..width * height).flat_map(|_| fill).collect();

        let mut belt = wgpu::util::StagingBelt::new(gpu.device.clone(), 1024 * 1024);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        let entry = atlas.upload(&gpu.device, &mut encoder, &mut belt, width, height, &pixels);

        belt.finish();
        let submission = gpu.queue.submit([encoder.finish()]);
        belt.recall();

        let _ = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        });

        entry
    }

    /// Reads one whole atlas layer back as RGBA bytes.
    fn read_layer(gpu: &Gpu, atlas: &Atlas, layer: u32) -> Vec<u8> {
        let bytes = (SIZE * SIZE * 4) as u64;

        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas test readback"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::default(),
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );

        let submission = gpu.queue.submit([encoder.finish()]);

        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});

        let _ = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        });

        let data = buffer.slice(..).get_mapped_range().to_vec();
        buffer.unmap();

        data
    }

    fn fill_of(i: u32) -> [u8; 4] {
        [(10 + i * 7) as u8, (20 + i * 3) as u8, (i * 5) as u8, 255]
    }

    fn run_sequential_growth(gpu: &Gpu, arrivals: u32) -> Statistics {
        let mut atlas = new_atlas(gpu, budget_for(64));

        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

        for i in 0..arrivals {
            assert!(
                upload(gpu, &mut atlas, SIZE, SIZE, fill_of(i)).is_some(),
                "arrival {i} must be allocated"
            );
        }

        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "growth produced a validation error: {error:?}"
        );

        atlas.statistics()
    }

    #[test]
    fn sequential_full_layer_arrivals_amortize_growth_and_copies() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        const ARRIVALS: u32 = 32;

        let stats = run_sequential_growth(&gpu, ARRIVALS);
        println!("{ARRIVALS} sequential single-layer arrivals -> {stats:?}");

        assert_eq!(stats.logical_layers, ARRIVALS as usize);
        assert!(
            stats.layers as u32 >= ARRIVALS,
            "capacity {} cannot hold {ARRIVALS} layers",
            stats.layers
        );

        // Exact-depth growth needs ARRIVALS - 1 reallocations. Geometric growth
        // needs at most log2(ARRIVALS) + 1.
        assert!(
            stats.growths <= 6,
            "{ARRIVALS} sequential arrivals reallocated the array {} times; \
             geometric growth allows at most 6",
            stats.growths
        );

        // Exact-depth growth copies 1+2+...+(N-1) = 496 layers for N = 32.
        let occupied = u64::from(ARRIVALS) * layer_bytes(SIZE);
        assert!(
            stats.bytes_copied <= 2 * occupied,
            "copied {} bytes to place {occupied} bytes of content; growth is \
             not amortized",
            stats.bytes_copied
        );

        assert_eq!(
            stats.occupied_bytes, occupied,
            "occupancy accounting drifted"
        );
        assert!(
            stats.bytes_uploaded >= occupied,
            "upload accounting ({}) cannot be below the content uploaded",
            stats.bytes_uploaded
        );
        assert_eq!(
            stats.allocated_bytes,
            stats.layers as u64 * layer_bytes(SIZE)
        );
    }

    #[test]
    fn every_growth_preserves_previously_uploaded_texels() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        const ARRIVALS: u32 = 17; // crosses the 2, 4, 8 and 16 capacity steps

        let mut atlas = new_atlas(&gpu, budget_for(64));
        let mut layers = Vec::new();

        for i in 0..ARRIVALS {
            let entry = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(i))
                .expect("arrival must be allocated");

            let Entry::Contiguous(allocation) = &entry else {
                panic!("a full-size image must occupy one whole layer");
            };

            layers.push((allocation.layer() as u32, fill_of(i)));

            // Every previously uploaded layer must still read back exactly.
            for (layer, fill) in &layers {
                let data = read_layer(&gpu, &atlas, *layer);
                let expected: Vec<u8> = (0..SIZE * SIZE).flat_map(|_| *fill).collect();

                assert_eq!(
                    data.len(),
                    expected.len(),
                    "layer {layer} read back at the wrong size after {i} arrivals"
                );
                assert!(
                    data == expected,
                    "layer {layer} lost its content after {i} arrivals \
                     (capacity {}, growths {})",
                    atlas.statistics().layers,
                    atlas.statistics().growths
                );
            }
        }

        // Zero control: the comparison above can only mean something if two
        // different fills actually differ in the read-back bytes.
        let a = read_layer(&gpu, &atlas, layers[0].0);
        let b = read_layer(&gpu, &atlas, layers[1].0);
        assert_ne!(
            a, b,
            "distinct fills read back identical; the probe is blind"
        );

        assert!(
            atlas.statistics().growths >= 3,
            "the test must actually cross growth boundaries, got {}",
            atlas.statistics().growths
        );
    }

    #[test]
    fn emptied_layers_are_reused_instead_of_growing() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let mut atlas = new_atlas(&gpu, budget_for(64));
        let mut entries = Vec::new();

        for i in 0..8 {
            entries
                .push(upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(i)).expect("arrival allocated"));
        }

        let settled = atlas.statistics();

        for entry in entries.drain(..) {
            atlas.remove(&entry);
        }

        assert_eq!(
            atlas.statistics().occupied_bytes,
            0,
            "removing every entry must leave nothing occupied"
        );

        for i in 0..8 {
            let _ = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(100 + i))
                .expect("re-arrival allocated");
        }

        let after = atlas.statistics();
        println!("reuse: settled {settled:?} after {after:?}");

        assert_eq!(
            after.growths, settled.growths,
            "refilling emptied layers must not reallocate the array"
        );
        assert_eq!(after.logical_layers, settled.logical_layers);
        assert_eq!(after.bytes_copied, settled.bytes_copied);
    }

    #[test]
    fn exhausting_the_budget_refuses_instead_of_erroring() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        // Room for three layers only.
        let mut atlas = new_atlas(&gpu, budget_for(3));

        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let mut entries = Vec::new();

        entries.push(upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(0)).expect("within the budget"));

        // A 2x2-page image needs four layers and only two are left: it must be
        // refused *and* must not keep the two pages it could have taken.
        assert!(
            upload(&gpu, &mut atlas, SIZE * 2, SIZE * 2, fill_of(1)).is_none(),
            "a fragmented image that cannot fit must be refused"
        );
        assert_eq!(
            atlas.statistics().occupied_bytes,
            layer_bytes(SIZE),
            "a partially allocated fragmented image must release its fragments"
        );

        for i in 1..3 {
            entries
                .push(upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(i)).expect("within the budget"));
        }

        assert!(
            upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(9)).is_none(),
            "an allocation past the budget must be refused"
        );

        // ...and the refusal must leave nothing dangling behind it.
        let stats = atlas.statistics();
        assert_eq!(stats.logical_layers, 3);
        assert!(stats.layers <= 3);
        assert_eq!(stats.occupied_bytes, 3 * layer_bytes(SIZE));

        // An image too large to ever fit is refused the same way.
        assert!(
            upload(&gpu, &mut atlas, SIZE * 4, SIZE * 4, fill_of(11)).is_none(),
            "an oversized image past the budget must be refused"
        );
        assert_eq!(
            atlas.statistics().occupied_bytes,
            3 * layer_bytes(SIZE),
            "a refused fragmented allocation must not leak the fragments it took"
        );

        // Freeing space makes the atlas usable again.
        atlas.remove(&entries.pop().expect("an entry"));
        assert!(
            upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(12)).is_some(),
            "a freed layer must be allocatable again"
        );

        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "hitting the budget produced a device error: {error:?}"
        );
    }

    #[test]
    fn fragmented_images_survive_growth_and_removal() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let mut atlas = new_atlas(&gpu, budget_for(64));

        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let fill = fill_of(3);
        let big = upload(&gpu, &mut atlas, SIZE * 2, SIZE * 2, fill).expect("fragmented upload");

        let Entry::Fragmented { fragments, size } = &big else {
            panic!("an image larger than one page must fragment");
        };

        assert_eq!(size.width, SIZE * 2);
        assert_eq!(fragments.len(), 4, "a 2x2-page image must take four pages");

        let occupied_with_big = atlas.statistics().occupied_bytes;
        let layers: Vec<u32> = fragments
            .iter()
            .map(|fragment| fragment.allocation.layer() as u32)
            .collect();

        // Force growth after the fragmented entry landed.
        for i in 0..12 {
            let _ = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(20 + i)).expect("arrival");
        }

        assert!(
            atlas.statistics().growths >= 2,
            "the test must cross a growth boundary"
        );

        let expected: Vec<u8> = (0..SIZE * SIZE).flat_map(|_| fill).collect();

        for layer in layers {
            assert!(
                read_layer(&gpu, &atlas, layer) == expected,
                "fragment layer {layer} lost its content across growth"
            );
        }

        let before_removal = atlas.statistics().occupied_bytes;
        atlas.remove(&big);

        assert_eq!(
            atlas.statistics().occupied_bytes,
            before_removal - 4 * layer_bytes(SIZE),
            "removing a fragmented entry must free exactly its own bytes"
        );
        assert_eq!(
            occupied_with_big,
            4 * layer_bytes(SIZE),
            "a 2x2-page image must occupy four pages"
        );

        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "fragmented growth errored: {error:?}");
    }

    #[test]
    fn the_bind_group_changes_only_when_the_texture_does() {
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let mut atlas = new_atlas(&gpu, budget_for(64));
        let mut previous = Arc::clone(atlas.bind_group());
        let mut changes = 0;

        for i in 0..32 {
            let _ = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(i)).expect("arrival");

            let current = Arc::clone(atlas.bind_group());

            if !Arc::ptr_eq(&previous, &current) {
                changes += 1;
                previous = current;
            }
        }

        let stats = atlas.statistics();
        println!("bind group changes: {changes}, {stats:?}");

        assert_eq!(
            changes, stats.growths,
            "the bind group must be replaced exactly once per growth: a change \
             without a growth is needless batch splitting, a growth without a \
             change is a stale texture binding"
        );
        assert!(
            changes <= 6,
            "32 arrivals split the image batch {changes} times"
        );
    }

    #[test]
    fn a_refused_fragmented_allocation_leaves_no_stale_layers_or_counters() {
        // enzUI-ICED #1513 (follow-up): the fragmented rollback path in
        // `allocate_inner` places some sub-fragments in freshly-pushed layers
        // before a later fragment is refused, and it deallocates them without
        // popping. `shrink_to_capacity` pops the trailing empties so
        // `logical_layers` does not exceed the physical `layers` the outgoing
        // texture actually has; the rollback loop uses `deallocate`, which
        // does *not* touch `occupied_bytes` or `removals`, because the failed
        // upload never bumped them.
        //
        // Gate A (shrink_to_capacity): emptying the body of
        // `shrink_to_capacity` reddens the `logical_layers <= layers` half.
        //
        // Gate B (counter preservation): making the rollback in the
        // fragmented branch touch `occupied_bytes` or `removals` — for
        // instance by replacing the `deallocate` call with a `remove`-style
        // path — reddens the counter half.
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        // Budget for three layers. On Vulkan the atlas starts with one empty
        // layer, so two full-size arrivals fill capacity 1 -> 2 and leave one
        // more layer of headroom — enough for the fragmented image to place
        // one sub-fragment (into a freshly-pushed third layer) before the
        // second is refused past `max_layers`.
        let mut atlas = new_atlas(&gpu, budget_for(3));

        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let first =
            upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(0)).expect("first arrival within budget");
        let second =
            upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(1)).expect("second arrival within budget");

        let settled = atlas.statistics();
        assert_eq!(
            settled.logical_layers, 2,
            "setup: two full-size arrivals must occupy two logical layers"
        );
        assert_eq!(settled.layers, 2, "setup: capacity grew to hold both");
        assert_eq!(settled.removals, 0, "setup: no removals yet");

        // A 2x2-page image needs four full layers. Only one layer of
        // headroom is left, so fragment 1 is placed in a freshly-pushed
        // layer 2 (logical_layers becomes 3) and fragment 2 is refused past
        // `max_layers`. The fragmented branch of `allocate_inner` then
        // deallocates fragment 1 (layer 2 back to Empty) and
        // `shrink_to_capacity` pops it, so `logical_layers` returns to 2.
        assert!(
            upload(&gpu, &mut atlas, SIZE * 2, SIZE * 2, fill_of(9)).is_none(),
            "a 2x2-page image cannot fit and must be refused"
        );

        let after = atlas.statistics();
        println!("after refusal: {after:?}");

        // Gate A: no logical layer may outlast the outgoing texture. If it
        // does, the next `grow` reads from a layer index the outgoing
        // texture never had (a real `TextureOverrun`, observed in the
        // pre-#1513 code and gated separately below).
        assert!(
            after.logical_layers <= after.layers,
            "a refused fragmented allocation left {} logical layer(s) backed \
             by a texture of only {} layer(s); `shrink_to_capacity` did not \
             pop the trailing empties",
            after.logical_layers,
            after.layers,
        );
        assert_eq!(
            after.logical_layers, 2,
            "the two successful arrivals must be the only logical layers left"
        );

        // Gate B: a refused upload never bumped `occupied_bytes` (it is
        // added *after* allocate succeeds, in `upload`) and never counted
        // as a removal (the rollback deallocates but is not a user-level
        // `remove`). If the rollback started calling `remove` instead of
        // the raw `deallocate`, it would decrement occupied_bytes into a
        // wrong low value and increment `removals` — reddening this half.
        assert_eq!(
            after.occupied_bytes, settled.occupied_bytes,
            "a refused upload changed occupied_bytes ({} -> {}); the rollback \
             accounted the failed upload as if it had succeeded",
            settled.occupied_bytes, after.occupied_bytes,
        );
        assert_eq!(
            after.removals, settled.removals,
            "a refused upload incremented removals ({} -> {}); the rollback \
             accounted the failed upload as a user-level removal",
            settled.removals, after.removals,
        );

        // And a validation-scoped device error must not have escaped along
        // the way — the whole point of refusing rather than uploading past
        // capacity.
        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "a refused fragmented allocation produced a device error: {error:?}"
        );

        atlas.remove(&first);
        atlas.remove(&second);
    }

    #[test]
    fn growth_after_a_refused_fragmented_allocation_never_reads_past_the_outgoing_texture() {
        // enzUI-ICED #1513 (follow-up): two guards form a **pair** for the
        // real `TextureOverrun` observed before the fix:
        //
        //   (A) `shrink_to_capacity` pops the trailing empty logical layers a
        //       refused fragmented allocation left behind, so `layers.len()`
        //       is back within `texture_layers` before the next upload.
        //   (B) `grow` clamps its copy count to `texture_layers` before
        //       reading, so even if `layers.len()` is somehow ahead of the
        //       outgoing texture, no read exceeds the outgoing depth.
        //
        // Each guard alone is enough to prevent the overrun; both together
        // are defence-in-depth. Removing **either** must keep the gate green,
        // and removing **both** must redden it (see the sabotage matrix
        // below). That calibration is what stops a redundant guard from
        // being silently dead code.
        //
        // Sabotage matrix, taken on the final tree:
        //   * (A) empty shrink_to_capacity  + (B) clamp intact    -> GREEN
        //   * (A) intact                    + (B) clamp removed   -> GREEN
        //   * (A) empty                     + (B) clamp removed   -> RED
        //     (validation: `copy_texture_to_texture` reads layer index
        //      `texture_layers`, which does not exist in the outgoing
        //      texture — the `TextureOverrun`.)
        let Some(gpu) = gpu() else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let mut atlas = new_atlas(&gpu, budget_for(3));
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

        // Two arrivals — capacity grows to 2, `texture_layers == 2`.
        let _first = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(0)).expect("first arrival");
        let _second = upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(1)).expect("second arrival");
        assert_eq!(atlas.statistics().layers, 2);

        // Refused 2x2 fragmented: fragment 1 pushes a third logical layer,
        // fragment 2 is refused past `max_layers`. Without (A) this leaves
        // `layers.len() == 3` behind an outgoing texture of depth 2.
        assert!(
            upload(&gpu, &mut atlas, SIZE * 2, SIZE * 2, fill_of(2)).is_none(),
            "the 2x2-page image cannot fit and must be refused"
        );

        // A third full-size arrival — this is the upload that would trigger
        // the overrunning growth if (A) had not popped the empty layer *and*
        // (B) had not clamped the copy count. On the healthy tree it grows
        // capacity 2 -> 3 and copies exactly 2 layers out of the outgoing
        // texture, which is well-defined.
        let _third =
            upload(&gpu, &mut atlas, SIZE, SIZE, fill_of(3)).expect("third arrival within budget");

        let stats = atlas.statistics();
        assert!(
            stats.growths >= 1,
            "the third arrival must have triggered a growth so the overrun \
             gate is actually exercising `grow`; got {} growths",
            stats.growths
        );
        assert_eq!(stats.logical_layers, 3);
        assert!(stats.layers >= 3);

        // No validation error must have surfaced — the whole gate.
        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "a growth after a refused fragmented allocation overran the \
             outgoing texture: {error:?}"
        );
    }

    #[test]
    fn gl_backend_grows_without_a_device_error() {
        let Some(gpu) = gpu_with(wgpu::Backends::GL) else {
            eprintln!("SKIP: no GL adapter available in this environment");
            return;
        };

        assert_eq!(gpu.backend, wgpu::Backend::Gl);

        let stats = run_sequential_growth(&gpu, 16);
        println!("GL: {stats:?}");

        assert!(
            !(stats.layers as u32).is_multiple_of(6),
            "GL capacity {} is a cubemap depth",
            stats.layers
        );
        assert!(stats.growths <= 5, "GL growths: {}", stats.growths);
    }
}
