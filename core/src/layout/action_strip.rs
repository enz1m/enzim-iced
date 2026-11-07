// Modified by enzUI contributors. See FORK.md.
//! Indexed command partitioning shared by native action bars and retained layout.

/// Where a visible command may be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Remains inline only while its priority-selected slot fits.
    #[default]
    Fold,
    /// Remains inline even when the slot budget is exhausted.
    Pinned,
    /// Never occupies an inline slot.
    Overflow,
}

/// Material variant used by an inline command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presentation {
    /// Full inline material.
    #[default]
    Expanded,
    /// An explicitly provided condensed variant.
    Compact,
}

/// Renderer-neutral policy for one authored command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// Controls inline eligibility independently of available width.
    pub placement: Placement,
    /// Lower values fold first; later authored entries lose equal-priority ties.
    pub priority: u32,
    /// Invisible commands appear in neither output partition.
    pub visible: bool,
    /// Initial presentation before optional fitting compaction.
    pub preferred: Presentation,
}

/// Authored-order partitions referring to the original command slice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Original indices and selected material variants.
    pub inline: Vec<(usize, Presentation)>,
    /// Original indices of menu commands.
    pub overflow: Vec<usize>,
}

/// Intrinsic widths in logical pixels; missing compact material is never synthesized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Widths {
    /// Width of the command's normal inline material.
    pub expanded: f32,
    /// Width of an explicitly supplied compact variant.
    pub compact: Option<f32>,
}

/// Partitions commands by an inline-slot budget while preserving authored order.
/// Pinned commands do not fold; `usize::MAX` is the explicit unbounded budget.
pub fn resolve(commands: &[Command], budget: usize) -> Plan {
    let mut inline = Vec::new();
    let mut overflow = Vec::new();
    let mut foldable = Vec::new();
    for (index, command) in commands
        .iter()
        .enumerate()
        .filter(|(_, command)| command.visible)
    {
        match command.placement {
            Placement::Pinned => inline.push(index),
            Placement::Overflow => overflow.push(index),
            Placement::Fold => foldable.push(index),
        }
    }
    let remaining = if budget == usize::MAX {
        usize::MAX
    } else {
        budget.saturating_sub(inline.len())
    };
    if remaining >= foldable.len() {
        inline.extend(foldable);
    } else {
        foldable.sort_by_key(|&index| (commands[index].priority, std::cmp::Reverse(index)));
        let split = foldable.len() - remaining;
        overflow.extend_from_slice(&foldable[..split]);
        inline.extend_from_slice(&foldable[split..]);
    }
    inline.sort_unstable();
    overflow.sort_unstable();
    Plan {
        inline: inline
            .into_iter()
            .map(|index| (index, commands[index].preferred))
            .collect(),
        overflow,
    }
}

/// Largest whole-item prefix, reserving the collector only when something folds.
/// Invalid available widths fold everything; fit tolerance is half a logical pixel.
pub fn measured_budget(
    widths: &[f32],
    available: f32,
    gap: f32,
    trigger: f32,
    trigger_gap: f32,
) -> usize {
    if !available.is_finite() || available < 0.0 {
        return 0;
    }
    let nonnegative = |value: f32| {
        if value.is_finite() && value >= 0.0 {
            value
        } else {
            0.0
        }
    };
    let gap = nonnegative(gap);
    let trigger_gap = nonnegative(trigger_gap);
    let fit = |budget: f32| {
        let mut used = 0.0;
        let mut count = 0;
        for (index, &width) in widths.iter().enumerate() {
            if !width.is_finite() || width < 0.0 {
                break;
            }
            let step = width + if index == 0 { 0.0 } else { gap };
            if used + step > budget + 0.5 {
                break;
            }
            used += step;
            count += 1;
        }
        count
    };
    if fit(available) == widths.len() {
        return widths.len();
    }
    fit((available - nonnegative(trigger) - trigger_gap).max(0.0))
}

/// Chooses the largest priority-selected set that fits, compacting lower priorities first.
/// Widths use authored indices. Missing widths cannot fit; pinned commands still remain inline.
pub fn resolve_fitting(
    commands: &[Command],
    widths: &[Widths],
    available: f32,
    gap: f32,
    trigger: f32,
    trigger_gap: f32,
    collector_always: bool,
) -> Plan {
    let candidates = commands
        .iter()
        .filter(|command| command.visible && command.placement != Placement::Overflow)
        .count();
    let used = |plan: &Plan| {
        let mut used = 0.0;
        for (position, &(index, presentation)) in plan.inline.iter().enumerate() {
            if position > 0 {
                used += gap;
            }
            used += widths
                .get(index)
                .map_or(f32::INFINITY, |width| match presentation {
                    Presentation::Expanded => width.expanded,
                    Presentation::Compact => width.compact.unwrap_or(width.expanded),
                });
        }
        if collector_always || !plan.overflow.is_empty() {
            if !plan.inline.is_empty() {
                used += trigger_gap;
            }
            used += trigger;
        }
        used
    };
    for budget in (0..=candidates).rev() {
        let mut plan = resolve(commands, budget);
        if used(&plan) <= available + 0.5 {
            return plan;
        }
        let mut compactable: Vec<_> = plan
            .inline
            .iter()
            .filter(|(index, _)| {
                widths
                    .get(*index)
                    .is_some_and(|width| width.compact.is_some())
            })
            .map(|&(index, _)| index)
            .collect();
        compactable.sort_by_key(|&index| commands[index].priority);
        for index in compactable {
            if let Some((_, presentation)) = plan
                .inline
                .iter_mut()
                .find(|(candidate, _)| *candidate == index)
            {
                *presentation = Presentation::Compact;
            }
            if used(&plan) <= available + 0.5 {
                return plan;
            }
        }
    }
    resolve(commands, 0)
}

/// Collector arrangement relative to the inline command row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorPlacement {
    /// Collector immediately follows the inline row.
    Inline,
    /// Only the collector is pinned to the trailing edge.
    Trailing,
    /// The surviving command cluster hugs the trailing edge unless it owns the residual width.
    TrailingCluster,
}

/// Complete inline material for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variants {
    /// Normal inline material slot.
    pub expanded: usize,
    /// Optional explicit compact material slot.
    pub compact: Option<usize>,
    /// The original inline composite declares a fluid width and a non-shrink cross extent.
    pub owns_residual: bool,
}

/// Callback-free folding and allocation policy for a closed command bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Specification {
    /// Authored command metadata, including invisible and overflow-only commands.
    pub commands: Vec<Command>,
    /// Inline material for each eligible command; absent for invisible and overflow-only commands.
    pub variants: Vec<Option<Variants>>,
    /// Native sizing of every material slot, including the collector.
    pub sizing: Vec<crate::Size<crate::Length>>,
    /// Material slot containing the collector.
    pub collector: Option<usize>,
    /// Logical-pixel gap between inline commands and within an inline collector cluster.
    pub gap: f32,
    /// Uniform inline width, or individually measured widths when absent.
    pub slot_width: Option<f32>,
    /// Explicit inline budget overriding width-driven folding.
    pub forced_budget: Option<usize>,
    /// Maximum foldable commands kept inline, without evicting pinned commands.
    pub inline_limit: Option<usize>,
    /// Whether the collector remains present with an empty overflow partition.
    pub collector_always: bool,
    /// Collector placement within the bar's allocation.
    pub placement: CollectorPlacement,
    /// Horizontal placement of an intrinsic cluster inside the bar's Fill allocation.
    pub align: crate::Alignment,
}

impl Specification {
    /// Required material population; every slot must have exactly one declared owner.
    pub fn material_count(&self) -> usize {
        self.sizing.len()
    }

    /// Checks complete slot ownership and finite, nonnegative layout parameters.
    pub fn valid(&self) -> bool {
        let count = self.material_count();
        if count > crate::renderer::presentation::MAX_ITEMS
            || self.commands.len() != self.variants.len()
            || self.commands.len() > crate::renderer::presentation::MAX_ITEMS
            || self.collector.is_some_and(|index| index >= count)
            || !self.gap.is_finite()
            || self.gap < 0.0
            || self
                .slot_width
                .is_some_and(|width| !width.is_finite() || width <= 0.0)
        {
            return false;
        }
        let needs_collector = self.collector_always
            || self
                .commands
                .iter()
                .any(|command| command.visible && command.placement != Placement::Pinned);
        if needs_collector && self.collector.is_none() {
            return false;
        }
        let mut used = vec![false; count];
        if let Some(index) = self.collector {
            used[index] = true;
        }
        for (command, variants) in self.commands.iter().zip(&self.variants) {
            let eligible = command.visible && command.placement != Placement::Overflow;
            if eligible != variants.is_some() {
                return false;
            }
            if let Some(variants) = variants {
                for index in std::iter::once(variants.expanded).chain(variants.compact) {
                    if index >= count || used[index] {
                        return false;
                    }
                    used[index] = true;
                }
            }
        }
        used.into_iter().all(|used| used)
    }

    /// Resolves membership from live intrinsic material widths and the assigned outer width.
    pub fn plan(&self, available: f32, material_widths: &[f32]) -> Plan {
        let n = self
            .commands
            .iter()
            .filter(|command| command.visible && command.placement != Placement::Overflow)
            .count();
        let trigger = self.collector.map_or(0.0, |index| {
            material_widths.get(index).copied().unwrap_or(f32::INFINITY)
        });
        let widths: Vec<_> = self
            .variants
            .iter()
            .map(|variants| {
                variants.map_or(
                    Widths {
                        expanded: f32::INFINITY,
                        compact: None,
                    },
                    |variants| Widths {
                        expanded: self.slot_width.unwrap_or_else(|| {
                            material_widths
                                .get(variants.expanded)
                                .copied()
                                .unwrap_or(f32::INFINITY)
                        }),
                        compact: if self.slot_width.is_none() {
                            variants
                                .compact
                                .and_then(|index| material_widths.get(index).copied())
                        } else {
                            None
                        },
                    },
                )
            })
            .collect();
        let plan = if let Some(budget) = self.forced_budget {
            resolve(&self.commands, budget)
        } else if let Some(slot) = self
            .slot_width
            .filter(|_| self.placement == CollectorPlacement::Inline)
        {
            let width = available + self.gap;
            let pitch = slot + self.gap;
            let slots = if width.is_finite() && width > 0.0 && pitch.is_finite() && pitch > 0.0 {
                ((width / pitch).floor().max(0.0) as usize).min(usize::MAX - 1)
            } else {
                0
            };
            let budget = if self.collector_always {
                slots.saturating_sub(1).min(n)
            } else if slots >= n {
                slots
            } else {
                slots.saturating_sub(1)
            };
            resolve(&self.commands, budget)
        } else if self.placement == CollectorPlacement::Inline {
            let widths: Vec<_> = self
                .commands
                .iter()
                .zip(&widths)
                .filter(|(command, _)| command.visible && command.placement != Placement::Overflow)
                .map(|(_, width)| width.expanded)
                .collect();
            let budget = if self.collector_always {
                measured_budget(
                    &widths,
                    (available - trigger - self.gap).max(0.0),
                    self.gap,
                    0.0,
                    self.gap,
                )
            } else {
                measured_budget(&widths, available, self.gap, trigger, self.gap)
            };
            resolve(&self.commands, budget)
        } else {
            resolve_fitting(
                &self.commands,
                &widths,
                available,
                self.gap,
                trigger,
                0.0,
                self.collector_always,
            )
        };
        if let Some(limit) = self.inline_limit {
            let pinned = plan
                .inline
                .iter()
                .filter(|(index, _)| self.commands[*index].placement == Placement::Pinned)
                .count();
            if plan.inline.len().saturating_sub(pinned) > limit {
                return resolve(&self.commands, pinned.saturating_add(limit));
            }
        }
        plan
    }

    /// Material indices selected for inline commands, preserving authored order.
    pub fn inline_slots(&self, plan: &Plan) -> Vec<usize> {
        plan.inline
            .iter()
            .filter_map(|&(index, presentation)| {
                self.variants
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|variants| match presentation {
                        Presentation::Expanded => variants.expanded,
                        Presentation::Compact => variants.compact.unwrap_or(variants.expanded),
                    })
            })
            .collect()
    }
}

use super::{Limits, Node, flex};
use crate::{Alignment, Length, Padding, Point, Size, Vector, length};

enum Part {
    Material(usize),
    Space,
    Row {
        width: Length,
        gap: f32,
        children: Vec<Part>,
    },
}

/// Fill weight of a row that encloses a max-bounded fill child. The spacer
/// before a trailing cluster weighs 1, so it takes only what the cluster's
/// bound leaves instead of splitting the band with it.
const ENCLOSED_FILL_WEIGHT: u16 = 1024;

impl Part {
    fn sizing(&self, policy: &Specification, widths: &[f32]) -> Size<Length> {
        match self {
            Self::Material(index) => policy.sizing[*index],
            Self::Space => Size::new(Length::Fill, Length::Shrink),
            Self::Row {
                width,
                gap,
                children,
            } => Size::new(
                enclosed_width(*width, *gap, children, policy, widths),
                Length::Shrink,
            ),
        }
    }

    /// The most main extent this part asks for: a bounded fill's maximum,
    /// a fixed length, or the measured intrinsic width; `None` when unbounded.
    fn cap(&self, policy: &Specification, widths: &[f32]) -> Option<f32> {
        match self.sizing(policy, widths).width {
            Length::Bounded {
                sizing: length::Sizing::Fill(_),
                bounds: length::Bounds::Max(max),
            } => Some(max),
            Length::Fixed(width) => Some(width),
            Length::Shrink | Length::Fit => match self {
                Self::Material(index) => widths.get(*index).copied().filter(|w| w.is_finite()),
                Self::Space => Some(0.0),
                Self::Row { gap, children, .. } => {
                    let mut total = *gap * children.len().saturating_sub(1) as f32;
                    for child in children {
                        total += child.cap(policy, widths)?;
                    }
                    Some(total)
                }
            },
            _ => None,
        }
    }
}

/// A `Shrink` row resolves to the intrinsic extent of its children, which
/// compresses a fill child to nothing: a fill-width search in a trailing
/// cluster measured zero. A row holding a max-bounded fill child therefore
/// fills up to the sum of its children's caps, as a sized row would.
/// Unbounded fill children keep the shrink behaviour.
fn enclosed_width(
    width: Length,
    gap: f32,
    children: &[Part],
    policy: &Specification,
    widths: &[f32],
) -> Length {
    if width != Length::Shrink {
        return width;
    }
    let bounded_fill = children.iter().any(|child| {
        matches!(
            child.sizing(policy, widths).width,
            Length::Bounded {
                sizing: length::Sizing::Fill(_),
                bounds: length::Bounds::Max(_),
            }
        )
    });
    if !bounded_fill {
        return width;
    }
    let mut cap = gap * children.len().saturating_sub(1) as f32;
    for child in children {
        match child.cap(policy, widths) {
            Some(extent) => cap += extent,
            None => return width,
        }
    }
    Length::Bounded {
        sizing: length::Sizing::Fill(ENCLOSED_FILL_WEIGHT),
        bounds: length::Bounds::Max(cap),
    }
}

fn layout_part<E>(
    part: &Part,
    policy: &Specification,
    widths: &[f32],
    limits: &Limits,
    layout: &mut impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    match part {
        Part::Material(index) => layout(*index, limits),
        Part::Space => Ok(super::atomic_node(limits, Length::Fill, Length::Shrink)),
        Part::Row {
            width,
            gap,
            children,
        } => {
            struct Children<'a, F> {
                parts: &'a [Part],
                policy: &'a Specification,
                widths: &'a [f32],
                layout: &'a mut F,
            }
            impl<E, F: FnMut(usize, &Limits) -> Result<Node, E>> flex::TryContent for Children<'_, F> {
                type Error = E;
                fn count(&self) -> usize {
                    self.parts.len()
                }
                fn sizing(&mut self, index: usize) -> Result<Size<Length>, E> {
                    Ok(self.parts[index].sizing(self.policy, self.widths))
                }
                fn layout(&mut self, index: usize, limits: &Limits) -> Result<Node, E> {
                    layout_part(
                        &self.parts[index],
                        self.policy,
                        self.widths,
                        limits,
                        self.layout,
                    )
                }
            }
            flex::try_resolve_content(
                flex::Axis::Horizontal,
                limits,
                enclosed_width(*width, *gap, children, policy, widths),
                Length::Shrink,
                Padding::ZERO,
                *gap,
                Alignment::Center,
                &mut Children {
                    parts: children,
                    policy,
                    widths,
                    layout,
                },
            )
        }
    }
}

fn gather(part: &Part, node: &Node, offset: Vector, material: &mut [Option<Node>]) {
    let position = offset + Vector::new(node.bounds().x, node.bounds().y);
    match part {
        Part::Material(index) => {
            material[*index] = Some(node.clone().move_to(Point::new(position.x, position.y)));
        }
        Part::Space => {}
        Part::Row { children, .. } => {
            for (part, child) in children.iter().zip(node.children()) {
                gather(part, child, position, material);
            }
        }
    }
}

/// Measures complete material, selects a live plan and applies the native nested-row allocation.
/// The specification must be valid. Child errors stop further measurement without publishing a partial node.
pub fn try_resolve<E>(
    limits: &Limits,
    policy: &Specification,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    let limits = limits.width(Length::Fill).height(Length::Shrink);
    let available = limits.max().width;
    let probe = Limits::new(Size::ZERO, Size::INFINITE)
        .width(Length::Shrink)
        .height(Length::Shrink);
    let mut widths = Vec::with_capacity(policy.material_count());
    for index in 0..policy.material_count() {
        widths.push(layout(index, &probe)?.size().width);
    }
    let plan = policy.plan(available, &widths);
    let slots = policy.inline_slots(&plan);
    let owns_residual = policy.slot_width.is_none()
        && plan.inline.iter().any(|&(index, _)| {
            policy.variants[index].is_some_and(|variants| variants.owns_residual)
        });
    let show_collector = !plan.overflow.is_empty() || policy.collector_always;
    // The gap separates surviving inline commands from the collector; a
    // collector alone has nothing to keep apart (enzUI-ICED #3658).
    let cluster_gap = if slots.is_empty() { 0.0 } else { policy.gap };
    let inline = Part::Row {
        width: if policy.placement == CollectorPlacement::TrailingCluster && owns_residual {
            Length::Fill
        } else {
            Length::Shrink
        },
        gap: policy.gap,
        children: slots.into_iter().map(Part::Material).collect(),
    };
    let mut cluster = vec![inline];
    if show_collector && let Some(index) = policy.collector {
        cluster.push(Part::Material(index));
    }
    let part = match policy.placement {
        CollectorPlacement::Inline => Part::Row {
            width: Length::Shrink,
            gap: cluster_gap,
            children: cluster,
        },
        CollectorPlacement::Trailing if cluster.len() > 1 => {
            cluster.insert(1, Part::Space);
            Part::Row {
                width: Length::Fill,
                gap: 0.0,
                children: cluster,
            }
        }
        CollectorPlacement::Trailing => Part::Row {
            width: Length::Shrink,
            gap: cluster_gap,
            children: cluster,
        },
        CollectorPlacement::TrailingCluster if owns_residual => Part::Row {
            width: Length::Fill,
            gap: cluster_gap,
            children: cluster,
        },
        CollectorPlacement::TrailingCluster => Part::Row {
            width: Length::Fill,
            gap: 0.0,
            children: vec![
                Part::Space,
                Part::Row {
                    width: Length::Shrink,
                    gap: cluster_gap,
                    children: cluster,
                },
            ],
        },
    };
    let mut material = Vec::with_capacity(policy.material_count());
    for (index, &width) in widths.iter().enumerate() {
        let width = if available.is_finite() {
            available.max(0.0)
        } else if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        material.push(layout(
            index,
            &Limits::new(Size::ZERO, Size::new(width, limits.max().height)),
        )?);
    }
    let content = layout_part(&part, policy, &widths, &limits.loose(), &mut layout)?;
    let size = limits.resolve(Length::Fill, Length::Shrink, content.size());
    let content = content.align(policy.align, Alignment::Start, size);
    let mut selected = vec![None; policy.material_count()];
    gather(&part, &content, Vector::ZERO, &mut selected);
    let children = material
        .into_iter()
        .enumerate()
        .map(|(index, hidden)| {
            let selected = selected[index].take();
            let visible = selected.is_some();
            let material = selected.unwrap_or(hidden).with_drawing_cull(false);
            let position = if visible {
                material.bounds().position()
            } else {
                Point::new(-10000.0 - material.size().width, 0.0)
            };
            super::item_strip::slot(material, visible, position, &limits)
        })
        .collect();
    Ok(Node::with_children(size, children))
}
