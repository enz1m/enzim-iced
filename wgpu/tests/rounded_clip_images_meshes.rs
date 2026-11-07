// Modified by enzUI contributors. See FORK.md.
//! enzICED #41 stage 3 — rounded layer clipping for **images** and **meshes**
//! in the wgpu backend.
//!
//! Stage 2 (`e13616ed`) threaded the layer's rounded clip region into the
//! *quad* uniforms only. `Renderer::prepare` (`wgpu/src/lib.rs`) handed
//! `clip_region_of(layer, scale_factor)` to `self.quad.prepare(..)` and handed
//! the triangle and image pipelines nothing at all, so an image or a mesh
//! placed inside a rounded clip layer painted with **square** corners over the
//! parent's arc. That is the avatar-in-a-rounded-card / chart-in-a-rounded-card
//! defect (enzUI-ICED #41.1 / #75.1 / #80.1).
//!
//! Note this is **layer** clipping, distinct from `Image::border_radius`
//! (upstream PR #3092), which rounds an individual image against its own
//! `clip_bounds` and is covered by `tests/image_border_radius.rs`. Both must
//! work, and they compose: the image path multiplies the two coverages.
//!
//! Calibration is the whole point of this file. Every rounded assertion is
//! paired with a radius-0 control rendered through the **same** code path, so a
//! "corner is background" result can never pass because the primitive failed to
//! draw at all.
//!
//! Requires a working wgpu adapter. With none available the tests **skip with a
//! printed reason** rather than fabricating a pixel claim.

use iced_wgpu::Renderer;
use iced_wgpu::core::border;
use iced_wgpu::core::image::{Handle, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Renderer as _, Settings};
use iced_wgpu::core::{Color, Image, Rectangle, Size};
use iced_wgpu::graphics::color;
use iced_wgpu::graphics::mesh::{self, Cache, Mesh, Renderer as _, SolidVertex2D};
use std::sync::Arc;

const W: u32 = 200;
const H: u32 = 200;
const BG: Color = Color::BLACK;

/// The clip radius used by every rounded case here. At 200x200 a 60px radius
/// leaves the 60x60 corner blocks roughly 27% background — hundreds of pixels,
/// never confusable with the exactly-0 an uncarved corner measures.
const R: f32 = 60.0;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn shot(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, Size::new(W, H), 1.0, BG)
}

fn full() -> Rectangle {
    Rectangle::with_size(Size::new(W as f32, H as f32))
}

/// A full-bleed opaque-magenta image inside a layer clipped by `radius`.
fn render_image(radius: border::Radius) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;

    // Uniformly magenta: any carved pixel is unambiguously the clip at work
    // and never a sample of image content.
    let pixels: Vec<u8> = (0..(W * H)).flat_map(|_| [255u8, 0, 255, 255]).collect();
    let image = Image::new(Handle::from_rgba(W, H, pixels));

    renderer.start_layer_rounded(full(), radius);
    renderer.draw_image(image, full(), full());
    renderer.end_layer();

    Some(shot(&mut renderer))
}

/// A full-bleed magenta solid mesh (two triangles) covering the whole surface.
fn full_bleed_mesh() -> Mesh {
    let magenta = color::pack(Color {
        r: 1.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    });
    let w = W as f32;
    let h = H as f32;

    Mesh::Solid {
        buffers: mesh::Indexed {
            vertices: vec![
                SolidVertex2D {
                    position: [0.0, 0.0],
                    color: magenta,
                },
                SolidVertex2D {
                    position: [w, 0.0],
                    color: magenta,
                },
                SolidVertex2D {
                    position: [w, h],
                    color: magenta,
                },
                SolidVertex2D {
                    position: [0.0, h],
                    color: magenta,
                },
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        transformation: iced_wgpu::core::Transformation::IDENTITY,
        clip_bounds: full(),
    }
}

/// A full-bleed magenta solid mesh inside a layer clipped by `radius`.
fn render_mesh(radius: border::Radius) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;

    renderer.start_layer_rounded(full(), radius);
    renderer.draw_mesh(full_bleed_mesh());
    renderer.end_layer();

    Some(shot(&mut renderer))
}

fn is_bg(buf: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    buf[i] < 60 && buf[i + 1] < 60 && buf[i + 2] < 60
}

fn is_child(buf: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    buf[i] > 150 && buf[i + 1] < 100 && buf[i + 2] > 150
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

/// Shared body: `render` must cover all four corners at radius 0 and carve all
/// four at radius `R`, with the centre and straight edges left alone.
fn assert_rounded_layer_clip(kind: &str, render: impl Fn(border::Radius) -> Option<Vec<u8>>) {
    // CALIBRATION FIRST. Without this, "the corner is background" would pass
    // just as well against a primitive that never drew anything.
    let Some(flat) = render(border::Radius::default()) else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let flat_cutouts = corner_cutouts(&flat);
    println!("{kind} radius=0 -> cutout[tl,tr,bl,br] = {flat_cutouts:?}");
    assert_eq!(
        flat_cutouts,
        [0, 0, 0, 0],
        "calibration failed: with no clip radius the {kind} must cover all four \
         corner blocks. If it does not, the {kind} never drew and the rounded \
         assertion below would pass for the wrong reason."
    );

    let rounded = render(border::Radius::new(R)).expect("adapter worked for the calibration");
    let cutouts = corner_cutouts(&rounded);
    println!("{kind} radius={R} -> cutout[tl,tr,bl,br] = {cutouts:?}");

    for (slot, name) in ["top_left", "top_right", "bottom_left", "bottom_right"]
        .into_iter()
        .enumerate()
    {
        assert!(
            cutouts[slot] > 300,
            "{kind}: the {R}px rounded layer clip did not carve the {name} \
             corner — cutout {} px (all corners: {cutouts:?}). A rectangular \
             scissor cannot express a corner arc, so the clip region must reach \
             this pipeline's shader.",
            cutouts[slot]
        );
    }

    // Direction, not just magnitude: the clip must remove ONLY the corner arcs.
    assert!(
        is_child(&rounded, W / 2, H / 2),
        "{kind}: the rounded clip over-masked — the centre should still be \
         covered"
    );
    assert!(
        is_child(&rounded, W / 2, 1) && is_child(&rounded, 1, H / 2),
        "{kind}: a straight edge midpoint was clipped; only the corner arcs \
         should be carved"
    );
}

#[test]
fn rounded_layer_clip_carves_image_corners() {
    assert_rounded_layer_clip("image", render_image);
}

#[test]
fn rounded_layer_clip_carves_mesh_corners() {
    assert_rounded_layer_clip("mesh", render_mesh);
}

/// Per-corner (#306 axis). A uniform radius is invariant under a point mirror
/// of the corner lookup (tl<->br, tr<->bl), so the tests above would pass
/// identically against a mirrored SDF. Only a single-corner radius can see it,
/// and each new shader gets its own copy of the lookup, so each needs its own
/// per-corner test.
fn assert_per_corner(kind: &str, render: impl Fn(border::Radius) -> Option<Vec<u8>>) {
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
        println!("{kind} clip_radius.{name}(48) -> cutout[tl,tr,bl,br] = {cutouts:?}");

        assert!(
            cutouts[expected] > 300,
            "{kind}: clip_radius.{name}(48) should carve the {name} corner, but \
             its cutout is only {} px (all corners: {cutouts:?})",
            cutouts[expected]
        );

        for (slot, count) in cutouts.iter().enumerate() {
            if slot == expected {
                continue;
            }
            assert_eq!(
                *count, 0,
                "{kind}: clip_radius.{name}(48) carved corner index {slot} too \
                 (all corners: {cutouts:?}) — the per-corner lookup is wrong; a \
                 point-mirrored lookup shows up here as the diagonally opposite \
                 corner being the ONLY one carved"
            );
        }
    }
}

#[test]
fn rounded_layer_clip_is_per_corner_for_images() {
    assert_per_corner("image", render_image);
}

#[test]
fn rounded_layer_clip_is_per_corner_for_meshes() {
    assert_per_corner("mesh", render_mesh);
}

/// The layer clip and `Image::border_radius` are independent mechanisms that
/// must compose rather than one overwriting the other. A top-left-only image
/// radius inside a bottom-right-only layer clip must carve BOTH corners.
#[test]
fn image_border_radius_and_layer_clip_compose() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let pixels: Vec<u8> = (0..(W * H)).flat_map(|_| [255u8, 0, 255, 255]).collect();
    let mut image = Image::new(Handle::from_rgba(W, H, pixels));
    image.border_radius = border::Radius::default().top_left(48.0);

    renderer.start_layer_rounded(full(), border::Radius::default().bottom_right(48.0));
    renderer.draw_image(image, full(), full());
    renderer.end_layer();

    let buf = shot(&mut renderer);
    let cutouts = corner_cutouts(&buf);
    println!("compose(image tl=48, layer br=48) -> cutout[tl,tr,bl,br] = {cutouts:?}");

    assert!(
        cutouts[0] > 300,
        "the image's own top-left border_radius stopped working once a layer \
         clip was present (cutouts {cutouts:?})"
    );
    assert!(
        cutouts[3] > 300,
        "the layer's bottom-right rounded clip did not reach the image \
         (cutouts {cutouts:?})"
    );
    assert_eq!(
        [cutouts[1], cutouts[2]],
        [0, 0],
        "neither radius touches the top-right or bottom-left corner, but one \
         of them was carved (cutouts {cutouts:?})"
    );
}

// ---------------------------------------------------------------------------
// The CACHED mesh path (`triangle::Item::Cached` / `Storage`)
// ---------------------------------------------------------------------------
//
// `draw_mesh` and `draw_mesh_cache` take different routes through
// `triangle::State::prepare`, and only the cached one memoises its GPU upload.
// `Storage::prepare` re-uploads when `version` or `transformation` changes; the
// clip region now lives in those same uniforms, so it had to join the cache key
// or a cached mesh would keep the clip it was FIRST uploaded with. That is the
// realistic app shape: a `Canvas` whose geometry is unchanged (same version,
// same transformation) scrolled from outside a rounded card to inside it.
//
// A single-frame test cannot see this — a cold cache always uploads. It needs
// two frames through the SAME renderer, with the `Cache` held alive across
// them so `State::trim` (`Arc::strong_count > 1`) does not evict it.

/// Draws `cache` under a rounded layer clip of `radius` and screenshots.
///
/// `reset` models the real frame boundary: `UserInterface::draw` opens every
/// frame with `renderer.reset(viewport)` (`runtime/src/user_interface.rs:506`).
/// Without it the layer stack accumulates and frame 2 replays frame 1's layers
/// as well as its own, so the mesh is drawn TWICE -- once unclipped -- and the
/// corners read as covered no matter what the cache key does. That is a
/// harness artifact, not the behaviour under test.
fn draw_cached_frame(renderer: &mut Renderer, cache: &Cache, radius: border::Radius) -> Vec<u8> {
    renderer.reset(full());

    renderer.start_layer_rounded(full(), radius);
    renderer.draw_mesh_cache(cache.clone());
    renderer.end_layer();

    shot(renderer)
}

#[test]
fn cached_mesh_picks_up_a_changed_layer_clip() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // Held for the whole test: `triangle::Storage::trim` drops any upload whose
    // `Cache` is gone, which would silently turn frame 2 into a cold upload and
    // make this test pass without ever exercising the cache-hit branch.
    let cache = Cache::new(Arc::from(vec![full_bleed_mesh()]));

    // Frame 1: no rounded clip. This is what populates the upload.
    let flat = draw_cached_frame(&mut renderer, &cache, border::Radius::default());
    let flat_cutouts = corner_cutouts(&flat);
    println!("cached mesh frame1 radius=0 -> cutout[tl,tr,bl,br] = {flat_cutouts:?}");
    assert_eq!(
        flat_cutouts,
        [0, 0, 0, 0],
        "calibration failed: the cached mesh did not cover the corners on \
         frame 1, so frame 2's assertion would be meaningless"
    );

    // Frame 2: SAME cache (same id, same version) and SAME transformation, but
    // now inside a rounded clip. Only the clip changed.
    let rounded = draw_cached_frame(&mut renderer, &cache, border::Radius::new(R));
    let cutouts = corner_cutouts(&rounded);
    println!("cached mesh frame2 radius={R} -> cutout[tl,tr,bl,br] = {cutouts:?}");

    for (slot, name) in ["top_left", "top_right", "bottom_left", "bottom_right"]
        .into_iter()
        .enumerate()
    {
        assert!(
            cutouts[slot] > 300,
            "cached mesh: the {name} corner was not carved on frame 2 — cutout \
             {} px (all corners: {cutouts:?}). The upload was reused with its \
             frame-1 clip region: `ClipRegion` must be part of \
             `triangle::Upload`'s cache key.",
            cutouts[slot]
        );
    }

    assert!(
        is_child(&rounded, W / 2, H / 2),
        "cached mesh: the rounded clip over-masked the centre on frame 2"
    );

    // And back again — a cached mesh leaving a rounded clip must stop being
    // clipped. Same staleness bug, opposite direction; the assertion above
    // alone would pass against a cache key that only ever ADDS clips.
    let flat_again = draw_cached_frame(&mut renderer, &cache, border::Radius::default());
    let flat_again_cutouts = corner_cutouts(&flat_again);
    println!("cached mesh frame3 radius=0 -> cutout[tl,tr,bl,br] = {flat_again_cutouts:?}");
    assert_eq!(
        flat_again_cutouts,
        [0, 0, 0, 0],
        "cached mesh: frame 3 removed the rounded clip but the corners are \
         still carved — the upload kept frame 2's stale clip region"
    );
}
