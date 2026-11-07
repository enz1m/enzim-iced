// Modified by enzUI contributors. See FORK.md.
//! enzICED #306 — per-corner `border_radius` on **images**, in the wgpu backend.
//!
//! `image.wgsl`'s fragment shader has its own copy of `rounded_box_sdf`, which
//! selects a corner from the **sign** of the point it is handed. It was called
//! with the un-negated offset `2.0 * (fragment - position - scale / 2.0)`,
//! which flips both sign tests and point-mirrors the lookup (tl<->br, tr<->bl).
//! Measured before the fix: `border_radius.top_left(48)` carved the
//! **bottom-right** corner.
//!
//! There was no image-radius test at all, and a uniform radius could not have
//! caught this: a uniform radius is invariant under that mirror. So this test
//! sets **one corner at a time**.
//!
//! Requires a working wgpu adapter. If none can be acquired the tests **skip
//! with a printed reason** rather than fabricating a pixel claim.

use iced_wgpu::Renderer;
use iced_wgpu::core::border;
use iced_wgpu::core::image::{Handle, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Settings};
use iced_wgpu::core::{Color, Image, Rectangle, Size};

const W: u32 = 200;
const H: u32 = 200;
const BG: Color = Color::BLACK;

/// Draws a full-bleed opaque-magenta image with `radius`, returning RGBA bytes
/// — or `None` if no adapter is available.
fn render(radius: border::Radius) -> Option<Vec<u8>> {
    let mut renderer = pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))?;
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    // A uniformly magenta image: every carved pixel is then unambiguously the
    // radius at work and never a sample of image content.
    let pixels: Vec<u8> = (0..(W * H)).flat_map(|_| [255u8, 0, 255, 255]).collect();

    let mut image = Image::new(Handle::from_rgba(W, H, pixels));
    image.border_radius = radius;

    renderer.draw_image(image, full, full);

    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        BG,
    ))
}

fn is_bg(buf: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    buf[i] < 60 && buf[i + 1] < 60 && buf[i + 2] < 60
}

/// Counts background pixels in each corner's 60x60 block, ordered
/// `[top_left, top_right, bottom_left, bottom_right]`.
fn corner_cutouts(buf: &[u8]) -> [u32; 4] {
    const B: u32 = 60;
    let mut out = [0u32; 4];

    for y in 0..B {
        for x in 0..B {
            for (slot, (sx, sy)) in [
                (x, y),
                (W - 1 - x, y),
                (x, H - 1 - y),
                (W - 1 - x, H - 1 - y),
            ]
            .into_iter()
            .enumerate()
            {
                if is_bg(buf, sx, sy) {
                    out[slot] += 1;
                }
            }
        }
    }

    out
}

#[test]
fn radius_0_leaves_every_corner_covered() {
    // CALIBRATION. With no radius the image covers every pixel, so all four
    // corner blocks must count ZERO background. If this failed, the probe is
    // reading the wrong pixels (or the image never drew) and the per-corner
    // assertions below would be meaningless.
    let Some(buf) = render(border::Radius::default()) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let cutouts = corner_cutouts(&buf);
    println!("radius0 -> cutout[tl,tr,bl,br] = {cutouts:?}");
    assert_eq!(
        cutouts,
        [0, 0, 0, 0],
        "calibration failed: a radius-0 image should cover all four corner \
         blocks completely"
    );
}

#[test]
fn each_corner_radius_carves_only_its_own_corner() {
    // Index order below is [tl, tr, bl, br], matching `corner_cutouts`.
    let cases: [(&str, border::Radius, usize); 4] = [
        ("top_left", border::Radius::default().top_left(48.0), 0),
        ("top_right", border::Radius::default().top_right(48.0), 1),
        (
            "bottom_left",
            border::Radius::default().bottom_left(48.0),
            2,
        ),
        (
            "bottom_right",
            border::Radius::default().bottom_right(48.0),
            3,
        ),
    ];

    for (name, radius, expected) in cases {
        let Some(buf) = render(radius) else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let cutouts = corner_cutouts(&buf);
        println!("{name} -> cutout[tl,tr,bl,br] = {cutouts:?}");

        assert!(
            cutouts[expected] > 300,
            "border_radius.{name}(48) should carve the {name} corner, but its \
             cutout is only {} px (all corners: {cutouts:?})",
            cutouts[expected]
        );

        for (slot, count) in cutouts.iter().enumerate() {
            if slot == expected {
                continue;
            }
            assert_eq!(
                *count, 0,
                "border_radius.{name}(48) carved corner index {slot} as well \
                 (all corners: {cutouts:?}) — the per-corner lookup is wrong; \
                 a point-mirrored lookup shows up here as the diagonally \
                 opposite corner being the ONLY one carved"
            );
        }
    }
}
