// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #2795: a retained responsive presentation program reflows its
//! recorded paint from driver tracks on a real wgpu device.

use std::sync::Arc;

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::presentation::{Bounds, Error, Expression, Layout, Node, Program};
use iced_wgpu::core::renderer::{self, Headless, NodeId, Track};
use iced_wgpu::core::{Color, Rectangle, Renderer as _, Size, Vector};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(WIDTH as f32, HEIGHT as f32))
}

fn program(third_id: u64) -> Arc<Program> {
    Arc::new(
        Program::new(
            vec![
                Expression::Constant(0.0),
                Expression::ViewportWidth,
                Expression::Driver {
                    node: 900,
                    channel: 0,
                },
                Expression::Driver {
                    node: 901,
                    channel: 0,
                },
                Expression::Subtract(1, 2),
                Expression::Constant(5.0),
            ],
            vec![
                Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: Bounds {
                            x: 0.0,
                            y: 0.0,
                            width: 80.0,
                            height: 30.0,
                        },
                    },
                    children: vec![],
                },
                Node {
                    id: 2,
                    layout: Layout::Fixed {
                        recorded: Bounds {
                            x: 90.0,
                            y: 0.0,
                            width: 80.0,
                            height: 30.0,
                        },
                    },
                    children: vec![],
                },
                Node {
                    id: third_id,
                    layout: Layout::Fixed {
                        recorded: Bounds {
                            x: 180.0,
                            y: 0.0,
                            width: 80.0,
                            height: 30.0,
                        },
                    },
                    children: vec![],
                },
                Node {
                    id: 4,
                    layout: Layout::Flow {
                        width: Some(4),
                        height: None,
                        column_gap: 3,
                        row_gap: 5,
                    },
                    children: vec![0, 1, 2],
                },
            ],
            vec![],
            [2, 0],
            Bounds {
                x: 0.0,
                y: 0.0,
                width: WIDTH as f32,
                height: HEIGHT as f32,
            },
        )
        .unwrap(),
    )
}

fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        renderer::Settings::default(),
        Some("wgpu"),
    ))
    .expect("a real wgpu adapter is required for this pixel test")
}

fn quad(renderer: &mut Renderer, x: f32, y: f32, color: Color) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x,
                y,
                width: 80.0,
                height: 30.0,
            },
            ..renderer::Quad::default()
        },
        color,
    );
}

fn pixels(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, Size::new(WIDTH, HEIGHT), 1.0, Color::BLACK)
}

fn paint(renderer: &mut Renderer) {
    for (id, x, color) in [
        (1, 0.0, Color::from_rgb(1.0, 0.0, 0.0)),
        (2, 90.0, Color::from_rgb(0.0, 1.0, 0.0)),
        (3, 180.0, Color::from_rgb(0.0, 0.0, 1.0)),
    ] {
        renderer.mark_recording(NodeId(id));
        quad(renderer, x, 0.0, color);
        renderer.mark_recording(NodeId::STATIC);
    }
    // The static sibling is declared as fixed layout paint. A raw `fill_quad`
    // outside every program node is refused (`OpaquePaint`): the renderer
    // cannot tell a quad that stays put from one computed at draw time.
    renderer.fill_layout_quad(
        NodeId::STATIC,
        renderer::Quad {
            bounds: Rectangle {
                x: 20.0,
                y: 150.0,
                width: 80.0,
                height: 30.0,
            },
            ..renderer::Quad::default()
        },
        Color::WHITE.into(),
    );
}

fn drivers(width: f32, gap: f32) -> [Track; 2] {
    [
        Track {
            translation: Vector::new(width, 0.0),
            ..Track::identity(NodeId(900))
        },
        Track {
            translation: Vector::new(gap, 0.0),
            ..Track::identity(NodeId(901))
        },
    ]
}

#[test]
fn renderer_reflows_recorded_paint_and_preserves_the_static_sibling() {
    let mut renderer = renderer();
    renderer.reset(bounds());
    assert!(renderer.start_recording(bounds()));
    renderer.record_presentation(program(3)).unwrap();
    paint(&mut renderer);
    let recording = renderer
        .finish_recording()
        .expect("valid program and retained paint");
    let recorded = pixels(&mut renderer);

    renderer.reset(bounds());
    assert!(!renderer.replay(&recording, Vector::ZERO));
    assert_eq!(
        renderer.presentation_refusal(),
        Some(Error::TrackReplayRequired)
    );

    renderer.reset(bounds());
    assert!(renderer.replay_tracks(&recording, &drivers(0.0, 10.0)));
    assert_eq!(pixels(&mut renderer), recorded);

    renderer.reset(bounds());
    assert!(renderer.replay_tracks(&recording, &drivers(150.0, 10.0)));
    let moved = pixels(&mut renderer);
    assert_ne!(moved, recorded);
    assert_eq!(renderer.presentation_work().layout_nodes, 4);

    renderer.reset(bounds());
    assert!(renderer.replay_tracks(&recording, &drivers(150.0, 10.0)));
    assert_eq!(pixels(&mut renderer), moved);
    assert_eq!(renderer.presentation_work().layout_nodes, 0);

    renderer.reset(bounds());
    quad(&mut renderer, 150.0, 0.0, Color::from_rgb(1.0, 0.0, 0.0));
    quad(&mut renderer, 240.0, 0.0, Color::from_rgb(0.0, 1.0, 0.0));
    quad(&mut renderer, 150.0, 35.0, Color::from_rgb(0.0, 0.0, 1.0));
    quad(&mut renderer, 20.0, 150.0, Color::WHITE);
    assert_eq!(moved, pixels(&mut renderer));

    renderer.reset(bounds());
    assert!(!renderer.replay_tracks(&recording, &drivers(300.0, 10.0)));
    assert_eq!(
        renderer.presentation_refusal(),
        Some(Error::ReflowRequired(1))
    );
}

#[test]
fn program_admission_checks_the_nodes_that_were_actually_recorded() {
    let mut renderer = renderer();
    renderer.reset(bounds());
    assert!(renderer.start_recording(bounds()));
    assert!(renderer.start_recording(bounds()));
    assert_eq!(
        renderer.record_presentation(program(3)),
        Err(Error::RecordingState)
    );
    let _ = renderer.finish_recording();
    let _ = renderer.finish_recording();
    renderer.reset(bounds());
    assert!(renderer.start_recording(bounds()));
    renderer.record_presentation(program(99)).unwrap();
    paint(&mut renderer);
    assert!(renderer.finish_recording().is_none());
    assert_eq!(
        renderer.presentation_refusal(),
        Some(Error::SceneIdentity(99))
    );
}
