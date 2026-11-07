// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #3244: a material composite draws in the frame's own pass.
//!
//! Each boundary ends the frame's pass to copy its backdrop and reopens it
//! afterwards. Its composites are the first paint of the reopened pass rather
//! than a pass of their own, which on anv is two command-buffer begins saved
//! per glass panel.

use iced_wgpu::Renderer;
use iced_wgpu::core::material::Material;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Color, Rectangle, Size};

const W: u32 = 200;
const H: u32 = 100;

fn glass() -> Material {
    Material {
        radii: [12.0; 4],
        blur_sigma: 6.0,
        refraction: 1.0,
        bevel_width: 10.0,
        index_of_refraction: 1.5,
        dispersion: 0.0,
        color_diffusion: 0.0,
        color_reach: 0.0,
        optical_depth: 10.0,
        tint: Color::WHITE,
        tint_opacity: 0.5,
        saturation: 1.0,
        highlight: 0.0,
        brightness: 0.0,
        light_angle: 225.0,
        profile_curvature: 2.5,
        contrast_protection: 0.0,
        protected_inset: 0.0,
        probe: 0,
        measure_only: false,
    }
}

fn panel(x: f32) -> Rectangle {
    Rectangle::new([x, 20.0].into(), Size::new(70.0, 60.0))
}

fn pixel(buf: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2]]
}

/// Two glass panels, each its own boundary, spend only the scene clear, two
/// filter passes each and the blit, and both still composite.
///
/// Sabotage: drop the `draw_composites` call after a boundary and the panels
/// stay the bare backdrop.
#[test]
fn glass_composites_spend_no_pass_of_their_own() {
    let Some(mut renderer) = pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    )) else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    renderer.fill_quad(
        Quad {
            bounds: full,
            ..Quad::default()
        },
        Color::from_rgb(0.0, 0.0, 0.6),
    );

    for x in [10.0, 110.0] {
        renderer.start_layer_material(panel(x), glass());
        renderer.end_layer();
        // Paint between the panels keeps them in separate boundaries.
        renderer.fill_quad(
            Quad {
                bounds: Rectangle::new([x + 75.0, 0.0].into(), Size::new(10.0, 10.0)),
                ..Quad::default()
            },
            Color::from_rgb(0.0, 0.6, 0.0),
        );
    }

    let buf = <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);
    let statistics = renderer.material_statistics();

    assert_eq!(statistics.boundaries, 2, "{statistics:?}");
    assert_eq!(statistics.composites, 2, "{statistics:?}");
    assert_eq!(statistics.render_passes, 1 + 2 + 2 + 1, "{statistics:?}");

    let backdrop = pixel(&buf, 100, 50);
    for x in [45, 145] {
        let glass = pixel(&buf, x, 50);
        assert!(
            glass[0] > backdrop[0] + 40,
            "the panel at {x} did not composite: {glass:?} over {backdrop:?}",
        );
    }
}
