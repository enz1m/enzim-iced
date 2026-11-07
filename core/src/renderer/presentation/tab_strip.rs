// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::Size;
use crate::layout::{Limits, item_strip as native};

pub(super) fn validate(value: &TabStrip, children: usize) -> Result<(), ()> {
    if !value.policy.valid()
        || value.policy.material_count() != Some(children)
        || value.children.len() != children
        || !value.recorded.valid()
        || !value.source.valid()
        || !value.source.contains(value.recorded)
    {
        return Err(());
    }
    if matches!(&value.policy, native::Policy::Actions(policy) if policy.sizing != value.children) {
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
            sizing: value.policy.sizing(),
            padding: crate::Padding::ZERO,
            gap: 0,
            align: crate::Alignment::Center,
            children: value.children.clone(),
        },
        children,
    )
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &TabStrip,
    available: f32,
    inherited: Option<Limits>,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    let base = inherited.unwrap_or(declaration.limits);
    let width = nonnegative(
        declaration
            .width
            .map_or(base.max().width.min(available), |v| values[v]),
        index,
    )?;
    if width > available {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let mut min = base.min();
    min.width = if declaration.width.is_some() && min.width == base.max().width {
        width
    } else {
        min.width.min(width)
    };
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let children = &program.nodes[index].children;
    let result = native::try_resolve(&limits, &declaration.policy, |slot, limits| {
        let child = if declaration.policy.wrapped() {
            program.nodes[children[slot]].children[0]
        } else {
            children[slot]
        };
        flex::measure_child(
            program,
            values,
            boxes,
            child,
            declaration.children[slot],
            limits,
            work,
            text,
        )
    })?;
    let size = result.size();
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || (size.width > available && !declaration.policy.allows_spill())
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    for (&child, node) in children.iter().zip(result.children()) {
        boxes[child] = Bounds {
            x: node.bounds().x,
            y: node.bounds().y,
            width: node.size().width,
            height: node.size().height,
        };
        if declaration.policy.wrapped() {
            let material = program.nodes[child].children[0];
            boxes[material].x = 0.0;
            boxes[material].y = 0.0;
        }
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
