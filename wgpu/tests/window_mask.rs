// Modified by enzUI contributors. See FORK.md.
//! The window's outermost rounded group, over a transparent clear, is masked
//! in place instead of composited from a target (enzUI-ICED #3442).

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{Handle, Image, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Color, Gradient, Rectangle, Size, border, gradient};

const SIZE: u32 = 120;

fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
    .expect("real WGPU adapter required for window mask verification")
}

fn window() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

/// A rounded window whose content reaches every corner: an image and a
/// translucent gradient over it, over an optional background quad of the
/// window's shape.
fn draw(renderer: &mut Renderer, paint_before: bool, background: Option<Color>, nested: bool) {
    if paint_before {
        // Paint before the group, under the window's opaque picture: the
        // backdrop is no longer known to be the clear, so the group must be
        // composited. (Fully transparent paint is not drawn at all.)
        renderer.start_layer(window());
        let middle = window().center();
        renderer.fill_quad(
            Quad {
                bounds: Rectangle::new(middle, Size::new(1.0, 1.0)),
                ..Default::default()
            },
            Color::from_rgba(0.0, 0.0, 0.0, 1.0 / 255.0),
        );
        renderer.end_layer();
    }
    if let Some(background) = background {
        renderer.fill_quad(
            Quad {
                bounds: window(),
                border: border::rounded(16.0),
                ..Default::default()
            },
            background,
        );
    }
    renderer.start_layer_rounded(window(), border::Radius::new(16.0));
    renderer.draw_image(
        Image::new(Handle::from_rgba(1, 1, vec![200, 60, 30, 255])),
        window(),
        window(),
    );
    let tint = Color::from_rgba(0.1, 0.3, 0.9, 0.5);
    renderer.fill_quad(
        Quad {
            bounds: window(),
            ..Default::default()
        },
        Gradient::Linear(
            gradient::Linear::new(0.0)
                .add_stop(0.0, tint)
                .add_stop(1.0, tint),
        ),
    );
    if nested {
        // A title bar: its own rounded group, reaching the top corners, still
        // composited inside the window's.
        let bar = Rectangle::new([0.0, 0.0].into(), Size::new(SIZE as f32, 30.0));
        renderer.start_layer_rounded(bar, border::Radius::new(6.0));
        renderer.draw_image(
            Image::new(Handle::from_rgba(1, 1, vec![20, 160, 60, 255])),
            bar,
            bar,
        );
        renderer.fill_quad(
            Quad {
                bounds: bar,
                ..Default::default()
            },
            Color::from_rgba(1.0, 1.0, 1.0, 0.3),
        );
        renderer.end_layer();
    }
    renderer.end_layer();
}

fn render(
    paint_before: bool,
    clear: Color,
    background: Option<Color>,
    nested: bool,
) -> (Vec<u8>, usize) {
    let mut renderer = renderer();
    draw(&mut renderer, paint_before, background, nested);
    let pixels =
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(SIZE, SIZE), 1.0, clear);
    (pixels, renderer.primary_render_passes())
}

#[test]
fn a_masked_window_matches_its_composite_with_fewer_passes() {
    let grey = Color::from_rgb8(30, 28, 33);
    let paper = Color::from_rgb8(240, 240, 235);
    for (clear, background, nested) in [
        (Color::TRANSPARENT, None, false),
        (grey, None, false),
        (grey, Some(paper), false),
        (Color::TRANSPARENT, Some(paper), false),
        (grey, Some(paper), true),
    ] {
        let case = format!("clear {clear:?}, background {background:?}, nested {nested}");
        let (masked, masked_passes) = render(false, clear, background, nested);
        let (composited, composited_passes) = render(true, clear, background, nested);
        assert!(
            masked_passes < composited_passes,
            "{case}: masking in place saves the group's passes \
             ({masked_passes} vs {composited_passes})"
        );
        let at = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            let i = ((y * SIZE + x) * 4) as usize;
            pixels[i..i + 4].try_into().unwrap()
        };
        assert_eq!(at(&masked, 0, 0), at(&composited, 0, 0), "{case}: corner");
        assert_ne!(
            at(&masked, 0, 0),
            at(&masked, SIZE / 2, SIZE / 2),
            "{case}: masked"
        );
        let mut worst = 0;
        for (a, b) in masked.iter().zip(&composited) {
            worst = worst.max(a.abs_diff(*b));
        }
        assert!(
            worst <= 1,
            "{case}: masked and composited differ by up to {worst}"
        );
    }
}
