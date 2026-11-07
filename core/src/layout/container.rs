// Modified by enzUI contributors. See FORK.md.
//! Native childless and single-child allocation shared with owned responsive layout.

use super::{Limits, Node};
use crate::{Alignment, Length, Padding, Point, Size};

/// Order in which the outer allocation, padding and child are resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Resolves an allocation with no children or intrinsic content.
    Atomic,
    /// A cached factory is retained only while its construction constraints stay unchanged.
    CachedFactory {
        /// Available size supplied when constructing the captured content.
        available: Size,
    },
    /// An unmounted subtree stays zero-sized even under nonzero minimum limits.
    Void,
    /// Reserved virtual content that must stay outside every presented viewport until materialized.
    Unrealized,
    /// Measure under loose padded limits, then align within the resolved outer size.
    Aligned,
    /// Preserve padded minimum constraints while measuring, then align the child.
    Padded,
    /// Resolve the outer field height before fitting padding and measuring its child.
    Field,
    /// Forward the original limits and preserve the child's size and position.
    Passthrough,
    /// Resolve the wrapper's own sizing without changing the child's native origin.
    Frame,
    /// Apply a height floor while preserving growth above it.
    HeightFloor {
        /// Minimum height in logical pixels, clamped to the available maximum.
        minimum: f32,
        /// Start raises the child's minimum; other alignments place its natural height.
        alignment: Alignment,
    },
    /// A strip assigns this slot's visible size; its retained child keeps full intrinsic geometry.
    Material,
    /// Cap the child's available width while reserving room for a later sibling.
    WidthCap {
        /// Logical width retained for later siblings.
        reserve: f32,
        /// Minimum cap before the ceiling applies.
        minimum: f32,
        /// Maximum width the child may receive.
        ceiling: f32,
    },
}

pub(crate) fn try_resolve<E>(
    limits: &Limits,
    sizing: Size<Length>,
    padding: Padding,
    alignment: [Alignment; 2],
    mode: Mode,
    mut layout: impl FnMut(&Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    match mode {
        Mode::Void => Ok(Node::new(Size::ZERO)),
        Mode::Atomic | Mode::Unrealized => {
            Ok(super::atomic_node(limits, sizing.width, sizing.height))
        }
        Mode::CachedFactory { .. } => {
            let limits = limits.width(sizing.width).height(sizing.height);
            let child = layout(&limits.loose())?;
            let size = limits.resolve(sizing.width, sizing.height, child.size());
            Ok(Node::with_children(size, vec![child]))
        }
        Mode::Aligned => super::try_positioned(
            limits,
            sizing.width,
            sizing.height,
            padding,
            |limits| layout(&limits.loose()),
            |content, size| content.align(alignment[0], alignment[1], size),
        ),
        Mode::Padded => super::try_positioned(
            limits,
            sizing.width,
            sizing.height,
            padding,
            layout,
            |content, size| content.align(alignment[0], alignment[1], size),
        ),
        Mode::WidthCap {
            reserve,
            minimum,
            ceiling,
        } => {
            let cap = (limits.max().width - reserve).max(minimum).min(ceiling);
            let min = limits.min();
            let capped = Limits::with_compression(
                Size::new(min.width.min(cap), min.height),
                Size::new(cap, limits.max().height),
                limits.compression(),
            );
            let child = layout(&capped)?;
            Ok(Node::with_children(child.size(), vec![child]))
        }
        Mode::HeightFloor { minimum, alignment } => {
            let max = limits.max();
            let min = limits.min();
            let floor = minimum.min(max.height).max(min.height);
            let child_limits = Limits::with_compression(
                Size::new(
                    min.width,
                    if alignment == Alignment::Start {
                        floor
                    } else {
                        0.0
                    },
                ),
                max,
                limits.compression(),
            );
            let child = layout(&child_limits)?;
            if alignment == Alignment::Start {
                return Ok(Node::with_children(child.size(), vec![child]));
            }
            let space = Size::new(
                child.size().width,
                floor.max(child.size().height).min(max.height),
            );
            let child = child
                .move_to(Point::ORIGIN)
                .align(Alignment::Start, alignment, space);
            Ok(Node::with_children(space, vec![child]))
        }
        Mode::Frame => {
            let limits = limits.width(sizing.width).height(sizing.height);
            let child = layout(&limits)?;
            let size = limits.resolve(sizing.width, sizing.height, child.size());
            Ok(Node::with_children(size, vec![child]))
        }
        Mode::Passthrough | Mode::Material => {
            let child = layout(limits)?;
            Ok(Node::with_children(child.size(), vec![child]))
        }
        Mode::Field => {
            let limits = limits.width(sizing.width).height(sizing.height);
            let desired_height = if matches!(sizing.height, Length::Fixed(_)) {
                0.0
            } else {
                let max = limits.max();
                let probe = Limits::new(
                    Size::ZERO,
                    Size::new(
                        (max.width - padding.x()).max(0.0),
                        (max.height - padding.y()).max(0.0),
                    ),
                );
                layout(&probe)?.size().height + padding.y()
            };
            let outer = limits.resolve(sizing.width, sizing.height, Size::new(0.0, desired_height));
            let padding = padding.fit(Size::ZERO, outer);
            let inner = Limits::new(
                Size::ZERO,
                Size::new(
                    (outer.width - padding.left - padding.right).max(0.0),
                    (outer.height - padding.top - padding.bottom).max(0.0),
                ),
            );
            let child = layout(&inner)?.move_to(Point::new(padding.left, padding.top));
            Ok(Node::with_children(outer, vec![child]))
        }
    }
}
