// Modified by enzUI contributors. See FORK.md.
//! Theme-reactive `rich_text` span colours.
//!
//! `Span::color` is baked when the view is built, so a *semantic* colour
//! ("muted", "accent") is pinned to whichever theme was active at build time
//! and keeps painting the old palette's ink after a theme switch.
//! `Span::color_key` + `Rich::resolve_color` resolve the colour from the live
//! `&Theme` at draw time instead.
//!
//! These tests assert on **rendered pixels**, not on stored fields: a test that
//! only checked the span remembers its key would pass even if the key never
//! reached the rasteriser.

use iced::advanced::renderer::Headless as _;
use iced::advanced::text::Span;
use iced::widget::rich_text;
use iced::{Color, Element, Font, Never, Size, Theme};
use iced_runtime::user_interface::{self, UserInterface};

type Renderer = iced_tiny_skia::Renderer;
type View = Element<'static, Never, Theme, Renderer>;

/// The `color_key` used for the "semantic" run under test.
const MUTED: u64 = 7;

/// Colour resolved for [`MUTED`] under a dark theme.
const DARK_INK: Color = Color::from_rgb(1.0, 0.0, 0.0);
/// Colour resolved for [`MUTED`] under a light theme.
const LIGHT_INK: Color = Color::from_rgb(0.0, 0.0, 1.0);

/// The colour baked into the span at build time. Deliberately distinct from
/// both resolved inks, so a painted pixel identifies *which* path produced it.
const BAKED: Color = Color::from_rgb(0.0, 1.0, 0.0);

const SIZE: Size = Size::new(300.0, 100.0);
const SCALE: f32 = 1.0;

/// A `rich_text` whose single span is both baked and keyed, with a resolver
/// installed that answers [`MUTED`] differently per theme.
fn themed_view() -> View {
    rich_text([Span::<Never>::new("MMMM").color(BAKED).color_key(MUTED)])
        .size(40)
        .font(Font::MONOSPACE)
        .resolve_color(|theme: &Theme, key| {
            (key == MUTED).then(|| {
                if theme.palette().is_dark {
                    DARK_INK
                } else {
                    LIGHT_INK
                }
            })
        })
        .into()
}

/// The same view with **no** resolver installed — the additivity control.
fn unthemed_view() -> View {
    rich_text([Span::<Never>::new("MMMM").color(BAKED).color_key(MUTED)])
        .size(40)
        .font(Font::MONOSPACE)
        .into()
}

/// Lays out `view`, draws it under `theme`, and returns the RGBA pixels.
fn render(view: View, theme: &Theme) -> Vec<u8> {
    use iced::advanced::mouse;
    use iced::theme::Base as _;

    let mut renderer = Renderer::new(iced::advanced::renderer::Settings::default());

    let mut ui = UserInterface::build(view, SIZE, user_interface::Cache::default(), &mut renderer);

    let base = theme.base();

    ui.draw(
        &mut renderer,
        theme,
        &iced::advanced::renderer::Style {
            text_color: base.text_color,
        },
        mouse::Cursor::Unavailable,
    );

    renderer.screenshot(
        Size::new((SIZE.width * SCALE) as u32, (SIZE.height * SCALE) as u32),
        SCALE,
        base.background_color,
    )
}

/// Renders the *same* interface twice, keeping the widget tree and its cached
/// paragraph alive across the two draws, changing only the theme.
///
/// This is the case the fix actually has to survive: a theme switch with no
/// rebuild of the view.
fn render_twice(view: View, first: &Theme, second: &Theme) -> (Vec<u8>, Vec<u8>) {
    use iced::advanced::mouse;
    use iced::theme::Base as _;

    let mut renderer = Renderer::new(iced::advanced::renderer::Settings::default());

    let mut ui = UserInterface::build(view, SIZE, user_interface::Cache::default(), &mut renderer);

    let shot = |theme: &Theme, renderer: &mut Renderer, ui: &mut UserInterface<'_, _, _, _>| {
        let base = theme.base();

        ui.draw(
            renderer,
            theme,
            &iced::advanced::renderer::Style {
                text_color: base.text_color,
            },
            mouse::Cursor::Unavailable,
        );

        renderer.screenshot(
            Size::new((SIZE.width * SCALE) as u32, (SIZE.height * SCALE) as u32),
            SCALE,
            base.background_color,
        )
    };

    let a = shot(first, &mut renderer, &mut ui);
    let b = shot(second, &mut renderer, &mut ui);

    (a, b)
}

/// Counts pixels within `tolerance` of `color`, ignoring near-transparent ones.
fn count_near(rgba: &[u8], color: Color, tolerance: u8) -> usize {
    let [r, g, b, _] = color.into_rgba8();

    rgba.chunks_exact(4)
        .filter(|px| {
            px[3] > 128
                && px[0].abs_diff(r) <= tolerance
                && px[1].abs_diff(g) <= tolerance
                && px[2].abs_diff(b) <= tolerance
        })
        .count()
}

/// The bounding box of all pixels within tolerance of `color`.
fn ink_bbox(rgba: &[u8], color: Color) -> Option<(usize, usize, usize, usize)> {
    let width = (SIZE.width * SCALE) as usize;
    let [r, g, b, _] = color.into_rgba8();

    let mut bbox: Option<(usize, usize, usize, usize)> = None;

    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let hit = px[3] > 128
            && px[0].abs_diff(r) <= 24
            && px[1].abs_diff(g) <= 24
            && px[2].abs_diff(b) <= 24;

        if hit {
            let (x, y) = (i % width, i / width);

            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }

    bbox
}

/// The headline claim: a keyed span paints a **different** colour under a dark
/// theme than under a light one.
#[test]
fn keyed_span_repaints_on_theme_change() {
    let dark = render(themed_view(), &Theme::Dark);
    let light = render(themed_view(), &Theme::Light);

    let dark_hits = count_near(&dark, DARK_INK, 24);
    let light_hits = count_near(&light, LIGHT_INK, 24);

    // Calibration: prove the probe can see ink at all before trusting a zero.
    assert!(
        dark_hits > 20,
        "dark theme must paint the dark-resolved ink; got {dark_hits} px"
    );
    assert!(
        light_hits > 20,
        "light theme must paint the light-resolved ink; got {light_hits} px"
    );

    // The actual defect: the two themes must not paint the same ink.
    assert_eq!(
        count_near(&dark, LIGHT_INK, 24),
        0,
        "dark theme must not paint the light theme's ink"
    );
    assert_eq!(
        count_near(&light, DARK_INK, 24),
        0,
        "light theme must not paint the dark theme's ink"
    );

    // And neither may fall back to the colour baked at build time.
    assert_eq!(
        count_near(&dark, BAKED, 24),
        0,
        "resolved colour must take precedence over the baked Span::color"
    );
    assert_eq!(
        count_near(&light, BAKED, 24),
        0,
        "resolved colour must take precedence over the baked Span::color"
    );
}

/// The same claim across a **live** theme switch: one widget tree, one cached
/// paragraph, two draws. This is what a running app does, and it is the case a
/// build-once test cannot see.
#[test]
fn keyed_span_repaints_without_a_rebuild() {
    let (dark, light) = render_twice(themed_view(), &Theme::Dark, &Theme::Light);

    let dark_hits = count_near(&dark, DARK_INK, 24);
    let light_hits = count_near(&light, LIGHT_INK, 24);

    assert!(
        dark_hits > 20,
        "first draw must paint the dark-resolved ink; got {dark_hits} px"
    );
    assert!(
        light_hits > 20,
        "second draw must repaint in the light-resolved ink without a rebuild; \
         got {light_hits} px"
    );
    assert_eq!(
        count_near(&light, DARK_INK, 24),
        0,
        "the cached paragraph must not keep the previous theme's ink"
    );

    // And back again, to prove the cache invalidates in both directions.
    let (_, back) = render_twice(themed_view(), &Theme::Light, &Theme::Dark);

    assert!(
        count_near(&back, DARK_INK, 24) > 20,
        "switching back must restore the dark-resolved ink"
    );
    assert_eq!(
        count_near(&back, LIGHT_INK, 24),
        0,
        "switching back must not keep the light theme's ink"
    );
}

/// Additivity: with no resolver installed, a `color_key` changes nothing — the
/// baked `Span::color` still wins, on either theme.
#[test]
fn color_key_is_inert_without_a_resolver() {
    for theme in [Theme::Dark, Theme::Light] {
        let rgba = render(unthemed_view(), &theme);

        let baked_hits = count_near(&rgba, BAKED, 24);

        assert!(
            baked_hits > 20,
            "{theme} without a resolver must paint the baked Span::color; \
             got {baked_hits} px"
        );
        assert_eq!(
            count_near(&rgba, DARK_INK, 24) + count_near(&rgba, LIGHT_INK, 24),
            0,
            "{theme} must not resolve a key when no resolver is installed"
        );
    }
}

/// A resolver returning `None` means "no opinion": that span falls back to its
/// own baked `Span::color` rather than being blanked.
///
/// The paragraph deliberately mixes a **resolved** run with a **declined** one.
/// A paragraph where every key is declined would never enter the re-bake path
/// at all, so it could not distinguish "falls back correctly" from "the
/// fallback is never exercised".
#[test]
fn declined_key_falls_back_to_baked_color() {
    const DECLINED: u64 = MUTED + 1;

    let view: View = rich_text([
        Span::<Never>::new("MM").color(BAKED).color_key(MUTED),
        Span::<Never>::new("WW").color(BAKED).color_key(DECLINED),
    ])
    .size(40)
    .font(Font::MONOSPACE)
    // Only `MUTED` is answered; `DECLINED` is explicitly declined.
    .resolve_color(|_theme: &Theme, key| (key == MUTED).then_some(DARK_INK))
    .into();

    let rgba = render(view, &Theme::Dark);

    let resolved_hits = count_near(&rgba, DARK_INK, 24);
    let baked_hits = count_near(&rgba, BAKED, 24);

    // Calibration: the re-bake path must actually have run.
    assert!(
        resolved_hits > 20,
        "the answered key must paint the resolved ink; got {resolved_hits} px"
    );

    // The claim: the declined run kept its baked colour through that re-bake.
    assert!(
        baked_hits > 20,
        "a declined key must fall back to the baked Span::color; got {baked_hits} px"
    );
}

/// Re-shaping with resolved colours must not move a glyph: colour does not
/// affect metrics, so the themed paragraph has to occupy the same box as the
/// unthemed one.
#[test]
fn resolving_a_color_does_not_change_layout() {
    let unthemed = render(unthemed_view(), &Theme::Dark);
    let themed = render(themed_view(), &Theme::Dark);

    let baked = ink_bbox(&unthemed, BAKED).expect("unthemed run must paint");
    let resolved = ink_bbox(&themed, DARK_INK).expect("themed run must paint");

    assert_eq!(
        baked, resolved,
        "colour resolution must not move the glyphs: {baked:?} vs {resolved:?}"
    );
}

/// Changing the span **text** while a resolver is installed must repaint the
/// new text, not a cached paragraph shaped from the old text.
///
/// The themed paragraph is cached in widget state, so if its invalidation
/// tracked only the resolved colours it would keep painting stale glyphs after
/// an edit — a regression strictly worse than the colour bug this hook fixes,
/// because it corrupts content rather than styling. The `version` counter on
/// the cache exists for this; nothing else in this file exercises it.
#[test]
fn editing_the_text_invalidates_the_themed_cache() {
    use iced::advanced::mouse;
    use iced::theme::Base as _;

    let keyed = |text: &'static str| -> View {
        rich_text([Span::<Never>::new(text).color_key(MUTED)])
            .size(40)
            .font(Font::MONOSPACE)
            .resolve_color(|_theme: &Theme, key| (key == MUTED).then_some(DARK_INK))
            .into()
    };

    let mut renderer = Renderer::new(iced::advanced::renderer::Settings::default());
    let base = Theme::Dark.base();

    let mut shot = |view: View, cache: user_interface::Cache| {
        let mut ui = UserInterface::build(view, SIZE, cache, &mut renderer);

        ui.draw(
            &mut renderer,
            &Theme::Dark,
            &iced::advanced::renderer::Style {
                text_color: base.text_color,
            },
            mouse::Cursor::Unavailable,
        );

        let rgba = renderer.screenshot(
            Size::new((SIZE.width * SCALE) as u32, (SIZE.height * SCALE) as u32),
            SCALE,
            base.background_color,
        );

        (rgba, ui.into_cache())
    };

    // A narrow run, then a much wider one, reusing the same widget state.
    let (narrow, cache) = shot(keyed("I"), user_interface::Cache::default());
    let (wide, _) = shot(keyed("MMMMMMMM"), cache);

    let narrow_box = ink_bbox(&narrow, DARK_INK).expect("narrow run must paint");
    let wide_box = ink_bbox(&wide, DARK_INK).expect("wide run must paint");

    let narrow_w = narrow_box.2 - narrow_box.0;
    let wide_w = wide_box.2 - wide_box.0;

    // Calibration: the first draw really did go through the themed path.
    assert!(
        count_near(&narrow, DARK_INK, 24) > 5,
        "the resolved ink must be painted before the edit"
    );

    assert!(
        wide_w > narrow_w * 3,
        "the edited text must be re-shaped, not served from the themed cache: \
         narrow={narrow_w}px wide={wide_w}px"
    );
}
