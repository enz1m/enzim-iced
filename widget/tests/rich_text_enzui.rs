// Modified by enzUI contributors. See FORK.md.
//! enzICED deviations of rich text, pinned.

use iced_runtime::UserInterface;
use iced_runtime::core::renderer::Headless as _;
use iced_runtime::core::text::Span;
use iced_runtime::core::{Color, Element, Size, Theme, mouse, renderer};
use iced_runtime::user_interface::Cache;
use iced_widget::text::Rich;

type Renderer = iced_tiny_skia::Renderer;

const BAKED: Color = Color::from_rgb(1.0, 0.0, 0.0);
const THEMED: Color = Color::from_rgb(0.0, 0.0, 1.0);
const KEY: u64 = 7;

/// Pixels of one keyed highlight drawn under `Theme::Dark`, with or without
/// a resolver that maps its key to [`THEMED`].
fn render(resolved: bool) -> Vec<u8> {
    let span: Span<'static, ()> = Span::new("highlighted")
        .background(BAKED)
        .highlight_key(KEY)
        .padding([0, 4]);
    let rich: Rich<'static, (), (), Theme> = Rich::with_spans([span]);
    let rich = if resolved {
        rich.resolve_color(|_, key| (key == KEY).then_some(THEMED))
    } else {
        rich
    };
    let element: Element<'static, (), Theme, Renderer> = rich.into();

    let mut renderer = Renderer::new(renderer::Settings::default());
    let size = Size::new(200.0, 40.0);
    let mut ui = UserInterface::build(element, size, Cache::default(), &mut renderer);
    let theme = Theme::Dark;
    ui.draw(
        &mut renderer,
        &theme,
        &renderer::Style {
            text_color: Color::WHITE,
        },
        mouse::Cursor::Unavailable,
    );
    renderer.screenshot(Size::new(200, 40), 1.0, Color::BLACK)
}

fn count(buf: &[u8], color: Color) -> usize {
    let [r, g, b, _] = color.into_rgba8();
    buf.chunks_exact(4)
        .filter(|p| p[0] == r && p[1] == g && p[2] == b)
        .count()
}

/// A highlight's background was baked when the view was built, so a keyed
/// ink followed a theme switch while the fill behind it did not.
///
/// Sabotage: paint `highlight.background` instead of the resolved one and
/// the themed fill disappears.
#[test]
fn a_keyed_highlight_background_follows_the_resolver() {
    let baked = render(false);
    assert!(
        count(&baked, BAKED) > 0 && count(&baked, THEMED) == 0,
        "calibration: without a resolver the baked background paints"
    );

    let themed = render(true);
    assert_eq!(
        count(&themed, BAKED),
        0,
        "the baked fallback must not paint"
    );
    assert!(
        count(&themed, THEMED) > 0,
        "the resolved background must paint"
    );
}
