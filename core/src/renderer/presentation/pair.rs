// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::Size;
use crate::layout::{Limits, pair as native};

pub(super) fn validate(value: &Pair, children: usize) -> Result<(), ()> {
    if children != 2 || !value.policy.breakpoint.is_finite() || value.policy.breakpoint < 0.0 {
        return Err(());
    }
    for arm in [value.policy.row, value.policy.stacked] {
        if !arm.spacing.is_finite() || arm.spacing < 0.0 {
            return Err(());
        }
        let mut lengths = value.children.to_vec();
        lengths.push(Size::new(arm.leading_width, crate::Length::Shrink));
        flex::validate(
            &Flex {
                bookends: None,
                sequence: None,
                collapse_spacing: false,
                axis: Axis::Horizontal,
                width: value.width,
                limits: value.limits,
                sizing: Size::new(value.policy.width, arm.height),
                padding: arm.padding,
                gap: 0,
                align: arm.align,
                children: lengths,
            },
            3,
        )?;
    }
    Ok(())
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Pair,
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
    let min = Size::new(base.min().width.min(width), base.min().height);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let children = &program.nodes[index].children;
    let result = native::try_resolve(
        &limits,
        declaration.policy,
        declaration.children,
        Some(Error::ReflowRequired(program.nodes[index].id)),
        |slot, limits| {
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
        },
    )?;
    let size = result.size();
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > available
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    for (child, geometry) in children.iter().zip(result.children()) {
        boxes[*child].x = geometry.bounds().x;
        boxes[*child].y = geometry.bounds().y;
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
