// Modified by enzUI contributors. See FORK.md.
//! Text atlas growth gates (enzUI-ICED #1514).
//!
//! Growing the glyph atlas used to re-rasterize (`SwashCache::get_image_uncached`)
//! and re-upload (`Queue::write_texture`) **every cached glyph**, once per
//! glyph, on the main thread. Growth now records one GPU texture-to-texture copy
//! of the whole old atlas, so the cost of a capacity boundary is independent of
//! how many glyphs are cached.
//!
//! These tests drive real prepared and presented frames through the headless
//! wgpu renderer and read the mechanism counters
//! (`Renderer::text_atlas_statistics`). They require a working wgpu adapter; if
//! none can be acquired they **skip with a printed reason** rather than
//! fabricating a result.

use iced_wgpu::Renderer;
use iced_wgpu::core::alignment;
use iced_wgpu::core::renderer::{Headless, Renderer as _, Settings};
use iced_wgpu::core::text::{Alignment, Ellipsis, LineHeight, Renderer as _, Shaping, Wrapping};
use iced_wgpu::core::{Color, Font, Pixels, Point, Rectangle, Size, Text};

const W: u32 = 320;
const H: u32 = 240;
const BG: Color = Color::BLACK;
const FG: Color = Color::WHITE;

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(W as f32, H as f32))
}

fn text(content: &str, size: f32) -> Text {
    Text {
        content: content.to_owned(),
        bounds: Size::new(W as f32, H as f32),
        size: Pixels(size),
        line_height: LineHeight::Relative(1.2),
        font: Font::DEFAULT,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::Word,
        ellipsis: Ellipsis::None,
        hint_factor: None,
    }
}

/// Draws every `(content, size)` and presents a frame, returning its RGBA bytes.
fn frame_of(renderer: &mut Renderer, runs: &[(&str, f32)]) -> Vec<u8> {
    renderer.reset(bounds());

    for (content, size) in runs {
        renderer.fill_text(text(content, *size), Point::new(4.0, 4.0), FG, bounds());
    }

    <Renderer as Headless>::screenshot(renderer, Size::new(W, H), 1.0, BG)
}

/// Draws `content` at `size` and presents a frame, returning its RGBA bytes.
fn frame(renderer: &mut Renderer, content: &str, size: f32) -> Vec<u8> {
    frame_of(renderer, &[(content, size)])
}

/// Non-background pixels, i.e. "this frame actually drew glyphs".
fn inked(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|p| p[0..3] != [0, 0, 0])
        .count()
}

/// A long string of distinct code points, enough to overflow a 256x256 and then
/// a 512x512 mask atlas at a large size. Latin, Greek, Cyrillic, CJK and
/// combining marks, so several scripts cross the boundary.
fn many_glyphs() -> String {
    let mut content = String::new();

    for c in ('\u{0041}'..='\u{007a}')
        .chain('\u{00c0}'..='\u{017f}')
        .chain('\u{0386}'..='\u{03ce}')
        .chain('\u{0400}'..='\u{045f}')
        .chain('\u{4e00}'..='\u{4e80}')
    {
        content.push(c);
    }

    content
}

/// Forces the atlas past at least one growth boundary, returning the number of
/// growths observed.
///
/// `keep` is drawn, at `keep_size`, in every growth frame, which marks exactly
/// the glyphs the caller cares about in use for that frame so the packer's LRU
/// cannot evict them: eviction is a different mechanism from growth, and a test
/// about growth must not measure it.
fn force_growth(renderer: &mut Renderer, keep: Option<(&str, f32)>) -> u64 {
    let before = renderer.text_atlas_statistics().growths;

    for size in [28.0, 36.0, 44.0, 52.0] {
        // `keep` is drawn FIRST so its glyphs are marked in use before the load
        // asks the packer for space: otherwise the packer evicts them, the
        // trailing draw rasterizes them again into the grown atlas, and the
        // test would no longer be observing the preserving copy at all.
        let load = many_glyphs();
        let mut runs: Vec<(&str, f32)> = keep.into_iter().collect();
        runs.push((load.as_str(), size));

        let _ = frame_of(renderer, &runs);
    }

    renderer.text_atlas_statistics().growths - before
}

#[test]
fn growth_copies_the_atlas_on_the_gpu_instead_of_re_rasterizing_it() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // A first frame of ordinary text: no growth yet, and every glyph in it was
    // rasterized exactly once and uploaded exactly once.
    let _ = frame(&mut renderer, "The quick brown fox", 16.0);

    let warm = renderer.text_atlas_statistics();
    assert_eq!(warm.growths, 0, "a short line should fit the initial atlas");
    assert!(warm.glyphs_rasterized > 0, "no glyph was rasterized at all");
    assert_eq!(
        warm.glyph_queue_writes,
        warm.glyphs_rasterized - warm.glyphs_without_pixels,
        "one upload per rasterized glyph with pixels"
    );

    // A repeat of the same line rasterizes nothing: the atlas is a cache.
    let _ = frame(&mut renderer, "The quick brown fox", 16.0);
    assert_eq!(
        renderer.text_atlas_statistics().glyphs_rasterized,
        warm.glyphs_rasterized,
        "a cached line must not rasterize again"
    );

    // Now cross at least one capacity boundary.
    let growths = force_growth(&mut renderer, None);
    assert!(growths >= 1, "the atlas never grew; the load is too small");

    let grown = renderer.text_atlas_statistics();

    // The mechanism: one GPU copy per growth, no CPU re-rasterization, and
    // still exactly one upload per rasterized glyph -- growth added none.
    assert_eq!(
        grown.glyphs_rerasterized_on_growth, 0,
        "growth must not rasterize a glyph that is already cached"
    );
    assert_eq!(
        grown.glyph_queue_writes,
        grown.glyphs_rasterized - grown.glyphs_without_pixels,
        "growth must not add glyph uploads"
    );
    assert_eq!(
        grown.texture_copies, grown.growths,
        "every growth must preserve the old atlas with exactly one GPU copy"
    );
    assert!(
        grown.texture_copy_bytes >= 256 * 256,
        "the first growth alone copies the whole 256x256 atlas, got {}",
        grown.texture_copy_bytes
    );
    assert!(
        grown.mask_size >= 512,
        "the mask atlas should have doubled at least once, got {}",
        grown.mask_size
    );
}

#[test]
fn glyphs_cached_before_growth_are_pixel_identical_after_it() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    const LINE: &str = "Handgloves 123 — Ωμέγα Ъ";

    let before = frame(&mut renderer, LINE, 16.0);
    assert!(inked(&before) > 0, "the control frame drew nothing");

    // Zero control: the same input twice, with no growth in between, must be
    // byte-identical. Without this, the comparison below could pass on a
    // renderer that draws nothing at all.
    let zero_control = frame(&mut renderer, LINE, 16.0);
    assert_eq!(before, zero_control, "same input rendered differently");

    // Positive control: a different string must differ, so the comparison is
    // capable of observing a change.
    let other = frame(&mut renderer, "Something else entirely", 16.0);
    assert_ne!(before, other, "different input rendered identically");

    let rasterized_before = renderer.text_atlas_statistics().glyphs_rasterized;
    let growths = force_growth(&mut renderer, Some((LINE, 16.0)));
    assert!(growths >= 1, "the atlas never grew; the load is too small");
    assert!(
        renderer.text_atlas_statistics().glyphs_rasterized > rasterized_before,
        "the growth load rasterized nothing, so it proves nothing"
    );

    // The glyphs of LINE were cached before the growth and moved with the GPU
    // copy: same pixels, and not one of them rasterized again.
    let rasterized_at_growth = renderer.text_atlas_statistics().glyphs_rasterized;
    let after = frame(&mut renderer, LINE, 16.0);

    assert_eq!(
        renderer.text_atlas_statistics().glyphs_rasterized,
        rasterized_at_growth,
        "a glyph cached before growth was rasterized again after it"
    );
    assert_eq!(before, after, "a glyph changed pixels across atlas growth");
}

#[test]
fn newly_revealed_glyphs_are_drawn_in_the_frame_that_grew_the_atlas() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // Load the atlas to just under a boundary, then reveal a large unseen set in
    // one frame: that frame both grows the atlas and must still draw its text.
    let _ = frame(&mut renderer, &many_glyphs(), 24.0);

    let before = renderer.text_atlas_statistics().growths;
    let grew = frame(&mut renderer, &many_glyphs(), 48.0);
    let after = renderer.text_atlas_statistics();

    assert!(after.growths > before, "this frame did not grow the atlas");
    assert!(
        inked(&grew) > 500,
        "the growing frame drew almost nothing ({} inked pixels): new glyphs \
         were deferred or dropped",
        inked(&grew)
    );
    assert_eq!(after.growth_failures, 0, "growth was refused unexpectedly");
}

#[test]
fn a_capped_atlas_reports_exhaustion_instead_of_growing_without_bound() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    // A VRAM budget of one initial atlas: growth must be refused, counted, and
    // survivable -- the frame still presents.
    renderer.set_text_atlas_max_size(256);

    for size in [24.0, 40.0, 56.0] {
        let pixels = frame(&mut renderer, &many_glyphs(), size);
        assert_eq!(pixels.len() as u32, W * H * 4, "the frame did not present");
    }

    let capped = renderer.text_atlas_statistics();

    assert_eq!(capped.growths, 0, "the cap did not bound growth");
    assert!(
        capped.growth_failures > 0,
        "exhaustion was not reported: growth_failures stayed 0"
    );
    assert_eq!(capped.mask_size, 256, "a capped atlas must not grow");
    assert_eq!(capped.texture_copies, 0, "a refused growth must not copy");

    // The cap is a bound on future growth, never a shrink: raising it lets the
    // atlas grow again, which proves the refusal was the cap and not a broken
    // atlas.
    renderer.set_text_atlas_max_size(2048);
    let _ = frame(&mut renderer, &many_glyphs(), 40.0);

    let raised = renderer.text_atlas_statistics();
    assert!(raised.growths > 0, "raising the cap did not allow growth");
    assert_eq!(
        raised.texture_copies, raised.growths,
        "growth after a refusal must still preserve the atlas by copy"
    );
}

/// A run of distinct color-emoji code points; each is a large bitmap, so a
/// handful of them overflow the 256x256 *color* atlas.
fn many_emoji(count: u32) -> String {
    (0x1f600..0x1f600 + count)
        .filter_map(char::from_u32)
        .flat_map(|c| [c, ' '])
        .collect()
}

#[test]
fn color_glyphs_survive_a_color_atlas_growth_without_aliasing_mask_tiles() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available in this environment");
        return;
    };

    const EMOJI: &str = "\u{1f600}\u{1f601}\u{1f602}";
    const LATIN: &str = "mask tiles";

    let before = frame_of(&mut renderer, &[(EMOJI, 24.0), (LATIN, 16.0)]);
    let warm = renderer.text_atlas_statistics();

    if warm.color_size == 0 || inked(&before) == 0 {
        eprintln!("SKIP: no color emoji font available in this environment");
        return;
    }

    // Zero control: identical input, no growth in between.
    assert_eq!(
        before,
        frame_of(&mut renderer, &[(EMOJI, 24.0), (LATIN, 16.0)]),
        "same input rendered differently"
    );

    // Grow the color atlas, keeping the glyphs under test in use (drawn first,
    // so the packer cannot evict and re-upload them).
    let mut growths = 0;

    for size in [20.0, 28.0, 36.0, 44.0] {
        let load = many_emoji(64);
        let _ = frame_of(
            &mut renderer,
            &[(EMOJI, 24.0), (LATIN, 16.0), (&load, size)],
        );
        growths = renderer.text_atlas_statistics().growths;
    }

    let grown = renderer.text_atlas_statistics();

    if grown.color_size <= 256 {
        eprintln!(
            "SKIP: no color glyph reached the color atlas in this environment \
             (it stayed at {}x{} over {growths} growth(s)), so the color path \
             is unexercised here",
            grown.color_size, grown.color_size
        );
        return;
    }

    assert_eq!(
        grown.glyphs_rerasterized_on_growth, 0,
        "color atlas growth rasterized cached glyphs again"
    );
    assert!(
        grown.texture_copy_bytes >= 256 * 256 * 4,
        "a color atlas growth copies 4 bytes per texel, got {}",
        grown.texture_copy_bytes
    );

    // Both the color glyphs and the mask glyphs drawn alongside them are
    // unchanged: neither kind picked up a stale or mismatched tile.
    let after = frame_of(&mut renderer, &[(EMOJI, 24.0), (LATIN, 16.0)]);
    assert_eq!(before, after, "a glyph changed pixels across color growth");
}
