// Modified by enzUI contributors. See FORK.md.
//! Closed, byte-owned font resources for bounded text that can reflow after capture.
use super::cosmic_text::{self, Buffer, BufferLine, FontSystem, ShapeLine, fontdb};
use crate::core::renderer::presentation::Error;
use crate::resource::{Permit, font_admission};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::sync::Arc;

/// Maximum copied font payload for one closed font revision.
pub const MAX_FONT_BYTES: usize = 128 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_GLYPHS: usize = 1024 * 1024;
const MAX_LINES: usize = 16_384;
const MAX_FACES: usize = 256;

/// Borrowed source lines whose complete shaping, not just visible glyphs, must remain available.
#[derive(Default)]
pub struct FontEnvelope<'a> {
    lines: Vec<(&'a BufferLine, u16)>,
    bytes: usize,
    cached_glyphs: usize,
}

struct Face {
    original: fontdb::FaceInfo,
    // Where the bytes were copied from, so an unchanged source is reused
    // without copying it again.
    source: SourceKey,
    bytes: Arc<Vec<u8>>,
}

/// A font database containing only owned bytes and validated source font choices.
pub struct FontSnapshot {
    database: fontdb::Database,
    faces: Vec<Face>,
    ids: BTreeMap<fontdb::ID, fontdb::ID>,
    bytes: usize,
    retained_bytes: usize,
    aliases: [String; 5],
    locale: String,
    // The owned shaping state `capture` validates against, kept warm across
    // captures that reuse this snapshot: a fresh one re-parses every font
    // and rebuilds every font-match list.
    validator: std::sync::Mutex<Option<FontSystem>>,
    // The lines validated against this snapshot, by `line_key`, and the faces
    // each shapes with: a capture whose lines are all here, over an unchanged
    // database, needs no shaping.
    validated: std::sync::Mutex<std::collections::HashMap<u64, Vec<fontdb::ID>>>,
    // Last so usage stays charged while the database and copied bytes are destroyed.
    _permit: Permit,
}

impl std::fmt::Debug for FontSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSnapshot")
            .field("faces", &self.faces.len())
            .field("bytes", &self.bytes)
            .field("retained_bytes", &self.retained_bytes)
            .finish()
    }
}

impl<'a> FontEnvelope<'a> {
    /// Adds all logical lines, refusing more than 4 MiB of text/attributes or 16384 lines.
    pub fn add_buffer(&mut self, buffer: &'a Buffer) -> Result<(), Error> {
        self.add_lines(&buffer.lines, buffer.tab_width())
    }

    pub(crate) fn add_lines(
        &mut self,
        lines: &'a [BufferLine],
        tab_width: u16,
    ) -> Result<(), Error> {
        if self.lines.len().saturating_add(lines.len()) > MAX_LINES {
            return Err(Error::Budget);
        }
        let mut bytes = self.bytes;
        let mut cached_glyphs = self.cached_glyphs;
        for line in lines {
            bytes = bytes.checked_add(line.text().len()).ok_or(Error::Budget)?;
            let attrs = line.attrs_list();
            if attrs.spans().len() > MAX_LINES {
                return Err(Error::Budget);
            }
            for attr in std::iter::once(attrs.defaults())
                .chain(attrs.spans().iter().map(|(_, attr)| attr.as_attrs()))
            {
                let family = match attr.family {
                    fontdb::Family::Name(name) => name.len(),
                    _ => 0,
                };
                bytes = bytes
                    .checked_add(family)
                    .and_then(|v| v.checked_add(std::mem::size_of::<cosmic_text::AttrsOwned>()))
                    .and_then(|v| {
                        v.checked_add(std::mem::size_of_val(
                            attr.font_features.features.as_slice(),
                        ))
                    })
                    .ok_or(Error::Budget)?;
            }
            let cached = line.shape_opt().map_or(0, |shape| glyphs(shape).count())
                + line.layout_opt().map_or(0, |rows| {
                    rows.iter().map(|row| row.glyphs.len()).sum::<usize>()
                });
            cached_glyphs = cached_glyphs.checked_add(cached).ok_or(Error::Budget)?;
            if bytes > MAX_TEXT_BYTES || cached_glyphs > MAX_GLYPHS {
                return Err(Error::Budget);
            }
        }
        self.bytes = bytes;
        self.cached_glyphs = cached_glyphs;
        self.lines
            .extend(lines.iter().map(|line| (line, tab_width)));
        Ok(())
    }

    /// Captures every face chosen by complete source shaping, including ellipsis probes.
    ///
    /// `byte_budget` is capped at 128 MiB. Missing data, changed font choices or excess
    /// work or aggregate process admission refuse before publication. The previous
    /// snapshot is reused only for equal face metadata and bytes; no file paths or
    /// mapped file storage enter the result.
    /// Sparse face-ID tables refuse rather than rebind cached glyphs to different faces.
    pub fn capture(
        self,
        fonts: &mut FontSystem,
        previous: Option<&Arc<FontSnapshot>>,
        byte_budget: usize,
    ) -> Result<Arc<FontSnapshot>, Error> {
        if fonts.db().len() > MAX_LINES {
            return Err(Error::Budget);
        }
        let metadata_bytes = fonts
            .db()
            .faces()
            .try_fold(0usize, |sum, face| {
                sum.checked_add(face.post_script_name.len())
                    .and_then(|sum| {
                        face.families.iter().try_fold(sum, |sum, (family, _)| {
                            sum.checked_add(family.len() + std::mem::size_of::<fontdb::FaceInfo>())
                        })
                    })
            })
            .ok_or(Error::Budget)?;
        if metadata_bytes > MAX_TEXT_BYTES {
            return Err(Error::Budget);
        }
        let mut probe_keys = HashSet::new();
        let probes: Vec<_> = self
            .lines
            .iter()
            .flat_map(|&(line, tab)| {
                let attrs = cosmic_text::AttrsOwned::new(&line.attrs_list().get_span(0));
                if probe_keys.insert((attrs, tab)) {
                    probes(line).into_iter().map(|probe| (probe, tab)).collect()
                } else {
                    Vec::new()
                }
            })
            .collect();
        let lines = || {
            self.lines
                .iter()
                .copied()
                .chain(probes.iter().map(|(line, tab)| (line, *tab)))
        };
        let keys: Vec<u64> = lines().map(|(line, tab)| line_key(line, tab)).collect();
        let families = [
            fontdb::Family::Serif,
            fontdb::Family::SansSerif,
            fontdb::Family::Cursive,
            fontdb::Family::Fantasy,
            fontdb::Family::Monospace,
        ];
        let same_database = |previous: &FontSnapshot| {
            previous.locale == fonts.locale()
                && previous
                    .aliases
                    .iter()
                    .zip(&families)
                    .all(|(alias, family)| alias == fonts.db().family_name(family))
                && previous.database.len() == fonts.db().len()
                && previous
                    .database
                    .faces()
                    .zip(fonts.db().faces())
                    .all(|(a, b)| same_face(a, b))
        };
        // Every line was validated against the previous snapshot, over the
        // same database, and needs exactly its faces: it serves unshaped.
        if let Some(previous) = previous
            && let Some(needed) = previous.known(&keys)
            && previous.faces.len() == needed.len()
            && previous.faces.iter().all(|face| {
                needed.contains(&face.original.id)
                    && fonts
                        .db()
                        .face(face.original.id)
                        .is_some_and(|original| source_key(&original.source) == face.source)
            })
            && same_database(previous)
        {
            return Ok(previous.clone());
        }
        let mut needed = BTreeSet::new();
        let mut per_line = Vec::with_capacity(keys.len());
        let mut work = 0usize;
        for (line, tab) in lines() {
            let mut line = line.clone();
            line.reset_shaping();
            let mut faces = BTreeSet::new();
            collect(shape(&mut line, fonts, tab), &mut faces, &mut work)?;
            needed.extend(faces.iter().copied());
            if needed.len() > MAX_FACES {
                return Err(Error::Budget);
            }
            per_line.push(faces.into_iter().collect::<Vec<_>>());
        }
        if fonts.locale().len() > MAX_TEXT_BYTES
            || families
                .iter()
                .any(|family| fonts.db().family_name(family).len() > MAX_TEXT_BYTES / 5)
        {
            return Err(Error::Budget);
        }
        let aliases = families.map(|family| fonts.db().family_name(&family).to_owned());
        let locale = fonts.locale().to_owned();
        // The previous snapshot serves when the font database, the aliases
        // and the needed faces and their sources are unchanged; nothing is
        // copied or compared byte by byte then.
        let reuse = previous.filter(|previous| {
            previous.aliases == aliases
                && previous.locale == locale
                && previous.database.len() == fonts.db().len()
                && previous
                    .database
                    .faces()
                    .zip(fonts.db().faces())
                    .all(|(a, b)| same_face(a, b))
                && previous.faces.len() == needed.len()
                && previous.faces.iter().all(|face| {
                    needed.contains(&face.original.id)
                        && fonts
                            .db()
                            .face(face.original.id)
                            .is_some_and(|original| source_key(&original.source) == face.source)
                })
        });
        let snapshot = if let Some(previous) = reuse {
            previous.clone()
        } else {
            let mut faces = Vec::new();
            let mut sources: BTreeMap<SourceKey, Arc<Vec<u8>>> = BTreeMap::new();
            let mut bytes = 0usize;
            for original in fonts.db().faces().filter(|face| needed.contains(&face.id)) {
                if faces.len() >= MAX_FACES {
                    return Err(Error::Budget);
                }
                let key = source_key(&original.source);
                let data = if let Some(bytes) = sources.get(&key) {
                    bytes.clone()
                } else {
                    let remaining = byte_budget
                        .min(MAX_FONT_BYTES)
                        .checked_sub(bytes)
                        .ok_or(Error::Budget)?;
                    let copied = Arc::new(copy_source(&original.source, remaining)?);
                    bytes = bytes.checked_add(copied.len()).ok_or(Error::Budget)?;
                    let _ = sources.insert(key.clone(), copied.clone());
                    copied
                };
                let mut owned_info = original.clone();
                owned_info.source = fontdb::Source::Binary(data.clone());
                faces.push(Face {
                    original: owned_info,
                    source: key,
                    bytes: data,
                });
            }
            if faces.len() != needed.len() {
                return Err(Error::FontAssets);
            }
            let retained_bytes = metadata_bytes
                .checked_add(bytes)
                .and_then(|bytes| {
                    aliases
                        .iter()
                        .try_fold(bytes, |bytes, alias| bytes.checked_add(alias.len()))
                })
                .and_then(|bytes| bytes.checked_add(locale.len()))
                .ok_or(Error::Budget)?;
            let permit = font_admission()
                .reserve(faces.len().max(1), retained_bytes)
                .map_err(|_| Error::Budget)?;
            let mut database = fontdb::Database::new();
            database.set_serif_family(aliases[0].clone());
            database.set_sans_serif_family(aliases[1].clone());
            database.set_cursive_family(aliases[2].clone());
            database.set_fantasy_family(aliases[3].clone());
            database.set_monospace_family(aliases[4].clone());
            let mut ids = BTreeMap::new();
            let empty = Arc::new(Vec::<u8>::new());
            for original in fonts.db().faces() {
                let mut info = original.clone();
                info.source = fontdb::Source::Binary(
                    faces
                        .iter()
                        .find(|face| face.original.id == original.id)
                        .map_or_else(|| empty.clone(), |face| face.bytes.clone()),
                );
                let new = database.push_face_info(info);
                if new != original.id {
                    return Err(Error::FontAssets);
                }
                let _ = ids.insert(original.id, new);
            }
            Arc::new(FontSnapshot {
                database,
                faces,
                ids,
                bytes,
                retained_bytes,
                aliases,
                locale,
                validator: std::sync::Mutex::new(None),
                validated: std::sync::Mutex::new(std::collections::HashMap::new()),
                _permit: permit,
            })
        };
        {
            let mut validator = snapshot.validator.lock().map_err(|_| Error::FontAssets)?;
            let owned = validator.get_or_insert_with(|| {
                #[cfg(test)]
                CAPTURE_VALIDATORS.with(|count| count.set(count.get() + 1));
                snapshot.font_system()
            });
            for face in &snapshot.faces {
                if owned
                    .get_font(face.original.id, face.original.weight)
                    .is_none()
                {
                    return Err(Error::FontAssets);
                }
            }
            for (source, tab) in lines() {
                let mut original = source.clone();
                original.reset_shaping();
                let mut copied = source.clone();
                copied.reset_shaping();
                if !same_shape(
                    shape(&mut original, fonts, tab),
                    shape(&mut copied, owned, tab),
                    &snapshot.ids,
                ) {
                    return Err(Error::FontAssets);
                }
            }
        }
        snapshot.remember(keys.into_iter().zip(per_line));
        Ok(snapshot)
    }
}

impl FontSnapshot {
    /// The faces the lines keyed by `keys` shape with, if every one of them
    /// was validated against this snapshot.
    fn known(&self, keys: &[u64]) -> Option<BTreeSet<fontdb::ID>> {
        let validated = self.validated.lock().ok()?;
        let mut needed = BTreeSet::new();
        for key in keys {
            needed.extend(validated.get(key)?.iter().copied());
        }
        Some(needed)
    }

    /// Records validated lines and the faces each shapes with.
    fn remember(&self, lines: impl IntoIterator<Item = (u64, Vec<fontdb::ID>)>) {
        if let Ok(mut validated) = self.validated.lock() {
            // Bounded: a long-lived snapshot sees every text a window shows.
            if validated.len() > MAX_LINES * 4 {
                validated.clear();
            }
            validated.extend(lines);
        }
    }

    /// Creates independent shaping state for the validated source, with no lazy font-file access.
    ///
    /// Changed text requires a new envelope; uncaptured faces have no font data.
    pub fn font_system(&self) -> FontSystem {
        FontSystem::new_with_locale_and_db(self.locale.clone(), self.database.clone())
    }
    /// Read-only metadata; selected faces own bytes and unselected faces have empty data.
    pub fn database(&self) -> &fontdb::Database {
        &self.database
    }
    /// Unique copied source bytes, counting a font collection once.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Returns the count and bytes charged to aggregate retained-font admission.
    pub fn retained_usage(&self) -> crate::resource::Usage {
        crate::resource::Usage {
            resources: self.faces.len().max(1),
            bytes: self.retained_bytes,
        }
    }
    /// Maps captured UI face IDs to the closed database's IDs.
    pub fn mapped_id(&self, id: fontdb::ID) -> Option<fontdb::ID> {
        self.ids.get(&id).copied()
    }
}

#[cfg(test)]
thread_local! {
    static CAPTURE_SHAPES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CAPTURE_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CAPTURE_VALIDATORS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn shape<'a>(line: &'a mut BufferLine, fonts: &mut FontSystem, tab: u16) -> &'a ShapeLine {
    #[cfg(test)]
    CAPTURE_SHAPES.with(|count| count.set(count.get() + 1));
    // Auto is the pre-1cdc3e0f behaviour: detect each paragraph from its text.
    line.shape(fonts, tab, cosmic_text::Direction::Auto)
}

/// What a line's shaping depends on besides the fonts: its text, attributes,
/// shaping strategy and tab width.
fn line_key(line: &BufferLine, tab: u16) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.text().hash(&mut hasher);
    tab.hash(&mut hasher);
    line.shaping().hash(&mut hasher);
    let attrs = line.attrs_list();
    cosmic_text::AttrsOwned::new(&attrs.defaults()).hash(&mut hasher);
    for (range, span) in attrs.spans() {
        range.hash(&mut hasher);
        span.hash(&mut hasher);
    }
    hasher.finish()
}

fn glyphs(shape: &ShapeLine) -> impl Iterator<Item = &cosmic_text::ShapeGlyph> {
    shape
        .spans
        .iter()
        .flat_map(|span| &span.words)
        .flat_map(|word| &word.glyphs)
}
fn collect(
    shape: &ShapeLine,
    needed: &mut BTreeSet<fontdb::ID>,
    work: &mut usize,
) -> Result<(), Error> {
    for glyph in glyphs(shape) {
        *work = work.checked_add(1).ok_or(Error::Budget)?;
        if *work > MAX_GLYPHS {
            return Err(Error::Budget);
        }
        let _ = needed.insert(glyph.font_id);
        if needed.len() > MAX_FACES {
            return Err(Error::Budget);
        }
    }
    Ok(())
}
fn probes(line: &BufferLine) -> Vec<BufferLine> {
    let attrs = line.attrs_list().get_span(0);
    [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced]
        .into_iter()
        .flat_map(|shaping| {
            ["…", "..."].map(|text| {
                BufferLine::new(
                    text,
                    cosmic_text::LineEnding::None,
                    cosmic_text::AttrsList::new(&attrs),
                    shaping,
                )
            })
        })
        .collect()
}
fn same_shape(a: &ShapeLine, b: &ShapeLine, ids: &BTreeMap<fontdb::ID, fontdb::ID>) -> bool {
    a.rtl == b.rtl
        && a.spans.len() == b.spans.len()
        && a.spans.iter().zip(&b.spans).all(|(a, b)| {
            a.level == b.level
                && a.words.len() == b.words.len()
                && a.words
                    .iter()
                    .zip(&b.words)
                    .all(|(a, b)| a.blank == b.blank && a.glyphs.len() == b.glyphs.len())
        })
        && glyphs(a).zip(glyphs(b)).all(|(a, b)| {
            ids.get(&a.font_id) == Some(&b.font_id)
                && a.font_weight == b.font_weight
                && a.glyph_id == b.glyph_id
                && a.start == b.start
                && a.end == b.end
                && [
                    a.x_advance,
                    a.y_advance,
                    a.x_offset,
                    a.y_offset,
                    a.ascent,
                    a.descent,
                ] == [
                    b.x_advance,
                    b.y_advance,
                    b.x_offset,
                    b.y_offset,
                    b.ascent,
                    b.descent,
                ]
                && a.font_monospace_em_width == b.font_monospace_em_width
        })
}
fn same_face(a: &fontdb::FaceInfo, b: &fontdb::FaceInfo) -> bool {
    a.id == b.id
        && a.index == b.index
        && a.families == b.families
        && a.post_script_name == b.post_script_name
        && a.style == b.style
        && a.weight == b.weight
        && a.stretch == b.stretch
        && a.monospaced == b.monospaced
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SourceKey {
    Memory(usize),
    File(std::path::PathBuf),
}
fn source_key(source: &fontdb::Source) -> SourceKey {
    match source {
        fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
            SourceKey::Memory(Arc::as_ptr(data) as *const () as usize)
        }
        fontdb::Source::File(path) => SourceKey::File(path.clone()),
    }
}
fn copy_source(source: &fontdb::Source, remaining: usize) -> Result<Vec<u8>, Error> {
    #[cfg(test)]
    CAPTURE_COPIES.with(|count| count.set(count.get() + 1));
    match source {
        fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
            let data = data.as_ref().as_ref();
            if data.len() > remaining {
                return Err(Error::Budget);
            }
            Ok(data.to_vec())
        }
        fontdb::Source::File(path) => {
            let mut file = std::fs::File::open(path).map_err(|_| Error::FontAssets)?;
            let metadata = file.metadata().map_err(|_| Error::FontAssets)?;
            if !metadata.is_file() {
                return Err(Error::FontAssets);
            }
            if metadata.len() > remaining as u64 {
                return Err(Error::Budget);
            }
            let mut bytes = Vec::new();
            let _ = file
                .by_ref()
                .take(remaining as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::FontAssets)?;
            if bytes.len() > remaining {
                return Err(Error::Budget);
            }
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_probe_work_shapes_each_attribute_probe_once_and_reuses_validated_lines() {
        let mut database = fontdb::Database::new();
        database.load_font_data(include_bytes!("../../fonts/FiraSans-Regular.ttf").to_vec());
        database.set_sans_serif_family("Fira Sans");
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), database);
        let mut buffer = Buffer::new_empty(cosmic_text::Metrics::new(15.0, 20.0));
        buffer.set_text(
            &"Material 123 with complete shaping.\n".repeat(101),
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Advanced,
            None,
        );
        let mut envelope = FontEnvelope::default();
        envelope.add_buffer(&buffer).unwrap();
        let count = buffer.lines.len();
        CAPTURE_SHAPES.with(|count| count.set(0));
        let first = envelope.capture(&mut fonts, None, MAX_FONT_BYTES).unwrap();
        assert_eq!(CAPTURE_SHAPES.with(std::cell::Cell::get), 3 * (count + 4));
        assert!(first.bytes() > 0);
        assert!(
            first
                .database()
                .faces()
                .any(|face| first.mapped_id(face.id).is_some())
        );
        let mut envelope = FontEnvelope::default();
        envelope.add_buffer(&buffer).unwrap();
        CAPTURE_SHAPES.with(|count| count.set(0));
        CAPTURE_COPIES.with(|count| count.set(0));
        CAPTURE_VALIDATORS.with(|count| count.set(0));
        let second = envelope
            .capture(&mut fonts, Some(&first), MAX_FONT_BYTES)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        // Sabotage: decide reuse after copying the sources again, or build the
        // validating font system per capture, and these count.
        assert_eq!(
            CAPTURE_COPIES.with(std::cell::Cell::get),
            0,
            "font bytes copied again"
        );
        assert_eq!(
            CAPTURE_VALIDATORS.with(std::cell::Cell::get),
            0,
            "validator rebuilt"
        );
        // Every line was validated against `first`: nothing shapes again.
        // Sabotage: drop the validated-line check and this counts 3 per line.
        assert_eq!(
            CAPTURE_SHAPES.with(std::cell::Cell::get),
            0,
            "unchanged lines shaped again"
        );
    }

    #[test]
    fn changed_text_is_shaped_and_validated_again() {
        let mut database = fontdb::Database::new();
        database.load_font_data(include_bytes!("../../fonts/FiraSans-Regular.ttf").to_vec());
        database.set_sans_serif_family("Fira Sans");
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), database);
        let buffer = |text: &str, attrs: cosmic_text::Attrs<'_>| {
            let mut buffer = Buffer::new_empty(cosmic_text::Metrics::new(15.0, 20.0));
            buffer.set_text(text, &attrs, cosmic_text::Shaping::Advanced, None);
            buffer
        };
        let plain = buffer("One\nTwo", cosmic_text::Attrs::new());
        let mut envelope = FontEnvelope::default();
        envelope.add_buffer(&plain).unwrap();
        let first = envelope.capture(&mut fonts, None, MAX_FONT_BYTES).unwrap();
        // New text, then the same text in another weight: each is unknown
        // to the snapshot, so each shapes and validates.
        for changed in [
            buffer("One\nThree", cosmic_text::Attrs::new()),
            buffer(
                "One\nTwo",
                cosmic_text::Attrs::new().weight(fontdb::Weight::BOLD),
            ),
        ] {
            let mut envelope = FontEnvelope::default();
            envelope.add_buffer(&changed).unwrap();
            CAPTURE_SHAPES.with(|count| count.set(0));
            let next = envelope
                .capture(&mut fonts, Some(&first), MAX_FONT_BYTES)
                .unwrap();
            assert!(
                CAPTURE_SHAPES.with(std::cell::Cell::get) > 0,
                "unknown lines shape"
            );
            assert!(next.bytes() > 0);
        }
    }

    #[test]
    fn font_probe_work_distinguishes_attributes_and_tab_configuration() {
        let mut database = fontdb::Database::new();
        database.load_font_data(include_bytes!("../../fonts/FiraSans-Regular.ttf").to_vec());
        database.set_sans_serif_family("Fira Sans");
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), database);
        let mut buffers = Vec::new();
        for index in 0..3 {
            let mut buffer = Buffer::new_empty(cosmic_text::Metrics::new(15.0, 20.0));
            let attrs = if index == 1 {
                cosmic_text::Attrs::new().weight(fontdb::Weight::BOLD)
            } else {
                cosmic_text::Attrs::new()
            };
            buffer.set_text(
                "One\nTwo\nThree",
                &attrs,
                cosmic_text::Shaping::Advanced,
                None,
            );
            if index == 2 {
                buffer.set_tab_width(3);
            }
            buffers.push(buffer);
        }
        let mut envelope = FontEnvelope::default();
        for buffer in &buffers {
            envelope.add_buffer(buffer).unwrap();
        }
        CAPTURE_SHAPES.with(|count| count.set(0));
        let snapshot = envelope.capture(&mut fonts, None, MAX_FONT_BYTES).unwrap();
        assert_eq!(CAPTURE_SHAPES.with(std::cell::Cell::get), 3 * (9 + 4 * 3));
        assert!(snapshot.bytes() > 0);
    }

    #[test]
    fn resource_reclamation_font_snapshot_releases_final_reservation() {
        let admission = crate::resource::Admission::new(1, 8);
        let snapshot = Arc::new(FontSnapshot {
            database: fontdb::Database::new(),
            faces: Vec::new(),
            ids: BTreeMap::new(),
            bytes: 0,
            retained_bytes: 8,
            aliases: std::array::from_fn(|_| String::new()),
            locale: String::new(),
            validator: std::sync::Mutex::new(None),
            validated: std::sync::Mutex::new(std::collections::HashMap::new()),
            _permit: admission
                .reserve(1, 8)
                .expect("snapshot should be admitted"),
        });
        let second = Arc::clone(&snapshot);

        drop(snapshot);
        assert_eq!(
            admission.usage(),
            crate::resource::Usage {
                resources: 1,
                bytes: 8,
            }
        );
        drop(second);
        assert_eq!(admission.usage(), crate::resource::Usage::default());
    }
}
