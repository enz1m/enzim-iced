// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::{Limits, Node as Geometry, flex as native};
use crate::{Length, Size, length};

pub(super) fn validate(value: &Flex, children: usize) -> Result<(), ()> {
    let finite = |v: f32| v.is_finite() && v >= 0.0;
    let valid_length = |value: Length| match value {
        Length::Fixed(v) | Length::Fluid(length::Constraint::Min(v)) => finite(v),
        Length::Bounded { bounds, .. } => match bounds {
            length::Bounds::Min(v) | length::Bounds::Max(v) => finite(v),
            length::Bounds::Both { min, max } => finite(min) && finite(max) && min <= max,
        },
        _ => true,
    };
    let min = value.limits.min();
    let max = value.limits.max();
    if value.bookends.is_some()
        && (children != 3
            || value.sequence.is_some()
            || value.collapse_spacing
            || !matches!(value.axis, Axis::Horizontal)
            || value.padding != crate::Padding::ZERO
            || value.align != crate::Alignment::Start
            || value.sizing.width != Length::Fill)
    {
        return Err(());
    }
    if value.sequence.is_some()
        && (value.collapse_spacing
            || value.padding != crate::Padding::ZERO
            || value.align != crate::Alignment::Start)
    {
        return Err(());
    }
    if value.children.len() != children
        || ![
            min.width,
            min.height,
            value.padding.left,
            value.padding.right,
            value.padding.top,
            value.padding.bottom,
        ]
        .into_iter()
        .all(finite)
        || max.width.is_nan()
        || max.height.is_nan()
        || max.width < min.width
        || max.height < min.height
        || !std::iter::once(value.sizing)
            .chain(value.children.iter().copied())
            .all(|size| valid_length(size.width) && valid_length(size.height))
    {
        return Err(());
    }
    Ok(())
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &Flex,
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
    let mut min = base.min();
    min.width = if declaration.width.is_some() && min.width == base.max().width {
        width
    } else {
        min.width.min(width)
    };
    let limits =
        Limits::with_compression(min, Size::new(width, base.max().height), base.compression());
    let gap = nonnegative(values[declaration.gap], index)?;
    let children = &program.nodes[index].children;
    let axis = match declaration.axis {
        Axis::Horizontal => native::Axis::Horizontal,
        Axis::Vertical => native::Axis::Vertical,
    };
    let mut content = Content {
        program,
        values,
        boxes,
        children,
        sizing: &declaration.children,
        work,
        text,
    };
    let result = if let Some(policy) = declaration.bookends {
        native::try_resolve_bookends_content(
            &limits,
            declaration.sizing.height,
            gap,
            policy,
            &mut content,
        )
    } else if let Some(flow) = declaration.sequence {
        native::try_resolve_sequence_content(
            axis,
            &limits,
            declaration.sizing.width,
            declaration.sizing.height,
            gap,
            flow,
            &mut content,
        )
    } else {
        let resolver = if declaration.collapse_spacing {
            native::try_resolve_collapsing_content
        } else {
            native::try_resolve_content
        };
        resolver(
            axis,
            &limits,
            declaration.sizing.width,
            declaration.sizing.height,
            declaration.padding,
            gap,
            declaration.align,
            &mut content,
        )
    }?;
    let size = result.size();
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
        || size.width > available.max(declaration.padding.x())
    {
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    for (&child, node) in children.iter().zip(result.children()) {
        let bounds = node.bounds();
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

struct Content<'a> {
    program: &'a Program,
    values: &'a [f32],
    boxes: &'a mut [Bounds],
    children: &'a [usize],
    sizing: &'a [Size<Length>],
    work: &'a mut Work,
    text: &'a mut dyn TextMeasurer,
}

impl native::TryContent for Content<'_> {
    type Error = Error;

    fn count(&self) -> usize {
        self.children.len()
    }

    fn sizing(&mut self, index: usize) -> Result<Size<Length>, Error> {
        Ok(self.sizing[index])
    }

    fn layout(&mut self, offset: usize, limits: &Limits) -> Result<Geometry, Error> {
        let index = self.children[offset];
        measure_child(
            self.program,
            self.values,
            self.boxes,
            index,
            self.sizing[offset],
            limits,
            self.work,
            self.text,
        )
    }
}

pub(super) fn measure_child(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    sizing: Size<Length>,
    limits: &Limits,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<Geometry, Error> {
    let node = &program.nodes[index];
    let limits = if matches!(node.layout, Layout::Input(_)) {
        *limits
    } else {
        limits.width(sizing.width).height(sizing.height)
    };
    work.visit_layout()?;
    let size = match &node.layout {
        Layout::Wrapping(declaration) => {
            let size = super::flow::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Input(declaration) => {
            let size = super::input::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Responsive(declaration) => {
            let size = super::responsive::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Media(declaration) => {
            let size = super::media::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Scroll(declaration) => {
            let size = super::scroll::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Editor(declaration) => {
            let size = super::editor::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Container(declaration) => {
            let size = super::container::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Clip(declaration) => {
            let size = super::clip::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::TabStrip(declaration) => {
            let size = super::tab_strip::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Band(declaration) => {
            let size = super::priority_band::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Pair(declaration) => {
            let size = super::pair::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Overlay(declaration) => {
            let size = super::overlay::measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::Flex(declaration) => {
            let size = measure(
                program,
                values,
                boxes,
                index,
                declaration,
                limits.max().width,
                Some(limits),
                work,
                text,
            )?;
            Size::new(size[0], size[1])
        }
        Layout::UnpaintedLeaf { recorded, maximum } => {
            let size = Size::new(recorded.width, recorded.height);
            if limits.max() != *maximum || limits.resolve(sizing.width, sizing.height, size) != size
            {
                return Err(Error::ReflowRequired(node.id));
            }
            size
        }
        Layout::Fixed { recorded } => {
            let size = Size::new(recorded.width, recorded.height);
            if limits.resolve(sizing.width, sizing.height, size) != size
                || size.width > limits.max().width
                || size.height > limits.max().height
            {
                return Err(Error::ReflowRequired(node.id));
            }
            size
        }
        Layout::Text {
            width: None,
            height: None,
            max_height: None,
            overflow,
            ..
        } => {
            let width = constraint(limits.max().width, index)?;
            let height = limits
                .max()
                .height
                .is_finite()
                .then_some(limits.max().height);
            work.measure_text()?;
            let intrinsic = text.measure(node.id, width, height)?;
            if intrinsic.into_iter().any(|v| !v.is_finite() || v < 0.0)
                || (*overflow == TextOverflow::Refuse
                    && (intrinsic[0] > width || height.is_some_and(|v| intrinsic[1] > v)))
            {
                return Err(Error::ReflowRequired(node.id));
            }
            limits.resolve(
                sizing.width,
                sizing.height,
                Size::new(intrinsic[0], intrinsic[1]),
            )
        }
        _ => return Err(Error::ReflowRequired(node.id)),
    };
    if !size.width.is_finite() || !size.height.is_finite() || size.width < 0.0 || size.height < 0.0
    {
        return Err(Error::ReflowRequired(node.id));
    }
    boxes[index].width = size.width;
    boxes[index].height = size.height;
    Ok(Geometry::new(size))
}
