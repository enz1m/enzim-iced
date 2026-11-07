// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::Size;
use crate::layout::{Limits, container};

pub(super) fn validate(value: &Container, children: usize) -> Result<(), ()> {
    if value
        .reflow_min_width
        .is_some_and(|width| !width.is_finite() || width < 0.0)
    {
        return Err(());
    }
    if let container::Mode::WidthCap {
        reserve,
        minimum,
        ceiling,
    } = value.mode
        && (![reserve, minimum, ceiling]
            .into_iter()
            .all(|v| v.is_finite() && v >= 0.0)
            || value.padding != crate::Padding::ZERO
            || value.alignment != [crate::Alignment::Start; 2])
    {
        return Err(());
    }
    if let container::Mode::CachedFactory { available } = value.mode
        && ([available.width, available.height]
            .into_iter()
            .any(|v| v.is_nan() || v < 0.0)
            || value.padding != crate::Padding::ZERO
            || value.alignment != [crate::Alignment::Start; 2])
    {
        return Err(());
    }
    if value.mode == container::Mode::Frame
        && (value.padding != crate::Padding::ZERO
            || value.alignment != [crate::Alignment::Start; 2])
    {
        return Err(());
    }
    if value.mode == container::Mode::Void
        && value.sizing != Size::new(crate::Length::Fixed(0.0), crate::Length::Fixed(0.0))
    {
        return Err(());
    }
    if value.mode == container::Mode::Material
        && (value.padding != crate::Padding::ZERO
            || value.alignment != [crate::Alignment::Start; 2]
            || value.width.is_some())
    {
        return Err(());
    }
    if let container::Mode::HeightFloor { minimum, .. } = value.mode
        && (!minimum.is_finite()
            || minimum < 0.0
            || value.padding != crate::Padding::ZERO
            || value.alignment != [crate::Alignment::Start; 2]
            || value.sizing.height != crate::Length::Shrink)
    {
        return Err(());
    }
    let atomic = matches!(
        value.mode,
        container::Mode::Atomic | container::Mode::Unrealized | container::Mode::Void
    );
    if let Some(inset) = value.inset {
        let alignment = [
            if inset.trailing {
                crate::Alignment::End
            } else {
                crate::Alignment::Start
            },
            crate::Alignment::Start,
        ];
        if value.mode != container::Mode::Aligned
            || !inset.policy.is_valid()
            || inset.driver == 0
            || inset.driver >= (1 << 63)
            || !inset.captured.is_finite()
            || !(0.0..=1.0).contains(&inset.captured)
            || value.alignment != alignment
            || value.padding != inset.policy.padding(inset.captured, inset.trailing)
        {
            return Err(());
        }
    }
    if children != usize::from(!atomic)
        || (atomic
            && (value.padding != crate::Padding::ZERO
                || value.alignment != [crate::Alignment::Start; 2]))
        || (value.mode == container::Mode::Field
            && (value.sizing.width != crate::Length::Fill
                || value.alignment != [crate::Alignment::Start; 2]))
        || (value.mode == container::Mode::Passthrough
            && (value.padding != crate::Padding::ZERO
                || value.alignment != [crate::Alignment::Start; 2]
                || value.sizing != value.child))
    {
        return Err(());
    }
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
            align: value.alignment[0],
            children: if atomic {
                Vec::new()
            } else {
                vec![value.child]
            },
        },
        children,
    )
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Container,
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
    if declaration
        .reflow_min_width
        .is_some_and(|minimum| width < minimum)
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
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
    if let container::Mode::CachedFactory { available } = declaration.mode
        && limits
            .width(declaration.sizing.width)
            .height(declaration.sizing.height)
            .max()
            != available
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let child = program.nodes[index].children.first().copied();
    let padding = if let Some(inset) = declaration.inset {
        let progress = *values
            .get(inset.progress.ok_or(Error::Node(index))?)
            .ok_or(Error::Node(index))?;
        if !progress.is_finite() {
            return Err(Error::NonFinite);
        }
        inset.policy.padding(progress, inset.trailing)
    } else {
        declaration.padding
    };
    let result = container::try_resolve(
        &limits,
        declaration.sizing,
        padding,
        declaration.alignment,
        declaration.mode,
        |limits| {
            flex::measure_child(
                program,
                values,
                boxes,
                child.ok_or(Error::Node(index))?,
                declaration.child,
                limits,
                work,
                text,
            )
        },
    )?;
    let size = result.size();
    let allowed_width = match declaration.mode {
        container::Mode::WidthCap {
            minimum, ceiling, ..
        } => available.max(minimum.min(ceiling)),
        _ => available,
    };
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > allowed_width
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    if let Some(child) = child {
        let bounds = result.children()[0].bounds();
        boxes[child].x = bounds.x;
        boxes[child].y = bounds.y;
    }
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
