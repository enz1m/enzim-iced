// Modified by enzUI contributors. See FORK.md.
use crate::core::image as raster;
use crate::core::{Rectangle, Size};
use crate::graphics;

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(Cache::default()),
        }
    }

    pub fn load(&self, handle: &raster::Handle) -> Result<raster::Allocation, raster::Error> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle)?;

        #[allow(unsafe_code)]
        Ok(unsafe { raster::allocate(handle, Size::new(image.width(), image.height())) })
    }

    pub fn dimensions(&self, handle: &raster::Handle) -> Option<Size<u32>> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle).ok()?;

        Some(Size::new(image.width(), image.height()))
    }

    pub fn draw(
        &mut self,
        handle: &raster::Handle,
        filter_method: raster::FilterMethod,
        bounds: Rectangle,
        opacity: f32,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        transform: tiny_skia::Transform,
        clip_mask: Option<&tiny_skia::Mask>,
    ) {
        let mut cache = self.cache.borrow_mut();

        let Ok(image) = cache.allocate(handle) else {
            return;
        };

        let width_scale = bounds.width / image.width() as f32;
        let height_scale = bounds.height / image.height() as f32;

        let transform = transform.pre_scale(width_scale, height_scale);

        let quality = match filter_method {
            raster::FilterMethod::Linear => tiny_skia::FilterQuality::Bilinear,
            raster::FilterMethod::Nearest => tiny_skia::FilterQuality::Nearest,
        };

        pixels.draw_pixmap(
            (bounds.x / width_scale) as i32,
            (bounds.y / height_scale) as i32,
            image,
            &tiny_skia::PixmapPaint {
                quality,
                opacity,
                ..Default::default()
            },
            transform,
            clip_mask,
        );
    }

    pub fn trim_cache(&mut self) {
        self.cache.borrow_mut().trim();
    }
}

#[derive(Debug, Default)]
struct Cache {
    // A cached `Err` remembers that this handle is known-bad: it is decoded at
    // most once and every later `allocate` replays the **same** error instead
    // of re-running `graphics::image::load` or (as before) panicking. See the
    // `known_bad_handle_*` tests below for why the second call is what matters.
    entries: FxHashMap<raster::Id, Result<Entry, raster::Error>>,
    hits: FxHashSet<raster::Id>,
}

impl Cache {
    pub fn allocate(
        &mut self,
        handle: &raster::Handle,
    ) -> Result<tiny_skia::PixmapRef<'_>, raster::Error> {
        let id = handle.id();

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(id) {
            let result = (|| {
                let image = graphics::image::load(handle)?;

                if image.width() == 0 || image.height() == 0 {
                    return Err(raster::Error::Empty);
                }

                let mut buffer = vec![0u32; image.width() as usize * image.height() as usize];

                for (i, pixel) in image.pixels().enumerate() {
                    let [r, g, b, a] = pixel.0;

                    buffer[i] =
                        bytemuck::cast(tiny_skia::ColorU8::from_rgba(b, g, r, a).premultiply());
                }

                Ok(Entry {
                    width: image.width(),
                    height: image.height(),
                    pixels: buffer,
                })
            })();

            let _ = entry.insert(result);
        }

        let _ = self.hits.insert(id);

        // The entry is present (we just inserted it if it was vacant). A cached
        // `Err` is replayed verbatim; only a cached `Ok` builds a pixmap. This
        // is the line that used to `.expect()` a stored `None` and panic on the
        // second frame for a bad handle.
        match self.entries.get(&id).unwrap() {
            Ok(entry) => Ok(tiny_skia::PixmapRef::from_bytes(
                bytemuck::cast_slice(&entry.pixels),
                entry.width,
                entry.height,
            )
            .expect("Build pixmap from image bytes")),
            Err(error) => Err(error.clone()),
        }
    }

    fn trim(&mut self) {
        self.entries.retain(|key, _| self.hits.contains(key));
        self.hits.clear();
    }
}

#[derive(Debug)]
struct Entry {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

#[cfg(all(test, feature = "image"))]
mod tests {
    use super::*;
    use crate::core::Bytes;
    use crate::core::image::Handle;

    // A 2x2 RGBA handle. `graphics::image::load` accepts a `Handle::Rgba`
    // without going through any image-format decoder, so this is the cheapest
    // *valid* handle: it exercises the success path of `allocate` without
    // depending on the `image` crate's format support.
    fn valid_handle() -> Handle {
        // Distinct per-pixel colours so a re-decode would be observable.
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 0, 255, // yellow
        ];
        Handle::from_rgba(2, 2, Bytes::from(pixels))
    }

    // A path to a file that does not exist -> `graphics::image::load` fails with
    // `Error::Inaccessible`. This is the "missing file" case from the issue.
    fn missing_path_handle() -> Handle {
        Handle::from_path("/nonexistent/enzui-iced-raster-643-does-not-exist.png")
    }

    // Bytes that are not a decodable image -> `Error::Invalid`. This is the
    // "truncated download / unsupported format" case from the issue.
    fn garbage_bytes_handle() -> Handle {
        Handle::from_bytes(Bytes::from(vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05]))
    }

    // A 0x0 RGBA handle decodes to an empty image -> `Error::Empty`. In the old
    // code this branch returned *without* caching, so it never panicked but
    // re-decoded on every frame; it now caches like any other error.
    fn empty_handle() -> Handle {
        Handle::from_rgba(0, 0, Bytes::from(Vec::<u8>::new()))
    }

    // --- Positive control: written first. A valid handle must keep working on
    // the second call and return the *same* pixmap. If this reddens, the fix
    // has become a blanket "always Err" that disables image caching. ---
    #[test]
    fn valid_handle_allocates_on_both_calls() {
        let mut cache = Cache::default();
        let handle = valid_handle();

        let (w1, h1, bytes1) = {
            let pixmap = cache
                .allocate(&handle)
                .expect("first allocate of a valid handle must succeed");
            (pixmap.width(), pixmap.height(), pixmap.data().to_vec())
        };
        assert_eq!((w1, h1), (2, 2));

        // Absolute anchor: the decoded pixels must be the real image, not just
        // *some* stable buffer. tiny_skia stores premultiplied BGRA; every
        // pixel here is fully opaque (a = 255) so premultiply is identity and
        // RGBA (r,g,b) maps to bytes [b, g, r, 255]. Red -> [0,0,255,255].
        assert_eq!(
            &bytes1[..4],
            &[0u8, 0, 255, 255],
            "first pixel must decode to premultiplied BGRA red"
        );

        let (w2, h2, bytes2) = {
            let pixmap = cache
                .allocate(&handle)
                .expect("second allocate of a valid handle must ALSO succeed");
            (pixmap.width(), pixmap.height(), pixmap.data().to_vec())
        };

        assert_eq!((w1, h1), (w2, h2), "dimensions must be stable across calls");
        assert_eq!(
            bytes1, bytes2,
            "the second call must return the same pixmap bytes"
        );
    }

    // --- Mechanism observation: the second successful call must NOT re-decode.
    // We poison the cached entry's pixel buffer with a sentinel after the first
    // call; a re-decode would overwrite it with the real pixels, a cache hit
    // leaves the sentinel intact. This observes the cache mechanism directly
    // rather than timing anything. ---
    #[test]
    fn valid_handle_second_call_does_not_re_decode() {
        let mut cache = Cache::default();
        let handle = valid_handle();
        let id = handle.id();

        // First call populates the cache.
        assert!(cache.allocate(&handle).is_ok());

        // Poison the cached buffer in place. If the second call re-runs
        // `graphics::image::load`, this sentinel is destroyed.
        const SENTINEL: u32 = 0xDEAD_BEEF;
        match cache.entries.get_mut(&id).expect("entry must be cached") {
            Ok(entry) => {
                for p in entry.pixels.iter_mut() {
                    *p = SENTINEL;
                }
            }
            Err(_) => panic!("valid handle must cache an Ok entry"),
        }

        // Second call: must be served from the cache, so the sentinel survives.
        let data = {
            let pixmap = cache
                .allocate(&handle)
                .expect("second allocate must succeed");
            pixmap.data().to_vec()
        };

        let expected: Vec<u8> = std::iter::repeat_n(SENTINEL, 4)
            .flat_map(u32::to_ne_bytes)
            .collect();
        assert_eq!(
            data, expected,
            "second call re-decoded the image instead of using the cache"
        );
    }

    // --- The bug: a bad handle must return `Err` on the SECOND call, not
    // panic. A single-call test passes against the broken code, so BOTH calls
    // are asserted. Covers the missing-file (`Inaccessible`) path. ---
    #[test]
    fn known_bad_handle_missing_file_errs_on_both_calls() {
        let mut cache = Cache::default();
        let handle = missing_path_handle();

        let first = cache.allocate(&handle);
        assert!(
            matches!(first, Err(raster::Error::Inaccessible(_))),
            "first call must return Inaccessible, got {first:?}"
        );

        // This is the frame that used to panic at `.expect("Image should be
        // allocated")`. It must return the SAME error instead.
        let second = cache.allocate(&handle);
        assert!(
            matches!(second, Err(raster::Error::Inaccessible(_))),
            "second call must ALSO return Inaccessible (not panic), got {second:?}"
        );
    }

    // --- Covers the undecodable-bytes (`Invalid`) path, likewise twice. ---
    #[test]
    fn known_bad_handle_garbage_bytes_errs_on_both_calls() {
        let mut cache = Cache::default();
        let handle = garbage_bytes_handle();

        let first = cache.allocate(&handle);
        assert!(
            matches!(first, Err(raster::Error::Invalid(_))),
            "first call must return Invalid, got {first:?}"
        );

        let second = cache.allocate(&handle);
        assert!(
            matches!(second, Err(raster::Error::Invalid(_))),
            "second call must ALSO return Invalid (not panic), got {second:?}"
        );
    }

    // --- The `Error::Empty` path (:105 in the original). It never panicked on
    // the second call, but it was also never cached, so it re-decoded every
    // frame. The refactor routes it through the same cached-`Err` machinery, so
    // both calls now return `Empty` and (like the other errors) decode once. ---
    #[test]
    fn empty_handle_errs_on_both_calls_and_is_cached() {
        let mut cache = Cache::default();
        let handle = empty_handle();
        let id = handle.id();

        let first = cache.allocate(&handle);
        assert!(
            matches!(first, Err(raster::Error::Empty)),
            "first call must return Empty, got {first:?}"
        );

        // The failure is now remembered in the cache (it was not before).
        assert!(
            matches!(cache.entries.get(&id), Some(Err(raster::Error::Empty))),
            "an Empty result must be cached so it is not re-decoded every frame"
        );

        let second = cache.allocate(&handle);
        assert!(
            matches!(second, Err(raster::Error::Empty)),
            "second call must ALSO return Empty, got {second:?}"
        );
    }
}
