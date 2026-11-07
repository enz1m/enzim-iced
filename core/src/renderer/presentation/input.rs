// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::Limits;
use crate::{Length, Size};

pub(super) fn parts(program: &Program, index: usize) -> Result<[usize; 5], Error> {
    parts_of(&program.nodes, index)
}

pub(super) fn parts_of(nodes: &[Node], index: usize) -> Result<[usize; 5], Error> {
    let node = &nodes[index];
    let Layout::Input(input) = &node.layout else {
        return Err(Error::Node(index));
    };
    let [clip, icon] = node.children.as_slice() else {
        return Err(Error::Node(index));
    };
    let [body] = nodes[*clip].children.as_slice() else {
        return Err(Error::Node(index));
    };
    let [text, caret] = nodes[*body].children.as_slice() else {
        return Err(Error::Node(index));
    };
    if !matches!(nodes[*clip].layout, Layout::Clip(_))
        || !matches!(nodes[*body].layout, Layout::Overlay(_))
        || !text_slot(&nodes[*text].layout)
        || !matches!(&nodes[*caret].layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Atomic)
    {
        return Err(Error::Node(index));
    }
    let valid_icon = if input.shape.icon.is_some() {
        text_slot(&nodes[*icon].layout)
    } else {
        matches!(&nodes[*icon].layout, Layout::Container(value) if value.mode == crate::layout::container::Mode::Void)
    };
    if !valid_icon {
        return Err(Error::Node(index));
    }
    Ok([*clip, *body, *text, *caret, *icon])
}

pub(super) fn measure(
    program: &Program,
    values: &[f32],
    boxes: &mut [Bounds],
    index: usize,
    declaration: &InputGeometry,
    available: f32,
    inherited: Option<Limits>,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<[f32; 2], Error> {
    let [clip, body, paragraph, caret, icon] = parts(program, index)?;
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
    let g = declaration.shape.resolve(&limits);
    measure_text_slot(program, paragraph, g.text.size(), work, text)?;
    for _ in 0..5 {
        work.visit_layout()?;
    }
    boxes[index] = Bounds {
        width: g.size.width,
        height: g.size.height,
        ..Bounds::default()
    };
    boxes[clip] = Bounds {
        x: g.text.x,
        y: g.text.y,
        width: g.text.width,
        height: g.text.height,
    };
    boxes[body] = Bounds {
        width: g.text.width,
        height: g.text.height,
        ..Bounds::default()
    };
    boxes[paragraph] = boxes[body];
    boxes[caret] = if let Some(c) = declaration.paint.caret {
        Bounds {
            x: c.x + declaration.paint.offset(g.text.width),
            y: 0.0,
            width: c.width,
            height: g.text.height,
        }
    } else {
        Bounds::default()
    };
    boxes[icon] = if declaration.shape.icon.is_some() {
        measure_text_slot(program, icon, g.icon.size(), work, text)?;
        Bounds {
            x: g.icon.x,
            y: g.icon.y,
            width: g.icon.width,
            height: g.icon.height,
        }
    } else {
        Bounds::default()
    };
    Ok([g.size.width, g.size.height])
}

pub(super) fn validate(value: &InputGeometry, children: usize) -> Result<(), ()> {
    if children != 2 || !value.recorded.valid() || !value.shape.valid() || !value.paint.valid() {
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
            sizing: Size::new(value.shape.width, Length::Shrink),
            padding: value.shape.padding,
            gap: 0,
            align: crate::Alignment::Start,
            children: vec![Size::new(Length::Fill, Length::Shrink); 2],
        },
        2,
    )
}

pub(super) fn paint_offset(program: &Program, leaf: usize, current: Bounds) -> f32 {
    let Some(body) = program.ancestors[leaf] else {
        return 0.0;
    };
    let Some(clip) = program.ancestors[body] else {
        return 0.0;
    };
    let Some(owner) = program.ancestors[clip] else {
        return 0.0;
    };
    let Layout::Input(value) = &program.nodes[owner].layout else {
        return 0.0;
    };
    if parts(program, owner).is_ok_and(|parts| parts[2] == leaf) {
        let Layout::Text { recorded, .. } = &program.nodes[leaf].layout else {
            return 0.0;
        };
        value.paint.offset(current.width) - value.paint.offset(recorded.width)
    } else {
        0.0
    }
}

fn text_slot(layout: &Layout) -> bool {
    matches!(layout, Layout::Text { .. })
        || matches!(layout, Layout::UnpaintedLeaf { maximum, .. } if maximum.height == 0.0)
}

fn measure_text_slot(
    program: &Program,
    index: usize,
    size: Size,
    work: &mut Work,
    text: &mut dyn TextMeasurer,
) -> Result<(), Error> {
    if let Layout::UnpaintedLeaf { maximum, .. } = program.nodes[index].layout {
        if maximum.height == 0.0 && size.height == 0.0 {
            return Ok(());
        }
        return Err(Error::ReflowRequired(program.nodes[index].id));
    }
    work.measure_text()?;
    let metrics = text.measure(program.nodes[index].id, size.width, Some(size.height))?;
    if !metrics.into_iter().all(|v| v.is_finite() && v >= 0.0) {
        return Err(Error::NonFinite);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_height_input_skips_absent_text_material_but_refuses_positive_height() {
        struct MissingText;
        impl TextMeasurer for MissingText {
            fn measure(&mut self, node: u64, _: f32, _: Option<f32>) -> Result<[f32; 2], Error> {
                Err(Error::TextIdentity(node))
            }
        }
        let node = crate::layout::input::node(
            &Limits::new(Size::ZERO, Size::new(100.0, 0.0)),
            crate::layout::input::Shape {
                width: Length::Fill,
                padding: crate::Padding::ZERO,
                line_height: 18.0,
                icon: None,
            },
            std::sync::Arc::default(),
            crate::renderer::NodeId(9001),
            crate::renderer::NodeId(9002),
        );
        let program = crate::layout::Placed::of(&node)
            .presentation_program(
                crate::renderer::NodeId(900),
                0,
                crate::Rectangle::with_size(Size::new(480.0, 260.0)),
            )
            .unwrap();
        let leaf = program.index(9001).unwrap();
        let mut work = Work::default();
        assert_eq!(
            measure_text_slot(
                &program,
                leaf,
                Size::new(50.0, 0.0),
                &mut work,
                &mut MissingText
            ),
            Ok(())
        );
        assert_eq!(
            measure_text_slot(
                &program,
                leaf,
                Size::new(50.0, 18.0),
                &mut work,
                &mut MissingText
            ),
            Err(Error::ReflowRequired(9001))
        );
    }
}
