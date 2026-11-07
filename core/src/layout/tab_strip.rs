// Modified by enzUI contributors. See FORK.md.
//! Width-dependent document membership with a complete, clipped material inventory.

use super::{Limits, Node};
use crate::{Length, Point, Size};

/// Immutable tab and affordance policy; all widths are logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Specification {
    /// One entry per logical document, indicating whether it has a close action.
    pub closable: Vec<bool>,
    /// The selected document, clamped to the inventory when nonempty.
    pub active: usize,
    /// The width a document's title needs to stay legible, measured before
    /// publication; a document showing its close action needs
    /// `close_chrome` more.
    pub legibility: f32,
    /// Per-document width ceiling.
    pub maximum: f32,
    /// Minimum compact width before the selected close reservation is applied.
    pub hard_minimum: f32,
    /// Inactive close actions disappear below this width.
    pub close_threshold: f32,
    /// Minimum selected width in the compact close-preserving allocation.
    pub active_minimum: f32,
    /// Extra width reserved for the selected close action in compact allocation.
    pub close_chrome: f32,
    /// Gap between displayed children, including affordances.
    pub gap: f32,
    /// Width of the complete-document-list affordance.
    pub list_width: f32,
    /// Optional trailing affordance; the list takes priority when only one fits.
    pub trailing_width: Option<f32>,
}

/// Visible documents and optional affordances for one exact allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// First visible document in the logical inventory.
    pub start: usize,
    /// Widths of the contiguous visible document range.
    pub widths: Vec<f32>,
    /// Tab allocation after reserving the list, when folded.
    pub strip_area: f32,
    /// Whether the complete-document list occupies a slot.
    pub list: bool,
    /// Whether the optional trailing affordance occupies a slot.
    pub trailing: bool,
}

impl Specification {
    /// Three material slots per document (two close variants and one list-count variant), plus trailing.
    pub fn material_count(&self) -> Option<usize> {
        self.closable.len().checked_mul(3)?.checked_add(1)
    }

    /// Whether every policy width is finite and nonnegative and the material count is bounded.
    pub fn valid(&self) -> bool {
        self.material_count()
            .is_some_and(|count| count <= crate::renderer::presentation::MAX_ITEMS)
            && [
                self.legibility,
                self.maximum,
                self.hard_minimum,
                self.close_threshold,
                self.active_minimum,
                self.close_chrome,
                self.gap,
                self.list_width,
            ]
            .into_iter()
            .chain(self.trailing_width)
            .all(|v| v.is_finite() && v >= 0.0)
            && self.maximum > 0.0
            && self.list_width > 0.0
    }

    fn width(&self, available: f32, count: usize) -> f32 {
        if count == 0 {
            return self.maximum;
        }
        let share = available.max(0.0) / count as f32;
        share.min(self.maximum).max(self.hard_minimum.min(share))
    }

    /// Widths for an already selected contiguous document range, in logical pixels.
    pub fn document_widths(&self, available: f32, count: usize, active: usize) -> Vec<f32> {
        if count == 0 {
            return Vec::new();
        }
        let uniform = self.width(available, count);
        if uniform >= self.close_threshold || count == 1 {
            return vec![uniform; count];
        }
        let inactive = (available - self.close_chrome).max(0.0) / count as f32;
        let selected = (inactive + self.close_chrome)
            .max(self.active_minimum)
            .min(available.max(0.0));
        let others = self.width((available - selected).max(0.0), count - 1);
        (0..count)
            .map(|i| if i == active { selected } else { others })
            .collect()
    }

    fn gap_total(&self, count: usize) -> f32 {
        self.gap * count.saturating_sub(1) as f32
    }

    fn fits(&self, area: f32, count: usize) -> bool {
        if count == 0 {
            return true;
        }
        // The first slot stands for the selected document, which always shows
        // its close action; another shows it from `close_threshold` up. The
        // sample must fit beside a close action that shows.
        let closable = self.closable.iter().any(|closable| *closable);
        let widths = self.document_widths((area - self.gap_total(count)).max(0.0), count, 0);
        widths.iter().sum::<f32>() + self.gap_total(count) <= area + 0.5
            && widths.iter().enumerate().all(|(index, width)| {
                let close = closable && (index == 0 || *width >= self.close_threshold);
                let title = width - if close { self.close_chrome } else { 0.0 };
                title >= self.legibility - 0.5
            })
    }

    /// Plans the tab area after any trailing reserve, before small-affordance suppression.
    pub fn tab_area(&self, available: f32) -> Plan {
        let count = self.closable.len();
        if count == 0 {
            return Plan {
                start: 0,
                widths: Vec::new(),
                strip_area: available,
                list: false,
                trailing: false,
            };
        }
        let active = self.active.min(count - 1);
        if self.fits(available, count) {
            return Plan {
                start: 0,
                widths: self.document_widths(
                    (available - self.gap_total(count)).max(0.0),
                    count,
                    active,
                ),
                strip_area: available,
                list: false,
                trailing: false,
            };
        }
        // The list slot is placed after the tab run with one gap between them.
        let strip_area = (available - self.list_width - self.gap).max(0.0);
        let shown = (1..count)
            .rev()
            .find(|n| self.fits(strip_area, *n))
            .unwrap_or(0);
        let start = active
            .saturating_sub(shown / 2)
            .min(count.saturating_sub(shown));
        Plan {
            start,
            widths: self.document_widths(
                (strip_area - self.gap_total(shown)).max(0.0),
                shown,
                active.saturating_sub(start),
            ),
            strip_area,
            list: true,
            trailing: false,
        }
    }

    /// Selects membership without retaining or invoking a widget callback.
    pub fn plan(&self, available: f32) -> Plan {
        let available = available.max(0.0);
        let trailing = self.trailing_width.filter(|width| available >= *width);
        // Every displayed slot is placed one gap after the previous one, so a
        // trailing affordance costs its width plus the gap before it; without
        // that the row overran its box and the trailing control was clipped.
        let trailing_reserve = trailing.map_or(0.0, |width| width + self.gap);
        let mut plan = self.tab_area((available - trailing_reserve).max(0.0));
        let mut show_trailing = trailing.is_some();
        if plan.list
            && show_trailing
            && available < self.list_width + self.gap + trailing.unwrap_or(0.0)
        {
            show_trailing = false;
            plan = self.tab_area(available);
        }
        let show_list = plan.list && available >= self.list_width;
        if plan.list && !show_list {
            plan.widths.clear();
        }
        plan.list = show_list;
        plan.trailing = show_trailing;
        plan
    }

    /// Material index and width for each displayed slot, in paint and keyboard order.
    pub fn displayed(&self, plan: &Plan) -> Vec<(usize, f32)> {
        let count = self.closable.len();
        let active = self.active.min(count.saturating_sub(1));
        let mut slots = Vec::with_capacity(plan.widths.len() + 2);
        for (offset, &width) in plan.widths.iter().enumerate() {
            let index = plan.start + offset;
            let close = self.closable[index] && (index == active || width >= self.close_threshold);
            slots.push((index * 2 + usize::from(close), width));
        }
        if plan.list {
            let hidden = count - plan.widths.len();
            if hidden > 0 {
                slots.push((count * 2 + hidden - 1, self.list_width));
            }
        }
        if plan.trailing {
            slots.push((count * 3, self.trailing_width.unwrap_or(0.0)));
        }
        slots
    }
}

/// Allocates every material slot; undisplayed variants remain outside the clipped viewport.
///
/// Keeping full-width hidden material permits later membership changes without reconstructing widgets.
/// The caller must clip painting and restrict input/semantics to [`Specification::displayed`].
///
/// # Errors
/// Propagates a child refusal without querying subsequent children.
pub fn try_resolve<E>(
    limits: &Limits,
    policy: &Specification,
    mut layout: impl FnMut(usize, &Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    let plan = policy.plan(limits.max().width);
    let displayed = policy.displayed(&plan);
    let count = policy.closable.len();
    let material_count = policy.material_count().unwrap_or(0);
    let stride = policy
        .maximum
        .max(policy.list_width)
        .max(policy.trailing_width.unwrap_or(0.0))
        + 1.0;
    let mut children = Vec::with_capacity(material_count);
    for index in 0..material_count {
        let width = displayed
            .iter()
            .find(|(slot, _)| *slot == index)
            .map(|(_, width)| *width)
            .unwrap_or_else(|| {
                if index < count * 2 {
                    policy.maximum
                } else if index < count * 3 {
                    policy.list_width
                } else {
                    policy.trailing_width.unwrap_or(0.0)
                }
            });
        let child_limits = Limits::with_compression(
            Size::new(width, 0.0),
            Size::new(width, limits.max().height),
            Size::new(false, limits.compression().height),
        );
        children.push(layout(index, &child_limits)?);
    }
    let intrinsic_width = displayed
        .iter()
        .map(|(index, _)| children[*index].size().width)
        .sum::<f32>()
        + policy.gap_total(displayed.len());
    let intrinsic_height = displayed
        .iter()
        .map(|(index, _)| children[*index].size().height)
        .fold(0.0_f32, f32::max);
    let size = limits.resolve(
        Length::Fill,
        Length::Fill,
        Size::new(intrinsic_width, intrinsic_height),
    );
    for (index, node) in children.iter_mut().enumerate() {
        let y = ((size.height - node.size().height) / 2.0).max(0.0);
        node.move_to_mut(Point::new(size.width + 1.0 + index as f32 * stride, y));
    }
    let mut x = 0.0;
    for (index, _) in displayed {
        let node = &mut children[index];
        let y = node.bounds().y;
        node.move_to_mut(Point::new(x, y));
        x += node.size().width + policy.gap;
    }
    Ok(Node::with_children(size, children))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(count: usize, trailing: Option<f32>) -> Specification {
        Specification {
            closable: vec![true; count],
            active: count / 2,
            legibility: 36.0,
            maximum: 200.0,
            hard_minimum: 30.0,
            close_threshold: 60.0,
            active_minimum: 80.0,
            close_chrome: 24.0,
            gap: 2.0,
            list_width: 28.0,
            trailing_width: trailing,
        }
    }

    /// Every displayed slot, gaps included, fits the allocation. Before the
    /// planner reserved the gaps before the list and trailing slots, a folded
    /// row with a trailing affordance overran its box by those gaps and the
    /// trailing control was clipped away.
    #[test]
    fn displayed_slots_and_their_gaps_fit_the_allocation() {
        for trailing in [None, Some(28.0)] {
            let spec = spec(12, trailing);
            for available in (120..=900).step_by(7) {
                let available = available as f32;
                let plan = spec.plan(available);
                let displayed = spec.displayed(&plan);
                let used = displayed.iter().map(|(_, width)| width).sum::<f32>()
                    + spec.gap_total(displayed.len());
                assert!(
                    used <= available + 0.5,
                    "{trailing:?} at {available}: displayed slots need {used}"
                );
            }
        }
    }

    #[test]
    fn a_folded_row_keeps_its_trailing_affordance_when_both_fit() {
        let spec = spec(12, Some(28.0));
        let plan = spec.plan(470.0);
        assert!(plan.list && plan.trailing, "{plan:?}");
    }

    /// Every displayed tab keeps the legibility sample beside its close
    /// action: a tab wide enough to show its close shows the sample too, or
    /// the row folds.
    #[test]
    fn every_displayed_title_keeps_the_legibility_sample_beside_its_close() {
        let spec = Specification {
            legibility: 60.0,
            close_threshold: 80.0,
            close_chrome: 28.0,
            ..spec(13, None)
        };
        for available in (300..=1400).step_by(9) {
            let available = available as f32;
            let plan = spec.plan(available);
            for (index, width) in plan.widths.iter().enumerate() {
                let document = plan.start + index;
                let close = spec.closable[document]
                    && (document == spec.active || *width >= spec.close_threshold);
                let title = width - if close { spec.close_chrome } else { 0.0 };
                assert!(
                    title >= spec.legibility - 0.5 || plan.widths.len() == 1,
                    "at {available}: tab {document} is {width} wide, {title} for its title"
                );
            }
        }
    }
}
