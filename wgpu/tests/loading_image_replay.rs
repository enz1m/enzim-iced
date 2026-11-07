// Modified by enzUI contributors. See FORK.md.
//! A scene showing an image that has not loaded yet still replays.

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{FilterMethod, Handle, Image, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Settings};
use iced_wgpu::core::{Background, Color, Radians, Rectangle, Renderer as _, Size, Vector, border};

const W: u32 = 120;
const H: u32 = 120;

fn renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn draw(renderer: &mut Renderer, image: &Handle) {
    renderer.fill_quad(
        Quad {
            bounds: Rectangle::new([10.0, 10.0].into(), Size::new(40.0, 40.0)),
            ..Quad::default()
        },
        Background::Color(Color::from_rgb(1.0, 0.0, 0.0)),
    );
    let bounds = Rectangle::new([60.0, 60.0].into(), Size::new(40.0, 40.0));
    renderer.draw_image(
        Image {
            handle: image.clone(),
            filter_method: FilterMethod::Linear,
            rotation: Radians(0.0),
            border_radius: border::Radius::default(),
            opacity: 1.0,
        },
        bounds,
        bounds,
    );
}

/// An image the frame shows nothing for -- one still loading, or one that
/// cannot load -- is left out of the recording rather than refusing it, so a
/// page whose icons are still arriving keeps its animations on the render
/// lane, and the replay paints what the frame painted (enzUI-ICED #3660).
///
/// Sabotage: refuse a raster with no source in the cache -- the scene is not
/// recorded.
#[test]
fn an_image_that_shows_nothing_yet_does_not_refuse_the_scene() {
    let Some(mut renderer) = renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let window = Rectangle::with_size(Size::new(W as f32, H as f32));
    let missing = Handle::from_path("/nonexistent/enzui-3660/icon.png");

    renderer.reset(window);
    assert!(renderer.start_recording(window));
    draw(&mut renderer, &missing);
    let scene = renderer
        .finish_recording()
        .expect("a scene with an image that shows nothing yet is recorded");

    let mut lane = self::renderer().expect("a second renderer, as the render lane has");
    lane.reset(window);
    assert!(lane.replay(&scene, Vector::ZERO), "the scene replays");
    let replayed =
        <Renderer as Headless>::screenshot(&mut lane, Size::new(W, H), 1.0, Color::BLACK);
    let pixel = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [replayed[i], replayed[i + 1], replayed[i + 2]]
    };
    let [r, g, b] = pixel(30, 30);
    assert!(
        r > 200 && g < 40 && b < 40,
        "the quad replays: {:?}",
        [r, g, b]
    );
    assert_eq!(
        pixel(80, 80),
        [0, 0, 0],
        "the image shows nothing, as drawn"
    );
}
