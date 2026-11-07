// Modified by enzUI contributors. See FORK.md.
//! enzICED #41 — rounded-corner clipping in the tiny_skia backend.
//!
//! The renderer's clip stack was rectangle-only end to end, so a child painted
//! inside a rounded container spilled over the corner arc. This test proves the
//! new `start_layer_rounded` path actually masks the corners on tiny_skia, by
//! rendering a full-bleed child quad inside a rounded clip layer and sampling
//! pixels.
//!
//! It is calibrated so it cannot pass for the wrong reason:
//!
//! * `radius_0_leaves_the_corner_covered` — with radius 0 the same corner
//!   pixel the rounded test checks IS the child colour. If the probe read the
//!   wrong pixel (e.g. off the child entirely), this control would fail, so a
//!   green rounded test would be meaningless.
//! * `large_radius_clips_the_corner_but_keeps_the_centre` — with a large radius
//!   that same corner pixel becomes background while the centre stays the child
//!   colour.
//! * `each_corner_radius_carves_only_its_own_corner` (#306) — every other test
//!   here uses a *uniform* radius, which is symmetric under a point mirror and
//!   therefore cannot observe a mirrored per-corner lookup. This one sets a
//!   single corner at a time. It is the reference reading that the wgpu test
//!   of the same name must match.
//!
//! **Nested clip inheritance (#325):**
//!
//! * `nested_plain_clip_inherits_rounded_parent` — a plain `start_layer` inside
//!   a rounded `start_layer_rounded` must still carve the parent's corners.
//! * `nested_plain_clip_without_rounded_parent_is_rectangular` — a plain clip
//!   with no enclosing rounded clip stays rectangular (no stale radius).
//! * `nested_plain_clip_inset_from_rounded_parent` — when the child is inset
//!   from the parent, the inherited radius shrinks correctly.

use iced_tiny_skia::Renderer;
use iced_tiny_skia::core::border;
use iced_tiny_skia::core::renderer::{Quad, Renderer as _, Scale, Settings};
use iced_tiny_skia::core::{Background, Border, Color, Rectangle, Shadow, Size};
use iced_tiny_skia::graphics::Viewport;
use iced_tiny_skia::window::compositor::screenshot;

const W: u32 = 200;
const H: u32 = 200;
/// Background: opaque black. Child: opaque magenta. Both absent from each other.
const BG: Color = Color::BLACK;
const CHILD: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Render a full-bleed magenta child clipped to the whole surface rounded by
/// `radius`, returning RGBA bytes.
fn render(radius: f32) -> Vec<u8> {
    render_radius(border::Radius::new(radius))
}

/// As [`render`], but with a full per-corner [`border::Radius`].
fn render_radius(radius: border::Radius) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(Size::new(W, H), Scale::default());

    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    // A rounded clip layer covering the whole surface, containing one quad that
    // wants to cover every pixel. Only the rounded clip can carve the corners.
    renderer.start_layer_rounded(full, radius);
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer();

    screenshot(&mut renderer, &viewport, BG)
}

fn px(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
}

fn is_child(p: (u8, u8, u8, u8)) -> bool {
    p.0 > 150 && p.1 < 100 && p.2 > 150
}

fn is_bg(p: (u8, u8, u8, u8)) -> bool {
    p.0 < 60 && p.1 < 60 && p.2 < 60
}

/// The corner pixel the tests sample: 2px in from the top-left, which is well
/// inside the cutout of a 60px-radius rounded rect but inside a square one.
const CX: u32 = 2;
const CY: u32 = 2;

#[test]
fn radius_0_leaves_the_corner_covered() {
    // CALIBRATION / positive control. Radius 0 => a plain rectangular clip; the
    // full-bleed child covers every pixel, including the corner the rounded
    // test inspects. If this is NOT the child colour, the probe samples the
    // wrong pixel and every other assertion here is worthless.
    let buf = render(0.0);
    let corner = px(&buf, CX, CY);
    let centre = px(&buf, W / 2, H / 2);
    println!("radius0 corner={corner:?} centre={centre:?}");
    assert!(
        is_child(corner),
        "calibration failed: radius-0 corner ({CX},{CY}) should be the child \
         colour, got {corner:?}"
    );
    assert!(
        is_child(centre),
        "radius-0 centre should be child, got {centre:?}"
    );
}

#[test]
fn large_radius_clips_the_corner_but_keeps_the_centre() {
    // The actual capability: a 60px radius must carve the corner cutout to
    // background while the centre stays the child colour.
    let buf = render(60.0);
    let corner = px(&buf, CX, CY);
    let centre = px(&buf, W / 2, H / 2);
    println!("radius60 corner={corner:?} centre={centre:?}");
    assert!(
        is_bg(corner),
        "rounded clip failed: corner ({CX},{CY}) should be background under a \
         60px radius, got {corner:?}"
    );
    assert!(
        is_child(centre),
        "rounded clip over-masked: centre should still be the child colour, \
         got {centre:?}"
    );
}

#[test]
fn all_four_corners_are_carved() {
    // Every corner, not just the top-left, to catch a path that rounds one
    // corner only. Sample 4px in from each corner.
    let buf = render(60.0);
    let d = 4u32;
    let corners = [
        (d, d),
        (W - 1 - d, d),
        (d, H - 1 - d),
        (W - 1 - d, H - 1 - d),
    ];
    for (x, y) in corners {
        let p = px(&buf, x, y);
        assert!(
            is_bg(p),
            "corner ({x},{y}) not carved by the 60px rounded clip: {p:?}"
        );
    }
    // And a straight-edge midpoint stays covered (the radius must not eat the
    // flat edges).
    let edge = px(&buf, W / 2, 1);
    assert!(
        is_child(edge),
        "top-edge midpoint should stay covered under a rounded clip, got {edge:?}"
    );
}

/// Counts background pixels in each corner's 60x60 block, ordered
/// `[top_left, top_right, bottom_left, bottom_right]`.
///
/// A cutout carved by a 48px radius measures a few hundred px here, and an
/// uncarved corner measures exactly 0, so the two are never confusable.
fn corner_cutouts(buf: &[u8]) -> [u32; 4] {
    const B: u32 = 60;
    let bg_at = |x: u32, y: u32| is_bg(px(buf, x, y));
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
                if bg_at(sx, sy) {
                    out[slot] += 1;
                }
            }
        }
    }

    out
}

#[test]
fn each_corner_radius_carves_only_its_own_corner() {
    // Per-corner reference reading (#306). tiny_skia builds an explicit
    // rounded-rect path, so this backend was already correct; the test exists
    // to pin that, and to be the twin of the wgpu test of the same name whose
    // SDF corner lookup WAS mirrored.
    //
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
        let cutouts = corner_cutouts(&render_radius(radius));
        println!("{name} -> cutout[tl,tr,bl,br] = {cutouts:?}");

        assert!(
            cutouts[expected] > 300,
            "radius_{name} should carve the {name} corner, but its cutout is \
             only {} px (all corners: {cutouts:?})",
            cutouts[expected]
        );

        for (slot, count) in cutouts.iter().enumerate() {
            if slot == expected {
                continue;
            }
            assert_eq!(
                *count, 0,
                "radius_{name} carved corner index {slot} as well \
                 (all corners: {cutouts:?}) — the per-corner lookup is wrong; \
                 a point-mirrored lookup shows up here as the diagonally \
                 opposite corner being the ONLY one carved"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// enzICED #325 — nested plain clip inheriting rounded parent clip
// ---------------------------------------------------------------------------

/// Render a full-bleed magenta child inside a **nested** plain clip that sits
/// inside a rounded clip of the given `radius`. The plain clip covers `inner`
/// (defaults to `full` when `None`). Returns RGBA bytes.
fn render_nested(radius: f32, inner: Option<Rectangle>) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(Size::new(W, H), Scale::default());

    let full = Rectangle::with_size(Size::new(W as f32, H as f32));
    let inner = inner.unwrap_or(full);

    // Outer: rounded clip covering the whole surface.
    renderer.start_layer_rounded(full, border::Radius::new(radius));
    // Inner: plain (rectangular) clip — this is what `Scrollable::draw` does.
    renderer.start_layer(inner);
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer(); // inner
    renderer.end_layer(); // outer

    screenshot(&mut renderer, &viewport, BG)
}

#[test]
fn nested_plain_clip_inherits_rounded_parent() {
    // A plain clip nested inside a 60px-rounded parent must still carve the
    // corners. Without enzICED #325 the plain clip would discard the parent's
    // radius, leaking pixels into the corner cutouts.
    let buf = render_nested(60.0, None);
    let corner = px(&buf, CX, CY);
    let centre = px(&buf, W / 2, H / 2);
    println!("nested corner={corner:?} centre={centre:?}");
    assert!(
        is_bg(corner),
        "nested plain clip inside rounded parent: corner ({CX},{CY}) should be \
         background, got {corner:?} — the child leaked through the parent's \
         rounded corner"
    );
    assert!(
        is_child(centre),
        "nested clip over-masked: centre should be child, got {centre:?}"
    );

    // All four corners must be carved, same as the direct rounded clip.
    let cutouts = corner_cutouts(&buf);
    println!("nested cutouts = {cutouts:?}");
    for (i, count) in cutouts.iter().enumerate() {
        assert!(
            *count > 300,
            "nested clip: corner {i} should be carved (got {count} bg px)"
        );
    }
}

#[test]
fn nested_plain_clip_without_rounded_parent_is_rectangular() {
    // A plain clip with NO enclosing rounded clip must remain rectangular,
    // including when it recycles a layer slot that previously held a rounded
    // clip. This is the recycling guard from enzICED #41.
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(Size::new(W, H), Scale::default());

    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    // Frame 1: rounded clip to "dirty" a layer slot with a radius.
    renderer.start_layer_rounded(full, border::Radius::new(60.0));
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer();
    // Capture frame 1 to force the layers to be used.
    let _ = screenshot(&mut renderer, &viewport, BG);

    // Frame 2: reset and use a plain clip in the same renderer (recycling the
    // layer slot). There is no enclosing rounded clip, so the stale radius
    // must be cleared.
    renderer.start_layer(full);
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer();

    let buf = screenshot(&mut renderer, &viewport, BG);
    let corner = px(&buf, CX, CY);
    println!("recycled plain corner={corner:?}");
    assert!(
        is_child(corner),
        "recycled plain clip without a rounded parent should NOT carve the \
         corner — stale radius leaked through, got {corner:?}"
    );
}

#[test]
fn nested_plain_clip_inset_from_rounded_parent() {
    // When the inner plain clip is inset from the parent, the inherited radius
    // shrinks. A 60px radius with a 30px inset from each edge leaves an
    // effective radius of 30px (= 60 - max(30,30)). So the corner of the
    // *child* bounds at (30,30) relative to the surface should still be carved
    // but by a smaller arc.
    let inset = 30.0;
    let inner = Rectangle {
        x: inset,
        y: inset,
        width: W as f32 - 2.0 * inset,
        height: H as f32 - 2.0 * inset,
    };
    let buf = render_nested(60.0, Some(inner));

    // The very corner of the inset child (at surface coords (30,30)) should be
    // carved. 2px into the child's bounds:
    let cx = inset as u32 + 2;
    let cy = inset as u32 + 2;
    let corner = px(&buf, cx, cy);
    let centre = px(&buf, W / 2, H / 2);
    println!("inset nested corner=({cx},{cy})={corner:?} centre={centre:?}");
    assert!(
        is_bg(corner),
        "inset nested clip: corner ({cx},{cy}) should be background under the \
         reduced inherited radius, got {corner:?}"
    );
    assert!(
        is_child(centre),
        "inset nested clip: centre should be child, got {centre:?}"
    );
}

/// enzICED #328 — the inherited-radius approximation must **never under-clip**.
///
/// `inherited_clip_radius` is deliberately conservative: it may clip away some
/// of the child's own corner (cosmetic), but no pixel outside the parent's arc
/// may ever leak through (the bug #325 exists to fix). This is the property
/// that a future "reduce the over-clip" change could silently break, so it is
/// pinned here directly rather than left to the ledger's prose.
///
/// It sweeps a grid of child insets and counts painted pixels that fall
/// outside the true parent-∩-child region. The count must be exactly zero.
///
/// Calibrated so it cannot pass vacuously: the same sweep also counts painted
/// pixels *inside* the corner region, which must be non-zero — otherwise a
/// renderer that drew nothing at all would score a perfect zero leak.
#[test]
fn nested_inherited_clip_never_under_clips() {
    const R: f32 = 60.0;

    /// Is `(x, y)` inside the parent (whole surface, rounded by `R`)?
    fn in_parent(x: f64, y: f64) -> bool {
        let (w, h) = (W as f64, H as f64);
        let r = R as f64;
        let (cx, cy) = (
            if x < r {
                r
            } else if x > w - r {
                w - r
            } else {
                x
            },
            if y < r {
                r
            } else if y > h - r {
                h - r
            } else {
                y
            },
        );
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r + 1e-9
    }

    let mut total_leak = 0usize;
    let mut total_corner_paint = 0usize;

    for dx in [0u32, 5, 12, 30, 44] {
        for dy in [0u32, 5, 12, 30, 44] {
            let inner = Rectangle {
                x: dx as f32,
                y: dy as f32,
                width: W as f32 - dx as f32,
                height: H as f32 - dy as f32,
            };
            let buf = render_nested(R, Some(inner));

            let mut leak = 0usize;
            let mut corner_paint = 0usize;
            for py in 0..H {
                for px_ in 0..W {
                    let p = px(&buf, px_, py);
                    if !is_child(p) {
                        continue;
                    }
                    // Pixel centres; a painted pixel is a leak if its centre
                    // lies outside the parent's rounded region or outside the
                    // child's rectangle.
                    let (x, y) = (px_ as f64 + 0.5, py as f64 + 0.5);
                    let inside_child = x >= dx as f64 && y >= dy as f64;
                    if !in_parent(x, y) || !inside_child {
                        leak += 1;
                    }
                    if x < R as f64 && y < R as f64 {
                        corner_paint += 1;
                    }
                }
            }
            println!("dx={dx} dy={dy} leak={leak} corner_paint={corner_paint}");
            assert_eq!(
                leak, 0,
                "under-clip at inset ({dx},{dy}): {leak} painted pixels fall \
                 outside the parent's rounded clip — the inherited-radius \
                 approximation must never under-clip"
            );
            total_leak += leak;
            total_corner_paint += corner_paint;
        }
    }

    assert_eq!(total_leak, 0, "no inset combination may leak");
    // Calibration: the child must actually be painted in the corner region for
    // the zero-leak result above to mean anything.
    assert!(
        total_corner_paint > 1000,
        "probe drew almost nothing in the corner region ({total_corner_paint} \
         px) — a zero leak count would be vacuous"
    );
}
// ---------------------------------------------------------------------------
// enzICED #319 — the rounded clip must not shrink as the scale factor rises
// ---------------------------------------------------------------------------
//
// `Renderer::draw` scales the layer bounds into physical pixels
// (`scaled_layer_bounds = layer.bounds * scale_factor`) but used to hand
// `adjust_clip_mask_rounded` the **unscaled** radius. Bounds and radius are both
// stored on the layer in logical (layout) units, so mixing them made the corner
// arc shrink as `radius / scale`: measured logical corner inset 9.00 at scale 1,
// 4.50 at 2, 2.25 at 4 — halving per doubling. Anyone on a 2x display got
// visibly squarer corners, and it made supersampling unusable for rounded-clip
// tests.
//
// **A single-scale test cannot see this.** The defect is defined entirely by the
// ratio between scales: at scale 1 the buggy and fixed code are bit-identical
// (`radius * 1.0 == radius`). So the test below SWEEPS scale 1/2/4 and asserts
// the inset measured in *logical* units stays put. `single_scale_probe_cannot_
// see_the_scale_bug` pins that fact directly, so nobody "simplifies" this back
// into a one-scale test.
//
// The reference is `iced_wgpu::clip_region_of` (`wgpu/src/lib.rs:76`), which has
// always multiplied both the rect and all four radii by `scale_factor`. Its
// measured logical insets are 9.00 / 8.50 / 8.75 for this specimen; tiny_skia
// now reads 9.00 / 9.00 / 8.75. The real contract is that the backends agree at
// EVERY scale, not just at 1.

/// Logical (layout-space) edge length of the scale-sweep specimen.
const SWEEP_LOGICAL: f32 = 100.0;
/// Logical corner radius of the scale-sweep specimen.
const SWEEP_RADIUS: f32 = 30.0;

/// Render the full-bleed child under a `SWEEP_RADIUS` rounded clip covering a
/// `SWEEP_LOGICAL` square, at the given `scale`.
///
/// The clip bounds are given in **logical** units (as a widget would), while the
/// viewport's physical size is `SWEEP_LOGICAL * scale`. That is exactly the
/// HiDPI / supersampling arrangement: same layout, more pixels.
fn render_at_scale(scale: f32) -> (Vec<u8>, u32) {
    let phys = (SWEEP_LOGICAL * scale) as u32;
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(
        Size::new(phys, phys),
        Scale {
            window: scale,
            application: 1.0,
        },
    );

    let full = Rectangle::with_size(Size::new(SWEEP_LOGICAL, SWEEP_LOGICAL));
    renderer.start_layer_rounded(full, border::Radius::new(SWEEP_RADIUS));
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer();

    (screenshot(&mut renderer, &viewport, BG), phys)
}

/// The corner arc inset in **physical** pixels: walking the top-left diagonal
/// (`y == x`), the first pixel that is the child colour. Outside the arc the
/// clip carves to background, so this distance measures the arc directly.
///
/// Physical rather than logical is the raw reading; the callers divide by the
/// scale to compare like with like.
fn diagonal_inset_physical(buf: &[u8], phys: u32) -> f32 {
    for d in 0..phys {
        let i = ((d * phys + d) * 4) as usize;
        let p = (buf[i], buf[i + 1], buf[i + 2], buf[i + 3]);
        if is_child(p) {
            return d as f32;
        }
    }
    f32::NAN
}

#[test]
fn rounded_clip_arc_holds_its_logical_size_across_scales() {
    // THE SWEEP. Scale 1, 2 and 4. Measured in logical units the corner inset
    // must stay put; the buggy code halves it at every doubling.
    //
    // Calibration first: the probe must find a real arc at scale 1, otherwise a
    // "constant" reading could just be NaN or 0 at every scale.
    let mut logical = Vec::new();
    let mut physical = Vec::new();
    for scale in [1.0f32, 2.0, 4.0] {
        let (buf, phys) = render_at_scale(scale);
        let pi = diagonal_inset_physical(&buf, phys);
        assert!(
            pi.is_finite(),
            "scale {scale}: the probe never found the child along the diagonal — \
             the clip carved everything, so no inset can be measured"
        );
        // The centre must still be painted, so a "large inset" can never come
        // from an over-masked (blank) render.
        let centre = px_at(&buf, phys, phys / 2, phys / 2);
        assert!(
            is_child(centre),
            "scale {scale}: centre should still be the child colour, got \
             {centre:?}"
        );
        println!(
            "scale={scale} phys={phys} inset_physical={pi:.2} inset_logical={:.4}",
            pi / scale
        );
        physical.push(pi);
        logical.push(pi / scale);
    }

    // Calibration: at scale 1 the arc is a real, several-pixel inset. If this is
    // ~0 the specimen is not rounded at all and the sweep proves nothing.
    assert!(
        logical[0] > 5.0,
        "calibration: at scale 1 the {SWEEP_RADIUS}px radius must produce a \
         multi-pixel corner inset, measured {:.4} logical px",
        logical[0]
    );

    // THE ASSERTION, stated as measured numbers rather than a loose tolerance.
    // Fixed code measures 9.00 / 9.00 / 8.75 logical; the bug measured
    // 9.00 / 4.50 / 2.25. A +-1.0 logical-px band around scale 1's reading
    // accepts the AA/threshold quantisation of the diagonal sample (wgpu varies
    // 9.00 / 8.50 / 8.75 for the same specimen) while rejecting the halving:
    // 4.50 is 4.5 logical px away from 9.00, four times the band.
    for (i, scale) in [1.0f32, 2.0, 4.0].iter().enumerate() {
        let drift = (logical[i] - logical[0]).abs();
        assert!(
            drift <= 1.0,
            "scale {scale}: the corner arc measured {:.4} logical px against \
             {:.4} at scale 1 (drift {drift:.4}). The radius is not being \
             scaled with the bounds — a `radius / scale` collapse (#319). All \
             logical insets: {logical:?}, physical: {physical:?}",
            logical[i],
            logical[0]
        );
    }

    // And the direct statement of the mechanism: the PHYSICAL inset must grow
    // roughly in step with the scale. The bug pinned it at a constant 9.00px at
    // every scale, which is the single most diagnostic reading in this test.
    assert!(
        physical[1] > physical[0] * 1.5,
        "the physical corner inset barely grew from scale 1 ({:.2}px) to scale \
         2 ({:.2}px). Under the #319 bug it stays constant, because the arc is \
         drawn at its unscaled size inside a scaled rectangle.",
        physical[0],
        physical[1]
    );
    assert!(
        physical[2] > physical[1] * 1.5,
        "the physical corner inset barely grew from scale 2 ({:.2}px) to scale \
         4 ({:.2}px) — see #319.",
        physical[1],
        physical[2]
    );
}

#[test]
fn single_scale_probe_cannot_see_the_scale_bug() {
    // This test exists to document WHY the sweep above must sweep. At scale 1
    // the fix is arithmetically a no-op (`radius * 1.0`), so a one-scale probe
    // passes identically against the broken and the fixed renderer. Anyone who
    // "simplifies" the sweep into a single-scale test reintroduces a check that
    // cannot fail.
    let (buf, phys) = render_at_scale(1.0);
    let pi = diagonal_inset_physical(&buf, phys);
    println!("single-scale (scale=1) inset_physical={pi:.2}");
    assert!(
        (pi - 9.0).abs() <= 1.0,
        "scale-1 inset should be ~9px for a {SWEEP_RADIUS}px radius on a \
         {SWEEP_LOGICAL}px square, got {pi:.2}. NOTE: this value is IDENTICAL \
         with and without the #319 fix — that is the point of this test. It is \
         not a guard against #319; see \
         `rounded_clip_arc_holds_its_logical_size_across_scales`."
    );
}

/// `px` hardcodes the module's `W`; the scale sweep renders at varying physical
/// sizes, so it needs a stride-aware sampler.
fn px_at(buf: &[u8], stride: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * stride + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
}

// ---------------------------------------------------------------------------
// enzUI-ICED #1442 — nested clip bounds are cumulative
// ---------------------------------------------------------------------------

fn render_cumulative_clip(outer: Option<Rectangle>, inner: Rectangle) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    let viewport = Viewport::with_physical_size(Size::new(W, H), Scale::default());
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

    if let Some(outer) = outer {
        renderer.start_layer(outer);
    }
    renderer.start_layer_rounded(inner, border::Radius::new(12.0));
    renderer.fill_quad(
        Quad {
            bounds: full,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(CHILD),
    );
    renderer.end_layer();
    if outer.is_some() {
        renderer.end_layer();
    }

    screenshot(&mut renderer, &viewport, BG)
}

#[test]
fn larger_rounded_child_cannot_replace_a_narrow_outer_clip() {
    let outer = Rectangle {
        x: 20.0,
        y: 20.0,
        width: 30.0,
        height: 100.0,
    };
    let resting_child = Rectangle {
        x: 20.0,
        y: 20.0,
        width: 140.0,
        height: 100.0,
    };

    // Inverse control: without the outer clip, the exact escaped pixel is
    // painted. This keeps a blank renderer from passing the absence assertion.
    let control = render_cumulative_clip(None, resting_child);
    assert!(
        is_child(px(&control, 100, 60)),
        "control did not paint the pixel that the outer clip must remove"
    );

    let clipped = render_cumulative_clip(Some(outer), resting_child);
    assert!(
        is_child(px(&clipped, 30, 60)),
        "nested clip over-masked the visible intersection"
    );
    assert!(
        is_bg(px(&clipped, 100, 60)),
        "larger rounded child painted outside the narrow ancestor"
    );
}

#[test]
fn empty_nested_intersection_paints_nothing() {
    let outer = Rectangle {
        x: 20.0,
        y: 20.0,
        width: 30.0,
        height: 100.0,
    };
    let disjoint_child = Rectangle {
        x: 80.0,
        y: 20.0,
        width: 80.0,
        height: 100.0,
    };
    let control = render_cumulative_clip(None, disjoint_child);
    assert!(is_child(px(&control, 100, 60)), "control must paint");

    let clipped = render_cumulative_clip(Some(outer), disjoint_child);
    assert!(
        (0..H).all(|y| (0..W).all(|x| is_bg(px(&clipped, x, y)))),
        "a disjoint nested clip became effectively unclipped"
    );
}
