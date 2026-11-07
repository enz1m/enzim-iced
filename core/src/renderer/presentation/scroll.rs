// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::{Size, layout::Limits};

pub(super) fn validate(value: &Scroll) -> Result<(), ()> {
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: Axis::Vertical,
            width: value.width,
            limits: value.limits,
            sizing: value.sizing,
            padding: crate::Padding::ZERO,
            gap: 0,
            align: crate::Alignment::Start,
            children: vec![value.child],
        },
        1,
    )
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Scroll,
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
    min.width = min.width.min(width);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let input = text.scroll_input(program.nodes[index].id)?;
    if !input.valid() {
        return Err(Error::NonFinite);
    }
    let child = program.nodes[index].children[0];
    let (result, visible) =
        declaration
            .config
            .resolve(&limits, declaration.sizing, input.visible, |limits| {
                flex::measure_child(
                    program,
                    values,
                    boxes,
                    child,
                    declaration.child,
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
        || size.width > available
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    text.scroll_visible(program.nodes[index].id, visible);
    let bounds = result.children()[0].bounds();
    boxes[child].x = bounds.x;
    boxes[child].y = bounds.y;
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
