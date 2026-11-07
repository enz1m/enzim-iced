// Modified by enzUI contributors. See FORK.md.
//! enzUI-ICED #1515 — quad clip-layer GPU storage.
//!
//! Before this patch every distinct, unmerged clip layer that contained quads
//! eagerly allocated **both** a solid and a gradient instance buffer sized for
//! `INITIAL_INSTANCES = 2_000` (256 KiB + 512 KiB after `Buffer`'s power-of-two
//! rounding), plus a uniform buffer — whether the layer held one quad or none
//! of that fill kind. `trim` never released a slot, so a transient clip-heavy
//! screen pinned that high-water mark for the renderer's lifetime.
//!
//! These tests drive real prepared frames through the headless wgpu renderer
//! and assert *mechanism counts* (`Renderer::quad_statistics`), which are
//! machine-independent, plus the pixels that prove the packing still paints the
//! same thing in the same order.
//!
//! Requires a working wgpu adapter; without one they skip with a printed
//! reason rather than fabricate a result.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{
    Background, Border, Color, Degrees, Gradient, Point, Rectangle, Shadow, Size, gradient,
};

const W: u32 = 200;
const H: u32 = 200;
const BG: Color = Color::BLACK;

/// Ceiling for what one clip layer holding a handful of quads may allocate.
///
/// One instance buffer at the `MIN_INSTANCES = 16` floor is 2 KiB (solid) or
/// 4 KiB (gradient); the uniform buffer is 96 B. The pre-patch layer allocated
/// 786 528 B, so this ceiling is ~180x tighter than the old behaviour and is
/// what the sabotage gate trips on.
const SMALL_LAYER_BYTES: u64 = 8 * 1024;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn viewport() -> Rectangle {
    Rectangle::with_size(Size::new(W as f32, H as f32))
}

fn quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

fn solid(color: Color) -> Background {
    Background::Color(color)
}

fn flat_gradient(color: Color) -> Background {
    Background::Gradient(Gradient::Linear(
        gradient::Linear::new(Degrees(90.0))
            .add_stop(0.0, color)
            .add_stop(1.0, color),
    ))
}

/// Runs one complete frame: resets the layer stack, draws, presents.
fn frame(renderer: &mut Renderer, draw: impl FnOnce(&mut Renderer)) -> Vec<u8> {
    renderer.reset(viewport());
    draw(renderer);
    <Renderer as Headless>::screenshot(renderer, Size::new(W, H), 1.0, BG)
}

/// `count` sibling clip layers with pairwise-distinct bounds, so the graphics
/// layer stack cannot merge them, each holding one quad built by `fill`.
fn distinct_layers(renderer: &mut Renderer, count: usize, fill: impl Fn(usize) -> Background) {
    for i in 0..count {
        // Distinct bounds (sub-pixel widths) defeat merging while keeping every
        // layer on screen and scissored to the same snapped rectangle.
        let bounds = Rectangle::new(
            Point::ORIGIN,
            Size::new(W as f32 - i as f32 * 0.01, H as f32),
        );

        renderer.start_layer(bounds);
        renderer.fill_quad(
            quad(Rectangle::new(Point::ORIGIN, Size::new(8.0, 8.0))),
            fill(i),
        );
        renderer.end_layer();
    }
}

fn px(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2])
}

/// FNV-1a over the whole screenshot, so a fixture's pixels can be compared
/// across two builds of the renderer by eye in the test log.
fn checksum(buf: &[u8]) -> u64 {
    buf.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn near(actual: (u8, u8, u8), expected: (u8, u8, u8)) -> bool {
    let d = |a: u8, b: u8| a.abs_diff(b) <= 6;
    d(actual.0, expected.0) && d(actual.1, expected.1) && d(actual.2, expected.2)
}

#[test]
fn solid_only_layers_allocate_no_gradient_storage() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    for count in [1, 10, 100] {
        let _ = frame(&mut renderer, |r| {
            distinct_layers(r, count, |_| solid(Color::WHITE));
        });

        let stats = renderer.quad_statistics();
        println!("{count} solid layers -> {stats:?}");

        assert_eq!(stats.active_layers, count, "one layer per distinct clip");
        assert_eq!(stats.solid_buffers, count, "one solid buffer per layer");
        assert_eq!(
            stats.gradient_buffers, 0,
            "a solid-only layer must not allocate gradient storage"
        );
        assert_eq!(stats.draw_calls, count as u64);
        assert!(
            stats.allocated_bytes <= stats.retained_layers as u64 * SMALL_LAYER_BYTES,
            "{} bytes for {} retained layers exceeds {SMALL_LAYER_BYTES} B/layer",
            stats.allocated_bytes,
            stats.retained_layers
        );
        assert!(
            stats.uploaded_bytes < stats.allocated_bytes,
            "a one-quad layer must upload one instance, not its whole buffer"
        );
    }
}

#[test]
fn gradient_only_layers_allocate_no_solid_storage() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let _ = frame(&mut renderer, |r| {
        distinct_layers(r, 100, |_| flat_gradient(Color::WHITE));
    });

    let stats = renderer.quad_statistics();
    println!("100 gradient layers -> {stats:?}");

    assert_eq!(stats.active_layers, 100);
    assert_eq!(stats.gradient_buffers, 100);
    assert_eq!(
        stats.solid_buffers, 0,
        "a gradient-only layer must not allocate solid storage"
    );
    assert!(
        stats.allocated_bytes <= 100 * SMALL_LAYER_BYTES,
        "{} bytes for 100 gradient layers",
        stats.allocated_bytes
    );
}

#[test]
fn mixed_runs_allocate_only_the_kinds_present() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // Alternating solid / gradient layers: half the slots need each kind.
    let _ = frame(&mut renderer, |r| {
        distinct_layers(r, 100, |i| {
            if i % 2 == 0 {
                solid(Color::WHITE)
            } else {
                flat_gradient(Color::WHITE)
            }
        });
    });

    let stats = renderer.quad_statistics();
    println!("mixed 100 layers -> {stats:?}");

    assert_eq!(stats.active_layers, 100);
    assert_eq!(stats.solid_buffers, 50);
    assert_eq!(stats.gradient_buffers, 50);
    assert_eq!(
        stats.solid_buffers + stats.gradient_buffers,
        100,
        "no layer may hold both kinds when it drew only one"
    );
}

#[test]
fn empty_and_offscreen_layers_allocate_and_upload_nothing() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let _ = frame(&mut renderer, |r| {
        // An empty clip layer.
        r.start_layer(Rectangle::new(Point::ORIGIN, Size::new(50.0, 50.0)));
        r.end_layer();

        // A clip layer entirely outside the viewport, holding a quad.
        let offscreen = Rectangle::new(Point::new(1_000.0, 1_000.0), Size::new(50.0, 50.0));
        r.start_layer(offscreen);
        r.fill_quad(quad(offscreen), solid(Color::WHITE));
        r.end_layer();
    });

    let stats = renderer.quad_statistics();
    println!("empty + offscreen -> {stats:?}");

    assert_eq!(stats.active_layers, 0);
    assert_eq!(stats.retained_layers, 0);
    assert_eq!(stats.allocated_bytes, 0);
    assert_eq!(stats.uploaded_bytes, 0);
    assert_eq!(stats.draw_calls, 0);
    assert_eq!(stats.buffers_created, 0);
    assert_eq!(stats.bind_groups_created, 0);
}

#[test]
fn stable_frames_reuse_buffers_and_bind_groups() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let draw = |r: &mut Renderer| distinct_layers(r, 10, |_| solid(Color::WHITE));

    for _ in 0..4 {
        let _ = frame(&mut renderer, draw);
    }
    let settled = renderer.quad_statistics();

    for _ in 0..12 {
        let _ = frame(&mut renderer, draw);
    }
    let later = renderer.quad_statistics();

    println!("settled={settled:?}\nlater={later:?}");

    assert_eq!(later.active_layers, 10);
    assert_eq!(later.retained_layers, 10);
    assert_eq!(
        later.buffers_created, settled.buffers_created,
        "a repeated identical frame must not create GPU buffers"
    );
    assert_eq!(
        later.bind_groups_created, settled.bind_groups_created,
        "a repeated identical frame must not create bind groups"
    );
    assert_eq!(
        later.buffers_released, settled.buffers_released,
        "a repeated identical frame must not release GPU buffers"
    );
    assert_eq!(
        later.allocated_bytes, settled.allocated_bytes,
        "steady-state footprint must not drift"
    );
    // 10 layers => 10 uniform buffers + 10 solid buffers, and nothing else.
    assert_eq!(later.buffers_created, 20);
    assert_eq!(later.bind_groups_created, 10);
}

#[test]
fn many_quads_in_one_layer_stay_in_one_geometrically_grown_buffer() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let draw = |r: &mut Renderer| {
        r.start_layer(viewport());
        for i in 0..2_000 {
            let x = (i % 100) as f32;
            let y = (i / 100) as f32;
            r.fill_quad(
                quad(Rectangle::new(Point::new(x, y), Size::new(1.0, 1.0))),
                solid(Color::WHITE),
            );
        }
        r.end_layer();
    };

    let _ = frame(&mut renderer, draw);
    let first = renderer.quad_statistics();
    println!("2000 quads, one layer, frame 1 -> {first:?}");

    assert_eq!(first.active_layers, 1);
    assert_eq!(first.solid_buffers, 1);
    assert_eq!(first.gradient_buffers, 0);
    assert_eq!(
        first.draw_calls, 1,
        "one contiguous run of solids is one draw call"
    );
    assert_eq!(
        first.buffers_created, 2,
        "sizing the buffer to the batch must not walk a resize chain"
    );
    assert!(
        first.uploaded_bytes >= 2_000 * 100,
        "every instance must be staged, got {}",
        first.uploaded_bytes
    );

    for _ in 0..8 {
        let _ = frame(&mut renderer, draw);
    }
    let later = renderer.quad_statistics();
    println!("2000 quads, one layer, frame 9 -> {later:?}");

    assert_eq!(
        later.buffers_created, first.buffers_created,
        "a stable large batch must not reallocate"
    );
    assert_eq!(later.buffers_released, first.buffers_released);
}

#[test]
fn transient_clip_layer_high_water_is_reclaimed_without_recurring_churn() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // A transient clip-heavy screen.
    let _ = frame(&mut renderer, |r| {
        distinct_layers(r, 1_000, |_| solid(Color::WHITE));
    });
    let peak = renderer.quad_statistics();
    println!("transient 1000 layers -> {peak:?}");

    assert_eq!(peak.active_layers, 1_000);
    assert_eq!(peak.retained_layers, 1_000);
    assert!(
        peak.allocated_bytes <= 1_000 * SMALL_LAYER_BYTES,
        "even the peak must scale with instances, not 2000 per fill kind: {} bytes",
        peak.allocated_bytes
    );

    // ... followed by a steady, small screen.
    let steady = |r: &mut Renderer| distinct_layers(r, 10, |_| solid(Color::WHITE));

    for _ in 0..20 {
        let _ = frame(&mut renderer, steady);
    }
    let settled = renderer.quad_statistics();
    println!("after 20 steady frames -> {settled:?}");

    assert_eq!(settled.active_layers, 10);
    assert_eq!(
        settled.retained_layers, 10,
        "surplus layer slots must be released"
    );
    assert!(
        settled.layers_reclaimed >= 990,
        "expected ~990 reclaimed slots, got {}",
        settled.layers_reclaimed
    );
    assert!(
        settled.allocated_bytes <= 10 * SMALL_LAYER_BYTES,
        "settled footprint {} B must return to the small-screen budget",
        settled.allocated_bytes
    );
    assert!(
        settled.allocated_bytes * 20 < peak.allocated_bytes,
        "settled {} B vs peak {} B",
        settled.allocated_bytes,
        peak.allocated_bytes
    );

    // Reclamation must not become an every-frame allocate/free cycle.
    for _ in 0..280 {
        let _ = frame(&mut renderer, steady);
    }
    let late = renderer.quad_statistics();
    println!("after 300 steady frames -> {late:?}");

    assert_eq!(
        late.buffers_created, settled.buffers_created,
        "steady frames after reclamation must not allocate again"
    );
    assert_eq!(
        late.buffers_released, settled.buffers_released,
        "steady frames after reclamation must not keep releasing"
    );
    assert_eq!(late.retained_layers, 10);
}

#[test]
fn transient_batch_size_inside_a_layer_is_reclaimed() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    let big = |r: &mut Renderer| {
        r.start_layer(viewport());
        for i in 0..4_000 {
            let x = (i % 100) as f32;
            let y = ((i / 100) % 100) as f32;
            r.fill_quad(
                quad(Rectangle::new(Point::new(x, y), Size::new(1.0, 1.0))),
                solid(Color::WHITE),
            );
        }
        r.end_layer();
    };

    let _ = frame(&mut renderer, big);
    let peak = renderer.quad_statistics();
    println!("4000 quads in one layer -> {peak:?}");
    assert!(peak.allocated_bytes >= 4_000 * 100);

    for _ in 0..12 {
        let _ = frame(&mut renderer, |r| {
            r.start_layer(viewport());
            r.fill_quad(
                quad(Rectangle::new(Point::ORIGIN, Size::new(8.0, 8.0))),
                solid(Color::WHITE),
            );
            r.end_layer();
        });
    }
    let settled = renderer.quad_statistics();
    println!("after settling to one quad -> {settled:?}");

    assert_eq!(settled.active_layers, 1);
    assert!(
        settled.allocated_bytes <= SMALL_LAYER_BYTES,
        "oversized instance capacity must be returned, got {} B",
        settled.allocated_bytes
    );
}

#[test]
fn alternating_solid_and_gradient_runs_paint_in_order_and_in_place() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    const RED: Color = Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    const GREEN: Color = Color {
        r: 0.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    const YELLOW: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };

    let half = W as f32 / 2.0;
    let tl = Rectangle::new(Point::ORIGIN, Size::new(half, half));
    let tr = Rectangle::new(Point::new(half, 0.0), Size::new(half, half));
    let bl = Rectangle::new(Point::new(0.0, half), Size::new(half, half));
    let br = Rectangle::new(Point::new(half, half), Size::new(half, half));

    // Solid, gradient, solid, gradient — four separate runs in one layer, so
    // each run must be sliced out of its own instance range.
    let buf = frame(&mut renderer, |r| {
        r.start_layer(viewport());
        r.fill_quad(quad(tl), solid(RED));
        r.fill_quad(quad(tr), flat_gradient(BLUE));
        r.fill_quad(quad(bl), solid(GREEN));
        r.fill_quad(quad(br), flat_gradient(YELLOW));
        r.end_layer();
    });

    println!("FIXTURE alternating checksum={:#018x}", checksum(&buf));

    let stats = renderer.quad_statistics();
    println!("alternating runs -> {stats:?}");
    assert_eq!(stats.draw_calls, 4, "four runs are four draw calls");
    assert_eq!(stats.bind_group_changes, 4);

    let q = W / 4;
    let sample_tl = px(&buf, q, q);
    let sample_tr = px(&buf, W - q, q);
    let sample_bl = px(&buf, q, H - q);
    let sample_br = px(&buf, W - q, H - q);
    println!("tl={sample_tl:?} tr={sample_tr:?} bl={sample_bl:?} br={sample_br:?}");

    assert!(near(sample_tl, (255, 0, 0)), "top-left {sample_tl:?}");
    assert!(near(sample_tr, (0, 0, 255)), "top-right {sample_tr:?}");
    assert!(
        near(sample_bl, (0, 255, 0)),
        "bottom-left {sample_bl:?}: the second solid run must read the second \
         solid instance, not the first"
    );
    assert!(
        near(sample_br, (255, 255, 0)),
        "bottom-right {sample_br:?}: the second gradient run must read the \
         second gradient instance, not the first"
    );

    // Same input, same output: the zero control for the samples above.
    let again = frame(&mut renderer, |r| {
        r.start_layer(viewport());
        r.fill_quad(quad(tl), solid(RED));
        r.fill_quad(quad(tr), flat_gradient(BLUE));
        r.fill_quad(quad(bl), solid(GREEN));
        r.fill_quad(quad(br), flat_gradient(YELLOW));
        r.end_layer();
    });
    assert_eq!(buf, again, "same input must produce identical pixels");

    // Reversing paint order must change what wins where the runs overlap.
    let over_gradient = frame(&mut renderer, |r| {
        r.start_layer(viewport());
        r.fill_quad(quad(viewport()), solid(RED));
        r.fill_quad(quad(viewport()), flat_gradient(BLUE));
        r.end_layer();
    });
    let over_solid = frame(&mut renderer, |r| {
        r.start_layer(viewport());
        r.fill_quad(quad(viewport()), flat_gradient(BLUE));
        r.fill_quad(quad(viewport()), solid(RED));
        r.end_layer();
    });
    let centre_gradient = px(&over_gradient, W / 2, H / 2);
    let centre_solid = px(&over_solid, W / 2, H / 2);
    println!("gradient-last={centre_gradient:?} solid-last={centre_solid:?}");
    assert!(near(centre_gradient, (0, 0, 255)));
    assert!(near(centre_solid, (255, 0, 0)));
}

#[test]
fn rounded_clips_borders_shadows_and_transforms_survive_lazy_layers() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    const MAGENTA: Color = Color {
        r: 1.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    const CYAN: Color = Color {
        r: 0.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    let scene = |r: &mut Renderer| {
        // Nested card-like clips: an outer rounded clip, an inner plain clip,
        // a transform, a bordered quad and a shadowed quad.
        r.start_layer_rounded(viewport(), iced_wgpu::core::border::Radius::new(60.0));
        r.fill_quad(quad(viewport()), solid(MAGENTA));

        r.start_layer(Rectangle::new(
            Point::new(40.0, 40.0),
            Size::new(120.0, 120.0),
        ));
        r.start_transformation(iced_wgpu::core::Transformation::translate(10.0, 10.0));
        r.fill_quad(
            Quad {
                bounds: Rectangle::new(Point::new(50.0, 50.0), Size::new(60.0, 60.0)),
                border: Border {
                    color: CYAN,
                    width: 6.0,
                    radius: iced_wgpu::core::border::Radius::new(8.0),
                },
                shadow: Shadow {
                    color: Color::BLACK,
                    offset: iced_wgpu::core::Vector::new(2.0, 2.0),
                    blur_radius: 4.0,
                },
                snap: false,
            },
            flat_gradient(Color::WHITE),
        );
        r.end_transformation();
        r.end_layer();

        r.end_layer();
    };

    let first = frame(&mut renderer, scene);
    let stats = renderer.quad_statistics();
    println!("nested clip scene -> {stats:?}");
    println!("FIXTURE nested clip checksum={:#018x}", checksum(&first));

    // The rounded outer clip still carves the corners...
    let corner = px(&first, 2, 2);
    let centre_edge = px(&first, 5, H / 2);
    println!("corner={corner:?} centre_edge={centre_edge:?}");
    assert!(
        corner.0 < 60 && corner.1 < 60 && corner.2 < 60,
        "rounded clip must leave the corner as background, got {corner:?}"
    );
    assert!(
        near(centre_edge, (255, 0, 255)),
        "the rounded clip must keep its middle, got {centre_edge:?}"
    );

    // ... the transformed, bordered, shadowed child is where it belongs ...
    let border_px = px(&first, 63, 63);
    println!("border={border_px:?}");
    assert!(
        near(border_px, (0, 255, 255)),
        "the transformed border should land at (63,63), got {border_px:?}"
    );

    // ... rendering it again is byte-identical (same-input zero control) ...
    let second = frame(&mut renderer, scene);
    assert_eq!(first, second, "same input must produce identical pixels");

    // ... and only the fill kinds actually drawn were allocated: the outer
    // layer is solid-only, the inner one gradient-only.
    assert_eq!(stats.active_layers, 2);
    assert_eq!(stats.solid_buffers, 1);
    assert_eq!(stats.gradient_buffers, 1);
}
