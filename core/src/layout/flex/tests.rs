// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::{Layout, Rectangle, Widget, mouse, renderer};
use std::cell::RefCell;
use std::rc::Rc;

type Trace = Rc<RefCell<Vec<(char, usize)>>>;

struct Child {
    index: usize,
    size: Size<Length>,
    trace: Trace,
}

impl Widget<(), (), ()> for Child {
    fn size(&self) -> Size<Length> {
        self.trace.borrow_mut().push(('s', self.index));
        self.size
    }
    fn layout(&mut self, tree: &mut widget::Tree, _: &(), limits: &Limits) {
        self.trace.borrow_mut().push(('l', self.index));
        measured(self.size, limits).apply(tree);
    }
    fn draw(
        &self,
        _: &widget::Tree,
        _: &mut (),
        _: &(),
        _: &renderer::Style,
        _: Layout,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
    }
}

fn measured(size: Size<Length>, limits: &Limits) -> Node {
    Node::new(limits.width(size.width).height(size.height).resolve(
        size.width,
        size.height,
        Size::new(10.0, 10.0),
    ))
}

struct Owned {
    sizes: Vec<Size<Length>>,
    trace: Trace,
}

impl Content for Owned {
    fn count(&self) -> usize {
        self.sizes.len()
    }
    fn sizing(&mut self, index: usize) -> Size<Length> {
        self.trace.borrow_mut().push(('s', index));
        self.sizes[index]
    }
    fn layout(&mut self, index: usize, limits: &Limits) -> Node {
        self.trace.borrow_mut().push(('l', index));
        measured(self.sizes[index], limits)
    }
}

fn sizes() -> Vec<Size<Length>> {
    vec![
        Size::new(Length::Fixed(50.0), Length::Fixed(10.0)),
        Size::new(Length::FillPortion(1), Length::Fill),
        Size::new(Length::FillPortion(3), Length::Fixed(20.0)),
        Size::new(Length::Fixed(20.0), Length::Fixed(10.0)),
    ]
}

fn expected() -> Vec<Rectangle> {
    vec![
        Rectangle {
            x: 10.0,
            y: 15.0,
            width: 50.0,
            height: 10.0,
        },
        Rectangle {
            x: 65.0,
            y: 5.0,
            width: 48.75,
            height: 30.0,
        },
        Rectangle {
            x: 118.75,
            y: 10.0,
            width: 146.25,
            height: 20.0,
        },
        Rectangle {
            x: 270.0,
            y: 15.0,
            width: 20.0,
            height: 10.0,
        },
    ]
}

fn padding() -> Padding {
    Padding {
        left: 10.0,
        right: 10.0,
        top: 5.0,
        bottom: 5.0,
    }
}

#[test]
fn owned_and_widget_flex_share_fill_allocation_and_cross_alignment() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut children: Vec<Element<'_, (), (), ()>> = sizes()
        .into_iter()
        .enumerate()
        .map(|(index, size)| {
            Element::new(Child {
                index,
                size,
                trace: trace.clone(),
            })
        })
        .collect();
    let mut trees: Vec<_> = children.iter().map(widget::Tree::new).collect();
    trace.borrow_mut().clear();
    let limits = Limits::new(Size::ZERO, Size::new(300.0, 40.0));
    let ui = resolve(
        Axis::Horizontal,
        &(),
        &limits,
        Length::Fill,
        Length::Fixed(40.0),
        padding(),
        5.0,
        Alignment::Center,
        &mut children,
        &mut trees,
    );
    assert_eq!(ui.size(), Size::new(300.0, 40.0));
    assert_eq!(
        ui.children().iter().map(Node::bounds).collect::<Vec<_>>(),
        expected()
    );
    let mut owned = Owned {
        sizes: sizes(),
        trace: Rc::new(RefCell::new(Vec::new())),
    };
    let result = resolve_content(
        Axis::Horizontal,
        &limits,
        Length::Fill,
        Length::Fixed(40.0),
        padding(),
        5.0,
        Alignment::Center,
        &mut owned,
    );
    assert_eq!(
        result
            .children()
            .iter()
            .map(Node::bounds)
            .collect::<Vec<_>>(),
        expected()
    );
    assert_eq!(*trace.borrow(), *owned.trace.borrow());
}

#[test]
fn static_layout_remains_interleaved_with_later_sizing_queries() {
    let mut owned = Owned {
        sizes: sizes(),
        trace: Rc::new(RefCell::new(Vec::new())),
    };
    let _ = resolve_content(
        Axis::Horizontal,
        &Limits::new(Size::ZERO, Size::new(300.0, 40.0)),
        Length::Fill,
        Length::Fixed(40.0),
        padding(),
        5.0,
        Alignment::Center,
        &mut owned,
    );
    assert_eq!(
        *owned.trace.borrow(),
        vec![
            ('s', 0),
            ('l', 0),
            ('s', 1),
            ('s', 2),
            ('s', 3),
            ('l', 3),
            ('l', 1),
            ('l', 2)
        ]
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Sizing(usize),
    Layout(usize),
}

struct Refusable {
    inner: Owned,
    refusal: Option<Refusal>,
}

impl TryContent for Refusable {
    type Error = Refusal;

    fn count(&self) -> usize {
        self.inner.count()
    }

    fn sizing(&mut self, index: usize) -> Result<Size<Length>, Refusal> {
        let sizing = self.inner.sizing(index);
        if self.refusal == Some(Refusal::Sizing(index)) {
            Err(Refusal::Sizing(index))
        } else {
            Ok(sizing)
        }
    }

    fn layout(&mut self, index: usize, limits: &Limits) -> Result<Node, Refusal> {
        self.inner.trace.borrow_mut().push(('l', index));
        if self.refusal == Some(Refusal::Layout(index)) {
            Err(Refusal::Layout(index))
        } else {
            Ok(measured(self.inner.sizes[index], limits))
        }
    }
}

fn refused_at(
    sizes: Vec<Size<Length>>,
    height: Length,
    refusal: Refusal,
    expected_trace: &[(char, usize)],
) {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut content = Refusable {
        inner: Owned {
            sizes,
            trace: trace.clone(),
        },
        refusal: Some(refusal),
    };
    let result = try_resolve_content(
        Axis::Horizontal,
        &Limits::new(Size::ZERO, Size::new(300.0, 40.0)),
        Length::Fill,
        height,
        Padding::ZERO,
        0.0,
        Alignment::Start,
        &mut content,
    );
    assert_eq!(result, Err(refusal));
    assert_eq!(*trace.borrow(), expected_trace);
    trace.borrow_mut().clear();
    content.refusal = None;
    let recovered = try_resolve_content(
        Axis::Horizontal,
        &Limits::new(Size::ZERO, Size::new(300.0, 40.0)),
        Length::Fill,
        height,
        Padding::ZERO,
        0.0,
        Alignment::Start,
        &mut content,
    )
    .expect("same resources recover without partial geometry");
    assert_eq!(recovered.children().len(), content.inner.sizes.len());
    assert!(recovered.children().iter().all(|n| n.size().height > 0.0));
}

#[test]
fn fallible_sizing_stops_before_later_children() {
    refused_at(
        vec![Size::new(Length::Fixed(20.0), Length::Fixed(10.0)); 3],
        Length::Fixed(40.0),
        Refusal::Sizing(1),
        &[('s', 0), ('l', 0), ('s', 1)],
    );
}

#[test]
fn fallible_static_measurement_stops_before_later_sizing() {
    refused_at(
        vec![Size::new(Length::Fixed(20.0), Length::Fixed(10.0)); 3],
        Length::Fixed(40.0),
        Refusal::Layout(0),
        &[('s', 0), ('l', 0)],
    );
}

#[test]
fn fallible_cross_fluid_measurement_stops_without_publishing_static_siblings() {
    refused_at(
        vec![
            Size::new(Length::Shrink, Length::Fill),
            Size::new(Length::Fixed(20.0), Length::Fixed(10.0)),
        ],
        Length::Shrink,
        Refusal::Layout(0),
        &[('s', 0), ('s', 1), ('l', 1), ('l', 0)],
    );
}

#[test]
fn fallible_bounded_measurement_stops_both_constraint_passes() {
    for bounds in [length::Bounds::Max(20.0), length::Bounds::Min(200.0)] {
        refused_at(
            vec![
                Size::new(
                    Length::Bounded {
                        sizing: length::Sizing::Fill(1),
                        bounds,
                    },
                    Length::Fixed(10.0),
                ),
                Size::new(Length::Fill, Length::Fixed(10.0)),
            ],
            Length::Fixed(40.0),
            Refusal::Layout(0),
            &[('s', 0), ('s', 1), ('l', 0)],
        );
    }
}

#[test]
fn fallible_main_fluid_measurement_stops_before_later_fluid_siblings() {
    refused_at(
        vec![Size::new(Length::Fill, Length::Fixed(10.0)); 2],
        Length::Fixed(40.0),
        Refusal::Layout(0),
        &[('s', 0), ('s', 1), ('l', 0)],
    );
}

#[test]
fn fallible_deferred_cross_measurement_does_not_publish_prior_fluid_siblings() {
    refused_at(
        vec![
            Size::new(Length::Fixed(20.0), Length::Fill),
            Size::new(Length::Fill, Length::Fixed(10.0)),
        ],
        Length::Shrink,
        Refusal::Layout(0),
        &[('s', 0), ('s', 1), ('l', 1), ('l', 0)],
    );
}

#[test]
fn fallible_success_preserves_shared_flex_geometry_and_query_order() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut content = Refusable {
        inner: Owned {
            sizes: sizes(),
            trace: trace.clone(),
        },
        refusal: None,
    };
    let result = try_resolve_content(
        Axis::Horizontal,
        &Limits::new(Size::ZERO, Size::new(300.0, 40.0)),
        Length::Fill,
        Length::Fixed(40.0),
        padding(),
        5.0,
        Alignment::Center,
        &mut content,
    )
    .expect("valid owned layout");
    assert_eq!(
        result
            .children()
            .iter()
            .map(Node::bounds)
            .collect::<Vec<_>>(),
        expected()
    );
    assert_eq!(
        *trace.borrow(),
        [
            ('s', 0),
            ('l', 0),
            ('s', 1),
            ('s', 2),
            ('s', 3),
            ('l', 3),
            ('l', 1),
            ('l', 2)
        ]
    );
}

/// A label that wants `natural` main-axis pixels and takes what the limits
/// allow, as a no-wrap eliding text does.
struct Greedy {
    natural: f32,
    layouts: Rc<RefCell<usize>>,
}

impl Content for Greedy {
    fn count(&self) -> usize {
        2
    }
    fn sizing(&mut self, index: usize) -> Size<Length> {
        if index == 0 {
            Size::new(Length::Shrink, Length::Shrink)
        } else {
            Size::new(Length::Fixed(20.0), Length::Fixed(20.0))
        }
    }
    fn layout(&mut self, index: usize, limits: &Limits) -> Node {
        if index == 0 {
            *self.layouts.borrow_mut() += 1;
            Node::new(Size::new(self.natural.min(limits.max.width), 20.0))
        } else {
            measured(self.sizing(1), limits)
        }
    }
}

#[test]
fn a_fixed_child_after_an_overflowing_label_keeps_its_extent() {
    let run = |natural, width| {
        let mut content = Greedy {
            natural,
            layouts: Rc::new(RefCell::new(0)),
        };
        let node = resolve_content(
            Axis::Horizontal,
            &Limits::new(Size::ZERO, Size::new(width, 20.0)),
            Length::Fill,
            Length::Shrink,
            Padding::ZERO,
            0.0,
            Alignment::Center,
            &mut content,
        );
        let bounds: Vec<_> = node.children().iter().map(Node::bounds).collect();
        (bounds, *content.layouts.borrow())
    };

    // The label would take all 100 px; the badge keeps its 20 at the end.
    let (bounds, measured_label) = run(140.0, 100.0);
    assert_eq!(bounds[0].width, 80.0);
    assert_eq!((bounds[1].x, bounds[1].width), (80.0, 20.0));
    assert_eq!(
        measured_label, 2,
        "re-measured once with the badge reserved"
    );

    // A label that fits is measured once and placed as before.
    let (bounds, measured_label) = run(50.0, 100.0);
    assert_eq!(bounds[0].width, 50.0);
    assert_eq!((bounds[1].x, bounds[1].width), (50.0, 20.0));
    assert_eq!(measured_label, 1);

    // Fixed children that cannot fit at all are not reserved: the label is
    // not starved to nothing for a badge that would overflow anyway.
    let (bounds, measured_label) = run(140.0, 15.0);
    assert_eq!(bounds[0].width, 15.0);
    assert_eq!(measured_label, 1);
}
