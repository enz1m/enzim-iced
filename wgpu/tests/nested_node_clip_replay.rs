// Modified by enzUI contributors. See FORK.md.
//! A clip opened outside every moving node stays where it is when a node
//! nested in another moving node is replayed.

use iced_wgpu::Renderer;
use iced_wgpu::core::border;
use iced_wgpu::core::renderer::{Headless, NodeId, Quad, Settings, Track};
use iced_wgpu::core::{Color, Point, Rectangle, Renderer as _, Size, Vector};

const W: u32 = 400;
const H: u32 = 120;

fn track(node: NodeId, translation: f32) -> Track {
    Track {
        node,
        translation: Vector::new(translation, 0.0),
        opacity: 1.0,
        scale: 1.0,
        scale_origin: Point::ORIGIN,
        rotation: 0.0,
        clip: None,
    }
}

/// A window-wide rounded clip, then a group node with a panel node inside
/// it, painted half off the window's leading edge. Replayed with the group
/// moved right, the panel's off-window half comes into view: the window's
/// clip and mask stay at the window, they do not move with the group.
///
/// Sabotage: map every clip enclosing a segment through all of its node's
/// ancestors -- the window's clip moves with the group and cuts the panel at
/// its old left edge, so the left of the moved panel stays black.
#[test]
fn a_clip_opened_outside_every_node_stays_put_under_a_moving_parent() {
    let Some(mut renderer) = pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    )) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let window = Rectangle::with_size(Size::new(W as f32, H as f32));
    let group = NodeId(9_001);
    let panel = NodeId(9_002);
    let red = Color::from_rgb(1.0, 0.0, 0.0);

    renderer.reset(window);
    assert!(renderer.start_recording(window));
    renderer.start_layer_rounded(window, border::Radius::from(8.0));
    renderer.mark_recording(group);
    renderer.mark_recording(panel);
    // The panel spans x = -100..100; only its right half is on screen.
    let panel_bounds = Rectangle::new(Point::new(-100.0, 0.0), Size::new(200.0, H as f32));
    renderer.with_layer_retained(
        Rectangle::new(Point::ORIGIN, Size::new(100.0, H as f32)),
        panel_bounds,
        |renderer| {
            renderer.fill_quad(
                Quad {
                    bounds: panel_bounds,
                    ..Quad::default()
                },
                red,
            );
        },
    );
    renderer.mark_recording(NodeId::STATIC);
    renderer.mark_recording(NodeId::STATIC);
    renderer.end_layer();
    let scene = renderer.finish_recording().expect("a recording");

    renderer.reset(window);
    let mut panel_track = track(panel, 0.0);
    panel_track.clip = Some(Rectangle::new(
        Point::new(-1.0e6, -1.0e6),
        Size::new(2.0e6, 2.0e6),
    ));
    assert!(renderer.replay_tracks(&scene, &[track(group, 100.0), panel_track]));
    let pixels =
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);

    let red_at = |x: u32| {
        let i = ((H / 2 * W + x) * 4) as usize;
        pixels[i] > 200 && pixels[i + 1] < 60 && pixels[i + 2] < 60
    };
    assert!(red_at(150), "the panel's recorded half moved to 100..200");
    assert!(
        red_at(50),
        "the panel's off-window half came into view at 0..100"
    );
    assert!(!red_at(250), "nothing past the panel");
}
