// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::core::renderer::presentation::{Error, MAX_ITEMS};
use crate::core::text::editor::paint::Paint;
use crate::graphics::text::editor::snapshot::{Replay, Snapshot};
use crate::graphics::text::{Raw, RetainedRaw, cosmic_text};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub snapshot: Arc<Snapshot>,
    pub paint: Paint<'static, crate::core::Font>,
    pub empty: bool,
    pub gutter_probe: Option<Arc<cosmic_text::Buffer>>,
}

struct Entry {
    source: Captured,
    replay: Replay,
    placeholder: Option<Arc<cosmic_text::Buffer>>,
    gutter_labels: BTreeMap<usize, (Arc<cosmic_text::Buffer>, f32)>,
}

#[derive(Default)]
pub(super) struct Editors {
    revision: Option<Arc<()>>,
    entries: BTreeMap<u64, Entry>,
}

impl State {
    pub(crate) fn capture_native_editors(&mut self) -> Result<crate::native_input::Frame, Error> {
        let mut frame = crate::native_input::Frame::default();
        let mut items = 0usize;
        for (&node, entry) in &mut self.editors.entries {
            let geometry = entry.replay.input_geometry(entry.source.snapshot.clone())?;
            items = items
                .checked_add(geometry.retained_items())
                .ok_or(Error::Budget)?;
            if items > MAX_ITEMS || frame.editors.len() >= MAX_ITEMS {
                return Err(Error::Budget);
            }
            let _ = frame.editors.insert(node, Arc::new(geometry));
        }
        Ok(frame)
    }

    pub(crate) fn clear_editors(&mut self) {
        self.editors = Editors::default();
    }

    pub(crate) fn prepare_editors(&mut self, scene: &crate::Scene) -> Result<(), Error> {
        if self
            .editors
            .revision
            .as_ref()
            .is_some_and(|revision| Arc::ptr_eq(revision, &scene.layout_revision))
        {
            return Ok(());
        }
        self.editors = Editors::default();
        if scene.editors.len() > MAX_ITEMS {
            return Err(Error::Budget);
        }
        if scene.editors.is_empty() {
            self.editors.revision = Some(scene.layout_revision.clone());
            return Ok(());
        }
        let fonts = &mut self.raster_fonts.as_mut().ok_or(Error::FontAssets)?.system;
        for (&node, source) in &scene.editors {
            let bounds = source.paint.text_bounds().size();
            let replay = source.snapshot.replay(fonts, bounds)?;
            let placeholder = if source.empty {
                source.paint.placeholder.as_ref().map(|value| {
                    let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics::new(
                        source.paint.size.0,
                        source.paint.line_height.to_absolute(source.paint.size).0,
                    ));
                    buffer.set_text(
                        value,
                        &crate::graphics::text::to_attributes(source.paint.font),
                        cosmic_text::Shaping::Advanced,
                        None,
                    );
                    buffer.set_wrap(crate::graphics::text::to_wrap(source.paint.wrapping));
                    Arc::new(buffer)
                })
            } else {
                None
            };
            let _ = self.editors.entries.insert(
                node,
                Entry {
                    source: source.clone(),
                    replay,
                    placeholder,
                    gutter_labels: BTreeMap::new(),
                },
            );
        }
        self.editors.revision = Some(scene.layout_revision.clone());
        Ok(())
    }

    pub(crate) fn measure_editor_view(
        &mut self,
        node: u64,
        width: f32,
        height: f32,
    ) -> Result<[f32; 2], Error> {
        let entry = self
            .editors
            .entries
            .get_mut(&node)
            .ok_or(Error::TextIdentity(node))?;
        let fonts = &mut self.raster_fonts.as_mut().ok_or(Error::FontAssets)?.system;
        entry.replay.reflow(fonts, Size::new(width, height))?;
        if let Some(buffer) = &mut entry.placeholder {
            let buffer = Arc::make_mut(buffer);
            buffer.set_size(Some(width), Some(height));
            buffer.shape_until_scroll(fonts, false);
        }
        let minimum = crate::graphics::text::measure(entry.replay.buffer()).0
            * (1.0 / entry.source.snapshot.hint_factor().unwrap_or(1.0));
        Ok([minimum.width, minimum.height])
    }

    pub(crate) fn editor_scroll_metrics(
        &mut self,
        node: u64,
    ) -> Result<crate::core::text::editor::ScrollMetrics, Error> {
        self.editors
            .entries
            .get_mut(&node)
            .ok_or(Error::TextIdentity(node))?
            .replay
            .scroll_metrics()
    }

    pub(crate) fn apply_editors(
        &mut self,
        placements: &[crate::core::renderer::presentation::Placement],
        segments: &mut [crate::Segment],
    ) -> Result<(), Error> {
        for segment in segments {
            let Some(node) = segment.path.last() else {
                continue;
            };
            let Some(entry) = self.editors.entries.get_mut(&node.0) else {
                continue;
            };
            let placement = placements
                .iter()
                .find(|placement| placement.node == node.0)
                .ok_or(Error::TextIdentity(node.0))?;
            let mut bounds = entry.source.paint.bounds;
            bounds.width = placement.bounds.width;
            bounds.height = placement.bounds.height;
            let paint = entry.source.paint.at_bounds(bounds);
            let pairs: Vec<_> = paint
                .highlights
                .iter()
                .map(|(range, _)| (range.start, range.end))
                .collect();
            let highlights = entry.replay.highlight_ranges(&pairs)?;
            let backgrounds = paint.background_quads(&highlights);
            let selection = if let Some(caret) = entry.replay.caret() {
                crate::core::text::editor::Selection::Caret(caret)
            } else {
                crate::core::text::editor::Selection::Range(entry.replay.selection_regions())
            };
            let selection = paint.selection_quads(selection);
            let remaining = MAX_ITEMS
                .checked_sub(backgrounds.len() + selection.len())
                .ok_or(Error::Budget)?;
            let decorations = entry.replay.decorations(remaining)?;
            let mut gutter_text = Vec::new();
            if let Some(gutter) = paint.gutter {
                let viewport = gutter.bounds(paint.bounds, paint.padding);
                let fonts = &mut self.raster_fonts.as_mut().ok_or(Error::FontAssets)?.system;
                for row in entry.replay.line_number_rows()? {
                    if !entry.gutter_labels.contains_key(&row.line) {
                        let number = gutter.number(row).ok_or(Error::Budget)?;
                        if entry.gutter_labels.len() >= 4096 {
                            return Err(Error::Budget);
                        }
                        let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics::new(
                            paint.size.0,
                            row.height,
                        ));
                        buffer.set_text(
                            &number,
                            &crate::graphics::text::to_attributes(paint.font),
                            cosmic_text::Shaping::Advanced,
                            None,
                        );
                        buffer.set_wrap(cosmic_text::Wrap::None);
                        buffer.set_size(Some(viewport.width), Some(row.height));
                        buffer.shape_until_scroll(fonts, false);
                        let minimum = crate::graphics::text::align(
                            &mut buffer,
                            fonts,
                            crate::core::text::Alignment::Right,
                        );
                        let _ = entry
                            .gutter_labels
                            .insert(row.line, (Arc::new(buffer), minimum.width));
                    }
                    let (buffer, width) = entry
                        .gutter_labels
                        .get(&row.line)
                        .ok_or(Error::TextIdentity(node.0))?;
                    gutter_text.push(
                        Text::Raw {
                            raw: Raw {
                                buffer: Arc::downgrade(buffer),
                                position: crate::core::Point::new(
                                    viewport.x + viewport.width - *width,
                                    viewport.y + row.top,
                                ),
                                color: gutter.color.unwrap_or(paint.value),
                                clip_bounds: viewport,
                            },
                            retained: None,
                            transformation: Transformation::IDENTITY,
                        }
                        .retain()
                        .ok_or(Error::OpaquePaint)?,
                    );
                }
            }
            let mut painted = false;
            for layer in &mut segment.layers {
                if layer.is_empty() {
                    continue;
                }
                if painted {
                    return Err(Error::OpaquePaint);
                }
                painted = true;
                layer.clear_paint();
                layer.draw_quad(
                    crate::core::renderer::Quad {
                        bounds,
                        border: paint.border,
                        ..Default::default()
                    },
                    paint.background,
                    Transformation::IDENTITY,
                );
                for (quad, color) in backgrounds.iter().chain(selection.iter()) {
                    layer.draw_quad(*quad, (*color).into(), Transformation::IDENTITY);
                }
                for (quad, color) in paint.text_decoration_quads(&decorations) {
                    layer.draw_quad(quad, color.into(), Transformation::IDENTITY);
                }
                let text_bounds = paint.text_bounds();
                let text = if entry.source.empty {
                    entry
                        .placeholder
                        .as_ref()
                        .map(|buffer| {
                            Text::Raw {
                                raw: Raw {
                                    buffer: Arc::downgrade(buffer),
                                    position: text_bounds.position(),
                                    color: paint.placeholder_color,
                                    clip_bounds: text_bounds,
                                },
                                retained: None,
                                transformation: Transformation::IDENTITY,
                            }
                            .retain()
                            .ok_or(Error::OpaquePaint)
                        })
                        .transpose()?
                } else {
                    let buffer = entry.replay.shared_buffer();
                    Some(Text::Raw {
                        raw: Raw {
                            buffer: Arc::downgrade(&buffer),
                            position: text_bounds.position(),
                            color: paint.value,
                            clip_bounds: text_bounds,
                        },
                        retained: Some(RetainedRaw::from_editor(
                            buffer,
                            entry.source.snapshot.clone(),
                        )),
                        transformation: Transformation::IDENTITY,
                    })
                };
                if let Some(text) = text {
                    layer.draw_text_group(vec![text], Transformation::IDENTITY);
                }
                if !gutter_text.is_empty() {
                    layer.draw_text_group(
                        std::mem::take(&mut gutter_text),
                        Transformation::IDENTITY,
                    );
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn validate(
    scene: &crate::Scene,
    program: &crate::core::renderer::presentation::Program,
) -> Result<(), Error> {
    for (node, declaration) in program.editor_nodes() {
        let captured = scene.editors.get(&node).ok_or(Error::OpaquePaint)?;
        if crate::presentation_bounds(captured.paint.bounds) != declaration.recorded
            || captured.paint.padding != declaration.padding
        {
            return Err(Error::SceneIdentity(node));
        }
        let mut count = 0;
        for segment in &scene.segments {
            if !segment.path.iter().any(|id| id.0 == node) {
                continue;
            }
            if segment.path.last().is_none_or(|id| id.0 != node) {
                return Err(Error::OpaquePaint);
            }
            for layer in &segment.layers {
                if !layer.primitives.is_empty()
                    || !layer.images.is_empty()
                    || !layer.triangles.is_empty()
                {
                    return Err(Error::OpaquePaint);
                }
                if !layer.is_empty() {
                    count += 1;
                }
            }
        }
        if count != 1 {
            return Err(Error::OpaquePaint);
        }
    }
    if scene.editors.len() != program.editor_nodes().count() {
        return Err(Error::OpaquePaint);
    }
    Ok(())
}
