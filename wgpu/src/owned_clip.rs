// Modified by enzUI contributors. See FORK.md.
use crate::{Scene, Segment, core, owned_scroll};
use core::Rectangle;
use core::renderer::{
    LayoutLayer,
    presentation::{Bounds, Error, MAX_ITEMS, Program, State},
};
use std::collections::BTreeMap;

fn rect(bounds: Bounds) -> Rectangle {
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
fn valid(bounds: Rectangle) -> bool {
    [
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
        bounds.x + bounds.width,
        bounds.y + bounds.height,
    ]
    .into_iter()
    .all(f32::is_finite)
        && bounds.width >= 0.0
        && bounds.height >= 0.0
}

pub(crate) fn capture_viewport(
    program: &Program,
    scrolls: &BTreeMap<u64, owned_scroll::Captured>,
    node: u64,
) -> Result<Rectangle, Error> {
    let mut viewport = rect(program.clip());
    for ancestor in program.viewport_ancestors(node) {
        if let Some(slot) = program.material_slot(ancestor) {
            let bounds = slot.recorded.ok_or(Error::SceneIdentity(ancestor))?;
            if bounds.width == 0.0 && bounds.height == 0.0 {
                viewport.width = 0.0;
                viewport.height = 0.0;
            }
            continue;
        }
        let bounds = rect(
            program
                .recorded_viewport(ancestor)
                .ok_or(Error::ClipCoverage(ancestor))?,
        ) * owned_scroll::transform(program, scrolls, ancestor)?;
        if !valid(bounds) {
            return Err(Error::NonFinite);
        }
        viewport = intersect(viewport, bounds);
    }
    Ok(viewport)
}

pub(crate) fn capture_source(
    program: &Program,
    scrolls: &BTreeMap<u64, owned_scroll::Captured>,
    source: LayoutLayer,
) -> Result<Rectangle, Error> {
    if let LayoutLayer::Cull(scope) = source {
        let cull = program
            .cull_source(scope.0)
            .ok_or(Error::SceneIdentity(scope.0))?;
        let viewport = capture_viewport(program, scrolls, cull.owner)?;
        let bounds = rect(cull.recorded) * owned_scroll::transform(program, scrolls, cull.owner)?;
        return Ok(if bounds.intersection(&viewport).is_some() {
            rect(program.clip())
        } else {
            Rectangle {
                width: 0.0,
                height: 0.0,
                ..rect(program.clip())
            }
        });
    }
    if let LayoutLayer::Material(node) = source {
        let slot = program
            .material_slot(node.0)
            .ok_or(Error::SceneIdentity(node.0))?;
        let bounds = slot.recorded.ok_or(Error::SceneIdentity(node.0))?;
        let mut viewport = capture_viewport(program, scrolls, node.0)?;
        if bounds.width == 0.0 && bounds.height == 0.0 {
            viewport.width = 0.0;
            viewport.height = 0.0;
        }
        return Ok(viewport);
    }
    let node = match source {
        LayoutLayer::Material(_) | LayoutLayer::Cull(_) => unreachable!(),
        LayoutLayer::Clip(node) => {
            let _ = program
                .clip_node(node.0)
                .ok_or(Error::SceneIdentity(node.0))?;
            node
        }
        LayoutLayer::Scroll { node, .. } => {
            let _ = scrolls.get(&node.0).ok_or(Error::SceneIdentity(node.0))?;
            node
        }
        LayoutLayer::TabStrip(node) => {
            let _ = program
                .tab_strip_node(node.0)
                .ok_or(Error::SceneIdentity(node.0))?;
            node
        }
        LayoutLayer::BandContent(node) => {
            let _ = program
                .band_node(node.0)
                .ok_or(Error::SceneIdentity(node.0))?;
            node
        }
        LayoutLayer::Inherited => return Err(Error::RecordingState),
    };
    let viewport = capture_viewport(program, scrolls, node.0)?;
    let bounds = rect(
        program
            .recorded_viewport(node.0)
            .ok_or(Error::SceneIdentity(node.0))?,
    ) * owned_scroll::transform(program, scrolls, node.0)?;
    let visible = intersect(bounds, viewport);
    let result = match source {
        LayoutLayer::Scroll { chrome: true, .. } => Rectangle {
            width: (visible.width + 2.0).min(viewport.width),
            height: (visible.height + 2.0).min(viewport.height),
            ..visible
        },
        _ => visible,
    };
    if valid(result) {
        Ok(result)
    } else {
        Err(Error::NonFinite)
    }
}

fn live_source(program: &Program, state: &State, source: LayoutLayer) -> Result<Rectangle, Error> {
    let node = match source {
        LayoutLayer::Clip(node)
        | LayoutLayer::BandContent(node)
        | LayoutLayer::TabStrip(node)
        | LayoutLayer::Material(node)
        | LayoutLayer::Cull(node)
        | LayoutLayer::Scroll { node, .. } => node,
        LayoutLayer::Inherited => return Err(Error::RecordingState),
    };
    let viewport = rect(
        state
            .incoming_viewport(node.0)
            .ok_or(Error::SceneIdentity(node.0))?,
    );
    if matches!(source, LayoutLayer::Cull(_)) {
        let cull = program
            .cull_source(node.0)
            .ok_or(Error::SceneIdentity(node.0))?;
        let bounds = rect(
            state
                .bounds(cull.owner)
                .ok_or(Error::SceneIdentity(cull.owner))?,
        );
        return Ok(if bounds.intersection(&viewport).is_some() {
            rect(program.clip())
        } else {
            Rectangle {
                width: 0.0,
                height: 0.0,
                ..rect(program.clip())
            }
        });
    }
    if matches!(source, LayoutLayer::Material(_)) {
        let bounds = state.bounds(node.0).ok_or(Error::SceneIdentity(node.0))?;
        return Ok(if bounds.width == 0.0 && bounds.height == 0.0 {
            Rectangle {
                width: 0.0,
                height: 0.0,
                ..viewport
            }
        } else {
            viewport
        });
    }
    let mut bounds = rect(
        if matches!(source, LayoutLayer::BandContent(_)) {
            state.band_content_clip(node.0)
        } else {
            state.bounds(node.0)
        }
        .ok_or(Error::SceneIdentity(node.0))?,
    );
    if let LayoutLayer::Clip(_) = source {
        let clip = program
            .clip_node(node.0)
            .ok_or(Error::SceneIdentity(node.0))?;
        bounds = if clip.reveal.is_some() {
            rect(
                state
                    .reveal_clip(node.0)
                    .ok_or(Error::SceneIdentity(node.0))?,
            )
        } else {
            core::layout::clip::bounds(bounds, clip.outset)
        };
    }
    let visible = intersect(bounds, viewport);
    let result = match source {
        LayoutLayer::Scroll { chrome: true, .. } => Rectangle {
            width: (visible.width + 2.0).min(viewport.width),
            height: (visible.height + 2.0).min(viewport.height),
            ..visible
        },
        _ => visible,
    };
    if valid(result) {
        Ok(result)
    } else {
        Err(Error::NonFinite)
    }
}

pub(crate) fn validate(scene: &Scene, program: &Program) -> Result<(), Error> {
    validate_scene(scene, program, true)
}

pub(crate) fn validate_sources(scene: &Scene, program: &Program) -> Result<(), Error> {
    validate_scene(scene, program, false)
}

fn validate_scene(scene: &Scene, program: &Program, captured_layers: bool) -> Result<(), Error> {
    let mut layers = 0;
    for segment in &scene.segments {
        let expected = segment
            .path
            .last()
            .map_or_else(Vec::new, |node| program.layer_ancestors(node.0));
        let marked: Vec<_> = segment
            .path
            .iter()
            .filter_map(|node| program.is_layer_scope_node(node.0).then_some(*node))
            .collect();
        let declared: Vec<_> = expected
            .iter()
            .filter_map(|source| match source {
                LayoutLayer::Clip(node)
                | LayoutLayer::BandContent(node)
                | LayoutLayer::TabStrip(node)
                | LayoutLayer::Material(node)
                | LayoutLayer::Cull(node) => Some(*node),
                _ => None,
            })
            .collect();
        for layer in segment.layers.iter().filter(|layer| !layer.is_empty()) {
            layers += 1;
            if layers > MAX_ITEMS {
                return Err(Error::Budget);
            }
            if marked != declared {
                return Err(Error::OpaquePaint);
            }
            let path = layer.clip_path.as_ref().ok_or(Error::OpaquePaint)?;
            let mut sources = Vec::new();
            let (bounds, radius) = path.evaluate(|source, captured| {
                sources.push(source);
                let expected = capture_source(program, &scene.scrolls, source)?;
                if captured != expected {
                    return Err(Error::ClipCoverage(
                        segment.path.last().map_or(0, |id| id.0),
                    ));
                }
                Ok(expected)
            })?;
            if sources != expected {
                return Err(Error::OpaquePaint);
            }
            if captured_layers
                && (bounds != layer.bounds || radius != layer.clip_radius || bounds != segment.clip)
            {
                return Err(Error::OpaquePaint);
            }
            if let Some(node) = segment.path.last() {
                let captured = scene
                    .node_clips
                    .iter()
                    .find(|(id, _)| *id == node.0)
                    .ok_or(Error::SceneIdentity(node.0))?
                    .1;
                if captured != bounds {
                    return Err(Error::ClipCoverage(node.0));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn apply(
    program: &Program,
    state: &State,
    segments: &mut [Segment],
) -> Result<(), Error> {
    for segment in segments {
        for layer in &mut segment.layers {
            for group in &mut layer.alpha_groups {
                if let Some((_, path)) = layer.clip_sources.iter().find(|(id, _)| *id == group.id) {
                    let (bounds, radius) =
                        path.evaluate(|source, _| live_source(program, state, source))?;
                    group.clip = Some((bounds, radius.unwrap_or_default()));
                }
            }
            if layer.is_empty() {
                continue;
            }
            let path = layer.clip_path.as_ref().ok_or(Error::OpaquePaint)?;
            let (bounds, radius) =
                path.evaluate(|source, _| live_source(program, state, source))?;
            layer.owned_layout_clip(bounds, radius);
        }
        segment.clip = rect(program.clip());
    }
    Ok(())
}
