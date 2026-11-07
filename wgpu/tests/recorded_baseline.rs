// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #3251: a node recorded at a baseline replays a track as its
//! whole translation, whichever frame of the travel recorded it.
//!
//! A slide draws its child moved by the current offset and is recorded that
//! way. The render lane pairs the newest scene with a curve published earlier,
//! so a curve measured against the previous recording put the panel off by one
//! frame's travel. With `mark_recording_at` the scene carries its own baseline
//! and a track is absolute.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{self, Headless, NodeId, Track};
use iced_wgpu::core::{Color, Rectangle, Renderer as _, Size, Vector};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 80;
const NODE: NodeId = NodeId(7);

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(WIDTH as f32, HEIGHT as f32))
}

fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        renderer::Settings::default(),
        Some("wgpu"),
    ))
    .expect("a real wgpu adapter is required for this pixel test")
}

fn panel(renderer: &mut Renderer, x: f32) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x,
                y: 20.0,
                width: 60.0,
                height: 40.0,
            },
            ..renderer::Quad::default()
        },
        Color::from_rgb(0.0, 0.6, 1.0),
    );
}

fn pixels(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, Size::new(WIDTH, HEIGHT), 1.0, Color::BLACK)
}

/// Records the panel drawn moved by `offset`, as a slide frame does.
fn record_at(renderer: &mut Renderer, offset: f32, baseline: bool) -> renderer::Recording {
    renderer.reset(bounds());
    assert!(renderer.start_recording(bounds()));
    if baseline {
        renderer.mark_recording_at(NODE, Vector::new(offset, 0.0));
    } else {
        renderer.mark_recording(NODE);
    }
    renderer.with_translation(Vector::new(offset, 0.0), |renderer| panel(renderer, 0.0));
    renderer.mark_recording(NodeId::STATIC);
    renderer.finish_recording().expect("a retained scene")
}

fn replayed(renderer: &mut Renderer, recording: &renderer::Recording, x: f32) -> Vec<u8> {
    renderer.reset(bounds());
    let track = Track {
        translation: Vector::new(x, 0.0),
        ..Track::identity(NODE)
    };
    assert!(renderer.replay_tracks(recording, &[track]));
    pixels(renderer)
}

fn drawn(renderer: &mut Renderer, x: f32) -> Vec<u8> {
    renderer.reset(bounds());
    panel(renderer, x);
    pixels(renderer)
}

/// Scenes recorded at two different points of the travel both put the panel
/// at the track's offset.
///
/// Sabotage: drop the baseline subtraction in `replay_tracks_at` and the
/// scene recorded at 40 lands at 200.
#[test]
fn a_track_is_absolute_for_a_node_recorded_at_a_baseline() {
    let mut renderer = renderer();
    let expected = drawn(&mut renderer, 160.0);

    for recorded_at in [40.0, 100.0] {
        let scene = record_at(&mut renderer, recorded_at, true);
        assert!(
            replayed(&mut renderer, &scene, 160.0) == expected,
            "a scene recorded at {recorded_at} did not replay the panel at 160",
        );
    }
}

/// A node marked without a baseline keeps the relative meaning every other
/// track publisher relies on.
///
/// Sabotage: let `Scene::baseline` answer for a node that has none and this
/// reddens.
#[test]
fn a_node_without_a_baseline_moves_by_the_track() {
    let mut renderer = renderer();
    let expected = drawn(&mut renderer, 160.0);

    let scene = record_at(&mut renderer, 40.0, false);
    assert!(replayed(&mut renderer, &scene, 120.0) == expected);
}
