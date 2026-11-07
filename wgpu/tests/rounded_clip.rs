// Modified by enzUI contributors. See FORK.md.
//! enzICED #41 stage 2 — rounded-corner clipping in the **wgpu** backend.
//!
//! Stage 1 (`5e2133cf`) implemented `start_layer_rounded` for tiny_skia only;
//! wgpu was a documented rectangular fallback. This test proves the wgpu SDF
//! path actually masks the corners, mirroring `tiny_skia/tests/rounded_clip.rs`.
//!
//! A full-bleed magenta child quad is drawn inside a rounded clip layer and
//! pixels are sampled. Calibrated so it cannot pass for the wrong reason:
//!
//! * `radius_0_leaves_the_corner_covered` — radius 0 => the same corner pixel
//!   the rounded test checks IS the child colour. If the probe read the wrong
//!   pixel, this control fails, so a green rounded test would be meaningless.
//! * `large_radius_clips_all_four_corners_but_keeps_the_centre` — a 60px radius
//!   carves every corner to background while the centre stays the child colour.
//!   All four corners are asserted independently (a one-corner bug is common).
//! * `each_corner_radius_carves_only_its_own_corner` (#306) — the above are all
//!   *uniform* radii, and a uniform radius is symmetric under a point mirror,
//!   so they pass identically against a shader whose per-corner lookup is
//!   mirrored (tl<->br, tr<->bl). This test sets **one corner at a time** and
//!   is the only assertion here that can observe that axis.
//!
//! Requires a working wgpu adapter. If none can be acquired in this environment
//! the tests **skip with a printed reason** rather than failing or fabricating
//! a result — an unverified pixel claim here would wrongly unblock #80.

use iced_wgpu::Renderer;
use iced_wgpu::core::border;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Background, Border, Color, Rectangle, Shadow, Size};

const W: u32 = 200;
const H: u32 = 200;
const BG: Color = Color::BLACK;
const CHILD: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Builds a headless wgpu renderer, or `None` if no adapter is available.
fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

/// Draw a full-bleed magenta child clipped to the whole surface rounded by
/// `radius`, returning RGBA bytes — or `None` if no adapter.
fn render(radius: f32) -> Option<Vec<u8>> {
    render_radius(border::Radius::new(radius))
}

/// As [`render`], but with a full per-corner [`border::Radius`].
fn render_radius(radius: border::Radius) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
    let full = Rectangle::with_size(Size::new(W as f32, H as f32));

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

fn is_child(p: (u8, u8, u8, u8)) -> bool {
    p.0 > 150 && p.1 < 100 && p.2 > 150
}

fn is_bg(p: (u8, u8, u8, u8)) -> bool {
    p.0 < 60 && p.1 < 60 && p.2 < 60
}

const CX: u32 = 2;
const CY: u32 = 2;

#[test]
fn radius_0_leaves_the_corner_covered() {
    let Some(buf) = render(0.0) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
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
fn large_radius_clips_all_four_corners_but_keeps_the_centre() {
    let Some(buf) = render(60.0) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let centre = px(&buf, W / 2, H / 2);
    println!("radius60 centre={centre:?}");
    assert!(
        is_child(centre),
        "rounded clip over-masked: centre should still be the child colour, \
         got {centre:?}"
    );

    // Every corner independently, 4px in, to catch a single-corner bug.
    let d = 4u32;
    for (x, y) in [
        (d, d),
        (W - 1 - d, d),
        (d, H - 1 - d),
        (W - 1 - d, H - 1 - d),
    ] {
        let p = px(&buf, x, y);
        println!("radius60 corner ({x},{y})={p:?}");
        assert!(
            is_bg(p),
            "corner ({x},{y}) not carved by the 60px rounded clip: {p:?}"
        );
    }

    // Straight-edge midpoints must stay covered (radius must not eat flat edges).
    let top = px(&buf, W / 2, 1);
    let left = px(&buf, 1, H / 2);
    assert!(
        is_child(top) && is_child(left),
        "a straight edge was clipped: top={top:?} left={left:?}"
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
    // THE per-corner test (#306). `rounded_box_sdf` picks its corner from the
    // sign of the point it is handed, so handing it an un-negated offset
    // point-mirrors the lookup: `radius_top_left` carves BOTTOM-RIGHT. Every
    // other test in this file uses a uniform radius, which is invariant under
    // that mirror, so this is the only one that can fail on it.
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
        let Some(buf) = render_radius(radius) else {
            eprintln!("SKIP: no wgpu adapter available in this environment");
            return;
        };

        let cutouts = corner_cutouts(&buf);
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
// enzICED #319 — scale sweep, and the cross-backend contract
// ---------------------------------------------------------------------------
//
// tiny_skia scaled its rounded-clip layer bounds but not the radius, so its
// corner arc collapsed as `radius / scale` (logical inset 9.00/4.50/2.25 at
// scale 1/2/4). wgpu was already correct: `clip_region_of` (`wgpu/src/lib.rs:76`)
// multiplies the rect AND all four radii by `scale_factor`.
//
// This is the wgpu half of the pair. It pins wgpu as the reference — if wgpu
// ever regresses the same way, the tiny_skia sweep alone would not say which
// backend moved. The two files use the SAME specimen (100px logical square,
// 30px radius, top-left diagonal probe) so their printed numbers are directly
// comparable:
//
//   scale | tiny_skia (fixed) | wgpu
//   ------|-------------------|------
//     1   |  9.00             | 9.00
//     2   |  9.00             | 8.50
//     4   |  8.75             | 8.75
//
// The residual sub-pixel spread is AA/threshold quantisation of a diagonal
// sample, not a scaling error: the bug it replaced was a 4.50 and a 2.25.

/// Logical edge length of the scale-sweep specimen (matches the tiny_skia test).
const SWEEP_LOGICAL: f32 = 100.0;
/// Logical corner radius of the scale-sweep specimen (matches tiny_skia).
const SWEEP_RADIUS: f32 = 30.0;

/// Render the full-bleed child under a `SWEEP_RADIUS` rounded clip covering a
/// `SWEEP_LOGICAL` logical square, rasterised at `scale`. `None` if no adapter.
///
/// Clip bounds are logical; the screenshot is `SWEEP_LOGICAL * scale` physical
/// pixels — the HiDPI / supersampling arrangement.
fn render_at_scale(scale: f32) -> Option<(Vec<u8>, u32)> {
    let phys = (SWEEP_LOGICAL * scale) as u32;
    let mut renderer = try_renderer()?;

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

    Some((
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(phys, phys), scale, BG),
        phys,
    ))
}

/// Corner arc inset in **physical** pixels: first child-coloured pixel along the
/// top-left diagonal (`y == x`).
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
    let Some((buf1, phys1)) = render_at_scale(1.0) else {
        println!("SKIPPED: no wgpu adapter available in this environment");
        return;
    };

    let mut logical = vec![diagonal_inset_physical(&buf1, phys1)];
    let mut physical = vec![logical[0]];
    println!(
        "wgpu scale=1 phys={phys1} inset_physical={:.2} inset_logical={:.4}",
        physical[0], logical[0]
    );

    for scale in [2.0f32, 4.0] {
        let (buf, phys) = render_at_scale(scale).expect("adapter worked at scale 1");
        let pi = diagonal_inset_physical(&buf, phys);
        assert!(
            pi.is_finite(),
            "wgpu scale {scale}: no child found along the diagonal"
        );
        println!(
            "wgpu scale={scale} phys={phys} inset_physical={pi:.2} inset_logical={:.4}",
            pi / scale
        );
        physical.push(pi);
        logical.push(pi / scale);
    }

    assert!(
        logical[0] > 5.0,
        "calibration: at scale 1 a {SWEEP_RADIUS}px radius must give a \
         multi-pixel inset, measured {:.4}",
        logical[0]
    );

    for (i, scale) in [1.0f32, 2.0, 4.0].iter().enumerate() {
        let drift = (logical[i] - logical[0]).abs();
        assert!(
            drift <= 1.0,
            "wgpu scale {scale}: corner arc measured {:.4} logical px against \
             {:.4} at scale 1 (drift {drift:.4}). `clip_region_of` must scale \
             the radius with the rect (#319). logical: {logical:?}, physical: \
             {physical:?}",
            logical[i],
            logical[0]
        );
    }

    // The mechanism, stated directly: the physical arc grows with resolution.
    // A backend that forgets to scale the radius pins this constant.
    assert!(
        physical[1] > physical[0] * 1.5,
        "wgpu physical inset barely grew from scale 1 ({:.2}) to 2 ({:.2}) — the \
         radius is not being scaled with the rect (#319)",
        physical[0],
        physical[1]
    );
}

// ---------------------------------------------------------------------------
// enzUI-ICED #1442 — nested clip bounds are cumulative
// ---------------------------------------------------------------------------

fn render_cumulative_clip(outer: Option<Rectangle>, inner: Rectangle) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
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

    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        BG,
    ))
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
    let Some(control) = render_cumulative_clip(None, resting_child) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    assert!(
        is_child(px(&control, 100, 60)),
        "control did not paint the pixel that the outer clip must remove"
    );

    let clipped =
        render_cumulative_clip(Some(outer), resting_child).expect("adapter worked for the control");
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
    let Some(control) = render_cumulative_clip(None, disjoint_child) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    assert!(is_child(px(&control, 100, 60)), "control must paint");

    let clipped = render_cumulative_clip(Some(outer), disjoint_child)
        .expect("adapter worked for the control");
    assert!(
        (0..H).all(|y| (0..W).all(|x| is_bg(px(&clipped, x, y)))),
        "a disjoint nested clip became effectively unclipped"
    );
}
