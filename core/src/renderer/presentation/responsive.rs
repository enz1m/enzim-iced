// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::{Limits, responsive::Rule};
use crate::{Alignment, Length, Padding, Size};

pub(super) fn validate(value: &Responsive, children: usize) -> Result<(), ()> {
    if children != 1 {
        return Err(());
    }
    match value.rule {
        Rule::Resize { from, to, progress }
            if [from.width, from.height, to.width, to.height]
                .into_iter()
                .any(|v| !v.is_finite() || v < 0.0)
                || !progress.is_finite()
                || !(0.0..=1.0).contains(&progress) =>
        {
            return Err(());
        }
        Rule::Reserve {
            resting, progress, ..
        } if !resting.is_finite()
            || resting < 0.0
            || !progress.is_finite()
            || !(0.0..=1.0).contains(&progress) =>
        {
            return Err(());
        }
        Rule::Content { .. } if value.driver.is_some() || value.progress.is_some() => {
            return Err(());
        }
        _ => {}
    }
    if value.driver.is_some() != value.progress.is_some() {
        return Err(());
    }
    flex::validate(
        &Flex {
            bookends: None,
            sequence: None,
            collapse_spacing: false,
            axis: Axis::Vertical,
            width: None,
            limits: value.limits,
            sizing: match value.rule {
                Rule::Content { sizing } => sizing,
                _ => Size::new(Length::Shrink, value.child.height),
            },
            padding: Padding::ZERO,
            gap: 0,
            align: Alignment::Start,
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
    declaration: &Responsive,
    available: f32,
    inherited: Option<Limits>,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    let mut rule = declaration.rule;
    if let Rule::Reserve { progress, .. } | Rule::Resize { progress, .. } = &mut rule {
        if let Some(expression) = declaration.progress {
            *progress = values[expression];
        }
        if !(0.0..=1.0).contains(progress) {
            return Err(Error::NonFinite);
        }
    }
    let base = inherited.unwrap_or(declaration.limits);
    let width = base.max().width.min(available);
    let min = Size::new(base.min().width.min(width), base.min().height);
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let child = program.nodes[index].children[0];
    let child_limits = rule.child_limits(&limits);
    let content = flex::measure_child(
        program,
        values,
        boxes,
        child,
        declaration.child,
        &child_limits,
        work,
        text,
    )?;
    let result = rule.finish(&limits, content);
    let size = result.size();
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > available
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    let position = result.children()[0].bounds();
    boxes[child].x = position.x;
    boxes[child].y = position.y;
    boxes[index] = Bounds {
        width: size.width,
        height: size.height,
        ..Bounds::default()
    };
    Ok([size.width, size.height])
}
