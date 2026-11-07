// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::{Size, layout::Limits};

pub(super) fn validate(value: &Media) -> Result<(), ()> {
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: Axis::Vertical,
            width: value.width,
            limits: value.limits,
            sizing: value.geometry.sizing,
            padding: crate::Padding::ZERO,
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
    declaration: &Media,
    available: f32,
    inherited: Option<Limits>,
) -> Result<[f32; 2], Error> {
    let base = inherited.unwrap_or(declaration.limits);
    let width = declaration
        .width
        .map_or(base.max().width.min(available), |slot| values[slot]);
    if width.is_nan() || width < 0.0 || width > available {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let mut min = base.min();
    min.width = min.width.min(width);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let size = declaration.geometry.allocation(&limits);
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > available
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
