// Modified by enzUI contributors. See FORK.md.
//! Native two-cell allocation shared with retained layout.

use super::{Limits, Node};
use crate::{Alignment, Length, Padding, Point, Size};

/// One arrangement's geometry; the leading-width override applies only to rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arm {
    /// Insets in logical pixels.
    pub padding: Padding,
    /// Inter-cell distance in logical pixels.
    pub spacing: f32,
    /// Alignment perpendicular to the arrangement's main axis.
    pub align: Alignment,
    /// Outer height policy, independent of each child's intrinsic height.
    pub height: Length,
    /// Fixed widths reserve their gutter even when the leading child hugs narrower.
    /// Proportional widths share space with the trailing cell, then apply their bounds.
    pub leading_width: Length,
}

/// Both arrangements are retained; positive finite widths below the breakpoint stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// Outer width policy, applied before selecting an arrangement.
    pub width: Length,
    /// Strict stacking threshold in logical pixels.
    pub breakpoint: f32,
    /// Geometry at and above the threshold, including unbounded probes.
    pub row: Arm,
    /// Geometry below the threshold; leading width is ignored.
    pub stacked: Arm,
}

/// Allocates both children and declares the same algorithm for retained replay.
/// Descendant paint and layout must be independently capturable. Finite height is
/// shared in leading-first order; both allocations remain inside that height.
/// An unbounded width gives the leading cell an intrinsic offer instead of a share.
///
/// # Panics
/// Panics in debug builds for an internal `Length::Fluid` leading policy, or a
/// proportional allocation that leaves no trailing space in a finite positive offer.
pub fn resolve(
    limits: &Limits,
    policy: Policy,
    children: [Size<Length>; 2],
    mut layout: impl FnMut(usize, &Limits) -> Node,
) -> Node {
    let result = try_resolve(limits, policy, children, None, |slot, limits| {
        Ok::<_, std::convert::Infallible>(layout(slot, limits))
    });
    match result {
        Ok(node) => node.with_pair_presentation(*limits, policy, children),
        Err(never) => match never {},
    }
}

pub(crate) fn try_resolve<E>(
    limits: &Limits,
    policy: Policy,
    children: [Size<Length>; 2],
    mut refusal: Option<E>,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    let outer = limits.width(policy.width);
    let available = outer.max().width;
    let stacked = available.is_finite() && available > 0.0 && available < policy.breakpoint;
    let arm = if stacked { policy.stacked } else { policy.row };

    let inner = outer.height(arm.height).shrink(arm.padding);
    let inner_max = inner.max();
    let (mut lead_node, mut trail_node) = if stacked {
        let bounded = inner_max.height.is_finite();
        let offered_h = if bounded {
            inner_max.height.max(0.0)
        } else {
            f32::INFINITY
        };
        let lead = layout(
            0,
            &Limits::new(Size::ZERO, Size::new(inner_max.width, offered_h)),
        )?;
        let lead_h = lead.size().height;
        let trail_h_max = if bounded {
            (offered_h - lead_h - arm.spacing).max(0.0)
        } else {
            f32::INFINITY
        };
        let trail = layout(
            1,
            &Limits::new(Size::ZERO, Size::new(inner_max.width, trail_h_max)),
        )?;
        let trail_y = if bounded {
            (lead_h + arm.spacing).min(offered_h)
        } else {
            lead_h + arm.spacing
        };
        (lead, trail.move_to(Point::new(0.0, trail_y)))
    } else {
        let space = arm.spacing.max(0.0);
        let avail = (inner_max.width - space).max(0.0);
        let lead_factor = arm.leading_width.fill_factor();

        let unsupported_fluid =
            arm.leading_width.fill_factor() > 0 && arm.leading_width.min(0.0).fill_factor() == 0;
        if unsupported_fluid && let Some(error) = refusal.take() {
            return Err(error);
        }
        debug_assert!(
            !unsupported_fluid,
            "reflow_pair: leading_width {:?} is a Length::Fluid; its open-ended \
             constraint cannot be read back and is IGNORED (treated as Fill). Use \
             Length::Fill.min(px) / .max(px), which is honoured exactly",
            arm.leading_width
        );

        let bounded = inner_max.width.is_finite();
        let trail_factor = children[1].width.fill_factor();
        let mut trail_pre = None;

        let (lead, lead_w, rest) = if lead_factor > 0 && bounded {
            let raw = if trail_factor > 0 {
                avail * lead_factor as f32 / (lead_factor as f32 + trail_factor as f32)
            } else {
                // Static trailing content precedes a proportional gutter, matching native flex's allocation order.
                let trail = layout(
                    1,
                    &Limits::new(Size::ZERO, Size::new(avail, inner_max.height)),
                )?;
                let taken = trail.size().width.min(avail);
                trail_pre = Some(trail);
                (avail - taken).max(0.0)
            };
            let probe = Limits::new(Size::ZERO, Size::new(avail, 0.0)).width(arm.leading_width);
            let (lo, hi) = (probe.min().width, probe.max().width.max(probe.min().width));
            let share = raw.clamp(lo, hi).clamp(0.0, avail);
            let limits = Limits::new(Size::new(share, 0.0), Size::new(share, inner_max.height));
            let lead = layout(0, &limits)?;
            (lead, share, (avail - share).max(0.0))
        } else {
            let policy = if lead_factor > 0 {
                Length::Shrink
            } else {
                arm.leading_width
            };
            let base = Limits::new(Size::ZERO, Size::new(avail, inner_max.height)).width(policy);
            // A static gutter must measure fluid descendants intrinsically instead of consuming the trailing cell's width.
            let lead_limits = Limits::with_compression(
                base.min(),
                base.max(),
                Size::new(true, base.compression().height),
            );
            let lead = layout(0, &lead_limits)?;
            let lead_w = match arm.leading_width {
                Length::Fixed(w) => w.min(avail),
                _ => lead.size().width,
            };
            let rest = if bounded {
                (avail - lead_w).max(0.0)
            } else {
                f32::INFINITY
            };
            (lead, lead_w, rest)
        };

        let starved = lead_factor > 0 && bounded && avail > 0.0 && rest <= 0.0;
        if starved && let Some(error) = refusal.take() {
            return Err(error);
        }
        debug_assert!(
            !starved,
            "reflow_pair: leading_width {:?} took all {avail}px of the row and left \
             the trailing child {rest}px -- a proportional leading width must never \
             collapse the trailing pane",
            arm.leading_width
        );

        let trail = match trail_pre {
            Some(trail) => trail,
            None => layout(
                1,
                &Limits::new(Size::ZERO, Size::new(rest, inner_max.height)).width(Length::Fill),
            )?,
        };
        (
            lead.move_to(Point::new(0.0, 0.0)),
            trail.move_to(Point::new(lead_w + space, 0.0)),
        )
    };

    let content = if stacked {
        let sum = lead_node.size().height + arm.spacing + trail_node.size().height;
        let sum = if inner_max.height.is_finite() {
            sum.min(inner_max.height.max(0.0))
        } else {
            sum
        };
        Size::new(inner_max.width, sum)
    } else {
        Size::new(
            inner_max.width,
            lead_node.size().height.max(trail_node.size().height),
        )
    };
    let resolved = inner.resolve(policy.width, arm.height, content);

    if stacked {
        lead_node.align_mut(arm.align, Alignment::Start, Size::new(resolved.width, 0.0));
        trail_node.align_mut(arm.align, Alignment::Start, Size::new(resolved.width, 0.0));
    } else {
        lead_node.align_mut(Alignment::Start, arm.align, Size::new(0.0, resolved.height));
        trail_node.align_mut(Alignment::Start, arm.align, Size::new(0.0, resolved.height));
    }
    lead_node.move_to_mut(Point::new(
        lead_node.bounds().x + arm.padding.left,
        lead_node.bounds().y + arm.padding.top,
    ));
    trail_node.move_to_mut(Point::new(
        trail_node.bounds().x + arm.padding.left,
        trail_node.bounds().y + arm.padding.top,
    ));

    Ok(Node::with_children(
        resolved.expand(Size::new(
            arm.padding.left + arm.padding.right,
            arm.padding.top + arm.padding.bottom,
        )),
        vec![lead_node, trail_node],
    ))
}
