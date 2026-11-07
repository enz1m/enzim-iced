// Modified by enzUI contributors. See FORK.md.
pub(crate) mod editor;
mod owned;
mod reflow;
pub(crate) use owned::{FontAssets, retain_section};

use crate::core::alignment;
use crate::core::text::Alignment;
use crate::core::{Rectangle, Size, Transformation, Vector};
use crate::graphics::cache;
use crate::graphics::color;
use crate::graphics::text::cache::{self as text_cache, Cache as BufferCache};
use crate::graphics::text::{Editor, Paragraph, font_system, to_color};
use crate::nudge;

use rustc_hash::FxHashMap;
use std::collections::hash_map;
use std::sync::atomic::{self, AtomicU64};
use std::sync::{self, Arc, RwLock, Weak};

pub use crate::graphics::Text;

const COLOR_MODE: cryoglyph::ColorMode = if color::GAMMA_CORRECTION {
    cryoglyph::ColorMode::Accurate
} else {
    cryoglyph::ColorMode::Web
};

pub type Batch = Vec<Item>;

#[derive(Debug, Clone)]
pub enum Item {
    Group {
        transformation: Transformation,
        text: Vec<Text>,
    },
    Cached {
        transformation: Transformation,
        cache: Cache,
    },
}

impl Item {
    pub(crate) fn bake_translation(&mut self) {
        let Self::Group {
            transformation,
            text,
        } = self
        else {
            return;
        };
        let offset = transformation.translation();
        if *transformation == Transformation::IDENTITY
            || !offset.x.is_finite()
            || !offset.y.is_finite()
            || *transformation != Transformation::translate(offset.x, offset.y)
        {
            return;
        }
        for section in text {
            match section {
                Text::Paragraph {
                    transformation: local,
                    ..
                }
                | Text::Editor {
                    transformation: local,
                    ..
                }
                | Text::Raw {
                    transformation: local,
                    ..
                } => {
                    *local = *transformation * *local;
                }
                Text::Cached {
                    bounds,
                    clip_bounds,
                    ..
                } => {
                    *bounds = *bounds * *transformation;
                    *clip_bounds = *clip_bounds * *transformation;
                }
            }
        }
        *transformation = Transformation::IDENTITY;
    }

    /// Offset this recorded text by `translation` (enzICED #2219 scene replay).
    ///
    /// Text keeps its transformation as data, so replaying at a new position is
    /// a pre-multiplication rather than a re-shape: no glyph is laid out again.
    pub fn translate(&mut self, translation: crate::core::Vector) {
        let offset = Transformation::translate(translation.x, translation.y);

        match self {
            Item::Group { transformation, .. } | Item::Cached { transformation, .. } => {
                *transformation = offset * *transformation;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cache {
    id: Id,
    group: cache::Group,
    text: Arc<[Text]>,
    version: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(u64);

impl Cache {
    /// What this cache holds: its id and how often it was updated.
    pub(crate) fn identity(&self) -> (u64, usize) {
        (self.id.0, self.version)
    }

    pub fn new(group: cache::Group, text: Vec<Text>) -> Option<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        if text.is_empty() {
            return None;
        }

        Some(Self {
            id: Id(NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed)),
            group,
            text: Arc::from(text),
            version: 0,
        })
    }

    pub fn update(&mut self, text: Vec<Text>) {
        if self.text.is_empty() && text.is_empty() {
            return;
        }

        self.text = Arc::from(text);
        self.version += 1;
    }
}

struct Upload {
    renderer: cryoglyph::TextRenderer,
    buffer_cache: BufferCache,
    transformation: Transformation,
    version: usize,
    group_version: usize,
    text: sync::Weak<[Text]>,
    _atlas: sync::Weak<()>,
}

#[derive(Default)]
pub struct Storage {
    groups: FxHashMap<cache::Group, Group>,
    uploads: FxHashMap<Id, Upload>,
}

struct Group {
    atlas: cryoglyph::TextAtlas,
    version: usize,
    should_trim: bool,
    handle: Arc<()>, // Keeps track of active uploads
}

impl Storage {
    fn get(&self, cache: &Cache) -> Option<(&cryoglyph::TextAtlas, &Upload)> {
        if cache.text.is_empty() {
            return None;
        }

        self.groups
            .get(&cache.group)
            .map(|group| &group.atlas)
            .zip(self.uploads.get(&cache.id))
    }

    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: &cryoglyph::Viewport,
        encoder: &mut wgpu::CommandEncoder,
        format: wgpu::TextureFormat,
        state: &cryoglyph::Cache,
        cache: &Cache,
        new_transformation: Transformation,
        bounds: Rectangle,
        owned_fonts: Option<&mut cryoglyph::FontSystem>,
        maximum: u32,
    ) {
        let group_count = self.groups.len();

        let group = self.groups.entry(cache.group).or_insert_with(|| {
            log::debug!(
                "New text atlas: {:?} (total: {})",
                cache.group,
                group_count + 1
            );

            Group {
                atlas: cryoglyph::TextAtlas::with_color_mode(
                    device, queue, state, format, COLOR_MODE,
                ),
                version: 0,
                should_trim: false,
                handle: Arc::new(()),
            }
        });

        group.atlas.set_max_size(maximum);
        match self.uploads.entry(cache.id) {
            hash_map::Entry::Occupied(entry) => {
                let upload = entry.into_mut();

                if upload.version != cache.version
                    || upload.group_version != group.version
                    || upload.transformation != new_transformation
                {
                    if !cache.text.is_empty() {
                        let _ = prepare(
                            device,
                            queue,
                            viewport,
                            encoder,
                            &mut upload.renderer,
                            &mut group.atlas,
                            &mut upload.buffer_cache,
                            &cache.text,
                            bounds,
                            new_transformation,
                            owned_fonts,
                        );
                    }

                    // Only trim if glyphs have changed
                    group.should_trim = group.should_trim || upload.version != cache.version;

                    upload.text = Arc::downgrade(&cache.text);
                    upload.version = cache.version;
                    upload.group_version = group.version;
                    upload.transformation = new_transformation;

                    upload.buffer_cache.trim();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                let mut renderer = cryoglyph::TextRenderer::new(
                    &mut group.atlas,
                    device,
                    wgpu::MultisampleState::default(),
                    None,
                );

                let mut buffer_cache = BufferCache::new();

                if !cache.text.is_empty() {
                    let _ = prepare(
                        device,
                        queue,
                        viewport,
                        encoder,
                        &mut renderer,
                        &mut group.atlas,
                        &mut buffer_cache,
                        &cache.text,
                        bounds,
                        new_transformation,
                        owned_fonts,
                    );
                }

                let _ = entry.insert(Upload {
                    renderer,
                    buffer_cache,
                    transformation: new_transformation,
                    version: 0,
                    group_version: group.version,
                    text: Arc::downgrade(&cache.text),
                    _atlas: Arc::downgrade(&group.handle),
                });

                group.should_trim = cache.group.is_singleton();

                log::debug!(
                    "New text upload: {} (total: {})",
                    cache.id.0,
                    self.uploads.len()
                );
            }
        }
    }

    pub fn trim(&mut self) {
        self.uploads
            .retain(|_id, upload| upload.text.strong_count() > 0);

        self.groups.retain(|id, group| {
            let active_uploads = Arc::weak_count(&group.handle);

            if active_uploads == 0 {
                log::debug!("Dropping text atlas: {id:?}");

                return false;
            }

            if group.should_trim {
                log::trace!("Trimming text atlas: {id:?}");

                group.atlas.trim();
                group.should_trim = false;

                // We only need to worry about glyph fighting
                // when the atlas may be shared by multiple
                // uploads.
                if !id.is_singleton() {
                    log::debug!(
                        "Invalidating text atlas: {id:?} \
                        (uploads: {active_uploads})"
                    );

                    group.version += 1;
                }
            }

            true
        });
    }
}

pub struct Viewport(cryoglyph::Viewport);

impl Viewport {
    pub fn update(&mut self, queue: &wgpu::Queue, resolution: Size<u32>) {
        self.0.update(
            queue,
            cryoglyph::Resolution {
                width: resolution.width,
                height: resolution.height,
            },
        );
    }
}

#[derive(Clone)]
pub struct Pipeline {
    format: wgpu::TextureFormat,
    cache: cryoglyph::Cache,
    atlas: Arc<RwLock<cryoglyph::TextAtlas>>,
    maximum: Arc<std::sync::atomic::AtomicU32>,
}

impl Pipeline {
    pub(crate) fn atlas_identity(&self) -> usize {
        Arc::as_ptr(&self.atlas) as usize
    }

    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let cache = cryoglyph::Cache::new(device);
        let atlas =
            cryoglyph::TextAtlas::with_color_mode(device, queue, &cache, format, COLOR_MODE);

        Pipeline {
            format,
            cache,
            atlas: Arc::new(RwLock::new(atlas)),
            maximum: Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX)),
        }
    }

    pub(crate) fn independent(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let mut pipeline = Self::new(device, queue, self.format);
        pipeline.maximum = self.maximum.clone();
        pipeline
    }

    pub fn create_viewport(&self, device: &wgpu::Device) -> Viewport {
        Viewport(cryoglyph::Viewport::new(device, &self.cache))
    }

    pub fn trim(&self) {
        self.atlas.write().expect("Write text atlas").trim();
    }

    /// Returns the glyph rasterization, upload and atlas growth mechanism
    /// counts accumulated by the shared text atlas.
    pub fn statistics(&self) -> TextAtlasStatistics {
        self.atlas.read().expect("Read text atlas").statistics()
    }

    /// Caps text atlas growth at `max_size` texels per side (a VRAM budget).
    ///
    /// See [`cryoglyph::TextAtlas::set_max_size`]; the cap never shrinks an
    /// atlas that has already grown past it.
    pub fn set_max_atlas_size(&self, max_size: u32) {
        self.maximum.store(max_size, atomic::Ordering::Relaxed);
        self.atlas
            .write()
            .expect("Write text atlas")
            .set_max_size(max_size);
    }

    /// Resets the counters returned by [`Pipeline::statistics`].
    pub fn reset_statistics(&self) {
        self.atlas
            .write()
            .expect("Write text atlas")
            .reset_statistics();
    }
}

/// Glyph rasterization, upload and atlas growth mechanism counts.
///
/// enzUI-ICED #1514.
pub type TextAtlasStatistics = cryoglyph::AtlasStatistics;

#[derive(Default)]
pub struct State {
    renderers: Vec<cryoglyph::TextRenderer>,
    prepare_layer: usize,
    cache: BufferCache,
    storage: Storage,
    /// Whether an atlas exhaustion has already been reported; the condition
    /// repeats every frame and one warning is enough to diagnose it.
    reported_atlas_full: bool,
    capture_assets: Option<Weak<FontAssets>>,
    raster_fonts: Option<owned::RasterFonts>,
    frame_font_claimed: bool,
    pub(crate) scroll_inputs: std::collections::BTreeMap<u64, crate::core::layout::scroll::Input>,
    pub(crate) scroll_revision: Option<Arc<()>>,
    reflow: reflow::Reflow,
    editors: editor::Editors,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn prepare(
        &mut self,
        pipeline: &Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: &Viewport,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
        layer_bounds: Rectangle,
        layer_transformation: Transformation,
    ) {
        let mut atlas = pipeline.atlas.write().expect("Write to text atlas");
        let maximum = pipeline.maximum.load(atomic::Ordering::Relaxed);
        atlas.set_max_size(maximum);

        for item in batch {
            match item {
                Item::Group {
                    transformation,
                    text,
                } => {
                    if self.renderers.len() <= self.prepare_layer {
                        self.renderers.push(cryoglyph::TextRenderer::new(
                            &mut atlas,
                            device,
                            wgpu::MultisampleState::default(),
                            None,
                        ));
                    }

                    let renderer = &mut self.renderers[self.prepare_layer];
                    let result = prepare(
                        device,
                        queue,
                        &viewport.0,
                        encoder,
                        renderer,
                        &mut atlas,
                        &mut self.cache,
                        text,
                        layer_bounds * layer_transformation,
                        layer_transformation * *transformation,
                        self.raster_fonts.as_mut().map(|fonts| &mut fonts.system),
                    );

                    match result {
                        Ok(()) => {
                            self.prepare_layer += 1;
                        }
                        Err(cryoglyph::PrepareError::AtlasFull) => {
                            // The atlas is at `max_texture_dimension_2d` and every
                            // sized glyph in it is in use this frame, so this text
                            // cannot be uploaded and will render incompletely.
                            // enzUI-ICED #1514: report it instead of silently
                            // dropping text -- `Pipeline::statistics()` carries
                            // `growth_failures` for a caller that wants to react.
                            if !self.reported_atlas_full {
                                self.reported_atlas_full = true;

                                let statistics = atlas.statistics();

                                log::warn!(
                                    "Text atlas is full: some text will not be drawn. \
                                     mask atlas {}x{}, color atlas {}x{}, {} growth \
                                     refusals so far.",
                                    statistics.mask_size,
                                    statistics.mask_size,
                                    statistics.color_size,
                                    statistics.color_size,
                                    statistics.growth_failures,
                                );
                            }
                        }
                    }
                }
                Item::Cached {
                    transformation,
                    cache,
                } => {
                    self.storage.prepare(
                        device,
                        queue,
                        &viewport.0,
                        encoder,
                        pipeline.format,
                        &pipeline.cache,
                        cache,
                        layer_transformation * *transformation,
                        layer_bounds * layer_transformation,
                        self.raster_fonts.as_mut().map(|fonts| &mut fonts.system),
                        maximum,
                    );
                }
            }
        }
    }

    pub fn render<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        viewport: &'a Viewport,
        start: usize,
        batch: &'a Batch,
        bounds: Rectangle<u32>,
        render_pass: &mut wgpu::RenderPass<'a>,
    ) -> usize {
        let atlas = pipeline.atlas.read().expect("Read text atlas");
        let mut layer_count = 0;

        render_pass.set_scissor_rect(bounds.x, bounds.y, bounds.width, bounds.height);

        for item in batch {
            match item {
                Item::Group { .. } => {
                    let renderer = &self.renderers[start + layer_count];

                    renderer
                        .render(&atlas, &viewport.0, render_pass)
                        .expect("Render text");

                    layer_count += 1;
                }
                Item::Cached { cache, .. } => {
                    if let Some((atlas, upload)) = self.storage.get(cache) {
                        upload
                            .renderer
                            .render(atlas, &viewport.0, render_pass)
                            .expect("Render cached text");
                    }
                }
            }
        }

        layer_count
    }

    pub fn trim(&mut self) {
        self.cache.trim();
        self.storage.trim();

        self.prepare_layer = 0;
    }
}

fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    viewport: &cryoglyph::Viewport,
    encoder: &mut wgpu::CommandEncoder,
    renderer: &mut cryoglyph::TextRenderer,
    atlas: &mut cryoglyph::TextAtlas,
    buffer_cache: &mut BufferCache,
    sections: &[Text],
    layer_bounds: Rectangle,
    layer_transformation: Transformation,
    owned_fonts: Option<&mut cryoglyph::FontSystem>,
) -> Result<(), cryoglyph::PrepareError> {
    let mut shared;
    let font_system = match owned_fonts {
        Some(fonts) => fonts,
        None => {
            shared = font_system().write().expect("Write font system");
            shared.raw()
        }
    };

    enum Allocation {
        Paragraph(Paragraph),
        Editor(Editor),
        Cache(text_cache::KeyHash),
        Raw(Arc<cryoglyph::Buffer>),
    }

    let allocations: Vec<_> = sections
        .iter()
        .map(|section| match section {
            Text::Paragraph { paragraph, .. } => paragraph.upgrade().map(Allocation::Paragraph),
            Text::Editor { editor, .. } => editor.upgrade().map(Allocation::Editor),
            Text::Cached {
                content,
                bounds,
                size,
                line_height,
                font,
                shaping,
                wrapping,
                ellipsis,
                align_x,
                ..
            } => {
                let (key, _) = buffer_cache.allocate(
                    font_system,
                    text_cache::Key {
                        content,
                        size: f32::from(*size),
                        line_height: f32::from(*line_height),
                        font: *font,
                        align_x: *align_x,
                        bounds: Size {
                            width: bounds.width,
                            height: bounds.height,
                        },
                        shaping: *shaping,
                        wrapping: *wrapping,
                        ellipsis: *ellipsis,
                    },
                );

                Some(Allocation::Cache(key))
            }
            // A retained command owns its buffer; a per-frame one still
            // borrows it from the widget (#2219).
            Text::Raw { raw, retained, .. } => retained
                .as_ref()
                .map(|retained| retained.buffer().clone())
                .or_else(|| raw.buffer.upgrade())
                .map(Allocation::Raw),
        })
        .collect();

    let text_areas = sections
        .iter()
        .zip(allocations.iter())
        .filter_map(|(section, allocation)| {
            let (buffer, hint_factor, position, color, clip_bounds, transformation) = match section
            {
                Text::Paragraph {
                    position,
                    color,
                    clip_bounds,
                    transformation,
                    ..
                } => {
                    use crate::core::text::Paragraph as _;

                    let Some(Allocation::Paragraph(paragraph)) = allocation else {
                        return None;
                    };

                    (
                        paragraph.buffer(),
                        paragraph.hint_factor(),
                        *position,
                        *color,
                        *clip_bounds,
                        *transformation,
                    )
                }
                Text::Editor {
                    position,
                    color,
                    clip_bounds,
                    transformation,
                    ..
                } => {
                    use crate::core::text::Editor as _;

                    let Some(Allocation::Editor(editor)) = allocation else {
                        return None;
                    };

                    (
                        editor.buffer(),
                        editor.hint_factor(),
                        *position,
                        *color,
                        *clip_bounds,
                        *transformation,
                    )
                }
                Text::Cached {
                    bounds,
                    align_x,
                    align_y,
                    color,
                    clip_bounds,
                    ..
                } => {
                    let Some(Allocation::Cache(key)) = allocation else {
                        return None;
                    };

                    let entry = buffer_cache.get(key).expect("Get cached buffer");

                    let mut position = bounds.position();

                    position.x = match align_x {
                        Alignment::Default | Alignment::Left | Alignment::Justified => position.x,
                        Alignment::Center => position.x - entry.min_bounds.width / 2.0,
                        Alignment::Right => position.x - entry.min_bounds.width,
                    };

                    position.y = match align_y {
                        alignment::Vertical::Top => position.y,
                        alignment::Vertical::Center => position.y - entry.min_bounds.height / 2.0,
                        alignment::Vertical::Bottom => position.y - entry.min_bounds.height,
                    };

                    (
                        &entry.buffer,
                        None,
                        position,
                        *color,
                        *clip_bounds,
                        Transformation::IDENTITY,
                    )
                }
                Text::Raw {
                    raw,
                    transformation,
                    retained,
                } => {
                    let Some(Allocation::Raw(buffer)) = allocation else {
                        return None;
                    };

                    (
                        buffer.as_ref(),
                        retained
                            .as_ref()
                            .and_then(iced_graphics::text::RetainedRaw::hint_factor),
                        raw.position,
                        raw.color,
                        raw.clip_bounds,
                        *transformation,
                    )
                }
            };

            // Upstream #3466: the text clip snaps like the quad shader and
            // the scissor, so text is cut exactly where its panel's edge is.
            let clip_bounds = layer_bounds
                .intersection(&(clip_bounds * transformation * layer_transformation))
                .and_then(nudge::snap)?;

            let translation = Vector::new(-buffer.scroll().horizontal, 0.0);

            let mut position = (position + translation) * transformation * layer_transformation;
            let mut scale = transformation.scale_factor() * layer_transformation.scale_factor();

            if let Some(hint_factor) = hint_factor {
                let font_size = (buffer.metrics().font_size / hint_factor).round() as u32;

                position.x = position.x.round()
                    // This is a hack! Empirically and cluelessly derived
                    // It tries to nudge hinted text to improve rasterization
                    + if font_size.is_multiple_of(2) {
                        0.25
                    } else if font_size.is_multiple_of(5) {
                        -0.45 // Deliberately avoid 0.5 to circumvent erratic rounding due to floating precision errors
                    } else {
                        -0.25
                    };

                scale /= hint_factor;
            }

            Some(cryoglyph::TextArea {
                text: buffer.layout_runs(),
                left: position.x,
                top: position.y,
                scale,
                bounds: cryoglyph::TextBounds {
                    left: clip_bounds.x as i32,
                    top: clip_bounds.y as i32,
                    right: (clip_bounds.x + clip_bounds.width) as i32,
                    bottom: (clip_bounds.y + clip_bounds.height) as i32,
                },
                default_color: to_color(color),
            })
        });

    renderer.prepare(
        device,
        queue,
        encoder,
        font_system,
        atlas,
        viewport,
        text_areas,
        &mut cryoglyph::SwashCache::new(),
    )
}

#[cfg(test)]
mod owned_batch_tests {
    use super::*;
    use crate::core::{Color, Font, Pixels, Point};

    fn cached() -> Text {
        Text::Cached {
            content: "A".into(),
            bounds: Rectangle {
                x: 3.0,
                y: 5.0,
                width: 20.0,
                height: 24.0,
            },
            color: Color::WHITE,
            size: Pixels(16.0),
            line_height: Pixels(20.0),
            font: Font::DEFAULT,
            align_x: Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: crate::core::text::Shaping::default(),
            wrapping: crate::core::text::Wrapping::default(),
            ellipsis: crate::core::text::Ellipsis::default(),
            clip_bounds: Rectangle {
                x: 1.0,
                y: 2.0,
                width: 80.0,
                height: 50.0,
            },
        }
    }

    #[test]
    fn text_batch_translation_composes_raw_transforms_and_cached_coordinates() {
        let translation = Transformation::translate(7.0, 9.0);
        let local = Transformation::scale(2.0);
        let mut item = Item::Group {
            transformation: translation,
            text: vec![
                cached(),
                Text::Raw {
                    raw: crate::graphics::text::Raw {
                        buffer: Weak::new(),
                        position: Point::new(4.0, 5.0),
                        color: Color::WHITE,
                        clip_bounds: Rectangle::with_size(Size::new(20.0, 20.0)),
                    },
                    retained: None,
                    transformation: local,
                },
            ],
        };
        item.bake_translation();
        let Item::Group {
            transformation,
            text,
        } = item
        else {
            panic!("group retained")
        };
        assert_eq!(transformation, Transformation::IDENTITY);
        let Text::Cached {
            bounds,
            clip_bounds,
            size,
            ..
        } = &text[0]
        else {
            panic!("cached section retained")
        };
        assert_eq!(bounds.position(), Point::new(10.0, 14.0));
        assert_eq!(bounds.size(), Size::new(20.0, 24.0));
        assert_eq!(clip_bounds.position(), Point::new(8.0, 11.0));
        assert_eq!(*size, Pixels(16.0));
        let Text::Raw {
            transformation,
            raw,
            ..
        } = &text[1]
        else {
            panic!("raw section retained")
        };
        assert_eq!(*transformation, translation * local);
        assert_eq!(raw.position, Point::new(4.0, 5.0));
        assert_eq!(raw.position * *transformation, Point::new(15.0, 19.0));
    }

    #[test]
    fn text_batch_translation_preserves_scale_and_nonfinite_groups() {
        for transformation in [
            Transformation::scale(0.5),
            Transformation::translate(f32::INFINITY, 1.0),
            Transformation::translate(f32::NAN, 1.0),
        ] {
            let before: Vec<_> = transformation
                .as_ref()
                .iter()
                .map(|value| value.to_bits())
                .collect();
            let mut item = Item::Group {
                transformation,
                text: vec![cached()],
            };
            item.bake_translation();
            let Item::Group {
                transformation,
                text,
            } = item
            else {
                panic!("group retained")
            };
            assert_eq!(
                transformation
                    .as_ref()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                before
            );
            assert_eq!(text, vec![cached()]);
        }
    }

    #[test]
    fn text_batch_translation_keeps_cached_group_identity_and_version() {
        let cache = Cache::new(cache::Group::unique(), vec![cached()]).unwrap();
        let id = cache.id;
        let version = cache.version;
        let transformation = Transformation::translate(7.0, 9.0);
        let mut item = Item::Cached {
            transformation,
            cache,
        };
        item.bake_translation();
        let Item::Cached {
            transformation: actual,
            cache,
        } = item
        else {
            panic!("cached group retained")
        };
        assert_eq!(actual, transformation);
        assert_eq!(cache.id, id);
        assert_eq!(cache.version, version);
        assert_eq!(&*cache.text, &[cached()]);
    }
}
