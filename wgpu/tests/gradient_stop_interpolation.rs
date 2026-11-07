// Modified by enzUI contributors. See FORK.md.
//! enzICED #78.1 — a gradient must interpolate its stops **linearly**.
//!
//! Both wgpu gradient shaders computed the blend factor between two adjacent
//! colour stops with `smoothstep(curr_offset, next_offset, coord_offset)`
//! (`wgpu/src/shader/quad/gradient.wgsl:115`,
//! `wgpu/src/shader/triangle/gradient.wgsl:74`). `smoothstep` is the cubic
//! ease `3t^2 - 2t^3`, whose derivative is **zero at both ends**, so every
//! colour stop sits at the bottom of a flat spot and the colour distribution
//! between stops is wrong everywhere except the points where the two curves
//! happen to cross. A linear ramp `(x - a) / (b - a)` is what a linear gradient
//! means, and what every other renderer (tiny_skia here, CSS, SVG, Cairo,
//! Skia) produces.
//!
//! # Where to sample — this is the whole difficulty
//!
//! `smoothstep` and `linear` **agree exactly** at local `t` = 0, 0.5 and 1:
//! `3(0.5)^2 - 2(0.5)^3 = 0.5`. So the obvious probe — sample the middle of
//! the gradient — reads the same value against the broken shader *and* the
//! fixed one, and passes either way. That is not a tolerance problem a tighter
//! band can fix; the two functions are genuinely equal there.
//!
//! The generalisation is not obvious and it caught the design that preceded
//! this file: **the crossing is at the local `t` of the enclosing segment, not
//! at the global gradient offset.** For a 3-stop gradient with stops at 0, 0.5
//! and 1, sampling global offset **0.25** lands at local `t = 0.5` *within the
//! first segment* — the exact blind spot, merely relocated. Adding stops does
//! not escape it; only moving off the local midpoint does. Every sample here is
//! taken at a local `t` of 0.25 or 0.75, where the separation is near-maximal
//! (0.25 vs 0.15625, i.e. 0.094 of full scale).
//!
//! # The instrument: assert on the decoded blend factor, not on bytes
//!
//! This is the part that had to be calibrated rather than assumed, and the
//! first version of this file got it wrong. The gradient shader blends in
//! **linear** RGB, and the headless surface then encodes to **sRGB**, so the
//! byte in the screenshot is `srgb_encode(factor) * 255`, not `factor * 255`.
//! The two are far apart: factor 0.25 is byte **137**, not 64. Predicting
//! bytes directly put the expected bands ~70 counts away from any real
//! reading, which would have "failed against the unfixed shader" for entirely
//! the wrong reason and then failed against the fixed one too.
//!
//! So [`factor_at`] decodes sRGB back to linear and recovers the blend factor,
//! and every assertion is stated in factor space where the arithmetic is the
//! shader's own. [`the_probe_recovers_a_known_factor`] pins that the decode is
//! right by measuring the *ends*, whose factors are known to be exactly 0 and
//! 1 regardless of which interpolation is in use.
//!
//! Requires a working wgpu adapter. With none present the tests **skip with a
//! printed reason** rather than fabricating a pixel claim — the same discipline
//! as `wgpu/tests/rounded_clip.rs` and `wgpu/tests/svg_tint_alpha.rs`.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{
    Background, Border, Color, Degrees, Gradient, Rectangle, Shadow, Size, gradient,
};
use iced_wgpu::graphics::mesh::{self, GradientVertex2D, Mesh, Renderer as _};

const W: u32 = 200;
const H: u32 = 40;

const BG: Color = Color::BLACK;
const RED: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BLUE: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};
const GREEN: Color = Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

/// `smoothstep`'s cubic ease, for stating in the assertions what the *wrong*
/// answer would have been. Kept as the real formula rather than a magic
/// number so the expectations cannot drift from the thing being excluded.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// sRGB -> linear, the inverse of what the surface applies on write.
fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

/// Fill the whole surface with `gradient` running left→right, returning RGBA
/// bytes — or `None` when no adapter is available.
fn render(gradient: gradient::Linear) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Gradient(Gradient::Linear(gradient)),
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

/// A left→right ramp. 90 degrees is +x in iced's gradient convention, which
/// [`the_probe_recovers_a_known_factor`] verifies rather than assumes.
fn horizontal() -> gradient::Linear {
    gradient::Linear::new(Degrees(90.0))
}

/// The mean sRGB byte of channel `ch` in column `x`, averaged over 5 rows so
/// the shader's ±0.3/255 dither (`noise_granularity`) cannot move a single
/// reading across an assertion band.
fn channel(buf: &[u8], x: u32, ch: usize) -> f32 {
    let mut sum = 0.0;
    for y in (H / 2 - 2)..(H / 2 + 3) {
        let p = px(buf, x, y);
        sum += [p.0, p.1, p.2][ch] as f32;
    }
    sum / 5.0
}

/// Recover the **blend factor** at column `x` from the channel that ramps
/// 0→1 across the segment (the destination colour's channel).
///
/// Decodes sRGB back to linear first: the shader mixes in linear space and the
/// surface encodes on write, so the raw byte is `encode(factor)`, roughly 137
/// for a factor of 0.25. Asserting on the raw byte is what makes a gradient
/// test read plausible and mean nothing.
fn factor_at(buf: &[u8], x: u32, dest_channel: usize) -> f32 {
    srgb_to_linear(channel(buf, x, dest_channel) / 255.0)
}

// ---------------------------------------------------------------------------
// Calibration
// ---------------------------------------------------------------------------

/// **Positive control.** The probe reports a factor it already knows the
/// answer to, at the two points where *every* interpolation curve agrees: the
/// endpoints, which are exactly 0 and 1 by definition.
///
/// This pins three things at once, all of which the interpolation tests below
/// silently depend on: the ramp really runs left→right (so a column index means
/// an offset), the sRGB decode is the right way round (an inverted decode would
/// report 1.0 where 0.0 belongs), and the surface is not applying some further
/// transform. Without it, a wrong-but-self-consistent instrument would produce
/// confident numbers about the wrong thing.
#[test]
fn the_probe_recovers_a_known_factor() {
    let Some(buf) = render(horizontal().add_stop(0.0, RED).add_stop(1.0, BLUE)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let start = factor_at(&buf, 1, 2);
    let end = factor_at(&buf, W - 2, 2);
    println!("calibration: factor at left edge = {start:.4}, right edge = {end:.4}");
    assert!(
        start < 0.02,
        "the left edge is the first stop, factor 0; probe says {start:.4} -- \
         the ramp is not left->right, or the sRGB decode is inverted"
    );
    assert!(
        end > 0.98,
        "the right edge is the last stop, factor 1; probe says {end:.4}"
    );
}

// ---------------------------------------------------------------------------
// The capability
// ---------------------------------------------------------------------------

/// **The discriminating sample.** A two-stop red→blue ramp read at 25% across.
///
/// * linear     -> factor 0.25
/// * smoothstep -> factor 0.15625
///
/// The band accepts one and excludes the other with ~0.05 of margin on each
/// side. Sampling at 50% instead would read 0.5 under both and prove nothing.
#[test]
fn two_stop_ramp_interpolates_linearly_at_the_quarter_point() {
    let Some(buf) = render(horizontal().add_stop(0.0, RED).add_stop(1.0, BLUE)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let f = factor_at(&buf, W / 4, 2);
    println!(
        "red->blue at 25%: factor {f:.4}  (linear 0.25, smoothstep {:.4})",
        smoothstep(0.25)
    );
    assert!(
        (f - 0.25).abs() < 0.04,
        "the blend factor 25% along must be 0.25 (linear); got {f:.4}. \
         {:.4} is smoothstep -- the shader is still easing between stops",
        smoothstep(0.25)
    );
}

/// The mirror sample at 75%, separating the two curves in the **opposite**
/// direction (linear 0.75 vs smoothstep 0.84375).
///
/// Not redundant with the 25% test: an implementation that merely offset the
/// factor would satisfy one and fail the other, and a symmetric error is
/// exactly what a single-sided probe cannot see.
#[test]
fn two_stop_ramp_interpolates_linearly_at_the_three_quarter_point() {
    let Some(buf) = render(horizontal().add_stop(0.0, RED).add_stop(1.0, BLUE)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let f = factor_at(&buf, 3 * W / 4, 2);
    println!(
        "red->blue at 75%: factor {f:.4}  (linear 0.75, smoothstep {:.4})",
        smoothstep(0.75)
    );
    assert!(
        (f - 0.75).abs() < 0.04,
        "the blend factor 75% along must be 0.75 (linear); got {f:.4}. \
         {:.4} is smoothstep",
        smoothstep(0.75)
    );
}

/// The **multi-stop** case, which is what #78.1 is actually about: with three
/// stops there is a flat spot at *each interior* stop, not only at the ends.
///
/// Sampled at global offset **0.125**, i.e. local `t = 0.25` inside the first
/// segment `[0.0, 0.5]`. Global 0.25 would be local `t = 0.5` — the exact point
/// where the two curves are equal — so this test would pass against the broken
/// shader if it sampled there. That is the trap documented at the top of this
/// file.
#[test]
fn three_stop_gradient_interpolates_linearly_within_its_first_segment() {
    let Some(buf) = render(
        horizontal()
            .add_stop(0.0, RED)
            .add_stop(0.5, GREEN)
            .add_stop(1.0, BLUE),
    ) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    // Global 0.125 of 200px = x 25. Local t within [0.0, 0.5] is 0.25.
    // The destination colour of this segment is GREEN, so read channel 1.
    let f = factor_at(&buf, W / 8, 1);
    println!(
        "red->green->blue at 12.5% (local t=0.25): factor {f:.4}  \
         (linear 0.25, smoothstep {:.4})",
        smoothstep(0.25)
    );
    assert!(
        (f - 0.25).abs() < 0.04,
        "within the first segment the factor at local t=0.25 must be 0.25 \
         (linear); got {f:.4}, and {:.4} is smoothstep",
        smoothstep(0.25)
    );

    // And the interior stop itself is still exactly its own colour -- a linear
    // ramp must not move where the stops ARE, only how it travels between them.
    let mid = (
        channel(&buf, W / 2, 0),
        channel(&buf, W / 2, 1),
        channel(&buf, W / 2, 2),
    );
    println!("the 0.5 stop reads {mid:?}");
    assert!(
        mid.1 > 235.0 && mid.0 < 25.0 && mid.2 < 25.0,
        "the 0.5 stop must render as its own colour (green), got {mid:?}"
    );
}

/// A **uniformity** check across the ramp: a linear interpolation has a
/// constant slope, so equally-spaced samples must be equally spaced in factor.
///
/// This is the assertion that no easing curve can satisfy, rather than one that
/// merely excludes a particular sample value. `smoothstep` is flat at the stops
/// and steep in the middle, so its successive factor differences vary by more
/// than 2x across the ramp; a linear ramp's are equal to within the dither.
#[test]
fn the_ramp_has_a_constant_slope() {
    let Some(buf) = render(horizontal().add_stop(0.0, RED).add_stop(1.0, BLUE)) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let xs = [20u32, 60, 100, 140, 180];
    let factors: Vec<f32> = xs.iter().map(|&x| factor_at(&buf, x, 2)).collect();
    let deltas: Vec<f32> = factors.windows(2).map(|w| w[1] - w[0]).collect();
    println!("factors = {factors:?}\ndeltas  = {deltas:?}");

    // Calibration: the ramp must actually be ramping, or "all deltas equal" is
    // trivially true for a flat image.
    assert!(
        deltas.iter().all(|d| *d > 0.05),
        "calibration: the factor must rise across the ramp, got {deltas:?}"
    );

    let min = deltas.iter().copied().fold(f32::INFINITY, f32::min);
    let max = deltas.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    // Linear: every delta is 0.2 +/- dither. smoothstep: the middle deltas are
    // ~2x the outer ones, so the spread is ~0.15 and this fails.
    assert!(
        max - min < 0.03,
        "a linear interpolation has a constant slope; deltas {deltas:?} span \
         {:.4}, which is the signature of an easing curve (smoothstep is flat \
         at the stops and steep in the middle)",
        max - min
    );
}

// ---------------------------------------------------------------------------
// The mesh (triangle) pipeline
// ---------------------------------------------------------------------------
//
// Everything above renders a gradient **quad**, which exercises only
// `shader/quad/gradient.wgsl`. `shader/triangle/gradient.wgsl` carries a
// byte-identical copy of the same interpolation loop and was fixed in the same
// commit, but it is reached by a different pipeline: a gradient `Mesh`, not a
// quad background. Without the test below, half the change would be asserted
// by nothing at all -- "I edited both files" is not evidence that both work,
// and the second file is exactly where a copy-paste fix silently fails to
// apply.

/// A full-bleed gradient mesh (two triangles) running left→right across the
/// whole surface, with the given stops.
fn gradient_mesh(gradient: iced_wgpu::graphics::gradient::Linear) -> Mesh {
    let packed = gradient.pack();
    let (w, h) = (W as f32, H as f32);
    let vertex = |x: f32, y: f32| GradientVertex2D {
        position: [x, y],
        gradient: packed,
    };

    Mesh::Gradient {
        buffers: mesh::Indexed {
            vertices: vec![
                vertex(0.0, 0.0),
                vertex(w, 0.0),
                vertex(w, h),
                vertex(0.0, h),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        transformation: iced_wgpu::core::Transformation::IDENTITY,
        clip_bounds: Rectangle::with_size(Size::new(w, h)),
    }
}

/// Render a red→blue gradient **mesh** spanning the surface left→right.
fn render_mesh() -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;

    renderer.draw_mesh(gradient_mesh(
        iced_wgpu::graphics::gradient::Linear::new(
            iced_wgpu::core::Point::new(0.0, 0.0),
            iced_wgpu::core::Point::new(W as f32, 0.0),
        )
        .add_stop(0.0, RED)
        .add_stop(1.0, BLUE),
    ));

    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        BG,
    ))
}

/// **Calibration for the mesh path.** Same endpoint check as
/// [`the_probe_recovers_a_known_factor`], but through the triangle pipeline:
/// if the mesh did not draw at all, every pixel would be background and the
/// interpolation test below would be reading the clear colour while looking
/// authoritative.
#[test]
fn the_mesh_probe_recovers_a_known_factor() {
    let Some(buf) = render_mesh() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let start = factor_at(&buf, 1, 2);
    let end = factor_at(&buf, W - 2, 2);
    println!("mesh calibration: factor at left edge = {start:.4}, right edge = {end:.4}");
    assert!(
        start < 0.03,
        "the mesh gradient's left edge is the first stop, factor 0; probe says \
         {start:.4} -- the mesh may not have drawn at all"
    );
    assert!(
        end > 0.97,
        "the mesh gradient's right edge is the last stop, factor 1; probe says {end:.4}"
    );
}

/// The discriminating sample through the **triangle** shader: same 25% probe,
/// same exclusion of smoothstep, different pipeline. This is the only
/// assertion in the file that can observe `shader/triangle/gradient.wgsl`.
#[test]
fn gradient_mesh_interpolates_linearly_at_the_quarter_point() {
    let Some(buf) = render_mesh() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let f = factor_at(&buf, W / 4, 2);
    println!(
        "mesh red->blue at 25%: factor {f:.4}  (linear 0.25, smoothstep {:.4})",
        smoothstep(0.25)
    );
    assert!(
        (f - 0.25).abs() < 0.04,
        "the mesh blend factor 25% along must be 0.25 (linear); got {f:.4}. \
         {:.4} is smoothstep -- the TRIANGLE shader is still easing",
        smoothstep(0.25)
    );
}

/// And the mirror sample at 75% through the mesh path.
#[test]
fn gradient_mesh_interpolates_linearly_at_the_three_quarter_point() {
    let Some(buf) = render_mesh() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let f = factor_at(&buf, 3 * W / 4, 2);
    println!(
        "mesh red->blue at 75%: factor {f:.4}  (linear 0.75, smoothstep {:.4})",
        smoothstep(0.75)
    );
    assert!(
        (f - 0.75).abs() < 0.04,
        "the mesh blend factor 75% along must be 0.75 (linear); got {f:.4}. \
         {:.4} is smoothstep",
        smoothstep(0.75)
    );
}

/// A **degenerate zero-width segment**: two stops at the same offset, which is
/// a hard colour break rather than a ramp and which makes the new
/// `(x - a) / (b - a)` divide by zero.
///
/// `smoothstep(a, a, x)` was total; a bare division is not, and an
/// `inf`/`NaN` factor propagating through `mix` would paint garbage. Asserted
/// as "both sides are their own stop colour, and no column is unlit".
///
/// Measured caveat, recorded rather than glossed: this passes **with and
/// without** the `saturate` in the fix on this adapter, so it does not prove
/// that call is load-bearing here -- the hardware already resolves the 0/0 to
/// something `mix` tolerates. What it does prove is that the fix did not
/// introduce a garbage-rendering regression in the degenerate case, which is
/// the risk a division carries and `smoothstep` did not.
#[test]
fn a_zero_width_segment_does_not_produce_garbage() {
    // NOTE: `add_stop` binary-searches by offset and *replaces* an existing
    // stop at the same offset, so the degenerate case cannot be built through
    // the builder -- `.add_stop(0.5, RED).add_stop(0.5, BLUE)` yields a single
    // blue stop at 0.5. (Measured: it renders as an ordinary 3-stop ramp.) The
    // `stops` array is a public field, so set it directly to reach the shader
    // path that actually divides by zero.
    let mut hard_break = horizontal();
    hard_break.stops[0] = Some(gradient::ColorStop {
        offset: 0.0,
        color: RED,
    });
    hard_break.stops[1] = Some(gradient::ColorStop {
        offset: 0.5,
        color: RED,
    });
    hard_break.stops[2] = Some(gradient::ColorStop {
        offset: 0.5,
        color: BLUE,
    });
    hard_break.stops[3] = Some(gradient::ColorStop {
        offset: 1.0,
        color: BLUE,
    });

    let Some(buf) = render(hard_break) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let left = (channel(&buf, W / 4, 0), channel(&buf, W / 4, 2));
    let right = (channel(&buf, 3 * W / 4, 0), channel(&buf, 3 * W / 4, 2));
    println!("hard break: left(r,b) = {left:?}  right(r,b) = {right:?}");
    assert!(
        left.0 > 240.0 && left.1 < 20.0,
        "left of a hard colour break must be solid red, got {left:?}"
    );
    assert!(
        right.1 > 240.0 && right.0 < 20.0,
        "right of a hard colour break must be solid blue, got {right:?}"
    );
    // No pixel anywhere may be black: a NaN factor collapses `mix` and would
    // show up as an unlit column that neither stop colour explains.
    for x in 0..W {
        let (r, g, b) = (
            channel(&buf, x, 0),
            channel(&buf, x, 1),
            channel(&buf, x, 2),
        );
        assert!(
            r + g + b > 40.0,
            "column {x} is unlit ({r},{g},{b}) -- a zero-width segment produced \
             a NaN/inf factor"
        );
    }
}
