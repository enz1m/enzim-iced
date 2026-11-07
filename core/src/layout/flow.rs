// Modified by enzUI contributors. See FORK.md.
//! Native wrapping geometry shared with responsive presentation.

use super::{Limits, Measure, Node};
pub use crate::renderer::presentation::Axis;
use crate::{Alignment, Element, Length, Padding, Point, Size, Vector, widget};

/// Intrinsic line packing; children keep source order and are never scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// Main axis along which items advance before wrapping.
    pub axis: Axis,
    /// Native outer width and height policies.
    pub sizing: Size<Length>,
    /// Insets in logical pixels, applied after line packing.
    pub padding: Padding,
    /// Main-axis and between-line spacing in logical pixels, including empty items.
    pub gaps: [f32; 2],
    /// Cross-axis alignment within each line.
    pub cross_alignment: Alignment,
    /// Main-axis line alignment relative to the widest intrinsic line, not unused fill space.
    pub main_alignment: Alignment,
}

impl Rule {
    fn main(self, size: Size) -> f32 {
        match self.axis {
            Axis::Horizontal => size.width,
            Axis::Vertical => size.height,
        }
    }
    fn cross(self, size: Size) -> f32 {
        match self.axis {
            Axis::Horizontal => size.height,
            Axis::Vertical => size.width,
        }
    }
    fn pack(self, main: f32, cross: f32) -> Size {
        match self.axis {
            Axis::Horizontal => Size::new(main, cross),
            Axis::Vertical => Size::new(cross, main),
        }
    }
    fn point(self, main: f32, cross: f32) -> Point {
        let size = self.pack(main, cross);
        Point::new(size.width, size.height)
    }
}

fn factor(alignment: Alignment) -> f32 {
    match alignment {
        Alignment::Start => 0.0,
        Alignment::Center => 2.0,
        Alignment::End => 1.0,
    }
}

/// Measures native widgets and declares their complete wrapping allocation.
/// Child tree order is unchanged, including items that move to another line.
pub fn resolve<Message, Theme, Renderer: crate::Renderer>(
    renderer: &Renderer,
    limits: &Limits,
    rule: Rule,
    items: &mut [Element<'_, Message, Theme, Renderer>],
    trees: &mut [widget::Tree],
) -> Node {
    let children = items
        .iter()
        .map(|item| item.as_widget().size())
        .collect::<Vec<_>>();
    let node = match try_resolve(limits, rule, children.len(), |index, limits| {
        Ok::<_, std::convert::Infallible>(items[index].as_widget_mut().measure(
            &mut trees[index],
            renderer,
            limits,
        ))
    }) {
        Ok(node) => node,
        Err(error) => match error {},
    };
    node.with_flow_presentation(crate::renderer::presentation::Wrapping {
        limits: *limits,
        rule,
        children,
        width: None,
    })
}

/// Packs independently measured children using the native wrapping algorithm.
///
/// # Errors
/// Stops at the first child error; already completed resource measurements are not rolled back.
pub fn try_resolve<E>(
    limits: &Limits,
    rule: Rule,
    count: usize,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    let limits = limits
        .width(rule.sizing.width)
        .height(rule.sizing.height)
        .shrink(rule.padding);
    let child_limits = limits.loose();
    let [spacing, cross_spacing] = rule.gaps;
    let maximum = rule.main(limits.max());
    let mut children: Vec<Node> = Vec::with_capacity(count);
    let mut intrinsic_main = 0.0_f32;
    let mut line_start = 0;
    let mut line_cross = 0.0_f32;
    let mut main = 0.0_f32;
    let mut cross = 0.0_f32;
    let cross_factor = factor(rule.cross_alignment);
    let align_cross = |range: std::ops::Range<usize>, extent: f32, children: &mut Vec<Node>| {
        if cross_factor != 0.0 {
            for child in &mut children[range] {
                let delta = rule.point(0.0, (extent - rule.cross(child.size())) / cross_factor);
                child.translate_mut(Vector::new(delta.x, delta.y));
            }
        }
    };
    for index in 0..count {
        let node = layout(index, &child_limits)?;
        let size = node.size();
        if main != 0.0 && main + rule.main(size) > maximum {
            intrinsic_main = intrinsic_main.max(main - spacing);
            align_cross(line_start..index, line_cross, &mut children);
            cross += line_cross + cross_spacing;
            main = 0.0;
            line_start = index;
            line_cross = 0.0;
        }
        line_cross = line_cross.max(rule.cross(size));
        let position = rule.point(main, cross);
        children.push(node.move_to(Point::new(
            position.x + rule.padding.left,
            position.y + rule.padding.top,
        )));
        main += rule.main(size) + spacing;
    }
    if main != 0.0 {
        intrinsic_main = intrinsic_main.max(main - spacing);
    }
    let intrinsic = rule.pack(intrinsic_main, cross + line_cross);
    align_cross(line_start..children.len(), line_cross, &mut children);
    let main_factor = factor(rule.main_alignment);
    if main_factor != 0.0 {
        let mut line_start = 0;
        for index in 0..children.len() {
            let bounds = children[index].bounds();
            let position = rule.main(Size::new(bounds.x, bounds.y));
            let extent = position + rule.main(bounds.size());
            let next = children
                .get(index + 1)
                .map(|node| {
                    let position = node.bounds().position();
                    rule.main(Size::new(position.x, position.y))
                })
                .unwrap_or_default();
            if next == 0.0 {
                let delta = rule.point((intrinsic_main - extent) / main_factor, 0.0);
                for child in &mut children[line_start..=index] {
                    child.translate_mut(Vector::new(delta.x, delta.y));
                }
                line_start = index + 1;
            }
        }
    }
    let size = limits.resolve(rule.sizing.width, rule.sizing.height, intrinsic);
    Ok(Node::with_children(size.expand(rule.padding), children))
}
