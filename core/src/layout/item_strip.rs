// Modified by enzUI contributors. See FORK.md.
//! Complete retained material for width-dependent strips, without layout callbacks during replay.

use super::{Limits, Node, container, tab_strip};
use crate::{Alignment, Length, Padding, Point, Size};

/// Width-dependent allocation over a fixed, complete material inventory.
#[derive(Debug, Clone, PartialEq)]
pub enum Policy {
    /// Document membership, close variants, list and trailing controls.
    Tabs(tab_strip::Specification),
    /// Priority-selected command variants and a fixed-form collector.
    Actions(super::action_strip::Specification),
    /// Intrinsic-width prefix followed by an indicator when any item is omitted.
    Prefix {
        /// Number of items before the final indicator slot.
        items: usize,
        /// Logical-pixel spacing, including the gap before the indicator.
        spacing: f32,
    },
    /// Selects slot zero below the threshold, slot one otherwise; both reflow at live width.
    WidthChoice {
        /// Logical-pixel width; equality selects slot one.
        below: f32,
    },
}

impl Policy {
    /// Number of required material slots, bounded by the owned-program item budget.
    pub fn material_count(&self) -> Option<usize> {
        let count = match self {
            Self::Tabs(policy) => return policy.material_count(),
            Self::Actions(policy) => policy.material_count(),
            Self::Prefix { items, .. } => items.checked_add(1)?,
            Self::WidthChoice { .. } => 2,
        };
        (count <= crate::renderer::presentation::MAX_ITEMS).then_some(count)
    }

    /// Whether the policy has finite nonnegative parameters and a bounded inventory.
    pub fn valid(&self) -> bool {
        self.material_count().is_some()
            && match self {
                Self::Tabs(policy) => policy.valid(),
                Self::Actions(policy) => policy.valid(),
                Self::Prefix { spacing, .. } => spacing.is_finite() && *spacing >= 0.0,
                Self::WidthChoice { below } => below.is_finite() && *below >= 0.0,
            }
    }

    /// Native outer sizing; intrinsic prefixes do not consume unused trailing space.
    pub fn sizing(&self) -> Size<Length> {
        match self {
            Self::Prefix { .. } => Size::new(Length::Shrink, Length::Shrink),
            Self::Actions(_) => Size::new(Length::Fill, Length::Shrink),
            _ => Size::new(Length::Fill, Length::Fill),
        }
    }

    /// Whether direct children are assignment slots around independently measured material.
    pub fn wrapped(&self) -> bool {
        !matches!(self, Self::Tabs(_))
    }

    /// Tabs own their allocation clip; other strips inherit their caller's paint viewport.
    pub fn clips_to_bounds(&self) -> bool {
        matches!(self, Self::Tabs(_))
    }

    /// Intrinsic indicators may exceed allocations too narrow to contain even the indicator.
    pub fn allows_spill(&self) -> bool {
        matches!(self, Self::Prefix { .. })
    }
}

pub(crate) fn is_hidden_slot(layout: super::Placed<'_>) -> bool {
    layout
        .node
        .container_presentation()
        .is_some_and(|value| value.mode == container::Mode::Material)
        && layout.bounds().size() == Size::ZERO
}

pub(super) fn slot(material: Node, visible: bool, position: Point, limits: &Limits) -> Node {
    let child = material
        .presentation_sizing()
        .unwrap_or(Size::new(Length::Shrink, Length::Shrink));
    let size = if visible { material.size() } else { Size::ZERO };
    Node::with_children(size, vec![material.move_to(Point::ORIGIN)])
        .with_container_presentation(crate::renderer::presentation::Container {
            reflow_min_width: None,
            clip_content: false,
            inset: None,
            mode: container::Mode::Material,
            recorded: None,
            width: None,
            limits: *limits,
            sizing: Size::new(Length::Shrink, Length::Shrink),
            padding: Padding::ZERO,
            alignment: [Alignment::Start; 2],
            child,
        })
        .move_to(position)
        .interposed()
}

/// Measures all material before selecting visible slots. A child failure stops further queries.
pub fn try_resolve<E>(
    limits: &Limits,
    policy: &Policy,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    match policy {
        Policy::Tabs(policy) => tab_strip::try_resolve(limits, policy, layout),
        Policy::Actions(policy) => super::action_strip::try_resolve(limits, policy, layout),
        Policy::WidthChoice { below } => {
            let limits = limits.width(Length::Fill).height(Length::Fill);
            let child_limits = limits.loose();
            let mut material = Vec::with_capacity(2);
            for index in 0..2 {
                material.push(layout(index, &child_limits)?);
            }
            let selected = usize::from(limits.max().width >= *below);
            let size = limits.resolve(Length::Fill, Length::Fill, material[selected].size());
            let children = material
                .into_iter()
                .enumerate()
                .map(|(index, child)| {
                    let visible = index == selected;
                    let position = if visible {
                        Point::ORIGIN
                    } else {
                        Point::new(-10000.0 - child.size().width, 0.0)
                    };
                    slot(child, visible, position, &child_limits)
                })
                .collect();
            Ok(Node::with_children(size, children))
        }
        Policy::Prefix { items, spacing } => {
            let child_limits =
                Limits::new(Size::ZERO, Size::new(f32::INFINITY, limits.max().height));
            let mut material = Vec::with_capacity(policy.material_count().unwrap_or(0));
            for index in 0..=*items {
                material.push(layout(index, &child_limits)?);
            }
            let indicator_width = material[*items].size().width;
            let mut used = 0.0_f32;
            let mut height = 0.0_f32;
            let mut positions = Vec::with_capacity(*items);
            for child in material.iter().take(*items) {
                let gap = if positions.is_empty() { 0.0 } else { *spacing };
                let end = used + gap + child.size().width;
                if end > limits.max().width + 0.5 {
                    break;
                }
                positions.push(used + gap);
                used = end;
                height = height.max(child.size().height);
            }
            let indicator = positions.len() < *items;
            if indicator {
                while !positions.is_empty()
                    && used + spacing + indicator_width > limits.max().width + 0.5
                {
                    let _ = positions.pop();
                    used = positions
                        .last()
                        .map_or(0.0, |x| x + material[positions.len() - 1].size().width);
                }
            }
            let indicator_x = used + if positions.is_empty() { 0.0 } else { *spacing };
            if indicator {
                used = indicator_x + indicator_width;
                height = height.max(material[*items].size().height);
            }
            let children = material
                .into_iter()
                .enumerate()
                .map(|(index, child)| {
                    let x = if index < positions.len() {
                        Some(positions[index])
                    } else if index == *items && indicator {
                        Some(indicator_x)
                    } else {
                        None
                    };
                    let position = match x {
                        Some(x) => Point::new(x, ((height - child.size().height) / 2.0).max(0.0)),
                        None => Point::new(-10000.0 - child.size().width, 0.0),
                    };
                    slot(child, x.is_some(), position, &child_limits)
                })
                .collect();
            Ok(Node::with_children(Size::new(used, height), children))
        }
    }
}
