// Modified by enzUI contributors. See FORK.md.
//! Upstream iced #3466: a clip snaps its edges like the quad shader.
//!
//! The quad vertex shader nudges every snapped edge by 0.001 before
//! rounding, so an edge a hair below a half pixel (a centred or evenly
//! spaced layout computed with float noise) rounds up onto the grid. The
//! CPU-side scissor used to round plainly, down. A panel's content was then
//! cut one pixel short of the panel's own snapped edge, leaving a one-pixel
//! seam of whatever was behind it.
//!
//! Requires a working wgpu adapter; without one the test skips with a
//! printed reason rather than fabricate a result.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Background, Border, Color, Point, Rectangle, Shadow, Size};

const W: u32 = 40;
const H: u32 = 40;

/// An edge meant to land on the half pixel, computed a hair below it.
const EDGE: f32 = 10.4998;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

/// Sabotage: make `nudge::snap` round without the nudge — row 10 is the
/// black background instead of the panel.
#[test]
fn a_clip_ends_on_the_same_pixel_as_the_snapped_quad_it_holds() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let viewport = Rectangle::with_size(Size::new(W as f32, H as f32));
    let panel = Rectangle::new(Point::ORIGIN, Size::new(W as f32, EDGE));

    renderer.reset(viewport);
    renderer.with_layer(panel, |renderer| {
        renderer.fill_quad(
            Quad {
                bounds: panel,
                border: Border::default(),
                shadow: Shadow::default(),
                snap: true,
            },
            Background::Color(Color::WHITE),
        );
    });
    let pixels =
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);

    let row = |y: u32| pixels[((y * W + W / 2) * 4) as usize];
    assert_eq!(row(9), 255, "inside the panel");
    assert_eq!(
        row(10),
        255,
        "the snapped quad reaches row 10, and its clip must too"
    );
    assert_eq!(row(11), 0, "below the panel");
}
