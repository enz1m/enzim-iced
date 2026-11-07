// Modified by enzUI contributors. See FORK.md.
pub(crate) mod source;
use crate::core::svg;
use crate::core::{Color, Size};
use crate::image::atlas::{self, Atlas};
use crate::image::cache::{ImageCachePolicy, ImageCacheStatistics};

use resvg::tiny_skia;
use resvg::usvg;
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::panic;
use std::sync::Arc;

/// Entry in cache corresponding to an svg handle
pub enum Svg {
    /// Parsed svg
    Loaded(Arc<usvg::Tree>),
    /// Svg not found or failed to parse
    NotFound,
}

impl Svg {
    /// Viewport width and height
    pub fn viewport_dimensions(&self) -> Size<u32> {
        match self {
            Svg::Loaded(tree) => {
                let size = tree.size();

                Size::new(size.width() as u32, size.height() as u32)
            }
            Svg::NotFound => Size::new(1, 1),
        }
    }
}

/// Caches svg vector and raster data
#[derive(Debug, Default)]
pub struct Cache {
    svgs: FxHashMap<u64, Svg>,
    svg_bytes: FxHashMap<u64, u64>,
    rasterized: FxHashMap<(u64, u32, u32, ColorFilter), atlas::Entry>,
    svg_last_used: FxHashMap<u64, u64>,
    rasterized_last_used: FxHashMap<(u64, u32, u32, ColorFilter), u64>,
    /// Rasters of SVGs the render lane cannot rasterize itself, by size and
    /// tint, with the frame each was last asked for.
    #[cfg(feature = "image")]
    frozen:
        FxHashMap<(u64, u32, u32, ColorFilter), (Arc<crate::image::raster::source::Source>, u64)>,
    frame: u64,
    uploads: u64,
    evictions: u64,
    #[cfg(feature = "svg-text")]
    fontdb: Option<Arc<usvg::fontdb::Database>>,
}

type ColorFilter = Option<[u8; 4]>;

impl Cache {
    pub(crate) fn identity(&self, handle: &svg::Handle) -> Option<usize> {
        match self.svgs.get(&handle.id()) {
            Some(Svg::Loaded(tree)) => Some(Arc::as_ptr(tree) as usize),
            _ => None,
        }
    }
    pub(crate) fn source(&mut self, handle: &svg::Handle) -> Option<Arc<source::Source>> {
        let tree = match self.load(handle) {
            Svg::Loaded(tree) => tree.clone(),
            Svg::NotFound => return None,
        };
        let encoded_bytes = self.svg_bytes.get(&handle.id()).copied().unwrap_or(0);
        source::capture(handle, &tree, encoded_bytes)
    }
    pub(crate) fn install_source(&mut self, source: &source::Source, atlas: &mut Atlas) {
        let id = source.handle.id();
        let _ = self.svg_last_used.insert(id, self.frame);
        if matches!(self.svgs.get(&id),Some(Svg::Loaded(tree)) if Arc::ptr_eq(tree,&source.tree)) {
            return;
        }
        let keys: Vec<_> = self
            .rasterized
            .keys()
            .filter(|key| key.0 == id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(entry) = self.rasterized.remove(&key) {
                atlas.remove(&entry);
            }
            let _ = self.rasterized_last_used.remove(&key);
        }
        let _ = self.svgs.insert(id, Svg::Loaded(source.tree.clone()));
        let _ = self.svg_bytes.insert(id, source.encoded_bytes);
    }

    /// Load svg
    pub fn load(&mut self, handle: &svg::Handle) -> &Svg {
        let id = handle.id();
        let _ = self.svg_last_used.insert(id, self.frame);

        if self.svgs.contains_key(&id) {
            return self.svgs.get(&id).unwrap();
        }

        // TODO: Reuse `cosmic-text` font database
        #[cfg(feature = "svg-text")]
        if self.fontdb.is_none() {
            let mut fontdb = usvg::fontdb::Database::new();
            fontdb.load_system_fonts();

            self.fontdb = Some(Arc::new(fontdb));
        }

        let options = usvg::Options {
            #[cfg(feature = "svg-text")]
            fontdb: self
                .fontdb
                .as_ref()
                .expect("fontdb must be initialized")
                .clone(),
            ..usvg::Options::default()
        };

        let (svg, source_bytes) = match handle.data() {
            svg::Data::Path(path) => {
                let source_bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
                let svg = fs::read_to_string(path)
                    .ok()
                    .and_then(|contents| usvg::Tree::from_str(&contents, &options).ok())
                    .map(|tree| Svg::Loaded(Arc::new(tree)))
                    .unwrap_or(Svg::NotFound);

                (svg, source_bytes)
            }
            svg::Data::Bytes(bytes) => (
                match usvg::Tree::from_data(bytes, &options) {
                    Ok(tree) => Svg::Loaded(Arc::new(tree)),
                    Err(_) => Svg::NotFound,
                },
                bytes.len() as u64,
            ),
        };

        let _ = self.svg_bytes.insert(id, source_bytes);
        let _ = self.svgs.insert(id, svg);
        self.svgs.get(&id).unwrap()
    }

    pub fn mark_used(&mut self, handle: &svg::Handle, color: Option<Color>, size: Size<u32>) {
        let id = handle.id();
        if self.svgs.contains_key(&id) {
            let _ = self.svg_last_used.insert(id, self.frame);
        }
        let key = (id, size.width, size.height, color.map(Color::into_rgba8));
        if self.rasterized.contains_key(&key) {
            let _ = self.rasterized_last_used.insert(key, self.frame);
        }
        #[cfg(feature = "image")]
        if let Some((_, used)) = self.frozen.get_mut(&key) {
            *used = self.frame;
        }
    }

    /// Load svg and upload raster data
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        handle: &svg::Handle,
        color: Option<Color>,
        size: Size<u32>,
        atlas: &mut Atlas,
    ) -> Option<&atlas::Entry> {
        let id = handle.id();

        let color = color.map(Color::into_rgba8);
        let key = (id, size.width, size.height, color);
        let _ = self.svg_last_used.insert(id, self.frame);
        let _ = self.rasterized_last_used.insert(key, self.frame);

        // TODO: Optimize!
        // We currently rerasterize the SVG when its size changes. This is slow
        // as heck. A GPU rasterizer like `pathfinder` may perform better.
        // It would be cool to be able to smooth resize the `svg` example.
        if self.rasterized.contains_key(&key) {
            return self.rasterized.get(&key);
        }

        match self.load(handle) {
            Svg::Loaded(tree) => {
                let rgba = rasterize(handle, tree, size, color)?;

                let allocation =
                    atlas.upload(device, encoder, belt, size.width, size.height, &rgba)?;

                log::debug!("allocating {id} {}x{}", size.width, size.height);

                let _ = self.rasterized.insert(key, allocation);
                self.uploads = self.uploads.saturating_add(1);

                self.rasterized.get(&key)
            }
            Svg::NotFound => None,
        }
    }

    /// A raster of `handle` at `size`, tinted by `color`, for a recording the
    /// render lane replays: an SVG it cannot rasterize itself (filters,
    /// patterns, text, images) is drawn there as this raster.
    #[cfg(feature = "image")]
    pub(crate) fn freeze(
        &mut self,
        handle: &svg::Handle,
        color: Option<Color>,
        size: Size<u32>,
        budget: &crate::image::raster::source::Budget,
    ) -> Option<Arc<crate::image::raster::source::Source>> {
        let color = color.map(Color::into_rgba8);
        let key = (handle.id(), size.width, size.height, color);
        let frame = self.frame;
        if let Some((source, used)) = self.frozen.get_mut(&key) {
            *used = frame;
            return Some(Arc::clone(source));
        }
        let Svg::Loaded(tree) = self.load(handle) else {
            return None;
        };
        let tree = Arc::clone(tree);
        let rgba = rasterize(handle, &tree, size, color)?;
        let source = budget.capture(size.width, size.height, &rgba)?;
        let _ = self.frozen.insert(key, (Arc::clone(&source), frame));
        Some(source)
    }

    /// Advance the cache epoch and evict entries past the grace policy.
    pub fn trim(&mut self, frame: u64, policy: ImageCachePolicy, atlas: &mut Atlas) {
        self.frame = frame;
        #[cfg(feature = "image")]
        self.frozen
            .retain(|_, (_, used)| frame.saturating_sub(*used) <= policy.grace_frames);

        let mut raster_removals = FxHashSet::default();
        let mut stale = self
            .rasterized
            .iter()
            .filter_map(|(key, entry)| {
                let age =
                    frame.saturating_sub(*self.rasterized_last_used.get(key).unwrap_or(&frame));

                if age == 0 {
                    return None;
                }

                if age > policy.grace_frames {
                    let _ = raster_removals.insert(*key);
                }

                let size = entry.size();
                Some((
                    *key,
                    age,
                    u64::from(size.width) * u64::from(size.height) * 4,
                ))
            })
            .collect::<Vec<_>>();

        let mut retained_stale_bytes = stale
            .iter()
            .filter(|(key, _, _)| !raster_removals.contains(key))
            .map(|(_, _, bytes)| *bytes)
            .sum::<u64>();

        if retained_stale_bytes > policy.stale_byte_budget {
            stale.sort_unstable_by_key(|(_, age, _)| std::cmp::Reverse(*age));

            for (key, _, bytes) in stale {
                if retained_stale_bytes <= policy.stale_byte_budget {
                    break;
                }

                if raster_removals.insert(key) {
                    retained_stale_bytes = retained_stale_bytes.saturating_sub(bytes);
                }
            }
        }

        for key in raster_removals {
            if let Some(entry) = self.rasterized.remove(&key) {
                atlas.remove(&entry);
                self.evictions = self.evictions.saturating_add(1);
            }
            let _ = self.rasterized_last_used.remove(&key);
        }

        let svg_removals = self
            .svgs
            .keys()
            .copied()
            .filter(|id| {
                frame.saturating_sub(*self.svg_last_used.get(id).unwrap_or(&frame))
                    > policy.grace_frames
            })
            .collect::<Vec<_>>();

        for id in svg_removals {
            let _ = self.svgs.remove(&id);
            let _ = self.svg_bytes.remove(&id);
            let _ = self.svg_last_used.remove(&id);
            self.evictions = self.evictions.saturating_add(1);
        }

        self.frame = frame.saturating_add(1);
    }

    pub fn add_statistics(&self, statistics: &mut ImageCacheStatistics) {
        let raster_bytes = self
            .rasterized
            .values()
            .map(|entry| {
                let size = entry.size();
                u64::from(size.width) * u64::from(size.height) * 4
            })
            .sum::<u64>();
        let source_bytes = self.svg_bytes.values().sum::<u64>();

        statistics.svg_entries = self.svgs.len();
        statistics.svg_raster_entries = self.rasterized.len();
        statistics.svg_bytes = source_bytes.saturating_add(raster_bytes);
        statistics.svg_stale_entries = self
            .svg_last_used
            .values()
            .filter(|last| last.saturating_add(1) < self.frame)
            .count()
            + self
                .rasterized_last_used
                .values()
                .filter(|last| last.saturating_add(1) < self.frame)
                .count();
        statistics.svg_stale_bytes = self
            .svgs
            .keys()
            .filter(|id| {
                self.svg_last_used
                    .get(id)
                    .is_some_and(|last| last.saturating_add(1) < self.frame)
            })
            .map(|id| self.svg_bytes.get(id).copied().unwrap_or(0))
            .sum::<u64>()
            + self
                .rasterized
                .iter()
                .filter(|(key, _)| {
                    self.rasterized_last_used
                        .get(key)
                        .is_some_and(|last| last.saturating_add(1) < self.frame)
                })
                .map(|(_, entry)| {
                    let size = entry.size();
                    u64::from(size.width) * u64::from(size.height) * 4
                })
                .sum::<u64>();
        statistics.svg_uploads = self.uploads;
        statistics.svg_evictions = self.evictions;
    }
}

impl std::fmt::Debug for Svg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Svg::Loaded(_) => write!(f, "Svg::Loaded"),
            Svg::NotFound => write!(f, "Svg::NotFound"),
        }
    }
}

/// Rasterizes `tree` into straight-alpha RGBA at `size`, tinted by `color`.
fn rasterize(
    handle: &svg::Handle,
    tree: &usvg::Tree,
    size: Size<u32>,
    color: ColorFilter,
) -> Option<Vec<u8>> {
    // TODO: Optimize!
    // We currently rerasterize the SVG when its size changes. This is slow
    // as heck. A GPU rasterizer like `pathfinder` may perform better.
    // It would be cool to be able to smooth resize the `svg` example.
    let mut img = tiny_skia::Pixmap::new(size.width, size.height)?;

    let tree_size = tree.size().to_int_size();

    let target_size = if size.width > size.height {
        tree_size.scale_to_height(size.height)
    } else {
        tree_size.scale_to_width(size.width)
    };

    let transform = if let Some(target_size) = target_size {
        let tree_size = tree_size.to_size();
        let target_size = target_size.to_size();

        tiny_skia::Transform::from_scale(
            target_size.width() / tree_size.width(),
            target_size.height() / tree_size.height(),
        )
    } else {
        tiny_skia::Transform::default()
    };

    // SVG rendering can panic on malformed or complex vectors.
    // We catch panics to prevent crashes and continue gracefully.
    let render = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        resvg::render(tree, transform, &mut img.as_mut());
    }));

    if let Err(error) = render {
        log::warn!("SVG rendering for {handle:?} panicked: {error:?}");
    }

    let mut rgba = img.take();

    // tiny-skia rasterises premultiplied, and the image shader premultiplies every
    // sample itself, so the atlas holds straight alpha.
    if color.is_none() {
        let (chunks, _) = rgba.as_chunks_mut::<4>();
        for [r, g, b, a] in chunks {
            let alpha = u32::from(*a);
            if alpha != 0 && alpha != 255 {
                for channel in [r, g, b] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }

    if let Some(color) = color {
        let (chunks, _) = rgba.as_chunks_mut::<4>();

        for [r, g, b, a] in chunks {
            if *a > 0 {
                *r = color[0];
                *g = color[1];
                *b = color[2];
                // Honour the tint's own alpha: fold it into the
                // coverage mask (the source glyph alpha) so a
                // translucent tint fades the stroke, matching how
                // `text` treats a translucent colour. `rgba` here is
                // straight (non-premultiplied) RGBA8, so scaling the
                // alpha alone is correct. `color[3] == 255` (opaque)
                // is a no-op. This composes multiplicatively with the
                // separate `Svg::opacity` layer, which is applied in
                // the image shader, not here.
                *a = (u16::from(*a) * u16::from(color[3]) / 255) as u8;
            }
        }
    }
    Some(rgba)
}
