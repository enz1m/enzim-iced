// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::{Size, layout::Limits};

pub(super) fn validate(value: &Editor) -> Result<(), ()> {
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: Axis::Vertical,
            width: value.width,
            limits: value.limits,
            sizing: value.sizing,
            padding: value.padding,
            gap: 0,
            align: crate::Alignment::Start,
            children: vec![],
        },
        0,
    )
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Editor,
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
    let mut min = base.min();
    min.width = min.width.min(width);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression())
            .width(declaration.sizing.width)
            .height(declaration.sizing.height);
    let viewport = limits.shrink(declaration.padding).max();
    if !viewport.width.is_finite()
        || !viewport.height.is_finite()
        || viewport.width < 0.0
        || viewport.height < 0.0
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    work.measure_text()?;
    let intrinsic =
        text.measure_editor(program.nodes[index].id, viewport.width, viewport.height)?;
    if intrinsic
        .into_iter()
        .any(|value| !value.is_finite() || value < 0.0)
    {
        return Err(Error::NonFinite);
    }
    let size = crate::layout::editor_size(
        &limits,
        declaration.sizing.height,
        declaration.padding,
        Size::new(intrinsic[0], intrinsic[1]),
    );
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width > available
        || size.width < 0.0
        || size.height < 0.0
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
