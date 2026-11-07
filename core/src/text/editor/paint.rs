// Modified by enzUI contributors. See FORK.md.
//! Editor paint data shared by live drawing and owned replay.
use super::{Editor, Position, Selection};
use crate::{Background, Border, Color, Padding, Pixels, Rectangle, Size, Text, renderer, text};
use std::{borrow::Cow, ops::Range};

/// One editor viewport's paint inputs; no widget state or callbacks are retained.
#[derive(Debug, Clone)]
pub struct Paint<'a, Font: Copy> {
    /// Logical outer allocation.
    pub bounds: Rectangle,
    /// Text insets inside the allocation.
    pub padding: Padding,
    /// Optional numbers sharing the editor's own line positions and font.
    pub gutter: Option<super::gutter::Gutter>,
    /// Outer fill, independent of text scaling.
    pub background: Background,
    /// Border metrics in logical pixels.
    pub border: Border,
    /// Default glyph and caret ink.
    pub value: Color,
    /// Placeholder ink.
    pub placeholder_color: Color,
    /// Selection fill.
    pub selection_color: Color,
    /// Empty-editor prompt; owned copies are bounded before publication.
    pub placeholder: Option<Cow<'a, str>>,
    /// Font for placeholder and gutter labels; the editor owns body glyph shaping.
    pub font: Font,
    /// Text size for placeholder and gutter labels, in logical pixels.
    pub size: Pixels,
    /// Line height used for text and caret geometry.
    pub line_height: text::LineHeight,
    /// Placeholder wrapping behavior.
    pub wrapping: text::Wrapping,
    /// Requested metrics hint for placeholder and gutter labels.
    pub hint_factor: Option<f32>,
    /// Whether selection/caret decoration belongs in this view.
    pub focused: bool,
    /// Cursor blink state captured with the scene.
    pub caret_visible: bool,
    /// Logical caret thickness, not scaled by width reflow.
    pub caret_width: f32,
    /// Keep painting a range selection while unfocused (enzICED), for an
    /// owner whose popup or dialog holds focus over the same selection. The
    /// caret never shows unfocused.
    pub retain_selection: bool,
    /// Logical document ranges and fill colors.
    pub highlights: Cow<'a, [(Range<Position>, Color)]>,
}

impl<Font: Copy> Paint<'_, Font> {
    /// Copies at most 16384 ranges and 1 MiB of placeholder text for publication.
    ///
    /// Returns `Budget` for excess payload and `NonFinite` for invalid ink or text metrics.
    pub fn owned(&self) -> Result<Paint<'static, Font>, renderer::presentation::Error> {
        if self.gutter.is_some_and(|gutter| !gutter.valid()) {
            return Err(renderer::presentation::Error::NonFinite);
        }
        if self.highlights.len() > renderer::presentation::MAX_ITEMS
            || self
                .placeholder
                .as_ref()
                .is_some_and(|text| text.len() > 1024 * 1024)
        {
            return Err(renderer::presentation::Error::Budget);
        }
        let colors = std::iter::once(self.value)
            .chain([self.placeholder_color, self.selection_color])
            .chain(self.highlights.iter().map(|(_, color)| *color));
        if colors
            .flat_map(|color| [color.r, color.g, color.b, color.a])
            .any(|value| !value.is_finite())
            || [
                self.size.0,
                self.line_height.to_absolute(self.size).0,
                self.caret_width,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || value <= 0.0)
            || self
                .hint_factor
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(renderer::presentation::Error::NonFinite);
        }
        Ok(Paint {
            placeholder: self
                .placeholder
                .as_ref()
                .map(|text| Cow::Owned(text.to_string())),
            highlights: Cow::Owned(self.highlights.to_vec()),
            ..self.clone_without_borrows()
        })
    }

    fn clone_without_borrows(&self) -> Paint<'static, Font> {
        Paint {
            bounds: self.bounds,
            padding: self.padding,
            gutter: self.gutter,
            background: self.background,
            border: self.border,
            value: self.value,
            placeholder_color: self.placeholder_color,
            selection_color: self.selection_color,
            placeholder: None,
            font: self.font,
            size: self.size,
            line_height: self.line_height,
            wrapping: self.wrapping,
            hint_factor: self.hint_factor,
            focused: self.focused,
            caret_visible: self.caret_visible,
            caret_width: self.caret_width,
            retain_selection: self.retain_selection,
            highlights: Cow::Borrowed(&[]),
        }
    }

    /// Borrows immutable paint inputs at a newly resolved allocation.
    pub fn at_bounds(&self, bounds: Rectangle) -> Paint<'_, Font> {
        Paint {
            bounds,
            placeholder: self
                .placeholder
                .as_ref()
                .map(|text| Cow::Borrowed(text.as_ref())),
            highlights: Cow::Borrowed(self.highlights.as_ref()),
            ..self.clone_without_borrows()
        }
    }

    /// Text viewport after applying the editor's insets.
    pub fn text_bounds(&self) -> Rectangle {
        self.bounds.shrink(self.padding)
    }

    /// Clipped fill quads for highlighted document ranges.
    pub fn background_quads(&self, ranges: &[Vec<Rectangle>]) -> Vec<(renderer::Quad, Color)> {
        let bounds = self.text_bounds();
        let translation = bounds.position() - crate::Point::ORIGIN;
        ranges
            .iter()
            .zip(self.highlights.iter())
            .flat_map(|(rectangles, (_, color))| {
                rectangles.iter().filter_map(move |rect| {
                    bounds.intersection(&(*rect + translation)).map(|bounds| {
                        (
                            renderer::Quad {
                                bounds,
                                ..Default::default()
                            },
                            *color,
                        )
                    })
                })
            })
            .collect()
    }

    /// Clipped underline and strike-through quads, painted after selection fills.
    pub fn text_decoration_quads<'a>(
        &'a self,
        decorations: &'a [super::Decoration],
    ) -> impl Iterator<Item = (renderer::Quad, Color)> + 'a {
        let bounds = self.text_bounds();
        let translation = bounds.position() - crate::Point::ORIGIN;
        decorations.iter().filter_map(move |decoration| {
            bounds
                .intersection(&(decoration.bounds + translation))
                .map(|bounds| {
                    (
                        renderer::Quad {
                            bounds,
                            ..Default::default()
                        },
                        decoration.color.unwrap_or(self.value),
                    )
                })
        })
    }

    /// Caret or selection quads in the same viewport as the glyphs.
    pub fn selection_quads(&self, selection: Selection) -> Vec<(renderer::Quad, Color)> {
        self.selection_quads_with_height(selection, self.line_height.to_absolute(self.size))
    }

    /// Caret or selection quads using the native caret row height in logical pixels.
    pub fn selection_quads_with_height(
        &self,
        selection: Selection,
        height: Pixels,
    ) -> Vec<(renderer::Quad, Color)> {
        if !self.focused && !(self.retain_selection && matches!(selection, Selection::Range(_))) {
            return Vec::new();
        }
        let bounds = self.text_bounds();
        let translation = bounds.position() - crate::Point::ORIGIN;
        match selection {
            Selection::Caret(point) if self.caret_visible => bounds
                .intersection(&Rectangle::new(
                    point + translation,
                    Size::new(self.caret_width, height.0),
                ))
                .map(|bounds| {
                    vec![(
                        renderer::Quad {
                            bounds,
                            ..Default::default()
                        },
                        self.value,
                    )]
                })
                .unwrap_or_default(),
            Selection::Range(ranges) => ranges
                .into_iter()
                .filter_map(|range| bounds.intersection(&(range + translation)))
                .map(|range| {
                    (
                        renderer::Quad {
                            bounds: range.round(),
                            ..Default::default()
                        },
                        self.selection_color,
                    )
                })
                .collect(),
            // Empty paint preserves blinking-caret damage tracking in renderers that need it.
            Selection::Caret(_) => vec![(renderer::Quad::default(), Color::TRANSPARENT)],
        }
    }
}

/// Draws ordinary editor paint without retaining its mutable state.
pub fn draw<R: text::Renderer>(
    renderer: &mut R,
    editor: &R::Editor,
    paint: &Paint<'_, crate::Font>,
) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: paint.bounds,
            border: paint.border,
            ..Default::default()
        },
        paint.background,
    );
    let pairs: Vec<_> = paint
        .highlights
        .iter()
        .map(|(range, _)| (range.start, range.end))
        .collect();
    let ranges = editor.highlight_ranges(&pairs);
    for (quad, color) in paint.background_quads(&ranges) {
        renderer.fill_quad(quad, color);
    }
    let bounds = paint.text_bounds();
    if editor.is_empty() {
        if let Some(placeholder) = &paint.placeholder {
            renderer.fill_text(
                Text {
                    content: placeholder.to_string(),
                    bounds: bounds.size(),
                    size: paint.size,
                    line_height: paint.line_height,
                    font: paint.font,
                    align_x: text::Alignment::Default,
                    align_y: crate::alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: paint.wrapping,
                    ellipsis: text::Ellipsis::None,
                    hint_factor: paint.hint_factor,
                },
                bounds.position(),
                paint.placeholder_color,
                bounds,
            );
        }
    } else {
        renderer.fill_editor(editor, bounds.position(), paint.value, bounds);
    }
    if let Some(gutter) = paint.gutter {
        let viewport = gutter.bounds(paint.bounds, paint.padding);
        if let Ok(rows) = editor.line_number_rows(bounds.height) {
            for row in rows {
                let Some(content) = gutter.number(row) else {
                    continue;
                };
                renderer.fill_text(
                    Text {
                        content,
                        bounds: Size::new(viewport.width, row.height),
                        size: paint.size,
                        line_height: text::LineHeight::Absolute(Pixels(row.height)),
                        font: paint.font,
                        align_x: text::Alignment::Right,
                        align_y: crate::alignment::Vertical::Top,
                        shaping: text::Shaping::Advanced,
                        wrapping: text::Wrapping::None,
                        ellipsis: text::Ellipsis::None,
                        hint_factor: paint.hint_factor,
                    },
                    crate::Point::new(viewport.x + viewport.width, viewport.y + row.top),
                    gutter.color.unwrap_or(paint.value),
                    viewport,
                );
            }
        }
    }
    if paint.focused || paint.retain_selection {
        for (quad, color) in
            paint.selection_quads_with_height(editor.selection(), editor.caret_height())
        {
            renderer.fill_quad(quad, color);
        }
    }
    for (quad, color) in paint.text_decoration_quads(&editor.decorations()) {
        renderer.fill_quad(quad, color);
    }
}
