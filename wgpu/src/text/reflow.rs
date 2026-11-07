// Modified by enzUI contributors. See FORK.md.
use super::{Item, State, Text};
use crate::core::renderer::presentation::{Bounds, Error, Program, TextMeasurer};
use crate::core::text::Paragraph as _;
use crate::core::{Point, Rectangle, Size, Transformation, Vector};
use crate::graphics::text::Paragraph;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Reflow {
    revision: Option<Arc<()>>,
    program: Option<Arc<Program>>,
    entries: Vec<Entry>,
}

struct Entry {
    node: u64,
    recorded: Bounds,
    paragraph: Paragraph,
    offset: Vector,
    allocation: Option<Size>,
    placed: Option<Size>,
    translation: Vector,
    clip: Option<Bounds>,
    intrinsic: Size,
}

fn rectangle(bounds: Bounds) -> Rectangle {
    Rectangle {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

impl State {
    pub(crate) fn prepare_reflow(
        &mut self,
        scene: &crate::Scene,
        program: &Arc<Program>,
    ) -> Result<(), Error> {
        if self
            .reflow
            .revision
            .as_ref()
            .is_some_and(|revision| Arc::ptr_eq(revision, &scene.layout_revision))
            && self
                .reflow
                .program
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, program))
        {
            return Ok(());
        }
        self.reflow = Reflow::default();
        let mut nodes: Vec<_> = program.text_nodes().collect();
        nodes.sort_unstable_by_key(|&(node, _)| node);
        if nodes.is_empty() {
            return Ok(());
        }
        if self.raster_fonts.is_none() {
            return Err(Error::FontAssets);
        }
        let mut entries: Vec<Option<Entry>> = (0..nodes.len()).map(|_| None).collect();
        for segment in &scene.segments {
            for node in &segment.path {
                let Ok(index) = nodes.binary_search_by_key(&node.0, |&(id, _)| id) else {
                    continue;
                };
                if segment.path.last() != Some(node) {
                    return Err(Error::TextIdentity(node.0));
                }
                for layer in &segment.layers {
                    if !layer.quads.is_empty()
                        || !layer.triangles.is_empty()
                        || !layer.primitives.is_empty()
                        || !layer.images.is_empty()
                    {
                        return Err(Error::TextIdentity(node.0));
                    }
                    for item in &layer.text {
                        let Item::Group {
                            transformation,
                            text,
                        } = item
                        else {
                            return Err(Error::TextIdentity(node.0));
                        };
                        if *transformation != Transformation::IDENTITY {
                            return Err(Error::TextIdentity(node.0));
                        }
                        for section in text {
                            let Text::Paragraph {
                                retained: Some(paragraph),
                                position,
                                transformation,
                                ..
                            } = section
                            else {
                                return Err(Error::TextIdentity(node.0));
                            };
                            if entries[index].is_some()
                                || *transformation != Transformation::IDENTITY
                            {
                                return Err(Error::TextIdentity(node.0));
                            }
                            let recorded = nodes[index].1;
                            let anchor = rectangle(recorded).anchor(
                                paragraph.min_bounds(),
                                paragraph.align_x(),
                                paragraph.align_y(),
                            );
                            entries[index] = Some(Entry {
                                node: node.0,
                                recorded,
                                paragraph: paragraph.clone(),
                                offset: *position - anchor,
                                allocation: None,
                                placed: None,
                                translation: Vector::ZERO,
                                clip: None,
                                intrinsic: paragraph.min_bounds(),
                            });
                        }
                    }
                }
            }
        }
        self.reflow.entries = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| entry.ok_or(Error::TextIdentity(nodes[index].0)))
            .collect::<Result<Vec<_>, _>>()?;
        self.reflow.program = Some(program.clone());
        self.reflow.revision = Some(scene.layout_revision.clone());
        Ok(())
    }

    pub(crate) fn place_reflow(
        &mut self,
        placements: &[crate::core::renderer::presentation::Placement],
    ) {
        for placement in placements {
            if let Ok(index) = self
                .reflow
                .entries
                .binary_search_by_key(&placement.node, |entry| entry.node)
            {
                self.reflow.entries[index].placed =
                    Some(Size::new(placement.bounds.width, placement.bounds.height));
                self.reflow.entries[index].translation =
                    Vector::new(placement.translation[0], placement.translation[1]);
            }
        }
    }

    pub(crate) fn apply_reflow(&self, segments: &mut [crate::Segment]) -> Result<(), Error> {
        for segment in segments {
            let Some(node) = segment.path.last() else {
                continue;
            };
            let Ok(index) = self
                .reflow
                .entries
                .binary_search_by_key(&node.0, |entry| entry.node)
            else {
                continue;
            };
            let entry = &self.reflow.entries[index];
            let allocation = entry.placed.ok_or(Error::TextIdentity(node.0))?;
            // Native `Text::draw` paints nothing when both its allocation and
            // its paragraph bounds are empty on an axis; a replay must agree.
            let bounds = crate::core::text::Paragraph::bounds(&entry.paragraph);
            let collapsed = (allocation.width == 0.0 && bounds.width == 0.0)
                || (allocation.height == 0.0 && bounds.height == 0.0);
            for item in segment.layers.iter_mut().flat_map(|layer| &mut layer.text) {
                let Item::Group { text, .. } = item else {
                    return Err(Error::TextIdentity(node.0));
                };
                for section in text {
                    let Text::Paragraph {
                        paragraph,
                        retained,
                        position,
                        clip_bounds,
                        ..
                    } = section
                    else {
                        return Err(Error::TextIdentity(node.0));
                    };
                    *paragraph = entry.paragraph.downgrade();
                    *retained = Some(entry.paragraph.clone());
                    if let Some(clip) = entry.clip {
                        *clip_bounds = rectangle(clip);
                    }
                    // The captured viewport stays fixed when layout moves the paragraph.
                    *clip_bounds -= entry.translation;
                    if collapsed {
                        *clip_bounds = Rectangle::new(clip_bounds.position(), Size::ZERO);
                    }
                    *position =
                        Rectangle::new(Point::new(entry.recorded.x, entry.recorded.y), allocation)
                            .anchor(
                                entry.intrinsic,
                                entry.paragraph.align_x(),
                                entry.paragraph.align_y(),
                            )
                            + entry.offset;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn clear_reflow(&mut self) {
        self.reflow = Reflow::default();
    }
}

impl TextMeasurer for State {
    fn text_clip(&mut self, node: u64, clip: Option<Bounds>) {
        if let Ok(index) = self
            .reflow
            .entries
            .binary_search_by_key(&node, |entry| entry.node)
        {
            self.reflow.entries[index].clip = clip;
        }
    }

    fn scroll_input(&mut self, node: u64) -> Result<crate::core::layout::scroll::Input, Error> {
        self.scroll_inputs
            .get(&node)
            .copied()
            .ok_or(Error::SceneIdentity(node))
    }
    fn scroll_visible(&mut self, node: u64, visible: bool) {
        if let Some(input) = self.scroll_inputs.get_mut(&node) {
            input.visible = visible;
        }
    }

    fn measure_editor(&mut self, node: u64, width: f32, height: f32) -> Result<[f32; 2], Error> {
        self.measure_editor_view(node, width, height)
    }

    fn probe(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
        if width.is_nan() || width <= 0.0 || height.is_some_and(|h| !h.is_finite() || h <= 0.0) {
            return Err(Error::NonFinite);
        }
        let index = self
            .reflow
            .entries
            .binary_search_by_key(&node, |entry| entry.node)
            .map_err(|_| Error::TextIdentity(node))?;
        let mut paragraph = self.reflow.entries[index].paragraph.clone();
        let fonts = self.raster_fonts.as_mut().ok_or(Error::FontAssets)?;
        paragraph.resize_owned(
            &mut fonts.system,
            Size::new(width, height.unwrap_or(f32::INFINITY)),
        );
        let intrinsic = paragraph.min_bounds();
        if !intrinsic.width.is_finite() || !intrinsic.height.is_finite() {
            return Err(Error::NonFinite);
        }
        Ok([intrinsic.width, intrinsic.height])
    }

    fn measure(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
        let index = self
            .reflow
            .entries
            .binary_search_by_key(&node, |entry| entry.node)
            .map_err(|_| Error::TextIdentity(node))?;
        let entry = &mut self.reflow.entries[index];
        let bounds = Size::new(width, height.unwrap_or(f32::MAX));
        if entry.allocation != Some(bounds) {
            let fonts = self.raster_fonts.as_mut().ok_or(Error::FontAssets)?;
            entry.paragraph.resize_owned(&mut fonts.system, bounds);
            entry.allocation = Some(bounds);
            entry.intrinsic = entry.paragraph.min_bounds();
        }
        Ok([entry.intrinsic.width, entry.intrinsic.height])
    }
}
