// Modified by enzUI contributors. See FORK.md.
//! A cached text group that only moves, as a scrolled canvas does, repaints
//! where it was and where it now is (enzUI-ICED #3447).

use std::sync::Arc;

use iced_tiny_skia::Layer;
use iced_tiny_skia::core::text::{Alignment, Ellipsis, Shaping, Wrapping};
use iced_tiny_skia::core::{
    Color, Font, Pixels, Point, Rectangle, Size, Transformation, alignment,
};
use iced_tiny_skia::graphics::Layer as _;
use iced_tiny_skia::graphics::text::Text;

fn frame(cache: &Arc<[Text]>, offset: f32) -> Layer {
    let mut layer = Layer::with_bounds(Rectangle::with_size(Size::new(400.0, 400.0)));
    layer.draw_text_cache(
        cache.clone(),
        Rectangle::with_size(Size::new(400.0, 400.0)),
        Transformation::translate(0.0, offset),
    );
    layer
}

#[test]
fn a_moved_text_cache_damages_its_new_place() {
    let cache: Arc<[Text]> = Arc::new([Text::Cached {
        content: "Administrator".into(),
        bounds: Rectangle::new(Point::new(10.0, 10.0), Size::new(120.0, 20.0)),
        color: Color::WHITE,
        size: Pixels(14.0),
        line_height: Pixels(20.0),
        font: Font::DEFAULT,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Basic,
        wrapping: Wrapping::None,
        ellipsis: Ellipsis::None,
        clip_bounds: Rectangle::with_size(Size::new(400.0, 400.0)),
    }]);
    let damage = Layer::damage(&frame(&cache, 0.0), &frame(&cache, 100.0));
    let moved_to = Rectangle::new(Point::new(10.0, 110.0), Size::new(120.0, 20.0));
    let was_at = Rectangle::new(Point::new(10.0, 10.0), Size::new(120.0, 20.0));
    assert!(
        damage.iter().any(|r| r.contains(moved_to.center())),
        "{damage:?}"
    );
    assert!(
        damage.iter().any(|r| r.contains(was_at.center())),
        "{damage:?}"
    );
}
