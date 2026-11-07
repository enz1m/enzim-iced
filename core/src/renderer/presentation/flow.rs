// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::Size;
use crate::layout::{Limits, flow as native};

pub(super) fn validate(value: &Wrapping, children: usize) -> Result<(), ()> {
    if !value
        .rule
        .gaps
        .into_iter()
        .all(|gap| gap.is_finite() && gap >= 0.0)
    {
        return Err(());
    }
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: value.rule.axis,
            width: value.width,
            limits: value.limits,
            sizing: value.rule.sizing,
            padding: value.rule.padding,
            gap: 0,
            align: value.rule.cross_alignment,
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
    declaration: &Wrapping,
    available: f32,
    inherited: Option<Limits>,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    let base = inherited.unwrap_or(declaration.limits);
    let width = constraint(
        declaration
            .width
            .map_or(base.max().width.min(available), |v| values[v]),
        index,
    )?;
    if width > available {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let limits = Limits::with_compression(
        Size::new(base.min().width.min(width), base.min().height),
        Size::new(width, base.max().height),
        base.compression(),
    );
    let children = &program.nodes[index].children;
    let result = native::try_resolve(&limits, declaration.rule, children.len(), |slot, limits| {
        flex::measure_child(
            program,
            values,
            boxes,
            children[slot],
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
        || size.width > available.max(declaration.rule.padding.x())
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    for (child, node) in children.iter().zip(result.children()) {
        boxes[*child].x = node.bounds().x;
        boxes[*child].y = node.bounds().y;
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
