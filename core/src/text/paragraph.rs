// Modified by enzUI contributors. See FORK.md.
//! Draw paragraphs.
use crate::alignment;
use crate::text::{
    Affinity, Alignment, CursorPosition, Difference, Ellipsis, Hit, LineHeight, Shaping, Span,
    Text, Wrapping,
};
use crate::{Font, Pixels, Point, Rectangle, Size};
use std::ops::Range;

/// A text paragraph.
pub trait Paragraph: Sized + Default {
    /// Creates a new [`Paragraph`] laid out with the given [`Text`].
    fn with_text(text: Text<&str>) -> Self;

    /// Creates a new [`Paragraph`] laid out with the given [`Text`].
    fn with_spans<Link>(text: Text<&[Span<'_, Link>]>) -> Self;

    /// Lays out the [`Paragraph`] with some new boundaries.
    fn resize(&mut self, new_bounds: Size);

    /// Compares the [`Paragraph`] with some desired [`Text`] and returns the
    /// [`Difference`].
    fn compare(&self, text: Text<()>) -> Difference;

    /// Returns the text size of the [`Paragraph`] in [`Pixels`].
    fn size(&self) -> Pixels;

    /// Returns the hint factor of the [`Paragraph`].
    fn hint_factor(&self) -> Option<f32>;

    /// Returns the [`Font`] of the [`Paragraph`].
    fn font(&self) -> Font;

    /// Returns the [`LineHeight`] of the [`Paragraph`].
    fn line_height(&self) -> LineHeight;

    /// Returns the horizontal alignment of the [`Paragraph`].
    fn align_x(&self) -> Alignment;

    /// Returns the vertical alignment of the [`Paragraph`].
    fn align_y(&self) -> alignment::Vertical;

    /// Returns the [`Wrapping`] strategy of the [`Paragraph`]>
    fn wrapping(&self) -> Wrapping;

    /// Returns the [`Ellipsis`] strategy of the [`Paragraph`]>
    fn ellipsis(&self) -> Ellipsis;

    /// Returns the [`Shaping`] strategy of the [`Paragraph`]>
    fn shaping(&self) -> Shaping;

    /// Returns the available bounds used to layout the [`Paragraph`].
    fn bounds(&self) -> Size;

    /// Returns the minimum boundaries that can fit the contents of the
    /// [`Paragraph`].
    fn min_bounds(&self) -> Size;

    /// Tests whether the provided point is within the boundaries of the
    /// [`Paragraph`], returning information about the nearest character.
    fn hit_test(&self, point: Point) -> Option<Hit>;

    /// Hit-tests a paragraph-local point and returns a byte offset in the
    /// paragraph's complete source text.
    ///
    /// Legacy [`Self::hit_test`] offsets may be local to the hard line that was
    /// hit. The default preserves single-line backends; renderers with hard-line
    /// storage override this to add preceding text and line-ending bytes.
    fn hit_test_source_offset(&self, point: Point) -> Option<usize> {
        self.hit_test(point).map(Hit::cursor)
    }

    /// Tests whether the provided point is within the boundaries of a
    /// [`Span`] in the [`Paragraph`], returning the index of the [`Span`]
    /// that was hit.
    fn hit_span(&self, point: Point) -> Option<usize>;

    /// Returns all bounds for the provided [`Span`] index of the [`Paragraph`].
    /// A [`Span`] can have multiple bounds for each line it's on.
    fn span_bounds(&self, index: usize) -> Vec<Rectangle>;

    /// Returns the laid-out glyph bounds covered by a source **byte** range.
    ///
    /// The result contains one rectangle per visual line. Implementations may
    /// return an empty vector when range geometry is unavailable. Read-only
    /// selection uses this instead of estimating from the paragraph box.
    fn range_bounds(&self, _range: Range<usize>) -> Vec<Rectangle> {
        Vec::new()
    }

    /// Returns the bounds of every laid-out grapheme, from one walk of the
    /// layout (enzICED).
    ///
    /// [`range_bounds`](Self::range_bounds) walks the layout per call, so
    /// asking it about each grapheme of a long paragraph is quadratic.
    /// `None` when the implementation cannot answer; callers then fall back
    /// to `range_bounds`.
    fn grapheme_bounds(&self) -> Option<Vec<GraphemeBounds>> {
        None
    }

    /// Returns the distance to the given grapheme index in the [`Paragraph`].
    fn grapheme_position(&self, line: usize, index: usize) -> Option<Point>;

    /// Resolves the [`CursorPosition`] of the logical cursor at the given source
    /// byte `offset` and [`Affinity`].
    ///
    /// Unlike [`grapheme_position`], which infers a location from a visual line
    /// and grapheme index, this asks the shaping engine directly for the caret
    /// that belongs to a *logical* offset. The distinction matters for mixed
    /// bidi: the last logical character of a run is not necessarily its
    /// rightmost glyph, so an x-coordinate heuristic places the caret in the
    /// wrong spot. The returned [`CursorPosition`] carries the resolved point
    /// together with the offset, affinity and bidi level, so a caller can place
    /// a caret and distinguish the two valid positions at a direction boundary
    /// without probing visual lines.
    ///
    /// Returns `None` if the offset does not resolve to a position (for example,
    /// an out-of-range offset).
    ///
    /// The default implementation returns `None`; backends that can compute
    /// cursor geometry override it.
    ///
    /// [`grapheme_position`]: Self::grapheme_position
    fn logical_cursor(&self, _offset: usize, _affinity: Affinity) -> Option<CursorPosition> {
        None
    }

    /// Resolves the [`CursorPosition`] of the *logical end* of the
    /// [`Paragraph`] — the caret after the last logical character — in a single
    /// call, without probing visual lines.
    ///
    /// This is the position a caller wants when placing a trailing caret (for
    /// instance, a streaming-text cursor). In mixed bidi it is the correct
    /// logical end, which may be visually interior rather than the rightmost
    /// glyph.
    ///
    /// The default implementation returns `None`; backends override it.
    fn logical_end_cursor(&self) -> Option<CursorPosition> {
        None
    }

    /// Returns the minimum width that can fit the contents of the [`Paragraph`].
    fn min_width(&self) -> f32 {
        self.min_bounds().width
    }

    /// Returns the minimum height that can fit the contents of the [`Paragraph`].
    fn min_height(&self) -> f32 {
        self.min_bounds().height
    }
}

/// The laid-out bounds of one grapheme of a [`Paragraph`] (enzICED).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphemeBounds {
    /// The grapheme's source **byte** range.
    pub range: Range<usize>,
    /// Its paragraph-local bounds.
    pub bounds: Rectangle,
    /// Whether it is laid out right to left.
    pub rtl: bool,
}

/// A [`Paragraph`] of plain text.
///
/// Clones copy the text state but receive a distinct automatic presentation identity.
#[derive(Debug)]
pub struct Plain<P: Paragraph> {
    raw: P,
    content: String,
    presentation_node: crate::renderer::NodeId,
}

impl<P: Paragraph> Default for Plain<P> {
    fn default() -> Self {
        Self {
            raw: P::default(),
            content: String::new(),
            presentation_node: crate::renderer::NodeId::unique_layout(),
        }
    }
}

impl<P: Paragraph + Clone> Clone for Plain<P> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            content: self.content.clone(),
            presentation_node: crate::renderer::NodeId::unique_layout(),
        }
    }
}

impl<P: Paragraph> Plain<P> {
    /// Stable allocation identity for recording this paragraph; clones receive a new identity.
    pub fn presentation_node(&self) -> crate::renderer::NodeId {
        self.presentation_node
    }

    /// Creates a new [`Plain`] paragraph.
    pub fn new(text: Text<String>) -> Self {
        Self {
            raw: P::with_text(text.as_ref()),
            content: text.content,
            presentation_node: crate::renderer::NodeId::unique_layout(),
        }
    }

    /// Updates the plain [`Paragraph`] to match the given [`Text`], if needed.
    ///
    /// Returns true if the [`Paragraph`] changed.
    pub fn update(&mut self, text: Text<&str>) -> bool {
        if self.content != text.content {
            text.content.clone_into(&mut self.content);
            self.raw = P::with_text(text);
            return true;
        }

        match self.raw.compare(text.with_content(())) {
            Difference::None => false,
            Difference::Bounds => {
                self.raw.resize(text.bounds);
                true
            }
            Difference::Shape => {
                self.raw = P::with_text(text);
                true
            }
        }
    }

    /// Returns the horizontal alignment of the [`Paragraph`].
    pub fn align_x(&self) -> Alignment {
        self.raw.align_x()
    }

    /// Returns the vertical alignment of the [`Paragraph`].
    pub fn align_y(&self) -> alignment::Vertical {
        self.raw.align_y()
    }

    /// Returns the minimum boundaries that can fit the contents of the
    /// [`Paragraph`].
    pub fn min_bounds(&self) -> Size {
        self.raw.min_bounds()
    }

    /// Returns the minimum width that can fit the contents of the
    /// [`Paragraph`].
    pub fn min_width(&self) -> f32 {
        self.raw.min_width()
    }

    /// Returns the minimum height that can fit the contents of the
    /// [`Paragraph`].
    pub fn min_height(&self) -> f32 {
        self.raw.min_height()
    }

    /// Returns the cached [`Paragraph`].
    pub fn raw(&self) -> &P {
        &self.raw
    }

    /// Returns the current content of the plain [`Paragraph`].
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the [`Paragraph`] as a [`Text`] definition.
    pub fn as_text(&self) -> Text<&str> {
        Text {
            content: &self.content,
            bounds: self.raw.bounds(),
            size: self.raw.size(),
            line_height: self.raw.line_height(),
            font: self.raw.font(),
            align_x: self.raw.align_x(),
            align_y: self.raw.align_y(),
            shaping: self.raw.shaping(),
            wrapping: self.raw.wrapping(),
            ellipsis: self.raw.ellipsis(),
            hint_factor: self.raw.hint_factor(),
        }
    }
}
