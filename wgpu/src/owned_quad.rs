// Modified by enzUI contributors. See FORK.md.
use crate::core::renderer::presentation::{Bounds, Error, MAX_ITEMS, Program, State};
use crate::core::{Background, Color, Rectangle, Transformation, renderer};
use crate::{Layer, Scene, Segment, presentation_bounds};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnedQuad {
    #[cfg(feature = "geometry")]
    pub border: Option<crate::graphics::geometry::border::Border>,
    pub allocation: Rectangle,
    pub rule: Option<crate::core::layout::rule::Paint>,
    pub editor_scrollbars: Option<EditorScrollbars>,
    pub hint_factor: f32,
    pub blink: Option<(
        crate::core::layout::input::Blink,
        crate::core::time::Instant,
    )>,
    pub quad: renderer::Quad,
    pub background: Background,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EditorScrollbars {
    pub editor: u64,
    pub metrics: crate::core::text::editor::ScrollMetrics,
    pub paint: crate::core::text::editor::scrollbar::Paint,
}

impl OwnedQuad {
    pub(crate) fn new(quad: renderer::Quad, background: Background) -> Self {
        Self {
            #[cfg(feature = "geometry")]
            border: None,
            allocation: quad.bounds,
            quad,
            background,
            rule: None,
            editor_scrollbars: None,
            hint_factor: 1.0,
            blink: None,
        }
    }

    pub(crate) fn paint_quads(
        &self,
        bounds: Rectangle,
        metrics: Option<crate::core::text::editor::ScrollMetrics>,
        at: Option<crate::core::time::Instant>,
    ) -> [Option<(renderer::Quad, Background)>; 2] {
        if self
            .blink
            .is_some_and(|(clock, captured)| !clock.visible(at.unwrap_or(captured)))
        {
            return [
                Some((
                    renderer::Quad {
                        bounds: Rectangle::new(bounds.position(), crate::core::Size::ZERO),
                        ..self.quad
                    },
                    Color::TRANSPARENT.into(),
                )),
                None,
            ];
        }
        if let Some(scrollbars) = self.editor_scrollbars {
            let mut quads = scrollbars
                .paint
                .quads(bounds, metrics.unwrap_or(scrollbars.metrics))
                .map(|quad| quad.map(|(quad, color)| (quad, color.into())));
            if quads.iter().all(Option::is_none) {
                quads[0] = Some((
                    renderer::Quad {
                        bounds: Rectangle::new(bounds.position(), crate::core::Size::ZERO),
                        ..Default::default()
                    },
                    Color::TRANSPARENT.into(),
                ));
            }
            quads
        } else {
            [
                Some((
                    renderer::Quad {
                        bounds,
                        ..self.quad
                    },
                    self.background,
                )),
                None,
            ]
        }
    }

    fn bounds(&self, allocation: Rectangle, hint_factor: f32) -> Rectangle {
        self.rule
            .map_or(allocation, |paint| paint.bounds(allocation, hint_factor))
    }

    pub fn valid(&self) -> bool {
        #[cfg(feature = "geometry")]
        if self.border.is_some_and(|border| {
            !border.is_valid() || self.rule.is_some() || self.editor_scrollbars.is_some()
        }) {
            return false;
        }
        let color = |color: Color| {
            [color.r, color.g, color.b, color.a]
                .into_iter()
                .all(f32::is_finite)
        };
        let radii: [f32; 4] = self.quad.border.radius.into();
        let background = match self.background {
            Background::Color(value) => color(value),
            Background::Gradient(crate::core::Gradient::Linear(value)) => {
                value.angle.0.is_finite()
                    && value.stops.iter().flatten().all(|stop| {
                        stop.offset.is_finite()
                            && (0.0..=1.0).contains(&stop.offset)
                            && color(stop.color)
                    })
            }
        };
        self.blink.is_none_or(|(clock, _)| {
            !clock.interval.is_zero() && self.rule.is_none() && self.editor_scrollbars.is_none()
        }) && self.editor_scrollbars.is_none_or(|value| {
            value.paint.valid()
                && self.rule.is_none()
                && value.editor != 0
                && [
                    value.metrics.viewport.width,
                    value.metrics.viewport.height,
                    value.metrics.content.width,
                    value.metrics.content.height,
                    value.metrics.offset.x,
                    value.metrics.offset.y,
                    value.metrics.first_line_top,
                ]
                .into_iter()
                .all(|v| v.is_finite() && v >= 0.0)
        }) && self.hint_factor.is_finite()
            && self.hint_factor > 0.0
            && presentation_bounds(self.allocation).contains(presentation_bounds(self.allocation))
            && self
                .rule
                .is_none_or(|paint| paint.is_valid() && paint.snap == self.quad.snap)
            && self.bounds(self.allocation, self.hint_factor) == self.quad.bounds
            && presentation_bounds(self.quad.bounds).contains(presentation_bounds(self.quad.bounds))
            && radii
                .into_iter()
                .chain([self.quad.border.width, self.quad.shadow.blur_radius])
                .all(|v| v.is_finite() && v >= 0.0)
            && [self.quad.shadow.offset.x, self.quad.shadow.offset.y]
                .into_iter()
                .all(f32::is_finite)
            && color(self.quad.border.color)
            && color(self.quad.shadow.color)
            && background
    }
}

pub(crate) fn validate(scene: &Scene, program: &Program) -> Result<(), Error> {
    if scene.owned_quads.len() > MAX_ITEMS {
        return Err(Error::Budget);
    }
    let mut seen = BTreeSet::new();
    #[cfg(feature = "geometry")]
    let mut border_cost = 0usize;
    for segment in &scene.segments {
        for node in &segment.path {
            if !program.is_container(node.0) {
                continue;
            }
            if segment.path.last() != Some(node) {
                return Err(Error::OpaquePaint);
            }
            let owned = scene.owned_quads.get(&node.0).ok_or(Error::OpaquePaint)?;
            if !owned.valid()
                || owned.blink.map(|(clock, _)| clock) != program.input_blink(node.0)
                || owned.editor_scrollbars.is_some_and(|value| {
                    !scene.editors.contains_key(&value.editor)
                        || !program.contains_descendant(node.0, value.editor)
                })
                || program.container_recorded(node.0) != Some(presentation_bounds(owned.allocation))
            {
                return Err(Error::OpaquePaint);
            }
            if !program.is_clipped_scoped(node.0)
                && !presentation_bounds(segment.clip).contains(program.clip())
            {
                return Err(Error::ClipCoverage(node.0));
            }
            for layer in &segment.layers {
                #[cfg(feature = "geometry")]
                let mut triangles_allowed = false;
                #[cfg(not(feature = "geometry"))]
                let triangles_allowed = false;
                #[cfg(feature = "geometry")]
                if let Some(border) = owned.border {
                    let mut expected = Layer::default();
                    crate::owned_border::paint(
                        &mut expected,
                        owned.quad.bounds,
                        border,
                        Transformation::IDENTITY,
                    );
                    if !crate::owned_border::matches(&layer.triangles, &expected.triangles) {
                        return Err(Error::OpaquePaint);
                    }
                    border_cost = border_cost
                        .checked_add(
                            crate::owned_border::cost(&layer.triangles).ok_or(Error::Budget)?,
                        )
                        .ok_or(Error::Budget)?;
                    if border_cost > MAX_ITEMS {
                        return Err(Error::Budget);
                    }
                    triangles_allowed = true;
                }
                if !layer.text.is_empty()
                    || (!triangles_allowed && !layer.triangles.is_empty())
                    || !layer.primitives.is_empty()
                    || !layer.images.is_empty()
                {
                    return Err(Error::OpaquePaint);
                }
                if layer.quads.is_empty() {
                    continue;
                }
                let mut expected = Layer::default();
                for (quad, background) in owned
                    .paint_quads(owned.quad.bounds, None, None)
                    .into_iter()
                    .flatten()
                {
                    expected.draw_quad(quad, background, Transformation::IDENTITY);
                }
                if !layer.quads.matches(&expected.quads) || !seen.insert(node.0) {
                    return Err(Error::OpaquePaint);
                }
            }
        }
    }
    if seen.len() != scene.owned_quads.len() {
        return Err(Error::OpaquePaint);
    }
    Ok(())
}

pub(crate) fn apply(
    owned: &BTreeMap<u64, OwnedQuad>,
    state: &State,
    hint_factor: f32,
    text: &mut crate::text::State,
    segments: &mut [Segment],
    at: crate::core::time::Instant,
) -> Result<(), Error> {
    for segment in segments {
        let Some(node) = segment.path.last() else {
            continue;
        };
        let Some(owned) = owned.get(&node.0) else {
            continue;
        };
        let Bounds {
            x,
            y,
            width,
            height,
        } = state
            .container_bounds(node.0)
            .ok_or(Error::SceneIdentity(node.0))?;
        if !hint_factor.is_finite() || hint_factor <= 0.0 {
            return Err(Error::NonFinite);
        }
        let bounds = owned.bounds(
            Rectangle {
                x,
                y,
                width,
                height,
            },
            hint_factor,
        );
        if !presentation_bounds(bounds).contains(presentation_bounds(bounds)) {
            return Err(Error::NonFinite);
        }
        let metrics = owned
            .editor_scrollbars
            .map(|value| text.editor_scroll_metrics(value.editor))
            .transpose()?;
        if let (Some(chrome), Some(metrics)) = (owned.editor_scrollbars, metrics)
            && (chrome.paint.projected_metrics(metrics).is_none()
                || chrome
                    .paint
                    .projection
                    .is_some_and(|projection| !projection.covers(metrics)))
        {
            return Err(Error::ReflowRequired(chrome.editor));
        }

        let quads = owned.paint_quads(bounds, metrics, Some(at));
        for layer in &mut segment.layers {
            if layer.quads.is_empty() {
                continue;
            }
            layer.quads.clear();
            #[cfg(feature = "geometry")]
            if let Some(border) = owned.border {
                layer.triangles.clear();
                crate::owned_border::paint(layer, bounds, border, Transformation::IDENTITY);
                if crate::owned_border::cost(&layer.triangles).is_none_or(|cost| cost > MAX_ITEMS) {
                    return Err(Error::Budget);
                }
            }
            for &(quad, background) in quads.iter().flatten() {
                layer.draw_quad(quad, background, Transformation::IDENTITY);
            }
        }
    }
    Ok(())
}
