// Modified by enzUI contributors. See FORK.md.
use super::*;
use crate::layout::{Limits, Node as Geometry, flex as native};
use crate::{Alignment, Length, Padding, Size};

#[derive(Clone)]
enum Content {
    Text {
        id: u64,
        advance: f32,
        sizing: Size<Length>,
    },
    Flex {
        axis: Axis,
        sizing: Size<Length>,
        children: Vec<Content>,
    },
    Opaque,
}

fn intrinsic(advance: f32, width: f32, height: Option<f32>) -> [f32; 2] {
    let natural = (advance / width).ceil().max(1.0) * 10.0;
    [
        advance.min(width),
        height.map_or(natural, |max| natural.min(max)),
    ]
}

impl Content {
    fn sizing(&self) -> Size<Length> {
        match self {
            Self::Text { sizing, .. } | Self::Flex { sizing, .. } => *sizing,
            Self::Opaque => Size::new(Length::Fill, Length::Shrink),
        }
    }

    fn layout(&self, limits: &Limits) -> Geometry {
        match self {
            Self::Text {
                id,
                advance,
                sizing,
            } => {
                let limits = limits.width(sizing.width).height(sizing.height);
                let metrics = intrinsic(
                    *advance,
                    limits.max().width,
                    limits
                        .max()
                        .height
                        .is_finite()
                        .then_some(limits.max().height),
                );
                Geometry::new(limits.resolve(
                    sizing.width,
                    sizing.height,
                    Size::new(metrics[0], metrics[1]),
                ))
                .with_text_presentation(
                    crate::renderer::NodeId(*id),
                    *sizing,
                    limits.max(),
                )
            }
            Self::Flex {
                axis,
                sizing,
                children,
            } => native::resolve_content(
                match axis {
                    Axis::Horizontal => native::Axis::Horizontal,
                    Axis::Vertical => native::Axis::Vertical,
                },
                limits,
                sizing.width,
                sizing.height,
                Padding::ZERO,
                4.0,
                Alignment::Start,
                &mut Children(children),
            ),
            Self::Opaque => Geometry::new(Size::new(10.0, 10.0)),
        }
    }
}

struct Children<'a>(&'a [Content]);
impl native::Content for Children<'_> {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn sizing(&mut self, index: usize) -> Size<Length> {
        self.0[index].sizing()
    }
    fn layout(&mut self, index: usize, limits: &Limits) -> Geometry {
        self.0[index].layout(limits)
    }
}

struct Metrics {
    calls: Vec<(u64, f32, Option<f32>)>,
    fail: Option<u64>,
}
impl TextMeasurer for Metrics {
    fn measure(&mut self, node: u64, width: f32, height: Option<f32>) -> Result<[f32; 2], Error> {
        self.calls.push((node, width, height));
        if self.fail == Some(node) {
            return Err(Error::FontAssets);
        }
        Ok(intrinsic(
            if node == 1 { 200.0 } else { 100.0 },
            width,
            height,
        ))
    }
}

fn root(children: &[Content], width: f32, height: Length) -> Geometry {
    native::resolve_content(
        native::Axis::Horizontal,
        &Limits::new(Size::ZERO, Size::new(width, 200.0)),
        Length::Fill,
        height,
        Padding {
            left: 10.0,
            right: 10.0,
            top: 5.0,
            bottom: 5.0,
        },
        10.0,
        Alignment::Center,
        &mut Children(children),
    )
    .move_to((20.0, 30.0))
}

fn text(id: u64, portion: u16, height: Length) -> Content {
    Content::Text {
        id,
        advance: if id == 1 { 200.0 } else { 100.0 },
        sizing: Size::new(Length::FillPortion(portion), height),
    }
}

fn lower(node: &Geometry) -> Arc<Program> {
    crate::layout::Placed::of(node)
        .presentation_program(
            crate::renderer::NodeId(900),
            0,
            crate::Rectangle::new(crate::Point::ORIGIN, Size::new(400.0, 240.0)),
        )
        .unwrap()
}

fn input(width: f32) -> Input {
    Input {
        node: 900,
        values: [width, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        available: 15,
    }
}

#[test]
fn default_flex_metadata_reflows_proportions_and_cross_alignment() {
    let node = root(
        &[text(1, 1, Length::Shrink), text(2, 3, Length::Shrink)],
        300.0,
        Length::Shrink,
    );
    let program = lower(&node);
    let mut state = State::default();
    let mut metrics = Metrics {
        calls: vec![],
        fail: None,
    };
    let revision = Arc::new(());
    let wide = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(300.0)],
            &revision,
            &mut metrics,
        )
        .unwrap();
    assert_eq!(
        wide[0].bounds,
        Bounds {
            x: 30.0,
            y: 35.0,
            width: 67.5,
            height: 30.0
        }
    );
    assert_eq!(
        wide[1].bounds,
        Bounds {
            x: 107.5,
            y: 45.0,
            width: 202.5,
            height: 10.0
        }
    );
    let narrow = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(200.0)],
            &revision,
            &mut metrics,
        )
        .unwrap();
    assert_eq!(
        narrow[0].bounds,
        Bounds {
            x: 30.0,
            y: 35.0,
            width: 42.5,
            height: 50.0
        }
    );
    assert_eq!(
        narrow[1].bounds,
        Bounds {
            x: 82.5,
            y: 55.0,
            width: 127.5,
            height: 10.0
        }
    );
    let calls = metrics.calls.len();
    let _ = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(200.0)],
            &revision,
            &mut metrics,
        )
        .unwrap();
    assert_eq!(metrics.calls.len(), calls);
    assert_eq!(state.work().layout_nodes, 0);
}

#[test]
fn default_flex_measurement_uses_current_finite_height_allocation() {
    let node = root(
        &[text(1, 1, Length::Fill), text(2, 1, Length::Fill)],
        300.0,
        Length::Fixed(40.0),
    );
    let program = lower(&node);
    let mut state = State::default();
    let mut metrics = Metrics {
        calls: vec![],
        fail: None,
    };
    let result = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(100.0)],
            &Arc::new(()),
            &mut metrics,
        )
        .unwrap();
    assert_eq!(
        metrics.calls,
        [(1, 35.0, Some(30.0)), (2, 35.0, Some(30.0))]
    );
    assert!(result.iter().all(|p| p.bounds.height == 30.0));
}

#[test]
fn default_flex_refusal_stops_measurement_and_recovers_without_replacing_program() {
    let node = root(
        &[text(1, 1, Length::Shrink), text(2, 1, Length::Shrink)],
        300.0,
        Length::Shrink,
    );
    let program = lower(&node);
    let mut state = State::default();
    let mut metrics = Metrics {
        calls: vec![],
        fail: Some(1),
    };
    let revision = Arc::new(());
    assert_eq!(
        state.sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(200.0)],
            &revision,
            &mut metrics
        ),
        Err(Error::FontAssets)
    );
    assert_eq!(metrics.calls.len(), 1);
    metrics.fail = None;
    let result = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(200.0)],
            &revision,
            &mut metrics,
        )
        .unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(metrics.calls.len(), 3);
}

#[test]
fn default_flex_does_not_infer_opaque_child_rules_from_its_geometry() {
    let node = root(
        &[text(1, 1, Length::Shrink), Content::Opaque],
        300.0,
        Length::Shrink,
    );
    assert_eq!(
        crate::layout::Placed::of(&node)
            .presentation_program(
                crate::renderer::NodeId(900),
                0,
                crate::Rectangle::new(crate::Point::ORIGIN, Size::new(400.0, 240.0))
            )
            .unwrap_err(),
        Error::UnsupportedBackend
    );
}

#[test]
fn default_flex_lowers_nested_containers_and_moves_following_text_after_reflow() {
    let children = [
        text(1, 1, Length::Shrink),
        Content::Flex {
            axis: Axis::Vertical,
            sizing: Size::new(Length::Fill, Length::Shrink),
            children: vec![text(2, 1, Length::Shrink), text(3, 1, Length::Shrink)],
        },
    ];
    let node = root(&children, 300.0, Length::Shrink);
    let program = lower(&node);
    let mut state = State::default();
    let mut metrics = Metrics {
        calls: vec![],
        fail: None,
    };
    let result = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(100.0)],
            &Arc::new(()),
            &mut metrics,
        )
        .unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(
        result[2].bounds.y - result[1].bounds.y,
        result[1].bounds.height + 4.0
    );
    assert_eq!(result[0].bounds.width, 35.0);
    assert_eq!(result[1].bounds.width, 35.0);
    assert_eq!(result[2].bounds.height, 30.0);
    let expanded = state
        .sample_with_text(
            &program,
            [400.0, 240.0],
            &[input(350.0)],
            &Arc::new(()),
            &mut metrics,
        )
        .unwrap();
    assert_eq!(expanded[1].bounds.width, 160.0);
    assert_eq!(expanded[2].bounds.width, 160.0);
    assert_eq!(expanded[2].bounds.height, 10.0);
}
