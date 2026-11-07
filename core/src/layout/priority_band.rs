// Modified by enzUI contributors. See FORK.md.
//! Four-slot allocation with reserved controls, bounded renegotiation, and a centred middle slot.

use super::{Limits, Node};
use crate::{Length, Point, Rectangle, Size, Vector};

/// Leading content in the fixed four-child ordering.
pub const CONTENT: usize = 0;
/// Optional middle content, centred on the entire band.
pub const CENTER: usize = 1;
/// Trailing commands allocated after the higher-priority slots.
pub const ACTIONS: usize = 2;
/// Window controls measured before any other slot.
pub const CONTROLS: usize = 3;

/// Allocation policy; all distances are logical pixels and absent slots still supply empty children.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Specification {
    /// Height constraint applied before any child measurement.
    pub height: Length,
    /// Reserves an inter-slot gap and derives the trailing inset from control height.
    pub has_controls: bool,
    /// Centres the middle on the full band using mirrored controls, not leading content width.
    pub has_center: bool,
    /// Enables command-floor and intrinsic-width probes.
    pub has_actions: bool,
    /// Commands that cannot fold, including any required collector.
    pub action_min_width: f32,
    /// Wide command inventory used to detect saturation, not a minimum reservation.
    pub action_reserve_width: f32,
    /// Retains the command floor when an intrinsic probe is unusable but a finite slot can render.
    pub action_can_render_without_intrinsic_width: bool,
    /// Minimum leading allocation when negotiating against the wide command inventory.
    pub fluid_content_min_width: Option<f32>,
    /// Reserves a readable share for fill-sized titles even when an icon gives the row nonzero intrinsic width.
    pub has_window_title: bool,
    /// Gap between occupied slots; absent control and action slots contribute no gap.
    pub spacing: f32,
    /// Leading inset, also used at the trailing edge when controls are absent.
    pub padding_x: f32,
    /// Actual shell band height for control centring; zero uses the resolved local height.
    pub centring_height: f32,
    /// Minimum command probe width; returned descendant extent determines its reservation.
    pub action_probe_width: f32,
    /// Fraction of the available content width reserved for a framework window title.
    pub leading_share: f32,
    /// Descendant spill tolerance shared with input and semantic clipping.
    pub spill_epsilon: f32,
    /// Right-to-left: the slots are reflected across the band, so content
    /// leads from the right and the controls sit at the left edge.
    pub mirrored: bool,
}

/// Geometry of a native intrinsic probe, reusable only with the same slot and constraints.
///
/// Contains no widget state or callbacks; at most three probes belong to one band.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub(crate) slot: usize,
    pub(crate) limits: Limits,
    pub(crate) size: Size,
    pub(crate) extent: Rectangle,
}

impl Probe {
    /// Captures the allocation and descendant extent before the probe's temporary tree is dropped.
    pub fn capture(slot: usize, limits: Limits, node: &Node) -> Self {
        Self {
            slot,
            limits,
            size: node.size(),
            extent: subtree_extent(node),
        }
    }

    pub(crate) fn geometry(&self) -> Node {
        Node::with_children(
            self.size,
            vec![Node::new(self.extent.size()).move_to(self.extent.position())],
        )
    }

    pub(crate) fn valid(&self) -> bool {
        self.slot < 4
            && [
                self.limits.min().width,
                self.limits.min().height,
                self.limits.max().width,
                self.limits.max().height,
                self.size.width,
                self.size.height,
                self.extent.width,
                self.extent.height,
            ]
            .into_iter()
            .all(|v| !v.is_nan() && v >= 0.0)
            && self.extent.x.is_finite()
            && self.extent.y.is_finite()
    }
}

/// The state-isolation requirements of a child measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pass {
    /// Must not replace the interactive child state used by the committed layout.
    Probe,
    /// Commits geometry to the state used by drawing and input.
    Layout,
    /// One additional leading-child layout with the measured deficit available to its descendants.
    Renegotiate(f32),
}

/// Matches the vertical inset of centred controls; invalid or negative slack yields zero.
pub fn trailing_inset(band_height: f32, control_height: f32) -> f32 {
    let slack = (band_height - control_height) / 2.0;
    if slack.is_finite() && slack > 0.0 {
        slack
    } else {
        0.0
    }
}

/// Includes overhanging descendants except content behind a declared scroll or tab viewport.
///
/// A measured node is shallow; it carries its subtree's extent when the
/// measurer attached one ([`Node::with_measured_extent`]).
pub fn subtree_extent(node: &Node) -> Rectangle {
    if let Some(extent) = node.measured_extent {
        return extent + crate::Vector::new(node.position().x, node.position().y);
    }
    layout_extent(super::Placed::with_offset(crate::Vector::ZERO, node))
}

/// [`subtree_extent`] of a laid out tree, relative to its origin, walking
/// the tree without building a snapshot (enzICED).
pub fn tree_extent(tree: &crate::widget::Tree) -> Rectangle {
    layout_extent_in(super::Layout::new(tree.size), tree)
}

/// [`layout_extent`] over a widget's tree, without building a snapshot
/// (enzICED).
pub fn layout_extent_in(layout: super::Layout, tree: &crate::widget::Tree) -> Rectangle {
    let bounds = layout.bounds();
    let mut extent = (bounds.width > 0.0 && bounds.height > 0.0).then_some(bounds);
    if !clips(tree.presentation()) {
        visit(tree, layout.position(), &mut |bounds, declared| {
            if hidden(declared, bounds) {
                return false;
            }
            if bounds.width > 0.0 && bounds.height > 0.0 {
                extent = Some(extent.map_or(bounds, |prior| prior.union(&bounds)));
            }
            !clips(declared)
        });
    }
    extent.unwrap_or(Rectangle::new(layout.position(), Size::ZERO))
}

/// [`content_clip`] over a widget's tree, without building a snapshot
/// (enzICED).
pub fn content_clip_in(
    layout: super::Layout,
    tree: &crate::widget::Tree,
    epsilon: f32,
) -> Rectangle {
    if clips(tree.presentation()) {
        return layout.bounds();
    }
    let slot = layout.bounds();
    let edge = slot.x + slot.width;
    let mut cut = edge;
    visit(tree, layout.position(), &mut |bounds, declared| {
        if hidden(declared, bounds) {
            return false;
        }
        if bounds.width > 0.0 && bounds.height > 0.0 && bounds.x + bounds.width > edge + epsilon {
            cut = cut.min(bounds.x.max(0.0));
        }
        !clips(declared)
    });
    Rectangle::new(
        slot.position(),
        Size::new((cut - slot.x).max(0.0), slot.height),
    )
}

/// Visits the descendants of `tree` placed at `origin` as its snapshot
/// lists them, interposed slots included; `visit` answers whether to
/// descend.
fn visit(
    tree: &crate::widget::Tree,
    origin: Point,
    visit_node: &mut impl FnMut(Rectangle, Option<&Node>) -> bool,
) {
    let mut each = |child: &crate::widget::Tree| {
        let position = origin + child.translation;
        if let Some(slot) = child.interposed.as_deref() {
            let slot_bounds = Rectangle::new(
                origin + crate::Vector::new(slot.position().x, slot.position().y),
                slot.size(),
            );
            if !visit_node(slot_bounds, Some(slot)) {
                return;
            }
        }
        if visit_node(Rectangle::new(position, child.size), child.presentation()) {
            visit(child, position, visit_node);
        }
    };
    match tree.presentation().and_then(|node| node.placed.as_deref()) {
        Some(placed) => placed
            .iter()
            .filter_map(|&index| tree.children.get(index))
            .for_each(&mut each),
        None => tree.children.iter().for_each(&mut each),
    }
}

fn hidden(declared: Option<&Node>, bounds: Rectangle) -> bool {
    declared
        .and_then(Node::container_presentation)
        .is_some_and(|value| value.mode == super::container::Mode::Material)
        && bounds.size() == Size::ZERO
}

fn clips(declared: Option<&Node>) -> bool {
    declared.is_some_and(|node| {
        node.scroll_presentation().is_some()
            || node
                .tab_strip_presentation()
                .is_some_and(|(_, value)| value.policy.clips_to_bounds())
    })
}

/// Computes negotiation extent in absolute layout coordinates, excluding clipped tab inventory.
pub fn layout_extent(layout: super::Placed<'_>) -> Rectangle {
    fn extent(layout: super::Placed<'_>) -> Option<Rectangle> {
        let bounds = layout.bounds();
        let own = (bounds.width > 0.0 && bounds.height > 0.0).then_some(bounds);
        if clips(Some(layout.node())) {
            return own;
        }
        layout
            .children()
            .filter(|&child| !super::item_strip::is_hidden_slot(child))
            .filter_map(extent)
            .fold(own, |acc, bounds| {
                Some(acc.map_or(bounds, |prior| prior.union(&bounds)))
            })
    }
    extent(layout).unwrap_or(Rectangle::new(layout.position(), Size::ZERO))
}

/// Reserves controls before leading content and commands, issuing at most nine child layouts.
///
/// Children use the fixed [`CONTENT`], [`CENTER`], [`ACTIONS`], [`CONTROLS`] ordering.
/// Probe callbacks must isolate interactive state and return descendant geometry.
/// Intrinsic probes use unbounded shrink constraints. Only a spilling leading child receives
/// [`Pass::Renegotiate`], once, so a non-converging child cannot cause an allocation loop.
/// Callback errors are propagated without further measurement.
pub fn try_resolve<E>(
    limits: &Limits,
    policy: Specification,
    mut layout: impl FnMut(usize, &Limits, Pass) -> Result<Node, E>,
) -> Result<Node, E> {
    let limits = limits.width(Length::Fill).height(policy.height);
    let Size {
        width: total_w,
        height: total_h,
    } = limits.max();
    let gap = if policy.has_controls {
        policy.spacing
    } else {
        0.0
    };
    let lead = policy.padding_x;
    let centre_y = |node: Node| {
        let height = node.size().height;
        node.translate(Vector::new(0.0, ((total_h - height) / 2.0).max(0.0)))
    };
    let controls_limits = Limits::new(Size::ZERO, Size::new((total_w - lead).max(0.0), total_h));
    let controls_node = layout(CONTROLS, &controls_limits, Pass::Layout)?;
    let controls_width = controls_node.size().width;
    let trail = if policy.has_controls {
        let height = if policy.centring_height > 0.0 {
            policy.centring_height
        } else {
            total_h
        };
        trailing_inset(height, controls_node.size().height)
    } else {
        policy.padding_x
    };
    let (lead, trail) = {
        let want = lead + trail;
        if want > total_w && want > 0.0 {
            let factor = total_w / want;
            (lead * factor, trail * factor)
        } else {
            (lead, trail)
        }
    };
    let inner_width = (total_w - lead - trail).max(0.0);
    let content_available = (inner_width - controls_width - gap).max(0.0);
    let shrink = Limits::new(Size::ZERO, Size::INFINITE)
        .width(Length::Shrink)
        .height(Length::Shrink);
    // The actions' measured inline width, when they report one.
    let mut action_intrinsic = None;
    let action_floor = if policy.has_actions {
        let floor_limits = Limits::new(
            Size::ZERO,
            Size::new(policy.action_probe_width.min(content_available), total_h),
        );
        let floor_node = layout(ACTIONS, &floor_limits, Pass::Probe)?;
        let extent = subtree_extent(&floor_node);
        let intrinsic = layout(ACTIONS, &shrink, Pass::Probe)?.size().width;
        let usable = intrinsic.is_finite() && intrinsic >= policy.spill_epsilon;
        if usable {
            action_intrinsic = Some(intrinsic);
        }
        if !usable && !policy.action_can_render_without_intrinsic_width {
            0.0
        } else {
            (extent.x + extent.width)
                .max(policy.action_probe_width)
                .max(policy.action_min_width)
                .min(content_available)
        }
    } else {
        0.0
    };
    let has_actions = policy.has_actions && action_floor > 0.0;
    let content_cap = if has_actions {
        (content_available - action_floor - policy.spacing).max(0.0)
    } else {
        content_available
    };
    // The leading content's intrinsic width: the controls before the title,
    // which never shorten (a window title reports none).
    let mut content_intrinsic = None;
    let content_limits = if let Some(minimum) = policy.fluid_content_min_width {
        // A measured run is exact (group separators, a collector only when
        // one renders); the slot estimate stands in only without it.
        let reserve = action_intrinsic.unwrap_or(policy.action_reserve_width);
        let wide = (content_available - reserve - policy.spacing).max(minimum);
        let want = wide.min(content_cap);
        Limits::new(Size::new(want, 0.0), Size::new(want, total_h))
    } else if has_actions {
        let intrinsic = layout(CONTENT, &shrink, Pass::Probe)?.size().width;
        let intrinsic = if intrinsic.is_finite() && intrinsic >= 0.0 {
            content_intrinsic = Some(intrinsic);
            intrinsic
        } else {
            // Probed once: the centre reads no floor rather than probing again.
            content_intrinsic = Some(0.0);
            content_cap
        };
        let want = if policy.has_window_title {
            intrinsic.max(content_available * policy.leading_share)
        } else {
            intrinsic
        }
        .min(content_cap);
        Limits::new(Size::new(want, 0.0), Size::new(want, total_h))
    } else {
        Limits::new(Size::ZERO, Size::new(content_cap, total_h))
    };
    let content_node = layout(CONTENT, &content_limits, Pass::Layout)?;
    let extent = subtree_extent(&content_node);
    let deficit = extent.x + extent.width - content_node.size().width;
    let content_node = if deficit > policy.spill_epsilon {
        layout(CONTENT, &content_limits, Pass::Renegotiate(deficit))?
    } else {
        content_node
    };
    let controls_x = (total_w - trail - controls_width).max(lead);
    // The centre yields to the window controls and to the actions' floor
    // (at least their overflow trigger), so a narrow band never leaves a
    // command unreachable; it stays centred on the band, so it gives up
    // width on both sides.
    let action_reserve = if has_actions {
        action_floor + policy.spacing
    } else {
        0.0
    };
    let trail_zone = if policy.has_center {
        (total_w - (controls_x - gap - action_reserve)).max(0.0)
    } else {
        0.0
    };
    // It yields to the leading controls too: they never shorten, the title
    // after them does.
    let lead_zone = if policy.has_center {
        let floor = match content_intrinsic {
            Some(width) => width,
            None => {
                let width = layout(CONTENT, &shrink, Pass::Probe)?.size().width;
                if width.is_finite() && width >= 0.0 {
                    width
                } else {
                    0.0
                }
            }
        }
        .min(content_cap);
        if floor > 0.0 {
            lead + floor + policy.spacing
        } else {
            lead
        }
    } else {
        lead
    };
    let middle_half = (total_w / 2.0 - trail_zone.max(lead_zone)).max(0.0);
    let center_limits = Limits::new(Size::ZERO, Size::new((middle_half * 2.0).max(0.0), total_h));
    let center_node = layout(CENTER, &center_limits, Pass::Layout)?;
    let center_x = ((total_w - center_node.size().width) / 2.0).max(0.0);
    let center_node = centre_y(center_node.move_to(Point::new(center_x, 0.0)));
    // What the centre paints, which can spill past a slot squeezed narrower
    // than its content (a collapsed search trigger keeps its size). The
    // leading content ends before it and the commands start after it.
    let center_paint = policy
        .has_center
        .then(|| subtree_extent(&center_node))
        .filter(|extent| extent.width > 0.0);
    // A slot narrower than what its content paints is laid out again at that
    // width: a container squeezed to nothing draws none of its children.
    let (center_node, center_paint) = match center_paint {
        Some(extent) if extent.width > center_node.size().width + policy.spill_epsilon => {
            let wide = Limits::new(Size::ZERO, Size::new(extent.width, total_h));
            let node = layout(CENTER, &wide, Pass::Layout)?;
            let node = centre_y(node.move_to(Point::new(extent.x, 0.0)));
            let paint = subtree_extent(&node);
            (node, Some(paint).filter(|extent| extent.width > 0.0))
        }
        _ => (center_node, center_paint),
    };
    let center_room =
        center_paint.map_or(center_x, |extent| extent.x.min(center_x)) - policy.spacing - lead;
    let content_node = if center_paint.is_some() && content_node.size().width > center_room {
        let room = center_room.max(0.0);
        layout(
            CONTENT,
            &Limits::new(Size::ZERO, Size::new(room, total_h)),
            Pass::Layout,
        )?
    } else {
        content_node
    };
    let content_width = content_node.size().width;
    let after_content = lead
        + content_width
        + if content_width > 0.0 {
            policy.spacing
        } else {
            0.0
        };
    let after_center = center_paint.map_or(0.0, |extent| {
        let bounds = center_node.bounds();
        (extent.x + extent.width).max(bounds.x + bounds.width) + policy.spacing
    });
    let saturated =
        content_width + 0.5 >= content_cap && policy.action_reserve_width > policy.action_min_width;
    let minimum_x = (controls_x - gap - policy.action_min_width).max(0.0);
    let actions_x = if saturated {
        after_content.max(after_center).max(minimum_x)
    } else {
        after_content.max(after_center)
    }
    .min(controls_x);
    let actions_limits = Limits::new(
        Size::ZERO,
        Size::new((controls_x - gap - actions_x).max(0.0), total_h),
    );
    let actions_node = layout(ACTIONS, &actions_limits, Pass::Layout)?;
    let mut slots = vec![
        centre_y(content_node.move_to(Point::new(lead, 0.0))),
        center_node,
        centre_y(actions_node.move_to(Point::new(actions_x, 0.0))),
        centre_y(controls_node.move_to(Point::new(controls_x, 0.0))),
    ];
    if policy.mirrored {
        slots = slots
            .into_iter()
            .map(|node| {
                let bounds = node.bounds();
                node.move_to(Point::new(total_w - bounds.x - bounds.width, bounds.y))
            })
            .collect();
    }
    Ok(Node::with_children(Size::new(total_w, total_h), slots))
}

/// Truncates a content slot before descendants that cannot fit whole at its trailing edge.
///
/// Descendant rectangles must use the same coordinate space as the slot. The tolerance
/// is in logical pixels and must also govern input and semantic containment.
pub fn clip_content_boxes(
    slot: Rectangle,
    descendants: impl IntoIterator<Item = Rectangle>,
    epsilon: f32,
) -> Rectangle {
    let edge = slot.x + slot.width;
    let mut cut = edge;
    for bounds in descendants {
        if bounds.width > 0.0 && bounds.height > 0.0 && bounds.x + bounds.width > edge + epsilon {
            cut = cut.min(bounds.x.max(0.0));
        }
    }
    Rectangle::new(
        slot.position(),
        Size::new((cut - slot.x).max(0.0), slot.height),
    )
}

/// Computes the content boundary from an existing native layout, including overhanging descendants.
pub fn content_clip(layout: super::Placed<'_>, epsilon: f32) -> Rectangle {
    if clips(Some(layout.node())) {
        return layout.bounds();
    }
    let mut pending: Vec<_> = layout.children().collect();
    let descendants = std::iter::from_fn(|| {
        while let Some(node) = pending.pop() {
            if super::item_strip::is_hidden_slot(node) {
                continue;
            }
            if !clips(Some(node.node())) {
                pending.extend(node.children());
            }
            return Some(node.bounds());
        }
        None
    });
    clip_content_boxes(layout.bounds(), descendants, epsilon)
}
