// Modified by enzUI contributors. See FORK.md.
//! Draw text.
pub mod cache;
pub mod editor;
pub mod font_envelope;
pub mod paragraph;

pub use cache::Cache;
pub use editor::Editor;
pub use paragraph::Paragraph;

pub use cosmic_text;

use crate::core::alignment;
use crate::core::font::{self, Font};
use crate::core::text::{Alignment, Ellipsis, Shaping, Wrapping};
use crate::core::{Color, Pixels, Point, Rectangle, Size, Transformation};

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// A text primitive.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// A paragraph.
    #[allow(missing_docs)]
    Paragraph {
        paragraph: paragraph::Weak,
        /// The strong reference that lets this command be **retained** past the
        /// frame that recorded it (enzICED #2219).
        ///
        /// The per-frame path leaves this `None` and keeps the historic
        /// borrow-from-the-widget behaviour with no extra cost. A retained
        /// scene calls [`Text::retain`], which upgrades `paragraph` and parks
        /// the strong handle here; because `paragraph` is a `Weak` over the
        /// same `Arc`, every existing consumer keeps working unchanged and
        /// simply stops observing a dead reference.
        retained: Option<Paragraph>,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// An editor.
    #[allow(missing_docs)]
    Editor {
        editor: editor::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// Some cached text.
    Cached {
        /// The contents of the text.
        content: String,
        /// The bounds of the text.
        bounds: Rectangle,
        /// The color of the text.
        color: Color,
        /// The size of the text in logical pixels.
        size: Pixels,
        /// The line height of the text.
        line_height: Pixels,
        /// The font of the text.
        font: Font,
        /// The horizontal alignment of the text.
        align_x: Alignment,
        /// The vertical alignment of the text.
        align_y: alignment::Vertical,
        /// The shaping strategy of the text.
        shaping: Shaping,
        /// The wrapping strategy of the text.
        wrapping: Wrapping,
        /// The ellipsis strategy of the text.
        ellipsis: Ellipsis,
        /// The clip bounds of the text.
        clip_bounds: Rectangle,
    },
    /// Some raw text.
    #[allow(missing_docs)]
    Raw {
        raw: Raw,
        /// The strong reference that lets this command be **retained** past the
        /// frame that recorded it (enzICED #2219).
        ///
        /// [`Raw`] carries only a `Weak<cosmic_text::Buffer>`, so a retained
        /// scene that kept the weak handle alone would paint *nothing* where
        /// the text was as soon as its producer went away. The per-frame path
        /// leaves this `None` and keeps the historic borrow; [`Text::retain`]
        /// upgrades the weak handle and parks the strong one here, and refuses
        /// the whole recording when the upgrade fails.
        retained: Option<RetainedRaw>,
        transformation: Transformation,
    },
}

impl Text {
    /// Retains directly ownable commands without consulting mutable font state.
    ///
    /// Live editors refuse here; backends can use [`Self::retain_editor`] with
    /// font access to materialize bounded owned editor text. Expired raw producers refuse.
    pub fn retain(&self) -> Option<Text> {
        match self {
            Text::Paragraph {
                paragraph,
                position,
                color,
                clip_bounds,
                transformation,
                ..
            } => Some(Text::Paragraph {
                paragraph: paragraph.clone(),
                retained: Some(paragraph.upgrade()?),
                position: *position,
                color: *color,
                clip_bounds: *clip_bounds,
                transformation: *transformation,
            }),
            Text::Editor { .. } => None,
            Text::Raw {
                raw,
                retained,
                transformation,
            } => {
                let retained = match retained {
                    Some(retained) => {
                        if !Weak::ptr_eq(&raw.buffer, &Arc::downgrade(retained.buffer())) {
                            return None;
                        }
                        retained.clone()
                    }
                    None => RetainedRaw {
                        buffer: raw.buffer.upgrade()?,
                        editor: None,
                        hint_factor: None,
                    },
                };
                Some(Text::Raw {
                    raw: raw.clone(),
                    retained: Some(retained),
                    transformation: *transformation,
                })
            }
            Text::Cached { .. } => Some(self.clone()),
        }
    }

    /// Materializes editor text with caller-owned fonts, without retaining the live editing state.
    ///
    /// This preserves captured geometry for fixed-layout effects, not responsive editor layout.
    /// Raw-text backends must honor the returned buffer's hint factor.
    pub fn retain_editor(
        &self,
        fonts: &mut cosmic_text::FontSystem,
    ) -> Result<Text, crate::core::renderer::presentation::Error> {
        use crate::core::renderer::presentation::Error;
        use crate::core::text::Editor as _;
        let Text::Editor {
            editor,
            position,
            color,
            clip_bounds,
            transformation,
        } = self
        else {
            return self.retain().ok_or(Error::OpaquePaint);
        };
        let editor = editor.upgrade().ok_or(Error::OpaquePaint)?;
        let snapshot = editor.snapshot(editor.bounds().height)?;
        let replay = snapshot.replay(fonts, editor.bounds())?;
        let buffer = Arc::new(replay.buffer().clone());
        Ok(Text::Raw {
            raw: Raw {
                buffer: Arc::downgrade(&buffer),
                position: *position,
                color: *color,
                clip_bounds: *clip_bounds,
            },
            retained: Some(RetainedRaw {
                buffer,
                editor: Some(snapshot),
                hint_factor: editor.hint_factor(),
            }),
            transformation: *transformation,
        })
    }

    /// Whether this command owns its content and can outlive the frame that
    /// recorded it.
    pub fn is_retained(&self) -> bool {
        match self {
            Text::Paragraph { retained, .. } => retained.is_some(),
            Text::Raw { retained, .. } => retained.is_some(),
            Text::Editor { .. } => false,
            Text::Cached { .. } => true,
        }
    }

    /// Returns the visible bounds of the [`Text`].
    pub fn visible_bounds(&self) -> Option<Rectangle> {
        match self {
            Text::Paragraph {
                position,
                paragraph,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, paragraph.min_bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Editor {
                editor,
                position,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, editor.bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Cached {
                bounds,
                clip_bounds,
                ..
            } => bounds.intersection(clip_bounds),
            Text::Raw { raw, .. } => Some(raw.clip_bounds),
        }
    }
}

/// The regular variant of the [Fira Sans] font.
///
/// It is loaded as part of the default fonts when the `fira-sans`
/// feature is enabled.
///
/// [Fira Sans]: https://mozilla.github.io/Fira/
#[cfg(feature = "fira-sans")]
pub const FIRA_SANS_REGULAR: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice();

/// Installed-by-default Linux sans-serif families, most desktop-typical first
/// (enzICED): the first one present replaces a configured family that is not.
pub const LINUX_SANS_SERIF: &[&str] = &[
    "Adwaita Sans",
    "Cantarell",
    "Noto Sans",
    "Inter",
    "DejaVu Sans",
    "Liberation Sans",
    "Ubuntu",
    "Roboto",
    "Fira Sans",
];

/// Installed-by-default Linux serif families (enzICED); see
/// [`LINUX_SANS_SERIF`]. cosmic-text names "DejaVu Serif" whether or not it
/// is installed, and an unresolved generic serif fell to the sans-first
/// common fallback.
pub const LINUX_SERIF: &[&str] = &[
    "Noto Serif",
    "DejaVu Serif",
    "Liberation Serif",
    "Nimbus Roman",
    "Source Serif 4",
    "Tinos",
    "FreeSerif",
];

/// Installed-by-default Linux monospace families (enzICED); see
/// [`LINUX_SANS_SERIF`].
pub const LINUX_MONOSPACE: &[&str] = &[
    "Adwaita Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Source Code Pro",
    "Ubuntu Mono",
    "Fira Mono",
];

/// Points the `generic` family at the first of `candidates` the database
/// holds, when the family it names now is not installed (enzICED). With no
/// candidate installed, fontconfig's family for the generic name is used
/// when the database holds it. A configured family that is installed is left
/// alone, and so is the family when nothing else is installed either.
///
/// Returns the family now in force.
pub fn settle_family(
    db: &mut cosmic_text::fontdb::Database,
    generic: cosmic_text::Family<'_>,
    candidates: &[&str],
) -> String {
    let installed = |db: &cosmic_text::fontdb::Database, name: &str| {
        db.faces()
            .any(|face| face.families.iter().any(|(family, _)| family == name))
    };
    let current = db.family_name(&generic).to_owned();
    if installed(db, &current) {
        return current;
    }
    let replacement = candidates
        .iter()
        .find(|name| installed(db, name))
        .map(|name| (*name).to_owned())
        .or_else(|| fontconfig_family(&generic).filter(|name| installed(db, name)));
    let Some(replacement) = replacement else {
        return current;
    };
    match generic {
        cosmic_text::Family::SansSerif => db.set_sans_serif_family(replacement.clone()),
        cosmic_text::Family::Serif => db.set_serif_family(replacement.clone()),
        cosmic_text::Family::Monospace => db.set_monospace_family(replacement.clone()),
        cosmic_text::Family::Cursive => db.set_cursive_family(replacement.clone()),
        cosmic_text::Family::Fantasy => db.set_fantasy_family(replacement.clone()),
        cosmic_text::Family::Name(_) => {}
    }
    replacement
}

/// fontconfig's family for a generic name, where fontconfig is available.
fn fontconfig_family(generic: &cosmic_text::Family<'_>) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let name = match generic {
            cosmic_text::Family::SansSerif => "sans-serif",
            cosmic_text::Family::Serif => "serif",
            cosmic_text::Family::Monospace => "monospace",
            cosmic_text::Family::Cursive => "cursive",
            cosmic_text::Family::Fantasy => "fantasy",
            cosmic_text::Family::Name(_) => return None,
        };
        cosmic_text::fontconfig::match_pattern(name).map(|found| found.family)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = generic;
        None
    }
}

/// How glyphs are rasterized (enzICED): the desktop's hinting and
/// antialiasing choice from [`preferences`](crate::core::preferences), or else fontconfig's
/// configuration for the sans-serif family, or else both on.
pub fn raster_preferences(preferences: &crate::core::preferences::Preferences) -> (bool, bool) {
    #[cfg(target_os = "linux")]
    let configured = {
        static FONTCONFIG: OnceLock<Option<cosmic_text::fontconfig::FontMatch>> = OnceLock::new();
        FONTCONFIG
            .get_or_init(|| cosmic_text::fontconfig::match_pattern("sans-serif"))
            .as_ref()
            .map_or((None, None), |found| {
                let hinting = match (found.hinting, found.hint_style) {
                    (Some(false), _) | (_, Some(0)) => Some(false),
                    (hinting, _) => hinting,
                };
                (hinting, found.antialias)
            })
    };
    #[cfg(not(target_os = "linux"))]
    let configured = (None, None);
    (
        preferences.text_hinting.or(configured.0).unwrap_or(true),
        preferences.text_antialias.or(configured.1).unwrap_or(true),
    )
}

#[cfg(feature = "fira-sans")]
fn embedded_fira_sans() -> [cosmic_text::fontdb::Source; 4] {
    [
        cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice(),
        )),
        cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../fonts/FiraSans-Bold.ttf").as_slice(),
        )),
        cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../fonts/FiraSans-Italic.ttf").as_slice(),
        )),
        cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../fonts/FiraSans-BoldItalic.ttf").as_slice(),
        )),
    ]
}

/// Returns the permanent process font catalog and loaded-font intern table.
///
/// Per-window retained snapshots and their usage accounting are separate; dropping
/// them never removes fonts deliberately loaded into this process-wide catalog.
/// Each call brings the glyph raster choice ([`raster_preferences`]) up to
/// date, so text laid out afterwards follows the desktop.
pub fn font_system() -> &'static RwLock<FontSystem> {
    static FONT_SYSTEM: OnceLock<RwLock<FontSystem>> = OnceLock::new();

    let (hinting, antialias) = raster_preferences(&crate::core::preferences::get());
    cosmic_text::set_raster_preferences(hinting, antialias);

    FONT_SYSTEM.get_or_init(|| {
        #[allow(unused_mut)]
        let mut fonts = vec![cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../fonts/Iced-Icons.ttf").as_slice(),
        ))];
        #[cfg(feature = "fira-sans")]
        fonts.extend(embedded_fira_sans());
        #[allow(unused_mut)]
        let mut raw = cosmic_text::FontSystem::new_with_fonts(fonts);

        #[cfg(feature = "fira-sans")]
        raw.db_mut().set_sans_serif_family("Fira Sans");

        // enzICED: cosmic-text names "Open Sans" / "Noto Sans Mono" whether or
        // not they are installed. Without them every generic-family run fell to
        // per-glyph fallback, which for some weights found no usable face and
        // left text blank (a Medium menu label vanished; "Open…" came out in a
        // monospace face).
        #[cfg(target_os = "linux")]
        {
            #[cfg(not(feature = "fira-sans"))]
            let _ = settle_family(
                raw.db_mut(),
                cosmic_text::Family::SansSerif,
                LINUX_SANS_SERIF,
            );
            let _ = settle_family(raw.db_mut(), cosmic_text::Family::Serif, LINUX_SERIF);
            let _ = settle_family(
                raw.db_mut(),
                cosmic_text::Family::Monospace,
                LINUX_MONOSPACE,
            );
        }

        #[cfg(target_os = "macos")]
        {
            #[cfg(not(feature = "fira-sans"))]
            raw.db_mut().set_sans_serif_family(".SF NS");
            raw.db_mut().set_serif_family("Times New Roman");
            raw.db_mut().set_monospace_family("Menlo");
        }

        #[cfg(target_os = "windows")]
        {
            #[cfg(not(feature = "fira-sans"))]
            raw.db_mut().set_sans_serif_family("Segoe UI");
            raw.db_mut().set_serif_family("Times New Roman");
            raw.db_mut().set_monospace_family("Consolas");
        }

        RwLock::new(FontSystem {
            raw,
            loaded_fonts: HashSet::new(),
            version: Version::default(),
        })
    })
}

/// A set of system fonts.
pub struct FontSystem {
    raw: cosmic_text::FontSystem,
    loaded_fonts: HashSet<usize>,
    version: Version,
}

impl FontSystem {
    /// Returns the raw [`cosmic_text::FontSystem`].
    pub fn raw(&mut self) -> &mut cosmic_text::FontSystem {
        &mut self.raw
    }

    /// Loads a font from its bytes.
    ///
    /// A font collection admits every face it contains. Loading the same
    /// borrowed bytes again is a no-op that neither reloads nor advances the
    /// [`version`](Self::version); owned bytes carry no address, so they are
    /// parsed on every call.
    ///
    /// # Errors
    ///
    /// Returns [`font::Error::InvalidData`] when the bytes contain no font
    /// face that could be parsed. A rejected load leaves the database, the
    /// version, and the set of admitted addresses unchanged, so the same bytes
    /// can be offered again and fail again.
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) -> Result<(), font::Error> {
        // Borrowed bytes live for the whole process, so their address is a
        // stable identity; owned bytes have none and are parsed every time.
        let address = match &bytes {
            Cow::Borrowed(bytes) => Some(bytes.as_ptr() as usize),
            Cow::Owned(_) => None,
        };

        if let Some(address) = address
            && self.loaded_fonts.contains(&address)
        {
            return Ok(());
        }

        let size = bytes.len();

        let ids = self
            .raw
            .db_mut()
            .load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(
                bytes.into_owned(),
            )));

        // The database parses every face a collection carries; an empty result
        // is the one signal that nothing was admitted.
        if ids.is_empty() {
            return Err(font::Error::InvalidData { bytes: size });
        }

        // The address is recorded only now, so rejected borrowed bytes are not
        // mistaken for an earlier success on a later attempt.
        if let Some(address) = address {
            let _ = self.loaded_fonts.insert(address);
        }

        self.version = Version(self.version.0 + 1);

        Ok(())
    }

    /// Returns an iterator over the family names of all font faces
    /// in the font database.
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.raw
            .db()
            .faces()
            .filter_map(|face| face.families.first())
            .map(|(name, _)| name.as_str())
    }

    /// Returns the current [`Version`] of the [`FontSystem`].
    ///
    /// A font load that admits a new face increases the version; a rejected or
    /// duplicate load leaves it unchanged.
    pub fn version(&self) -> Version {
        self.version
    }
}

/// A version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version(u32);

/// Owned glyph storage and optional editor source data, independent of the live producer.
///
/// Equality compares buffer/source identity and hint scale, not glyph contents.
#[derive(Debug, Clone)]
pub struct RetainedRaw {
    buffer: Arc<cosmic_text::Buffer>,
    editor: Option<Arc<editor::snapshot::Snapshot>>,
    hint_factor: Option<f32>,
}

impl RetainedRaw {
    /// Couples an owned editor buffer with its immutable source and metrics scale.
    pub fn from_editor(
        buffer: Arc<cosmic_text::Buffer>,
        editor: Arc<editor::snapshot::Snapshot>,
    ) -> Self {
        let hint_factor = editor.hint_factor();
        Self {
            buffer,
            editor: Some(editor),
            hint_factor,
        }
    }

    /// The owned buffer.
    pub fn buffer(&self) -> &Arc<cosmic_text::Buffer> {
        &self.buffer
    }

    /// Buffer-to-logical metrics scale, when the source editor used hinted shaping.
    pub fn hint_factor(&self) -> Option<f32> {
        self.hint_factor
    }

    /// Immutable source retained for later owned editor reflow, if this buffer came from an editor.
    pub fn editor_snapshot(&self) -> Option<&Arc<editor::snapshot::Snapshot>> {
        self.editor.as_ref()
    }
}

impl PartialEq for RetainedRaw {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.buffer, &other.buffer)
            && self.hint_factor == other.hint_factor
            && match (&self.editor, &other.editor) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// A weak reference to a [`cosmic_text::Buffer`] that can be drawn.
#[derive(Debug, Clone)]
pub struct Raw {
    /// A weak reference to a [`cosmic_text::Buffer`].
    pub buffer: Weak<cosmic_text::Buffer>,
    /// The position of the text.
    pub position: Point,
    /// The color of the text.
    pub color: Color,
    /// The clip bounds of the text.
    pub clip_bounds: Rectangle,
}

impl PartialEq for Raw {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: There is no proper way to compare raw buffers
        // For now, no two instances of `Raw` text will be equal.
        // This should be fine, but could trigger unnecessary redraws
        // in the future.
        false
    }
}

/// Measures the dimensions of the given [`cosmic_text::Buffer`].
pub fn measure(buffer: &cosmic_text::Buffer) -> (Size, bool) {
    let (width, height, has_rtl) =
        buffer
            .layout_runs()
            .fold((0.0, 0.0, false), |(width, height, has_rtl), run| {
                (
                    run.line_w.max(width),
                    height + run.line_height,
                    has_rtl || run.rtl,
                )
            });

    (Size::new(width, height), has_rtl)
}

/// Aligns the given [`cosmic_text::Buffer`] with the given [`Alignment`]
/// and returns its minimum [`Size`].
pub fn align(
    buffer: &mut cosmic_text::Buffer,
    font_system: &mut cosmic_text::FontSystem,
    alignment: Alignment,
) -> Size {
    let (min_bounds, has_rtl) = measure(buffer);
    let mut needs_relayout = has_rtl;

    if let Some(align) = to_align(alignment) {
        let has_multiple_lines = buffer.lines.len() > 1
            || buffer
                .lines
                .first()
                .is_some_and(|line| line.layout_opt().is_some_and(|layout| layout.len() > 1));

        if has_multiple_lines {
            for line in &mut buffer.lines {
                let _ = line.set_align(Some(align));
            }

            needs_relayout = true;
        } else if let Some(line) = buffer.lines.first_mut() {
            needs_relayout |= line.set_align(None);
        }
    }

    // TODO: Avoid relayout with some changes to `cosmic-text` (?)
    if needs_relayout {
        log::trace!("Relayouting paragraph...");

        buffer.set_size(Some(min_bounds.width), Some(min_bounds.height));
        buffer.shape_until_scroll(font_system, false);
    }

    min_bounds
}

/// Returns the attributes of the given [`Font`].
pub fn to_attributes(font: Font) -> cosmic_text::Attrs<'static> {
    let attributes = cosmic_text::Attrs::new()
        .family(to_family(font.family))
        .weight(to_weight(font.weight))
        .stretch(to_stretch(font.stretch))
        .style(to_style(font.style));
    match font.numeric {
        font::Numeric::Proportional => attributes,
        font::Numeric::Tabular => attributes.font_features({
            let mut features = cosmic_text::FontFeatures::new();
            let _ = features.enable(cosmic_text::FeatureTag::new(b"tnum"));
            features
        }),
    }
}

fn to_family(family: font::Family) -> cosmic_text::Family<'static> {
    match family {
        font::Family::Name(name) => cosmic_text::Family::Name(name),
        font::Family::SansSerif => cosmic_text::Family::SansSerif,
        font::Family::Serif => cosmic_text::Family::Serif,
        font::Family::Cursive => cosmic_text::Family::Cursive,
        font::Family::Fantasy => cosmic_text::Family::Fantasy,
        font::Family::Monospace => cosmic_text::Family::Monospace,
    }
}

fn to_weight(weight: font::Weight) -> cosmic_text::Weight {
    match weight {
        font::Weight::Thin => cosmic_text::Weight::THIN,
        font::Weight::ExtraLight => cosmic_text::Weight::EXTRA_LIGHT,
        font::Weight::Light => cosmic_text::Weight::LIGHT,
        font::Weight::Normal => cosmic_text::Weight::NORMAL,
        font::Weight::Medium => cosmic_text::Weight::MEDIUM,
        font::Weight::Semibold => cosmic_text::Weight::SEMIBOLD,
        font::Weight::Bold => cosmic_text::Weight::BOLD,
        font::Weight::ExtraBold => cosmic_text::Weight::EXTRA_BOLD,
        font::Weight::Black => cosmic_text::Weight::BLACK,
    }
}

fn to_stretch(stretch: font::Stretch) -> cosmic_text::Stretch {
    match stretch {
        font::Stretch::UltraCondensed => cosmic_text::Stretch::UltraCondensed,
        font::Stretch::ExtraCondensed => cosmic_text::Stretch::ExtraCondensed,
        font::Stretch::Condensed => cosmic_text::Stretch::Condensed,
        font::Stretch::SemiCondensed => cosmic_text::Stretch::SemiCondensed,
        font::Stretch::Normal => cosmic_text::Stretch::Normal,
        font::Stretch::SemiExpanded => cosmic_text::Stretch::SemiExpanded,
        font::Stretch::Expanded => cosmic_text::Stretch::Expanded,
        font::Stretch::ExtraExpanded => cosmic_text::Stretch::ExtraExpanded,
        font::Stretch::UltraExpanded => cosmic_text::Stretch::UltraExpanded,
    }
}

fn to_style(style: font::Style) -> cosmic_text::Style {
    match style {
        font::Style::Normal => cosmic_text::Style::Normal,
        font::Style::Italic => cosmic_text::Style::Italic,
        font::Style::Oblique => cosmic_text::Style::Oblique,
    }
}

pub(crate) fn to_align(alignment: Alignment) -> Option<cosmic_text::Align> {
    match alignment {
        Alignment::Default => None,
        Alignment::Left => Some(cosmic_text::Align::Left),
        Alignment::Center => Some(cosmic_text::Align::Center),
        Alignment::Right => Some(cosmic_text::Align::Right),
        Alignment::Justified => Some(cosmic_text::Align::Justified),
    }
}

/// Converts some [`Shaping`] strategy to a [`cosmic_text::Shaping`] strategy.
pub fn to_shaping(shaping: Shaping, text: &str) -> cosmic_text::Shaping {
    match shaping {
        Shaping::Auto => {
            if text.is_ascii() {
                cosmic_text::Shaping::Basic
            } else {
                cosmic_text::Shaping::Advanced
            }
        }
        Shaping::Basic => cosmic_text::Shaping::Basic,
        Shaping::Advanced => cosmic_text::Shaping::Advanced,
    }
}

/// [`to_shaping`] for text set in `font` (enzICED): a font that asks for
/// OpenType features (tabular figures) is always shaped advanced, because
/// basic shaping skips features, and `Auto` picks basic for ASCII.
pub fn to_shaping_for(shaping: Shaping, text: &str, font: Font) -> cosmic_text::Shaping {
    match font.numeric {
        font::Numeric::Proportional => to_shaping(shaping, text),
        font::Numeric::Tabular => cosmic_text::Shaping::Advanced,
    }
}

/// Converts some [`Wrapping`] strategy to a [`cosmic_text::Wrap`] strategy.
pub fn to_wrap(wrapping: Wrapping) -> cosmic_text::Wrap {
    match wrapping {
        Wrapping::None => cosmic_text::Wrap::None,
        Wrapping::Word => cosmic_text::Wrap::Word,
        Wrapping::Glyph => cosmic_text::Wrap::Glyph,
        Wrapping::WordOrGlyph => cosmic_text::Wrap::WordOrGlyph,
    }
}

/// Converts some [`Ellipsis`] strategy to a [`cosmic_text::Ellipsize`] strategy.
pub fn to_ellipsize(ellipsis: Ellipsis, max_height: f32) -> cosmic_text::Ellipsize {
    let limit = cosmic_text::EllipsizeHeightLimit::Height(max_height);

    match ellipsis {
        Ellipsis::None => cosmic_text::Ellipsize::None,
        Ellipsis::Start => cosmic_text::Ellipsize::Start(limit),
        Ellipsis::Middle => cosmic_text::Ellipsize::Middle(limit),
        Ellipsis::End => cosmic_text::Ellipsize::End(limit),
    }
}

/// Converts some [`Color`] to a [`cosmic_text::Color`].
pub fn to_color(color: Color) -> cosmic_text::Color {
    let [r, g, b, a] = color.into_rgba8();

    cosmic_text::Color::rgba(r, g, b, a)
}

/// Returns the ideal hint factor given the size and scale factor of some text.
pub fn hint_factor(_size: Pixels, _scale_factor: Option<f32>) -> Option<f32> {
    // TODO: Fix hinting in `cosmic-text`
    // const MAX_HINTING_SIZE: f32 = 18.0;

    // let hint_factor = scale_factor?;

    // if size.0 * hint_factor < MAX_HINTING_SIZE {
    //     Some(hint_factor)
    // } else {
    //     None
    // }

    None // Disable all text hinting for now
}

/// A text renderer coupled to `iced_graphics`.
pub trait Renderer {
    /// Draws the given [`Raw`] text.
    fn fill_raw(&mut self, raw: Raw);
}

#[cfg(test)]
mod settle_family_tests {
    use super::settle_family;
    use cosmic_text::Family;
    use cosmic_text::fontdb::Database;

    #[test]
    #[cfg(feature = "fira-sans")]
    fn embedded_sans_shapes_distinct_regular_and_bold_faces() {
        use cosmic_text::{Attrs, Buffer, Metrics, Shaping, Style, Weight};
        let mut guard = super::font_system().write().expect("font system");
        let system = guard.raw();
        assert_eq!(system.db().family_name(&Family::SansSerif), "Fira Sans");
        let mut shaped = Vec::new();
        for (weight, style) in [
            (Weight::NORMAL, Style::Normal),
            (Weight::BOLD, Style::Normal),
            (Weight::NORMAL, Style::Italic),
            (Weight::BOLD, Style::Italic),
        ] {
            let mut buffer = Buffer::new(system, Metrics::new(20.0, 26.0));
            buffer.set_size(Some(500.0), Some(100.0));
            buffer.set_text(
                "Regular and bold",
                &Attrs::new()
                    .family(Family::SansSerif)
                    .weight(weight)
                    .style(style),
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(system, false);
            let faces = buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
                .collect::<Vec<_>>();
            assert!(!faces.is_empty());
            for id in &faces {
                let face = system.db().face(*id).unwrap();
                assert_eq!(face.weight, weight);
                assert_eq!(face.style, style);
                assert!(face.families.iter().any(|(name, _)| name == "Fira Sans"));
            }
            assert!(
                !shaped.contains(&faces),
                "each style must shape with its own face"
            );
            shaped.push(faces);
        }
    }

    fn db_with_fira_sans() -> Database {
        let mut db = Database::new();
        db.load_font_data(include_bytes!("../fonts/FiraSans-Regular.ttf").to_vec());
        db
    }

    /// cosmic-text names "Open Sans" whether or not it is installed; on a
    /// system without it, generic-family text fell to per-glyph fallback.
    ///
    /// Sabotage: return `current` unconditionally and the family stays
    /// "Open Sans".
    #[test]
    fn an_uninstalled_family_is_replaced_by_the_first_installed_candidate() {
        let mut db = db_with_fira_sans();
        db.set_sans_serif_family("Open Sans");
        let settled = settle_family(&mut db, Family::SansSerif, &["Not Installed", "Fira Sans"]);
        assert_eq!(settled, "Fira Sans");
        assert_eq!(db.family_name(&Family::SansSerif), "Fira Sans");
    }

    /// The serif generic is settled the same way: with its configured face
    /// missing, an installed candidate takes over, and an installed
    /// configured serif stays. Fira Sans stands in for an installed face.
    #[test]
    fn a_missing_serif_is_replaced_and_an_installed_one_kept() {
        let mut db = db_with_fira_sans();
        db.set_serif_family("DejaVu Serif");
        assert_eq!(
            settle_family(&mut db, Family::Serif, &["Noto Serif", "Fira Sans"]),
            "Fira Sans"
        );
        assert_eq!(db.family_name(&Family::Serif), "Fira Sans");
        assert_eq!(
            settle_family(&mut db, Family::Serif, &["Noto Serif"]),
            "Fira Sans",
            "an installed configured serif stays selected"
        );
    }

    /// With no listed candidate installed, the generic family takes the face
    /// fontconfig picks for it.
    #[test]
    #[cfg(target_os = "linux")]
    fn with_no_candidate_installed_fontconfig_picks_the_family() {
        let Some(found) = cosmic_text::fontconfig::match_pattern("monospace") else {
            return;
        };
        let system = cosmic_text::FontSystem::new();
        let Some(id) = system.db().query(&cosmic_text::fontdb::Query {
            families: &[cosmic_text::fontdb::Family::Name(&found.family)],
            ..Default::default()
        }) else {
            return;
        };
        let mut db = db_with_fira_sans();
        db.load_font_data(
            system
                .db()
                .with_face_data(id, |bytes, _| bytes.to_vec())
                .expect("face data"),
        );
        db.set_monospace_family("Not Installed Mono");
        assert_eq!(
            settle_family(&mut db, Family::Monospace, &["Also Not Installed"]),
            found.family
        );
    }

    /// On this machine the process font system's generic serif names an
    /// installed face whenever any candidate serif is installed.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_process_serif_is_installed_when_any_candidate_is() {
        let mut system = super::font_system().write().expect("font system");
        let db = system.raw().db();
        let installed = |name: &str| {
            db.faces()
                .any(|face| face.families.iter().any(|(family, _)| family == name))
        };
        if super::LINUX_SERIF.iter().any(|name| installed(name)) {
            let serif = db.family_name(&Family::Serif);
            assert!(
                installed(serif),
                "generic serif names {serif:?}, which is not installed"
            );
        }
    }

    #[test]
    fn an_installed_family_is_kept_and_no_candidate_changes_nothing() {
        let mut db = db_with_fira_sans();
        db.set_sans_serif_family("Fira Sans");
        assert_eq!(
            settle_family(&mut db, Family::SansSerif, &["Other"]),
            "Fira Sans"
        );

        db.set_monospace_family("Missing Mono");
        assert_eq!(
            settle_family(&mut db, Family::Monospace, &["Also Missing"]),
            "Missing Mono",
            "with no installed candidate the configured family stays"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod raster_preference_tests {
    use super::raster_preferences;
    use crate::core::preferences::Preferences;

    /// The desktop's word wins; without it fontconfig's configuration (or
    /// "on") applies.
    #[test]
    fn the_desktop_choice_outranks_fontconfig() {
        let off = Preferences {
            text_hinting: Some(false),
            text_antialias: Some(false),
            ..Preferences::DEFAULT
        };
        assert_eq!(raster_preferences(&off), (false, false));
        let on = Preferences {
            text_hinting: Some(true),
            text_antialias: Some(true),
            ..Preferences::DEFAULT
        };
        assert_eq!(raster_preferences(&on), (true, true));
        let configured = cosmic_text::fontconfig::match_pattern("sans-serif");
        let (_, antialias) = raster_preferences(&Preferences::DEFAULT);
        assert_eq!(
            antialias,
            configured.and_then(|found| found.antialias).unwrap_or(true)
        );
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::to_attributes;
    use crate::core::Font;

    /// The width `text` shapes to in Fira Sans with `font`'s figures.
    fn width(font: Font, text: &str) -> f32 {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(include_bytes!("../fonts/FiraSans-Regular.ttf").to_vec());
        let mut fonts = cosmic_text::FontSystem::new_with_locale_and_db("en-US".into(), db);
        let mut buffer =
            cosmic_text::Buffer::new(&mut fonts, cosmic_text::Metrics::new(20.0, 24.0));
        buffer.set_text(
            text,
            &to_attributes(font),
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fonts, false);
        buffer.layout_runs().map(|run| run.line_w).sum()
    }

    /// Text that may shape basic (`Auto` on ASCII) still gets tabular
    /// figures: the paragraph shapes it advanced.
    #[test]
    fn auto_shaped_ascii_gets_tabular_figures() {
        use crate::core::text::Paragraph as _;
        let _ = super::font_system()
            .write()
            .expect("font system")
            .load_font(std::borrow::Cow::Borrowed(include_bytes!(
                "../fonts/FiraSans-Regular.ttf"
            )));
        let width = |font: Font, content: &str| {
            super::paragraph::Paragraph::with_text(crate::core::Text {
                content,
                bounds: crate::core::Size::INFINITE,
                size: 20.0.into(),
                line_height: crate::core::text::LineHeight::default(),
                font,
                align_x: crate::core::text::Alignment::Left,
                align_y: crate::core::alignment::Vertical::Top,
                shaping: crate::core::text::Shaping::Auto,
                wrapping: crate::core::text::Wrapping::None,
                ellipsis: crate::core::text::Ellipsis::None,
                hint_factor: None,
            })
            .min_width()
        };
        let tabular = Font::new("Fira Sans").tabular();
        assert_eq!(width(tabular, "1111"), width(tabular, "0000"));
    }

    /// Tabular figures are one width; the default figures are not.
    #[test]
    fn tabular_figures_have_one_width() {
        let fira = Font::new("Fira Sans");
        assert_ne!(width(fira, "1111"), width(fira, "0000"));
        let tabular = fira.tabular();
        assert_eq!(width(tabular, "1111"), width(tabular, "0000"));
    }
}
