// Modified by enzUI contributors. See FORK.md.
//! A raster image's own border radius rounds what the software renderer
//! draws, as it does on the GPU (enzUI-ICED #3448).

use iced_tiny_skia::Renderer;
use iced_tiny_skia::core::image::{Handle, Image, Renderer as _};
use iced_tiny_skia::core::renderer::{Scale, Settings};
use iced_tiny_skia::core::{Color, Rectangle, Size, border};
use iced_tiny_skia::graphics::Viewport;
use iced_tiny_skia::window::compositor::screenshot;

const LOGICAL: u32 = 100;
const BG: Color = Color::from_rgb(0.0, 0.0, 1.0);

/// A red 72 px avatar at (14, 14), radius 36, drawn from `bounds`.
fn render(scale: f32, bounds: Rectangle) -> (Vec<u8>, u32) {
    let mut renderer = Renderer::new(Settings::default());
    let size = (LOGICAL as f32 * scale) as u32;
    let viewport = Viewport::with_physical_size(
        Size::new(size, size),
        Scale {
            window: scale,
            application: 1.0,
        },
    );
    let visible = Rectangle::new([14.0, 14.0].into(), Size::new(72.0, 72.0));
    let image = Image {
        border_radius: border::Radius::new(36.0),
        ..Image::new(Handle::from_rgba(2, 2, [255, 0, 0, 255].repeat(4)))
    };
    renderer.draw_image(image, bounds, visible);
    (screenshot(&mut renderer, &viewport, BG), size)
}

fn red(pixels: &[u8], size: u32, x: f32, y: f32, scale: f32) -> bool {
    let (x, y) = ((x * scale) as u32, (y * scale) as u32);
    let i = ((y * size + x) * 4) as usize;
    pixels[i] > 200 && pixels[i + 2] < 60
}

#[test]
fn a_round_avatar_leaves_its_corners_to_the_background() {
    let visible = Rectangle::new([14.0, 14.0].into(), Size::new(72.0, 72.0));
    // Cover: the image is taller than the visible square and cropped to it.
    let cover = Rectangle::new([14.0, -6.0].into(), Size::new(72.0, 112.0));
    for scale in [1.0, 2.0] {
        for bounds in [visible, cover] {
            let (pixels, size) = render(scale, bounds);
            let case = format!("scale {scale}, bounds {bounds:?}");
            assert!(red(&pixels, size, 50.0, 50.0, scale), "{case}: centre");
            assert!(red(&pixels, size, 50.0, 15.0, scale), "{case}: top edge");
            for (x, y) in [(16.0, 16.0), (84.0, 16.0), (84.0, 84.0), (16.0, 84.0)] {
                assert!(
                    !red(&pixels, size, x, y, scale),
                    "{case}: corner ({x}, {y})"
                );
            }
        }
    }
}
