// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #3244: layers with images share the frame's render pass.
//!
//! wgpu-core begins two command buffers per render pass, and anv clears a
//! large state block on each begin, so a pass per image layer showed up as a
//! memset in every animated frame. Images draw into the open pass; only a
//! change of target needs a new one.

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{Handle, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Color, Image, Rectangle, Size};

const W: u32 = 120;
const H: u32 = 40;

fn solid(color: [u8; 4]) -> Handle {
    Handle::from_rgba(4, 4, color.repeat(16))
}

fn cell(index: u32) -> Rectangle {
    Rectangle::new([index as f32 * 20.0, 0.0].into(), Size::new(20.0, H as f32))
}

fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2]]
}

/// Six clip layers alternating image and quad, each painting over the one
/// before it, render in one pass and keep their order.
///
/// Sabotage: split the pass again for every layer with images and the count
/// reads 4.
#[test]
fn image_layers_share_the_frame_pass_in_order() {
    let Some(mut renderer) = pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    )) else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    for index in 0..6 {
        renderer.start_layer(full);
        // Each layer covers its own cell and the next one, so the next layer
        // shows on top only if it drew later.
        let bounds = Rectangle {
            width: 40.0,
            ..cell(index)
        };
        if index % 2 == 0 {
            renderer.draw_image(Image::new(solid([255, 0, 0, 255])), bounds, full);
        } else {
            renderer.fill_quad(
                Quad {
                    bounds,
                    ..Quad::default()
                },
                Color::from_rgb(0.0, 0.0, 1.0),
            );
        }
        renderer.end_layer();
    }

    let buf = <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);

    assert_eq!(renderer.primary_render_passes(), 1);

    for index in 0..6 {
        let [r, _, b] = pixel(&buf, index * 20 + 10, H / 2);
        let expected_red = index % 2 == 0;
        assert!(
            (r > 200) == expected_red && (b > 200) != expected_red,
            "cell {index} shows the wrong layer on top: rgb {r}/{b}",
        );
    }
}
