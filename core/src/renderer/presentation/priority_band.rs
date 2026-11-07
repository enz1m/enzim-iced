// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::{Limits, Node as Geometry, priority_band as native};
use crate::{Alignment, Length, Padding, Point, Rectangle, Size};

pub(super) fn validate(value: &Band, children: usize) -> Result<(), ()> {
    let policy = value.policy;
    let finite = |value: f32| value.is_finite() && value >= 0.0;
    if children != 4
        || value.probes.len() > 3
        || value.probes.iter().any(|probe| !probe.valid())
        || value.probes.iter().enumerate().any(|(i, probe)| {
            value.probes[..i]
                .iter()
                .any(|previous| previous.slot == probe.slot && previous.limits == probe.limits)
        })
        || !value.recorded.valid()
        || !value.recorded_content_clip.valid()
        || ![
            policy.action_min_width,
            policy.action_reserve_width,
            policy.spacing,
            policy.padding_x,
            policy.centring_height,
            policy.action_probe_width,
            policy.leading_share,
            policy.spill_epsilon,
        ]
        .into_iter()
        .all(finite)
        || policy.leading_share > 1.0
        || policy
            .fluid_content_min_width
            .is_some_and(|value| !finite(value))
    {
        return Err(());
    }
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: Axis::Horizontal,
            width: value.width,
            limits: value.limits,
            sizing: Size::new(Length::Fill, policy.height),
            padding: Padding::ZERO,
            gap: 0,
            align: Alignment::Start,
            children: value.children.clone(),
        },
        children,
    )
}

fn clips(layout: &Layout) -> bool {
    matches!(layout, Layout::Scroll(_))
        || matches!(layout, Layout::TabStrip(strip) if strip.policy.clips_to_bounds())
}

fn geometry(program: &Program, boxes: &[Bounds], index: usize) -> Geometry {
    let bounds = boxes[index];
    Geometry::with_children(
        Size::new(bounds.width, bounds.height),
        if clips(&program.nodes[index].layout) {
            Vec::new()
        } else {
            program.nodes[index]
                .children
                .iter()
                .filter(|&&child| !hidden_material(program, boxes, child))
                .map(|&child| geometry(program, boxes, child))
                .collect()
        },
    )
    .move_to(Point::new(bounds.x, bounds.y))
}

pub(super) fn content_clip(program: &Program, boxes: &[Bounds], index: usize) -> Bounds {
    let Layout::Band(band) = &program.nodes[index].layout else {
        unreachable!()
    };
    let child = program.nodes[index].children[native::CONTENT];
    let slot = boxes[child];
    if clips(&program.nodes[child].layout) {
        return slot;
    }
    let mut pending = program.nodes[child].children.clone();
    let descendants = std::iter::from_fn(|| {
        while let Some(node) = pending.pop() {
            if hidden_material(program, boxes, node) {
                continue;
            }
            if !clips(&program.nodes[node].layout) {
                pending.extend_from_slice(&program.nodes[node].children);
            }
            let bounds = boxes[node];
            return Some(Rectangle {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
            });
        }
        None
    });
    let bounds = native::clip_content_boxes(
        Rectangle {
            x: slot.x,
            y: slot.y,
            width: slot.width,
            height: slot.height,
        },
        descendants,
        band.policy.spill_epsilon,
    );
    Bounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Band,
    available: f32,
    inherited: Option<Limits>,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    let base = inherited.unwrap_or(declaration.limits);
    let width = nonnegative(
        declaration
            .width
            .map_or(base.max().width.min(available), |value| values[value]),
        index,
    )?;
    if width > available {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let min_width = if declaration.width.is_some() && base.min().width == base.max().width {
        width
    } else {
        base.min().width.min(width)
    };
    let limits = Limits::with_compression(
        Size::new(min_width, base.min().height),
        Size::new(width, base.max().height),
        base.compression(),
    );
    let children = &program.nodes[index].children;
    let result = native::try_resolve(&limits, declaration.policy, |slot, limits, pass| {
        let child = children[slot];
        match pass {
            native::Pass::Probe => {
                if !declaration.probes.is_empty() {
                    return declaration
                        .probes
                        .iter()
                        .find(|probe| probe.slot == slot && probe.limits == *limits)
                        .map(native::Probe::geometry)
                        .ok_or(Error::ReflowRequired(program.nodes[index].id));
                }
                let mut probe = Probe(text);
                let _ = flex::measure_child(
                    program,
                    values,
                    boxes,
                    child,
                    declaration.children[slot],
                    limits,
                    work,
                    &mut probe,
                )?;
            }
            native::Pass::Renegotiate(_) => {
                return Err(Error::ReflowRequired(program.nodes[index].id));
            }
            native::Pass::Layout => {
                let _ = flex::measure_child(
                    program,
                    values,
                    boxes,
                    child,
                    declaration.children[slot],
                    limits,
                    work,
                    text,
                )?;
            }
        }
        Ok(geometry(program, boxes, child).move_to(Point::ORIGIN))
    })?;
    let size = result.size();
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > available
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    for (&child, node) in children.iter().zip(result.children()) {
        boxes[child].x = node.bounds().x;
        boxes[child].y = node.bounds().y;
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}

struct Probe<'a>(&'a mut dyn TextMeasurer);
impl TextMeasurer for Probe<'_> {
    fn measure(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
        self.0.probe(node, width, height)
    }
    fn probe(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
        self.0.probe(node, width, height)
    }
}

fn hidden_material(program: &Program, boxes: &[Bounds], index: usize) -> bool {
    matches!(&program.nodes[index].layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Material)
        && boxes[index].width == 0.0
        && boxes[index].height == 0.0
}
