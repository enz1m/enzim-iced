// Modified by enzUI contributors. See FORK.md.
//! enzICED #351 — SVG tint alpha must be honoured, matching `text`.
//!
//! Both SVG backends historically discarded the *tint colour's* alpha: the
//! `svg::Style::color` filter replaced R,G,B and kept the glyph's own coverage
//! as the mask, dropping `color[3]` on the floor. So an icon tinted
//! `Color { a: 0.5, .. }` rendered fully opaque while an adjacent `text()` given
//! the identical colour rendered at 50%. This test renders a fully-opaque SVG
//! shape with a translucent tint over a known background and measures the blend
//! on the *tiny_skia* backend (the cheap headless one).
//!
//! Calibrated so it cannot pass for the wrong reason:
//!
//! * `opaque_tint_is_fully_covered` — a positive control. An opaque tint over
//!   the background produces the pure tint colour at the sample point, proving
//!   the harness renders the shape at all and samples a covered pixel. If this
//!   fails, every alpha assertion below is meaningless.
//! * `half_alpha_tint_blends_halfway` — the capability: a 50%-alpha tint must
//!   land roughly halfway between the tint colour and the background. Before the
//!   fix it landed on the pure tint colour (identical to the opaque case).
//! * `alpha_scales_monotonically` — sweeps a=1.0/0.75/0.5/0.25/0.0 and asserts
//!   the sampled channel moves monotonically from tint toward background. A
//!   single point could pass by luck; the ramp cannot.
//!
//! The reported sabotage (reverting the fix) collapses the whole ramp onto the
//! opaque reading — that is the exact failure the calibration control rules out
//! as a false pass.

use iced_tiny_skia::Renderer;
use iced_tiny_skia::core::renderer::{Scale, Settings};
use iced_tiny_skia::core::svg::{Handle, Svg};
use iced_tiny_skia::core::{Color, Rectangle, Size};
use iced_tiny_skia::graphics::Viewport;
use iced_tiny_skia::window::compositor::screenshot;

const W: u32 = 100;
const H: u32 = 100;

/// Opaque green background. Distinct in every channel from the red tint below,
/// so a blend of the two is unambiguous per channel.
const BG: Color = Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

/// The tint hue (red). Its own alpha is varied per test.
fn tint(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a,
    }
}

/// A fully-opaque black square that fills its whole 100x100 viewport. Painted
/// with an explicit `fill` (not `currentColor`) — the tiny_skia colour filter
/// replaces R,G,B for every covered pixel regardless of the source hue and
/// keeps the source coverage (here 1.0 everywhere) as the mask, which is
/// exactly the code path #351 fixes. Because coverage is 1.0 across the whole
/// square, the *only* thing that can make the centre non-opaque is the tint's
/// own alpha being folded into the mask.
const SQUARE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><rect x="0" y="0" width="100" height="100" fill="#000000"/></svg>"##;

/// Render the square SVG, tinted `tint`, over `BG`, and return RGBA bytes.
fn render(tint: Color) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(Size::new(W, H), Scale::default());

    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    use iced_tiny_skia::core::svg::Renderer as _;
    renderer.draw_svg(
        Svg {
            handle: Handle::from_memory(SQUARE_SVG.to_vec()),
            color: Some(tint),
            rotation: iced_tiny_skia::core::Radians(0.0),
            opacity: 1.0,
        },
        full,
        full,
    );

    screenshot(&mut renderer, &viewport, BG)
}

fn px(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
}

const CX: u32 = W / 2;
const CY: u32 = H / 2;

#[test]
fn opaque_tint_is_fully_covered() {
    // CALIBRATION / positive control. An opaque red tint over a green
    // background must produce (near-)pure red at the centre — proving the shape
    // is rendered and the sample point is fully covered. If this is NOT red the
    // harness is broken and nothing else here means anything.
    let buf = render(tint(1.0));
    let c = px(&buf, CX, CY);
    println!("opaque tint centre = {c:?}");
    assert!(
        c.0 > 220 && c.1 < 40 && c.2 < 40,
        "calibration failed: opaque red tint should paint pure red at the \
         centre, got {c:?}"
    );
}

#[test]
fn half_alpha_tint_blends_halfway() {
    // THE CAPABILITY. A 50%-alpha red tint over green must land roughly halfway:
    // red ~128, green ~128. Before #351 the tint alpha was discarded and this
    // read pure red (255,0,0) — identical to the opaque control.
    let buf = render(tint(0.5));
    let c = px(&buf, CX, CY);
    println!("half-alpha tint centre = {c:?}");
    // Red pulled down from 255 toward the background's 0...
    assert!(
        (90..=165).contains(&(c.0 as i32)),
        "50% tint red channel should be ~128 (halfway from 255 to 0), got {}",
        c.0
    );
    // ...and green pulled up from 0 toward the background's 255.
    assert!(
        (90..=165).contains(&(c.1 as i32)),
        "50% tint green channel should be ~128 (background bleeding through), \
         got {}",
        c.1
    );
    // And it is distinctly NOT the opaque reading — the exact pre-fix bug.
    assert!(
        c.0 < 220 || c.1 > 40,
        "50% tint rendered as an opaque red {c:?} — the tint alpha was \
         discarded (#351)"
    );
}

#[test]
fn alpha_scales_monotonically() {
    // THE RAMP. As tint alpha falls, the red channel must fall monotonically
    // (tint fading out) and the green channel must rise monotonically
    // (background bleeding through). A single sample can pass by luck; a
    // monotone ramp across five alphas cannot.
    let alphas = [1.0f32, 0.75, 0.5, 0.25, 0.0];
    let mut reds = Vec::new();
    let mut greens = Vec::new();
    for a in alphas {
        let c = px(&render(tint(a)), CX, CY);
        println!("alpha={a} centre={c:?}");
        reds.push(c.0 as i32);
        greens.push(c.1 as i32);
    }

    // Endpoints: fully opaque ~ pure red; fully transparent ~ pure background.
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

    // Monotone, unconditionally, index by index (a flat ramp — the pre-fix
    // behaviour where every alpha reads pure red — fails the strict
    // inequalities here).
    for i in 1..alphas.len() {
        assert!(
            reds[i] < reds[i - 1],
            "red channel not monotonically decreasing across the alpha ramp: \
             {reds:?} at alphas {alphas:?}"
        );
        assert!(
            greens[i] > greens[i - 1],
            "green channel not monotonically increasing across the alpha ramp: \
             {greens:?} at alphas {alphas:?}"
        );
    }
}
