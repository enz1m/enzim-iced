// Modified by enzUI contributors. See FORK.md.
//! Scroll chrome rebuilt from native geometry, without widget or theme callbacks.
use crate::layout::scroll::{Bars, Config, Input};
use crate::{Background, Border, Color, Rectangle, renderer};

/// Resolved paint for one rail and its thumb.
#[derive(Debug, Clone, Copy)]
pub struct Rail {
    /// Optional rail fill.
    pub background: Option<Background>,
    /// Rail border metrics.
    pub border: Border,
    /// Thumb fill.
    pub thumb: Background,
    /// Thumb border metrics.
    pub thumb_border: Border,
}
/// Resolved scroll appearance, captured at draw time.
#[derive(Debug, Clone, Copy)]
pub struct Paint {
    /// Optional background quad; sampling changes allocation, not style metrics.
    pub background: Option<(renderer::Quad, Background)>,
    /// Horizontal and vertical rail styles.
    pub rails: [Rail; 2],
    /// Fill between simultaneously visible rails.
    pub gap: Option<Background>,
}
impl Paint {
    /// Background paint at the current viewport allocation.
    pub fn background(&self, bounds: Rectangle) -> Option<(renderer::Quad, Background)> {
        self.background
            .map(|(quad, fill)| (renderer::Quad { bounds, ..quad }, fill))
    }
    /// Returns chrome in layout coordinates; apply ancestor scroll translation afterward.
    pub fn chrome(
        &self,
        config: Config,
        input: Input,
        bounds: Rectangle,
        content: Rectangle,
    ) -> Vec<(renderer::Quad, Background)> {
        let bars = Bars::new(config, input, bounds, content);
        let mut quads = Vec::new();
        for (bar, style) in [(bars.y, self.rails[1]), (bars.x, self.rails[0])] {
            let Some(bar) = bar else { continue };
            if bar.bounds.width > 0.0
                && bar.bounds.height > 0.0
                && (style.background.is_some()
                    || (style.border.color != Color::TRANSPARENT && style.border.width > 0.0))
            {
                quads.push((
                    renderer::Quad {
                        bounds: bar.bounds,
                        border: style.border,
                        ..Default::default()
                    },
                    style
                        .background
                        .unwrap_or(Background::Color(Color::TRANSPARENT)),
                ));
            }
            if let Some(bounds) = bar.scroller
                && bounds.width > 0.0
                && bounds.height > 0.0
                && (style.thumb != Background::Color(Color::TRANSPARENT)
                    || (style.thumb_border.color != Color::TRANSPARENT
                        && style.thumb_border.width > 0.0))
            {
                quads.push((
                    renderer::Quad {
                        bounds,
                        border: style.thumb_border,
                        ..Default::default()
                    },
                    style.thumb,
                ));
            }
        }
        if let (Some(x), Some(y), Some(fill)) = (
            bars.x,
            bars.y,
            self.gap.or(self.background.map(|(_, fill)| fill)),
        ) {
            quads.push((
                renderer::Quad {
                    bounds: Rectangle {
                        x: y.bounds.x,
                        y: x.bounds.y,
                        width: y.bounds.width,
                        height: x.bounds.height,
                    },
                    ..Default::default()
                },
                fill,
            ));
        }
        quads
    }
}
