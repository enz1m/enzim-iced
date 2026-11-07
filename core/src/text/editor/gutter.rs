// Modified by enzUI contributors. See FORK.md.
//! Line numbers positioned by the editor's own visible logical-line heads.

use crate::{Color, Padding, Rectangle};

/// One logical-line head in text-viewport coordinates; continuation rows have no entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    /// Zero-based index in the editor's content.
    pub line: usize,
    /// Top edge relative to the text viewport, including its scroll translation.
    pub top: f32,
    /// Height of the first visual row in logical pixels.
    pub height: f32,
}

/// A fixed-width leading gutter sharing editor wrapping and scroll state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gutter {
    /// Reserved logical width, including horizontal padding on both sides.
    pub width: f32,
    /// Horizontal label inset; zero by default.
    pub padding: f32,
    /// Document line origin for a projected editor slice; zero by default.
    pub line_offset: usize,
    /// Number ink; `None` uses the editor's normal ink.
    pub color: Option<Color>,
}
impl Gutter {
    /// Reserves a nonnegative finite width; invalid widths disable the reservation.
    pub fn new(width: f32) -> Self {
        Self {
            width: if width.is_finite() {
                width.max(0.0)
            } else {
                0.0
            },
            padding: 0.0,
            line_offset: 0,
            color: None,
        }
    }
    /// Adds nonnegative horizontal label padding within the reserved width.
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = if padding.is_finite() {
            padding.max(0.0).min(self.width / 2.0)
        } else {
            0.0
        };
        self
    }
    /// Sets the zero-based document origin of the editor's first logical line.
    pub fn line_offset(mut self, line_offset: usize) -> Self {
        self.line_offset = line_offset;
        self
    }
    /// Overrides the editor ink for line numbers.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    /// Extends the leading text inset without changing the outer editor allocation.
    pub fn content_padding(self, mut padding: Padding) -> Padding {
        padding.left += self.width;
        padding
    }
    /// Number viewport inside the editor's text padding, excluding trailing text insets.
    pub fn bounds(self, outer: Rectangle, padding: Padding) -> Rectangle {
        let text = outer.shrink(padding);
        Rectangle {
            x: text.x - self.width + self.padding,
            y: text.y,
            width: (self.width - 2.0 * self.padding).max(0.0),
            height: text.height,
        }
    }
    /// Formats a one-based number; overflow refuses instead of wrapping the document origin.
    pub fn number(self, row: Row) -> Option<String> {
        self.line_offset
            .checked_add(row.line)?
            .checked_add(1)
            .map(|n| n.to_string())
    }
    pub(crate) fn valid(self) -> bool {
        self.width.is_finite()
            && self.width >= 0.0
            && self.padding.is_finite()
            && self.padding >= 0.0
            && self.padding <= self.width / 2.0
            && self
                .color
                .is_none_or(|c| [c.r, c.g, c.b, c.a].into_iter().all(f32::is_finite))
    }
}

/// A read-only geometry snapshot exposed through the editor's custom widget operation.
#[derive(Debug, Clone)]
pub struct View {
    /// Number viewport in window coordinates.
    pub bounds: Rectangle,
    /// Live source rows, relative to the text viewport.
    pub rows: Vec<Row>,
    /// Current sparse editor scroll geometry, not an independently estimated gutter offset.
    pub scroll: super::ScrollMetrics,
    /// Numbering policy used by this editor.
    pub gutter: Gutter,
}
