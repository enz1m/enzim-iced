// Modified by enzUI contributors. See FORK.md.
//! Position your widgets properly.
mod limits;
mod node;
mod presentation;

pub mod action_strip;
pub mod clip;
pub mod container;
pub mod flex;
pub mod flow;
pub mod input;
pub mod inset;
pub mod item_strip;
pub mod media;
pub mod overlay;
pub mod pair;
pub mod priority_band;
pub mod responsive;
pub mod reveal;
pub mod rule;
pub mod scroll;
pub mod tab_strip;

pub use limits::Limits;
pub use node::{Measure, Node, Placed, pass_through};

use crate::widget;
use crate::{Length, Padding, Point, Rectangle, Size, Vector};

/// The bounds of a widget and its children, using absolute coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    position: Point,
    size: Size,
    parent: Option<Rectangle>,
}

impl Layout {
    /// Creates a new [`Layout`] for the given [`Size`] at the origin.
    pub fn new(size: Size) -> Self {
        Self {
            position: Point::ORIGIN,
            size,
            parent: None,
        }
    }

    /// Creates a [`Layout`] at the position and with the size of `node`,
    /// e.g. one returned by [`Measure::measure`] (enzICED).
    pub fn of(node: &Node) -> Self {
        Self::new(node.size()).move_to(node.position())
    }

    /// Returns the position of the [`Layout`].
    pub fn position(&self) -> Point {
        self.position
    }

    /// Moves the [`Layout`] to the given position.
    pub fn move_to(self, position: impl Into<Point>) -> Self {
        Self {
            position: position.into(),
            size: self.size,
            parent: self.parent,
        }
    }

    /// Returns the bounds of the [`Layout`].
    ///
    /// The returned [`Rectangle`] describes the position and size of the
    /// laid out widget.
    pub fn bounds(&self) -> Rectangle {
        Rectangle::new(self.position, self.size)
    }

    /// Returns the size of the [`Layout`].
    pub fn size(&self) -> Size {
        self.size
    }

    /// Returns the bounds of the parent of this [`Layout`], if any.
    pub fn parent(&self) -> Option<Rectangle> {
        self.parent
    }

    /// Returns an iterator over the children of this [`Layout`].
    pub fn iter(
        self,
        children: &[widget::Tree],
    ) -> impl DoubleEndedIterator<Item = (Self, &widget::Tree)> + ExactSizeIterator {
        children
            .iter()
            .map(move |child| (self.child_of(child), child))
    }

    /// Returns a mutable iterator over the children of this [`Layout`].
    pub fn iter_mut(
        self,
        children: &mut [widget::Tree],
    ) -> impl DoubleEndedIterator<Item = (Self, &mut widget::Tree)> + ExactSizeIterator {
        children
            .iter_mut()
            .map(move |child| (self.child_of(child), child))
    }

    fn child_of(&self, child: &widget::Tree) -> Self {
        Self {
            position: self.position + child.translation,
            size: child.size,
            parent: Some(self.bounds()),
        }
    }

    /// Returns the layouts of the given children, without their trees (enzICED).
    ///
    /// The iterator is a plain borrow of `children`, so it ends where it is
    /// last used: `layout.children(&tree.children).next()` can precede a
    /// `&mut tree.children[0]` in the same statement.
    pub fn children(self, children: &[widget::Tree]) -> Children<'_> {
        Children {
            parent: self,
            children: children.iter(),
        }
    }

    /// Returns the [`Layout`] of the child at the given index (enzICED).
    ///
    /// This can be useful if you ever need to access children out of order
    /// for layering purposes.
    ///
    /// # Panics
    /// Panics if index is out of bounds.
    pub fn child(self, children: &[widget::Tree], index: usize) -> Self {
        self.child_of(&children[index])
    }

    /// The synthetic parts a leaf widget laid out below itself (an input's
    /// text and caret), placed at this layout (enzICED).
    ///
    /// A widget that declared nothing has no parts.
    pub fn synthetic(self, tree: &widget::Tree) -> Placed<'_> {
        Placed::new(self.position, tree.presentation().unwrap_or(&node::EMPTY))
    }

    /// Draws a native viewport-culled subtree, retaining its material behind a typed capture mask.
    /// Visible material keeps its inherited physical clip rather than being cropped to its allocation.
    pub fn draw_culled<R: crate::Renderer>(
        self,
        tree: &widget::Tree,
        renderer: &mut R,
        viewport: &Rectangle,
        draw: impl FnOnce(&mut R, &Rectangle),
    ) {
        let visible = self.bounds().intersection(viewport);
        let Some((scope, clip)) = tree.presentation().and_then(|node| node.drawing_cull) else {
            if visible.is_some() {
                draw(renderer, viewport);
            }
            return;
        };
        if let Some(capture) = renderer.layout_culling_capture(scope, *viewport) {
            if renderer.layout_culling_active(scope) {
                draw(renderer, &capture.viewport);
            } else {
                renderer.with_layout_layer(
                    crate::renderer::LayoutLayer::Cull(scope),
                    capture.mask,
                    None,
                    |renderer| draw(renderer, &capture.viewport),
                );
            }
        } else if let Some(visible) = visible {
            draw(renderer, if clip { &visible } else { viewport });
        }
    }

    /// Projects a source-matching presented sample onto this layout's recorded hierarchy.
    ///
    /// The returned root is in absolute sample coordinates. Callers must validate the
    /// frame/source lease before using the result for input or accessibility.
    ///
    /// # Errors
    /// Refuses a different source hierarchy or an allocation projection beyond the program budget.
    pub fn project_presented(
        self,
        tree: &widget::Tree,
        sample: &crate::renderer::presentation::Sample,
    ) -> Result<Node, crate::renderer::presentation::Error> {
        sample.project_layout(&Node::snapshot(tree))
    }

    /// Lowers built-in text, flex, and aligned-container allocations into an owned width-responsive program.
    ///
    /// The existing track channel supplies the current maximum width. Unsupported descendants
    /// refuse the whole program; container rules are not inferred from photographed geometry.
    pub fn presentation_program(
        self,
        tree: &widget::Tree,
        driver: crate::renderer::NodeId,
        channel: u8,
        clip: Rectangle,
    ) -> Result<
        std::sync::Arc<crate::renderer::presentation::Program>,
        crate::renderer::presentation::Error,
    > {
        let node = Node::snapshot(tree);
        presentation::lower(Placed::new(self.position, &node), driver, channel, clip)
    }

    /// Lowers the complete supplied layout with scalar docked progress, not a root pixel width.
    ///
    /// Opaque descendants fail the entire lowering; none are inferred to be width-independent.
    pub fn docked_presentation(
        self,
        tree: &widget::Tree,
        driver: crate::renderer::NodeId,
        clip: Rectangle,
    ) -> Result<
        std::sync::Arc<crate::renderer::presentation::Program>,
        crate::renderer::presentation::Error,
    > {
        self.driven_presentation(tree, driver, clip)
    }

    /// Lowers declared driver-bound geometry, including non-sidebar content size ranges.
    ///
    /// # Errors
    /// Refuses opaque descendants, missing driven geometry, and invalid or over-budget declarations.
    pub fn driven_presentation(
        self,
        tree: &widget::Tree,
        driver: crate::renderer::NodeId,
        clip: Rectangle,
    ) -> Result<
        std::sync::Arc<crate::renderer::presentation::Program>,
        crate::renderer::presentation::Error,
    > {
        let node = Node::snapshot(tree);
        presentation::lower_docked(Placed::new(self.position, &node), driver, clip)
    }

    /// Native editor text viewport in window coordinates, excluding padding and its integrated gutter.
    ///
    /// Returns `None` without an editor contract. Undersized allocations retain
    /// the native inset rectangle's negative dimensions.
    pub fn editor_content_bounds(self, tree: &widget::Tree) -> Option<Rectangle> {
        let (_, editor) = tree.presentation()?.editor_presentation()?;
        Some(self.bounds().shrink(editor.padding))
    }

    /// Draws a quad belonging to this container allocation, preserving ordinary paint on refusal.
    pub fn fill_presentation_quad(
        self,
        tree: &widget::Tree,
        renderer: &mut impl crate::Renderer,
        quad: crate::renderer::Quad,
        background: crate::Background,
    ) {
        if let Some(node) = tree.container_presentation_node() {
            renderer.fill_layout_quad(node, quad, background);
        } else {
            renderer.fill_quad(quad, background);
        }
    }

    /// Binds rule paint to this allocation, including fill mode and per-frame hint scaling.
    pub fn fill_presentation_rule(
        self,
        tree: &widget::Tree,
        renderer: &mut impl crate::Renderer,
        quad: crate::renderer::Quad,
        background: crate::Background,
        paint: rule::Paint,
    ) {
        if let Some(node) = tree.container_presentation_node() {
            renderer.fill_layout_rule(node, self.bounds(), quad, background, paint);
        } else {
            renderer.fill_quad(quad, background);
        }
    }

    /// Refuses responsive capture of this container when its paint cannot follow the owned allocation.
    ///
    /// Other layout identities and ordinary drawing are unaffected.
    pub fn reject_presentation_paint(
        self,
        tree: &widget::Tree,
        renderer: &mut impl crate::Renderer,
    ) {
        if let Some(node) = tree.container_presentation_node() {
            renderer.reject_layout_paint(node);
        }
    }
}

/// The layouts of some children of a [`Layout`]; see [`Layout::children`].
#[derive(Debug, Clone)]
pub struct Children<'a> {
    parent: Layout,
    children: std::slice::Iter<'a, widget::Tree>,
}

impl Iterator for Children<'_> {
    type Item = Layout;

    fn next(&mut self) -> Option<Layout> {
        self.children
            .next()
            .map(|child| self.parent.child_of(child))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.children.size_hint()
    }

    fn nth(&mut self, n: usize) -> Option<Layout> {
        self.children
            .nth(n)
            .map(|child| self.parent.child_of(child))
    }
}

impl DoubleEndedIterator for Children<'_> {
    fn next_back(&mut self) -> Option<Layout> {
        self.children
            .next_back()
            .map(|child| self.parent.child_of(child))
    }
}

impl ExactSizeIterator for Children<'_> {}

/// Lays out two children side by side in the provided [`widget::Tree`].
///
/// The tree must have exactly two children. The first is placed at the left
/// edge, and the second to its right after `spacing`, both vertically
/// centered with respect to each other.
///
/// The resulting size is stored in the tree, along with the children's
/// sizes and translations.
pub fn next_to_each_other(tree: &mut widget::Tree, left: Size, right: Size, spacing: f32) {
    let (left_y, right_y) = if left.height > right.height {
        (0.0, (left.height - right.height) / 2.0)
    } else {
        ((right.height - left.height) / 2.0, 0.0)
    };

    tree.size = Size::new(
        left.width + spacing + right.width,
        left.height.max(right.height),
    );

    tree.children[0].size = left;
    tree.children[0].translation = Vector::new(0.0, left_y);

    tree.children[1].size = right;
    tree.children[1].translation = Vector::new(left.width + spacing, right_y);
}

/// Computes the resulting [`Size`] that fits the [`Limits`] given
/// some width and height requirements and no intrinsic size.
pub fn atomic(limits: &Limits, width: impl Into<Length>, height: impl Into<Length>) -> Size {
    let width = width.into();
    let height = height.into();

    limits
        .width(width)
        .height(height)
        .resolve(width, height, Size::ZERO)
}

/// Computes the resulting [`Size`] that fits the [`Limits`] given
/// some width and height requirements and a closure that produces
/// the intrinsic [`Size`] inside the given [`Limits`].
pub fn sized(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    f: impl FnOnce(&Limits) -> Size,
) -> Size {
    let width = width.into();
    let height = height.into();

    let limits = limits.width(width).height(height);
    let intrinsic_size = f(&limits);

    limits.resolve(width, height, intrinsic_size)
}

/// Computes the resulting [`Size`] that fits the [`Limits`] given
/// some width and height requirements and a closure that produces
/// the content inside the given [`Limits`].
pub fn contained(
    tree: &mut widget::Tree,
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    f: impl FnOnce(&mut widget::Tree, &Limits) -> Size,
) {
    let width = width.into();
    let height = height.into();
    let limits = limits.width(width).height(height);

    let child = &mut tree.children[0];
    let content = f(child, &limits);

    tree.size = limits.resolve(width, height, content);
    child.translation = Vector::ZERO;
}

/// Computes the [`Size`] that fits the [`Limits`] given some width, height, and
/// [`Padding`] requirements and a closure that produces the content
/// inside the given [`Limits`].
pub fn padded(
    tree: &mut widget::Tree,
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    padding: impl Into<Padding>,
    layout: impl FnOnce(&mut widget::Tree, &Limits) -> Size,
) {
    positioned(tree, limits, width, height, padding, layout, |_, _| {
        Vector::ZERO
    });
}

/// Computes a [`padded`] [`Size`] with a positioning step.
pub fn positioned(
    tree: &mut widget::Tree,
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    padding: impl Into<Padding>,
    layout: impl FnOnce(&mut widget::Tree, &Limits) -> Size,
    translate: impl FnOnce(Size, Size) -> Vector,
) {
    let width = width.into();
    let height = height.into();
    let padding = padding.into();
    let limits = limits.width(width).height(height);

    let child = &mut tree.children[0];
    let content = layout(child, &limits.shrink(padding));
    let padding = padding.fit(content, limits.bounds());

    let container = limits.shrink(padding).resolve(width, height, content);

    tree.size = container.expand(padding);
    child.translation = Vector::new(padding.left, padding.top) + translate(content, container);
}

/// Computes the resulting [`Node`] that fits the [`Limits`] given
/// some width and height requirements and no intrinsic size.
pub fn atomic_node(limits: &Limits, width: impl Into<Length>, height: impl Into<Length>) -> Node {
    let width = width.into();
    let height = height.into();

    Node::new(
        limits
            .width(width)
            .height(height)
            .resolve(width, height, Size::ZERO),
    )
}

/// Resolves a childless paint allocation; responsive capture requires explicit layout-bound paint.
pub fn paint_box(limits: &Limits, width: Length, height: Length) -> Node {
    atomic_declaration(limits, width, height, container::Mode::Atomic)
}

/// Reserves unmaterialized content; owned replay refuses if any of it becomes visible.
pub fn unrealized(limits: &Limits, width: Length, height: Length) -> Node {
    atomic_declaration(limits, width, height, container::Mode::Unrealized)
}

/// Declares an unmounted subtree with no material, always zero-sized under inherited limits.
pub fn void() -> Node {
    atomic_declaration(
        &Limits::NONE,
        Length::Fixed(0.0),
        Length::Fixed(0.0),
        container::Mode::Void,
    )
}

fn atomic_declaration(
    limits: &Limits,
    width: Length,
    height: Length,
    mode: container::Mode,
) -> Node {
    atomic_node(limits, width, height).with_container_presentation(
        crate::renderer::presentation::Container {
            reflow_min_width: None,
            clip_content: false,
            inset: None,
            mode,
            recorded: None,
            width: None,
            limits: *limits,
            sizing: Size::new(width, height),
            padding: Padding::ZERO,
            alignment: [crate::Alignment::Start; 2],
            child: Size::new(Length::Shrink, Length::Shrink),
        },
    )
}

/// Declares an empty spacer with native length resolution and no retained paint.
pub fn empty(limits: &Limits, width: Length, height: Length) -> Node {
    atomic_node(limits, width, height).with_flex_presentation(node::FlexPresentation {
        bookends: None,
        sequence: None,
        collapse_spacing: false,
        axis: crate::renderer::presentation::Axis::Horizontal,
        limits: *limits,
        sizing: Size::new(width, height),
        padding: Padding::ZERO,
        spacing: 0.0,
        align: crate::Alignment::Start,
        children: Vec::new(),
    })
}

/// Computes the resulting [`Node`] that fits the [`Limits`] given
/// some width and height requirements and a closure that produces
/// the intrinsic [`Size`] inside the given [`Limits`].
pub fn sized_node(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    f: impl FnOnce(&Limits) -> Size,
) -> Node {
    let width = width.into();
    let height = height.into();

    let limits = limits.width(width).height(height);
    let intrinsic_size = f(&limits);

    Node::new(limits.resolve(width, height, intrinsic_size))
}

/// Computes the resulting [`Node`] that fits the [`Limits`] given
/// some width and height requirements and a closure that produces
/// the content [`Node`] inside the given [`Limits`].
pub fn contained_node(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    f: impl FnOnce(&Limits) -> Node,
) -> Node {
    let width = width.into();
    let height = height.into();

    let limits = limits.width(width).height(height);
    let content = f(&limits);

    Node::with_children(limits.resolve(width, height, content.size()), vec![content])
}

/// Resolves padded child constraints without loosening their minimums.
///
/// Typed children also declare owned allocation; paint is checked separately,
/// and opaque child layouts remain unsupported.
pub fn padded_node(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    padding: impl Into<Padding>,
    layout: impl FnOnce(&Limits) -> Node,
) -> Node {
    padded_aligned(
        limits,
        width.into(),
        height.into(),
        padding.into(),
        [crate::Alignment::Start; 2],
        layout,
    )
}

/// Computes a [`padded_node`] [`Node`] with a positioning step.
pub fn positioned_node(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    padding: impl Into<Padding>,
    layout: impl FnOnce(&Limits) -> Node,
    position: impl FnOnce(Node, Size) -> Node,
) -> Node {
    match try_positioned(
        limits,
        width,
        height,
        padding,
        |limits| Ok::<_, std::convert::Infallible>(layout(limits)),
        position,
    ) {
        Ok(node) => node,
        Err(error) => match error {},
    }
}

/// Resolves padded geometry, propagating intrinsic-measurement failure before positioning.
///
/// Completed resource side effects belong to the caller and are not rolled back on failure.
pub fn try_positioned<E>(
    limits: &Limits,
    width: impl Into<Length>,
    height: impl Into<Length>,
    padding: impl Into<Padding>,
    layout: impl FnOnce(&Limits) -> Result<Node, E>,
    position: impl FnOnce(Node, Size) -> Node,
) -> Result<Node, E> {
    let width = width.into();
    let height = height.into();
    let padding = padding.into();

    let limits = limits.width(width).height(height);
    let content = layout(&limits.shrink(padding))?;
    let padding = padding.fit(content.size(), limits.bounds());

    let size = limits
        .shrink(padding)
        .resolve(width, height, content.size());

    Ok(Node::with_children(
        size.expand(padding),
        vec![position(content.move_to((padding.left, padding.top)), size)],
    ))
}

/// Aligns one child measured under loose padded limits, retaining owned layout constraints.
///
/// Unsupported child metadata remains unsupported; paint is independently admitted by the renderer.
pub fn aligned(
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    alignment: [crate::Alignment; 2],
    layout: impl FnOnce(&Limits) -> Node,
) -> Node {
    let node = positioned_node(
        limits,
        width,
        height,
        padding,
        |limits| layout(&limits.loose()),
        |content, size| content.align(alignment[0], alignment[1], size),
    );
    let Some(child) = node.children()[0].presentation_sizing() else {
        return node;
    };
    node.with_container_presentation(crate::renderer::presentation::Container {
        reflow_min_width: None,
        clip_content: false,
        inset: None,
        mode: container::Mode::Aligned,
        recorded: None,
        width: None,
        limits: *limits,
        sizing: Size::new(width, height),
        padding,
        alignment,
        child,
    })
}

/// Aligns a padded child without loosening its minimum constraints.
///
/// Declares the same allocation for owned replay; untyped child layout remains unsupported.
pub fn padded_aligned(
    limits: &Limits,
    width: Length,
    height: Length,
    padding: Padding,
    alignment: [crate::Alignment; 2],
    layout: impl FnOnce(&Limits) -> Node,
) -> Node {
    let node = positioned_node(limits, width, height, padding, layout, |content, size| {
        content.align(alignment[0], alignment[1], size)
    });
    let Some(child) = node.children()[0].presentation_sizing() else {
        return node;
    };
    node.with_container_presentation(crate::renderer::presentation::Container {
        reflow_min_width: None,
        clip_content: false,
        inset: None,
        mode: container::Mode::Padded,
        recorded: None,
        width: None,
        limits: *limits,
        sizing: Size::new(width, height),
        padding,
        alignment,
        child,
    })
}

/// Resolves a fill-width field, then fits padding inside the resolved height.
///
/// Non-fixed heights probe the child before its final allocation. The callback
/// must support repeated measurement without consuming its content.
pub fn field(
    limits: &Limits,
    height: Length,
    padding: Padding,
    layout: impl FnMut(&Limits) -> Node,
) -> Node {
    owned_container(
        limits,
        Size::new(Length::Fill, height),
        padding,
        container::Mode::Field,
        layout,
    )
}

/// Preserves child limits and geometry while declaring a separate retained wrapper.
///
/// The wrapper's paint must use owned quad hooks or explicitly refuse capture.
pub fn passthrough(
    limits: &Limits,
    sizing: Size<Length>,
    layout: impl FnMut(&Limits) -> Node,
) -> Node {
    owned_container(
        limits,
        sizing,
        Padding::ZERO,
        container::Mode::Passthrough,
        layout,
    )
}

/// Reserves a later sibling's width before measuring a child, without changing its sizing intent.
/// The cap is `max(available - reserve, minimum).min(ceiling)` in logical pixels.
pub fn reserved_width(
    limits: &Limits,
    sizing: Size<Length>,
    reserve: f32,
    minimum: f32,
    ceiling: f32,
    layout: impl FnMut(&Limits) -> Node,
) -> Node {
    owned_container(
        limits,
        sizing,
        Padding::ZERO,
        container::Mode::WidthCap {
            reserve,
            minimum,
            ceiling,
        },
        layout,
    )
}

/// Resolves a frame's sizing while preserving its child's native origin and limits.
pub fn frame(limits: &Limits, sizing: Size<Length>, layout: impl FnMut(&Limits) -> Node) -> Node {
    owned_container(
        limits,
        sizing,
        Padding::ZERO,
        container::Mode::Frame,
        layout,
    )
}

/// Applies a logical-pixel height floor without preventing growth above it.
///
/// Start alignment raises the child's minimum height; center and end align its
/// natural height within the floor. The parent's width minimum is preserved.
pub fn minimum_height(
    limits: &Limits,
    width: Length,
    minimum: f32,
    alignment: crate::Alignment,
    layout: impl FnMut(&Limits) -> Node,
) -> Node {
    owned_container(
        limits,
        Size::new(width, Length::Shrink),
        Padding::ZERO,
        container::Mode::HeightFloor { minimum, alignment },
        layout,
    )
}

fn owned_container(
    limits: &Limits,
    sizing: Size<Length>,
    padding: Padding,
    mode: container::Mode,
    mut layout: impl FnMut(&Limits) -> Node,
) -> Node {
    let alignment = [crate::Alignment::Start; 2];
    let node = match container::try_resolve(limits, sizing, padding, alignment, mode, |limits| {
        Ok::<_, std::convert::Infallible>(layout(limits))
    }) {
        Ok(node) => node,
        Err(error) => match error {},
    };
    let Some(child) = node.children()[0].presentation_sizing() else {
        return node;
    };
    node.with_container_presentation(crate::renderer::presentation::Container {
        reflow_min_width: None,
        clip_content: false,
        inset: None,
        mode,
        recorded: None,
        width: None,
        limits: *limits,
        sizing,
        padding,
        alignment,
        child,
    })
}

/// Resolves editor viewport sizing after native or owned intrinsic measurement.
pub fn editor_size(limits: &Limits, height: Length, padding: Padding, minimum: Size) -> Size {
    match height {
        Length::Shrink | Length::Fit => limits
            .height(minimum.height)
            .max()
            .expand(Size::new(0.0, padding.y())),
        _ => limits.max(),
    }
}

/// Wraps cached factory content without allowing replay to change its construction constraints.
/// Unknown descendant layout and paint still refuse capture independently.
pub fn cached_factory(limits: &Limits, sizing: Size<Length>, child: Node) -> Node {
    let available = limits.width(sizing.width).height(sizing.height);
    let child_sizing = child
        .presentation_sizing()
        .unwrap_or(Size::new(Length::Shrink, Length::Shrink));
    Node::with_children(
        available.resolve(sizing.width, sizing.height, child.size()),
        vec![child],
    )
    .with_container_presentation(crate::renderer::presentation::Container {
        reflow_min_width: None,
        inset: None,
        clip_content: false,
        mode: container::Mode::CachedFactory {
            available: available.max(),
        },
        recorded: None,
        width: None,
        limits: *limits,
        sizing,
        padding: Padding::ZERO,
        alignment: [crate::Alignment::Start; 2],
        child: child_sizing,
    })
}
