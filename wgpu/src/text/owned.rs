// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::core::renderer::presentation::Error;
use crate::graphics::text::{Raw, Version, cosmic_text};
use std::collections::HashSet;

#[derive(Debug)]
pub(crate) struct FontAssets {
    snapshot: Arc<crate::graphics::text::font_envelope::FontSnapshot>,
    version: Version,
    glyph_fonts: HashSet<(cosmic_text::fontdb::ID, cosmic_text::fontdb::Weight)>,
}

pub(super) struct RasterFonts {
    pub(super) system: cosmic_text::FontSystem,
    // Last so snapshot admission remains charged through font-system destruction.
    assets: Arc<FontAssets>,
}

impl State {
    pub(crate) fn owned_font_usage(&self) -> crate::graphics::resource::Usage {
        self.raster_fonts
            .as_ref()
            .map_or_else(crate::graphics::resource::Usage::default, |fonts| {
                fonts.assets.snapshot.retained_usage()
            })
    }

    pub(crate) fn owned_font_identity(&self) -> Option<[usize; 2]> {
        self.raster_fonts.as_ref().map(|fonts| {
            [
                Arc::as_ptr(&fonts.assets.snapshot) as usize,
                &fonts.system as *const _ as usize,
            ]
        })
    }

    pub(crate) fn capture_fonts(
        &mut self,
        segments: &mut [crate::Segment],
        editors: &std::collections::BTreeMap<u64, super::editor::Captured>,
    ) -> Result<Option<Arc<FontAssets>>, Error> {
        for layer in segments.iter_mut().flat_map(|segment| &mut segment.layers) {
            crate::graphics::layer::Layer::flush(layer);
        }
        if editors.is_empty()
            && segments
                .iter()
                .flat_map(|segment| &segment.layers)
                .all(|layer| layer.text.is_empty())
        {
            self.capture_assets = None;
            return Ok(None);
        }
        let mut shared = font_system().write().map_err(|_| Error::FontAssets)?;
        let version = shared.version();
        let fonts = shared.raw();
        let mut glyph_fonts = HashSet::new();
        for layer in segments.iter_mut().flat_map(|segment| &mut segment.layers) {
            for item in &mut layer.text {
                match item {
                    Item::Group { text, .. } => {
                        for section in text {
                            *section = prepare_owned(section, &mut self.cache, fonts)?;
                            collect_fonts(section, &mut glyph_fonts)?;
                        }
                    }
                    Item::Cached { cache, .. } => {
                        let sections = cache
                            .text
                            .iter()
                            .map(|section| {
                                let owned = prepare_owned(section, &mut self.cache, fonts)?;
                                collect_fonts(&owned, &mut glyph_fonts)?;
                                Ok(owned)
                            })
                            .collect::<Result<Vec<_>, Error>>()?;
                        cache.text = sections.into();
                    }
                }
            }
        }
        let mut envelope = crate::graphics::text::font_envelope::FontEnvelope::default();
        for editor in editors.values() {
            editor.snapshot.include_fonts(&mut envelope)?;
            if let Some(probe) = &editor.gutter_probe {
                envelope.add_buffer(probe)?;
            }
        }
        for layer in segments.iter().flat_map(|segment| &segment.layers) {
            for item in &layer.text {
                let sections = match item {
                    Item::Group { text, .. } => text.as_slice(),
                    Item::Cached { cache, .. } => cache.text.as_ref(),
                };
                for section in sections {
                    match section {
                        Text::Paragraph {
                            retained: Some(paragraph),
                            ..
                        } => envelope.add_buffer(paragraph.buffer())?,
                        Text::Raw {
                            retained: Some(raw),
                            ..
                        } => envelope.add_buffer(raw.buffer())?,
                        _ => return Err(Error::FontAssets),
                    }
                }
            }
        }
        let previous = self
            .capture_assets
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let snapshot = envelope.capture(
            fonts,
            previous.as_ref().map(|assets| &assets.snapshot),
            crate::graphics::text::font_envelope::MAX_FONT_BYTES,
        )?;
        if let Some(previous) = previous
            && previous.version == version
            && previous.glyph_fonts == glyph_fonts
            && Arc::ptr_eq(&previous.snapshot, &snapshot)
        {
            return Ok(Some(previous));
        }

        let assets = Arc::new(FontAssets {
            snapshot,
            version,
            glyph_fonts,
        });
        self.capture_assets = Some(Arc::downgrade(&assets));
        Ok(Some(assets))
    }

    pub(crate) fn begin_font_frame(&mut self) {
        self.frame_font_claimed = false;
    }

    pub(crate) fn retire_owned_fonts(&mut self) {
        self.capture_assets = None;
        self.raster_fonts = None;
        self.frame_font_claimed = false;
        self.clear_font_caches();
    }

    pub(crate) fn use_owned_fonts(
        &mut self,
        assets: Option<&Arc<FontAssets>>,
    ) -> Result<bool, Error> {
        let Some(assets) = assets else {
            if self.frame_font_claimed {
                return Ok(false);
            }
            let changed = self.raster_fonts.take().is_some();
            if changed {
                self.clear_font_caches();
            }
            return Ok(changed);
        };
        let changed = self
            .raster_fonts
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(&current.assets.snapshot, &assets.snapshot));
        if self.frame_font_claimed && changed {
            return Err(Error::FontAssets);
        }
        self.frame_font_claimed = true;
        if !changed {
            self.raster_fonts.as_mut().unwrap().assets = assets.clone();
            return Ok(false);
        }
        self.raster_fonts = Some(RasterFonts {
            system: assets.snapshot.font_system(),
            assets: assets.clone(),
        });
        self.clear_font_caches();
        Ok(true)
    }

    fn clear_font_caches(&mut self) {
        self.renderers.clear();
        self.prepare_layer = 0;
        self.cache = BufferCache::default();
        self.storage = Storage::default();
        self.clear_reflow();
        self.clear_editors();
    }
}

pub(crate) fn retain_section(section: &Text) -> Result<Text, Error> {
    if matches!(section, Text::Editor { .. }) {
        // Capture runs on the UI thread; independent replay does not enter this font lock.
        let mut fonts = font_system().write().map_err(|_| Error::FontAssets)?;
        section.retain_editor(fonts.raw())
    } else {
        section.retain().ok_or(Error::OpaquePaint)
    }
}

fn collect_fonts(
    section: &Text,
    output: &mut HashSet<(cosmic_text::fontdb::ID, cosmic_text::fontdb::Weight)>,
) -> Result<(), Error> {
    match section {
        Text::Paragraph {
            retained: Some(paragraph),
            ..
        } => {
            for glyph in paragraph.buffer().layout_runs().flat_map(|run| run.glyphs) {
                let _ = output.insert((glyph.font_id, glyph.font_weight));
            }
        }
        Text::Raw {
            retained: Some(raw),
            ..
        } => {
            for glyph in raw.buffer().layout_runs().flat_map(|run| run.glyphs) {
                let _ = output.insert((glyph.font_id, glyph.font_weight));
            }
        }
        _ => return Err(Error::FontAssets),
    }
    Ok(())
}

fn prepare_owned(
    section: &Text,
    cache: &mut BufferCache,
    fonts: &mut cosmic_text::FontSystem,
) -> Result<Text, Error> {
    let Text::Cached {
        content,
        bounds,
        color,
        size,
        line_height,
        font,
        align_x,
        align_y,
        shaping,
        wrapping,
        ellipsis,
        clip_bounds,
    } = section
    else {
        return section.retain_editor(fonts);
    };
    let (_, entry) = cache.allocate(
        fonts,
        text_cache::Key {
            content,
            size: f32::from(*size),
            line_height: f32::from(*line_height),
            font: *font,
            align_x: *align_x,
            bounds: bounds.size(),
            shaping: *shaping,
            wrapping: *wrapping,
            ellipsis: *ellipsis,
        },
    );
    let mut position = bounds.position();
    position.x -= match align_x {
        Alignment::Center => entry.min_bounds.width / 2.0,
        Alignment::Right => entry.min_bounds.width,
        _ => 0.0,
    };
    position.y -= match align_y {
        alignment::Vertical::Center => entry.min_bounds.height / 2.0,
        alignment::Vertical::Bottom => entry.min_bounds.height,
        alignment::Vertical::Top => 0.0,
    };
    let buffer = Arc::new(entry.buffer.clone());
    Text::Raw {
        raw: Raw {
            buffer: Arc::downgrade(&buffer),
            position,
            color: *color,
            clip_bounds: *clip_bounds,
        },
        retained: None,
        transformation: Transformation::IDENTITY,
    }
    .retain()
    .ok_or(Error::FontAssets)
}

#[cfg(test)]
mod tests;
