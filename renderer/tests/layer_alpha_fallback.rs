// Modified by enzUI contributors. See FORK.md.
//! The fallback renderer must forward subtree alpha to its active backend.
//!
//! If `fallback.rs` omits the method, Rust selects the core trait's opaque
//! default on the wrapper and tiny-skia's real implementation is unreachable.

use iced_renderer::core::renderer::{Quad, Renderer as _, Scale, Settings};
use iced_renderer::core::{Background, Border, Color, Rectangle, Shadow, Size};
use iced_renderer::fallback;
use iced_renderer::graphics::Viewport;
use iced_tiny_skia::window::compositor::screenshot;

const W: u32 = 40;
const H: u32 = 40;

#[test]
fn fallback_forwards_alpha_to_tiny_skia() {
    let mut renderer = fallback::Renderer::<(), iced_tiny_skia::Renderer>::Secondary(
        iced_tiny_skia::Renderer::new(Settings::default()),
    );
    let bounds = Rectangle::with_size(Size::new(W as f32, H as f32));

    renderer.start_layer_alpha(bounds, 0.5);
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
    );
    renderer.end_layer();

    let fallback::Renderer::Secondary(renderer) = &mut renderer else {
        unreachable!("the test constructs the tiny-skia fallback variant")
    };
    let pixels = screenshot(
        renderer,
        &Viewport::with_physical_size(Size::new(W, H), Scale::default()),
        Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        },
    );
    let i = (((H / 2) * W + W / 2) * 4) as usize;
    let pixel = &pixels[i..i + 4];

    assert!(
        (110..=145).contains(&pixel[0]) && pixel[1] < 8 && (110..=145).contains(&pixel[2]),
        "fallback swallowed tiny-skia alpha; expected half red over blue, got {pixel:?}"
    );
}
