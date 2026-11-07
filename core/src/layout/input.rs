// Modified by enzUI contributors. See FORK.md.
//! Single-line input geometry shared by native widgets and retained presentation.

use super::{Limits, Node};
use crate::{Length, Padding, Point, Rectangle, Size, alignment, renderer};
use std::sync::{Arc, Mutex};

/// Fixed icon metrics and placement in a single-line input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Icon {
    /// Natural glyph width in logical pixels.
    pub width: f32,
    /// Gap between icon and text in logical pixels.
    pub spacing: f32,
    /// Whether the icon precedes the text.
    pub leading: bool,
}

/// Width, line height, and padding owned by the input widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// Native width policy, including fill and bounded lengths.
    pub width: Length,
    /// Padding fitted inside the available native limits.
    pub padding: Padding,
    /// Absolute single-line height in logical pixels.
    pub line_height: f32,
    /// Optional icon measured by the native paragraph engine.
    pub icon: Option<Icon>,
}

/// A captured caret or selection rectangle relative to the unscrolled text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    /// Leading glyph coordinate in logical pixels.
    pub x: f32,
    /// Caret stroke or selected glyph span in logical pixels.
    pub width: f32,
}

/// A caret blink epoch shared by native and retained paint, without a widget callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blink {
    /// Monotonic epoch of the last caret activity; the first half-cycle is visible.
    pub epoch: crate::time::Instant,
    /// Duration of each visible or hidden half-cycle; zero refuses retained capture.
    pub interval: crate::time::Duration,
}

impl Blink {
    /// Samples the native on/off phase; zero intervals are invisible.
    /// Instants before a nonzero clock epoch remain visible.
    pub fn visible(self, at: crate::time::Instant) -> bool {
        !self.interval.is_zero()
            && (at.saturating_duration_since(self.epoch).as_nanos() / self.interval.as_nanos())
                .is_multiple_of(2)
    }
}

/// Paint-only input state copied into an immutable presentation program.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    /// Unwrapped paragraph width in logical pixels.
    pub text_width: f32,
    /// Horizontal alignment before caret scrolling.
    pub alignment: alignment::Horizontal,
    /// Focused cursor coordinate; absent for unfocused inputs.
    pub scroll_anchor: Option<f32>,
    /// Native caret or selection geometry; live paint omits the caret during its hidden phase.
    pub caret: Option<Caret>,
    /// Complete caret geometry and immutable blink clock, including blink-off captures.
    pub blink: Option<(Caret, Blink)>,
}

impl Default for Paint {
    fn default() -> Self {
        Self {
            text_width: 0.0,
            alignment: alignment::Horizontal::Left,
            scroll_anchor: None,
            caret: None,
            blink: None,
        }
    }
}

impl Paint {
    /// Horizontal displacement of the paragraph and caret at the allocated text width.
    pub fn offset(self, width: f32) -> f32 {
        let slack = (width - self.text_width).max(0.0);
        let align = match self.alignment {
            alignment::Horizontal::Left => 0.0,
            alignment::Horizontal::Center => slack / 2.0,
            alignment::Horizontal::Right => slack,
        };
        align
            - self
                .scroll_anchor
                .map_or(0.0, |x| (x + 5.0 - width).max(0.0))
    }

    pub(crate) fn valid(self) -> bool {
        self.text_width.is_finite()
            && self.text_width >= 0.0
            && self.scroll_anchor.is_none_or(|x| x.is_finite() && x >= 0.0)
            && self.caret.is_none_or(|c| {
                c.x.is_finite() && c.x >= 0.0 && c.width.is_finite() && c.width >= 0.0
            })
            && self.blink.is_none_or(|(c, clock)| {
                !clock.interval.is_zero()
                    && c.x.is_finite()
                    && c.x >= 0.0
                    && c.width.is_finite()
                    && c.width > 0.0
                    && self.caret.is_none_or(|visible| visible == c)
            })
    }
}

/// Resolved input allocation and its text/icon regions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// Total native allocation.
    pub size: Size,
    /// Editable text viewport relative to the input origin.
    pub text: Rectangle,
    /// Icon allocation; empty when no icon is present.
    pub icon: Rectangle,
}

impl Shape {
    /// Resolves native constraints without querying widget or renderer state.
    pub fn resolve(self, limits: &Limits) -> Geometry {
        let padding = self.padding.fit(Size::ZERO, limits.max());
        let inner = limits.width(self.width).shrink(padding);
        let text_size = inner.resolve(self.width, self.line_height, Size::ZERO);
        let mut text = Rectangle::new(Point::new(padding.left, padding.top), text_size);
        let mut icon = Rectangle::new(text.position(), Size::ZERO);
        if let Some(glyph) = self.icon {
            text.width = (text.width - glyph.width - glyph.spacing).max(0.0);
            icon.width = glyph.width;
            icon.height = text.height;
            if glyph.leading {
                text.x += glyph.width + glyph.spacing;
            } else {
                icon.x += text_size.width - glyph.width;
            }
        }
        Geometry {
            size: text_size.expand(padding),
            text,
            icon,
        }
    }

    pub(crate) fn valid(self) -> bool {
        self.line_height.is_finite()
            && self.line_height >= 0.0
            && [
                self.padding.left,
                self.padding.right,
                self.padding.top,
                self.padding.bottom,
            ]
            .into_iter()
            .all(|v| v.is_finite() && v >= 0.0)
            && self.icon.is_none_or(|i| {
                i.width.is_finite() && i.width >= 0.0 && i.spacing.is_finite() && i.spacing >= 0.0
            })
    }
}

/// UI-side paint snapshot retained by native layout clones.
/// Worker programs contain a copied [`Paint`], not this synchronization object.
#[derive(Debug, Default)]
pub struct LivePaint(Mutex<Paint>);

impl LivePaint {
    /// Replaces the paint snapshot after a native input state transition.
    pub fn set(&self, paint: Paint) {
        *self.0.lock().expect("input paint state poisoned") = paint;
    }

    /// Copies the current snapshot during native drawing or UI-side lowering.
    pub fn get(&self) -> Paint {
        *self.0.lock().expect("input paint state poisoned")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    pub limits: Limits,
    pub shape: Shape,
    pub paint: Arc<LivePaint>,
}

/// Declares one input's background, text clip, paragraph, caret and optional icon.
/// The paint snapshot is copied during UI-side lowering, never by the presentation worker.
pub fn node(
    limits: &Limits,
    shape: Shape,
    paint: Arc<LivePaint>,
    text: renderer::NodeId,
    icon: renderer::NodeId,
) -> Node {
    let g = shape.resolve(limits);
    let text_node = Node::new(g.text.size()).with_text_presentation(
        text,
        Size::new(Length::Fill, Length::Fixed(g.text.height)),
        Size::new(f32::INFINITY, g.text.height),
    );
    let caret = paint.get().caret;
    let caret_size = Size::new(
        caret.map_or(0.0, |c| c.width),
        if caret.is_some() { g.text.height } else { 0.0 },
    );
    let caret_node = super::paint_box(
        &Limits::NONE,
        Length::Fixed(caret_size.width),
        Length::Fixed(caret_size.height),
    )
    .move_to(Point::new(
        caret.map_or(0.0, |c| c.x + paint.get().offset(g.text.width)),
        0.0,
    ));
    let body = Node::with_children(g.text.size(), vec![text_node, caret_node])
        .with_overlay_presentation(
            *limits,
            Size::new(Length::Fill, Length::Fixed(g.text.height)),
            0,
            vec![
                Size::new(Length::Fill, Length::Fixed(g.text.height)),
                Size::new(Length::Shrink, Length::Shrink),
            ],
        );
    let clipped = Node::with_children(g.text.size(), vec![body])
        .with_clip_presentation(
            *limits,
            Size::new(Length::Fill, Length::Fixed(g.text.height)),
            Padding::ZERO,
        )
        .move_to(g.text.position());
    let icon = if shape.icon.is_some() {
        Node::new(g.icon.size())
            .with_text_presentation(
                icon,
                Size::new(Length::Fixed(g.icon.width), Length::Fixed(g.icon.height)),
                Size::new(f32::INFINITY, g.icon.height),
            )
            .move_to(g.icon.position())
    } else {
        super::void()
    };
    Node::with_children(g.size, vec![clipped, icon]).with_input_presentation(Declaration {
        limits: *limits,
        shape,
        paint,
    })
}
