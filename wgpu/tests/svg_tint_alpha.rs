// Modified by enzUI contributors. See FORK.md.
//! enzICED #351 — SVG tint alpha must be honoured in the **wgpu** backend too.
//!
//! Twin of `tiny_skia/tests/svg_tint_alpha.rs`. The wgpu vector atlas path
//! (`wgpu/src/image/vector.rs`) historically copied `color[0..3]` and never
//! read `color[3]`, so a translucent `.icon_color()` rendered fully opaque
//! while an adjacent `text()` with the same colour rendered translucent. The
//! fix folds the tint alpha into the coverage mask (`rgba[3] = rgba[3] *
//! color[3] / 255`), exactly as the tiny_skia backend does — the two backends
//! must agree.
//!
//! Requires a working wgpu adapter. If none can be acquired here the tests
//! **skip with a printed reason** rather than fabricating a pixel claim
//! (matching `wgpu/tests/rounded_clip.rs`). The cheaper tiny_skia twin is the
//! primary, always-run proof; this exists so the wgpu arithmetic is exercised
//! wherever an adapter is present.
//!
//! Calibrated like its twin: `opaque_tint_is_fully_covered` is a positive
//! control (opaque tint => pure tint colour), `half_alpha_tint_blends_halfway`
//! is the capability, `alpha_scales_monotonically` sweeps the ramp so a lucky
//! single sample cannot pass.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Settings};
use iced_wgpu::core::svg::{Handle, Renderer as _, Svg};
use iced_wgpu::core::{Color, Radians, Rectangle, Size};

const W: u32 = 100;
const H: u32 = 100;

/// Opaque green background, distinct in every channel from the red tint.
const BG: Color = Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

fn tint(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a,
    }
}

/// A fully-opaque black square filling the whole 100x100 viewport. See the
/// tiny_skia twin for why an opaque, full-coverage shape isolates the tint
/// alpha as the only thing that can make the centre non-opaque.
const SQUARE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><rect x="0" y="0" width="100" height="100" fill="#000000"/></svg>"##;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

/// Render the square SVG, tinted `tint`, over `BG`. `None` if no adapter.
fn render(tint: Color) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    renderer.draw_svg(
        Svg {
            handle: Handle::from_memory(SQUARE_SVG.to_vec()),
            color: Some(tint),
            rotation: Radians(0.0),
            opacity: 1.0,
        },
        full,
        full,
    );

    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        BG,
    ))
}

fn px(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
}

const CX: u32 = W / 2;
const CY: u32 = H / 2;

#[test]
fn opaque_tint_is_fully_covered() {
    let Some(buf) = render(tint(1.0)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let c = px(&buf, CX, CY);
    println!("wgpu opaque tint centre = {c:?}");
    assert!(
        c.0 > 220 && c.1 < 40 && c.2 < 40,
        "calibration failed: opaque red tint should paint pure red, got {c:?}"
    );
}

#[test]
fn half_alpha_tint_blends_halfway() {
    let Some(buf) = render(tint(0.5)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let c = px(&buf, CX, CY);
    println!("wgpu half-alpha tint centre = {c:?}");
    // wgpu blends in LINEAR space then encodes to sRGB, so a 50% coverage lands
    // near sRGB ~188, not the naive-sRGB ~128 the software (tiny_skia) path
    // gives. Both are a genuine ~half blend; the band below spans both encodings
    // and only excludes the opaque (255) and fully-transparent (0) readings.
    // The definitive proof of "alpha is honoured" is the monotone ramp test.
    assert!(
        (100..=220).contains(&(c.0 as i32)),
        "50% tint red channel should be a partial blend (not opaque 255), got {}",
        c.0
    );
    assert!(
        (60..=220).contains(&(c.1 as i32)),
        "50% tint green channel should show background bleed (not 0), got {}",
        c.1
    );
    assert!(
        c.0 < 240 && c.1 > 30,
        "50% tint rendered as opaque red {c:?} — tint alpha discarded (#351)"
    );
}

#[test]
fn alpha_scales_monotonically() {
    let alphas = [1.0f32, 0.75, 0.5, 0.25, 0.0];
    let mut reds = Vec::new();
    let mut greens = Vec::new();
    for a in alphas {
        let Some(buf) = render(tint(a)) else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };
        let c = px(&buf, CX, CY);
        println!("wgpu alpha={a} centre={c:?}");
        reds.push(c.0 as i32);
        greens.push(c.1 as i32);
    }

    assert!(
        reds[0] > 220 && greens[0] < 40,
        "alpha=1.0 should be pure red, got r={} g={}",
        reds[0],
        greens[0]
    );
    assert!(
        reds[4] < 40 && greens[4] > 220,
        "alpha=0.0 should be pure background green, got r={} g={}",
        reds[4],
        greens[4]
    );
    for i in 1..alphas.len() {
        assert!(
            reds[i] < reds[i - 1],
            "wgpu red channel not monotonically decreasing: {reds:?} at {alphas:?}"
        );
        assert!(
            greens[i] > greens[i - 1],
            "wgpu green channel not monotonically increasing: {greens:?} at {alphas:?}"
        );
    }
}
