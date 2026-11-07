// Modified by enzUI contributors. See FORK.md.
//! Renderer image-cache lifetime gates (enzUI-ICED #1512).
//!
//! These tests drive actual prepared frames. They verify that a text-only frame
//! advances cache epochs even when no replacement image is inserted.

use iced_wgpu::core::image::{Handle as ImageHandle, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Settings};
use iced_wgpu::core::svg::{Handle as SvgHandle, Renderer as _, Svg};
use iced_wgpu::core::{Color, Image, Radians, Rectangle, Renderer as _, Size};
use iced_wgpu::{ImageCachePolicy, Renderer};

const SIZE: u32 = 64;
const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" fill="#369"/></svg>"##;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

fn present(renderer: &mut Renderer) {
    let _ = <Renderer as Headless>::screenshot(renderer, Size::new(SIZE, SIZE), 1.0, Color::BLACK);
}

fn reset(renderer: &mut Renderer) {
    renderer.reset(bounds());
}

fn raster_handle() -> ImageHandle {
    ImageHandle::from_rgba(
        SIZE,
        SIZE,
        (0..SIZE * SIZE)
            .flat_map(|_| [0x33, 0x66, 0x99, 0xff])
            .collect::<Vec<_>>(),
    )
}

fn draw_raster(renderer: &mut Renderer, handle: &ImageHandle) {
    renderer.draw_image(Image::new(handle.clone()), bounds(), bounds());
}

fn draw_svg(renderer: &mut Renderer, handle: &SvgHandle) {
    renderer.draw_svg(
        Svg {
            handle: handle.clone(),
            color: None,
            rotation: Radians(0.0),
            opacity: 1.0,
        },
        bounds(),
        bounds(),
    );
}

#[test]
fn departed_raster_and_svg_are_evicted_after_a_complete_missed_frame() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    renderer.set_image_cache_policy(ImageCachePolicy::new(1, 64 * 1024 * 1024));
    let raster = raster_handle();
    let svg = SvgHandle::from_memory(SVG.to_vec());

    draw_raster(&mut renderer, &raster);
    draw_svg(&mut renderer, &svg);
    present(&mut renderer);

    let live = renderer.image_cache_statistics();
    assert_eq!(live.raster_entries, 1);
    assert_eq!(live.svg_entries, 1);
    assert_eq!(live.svg_raster_entries, 1);
    assert_eq!(live.atlas_allocations, 2);
    assert_eq!(live.atlas_live_layers, 1);
    assert_eq!(live.atlas_occupied_bytes, 2 * u64::from(SIZE * SIZE) * 4);

    // The first text-only frame is a complete miss, but it is intentionally a
    // grace frame so prepare ordering cannot evict an entry prematurely.
    reset(&mut renderer);
    present(&mut renderer);
    let grace = renderer.image_cache_statistics();
    assert_eq!(grace.raster_entries, 1);
    assert_eq!(grace.svg_raster_entries, 1);
    assert!(grace.raster_stale_bytes >= u64::from(SIZE * SIZE) * 4);
    assert!(grace.svg_stale_bytes >= u64::from(SIZE * SIZE) * 4);

    reset(&mut renderer);
    present(&mut renderer);
    let evicted = renderer.image_cache_statistics();
    assert_eq!(evicted.raster_entries, 0);
    assert_eq!(evicted.svg_entries, 0);
    assert_eq!(evicted.svg_raster_entries, 0);
    assert_eq!(evicted.atlas_allocations, 0);
    assert_eq!(evicted.atlas_live_layers, 0);
    assert_eq!(evicted.atlas_empty_layers, evicted.atlas_layers);
    assert_eq!(evicted.atlas_occupied_bytes, 0);
    assert_eq!(evicted.atlas_removals, 2);
}

#[test]
fn continuously_visible_and_one_frame_route_toggles_reuse_without_reupload() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let raster = raster_handle();
    let svg = SvgHandle::from_memory(SVG.to_vec());

    for _ in 0..8 {
        reset(&mut renderer);
        draw_raster(&mut renderer, &raster);
        draw_svg(&mut renderer, &svg);
        present(&mut renderer);
    }

    // Alternate a gallery frame with a text-only frame. One missed frame is
    // within the grace, so normal back/forward navigation must reuse both
    // uploads instead of churning them.
    for _ in 0..12 {
        reset(&mut renderer);
        present(&mut renderer);
        reset(&mut renderer);
        draw_raster(&mut renderer, &raster);
        draw_svg(&mut renderer, &svg);
        present(&mut renderer);
    }

    let statistics = renderer.image_cache_statistics();
    assert_eq!(statistics.raster_entries, 1);
    assert_eq!(statistics.svg_raster_entries, 1);
    assert_eq!(statistics.raster_uploads, 1);
    assert_eq!(statistics.svg_uploads, 1);
    assert_eq!(statistics.atlas_uploads, 2);
    assert_eq!(statistics.atlas_removals, 0);
    assert_eq!(statistics.raster_stale_entries, 0);
    assert_eq!(statistics.svg_stale_entries, 0);
}

/// Leaving a page for a few seconds keeps its images: by default the cache
/// does not decode and upload them again when the page returns.
///
/// Sabotage: a default grace of one frame -- both entries are evicted while
/// the page is away and uploaded again on return.
#[test]
fn images_on_a_page_left_for_a_few_seconds_are_reused_by_default() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let raster = raster_handle();
    let svg = SvgHandle::from_memory(SVG.to_vec());

    draw_raster(&mut renderer, &raster);
    draw_svg(&mut renderer, &svg);
    present(&mut renderer);
    let shown = renderer.image_cache_statistics();

    // Another page for three seconds at 144 Hz.
    for _ in 0..432 {
        reset(&mut renderer);
        present(&mut renderer);
    }

    reset(&mut renderer);
    draw_raster(&mut renderer, &raster);
    draw_svg(&mut renderer, &svg);
    present(&mut renderer);
    let returned = renderer.image_cache_statistics();

    assert_eq!(returned.raster_entries, 1);
    assert_eq!(returned.svg_raster_entries, 1);
    assert_eq!(
        returned.raster_uploads, shown.raster_uploads,
        "the raster was uploaded again"
    );
    assert_eq!(
        returned.atlas_removals, shown.atlas_removals,
        "an entry was evicted"
    );
}

#[test]
fn external_allocation_outlives_epochs_and_zero_budget_evicts_after_one_miss() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    renderer.set_image_cache_policy(ImageCachePolicy::new(1, 64 * 1024 * 1024));
    let held_handle = raster_handle();
    let allocation = renderer
        .load_image(&held_handle)
        .expect("RGBA allocation should upload");

    for _ in 0..4 {
        reset(&mut renderer);
        present(&mut renderer);
    }
    let held = renderer.image_cache_statistics();
    assert_eq!(held.raster_entries, 1);
    assert_eq!(held.atlas_allocations, 1);

    drop(allocation);
    reset(&mut renderer);
    present(&mut renderer);
    let released = renderer.image_cache_statistics();
    assert_eq!(released.raster_entries, 0);
    assert_eq!(released.atlas_allocations, 0);
    assert_eq!(released.atlas_removals, 1);

    let Some(mut budgeted) = try_renderer() else {
        eprintln!("SKIP: no second wgpu adapter available in this environment");
        return;
    };
    budgeted.set_image_cache_policy(ImageCachePolicy::new(100, 0));
    let handle = raster_handle();
    draw_raster(&mut budgeted, &handle);
    present(&mut budgeted);

    reset(&mut budgeted);
    present(&mut budgeted);
    let bounded = budgeted.image_cache_statistics();
    assert_eq!(bounded.grace_frames, 100);
    assert_eq!(bounded.stale_byte_budget, 0);
    assert_eq!(bounded.raster_entries, 0);
    assert_eq!(bounded.atlas_allocations, 0);
}

#[test]
fn hundred_image_working_set_returns_to_allocated_layer_baseline_after_the_grace() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };
    let baseline = renderer.image_cache_statistics();
    let rasters = (0..100u8)
        .map(|value| {
            ImageHandle::from_rgba(
                256,
                256,
                (0..256 * 256)
                    .flat_map(|_| [value, value.wrapping_add(1), 0x99, 0xff])
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let svg = SvgHandle::from_memory(SVG.to_vec());

    for handle in &rasters {
        draw_raster(&mut renderer, handle);
    }
    for side in [8.0, 16.0, 32.0, 64.0] {
        let bounds = Rectangle::with_size(Size::new(side, side));
        renderer.draw_svg(
            Svg {
                handle: svg.clone(),
                color: None,
                rotation: Radians(0.0),
                opacity: 1.0,
            },
            bounds,
            bounds,
        );
    }
    present(&mut renderer);

    let peak = renderer.image_cache_statistics();
    println!(
        "#1512 mechanism peak: raster={} svg_rasters={} bytes={} atlas_layers={} atlas_allocated={} uploads={}",
        peak.raster_entries,
        peak.svg_raster_entries,
        peak.raster_bytes + peak.svg_bytes,
        peak.atlas_layers,
        peak.atlas_allocated_bytes,
        peak.atlas_uploads,
    );
    assert_eq!(peak.raster_entries, 100);
    assert_eq!(peak.raster_uploads, 100);
    assert_eq!(peak.svg_raster_entries, 4);
    assert_eq!(peak.svg_uploads, 4);
    assert_eq!(peak.atlas_allocations, 104);
    assert!(peak.atlas_layers > baseline.atlas_layers);
    assert!(peak.atlas_allocated_bytes > baseline.atlas_allocated_bytes);

    for _ in 0..=peak.grace_frames {
        reset(&mut renderer);
        present(&mut renderer);
    }

    let settled = renderer.image_cache_statistics();
    println!(
        "#1512 mechanism settled: raster={} svg={} allocations={} live_layers={} layers={} atlas_allocated={} removals={}",
        settled.raster_entries,
        settled.svg_raster_entries,
        settled.atlas_allocations,
        settled.atlas_live_layers,
        settled.atlas_layers,
        settled.atlas_allocated_bytes,
        settled.atlas_removals,
    );
    assert_eq!(settled.raster_entries, 0);
    assert_eq!(settled.svg_entries, 0);
    assert_eq!(settled.svg_raster_entries, 0);
    assert_eq!(settled.atlas_allocations, 0);
    assert_eq!(settled.atlas_live_layers, 0);
    assert_eq!(settled.atlas_layers, baseline.atlas_layers);
    assert_eq!(
        settled.atlas_allocated_bytes,
        baseline.atlas_allocated_bytes
    );
    assert_eq!(settled.atlas_uploads, 104);
    assert_eq!(settled.atlas_removals, 104);
}
