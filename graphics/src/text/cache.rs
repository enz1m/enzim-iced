// Modified by enzUI contributors. See FORK.md.
//! Cache text.
use crate::core::{Font, Size};
use crate::text;

use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::hash::{Hash, Hasher};

/// A store of recently used sections of text.
#[derive(Debug, Default)]
pub struct Cache {
    entries: FxHashMap<KeyHash, Entry>,
    aliases: FxHashMap<KeyHash, KeyHash>,
    layouts: FxHashMap<KeyHash, KeyHash>,
    recently_used: FxHashSet<KeyHash>,
}

impl Cache {
    /// Creates a new empty [`Cache`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Gets the text [`Entry`] with the given [`KeyHash`].
    pub fn get(&self, key: &KeyHash) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// Allocates a text [`Entry`] if it is not already present in the [`Cache`].
    ///
    /// A miss that differs from a cached entry only in its bounds lays out
    /// that entry's shaped text again instead of shaping it anew.
    pub fn allocate(
        &mut self,
        font_system: &mut cosmic_text::FontSystem,
        key: Key<'_>,
    ) -> (KeyHash, &mut Entry) {
        let hash = key.hash(FxHasher::default());

        if let Some(&alias) = self.aliases.get(&hash)
            && self.entries.contains_key(&alias)
        {
            let _ = self.recently_used.insert(alias);

            return (alias, self.entries.get_mut(&alias).unwrap());
        }

        if !self.entries.contains_key(&hash) {
            let layout = key.layout_hash(FxHasher::default());
            let max_height = key.bounds.height.max(key.line_height);

            let mut buffer = match self.shaped_buffer(layout) {
                Some(mut buffer) => {
                    for line in &mut buffer.lines {
                        let _ = line.set_align(None);
                    }
                    buffer
                }
                None => {
                    let metrics =
                        cosmic_text::Metrics::new(key.size, key.line_height.max(f32::MIN_POSITIVE));
                    let mut buffer = cosmic_text::Buffer::new(font_system, metrics);
                    buffer.set_wrap(text::to_wrap(key.wrapping));
                    buffer.set_text(
                        key.content,
                        &text::to_attributes(key.font),
                        text::to_shaping_for(key.shaping, key.content, key.font),
                        None,
                    );
                    buffer
                }
            };

            buffer.set_size(Some(key.bounds.width), Some(max_height));
            buffer.set_ellipsize(text::to_ellipsize(key.ellipsis, max_height));
            buffer.shape_until_scroll(font_system, false);

            let bounds = text::align(&mut buffer, font_system, key.align_x);

            let _ = self.entries.insert(
                hash,
                Entry {
                    buffer,
                    min_bounds: bounds,
                },
            );
            let _ = self.layouts.insert(layout, hash);

            for bounds in [
                bounds,
                Size {
                    width: key.bounds.width,
                    ..bounds
                },
            ] {
                if key.bounds != bounds {
                    let _ = self
                        .aliases
                        .insert(Key { bounds, ..key }.hash(FxHasher::default()), hash);
                }
            }
        }

        let _ = self.recently_used.insert(hash);

        (hash, self.entries.get_mut(&hash).unwrap())
    }

    /// Takes the buffer of the latest entry with the given layout hash, or
    /// copies it if that entry was used since the last trim, since its hash
    /// may still be looked up.
    fn shaped_buffer(&mut self, layout: KeyHash) -> Option<cosmic_text::Buffer> {
        let source = *self.layouts.get(&layout)?;

        if self.recently_used.contains(&source) {
            self.entries.get(&source).map(|entry| entry.buffer.clone())
        } else {
            self.entries.remove(&source).map(|entry| entry.buffer)
        }
    }

    /// Trims the [`Cache`].
    ///
    /// This will clear the sections of text that have not been used since the last `trim`.
    pub fn trim(&mut self) {
        self.entries
            .retain(|key, _| self.recently_used.contains(key));

        self.aliases
            .retain(|_, value| self.recently_used.contains(value));

        self.layouts
            .retain(|_, value| self.recently_used.contains(value));

        self.recently_used.clear();
    }
}

/// A cache key representing a section of text.
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    /// The content of the text.
    pub content: &'a str,
    /// The size of the text.
    pub size: f32,
    /// The line height of the text.
    pub line_height: f32,
    /// The [`Font`] of the text.
    pub font: Font,
    /// The bounds of the text.
    pub bounds: Size,
    /// The shaping strategy of the text.
    pub shaping: text::Shaping,
    /// The alignment of the text.
    pub align_x: text::Alignment,
    /// The wrapping strategy of the text.
    pub wrapping: text::Wrapping,
    /// The ellipsis strategy of the text.
    pub ellipsis: text::Ellipsis,
}

impl Key<'_> {
    fn hash<H: Hasher>(self, mut hasher: H) -> KeyHash {
        self.bounds.width.to_bits().hash(&mut hasher);
        self.bounds.height.to_bits().hash(&mut hasher);

        self.layout_hash(hasher)
    }

    /// Hashes everything but the bounds, which only change the layout.
    fn layout_hash<H: Hasher>(self, mut hasher: H) -> KeyHash {
        self.content.hash(&mut hasher);
        self.size.to_bits().hash(&mut hasher);
        self.line_height.to_bits().hash(&mut hasher);
        self.font.hash(&mut hasher);
        self.shaping.hash(&mut hasher);
        self.align_x.hash(&mut hasher);
        self.wrapping.hash(&mut hasher);
        self.ellipsis.hash(&mut hasher);

        hasher.finish()
    }
}

/// The hash of a [`Key`].
pub type KeyHash = u64;

/// A cache entry.
#[derive(Debug)]
pub struct Entry {
    /// The buffer of text, ready for drawing.
    pub buffer: cosmic_text::Buffer,
    /// The minimum bounds of the text.
    pub min_bounds: Size,
}

#[cfg(test)]
mod tests {
    use super::{Cache, Key};
    use crate::core::text::{Alignment, Ellipsis, Shaping, Wrapping};
    use crate::core::{Font, Size};

    const TEXT: &str = "The quick brown fox jumps over the lazy dog";
    const BOUNDS: Size = Size::new(120.0, 200.0);

    fn fonts() -> cosmic_text::FontSystem {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(include_bytes!("../../fonts/FiraSans-Regular.ttf").to_vec());
        db.set_sans_serif_family("Fira Sans");
        cosmic_text::FontSystem::new_with_locale_and_db("en-US".into(), db)
    }

    fn key(
        bounds: Size,
        align_x: Alignment,
        wrapping: Wrapping,
        ellipsis: Ellipsis,
    ) -> Key<'static> {
        Key {
            content: TEXT,
            size: 16.0,
            line_height: 20.0,
            font: Font::DEFAULT,
            bounds,
            shaping: Shaping::Advanced,
            align_x,
            wrapping,
            ellipsis,
        }
    }

    fn layout(
        cache: &mut Cache,
        fonts: &mut cosmic_text::FontSystem,
        key: Key<'_>,
    ) -> (Size, Vec<(usize, u32, Vec<(usize, u16, u32)>)>) {
        let (_, entry) = cache.allocate(fonts, key);
        let runs = entry
            .buffer
            .layout_runs()
            .map(|run| {
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|glyph| (glyph.start, glyph.glyph_id, glyph.x.to_bits()))
                    .collect();
                (run.line_i, run.line_w.to_bits(), glyphs)
            })
            .collect();
        (entry.min_bounds, runs)
    }

    #[test]
    fn wrapping_and_ellipsis_select_distinct_layouts() {
        let mut fonts = fonts();
        let mut cache = Cache::new();

        let wrapped = layout(
            &mut cache,
            &mut fonts,
            key(BOUNDS, Alignment::Left, Wrapping::Word, Ellipsis::None),
        );
        assert!(wrapped.1.len() > 1);

        for (wrapping, ellipsis) in [
            (Wrapping::None, Ellipsis::None),
            (Wrapping::None, Ellipsis::End),
        ] {
            // The measured bounds of the wrapped text are also one of its aliases.
            for bounds in [BOUNDS, wrapped.0] {
                let key = key(bounds, Alignment::Left, wrapping, ellipsis);
                let alone = layout(&mut Cache::new(), &mut fonts, key);
                let shared = layout(&mut cache, &mut fonts, key);

                assert_eq!(shared, alone, "{wrapping:?} {ellipsis:?} in {bounds:?}");
                assert_ne!(shared, wrapped, "{wrapping:?} {ellipsis:?} in {bounds:?}");
            }
        }
    }

    #[test]
    fn bounds_changes_match_a_fresh_layout() {
        let mut fonts = fonts();

        for align_x in [Alignment::Left, Alignment::Center, Alignment::Right] {
            for ellipsis in [Ellipsis::None, Ellipsis::End] {
                let mut cache = Cache::new();

                // The second width copies an entry in use; later ones take it after a trim.
                for (i, width) in [400.0, 120.0, 90.0, 260.0, 120.0].into_iter().enumerate() {
                    let key = key(Size::new(width, 200.0), align_x, Wrapping::Word, ellipsis);
                    let expected = layout(&mut Cache::new(), &mut fonts, key);

                    assert_eq!(layout(&mut cache, &mut fonts, key), expected, "{width}");

                    if i > 0 {
                        cache.trim();
                    }
                }
            }
        }
    }
}
