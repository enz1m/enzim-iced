// Modified by enzUI contributors. See FORK.md.
//! An SVG the render lane cannot rasterize itself still replays there.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Settings};
use iced_wgpu::core::svg::{Handle, Renderer as _, Svg};
use iced_wgpu::core::{Color, Radians, Rectangle, Renderer as _, Size, Vector};

const W: u32 = 120;
const H: u32 = 120;

const FILTERED: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
    <filter id="soft"><feGaussianBlur stdDeviation="1"/></filter>
    <rect x="8" y="8" width="48" height="48" fill="#e01b24" filter="url(#soft)"/>
</svg>"##;

fn renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn draw(renderer: &mut Renderer, handle: &Handle) {
    let bounds = Rectangle::new([20.0, 20.0].into(), Size::new(80.0, 80.0));
    renderer.draw_svg(
        Svg {
            handle: handle.clone(),
            color: None,
            rotation: Radians(0.0),
            opacity: 1.0,
        },
        bounds,
        bounds,
    );
}

fn centre(pixels: &[u8]) -> [u8; 3] {
    let i = (((H / 2) * W + W / 2) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// A filtered SVG is drawn on the render lane from the raster the UI thread
/// froze at its drawn size, so a scene showing it is still recorded and its
/// replay on another renderer paints what a direct draw paints.
///
/// Sabotage: refuse an SVG the lane cannot rasterize instead of freezing
/// it -- the scene is not recorded.
#[test]
fn an_svg_with_a_filter_replays_from_its_frozen_raster() {
    let Some(mut renderer) = renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let window = Rectangle::with_size(Size::new(W as f32, H as f32));
    let handle = Handle::from_memory(FILTERED.to_vec());

    renderer.reset(window);
    draw(&mut renderer, &handle);
    let drawn =
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);

    renderer.reset(window);
    assert!(renderer.start_recording(window));
    draw(&mut renderer, &handle);
    let scene = renderer
        .finish_recording()
        .expect("a scene with a filtered SVG is recorded");

    let mut lane = self::renderer().expect("a second renderer, as the render lane has");
    lane.reset(window);
    assert!(lane.replay(&scene, Vector::ZERO), "the scene replays");
    let replayed =
        <Renderer as Headless>::screenshot(&mut lane, Size::new(W, H), 1.0, Color::BLACK);

    let [r, g, b] = centre(&drawn);
    assert!(
        r > 180 && g < 80 && b < 80,
        "the direct draw is red: {:?}",
        [r, g, b]
    );
    let replayed = centre(&replayed);
    for (a, b) in [r, g, b].into_iter().zip(replayed) {
        assert!(
            a.abs_diff(b) <= 4,
            "replayed {replayed:?} where drawn {:?}",
            [r, g, b]
        );
    }
}
