// Modified by enzUI contributors. See FORK.md.
//! Scroll viewport boundaries in titlebar extent and spill protection.

#[test]
fn priority_band_stops_at_a_declared_scroll_viewport_but_rejects_uncontained_spill() {
    use iced_core::layout::{self, Limits, Node};
    use iced_core::{Length, Point, Size};
    let viewport = Size::new(900.0, 44.0);
    let material = Size::new(2340.0, 44.0);
    let raw = Node::with_children(viewport, vec![Node::new(material)]);
    let scroll = raw.clone().with_scroll_presentation(
        Limits::new(Size::ZERO, viewport),
        Size::new(Length::Fill, Length::Fixed(44.0)),
        Size::new(Length::Fixed(2340.0), Length::Fixed(44.0)),
        layout::scroll::Config {
            horizontal: Some(layout::scroll::Bar {
                width: 0.0,
                margin: 0.0,
                scroller_width: 0.0,
                scroller_min_length: 0.0,
                from_end: false,
                spacing: None,
                padding: [0.0, 0.0],
            }),
            vertical: None,
        },
    );
    let contained = Node::with_children(viewport, vec![scroll]);
    assert_eq!(
        layout::priority_band::subtree_extent(&contained).size(),
        viewport
    );
    assert_eq!(
        layout::priority_band::content_clip(layout::Placed::new(Point::ORIGIN, &contained), 0.01)
            .size(),
        viewport
    );
    assert_eq!(layout::priority_band::subtree_extent(&raw).size(), material);
    assert_eq!(
        layout::priority_band::content_clip(layout::Placed::new(Point::ORIGIN, &raw), 0.01).width,
        0.0
    );
}
