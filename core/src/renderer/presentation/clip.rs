// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::Limits;
use crate::{Alignment, Padding, Size};

pub(super) fn validate(value: &Clip, children: usize) -> Result<(), ()> {
    if children != 1 || !value.recorded.valid() {
        return Err(());
    }
    if let Some(reveal) = value.reveal
        && (!reveal.rule.is_valid()
            || !reveal.captured.is_finite()
            || !(0.0..=1.0).contains(&reveal.captured)
            || !reveal.source.valid()
            || !reveal.source.contains(value.recorded)
            || value.outset != Padding::ZERO
            || reveal
                .driver
                .is_some_and(|driver| driver == 0 || driver >= (1 << 63))
            || reveal.driver.is_some() != reveal.progress.is_some())
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
            sizing: value.reveal.map_or(value.child, |reveal| reveal.sizing),
            padding: value.outset,
            gap: 0,
            align: Alignment::Start,
            children: vec![value.child],
        },
        1,
    )
}

pub(super) fn expanded(bounds: Bounds, outset: Padding) -> Result<Bounds, Error> {
    let result = crate::layout::clip::bounds(
        crate::Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        },
        outset,
    );
    let result = Bounds {
        x: result.x,
        y: result.y,
        width: result.width,
        height: result.height,
    };
    if result.valid() {
        Ok(result)
    } else {
        Err(Error::NonFinite)
    }
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Clip,
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
    let min_width = if declaration.width.is_some() && base.min().width == base.max().width {
        width
    } else {
        base.min().width.min(width)
    };
    let min = Size::new(min_width, base.min().height);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let limits = declaration
        .reveal
        .map_or(limits, |reveal| reveal.rule.child_limits(&limits));
    let child = program.nodes[index].children[0];
    let content = flex::measure_child(
        program,
        values,
        boxes,
        child,
        declaration.child,
        &limits,
        work,
        text,
    )?;
    boxes[child].x = content.bounds().x;
    boxes[child].y = content.bounds().y;
    let size = if let Some(reveal) = declaration.reveal {
        let progress = progress(reveal, values)?;
        reveal.rule.finish(content, progress).size()
    } else {
        content.size()
    };
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}

pub(super) fn progress(reveal: Reveal, values: &[f32]) -> Result<f32, Error> {
    let progress = reveal
        .progress
        .map_or(reveal.captured, |index| values[index]);
    if progress.is_finite() && (0.0..=1.0).contains(&progress) {
        Ok(progress)
    } else {
        Err(Error::NonFinite)
    }
}

pub(super) fn visible(bounds: Bounds, declaration: &Clip, values: &[f32]) -> Result<Bounds, Error> {
    let Some(reveal) = declaration.reveal else {
        return expanded(bounds, declaration.outset);
    };
    let rect = reveal.rule.visible(
        crate::Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        },
        progress(reveal, values)?,
    );
    Ok(Bounds {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    })
}
