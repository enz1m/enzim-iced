// Modified by enzUI contributors. See FORK.md.
//! Distribute elements using a flex-based layout.
// This code is heavily inspired by the [`druid`] codebase.
//
// [`druid`]: https://github.com/xi-editor/druid
//
// Copyright 2018 The xi-editor Authors, Héctor Ramón
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
use crate::Element;

use crate::layout::{Limits, Measure, Node};
use crate::length;
use crate::widget;
use crate::{Alignment, Length, Padding, Size};

/// The main axis of a flex layout.
#[derive(Debug, Clone, Copy)]
pub enum Axis {
    /// The horizontal axis
    Horizontal,

    /// The vertical axis
    Vertical,
}

impl Axis {
    fn main<T>(self, size: Size<T>) -> T {
        match self {
            Axis::Horizontal => size.width,
            Axis::Vertical => size.height,
        }
    }

    fn cross<T>(self, size: Size<T>) -> T {
        match self {
            Axis::Horizontal => size.height,
            Axis::Vertical => size.width,
        }
    }

    fn pack<T>(self, main: T, cross: T) -> (T, T) {
        match self {
            Axis::Horizontal => (main, cross),
            Axis::Vertical => (cross, main),
        }
    }
}

/// Computes the flex layout with the given axis and limits, applying spacing,
/// padding and alignment to the items as needed.
///
/// It returns a new layout [`Node`].
pub fn resolve<Message, Theme, Renderer>(
    axis: Axis,
    renderer: &Renderer,
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    items: &mut [Element<'_, Message, Theme, Renderer>],
    trees: &mut [widget::Tree],
) -> Node
where
    Renderer: crate::Renderer,
{
    resolve_content(
        axis,
        limits,
        width,
        height,
        padding,
        spacing,
        align_items,
        &mut Widgets {
            renderer,
            items,
            trees,
        },
    )
}

/// Resolves gaps only between children whose measured main-axis size exceeds epsilon.
///
/// A collapsed child keeps its tree/layout slot. At most two measurement passes return
/// unused gap space to siblings; the same rule is retained for renderer-owned reflow.
pub fn resolve_collapsing<Message, Theme, Renderer>(
    axis: Axis,
    renderer: &Renderer,
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    items: &mut [Element<'_, Message, Theme, Renderer>],
    trees: &mut [widget::Tree],
) -> Node
where
    Renderer: crate::Renderer,
{
    match try_resolve_collapsing_content(
        axis,
        limits,
        width,
        height,
        padding,
        spacing,
        align_items,
        &mut InfallibleContent(&mut Widgets {
            renderer,
            items,
            trees,
        }),
    ) {
        Ok(node) => node,
        Err(never) => match never {},
    }
}

/// Resolves collapsed-gap flex with the same measured-child rule as [`resolve_collapsing`].
///
/// # Errors
/// Returns a child refusal without querying later children. Earlier measurements may
/// have side effects, including those from the first pass when gaps collapse.
pub fn try_resolve_collapsing_content<C: TryContent>(
    axis: Axis,
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    content: &mut C,
) -> Result<Node, C::Error> {
    let max_gaps = content.count().saturating_sub(1);
    let gap_size = |count: usize| {
        let (x, y) = axis.pack(spacing * count as f32, 0.0);
        Size::new(x, y)
    };
    let mut node = try_resolve_content(
        axis,
        &limits.shrink(gap_size(max_gaps)),
        width,
        height,
        padding,
        0.0,
        align_items,
        content,
    )?;
    let active = node
        .children()
        .iter()
        .filter(|child| axis.main(child.size()) > f32::EPSILON)
        .count();
    let gaps = active.saturating_sub(1);
    if gaps != max_gaps {
        node = try_resolve_content(
            axis,
            &limits.shrink(gap_size(gaps)),
            width,
            height,
            padding,
            0.0,
            align_items,
            content,
        )?;
    }
    let declaration = node.flex_presentation().map(|value| {
        let mut declaration = value.clone();
        declaration.collapse_spacing = true;
        declaration.limits = *limits;
        declaration.spacing = spacing;
        declaration
    });
    let mut children = node.children().to_vec();
    let mut seen = false;
    let mut offset = 0.0;
    for child in &mut children {
        if axis.main(child.size()) > f32::EPSILON {
            if seen {
                offset += spacing;
            }
            seen = true;
        }
        let (x, y) = axis.pack(offset, 0.0);
        child.translate_mut(crate::Vector::new(x, y));
    }
    let size = node.size() + gap_size(gaps);
    let node = Node::with_children(size, children);
    Ok(match declaration {
        Some(declaration) => node.with_flex_presentation(declaration),
        None => node,
    })
}

/// Indexed children for the shared widget and owned-presentation flex algorithm.
///
/// The child count must remain stable during resolution. Sizing is queried in
/// child order and may be interleaved with layout of earlier static children.
pub trait Content {
    /// Number of children participating in this layout, excluding filtered void widgets.
    fn count(&self) -> usize;
    /// Length constraints for the indexed child before its first layout request.
    fn sizing(&mut self, index: usize) -> Size<Length>;
    /// Measures the child within the supplied limits; may run after sibling measurements.
    fn layout(&mut self, index: usize, limits: &Limits) -> Node;
}

/// Indexed intrinsic layout that can refuse unavailable or unsupported owned resources.
///
/// Child count is stable during resolution; queries retain the same ordering as [`Content`].
pub trait TryContent {
    /// The caller's refusal type, propagated without replacing a failed child with empty geometry.
    type Error;

    /// Number of children in this layout, excluding filtered void widgets.
    fn count(&self) -> usize;
    /// Queries constraints before the child's first layout request.
    fn sizing(&mut self, index: usize) -> Result<Size<Length>, Self::Error>;
    /// Measures one child; an error stops resolution before any further child queries.
    fn layout(&mut self, index: usize, limits: &Limits) -> Result<Node, Self::Error>;
}

struct InfallibleContent<'a, C>(&'a mut C);

impl<C: Content> TryContent for InfallibleContent<'_, C> {
    type Error = std::convert::Infallible;

    fn count(&self) -> usize {
        self.0.count()
    }

    fn sizing(&mut self, index: usize) -> Result<Size<Length>, Self::Error> {
        Ok(self.0.sizing(index))
    }

    fn layout(&mut self, index: usize, limits: &Limits) -> Result<Node, Self::Error> {
        Ok(self.0.layout(index, limits))
    }
}

struct Widgets<'a, 'b, Message, Theme, Renderer> {
    renderer: &'b Renderer,
    items: &'b mut [Element<'a, Message, Theme, Renderer>],
    trees: &'b mut [widget::Tree],
}

impl<Message, Theme, Renderer: crate::Renderer> Content
    for Widgets<'_, '_, Message, Theme, Renderer>
{
    fn count(&self) -> usize {
        self.items.len()
    }
    fn sizing(&mut self, index: usize) -> Size<Length> {
        self.items[index].as_widget().size()
    }
    fn layout(&mut self, index: usize, limits: &Limits) -> Node {
        self.items[index]
            .as_widget_mut()
            .measure(&mut self.trees[index], self.renderer, limits)
    }
}

/// Resolves flex geometry from indexed intrinsic measurements, without owning widgets.
///
/// Uses the same compression, min/max, fill-factor and alignment passes as [`resolve`].
/// The caller owns the measurements and must keep their resource access appropriate
/// for the thread performing layout.
pub fn resolve_content(
    axis: Axis,
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    content: &mut impl Content,
) -> Node {
    match try_resolve_content(
        axis,
        limits,
        width,
        height,
        padding,
        spacing,
        align_items,
        &mut InfallibleContent(content),
    ) {
        Ok(node) => node,
        Err(never) => match never {},
    }
}

/// Resolves the shared flex algorithm without publishing partially measured geometry.
///
/// # Errors
/// Returns the first child sizing or layout error and makes no further child queries.
/// Completed child queries are not rolled back; callers own their resource/cache side effects.
pub fn try_resolve_content<C: TryContent>(
    axis: Axis,
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    spacing: f32,
    align_items: Alignment,
    content: &mut C,
) -> Result<Node, C::Error> {
    let captured_limits = *limits;
    let limits = limits.width(width).height(height).shrink(padding);
    let total_spacing = spacing * content.count().saturating_sub(1) as f32;
    // Upstream #3472: the max is a hint on an infinite axis, which the
    // children inherit.
    let max_cross = axis.cross(limits.max);

    let compression = limits.compression;
    let infinite = limits.infinite;

    let (main_compress, cross_compress) = axis.pack(compression.width, compression.height);

    let compression = {
        let (compress_x, compress_y) = axis.pack(main_compress, false);
        Size::new(compress_x, compress_y)
    };

    let cross_infinite = {
        let (infinite_x, infinite_y) = axis.pack(axis.main(infinite), false);
        Size::new(infinite_x, infinite_y)
    };

    let cross_dynamic = cross_compress || axis.cross(infinite);

    let mut fill_main_sum = 0;
    let mut some_fill_cross = false;
    let mut some_fill_max = false;
    let mut some_fill_min = false;
    let mut min_total = 0.0;
    let mut min_factors = 0;
    let mut cross = 0.0f32;
    let mut available = axis.main(limits.max) - total_spacing;

    let mut nodes: Vec<Node> = Vec::with_capacity(content.count());
    nodes.resize(content.count(), Node::default());

    #[derive(Debug, Clone, Copy)]
    struct Meta {
        main: Length,
        cross: Length,
        category: Category,
        resolved: bool,
    }

    #[derive(Debug, Clone, Copy)]
    enum Category {
        Static,
        CrossFluid,
        CrossFluidDeferred(f32),
        MainFluid,
    }

    let mut metas = Vec::with_capacity(content.count());
    let mut deferred_main = 0.0f32;

    // A compressed cross extent must come from intrinsic siblings
    // before cross-fluid children can use it.
    for (i, node) in nodes.iter_mut().enumerate() {
        let size = content.sizing(i)?;
        let (size_main, size_cross) = axis.pack(size.width, size.height);

        let fill_main_factor = size_main.fill_factor();
        let fill_cross_factor = size_cross.fill_factor();
        let main_is_static = main_compress || fill_main_factor == 0;

        let category = match (main_is_static, cross_dynamic, fill_cross_factor == 0) {
            (true, false, _) | (true, _, true) => Category::Static,
            (true, true, false) => {
                if let Length::Fixed(main) = size_main {
                    available -= main;
                    deferred_main += main;
                    Category::CrossFluidDeferred(main)
                } else {
                    Category::CrossFluid
                }
            }
            (false, _, _) => Category::MainFluid,
        };

        let meta = Meta {
            main: size_main,
            cross: size_cross,
            resolved: false,
            category,
        };

        metas.push(meta);

        match meta.main {
            Length::Bounded {
                sizing: length::Sizing::Fill(_),
                bounds: length::Bounds::Min(min),
            }
            | Length::Fluid(length::Constraint::Min(min)) => {
                min_total += min;
                min_factors += fill_main_factor;
                some_fill_min = true;
            }
            Length::Bounded {
                sizing: length::Sizing::Fill(_),
                bounds: length::Bounds::Max(_),
            }
            | Length::Fluid(length::Constraint::Max) => {
                some_fill_max = true;
            }
            Length::Bounded {
                sizing: length::Sizing::Fill(_),
                bounds: length::Bounds::Both { .. },
            } => {
                some_fill_max = true;
                some_fill_min = true;
            }
            _ => {}
        }

        let Category::Static = meta.category else {
            fill_main_sum += fill_main_factor;
            some_fill_cross = some_fill_cross || fill_cross_factor != 0;
            continue;
        };

        let (max_width, max_height) = axis.pack(available.max(0.0), max_cross);

        let child_limits = Limits::with_flags(
            Size::ZERO,
            Size::new(max_width, max_height),
            compression,
            infinite,
        );

        let layout = content.layout(i, &child_limits)?;

        let size = layout.size();

        available -= axis.main(size);
        cross = cross.max(axis.cross(size));
        *node = layout;
    }

    // A fixed-size static child keeps its extent. In-order measurement lets
    // an earlier shrinking sibling (a long label before a badge) claim the
    // space first and crush the fixed child; when that happened, measure
    // the static children again with every later fixed extent reserved, so
    // the label elides or wraps instead. Rows that fit are measured once.
    let fixed_main = |meta: &Meta| match (meta.category, meta.main) {
        (Category::Static, Length::Fixed(main)) => Some(main),
        _ => None,
    };
    let crushed = !main_compress
        && metas.iter().zip(&nodes).any(|(meta, node)| {
            fixed_main(meta).is_some_and(|main| axis.main(node.size()) + 0.5 < main)
        });
    let mut room = axis.main(limits.max) - total_spacing - deferred_main;
    let mut reserve: f32 = metas.iter().filter_map(fixed_main).sum();
    // When the fixed children alone do not fit, reserving them would starve
    // every earlier child; keep in-order measurement there.
    if crushed && reserve <= room {
        cross = 0.0;
        for i in 0..content.count() {
            let meta = metas[i];
            let Category::Static = meta.category else {
                continue;
            };
            let limit = if let Some(main) = fixed_main(&meta) {
                reserve -= main;
                room
            } else {
                room - reserve
            };
            let (max_width, max_height) = axis.pack(limit.max(0.0), max_cross);
            let child_limits = Limits::with_flags(
                Size::ZERO,
                Size::new(max_width, max_height),
                compression,
                infinite,
            );
            let layout = content.layout(i, &child_limits)?;
            let size = layout.size();
            room -= axis.main(size);
            cross = cross.max(axis.cross(size));
            nodes[i] = layout;
        }
        available = room;
    }

    // Fixed-main children can wait for the main-fluid pass
    // to establish the final compressed cross extent.
    if cross_dynamic && some_fill_cross {
        for i in 0..content.count() {
            let meta = metas[i];

            let Category::CrossFluid = meta.category else {
                continue;
            };

            let (max_width, max_height) = axis.pack(available.max(0.0), cross);

            let child_limits = Limits::with_flags(
                Size::ZERO,
                Size::new(max_width, max_height),
                compression,
                cross_infinite,
            );

            let layout = content.layout(i, &child_limits)?;

            let size = layout.size();

            available -= axis.main(size);
            cross = cross.max(axis.cross(size));
            nodes[i] = layout;
        }
    }

    let mut remaining = available.max(0.0);

    // Maximum bounds release space before minimum bounds claim it;
    // stabilization must not remeasure resolved children.
    #[derive(Debug, Clone, Copy)]
    enum Stage {
        Max,
        Min,
    }

    let mut step = if main_compress {
        None
    } else if some_fill_max {
        Some(Stage::Max)
    } else if some_fill_min {
        Some(Stage::Min)
    } else {
        None
    };

    while let Some(stage) = step {
        let current = remaining;
        let (reserved_space, reserved_factors) = match stage {
            Stage::Max => (min_total, min_factors),
            Stage::Min => (0.0, 0),
        };

        for i in 0..content.count() {
            let meta = &mut metas[i];

            if meta.resolved {
                continue;
            }

            let fill_main_factor = meta.main.fill_factor();

            if fill_main_factor == 0 {
                continue;
            }

            let bounds = match stage {
                Stage::Max => match meta.main {
                    Length::Bounded {
                        bounds: bounds @ (length::Bounds::Max(_) | length::Bounds::Both { .. }),
                        ..
                    } => bounds,
                    Length::Fluid(length::Constraint::Max) => length::Bounds::Min(0.0),
                    _ => continue,
                },
                Stage::Min => match meta.main {
                    Length::Bounded {
                        bounds: bounds @ (length::Bounds::Min(_) | length::Bounds::Both { .. }),
                        ..
                    } => bounds,
                    Length::Fluid(length::Constraint::Min(min)) => length::Bounds::Min(min),
                    _ => continue,
                },
            };

            let max_available = (remaining - reserved_space) * fill_main_factor as f32
                / (fill_main_sum - reserved_factors) as f32;

            let max_available = if max_available.is_nan() {
                f32::INFINITY
            } else {
                max_available
            };

            let (min, max) = match bounds {
                length::Bounds::Max(max) => (0.0, max),
                length::Bounds::Both { min, max } => (min, max),
                length::Bounds::Min(min) => (min, max_available),
            };

            match stage {
                Stage::Max if max > max_available => continue,
                Stage::Min if min < max_available => continue,
                _ => {}
            }

            let min = min.min(remaining);
            let max = max.min(max_available).max(min);

            let (max_cross, infinite) = if !cross_dynamic || meta.cross.fill_factor() == 0 {
                (max_cross, infinite)
            } else {
                (cross, cross_infinite)
            };

            let (min_width, min_height) = axis.pack(min, 0.0);
            let (max_width, max_height) = axis.pack(max, max_cross);

            let child_limits = Limits::with_flags(
                Size::new(min_width, min_height),
                Size::new(max_width, max_height),
                compression,
                infinite,
            );

            let layout = content.layout(i, &child_limits)?;

            cross = cross.max(axis.cross(layout.size()));
            remaining -= axis.main(layout.size());
            fill_main_sum -= fill_main_factor;
            nodes[i] = layout;
            meta.resolved = true;
        }

        if remaining == current {
            step = match stage {
                Stage::Max if some_fill_min => Some(Stage::Min),
                _ => None,
            };
        }
    }

    if !main_compress {
        for i in 0..content.count() {
            let meta = &mut metas[i];

            if meta.resolved {
                continue;
            }

            let Category::MainFluid = meta.category else {
                continue;
            };

            let max_main = remaining * meta.main.fill_factor() as f32 / fill_main_sum as f32;

            let max_main = if max_main.is_nan() {
                f32::INFINITY
            } else {
                max_main
            };

            let min_main = if max_main.is_infinite() {
                0.0
            } else {
                max_main
            };

            let (max_cross, infinite) = if !cross_dynamic || meta.cross.fill_factor() == 0 {
                (max_cross, infinite)
            } else {
                (cross, cross_infinite)
            };

            let (min_width, min_height) = axis.pack(min_main, 0.0);
            let (max_width, max_height) = axis.pack(max_main, max_cross);

            let child_limits = Limits::with_flags(
                Size::new(min_width, min_height),
                Size::new(max_width, max_height),
                compression,
                infinite,
            );

            let layout = content.layout(i, &child_limits)?;

            cross = cross.max(axis.cross(layout.size()));
            nodes[i] = layout;
        }
    }

    if cross_dynamic && some_fill_cross {
        for i in 0..content.count() {
            let meta = metas[i];

            let Category::CrossFluidDeferred(main) = meta.category else {
                continue;
            };

            let (max_width, max_height) = axis.pack(main, cross);

            let child_limits = Limits::with_flags(
                Size::ZERO,
                Size::new(max_width, max_height),
                Size::new(false, false),
                cross_infinite,
            );

            let layout = content.layout(i, &child_limits)?;

            let size = layout.size();

            cross = cross.max(axis.cross(size));
            nodes[i] = layout;
        }
    }

    let pad = axis.pack(padding.left, padding.top);
    let mut main = pad.0;

    let cross = match axis {
        Axis::Horizontal => limits.resolve_height(height, cross),
        Axis::Vertical => limits.resolve_width(width, cross),
    };

    for (i, node) in nodes.iter_mut().enumerate() {
        if i > 0 {
            main += spacing;
        }

        node.move_to_mut(axis.pack(main, pad.1));

        match axis {
            Axis::Horizontal => {
                node.align_mut(Alignment::Start, align_items, Size::new(0.0, cross));
            }
            Axis::Vertical => {
                node.align_mut(align_items, Alignment::Start, Size::new(cross, 0.0));
            }
        }

        main += axis.main(node.size());
    }

    let main = match axis {
        Axis::Horizontal => limits.resolve_width(width, main - pad.0),
        Axis::Vertical => limits.resolve_height(height, main - pad.0),
    };

    let size = Size::from(axis.pack(main, cross));

    Ok(
        Node::with_children(size.expand(padding), nodes).with_flex_presentation(
            crate::layout::node::FlexPresentation {
                bookends: None,
                sequence: None,
                collapse_spacing: false,
                axis: match axis {
                    Axis::Horizontal => crate::renderer::presentation::Axis::Horizontal,
                    Axis::Vertical => crate::renderer::presentation::Axis::Vertical,
                },
                limits: captured_limits,
                sizing: Size::new(width, height),
                padding,
                spacing,
                align: align_items,
                children: metas
                    .iter()
                    .map(|meta| {
                        let (width, height) = axis.pack(meta.main, meta.cross);
                        Size::new(width, height)
                    })
                    .collect(),
            },
        ),
    )
}

#[cfg(test)]
mod tests;

/// Unbounded-main-axis ordering; reverse placement begins at the allocated main-axis end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceFlow {
    /// Places the first child at the leading origin.
    Forward,
    /// Places the first child against the allocated trailing edge.
    Reverse,
}

/// Measures children with an unbounded main axis and preserves their source order.
/// Spacing is in logical pixels and applies even to zero-sized children.
pub fn resolve_sequence<Message, Theme, Renderer>(
    axis: Axis,
    renderer: &Renderer,
    limits: &Limits,
    width: Length,
    height: Length,
    spacing: f32,
    flow: SequenceFlow,
    items: &mut [Element<'_, Message, Theme, Renderer>],
    trees: &mut [widget::Tree],
) -> Node
where
    Renderer: crate::Renderer,
{
    let mut widgets = Widgets {
        renderer,
        items,
        trees,
    };
    match try_resolve_sequence_content(
        axis,
        limits,
        width,
        height,
        spacing,
        flow,
        &mut InfallibleContent(&mut widgets),
    ) {
        Ok(node) => node,
        Err(error) => match error {},
    }
}

/// Shares sequential measurement with owned layout; child failures stop further queries.
pub fn try_resolve_sequence_content<C: TryContent>(
    axis: Axis,
    limits: &Limits,
    width: Length,
    height: Length,
    spacing: f32,
    flow: SequenceFlow,
    content: &mut C,
) -> Result<Node, C::Error> {
    let captured_limits = *limits;
    let limits = limits.width(width).height(height);
    let count = content.count();
    let (max_width, max_height) = axis.pack(f32::INFINITY, axis.cross(limits.max()));
    let child_limits = Limits::new(Size::ZERO, Size::new(max_width, max_height));
    let mut children = Vec::with_capacity(count);
    let mut sizing = Vec::with_capacity(count);
    let mut main = 0.0_f32;
    let mut cross = 0.0_f32;
    for index in 0..count {
        sizing.push(content.sizing(index)?);
        let child = content.layout(index, &child_limits)?;
        main += axis.main(child.size());
        cross = cross.max(axis.cross(child.size()));
        if index + 1 < count {
            main += spacing;
        }
        children.push(child);
    }
    let (intrinsic_width, intrinsic_height) = axis.pack(main, cross);
    let resolved = limits.resolve(width, height, Size::new(intrinsic_width, intrinsic_height));
    let mut cursor = if flow == SequenceFlow::Reverse {
        axis.main(resolved)
    } else {
        0.0
    };
    for child in &mut children {
        let extent = axis.main(child.size());
        if flow == SequenceFlow::Reverse {
            cursor -= extent;
        }
        let (x, y) = axis.pack(cursor, 0.0);
        child.move_to_mut(crate::Point::new(x, y));
        if flow == SequenceFlow::Reverse {
            cursor -= spacing;
        } else {
            cursor += extent;
            cursor += spacing;
        }
    }
    Ok(
        Node::with_children(resolved, children).with_flex_presentation(
            super::node::FlexPresentation {
                bookends: None,
                sequence: Some(flow),
                collapse_spacing: false,
                axis: match axis {
                    Axis::Horizontal => crate::renderer::presentation::Axis::Horizontal,
                    Axis::Vertical => crate::renderer::presentation::Axis::Vertical,
                },
                limits: captured_limits,
                sizing: Size::new(width, height),
                padding: Padding::ZERO,
                spacing,
                align: Alignment::Start,
                children: sizing,
            },
        ),
    )
}

/// Presence and reservation order of the two edges around a residual-width middle slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bookends {
    /// Charges the gap after the leading slot even if its current allocation is empty.
    pub has_leading: bool,
    /// Charges the gap before the trailing slot even if its current allocation is empty.
    pub has_trailing: bool,
    /// Reserves leading width first; false preserves trailing priority on ties.
    pub leading_first: bool,
}

/// Reserves both edges before the middle; children are leading, middle, trailing.
///
/// # Panics
/// The child inventory must contain exactly three slots, including absent edge placeholders.
pub fn resolve_bookends<Message, Theme, Renderer>(
    renderer: &Renderer,
    limits: &Limits,
    height: Length,
    spacing: f32,
    policy: Bookends,
    items: &mut [Element<'_, Message, Theme, Renderer>],
    trees: &mut [widget::Tree],
) -> Node
where
    Renderer: crate::Renderer,
{
    let mut widgets = Widgets {
        renderer,
        items,
        trees,
    };
    match try_resolve_bookends_content(
        limits,
        height,
        spacing,
        policy,
        &mut InfallibleContent(&mut widgets),
    ) {
        Ok(node) => node,
        Err(error) => match error {},
    }
}

/// Shares edge-priority measurement with owned layout; failed children stop subsequent queries.
///
/// # Panics
/// Requires exactly three indexed children in leading, middle, trailing order.
pub fn try_resolve_bookends_content<C: TryContent>(
    limits: &Limits,
    height: Length,
    spacing: f32,
    policy: Bookends,
    content: &mut C,
) -> Result<Node, C::Error> {
    assert_eq!(content.count(), 3, "bookend inventory");
    let captured = *limits;
    let limits = limits.width(Length::Fill).height(height);
    let max = limits.max();
    let gaps = usize::from(policy.has_leading) + usize::from(policy.has_trailing);
    let total_gap = spacing * gaps as f32;
    let sizing = (0..3)
        .map(|index| content.sizing(index))
        .collect::<Result<Vec<_>, _>>()?;
    let first = if policy.leading_first { 0 } else { 2 };
    let second = 2 - first;
    let first_limits = Limits::new(
        Size::ZERO,
        Size::new((max.width - total_gap).max(0.0), max.height),
    );
    let first_node = content.layout(first, &first_limits)?;
    let second_limits = Limits::new(
        Size::ZERO,
        Size::new(
            (max.width - first_node.size().width - total_gap).max(0.0),
            max.height,
        ),
    );
    let second_node = content.layout(second, &second_limits)?;
    let (leading, trailing) = if policy.leading_first {
        (first_node, second_node)
    } else {
        (second_node, first_node)
    };
    let lw = leading.size().width;
    let tw = trailing.size().width;
    let middle_limits = Limits::new(
        Size::ZERO,
        Size::new((max.width - lw - tw - total_gap).max(0.0), max.height),
    );
    let middle = content.layout(1, &middle_limits)?;
    let natural_height = leading
        .size()
        .height
        .max(middle.size().height)
        .max(trailing.size().height);
    let size = limits.resolve(Length::Fill, height, Size::new(max.width, natural_height));
    let position = |node: Node, x: f32| {
        let height = node.size().height;
        node.move_to(crate::Point::new(x, 0.0))
            .translate(crate::Vector::new(
                0.0,
                ((size.height - height) / 2.0).max(0.0),
            ))
    };
    Ok(Node::with_children(
        size,
        vec![
            position(leading, 0.0),
            position(
                middle,
                if policy.has_leading {
                    lw + spacing
                } else {
                    0.0
                },
            ),
            position(trailing, (max.width - tw).max(0.0)),
        ],
    )
    .with_flex_presentation(super::node::FlexPresentation {
        bookends: Some(policy),
        sequence: None,
        collapse_spacing: false,
        axis: crate::renderer::presentation::Axis::Horizontal,
        limits: captured,
        sizing: Size::new(Length::Fill, height),
        padding: Padding::ZERO,
        spacing,
        align: Alignment::Start,
        children: sizing,
    }))
}

#[cfg(test)]
mod bookend_height_tests {
    use super::*;

    struct Children;
    impl TryContent for Children {
        type Error = std::convert::Infallible;
        fn count(&self) -> usize {
            3
        }
        fn sizing(&mut self, _: usize) -> Result<Size<Length>, Self::Error> {
            Ok(Size::new(Length::Shrink, Length::Shrink))
        }
        fn layout(&mut self, index: usize, limits: &Limits) -> Result<Node, Self::Error> {
            let natural = [
                Size::new(40.0, 20.0),
                Size::new(80.0, 36.0),
                Size::new(30.0, 12.0),
            ][index];
            Ok(Node::new(limits.resolve(
                Length::Shrink,
                Length::Shrink,
                natural,
            )))
        }
    }

    #[test]
    fn bookends_resolve_natural_height_in_unbounded_and_bounded_hosts() {
        for (height, max_height, expected) in [
            (Length::Shrink, f32::INFINITY, 36.0),
            (Length::Shrink, 100.0, 36.0),
            (Length::Fill, 100.0, 100.0),
            (Length::Fixed(60.0), f32::INFINITY, 60.0),
        ] {
            let node = try_resolve_bookends_content(
                &Limits::new(Size::ZERO, Size::new(300.0, max_height)),
                height,
                8.0,
                Bookends {
                    has_leading: true,
                    has_trailing: true,
                    leading_first: true,
                },
                &mut Children,
            )
            .unwrap();
            assert_eq!(node.size(), Size::new(300.0, expected));
            for child in node.children() {
                assert_eq!(child.bounds().y, (expected - child.size().height) / 2.0);
                assert!(child.bounds().x.is_finite());
            }
            assert_eq!(node.children()[1].bounds().x, 48.0);
            assert_eq!(node.children()[2].bounds().x, 270.0);
        }
    }
}
