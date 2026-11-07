// Modified by enzUI contributors. See FORK.md.
use crate::{Layer, Scene, Segment, core, presentation_bounds};
use core::layout::scroll::{Config, Input};
use core::renderer::presentation::{Bounds, Error, MAX_ITEMS, Program, State};
use core::{Background, Color, Rectangle, Transformation, Vector, renderer};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub bounds: Rectangle,
    pub content: Rectangle,
    pub config: Config,
    pub input: Input,
    pub paint: core::renderer::presentation::scroll_paint::Paint,
    pub chrome: u64,
}
impl Captured {
    fn translation(&self) -> Vector {
        self.config
            .paint_translation(self.input, self.bounds.size(), self.content.size())
    }
    fn quads(
        &self,
        chrome: bool,
        bounds: Rectangle,
        content: Rectangle,
        input: Input,
    ) -> Vec<(renderer::Quad, Background)> {
        let mut result = if chrome {
            self.paint.chrome(self.config, input, bounds, content)
        } else {
            self.paint.background(bounds).into_iter().collect()
        };
        if result.is_empty() {
            result.push((
                renderer::Quad {
                    bounds: Rectangle::new(bounds.position(), core::Size::ZERO),
                    ..Default::default()
                },
                Color::TRANSPARENT.into(),
            ));
        }
        result
    }
    pub fn valid(&self) -> bool {
        let bounds = Rectangle::with_size(core::Size::new(1.0, 1.0));
        let valid = |quad, background| crate::owned_quad::OwnedQuad::new(quad, background).valid();
        self.config.valid()
            && self.input.valid()
            && self
                .paint
                .background
                .is_none_or(|(quad, fill)| valid(quad, fill))
            && self.paint.gap.is_none_or(|fill| {
                valid(
                    renderer::Quad {
                        bounds,
                        ..Default::default()
                    },
                    fill,
                )
            })
            && self.paint.rails.iter().all(|rail| {
                valid(
                    renderer::Quad {
                        bounds,
                        border: rail.border,
                        ..Default::default()
                    },
                    rail.background.unwrap_or(Color::TRANSPARENT.into()),
                ) && valid(
                    renderer::Quad {
                        bounds,
                        border: rail.thumb_border,
                        ..Default::default()
                    },
                    rail.thumb,
                )
            })
    }
}
fn rectangle(bounds: Bounds) -> Rectangle {
    Rectangle {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}
fn intersect(a: Rectangle, b: Rectangle) -> Rectangle {
    a.intersection(&b).unwrap_or(Rectangle {
        x: a.x.max(b.x),
        y: a.y.max(b.y),
        width: 0.0,
        height: 0.0,
    })
}
fn same_clip(a: Rectangle, b: Rectangle) -> bool {
    a == b || (a.width <= 0.0 && a.height <= 0.0 && b.width <= 0.0 && b.height <= 0.0)
}

pub(crate) fn transform(
    program: &Program,
    scrolls: &BTreeMap<u64, Captured>,
    node: u64,
) -> Result<Transformation, Error> {
    let mut offset = Vector::ZERO;
    for id in program.scroll_ancestors(node) {
        offset += scrolls
            .get(&id)
            .ok_or(Error::SceneIdentity(id))?
            .translation();
    }
    Ok(Transformation::translate(-offset.x, -offset.y))
}
fn source_scope(
    program: &Program,
    scrolls: &BTreeMap<u64, Captured>,
    node: u64,
) -> Result<(Rectangle, Vector), Error> {
    let mut clip = rectangle(program.clip());
    let mut offset = Vector::ZERO;
    for id in program.scroll_ancestors(node) {
        let source = scrolls.get(&id).ok_or(Error::SceneIdentity(id))?;
        clip = intersect(clip, source.bounds - offset);
        offset += source.translation();
    }
    Ok((clip, offset))
}
fn owner(scrolls: &BTreeMap<u64, Captured>, node: u64) -> Option<(u64, &Captured, bool)> {
    scrolls.iter().find_map(|(&id, value)| {
        if id == node {
            Some((id, value, false))
        } else if value.chrome == node {
            Some((id, value, true))
        } else {
            None
        }
    })
}
fn source_clip(
    program: &Program,
    scrolls: &BTreeMap<u64, Captured>,
    node: u64,
) -> Result<(Rectangle, Vector), Error> {
    let (clip, offset) = source_scope(program, scrolls, node)?;
    if let Some((_, source, true)) = owner(scrolls, node) {
        let bounds = source.bounds - offset;
        Ok((
            intersect(
                clip,
                Rectangle {
                    width: bounds.width + 2.0,
                    height: bounds.height + 2.0,
                    ..bounds
                },
            ),
            offset,
        ))
    } else {
        Ok((clip, offset))
    }
}
fn check_path(
    program: &Program,
    scrolls: &BTreeMap<u64, Captured>,
    segment: &Segment,
) -> Result<u64, Error> {
    let node = segment.path.last().ok_or(Error::OpaquePaint)?.0;
    let own = owner(scrolls, node).map(|(id, _, _)| id);
    let actual: Vec<_> = segment
        .path
        .iter()
        .filter_map(|id| (scrolls.contains_key(&id.0) && Some(id.0) != own).then_some(id.0))
        .collect();
    if actual != program.scroll_ancestors(node) {
        return Err(Error::SceneIdentity(node));
    }
    Ok(node)
}

pub(crate) fn normalize(scene: &mut Scene, program: &Program) -> Result<(), Error> {
    if scene.scrolls.len() > MAX_ITEMS {
        return Err(Error::Budget);
    }
    for segment in &mut scene.segments {
        if segment.layers.iter().all(Layer::is_empty) {
            continue;
        }
        let Some(last) = segment.path.last() else {
            continue;
        };
        let node = last.0;
        if !program.is_scroll_scoped(node) && owner(&scene.scrolls, node).is_none() {
            continue;
        }
        let node = check_path(program, &scene.scrolls, segment)?;
        let (legacy_clip, offset) = source_clip(program, &scene.scrolls, node)?;
        let clip = match segment
            .layers
            .iter()
            .find(|layer| !layer.is_empty())
            .and_then(|layer| layer.clip_path.as_ref())
        {
            Some(path) => path.evaluate(|_, captured| Ok(captured))?.0,
            None => legacy_clip,
        };
        if !same_clip(segment.clip, clip) {
            return Err(Error::ClipCoverage(node));
        }
        for layer in &mut segment.layers {
            if layer.is_empty() {
                continue;
            }
            let typed = owner(&scene.scrolls, node)
                .map(|(_, source, chrome)| {
                    source.quads(chrome, source.bounds, source.content, source.input)
                })
                .or_else(|| {
                    scene
                        .owned_quads
                        .get(&node)
                        .map(|quad| vec![(quad.quad, quad.background)])
                });
            if let Some(quads) = &typed {
                let mut expected = Layer::default();
                for &(quad, fill) in quads {
                    expected.draw_quad(quad, fill, Transformation::translate(-offset.x, -offset.y));
                }
                if !layer.text.is_empty() || !layer.quads.matches(&expected.quads) {
                    return Err(Error::OpaquePaint);
                }
            }
            #[cfg(any(feature = "image", feature = "svg"))]
            let media = scene
                .media
                .get(&node)
                .map(|capture| {
                    crate::owned_media::normalize_layer(
                        layer,
                        capture,
                        &scene.media_assets,
                        Transformation::translate(-offset.x, -offset.y),
                    )
                })
                .transpose()?;
            layer
                .normalize_scroll(offset, clip, rectangle(program.clip()))
                .map_err(|_| Error::ClipCoverage(node))?;
            #[cfg(any(feature = "image", feature = "svg"))]
            if let Some(images) = media {
                layer.images = images;
            }
            if let Some(quads) = typed {
                layer.quads.clear();
                for (quad, fill) in quads {
                    layer.draw_quad(quad, fill, Transformation::IDENTITY);
                }
            }
        }
        segment.clip = rectangle(program.clip());
    }
    Ok(())
}

pub(crate) fn validate(scene: &Scene, program: &Program) -> Result<(), Error> {
    if scene.scrolls.len() != program.scroll_nodes().count() {
        return Err(Error::OpaquePaint);
    }
    for (id, value) in program.scroll_nodes() {
        let source = scene.scrolls.get(&id).ok_or(Error::SceneIdentity(id))?;
        if source.chrome != value.chrome
            || source.config != value.config
            || presentation_bounds(source.bounds) != value.recorded
            || presentation_bounds(source.content) != value.recorded_content
            || !source.valid()
        {
            return Err(Error::SceneIdentity(id));
        }
        for (paint, chrome) in [(id, false), (value.chrome, true)] {
            let mut count = 0;
            let mut expected = Layer::default();
            for (quad, fill) in source.quads(chrome, source.bounds, source.content, source.input) {
                expected.draw_quad(quad, fill, Transformation::IDENTITY);
            }
            for segment in &scene.segments {
                if segment.path.last().is_none_or(|node| node.0 != paint) {
                    continue;
                }
                let _ = check_path(program, &scene.scrolls, segment)?;
                for layer in &segment.layers {
                    if layer.is_empty() {
                        continue;
                    }
                    if !layer.text.is_empty()
                        || !layer.images.is_empty()
                        || !layer.triangles.is_empty()
                        || !layer.primitives.is_empty()
                        || !layer.quads.matches(&expected.quads)
                    {
                        return Err(Error::OpaquePaint);
                    }
                    count += 1;
                }
            }
            if count != 1 {
                return Err(Error::SceneIdentity(paint));
            }
        }
    }
    for segment in &scene.segments {
        if segment.layers.iter().all(Layer::is_empty) {
            continue;
        }
        let Some(node) = segment.path.last().map(|id| id.0) else {
            continue;
        };
        if !program.is_scroll_scoped(node) && owner(&scene.scrolls, node).is_none() {
            continue;
        }
        let _ = check_path(program, &scene.scrolls, segment)?;
        let (legacy_clip, _) = source_clip(program, &scene.scrolls, node)?;
        let expected = match segment
            .layers
            .iter()
            .find(|layer| !layer.is_empty())
            .and_then(|layer| layer.clip_path.as_ref())
        {
            Some(path) => path.evaluate(|_, captured| Ok(captured))?.0,
            None => legacy_clip,
        };
        let actual = scene
            .node_clips
            .iter()
            .find(|(id, _)| *id == node)
            .ok_or(Error::SceneIdentity(node))?
            .1;
        if !same_clip(actual, expected) {
            return Err(Error::ClipCoverage(node));
        }
    }
    Ok(())
}

pub(crate) fn apply(
    scene: &Scene,
    program: &Program,
    state: &State,
    text: &mut crate::text::State,
    segments: &mut [Segment],
) -> Result<(), Error> {
    for segment in segments {
        let Some(node) = segment.path.last().map(|id| id.0) else {
            continue;
        };
        if !program.is_scroll_scoped(node) && owner(&scene.scrolls, node).is_none() {
            continue;
        }
        let inherited = state.node_clip(node).unwrap_or(program.clip());
        let mut clip = rectangle(inherited);
        if let Some((id, source, chrome)) = owner(&scene.scrolls, node) {
            let bounds = rectangle(state.bounds(id).ok_or(Error::SceneIdentity(id))?);
            let content = rectangle(state.scroll_content(id).ok_or(Error::SceneIdentity(id))?);
            let input = core::renderer::presentation::TextMeasurer::scroll_input(text, id)?;
            if chrome {
                clip = intersect(
                    clip,
                    Rectangle {
                        width: bounds.width + 2.0,
                        height: bounds.height + 2.0,
                        ..bounds
                    },
                );
            }
            let logical = rectangle(
                state
                    .unscrolled_bounds(id)
                    .ok_or(Error::SceneIdentity(id))?,
            );
            let offset = state.scroll_offset(id).ok_or(Error::SceneIdentity(id))?;
            for layer in &mut segment.layers {
                if layer.is_empty() {
                    continue;
                }
                layer.clear_paint();
                for (quad, fill) in source.quads(chrome, logical, content, input) {
                    layer.draw_quad(quad, fill, Transformation::translate(-offset.x, -offset.y));
                }
            }
        }
        if let Some(placement) = state
            .placements()
            .iter()
            .find(|placement| placement.node == node)
        {
            for layer in &mut segment.layers {
                for item in &mut layer.text {
                    if let crate::text::Item::Group { text, .. } = item {
                        for section in text {
                            if let crate::text::Text::Paragraph { clip_bounds, .. } = section {
                                *clip_bounds = clip
                                    - Vector::new(
                                        placement.translation[0],
                                        placement.translation[1],
                                    );
                            }
                        }
                    }
                }
            }
        }
        segment.clip = clip;
        for layer in &mut segment.layers {
            layer.owned_scroll_clip(clip);
        }
    }
    Ok(())
}

impl crate::text::State {
    pub(crate) fn clear_scroll(&mut self) {
        self.scroll_inputs.clear();
        self.scroll_revision = None;
    }

    pub(crate) fn prepare_scroll(&mut self, scene: &Scene) {
        if self
            .scroll_revision
            .as_ref()
            .is_some_and(|revision| Arc::ptr_eq(revision, &scene.layout_revision))
        {
            return;
        }
        self.scroll_inputs = scene
            .scrolls
            .iter()
            .map(|(&id, source)| (id, source.input))
            .collect();
        self.scroll_revision = Some(scene.layout_revision.clone());
    }
}
