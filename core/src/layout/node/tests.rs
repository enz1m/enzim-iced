// Modified by enzUI contributors. See FORK.md.
//! The bridge between tree-owned layout and the fork's owned nodes
//! (enzICED).
use super::*;
use crate::layout::{self, Layout, Limits};
use crate::{Length, Size};

fn tree(children: usize) -> Tree {
    let mut tree = Tree::empty();
    tree.children = (0..children).map(|_| Tree::empty()).collect();
    tree
}

fn spacer(width: f32, height: f32) -> Node {
    layout::empty(&Limits::NONE, Length::Fixed(width), Length::Fixed(height))
}

/// Sabotage: skip the per-child `apply` and the second child keeps its
/// origin.
#[test]
fn applying_a_node_places_the_trees_children() {
    let mut tree = tree(2);
    let node = Node::with_children(
        Size::new(50.0, 20.0),
        vec![
            Node::new(Size::new(10.0, 20.0)),
            Node::new(Size::new(30.0, 20.0)).move_to(Point::new(15.0, 0.0)),
        ],
    );

    node.apply(&mut tree);

    assert_eq!(tree.size, Size::new(50.0, 20.0));
    assert_eq!(tree.children[1].translation, Vector::new(15.0, 0.0));
    assert_eq!(tree.children[1].size, Size::new(30.0, 20.0));
    assert_eq!(
        Layout::new(tree.size).child(&tree.children, 1).bounds(),
        Rectangle::new(Point::new(15.0, 0.0), Size::new(30.0, 20.0))
    );
}

/// A node measured from a laid out tree carries that tree's declarations,
/// and applying it back keeps them.
///
/// Sabotage: store `None` in `apply` and the identity is lost.
#[test]
fn a_measured_node_round_trips_its_declarations() {
    let mut tree = tree(0);
    spacer(10.0, 10.0).apply(&mut tree);
    let id = tree.presentation_node();

    assert!(id.is_some());
    assert_eq!(Node::of(&tree).presentation_node(), id);

    Node::of(&tree)
        .move_to(Point::new(4.0, 0.0))
        .apply(&mut tree);
    assert_eq!(tree.presentation_node(), id);
}

/// A leaf widget's synthetic parts have no trees of their own; they stay
/// with its declarations.
///
/// Sabotage: drop the node's children for a childless tree in `apply` and
/// the parts disappear.
#[test]
fn a_leaf_keeps_its_synthetic_parts() {
    let mut tree = tree(0);
    Node::with_children(
        Size::new(40.0, 10.0),
        vec![spacer(10.0, 10.0).move_to(Point::new(25.0, 0.0))],
    )
    .apply(&mut tree);

    let parts = Layout::new(tree.size)
        .move_to(Point::new(100.0, 0.0))
        .synthetic(&tree);

    assert_eq!(parts.child(0).position(), Point::new(125.0, 0.0));
    assert_eq!(Node::snapshot(&tree).children().len(), 1);
}

/// An undeclared wrapper that hands its only child its whole allocation is
/// transparent to owned presentation, as it was when it returned the
/// child's node as its own.
///
/// Sabotage: drop the `transparent_child` branch in lowering and the
/// wrapped program is refused.
#[test]
fn lowering_looks_through_an_undeclared_wrapper() {
    let clip = Rectangle::with_size(Size::new(100.0, 100.0));
    let driver = crate::renderer::NodeId(900);
    let wrapped = Node::with_children(Size::new(10.0, 10.0), vec![spacer(10.0, 10.0)]);
    let offset = Node::with_children(
        Size::new(10.0, 10.0),
        vec![spacer(10.0, 5.0).move_to(Point::new(0.0, 5.0))],
    );

    assert!(
        Placed::of(&wrapped)
            .presentation_program(driver, 0, clip)
            .is_ok()
    );
    assert!(
        Placed::of(&offset)
            .presentation_program(driver, 0, clip)
            .is_err(),
        "a wrapper that moves its child is still opaque"
    );
}

/// Sabotage: stop descending in `Tree::presentation_node` and the wrapped
/// identity is `None`.
#[test]
fn a_trees_presentation_identity_looks_through_wrappers() {
    let mut wrapper = tree(1);
    let child = spacer(10.0, 10.0);
    Node::with_children(child.size(), vec![child]).apply(&mut wrapper);

    assert!(wrapper.presentation().is_none());
    assert!(wrapper.presentation_node().is_some());
    assert_eq!(
        wrapper.presentation_node(),
        wrapper.children[0].presentation_node()
    );
}

/// A parent that places an earlier probe of a child whose tree a later
/// layout replaced pairs stale geometry with fresh descendants.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "no longer holds")]
fn placing_a_stale_probe_is_a_layout_defect() {
    let mut parent = tree(1);
    parent.children[0].children.push(Tree::empty());
    parent.children[0].size = Size::new(20.0, 20.0);

    let probe = Node::new(Size::new(10.0, 10.0));
    Node::with_children(Size::new(20.0, 20.0), vec![probe]).apply(&mut parent);
}

/// A node that lists its tree's children in reverse places each child in
/// its own tree and snapshots back in its own order.
///
/// Sabotage: pair children in order in `apply` and the first tree takes the
/// last slot's geometry.
#[test]
fn reversed_children_pair_with_their_own_trees() {
    let mut tree = tree(2);
    Node::with_children(
        Size::new(30.0, 10.0),
        vec![
            Node::new(Size::new(20.0, 10.0)),
            Node::new(Size::new(10.0, 10.0)).move_to(Point::new(20.0, 0.0)),
        ],
    )
    .with_reversed_children(true)
    .apply(&mut tree);

    assert_eq!(tree.children[1].size, Size::new(20.0, 10.0));
    assert_eq!(tree.children[0].translation, Vector::new(20.0, 0.0));
    assert_eq!(
        Node::snapshot(&tree).children()[0].size(),
        Size::new(20.0, 10.0)
    );
}

/// A synthetic slot between a strip and its child keeps its own geometry
/// and declarations on the child's tree, and snapshots rebuild the level.
///
/// Sabotage: pair the slot itself with the child's tree in `place` and the
/// child's descendants are mispaired (the debug check panics).
#[test]
fn an_interposed_slot_rides_on_its_childs_tree() {
    let mut strip = tree(1);
    strip.children[0].children.push(Tree::empty());

    let content = Node::with_children(Size::new(10.0, 10.0), vec![Node::new(Size::new(4.0, 4.0))])
        .move_to(Point::new(1.0, 2.0));
    let slot = Node::with_children(Size::ZERO, vec![content])
        .with_drawing_cull(false)
        .move_to(Point::new(20.0, 0.0))
        .interposed();
    Node::with_children(Size::new(40.0, 10.0), vec![slot]).apply(&mut strip);

    let child = &strip.children[0];
    assert_eq!(child.translation, Vector::new(21.0, 2.0));
    assert_eq!(child.size, Size::new(10.0, 10.0));
    assert!(child.interposed.is_some());

    let snapshot = Node::snapshot(&strip);
    let slot = &snapshot.children()[0];
    assert_eq!(slot.position(), Point::new(20.0, 0.0));
    assert_eq!(slot.size(), Size::ZERO);
    assert_eq!(slot.children()[0].position(), Point::new(1.0, 2.0));

    // A projection of the snapshot applies back through the same slot.
    snapshot.apply(&mut strip);
    assert_eq!(strip.children[0].translation, Vector::new(21.0, 2.0));
}

/// A node that places only its tree's first children (a measuring twin
/// stays unplaced) pairs them and snapshots only what it placed.
///
/// Sabotage: ignore `placed_prefix` in `snapshot` and the twin appears.
#[test]
fn a_placed_prefix_leaves_the_rest_unplaced() {
    let mut tree = tree(2);
    tree.children[1].size = Size::new(99.0, 99.0);
    Node::with_children(
        Size::new(10.0, 10.0),
        vec![Node::new(Size::new(10.0, 10.0))],
    )
    .with_placed_prefix()
    .apply(&mut tree);

    assert_eq!(tree.children[1].size, Size::new(99.0, 99.0));
    assert_eq!(Node::snapshot(&tree).children().len(), 1);
}

/// A bounded `Length::min` below an incoming minimum keeps the incoming
/// one, so a floor (`min_height`) holds over a child with its own smaller
/// minimum. Upstream #579ed4d37 replaces the incoming minimum.
///
/// Sabotage: drop the `.max(self.min.height)` in `Limits::height` and the
/// floor is lost.
#[test]
fn a_bounded_minimum_keeps_an_incoming_floor() {
    let floor = Limits::new(Size::new(0.0, 60.0), Size::new(100.0, 100.0));
    let limits = floor.height(Length::Shrink.min(24.0));

    assert_eq!(limits.min().height, 60.0);
}

/// A wrapper that lays out as one child while holding another tree (a
/// popup) places only that child, and stays transparent.
///
/// Sabotage: leave `presentation` unset in `pass_through` and the popup
/// joins the snapshot.
#[test]
fn a_pass_through_leaves_its_popup_unplaced() {
    let mut wrapper = tree(2);
    spacer(10.0, 10.0).apply(&mut wrapper.children[0]);
    wrapper.children[1].size = Size::new(200.0, 300.0);

    layout::pass_through(&mut wrapper, 0);

    assert_eq!(wrapper.size, Size::new(10.0, 10.0));
    assert_eq!(Node::snapshot(&wrapper).children().len(), 1);
    assert_eq!(
        wrapper.presentation_node(),
        wrapper.children[0].presentation_node()
    );
    assert!(
        Placed::of(&Node::snapshot(&wrapper))
            .presentation_program(
                crate::renderer::NodeId(900),
                0,
                Rectangle::with_size(Size::new(100.0, 100.0)),
            )
            .is_ok()
    );
}

/// A measured child comes back at the origin, as a fresh layout result;
/// a parent that moves it relatively (a slide) must not add to where it
/// placed the child last time.
///
/// Sabotage: return `Node::of(tree)` unmoved from `measure` and the second
/// layout lands at twice the offset.
#[test]
fn a_measured_node_starts_at_the_origin() {
    struct Leaf;

    impl crate::Widget<(), crate::Theme, ()> for Leaf {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fixed(10.0), Length::Fixed(10.0))
        }

        // Like upstream widgets: sets its size, leaves its translation to
        // the parent.
        fn layout(&mut self, tree: &mut Tree, _renderer: &(), _limits: &Limits) {
            tree.size = Size::new(10.0, 10.0);
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut (),
            _theme: &crate::Theme,
            _style: &crate::renderer::Style,
            _layout: Layout,
            _cursor: crate::mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    let mut leaf = Leaf;
    let mut tree = Tree::empty();

    for _ in 0..2 {
        let node =
            Measure::<(), crate::Theme, ()>::measure(&mut leaf, &mut tree, &(), &Limits::NONE);
        // A parent that places its child relatively, then keeps the result.
        let mut placed = node.translate(Vector::new(5.0, 0.0));
        placed.move_to_mut(placed.position());
        tree.translation = Vector::new(placed.position().x, placed.position().y);
    }

    assert_eq!(tree.translation, Vector::new(5.0, 0.0));
}

/// A wrapper that lays out as its child reports that child's sizing to its
/// parent, so a parent container can still declare itself over it.
///
/// Sabotage: skip the pass-through walk in `Node::of` and the container
/// refuses its owned presentation.
#[test]
fn a_pass_through_passes_its_childs_sizing_up() {
    let mut wrapper = tree(1);
    spacer(10.0, 10.0).apply(&mut wrapper.children[0]);
    layout::pass_through(&mut wrapper, 0);

    let node = Node::of(&wrapper);
    assert!(node.presentation_sizing().is_some());

    let container = layout::aligned(
        &Limits::new(Size::ZERO, Size::new(40.0, 40.0)),
        Length::Shrink,
        Length::Shrink,
        crate::Padding::ZERO,
        [crate::Alignment::Start; 2],
        |_| node.clone(),
    );
    assert!(container.presentation_node().is_some());

    let mut applied = tree(1);
    node.apply(&mut applied);
    assert!(
        applied.presentation().is_none(),
        "the passed sizing is not stored"
    );
}

/// A row culls each child; a child that is a wrapper around a declared
/// widget stays transparent, and its cull scopes that widget.
///
/// Sabotage: test `declares_beyond_placement` in `transparent_child` and the
/// culled wrapper refuses the program; push the wrapper's cull unmerged and
/// the program refuses a second scope on one owner.
#[test]
fn a_culled_wrapper_stays_transparent() {
    // The row culls the wrapper; the widget it covers culls itself too.
    let wrapper = Node::with_children(
        Size::new(10.0, 10.0),
        vec![spacer(10.0, 10.0).with_drawing_cull(false)],
    )
    .with_drawing_cull(false);
    let program = Placed::of(&wrapper)
        .presentation_program(
            crate::renderer::NodeId(900),
            0,
            Rectangle::with_size(Size::new(100.0, 100.0)),
        )
        .expect("a culled wrapper is transparent");

    drop(program);
}

/// The band's allocation-free tree walks agree with the snapshot they
/// replace: an overhanging grandchild counts, hidden slot material does not.
///
/// Sabotage: drop the `hidden` check in `visit`'s callers and the hidden
/// slot's material widens the tree extent.
#[test]
fn tree_extents_agree_with_the_snapshot() {
    use crate::layout::priority_band::{
        content_clip, content_clip_in, subtree_extent, tree_extent,
    };

    let mut band = tree(2);
    band.children[0].children.push(Tree::empty());
    band.children[1].children.push(Tree::empty());

    let overhang =
        Node::with_children(Size::new(10.0, 10.0), vec![Node::new(Size::new(30.0, 5.0))]);
    let material =
        Node::with_children(Size::new(10.0, 10.0), vec![Node::new(Size::new(80.0, 5.0))]);
    let hidden =
        crate::layout::item_strip::slot(material, false, Point::new(12.0, 0.0), &Limits::NONE);
    Node::with_children(Size::new(20.0, 10.0), vec![overhang, hidden]).apply(&mut band);

    let snapshot = Node::snapshot(&band).move_to(Point::ORIGIN);
    assert_eq!(tree_extent(&band), subtree_extent(&snapshot));
    assert_eq!(tree_extent(&band).width, 30.0);
    assert_eq!(
        content_clip_in(Layout::new(band.size), &band, 0.5),
        content_clip(Placed::of(&snapshot), 0.5)
    );
}
