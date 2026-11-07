// Modified by enzUI contributors. See FORK.md.
//! Base-sized overlapping layers, measured in native paint order.

use super::{Limits, Node};
use crate::{Length, Size};

/// Measures the base first, then constrains every other layer to its resolved size.
///
/// Layer indices remain in paint order, including layers beneath the base. An
/// absent base yields an empty allocation and does not measure other children.
pub fn try_resolve<E>(
    limits: &Limits,
    sizing: Size<Length>,
    base: usize,
    children: usize,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    let limits = limits.width(sizing.width).height(sizing.height);
    if base >= children {
        return Ok(Node::new(limits.resolve(
            sizing.width,
            sizing.height,
            Size::ZERO,
        )));
    }
    let base_node = layout(base, &limits)?;
    let size = limits.resolve(sizing.width, sizing.height, base_node.size());
    let limits = Limits::new(Size::ZERO, size);
    let mut nodes = Vec::with_capacity(children);
    for index in 0..base {
        nodes.push(layout(index, &limits)?);
    }
    nodes.push(base_node);
    for index in base + 1..children {
        nodes.push(layout(index, &limits)?);
    }
    Ok(Node::with_children(size, nodes))
}
