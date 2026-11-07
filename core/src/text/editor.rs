// Modified by enzUI contributors. See FORK.md.
//! Edit text.
pub mod gutter;
pub mod paint;
pub mod scrollbar;

use crate::clipboard;
use crate::input_method;
use crate::keyboard;
use crate::keyboard::key;
use crate::mouse;
use crate::renderer;
use crate::text::highlighter;
use crate::text::{self, Alignment, LineHeight, Wrapping};
use crate::time::{Duration, Instant};
use crate::touch;
use crate::widget::operation::{Focusable, TextInput};
use crate::window;
use crate::{
    Color, Event, Font, InputMethod, Padding, Pixels, Point, Rectangle, Size, SmolStr, Vector,
};

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

/// A component that can be used by widgets to edit multi-line text.
pub trait Editor: Sized + Default {
    /// Creates a new [`Editor`] laid out with the given text.
    fn with_text(text: &str) -> Self;

    /// Returns true if the [`Editor`] has no contents.
    fn is_empty(&self) -> bool;

    /// Returns the current [`Cursor`] of the [`Editor`].
    fn cursor(&self) -> Cursor;

    /// Returns the current [`Selection`] geometry of the [`Editor`].
    fn selection(&self) -> Selection;

    /// Returns the logical bounds of the current selection without copying its
    /// text or producing paint geometry.
    ///
    /// The returned range is ordered from document start to document end,
    /// including the expanded bounds of word and line selections. This is the
    /// cheap metadata seam for consumers that already own a document snapshot
    /// and need to process the selected slab away from the UI thread.
    fn selection_range(&self) -> Option<std::ops::Range<Position>> {
        None
    }

    /// Returns the current selected text of the [`Editor`].
    fn copy(&self) -> Option<String>;

    /// Returns the text of the given line in the [`Editor`], if it exists.
    fn line(&self, index: usize) -> Option<Line<'_>>;

    /// Returns the amount of lines in the [`Editor`].
    fn line_count(&self) -> usize;

    /// Performs an [`Action`] on the [`Editor`], returning the exact
    /// [`EditDelta`] it produced.
    ///
    /// An action that does not change the text -- a caret motion, a selection,
    /// a click, a drag, a scroll -- returns [`None`], and so does an editing
    /// action that turned out to be a no-op (a `Backspace` at the very start of
    /// the document, an `Undo` with nothing left to undo).
    ///
    /// The delta is the whole reason this returns anything: a caller that keeps
    /// its own copy of the document (a search index, a save snapshot, a model)
    /// learns *exactly* which bytes changed, in `O(changed text)`, instead of
    /// walking the editor with [`Editor::line`] after every keystroke and
    /// copying the whole document to find out.
    ///
    /// How much the caller then pays to record that change is the caller's data
    /// structure, not this one's: replaying into a contiguous [`String`] still
    /// shifts the suffix (see [`EditDelta::apply`]), while a rope or chunked
    /// store can apply the same items in bounded time. What this removes is the
    /// unavoidable full flatten-and-copy, not every cost.
    fn perform(&mut self, action: Action) -> Option<EditDelta>;

    /// Moves the cursor to the given position.
    fn move_to(&mut self, cursor: Cursor);

    /// Returns the current boundaries of the [`Editor`].
    fn bounds(&self) -> Size;

    /// Returns the current scroll geometry of the [`Editor`].
    ///
    /// The offset is absolute in laid-out content coordinates. It is not a
    /// logical-line estimate: wrapped rows and the renderer's actual line
    /// heights are included.
    fn scroll_metrics(&mut self) -> ScrollMetrics;

    /// Already-resolved metrics, without changing the backing allocation used by submitted text.
    fn cached_scroll_metrics(&self) -> Option<ScrollMetrics> {
        None
    }

    /// Visible logical-line heads from the same shaped rows used for text painting.
    /// Unsupported backends refuse instead of estimating wrapping from another paragraph.
    fn line_number_rows(
        &self,
        _height: f32,
    ) -> Result<Vec<gutter::Row>, crate::renderer::presentation::Error> {
        Err(crate::renderer::presentation::Error::UnsupportedBackend)
    }

    /// Identity of the live editor model, without retaining its content or mutable state.
    /// Unsupported backends return `None` and cannot bind external editor chrome.
    fn presentation_identity(&self) -> Option<Arc<()>> {
        None
    }

    /// Returns the minimum boundaries to fit the current contents of
    /// the [`Editor`].
    fn min_bounds(&self) -> Size;

    /// Returns the hint factor of the [`Editor`].
    fn hint_factor(&self) -> Option<f32>;

    /// Updates the [`Editor`] with some new attributes.
    fn update(
        &mut self,
        new_bounds: Size,
        new_font: Font,
        new_size: Pixels,
        new_line_height: LineHeight,
        new_wrapping: Wrapping,
        new_alignment: Alignment,
        new_hint_factor: Option<f32>,
        new_parser: &mut impl text::Parser,
    );

    /// Materializes attributes for responsive capture up to `maximum_height` logical pixels.
    ///
    /// The default returns `UnsupportedBackend`. Implementations may also refuse unavailable
    /// parser context, invalid viewports or excess payload; no callbacks run during owned replay.
    fn prepare_presentation<P: text::Parser>(
        &mut self,
        _font: Font,
        _maximum_height: f32,
        _parser: &mut P,
        _highlight: impl Fn(P::Output) -> highlighter::Style,
    ) -> Result<(), crate::renderer::presentation::Error> {
        Err(crate::renderer::presentation::Error::UnsupportedBackend)
    }

    /// Overwrites the current contents of the [`Editor`].
    fn overwrite(&mut self, new_text: &str);

    /// Runs a [`text::Highlighter`] in the [`Editor`].
    fn highlight<P: text::Parser>(
        &mut self,
        font: Font,
        parser: &mut P,
        highlight: impl Fn(P::Output) -> highlighter::Style,
    );

    /// Returns the laid-out [`Rectangle`]s covering each of the given
    /// `(start, end)` position ranges.
    ///
    /// The returned `Vec` is parallel to `ranges`: entry *i* holds the
    /// rectangles for `ranges[i]`. An out-of-bounds or empty range
    /// produces an empty inner `Vec`.
    ///
    /// Used by consumers that need to paint background runs (search-match
    /// highlights, lint markers, diff ranges) under the editor text.
    fn highlight_ranges(&self, _ranges: &[(Position, Position)]) -> Vec<Vec<Rectangle>> {
        Vec::new()
    }

    /// Returns the underline and strike-through lines the current attributes
    /// laid out, for painting.
    ///
    /// The backend shapes them from the run they cover and the font's own
    /// metrics; the caller paints them. Decoration geometry never feeds back
    /// into layout, so a decorated range keeps the caret, hit testing and
    /// selection geometry of the same undecorated text.
    ///
    /// Bounds are logical pixels in the same editor-local space as
    /// [`Editor::highlight_ranges`]: relative to the top-left of the editor's
    /// text, with scrolling applied and hinting divided back out. They are
    /// **not** clipped to the visible bounds, so a caller clips them the way it
    /// clips the text itself; only laid-out, visible lines contribute.
    ///
    /// Lines come in visual order, each decorated run's underline before its
    /// strike-through. A run whose glyphs are visually discontiguous, as a
    /// bidirectional run can be, reports one line spanning its whole extent.
    /// An empty or zero-width span reports nothing rather than an empty
    /// rectangle. Defaults to none, and never panics.
    fn decorations(&self) -> Vec<Decoration> {
        Vec::new()
    }

    /// Sets the [`Alignment`] of each logical line, by index.
    ///
    /// The editor asks `alignment` for every line it holds, so one call states
    /// the whole document: a caller that tracks alignment per paragraph hands
    /// the same closure every pass and does not have to know which lines
    /// changed. [`Alignment::Default`] leaves a line laid out from its reading
    /// edge, which is what an editor with no alignment at all does.
    ///
    /// Alignment moves the glyphs, so it moves the caret, the selection
    /// rectangles and the hit test with them: an aligned line reports the same
    /// geometry for the same character as it would have unaligned, shifted by
    /// its own correction. Nothing else about the layout changes — line
    /// breaking is decided by the bounds, not by the alignment.
    ///
    /// Call it on the same pass that installs the bounds, before
    /// [`Editor::update`], so a line reshapes once with both. Defaults to
    /// leaving every line unaligned, and never panics.
    fn set_line_alignment(&mut self, _alignment: &dyn Fn(usize) -> Alignment) {}

    /// Releases any alignment previously installed by
    /// [`set_line_alignment`](Self::set_line_alignment), returning every line
    /// to [`Alignment::Default`].
    ///
    /// Costs nothing when no alignment is installed, so a caller that never
    /// aligns can say so on every layout.
    fn clear_line_alignment(&mut self) {}

    /// Sets the rectangles the text flows around, replacing the previous
    /// ones (see [`Exclusion`]).
    ///
    /// Each wrapped row is laid out in the widest gap they leave at its
    /// height, and a row they leave no usable gap for moves below them. The
    /// text, the cursor and the history are untouched; only the layout
    /// changes. Call it on every layout, before [`Editor::update`], with an
    /// empty slice when nothing is excluded. Defaults to ignoring them.
    fn set_exclusions(&mut self, _exclusions: &[Exclusion]) {}

    /// Where each exclusion sat at the last [`Editor::update`], in the order
    /// they were set, in the coordinates of [`ScrollMetrics::offset`]: an
    /// anchored one at its line's top. `None` for one anchored past the last
    /// line. Empty when the editor ignores exclusions.
    fn exclusion_bounds(&self) -> Vec<Option<crate::Rectangle>> {
        Vec::new()
    }

    /// Returns an iterator of the text of the lines in the [`Editor`].
    fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        (0..)
            .map(|i| self.line(i))
            .take_while(Option::is_some)
            .flatten()
    }

    /// Returns the text of the [`Editor`].
    fn text(&self) -> String {
        let mut contents = String::new();
        let mut lines = self.lines().peekable();

        while let Some(line) = lines.next() {
            contents.push_str(&line.text);

            if lines.peek().is_some() {
                contents.push_str(if line.ending == LineEnding::None {
                    LineEnding::default().as_str()
                } else {
                    line.ending.as_str()
                });
            }
        }

        contents
    }

    /// Returns the current [`Font`] of the [`Editor`].
    fn font(&self) -> Font;

    /// Returns the current text size of the [`Editor`].
    fn text_size(&self) -> Pixels;

    /// Returns the current [`LineHeight`] of the [`Editor`].
    fn line_height(&self) -> LineHeight;

    /// Visible caret row height in logical pixels, including per-run metrics.
    /// Backends without per-run metrics use the base line height.
    fn caret_height(&self) -> Pixels {
        self.line_height().to_absolute(self.text_size())
    }
}

/// A decoration line laid out by an [`Editor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    /// The bounds of the line, in the space
    /// [`Editor::decorations`] documents.
    pub bounds: Rectangle,

    /// The [`Color`] the decorated run named.
    ///
    /// [`None`] means the line takes the ink of the text it decorates.
    pub color: Option<Color>,
}

/// An interaction with an [`Editor`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Apply a [`Motion`].
    Move(Motion),
    /// Select text with a given [`Motion`].
    Select(Motion),
    /// Select the word at the current cursor.
    SelectWord,
    /// Select the line at the current cursor.
    SelectLine,
    /// Select the entire buffer.
    SelectAll,
    /// Perform an [`Edit`].
    Edit(Edit),
    /// Click the [`Editor`] at the given [`Point`].
    Click(Point, mouse::click::Kind),
    /// Drag the mouse on the [`Editor`] to the given [`Point`].
    Drag(Point),
    /// Scroll the [`Editor`] a certain amount of lines.
    Scroll {
        /// The amount of lines to scroll.
        lines: i32,
    },
    /// Place a logical line at the viewport's leading edge without moving the selection.
    ///
    /// The zero-based line belongs to the current editor model; chunked hosts
    /// subtract their materialized slice's origin from a global target. The offset
    /// is in logical pixels: horizontal content offset and vertical displacement
    /// within the target paragraph, including partial wrapped rows. Non-finite or
    /// negative offsets become zero; the line and displacement are clamped to the
    /// model and paragraph. The document end may prevent leading-edge alignment.
    /// A model with no lines ignores this action.
    ScrollToLine {
        /// Zero-based line in the current editor model.
        line: usize,
        /// Horizontal content offset and vertical displacement within that line.
        offset: Vector,
    },
    /// Scroll the [`Editor`] to an absolute offset in laid-out content
    /// coordinates.
    ScrollTo {
        /// The absolute horizontal and vertical offset.
        offset: Vector,
    },
}

/// A rectangle an [`Editor`]'s text flows around, like a floated image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exclusion {
    /// In logical pixels from the text's origin, before scrolling (the
    /// coordinates of [`ScrollMetrics::offset`]); `y` is measured from the
    /// top of [`Self::line`] when it is set.
    pub bounds: crate::Rectangle,
    /// The line the rectangle belongs to: it moves with that line as the text
    /// above changes, and the lines before it never flow around it. The index
    /// is the caller's to keep as lines are inserted and removed above it.
    pub line: Option<usize>,
    /// Whether `bounds.x` is measured from the text's trailing (right) edge to
    /// the rectangle's right edge instead of from its leading edge, so a
    /// rectangle against the right edge stays there as the editor resizes.
    pub trailing: bool,
}

impl Exclusion {
    /// An exclusion at fixed content coordinates.
    pub const fn new(bounds: crate::Rectangle) -> Self {
        Self {
            bounds,
            line: None,
            trailing: false,
        }
    }

    /// An exclusion anchored to `line`, `bounds.y` measured from its top.
    pub const fn anchored(line: usize, bounds: crate::Rectangle) -> Self {
        Self {
            bounds,
            line: Some(line),
            trailing: false,
        }
    }

    /// Measures `bounds.x` from the trailing edge; see [`Self::trailing`].
    pub const fn from_trailing_edge(mut self) -> Self {
        self.trailing = true;
        self
    }
}

/// Read-only scroll geometry of an [`Editor`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollMetrics {
    /// The laid-out viewport size.
    pub viewport: Size,
    /// The full laid-out content size.
    pub content: Size,
    /// The absolute horizontal and vertical content offset.
    pub offset: Vector,
    /// The first logical line the viewport can show.
    ///
    /// A consumer that paints its own column beside the editor -- a line
    /// number gutter, a diff ribbon, a breakpoint margin -- needs to know
    /// *which* lines are on screen and where they start, or it has to
    /// materialise a row for every line in the document on every frame. With
    /// wrapping enabled it cannot derive either number from `offset` alone,
    /// because a logical line can occupy several visual rows (enzUI-ICED
    /// #1804).
    pub first_line: usize,
    /// The content-space `y` of [`first_line`](Self::first_line)'s top edge.
    ///
    /// This is at or just above the vertical component of
    /// [`offset`](Self::offset): the viewport usually starts part-way through
    /// the first visible line.
    pub first_line_top: f32,
}

impl Action {
    /// Returns whether the [`Action`] is an editing action.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit(_))
    }
}

/// An action that edits text.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Insert the given character.
    Insert(char),
    /// Paste the given text.
    Paste(Arc<String>),
    /// Break the current line.
    Enter,
    /// Indent the current line.
    Indent,
    /// Unindent the current line.
    Unindent,
    /// Delete the previous character.
    Backspace,
    /// Delete the word before the cursor.
    BackspaceWord,
    /// Delete the line before the cursor.
    BackspaceLine,
    /// Delete the next character.
    Delete,
    /// Delete the word after the cursor.
    DeleteWord,
    /// Delete the line after the cursor.
    DeleteLine,
    /// Undo the last change performed on the [`Editor`].
    Undo,
    /// Redo the last undone change on the [`Editor`].
    Redo,
    /// Replace multiple disjoint ranges as one undoable transaction.
    ///
    /// The ranges must be valid UTF-8 byte positions in the current document,
    /// sorted from document start to end, and non-overlapping. An invalid batch
    /// is rejected without changing the document. The editor applies a valid
    /// batch from end to start, so every range continues to address the
    /// pre-edit document, and records the whole batch as one history entry.
    Replace(Arc<ReplaceBatch>),
}

/// A set of range replacements performed as one editor history transaction.
///
/// [`replacements`](Self::replacements) address the document before the edit.
/// [`cursor_after`](Self::cursor_after) is installed after the replacements and
/// restored by redo; undo restores the cursor that preceded the transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceBatch {
    /// Ordered, disjoint replacements in ascending document order.
    pub replacements: Vec<Replacement>,
    /// Cursor and optional selection to install after the transaction.
    pub cursor_after: Cursor,
}

/// One byte-positioned range replacement in a [`ReplaceBatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// Range in the document before the batch is applied.
    pub range: Range<Position>,
    /// Replacement text. Empty text deletes the range.
    pub text: Arc<String>,
}

impl Replacement {
    /// Creates a range replacement.
    pub fn new(range: Range<Position>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: Arc::new(text.into()),
        }
    }
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move to the left boundary of a word.
    WordLeft,
    /// Move to the right boundary of a word.
    WordRight,
    /// Move to the start of the line.
    Home,
    /// Move to the end of the line.
    End,
    /// Move to the start of the previous window.
    PageUp,
    /// Move to the start of the next window.
    PageDown,
    /// Move to the start of the text.
    DocumentStart,
    /// Move to the end of the text.
    DocumentEnd,
}

impl Motion {
    /// Widens the [`Motion`], if possible.
    pub fn widen(self) -> Self {
        match self {
            Self::Left => Self::WordLeft,
            Self::Right => Self::WordRight,
            Self::Home => Self::DocumentStart,
            Self::End => Self::DocumentEnd,
            _ => self,
        }
    }

    /// Returns the [`Direction`] of the [`Motion`].
    pub fn direction(&self) -> Direction {
        match self {
            Self::Left
            | Self::Up
            | Self::WordLeft
            | Self::Home
            | Self::PageUp
            | Self::DocumentStart => Direction::Left,
            Self::Right
            | Self::Down
            | Self::WordRight
            | Self::End
            | Self::PageDown
            | Self::DocumentEnd => Direction::Right,
        }
    }
}

/// A direction in some text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// <-
    Left,
    /// ->
    Right,
}

/// The cursor of an [`Editor`].
#[derive(Debug, Clone)]
pub enum Selection {
    /// Cursor without a selection
    Caret(Point),

    /// Cursor selecting a range of text
    Range(Vec<Rectangle>),
}

/// The range of an [`Editor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    /// The cursor position.
    pub position: Position,

    /// The selection position, if any.
    pub selection: Option<Position>,
}

/// The exact text change one performed [`Action`] made to an [`Editor`].
///
/// A delta is an ordered, non-empty list of [`EditItem`]s. Order matters: each
/// item's [`Position`]s describe the document *as left by the previous item*,
/// which is what makes [`EditDelta::apply`] a faithful replay. A single action
/// can produce several items -- replacing a selection is a delete followed by
/// an insert, indenting a selection is one insert per line.
///
/// # Example
///
/// Mirroring an editor into a plain [`String`], without ever flattening the
/// editor itself (see [`EditDelta::apply`] for what this does and does not
/// cost):
///
/// ```ignore
/// let mut mirror = String::new();
///
/// if let Some(delta) = editor.perform(action) {
///     assert!(delta.apply(&mut mirror));
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDelta {
    items: Vec<EditItem>,
}

/// One insertion or deletion of an [`EditDelta`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditItem {
    /// The position the change starts at.
    pub start: Position,
    /// The position the change ends at.
    ///
    /// For an [`EditKind::Insert`] this is where the cursor landed after the
    /// text was inserted; for an [`EditKind::Delete`] it is the end of the
    /// range that was removed.
    pub end: Position,
    /// The exact text that was inserted or deleted, line endings included.
    pub text: String,
    /// Whether the [`text`](Self::text) was inserted or deleted.
    pub kind: EditKind,
}

/// The direction of an [`EditItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditKind {
    /// The text was inserted at [`EditItem::start`].
    Insert,
    /// The text was removed from `start..end`.
    Delete,
}

impl EditItem {
    /// Returns the amount of bytes of text this [`EditItem`] carries.
    pub fn bytes(&self) -> usize {
        self.text.len()
    }
}

impl EditDelta {
    /// Creates an [`EditDelta`] from the given ordered items, dropping every
    /// one whose text is empty.
    ///
    /// Returns [`None`] when nothing is left: an action that ends up changing
    /// no bytes has no delta, rather than an empty one, so a caller can treat
    /// [`Some`] as "the document moved". This keeps the type's invariant
    /// non-empty (there is no `Default`), so `Some(delta)` always carries at
    /// least one item carrying at least one byte.
    ///
    /// A change is bytes: an [`EditItem`] with empty text moves no bytes
    /// regardless of its endpoints, so it is dropped whether `start == end` or
    /// not. An item that spans a range but reports no text is a producer bug
    /// -- a deletion always carries the bytes it removed -- and would in any
    /// case be unreplayable, so it is not a change the caller can be told
    /// about.
    pub fn new(items: impl IntoIterator<Item = EditItem>) -> Option<Self> {
        let items: Vec<_> = items
            .into_iter()
            .filter(|item| !item.text.is_empty())
            .collect();

        (!items.is_empty()).then_some(Self { items })
    }

    /// Returns the ordered items of the [`EditDelta`].
    pub fn items(&self) -> &[EditItem] {
        &self.items
    }

    /// Returns the total amount of bytes of text the [`EditDelta`] carries.
    ///
    /// This is the size of one edit, not of the document: a one-character
    /// insertion in a 40 MiB buffer carries the character.
    pub fn bytes(&self) -> usize {
        self.items.iter().map(EditItem::bytes).sum()
    }

    /// Replays the [`EditDelta`] on a [`String`] holding the text the editor
    /// had *before* the action.
    ///
    /// # Cost
    ///
    /// This is a *convenience replay* onto a contiguous [`String`], not a
    /// bounded-cost document store. The delta itself is `O(changed text)` --
    /// that is what [`Editor::perform`] now exposes instead of forcing a full
    /// flatten -- but mutating a [`String`] in the middle shifts its whole
    /// suffix, and addressing a line walks the text up to it, so one call here
    /// is still `O(document)` for an edit that is not at the very end.
    ///
    /// In other words: what this removes is the *extra* flatten-and-copy of the
    /// editor's own text after every keystroke. A caller that needs bounded
    /// per-edit work must apply the same items to a rope or chunked store of
    /// its own; the delta carries everything needed to do so.
    ///
    /// Returns `false` -- leaving the items applied so far in place -- when an
    /// item does not address the given text, or when the text a deletion claims
    /// to remove is not the text found there. Either means the caller's copy
    /// has drifted from the editor.
    ///
    /// # Line endings
    ///
    /// An [`EditItem`] addresses a *line* and a byte column inside it, so
    /// `document` is split into lines the same way an editor splits text it is
    /// given: over `\r\n`, `\n\r`, `\n` and `\r`, longest first.
    ///
    /// That makes one class of document ambiguous: a `\n` immediately followed
    /// by a `\r` is a single `\n\r` ending here, while an editor that produced
    /// it by *editing* (an LF-terminated line followed by a CRLF-terminated
    /// one) still holds two lines. Mirror LF-normalized text, or text with one
    /// consistent line ending, and the question never arises.
    #[must_use]
    pub fn apply(&self, document: &mut String) -> bool {
        for item in &self.items {
            let Some(start) = byte_offset(document, item.start) else {
                return false;
            };

            match item.kind {
                EditKind::Insert => document.insert_str(start, &item.text),
                EditKind::Delete => {
                    let Some(end) = byte_offset(document, item.end) else {
                        return false;
                    };

                    if end < start || document.get(start..end) != Some(item.text.as_str()) {
                        return false;
                    }

                    document.replace_range(start..end, "");
                }
            }
        }

        true
    }
}

/// Returns the byte offset of `position` in `text`, if it addresses it.
///
/// A [`Position`] is a line plus a byte column *within that line*, so the walk
/// has to split lines exactly the way an [`Editor`] does -- including the four
/// line endings [`LineEnding`] knows about, which is why `\r\n` cannot simply
/// be treated as two lines.
fn byte_offset(text: &str, position: Position) -> Option<usize> {
    let mut line_start = 0;
    let mut line = 0;

    while line < position.line {
        let rest = &text[line_start..];
        let break_at = rest.find(['\r', '\n'])?;
        let ending = line_ending_at(&rest[break_at..]);

        line_start += break_at + ending.as_str().len();
        line += 1;
    }

    let rest = &text[line_start..];
    let line_len = rest.find(['\r', '\n']).unwrap_or(rest.len());

    (position.index <= line_len && rest.is_char_boundary(position.index))
        .then_some(line_start + position.index)
}

/// The [`LineEnding`] starting at the beginning of `text`.
fn line_ending_at(text: &str) -> LineEnding {
    if text.starts_with("\r\n") {
        LineEnding::CrLf
    } else if text.starts_with("\n\r") {
        LineEnding::LfCr
    } else if text.starts_with('\n') {
        LineEnding::Lf
    } else if text.starts_with('\r') {
        LineEnding::Cr
    } else {
        LineEnding::None
    }
}

/// A cursor position in an [`Editor`]: upstream's [`text::Position`], whose
/// byte offset within the line is `index` (enzICED called it `column` before
/// the #3404 sync).
pub use crate::text::Position;

/// A line of an [`Editor`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Line<'a> {
    /// The raw text of the [`Line`].
    pub text: Cow<'a, str>,
    /// The line ending of the [`Line`].
    pub ending: LineEnding,
}

/// The line ending of a [`Line`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineEnding {
    /// Use `\n` for line ending (POSIX-style)
    #[default]
    Lf,
    /// Use `\r\n` for line ending (Windows-style)
    CrLf,
    /// Use `\r` for line ending (many legacy systems)
    Cr,
    /// Use `\n\r` for line ending (some legacy systems)
    LfCr,
    /// No line ending
    None,
}

impl LineEnding {
    /// Gets the string representation of the [`LineEnding`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
            Self::LfCr => "\n\r",
            Self::None => "",
        }
    }
}

/// The internal state of an [`Editor`].
#[derive(Debug, Clone, Default)]
pub struct State {
    focus: Option<Focus>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    is_dragging: bool,
    partial_scroll: f32,
}

impl State {
    /// Creates a new [`State`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the [`State`] for the given [`Editor`] and returns any relevant [`Update`].
    pub fn update<Message>(
        &mut self,
        editor: &impl Editor,
        event: &Event,
        bounds: Rectangle,
        padding: Padding,
        cursor: mouse::Cursor,
        key_binding: impl Fn(KeyPress) -> Option<Binding<Message>>,
    ) -> Option<Update<Message>> {
        match event {
            Event::Window(window::Event::Unfocused) => {
                if let Some(focus) = &mut self.focus {
                    focus.is_window_focused = false;
                }

                None
            }
            Event::Window(window::Event::Focused) => {
                if let Some(focus) = &mut self.focus {
                    focus.is_window_focused = true;
                    focus.updated_at = Instant::now();
                }

                Some(Update::Focus)
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let focus = self.focus.as_mut()?;

                if !focus.is_window_focused {
                    return None;
                }

                focus.now = *now;

                let millis_until_redraw = Focus::CURSOR_BLINK_INTERVAL_MILLIS
                    - (focus.now - focus.updated_at).as_millis()
                        % Focus::CURSOR_BLINK_INTERVAL_MILLIS;

                Some(Update::RedrawAt(
                    focus.now + Duration::from_millis(millis_until_redraw as u64),
                ))
            }
            Event::Clipboard(clipboard::Event::Read(Ok(content))) => {
                let focus = self.focus.as_ref()?;

                if !focus.is_window_focused {
                    return None;
                }

                let clipboard::Content::Text(text) = content.as_ref() else {
                    return None;
                };

                Some(Update::Action(Action::Edit(Edit::Paste(Arc::new(
                    text.clone(),
                )))))
            }
            Event::Mouse(event) => match event {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    if let Some(cursor_position) = cursor.position_in(bounds) {
                        let cursor_position =
                            cursor_position - Vector::new(padding.left, padding.top);

                        let click = mouse::Click::new(
                            cursor_position,
                            mouse::Button::Left,
                            self.last_click,
                        );

                        self.focus = Some(Focus::now());
                        self.last_click = Some(click);
                        self.is_dragging = true;

                        Some(Update::Action(Action::Click(
                            click.position(),
                            click.kind(),
                        )))
                    } else if self.focus.is_some() {
                        self.focus = None;

                        Some(Update::Unfocus)
                    } else {
                        None
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => {
                    self.is_dragging = false;

                    Some(Update::Release)
                }
                mouse::Event::CursorMoved { .. } if self.is_dragging => {
                    let position = cursor.position_from(bounds.position())?
                        - Vector::new(padding.left, padding.top);

                    Some(Update::Action(Action::Drag(position)))
                }
                mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                    let bounds = editor.bounds();

                    if bounds.height >= i32::MAX as f32 {
                        return None;
                    }

                    let lines = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => {
                            if y.abs() > 0.0 {
                                y.signum() * -(y.abs() * 4.0).max(1.0)
                            } else {
                                0.0
                            }
                        }
                        mouse::ScrollDelta::Pixels { y, .. } => -y / 4.0,
                    };

                    let lines = lines + self.partial_scroll;
                    self.partial_scroll = lines.fract();

                    Some(Update::Action(Action::Scroll {
                        lines: lines as i32,
                    }))
                }
                _ => None,
            },
            Event::Touch(event) => match event {
                touch::Event::FingerPressed { .. } => {
                    if let Some(cursor_position) = cursor.position_in(bounds) {
                        let cursor_position =
                            cursor_position - Vector::new(padding.left, padding.top);

                        let click = mouse::Click::new(
                            cursor_position,
                            mouse::Button::Left,
                            self.last_click,
                        );

                        self.focus = Some(Focus::now());
                        self.last_click = Some(click);
                        self.is_dragging = true;

                        Some(Update::Action(Action::Click(
                            click.position(),
                            click.kind(),
                        )))
                    } else if self.focus.is_some() {
                        self.focus = None;

                        Some(Update::Unfocus)
                    } else {
                        None
                    }
                }
                touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. } => {
                    self.is_dragging = false;

                    Some(Update::Release)
                }
                touch::Event::FingerMoved { .. } if self.is_dragging => {
                    let position =
                        cursor.position_in(bounds)? - Vector::new(padding.left, padding.top);

                    Some(Update::Action(Action::Drag(position)))
                }
                touch::Event::FingerMoved { .. } => None,
            },
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => {
                    let is_open = matches!(event, input_method::Event::Opened);
                    self.preedit = is_open.then(input_method::Preedit::new);

                    Some(Update::InputMethod)
                }
                input_method::Event::Preedit(content, selection) if self.focus.is_some() => {
                    self.preedit = Some(input_method::Preedit {
                        content: content.clone(),
                        selection: selection.clone(),
                        text_size: Some(editor.text_size()),
                    });

                    Some(Update::InputMethod)
                }
                input_method::Event::Commit(content) if self.focus.is_some() => Some(
                    Update::Action(Action::Edit(Edit::Paste(Arc::new(content.clone())))),
                ),
                _ => None,
            },
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                modifiers,
                text,
                ..
            }) => {
                let key_press = KeyPress {
                    key: key.clone(),
                    modified_key: modified_key.clone(),
                    physical_key: *physical_key,
                    modifiers: *modifiers,
                    text: text.clone(),
                    is_focused: self.is_focused(),
                };

                fn apply_binding<Message>(
                    binding: Binding<Message>,
                    editor: &impl Editor,
                    state: &mut State,
                ) -> Option<Update<Message>> {
                    let action = |action| Update::Action(action);
                    let edit = |edit| action(Action::Edit(edit));

                    match binding {
                        Binding::Unfocus => {
                            state.focus = None;
                            state.is_dragging = false;

                            None
                        }
                        Binding::Copy => {
                            let selection = editor.copy()?;

                            Some(Update::Copy(selection))
                        }
                        Binding::Cut => {
                            let selection = editor.copy()?;

                            Some(Update::Sequence(vec![
                                Update::Copy(selection),
                                edit(Edit::Backspace),
                            ]))
                        }
                        Binding::Paste => Some(Update::Paste),
                        Binding::Undo => Some(edit(Edit::Undo)),
                        Binding::Redo => Some(edit(Edit::Redo)),
                        Binding::Move(motion) => Some(action(Action::Move(motion))),
                        Binding::Select(motion) => Some(action(Action::Select(motion))),
                        Binding::SelectWord => Some(action(Action::SelectWord)),
                        Binding::SelectLine => Some(action(Action::SelectLine)),
                        Binding::SelectAll => Some(action(Action::SelectAll)),
                        Binding::Insert(c) => Some(edit(Edit::Insert(c))),
                        Binding::Enter => Some(edit(Edit::Enter)),
                        Binding::Backspace => Some(edit(Edit::Backspace)),
                        Binding::BackspaceWord => Some(edit(Edit::BackspaceWord)),
                        Binding::BackspaceLine => Some(edit(Edit::BackspaceLine)),
                        Binding::Delete => Some(action(Action::Edit(Edit::Delete))),
                        Binding::DeleteWord => Some(edit(Edit::DeleteWord)),
                        Binding::DeleteLine => Some(edit(Edit::DeleteLine)),
                        Binding::Sequence(sequence) => {
                            let updates: Vec<_> = sequence
                                .into_iter()
                                .flat_map(|binding| apply_binding(binding, editor, state))
                                .collect();

                            if updates.is_empty() {
                                return None;
                            }

                            Some(Update::Sequence(updates))
                        }
                        Binding::Custom(message) => Some(Update::Custom(message)),
                    }
                }

                let update = apply_binding(key_binding(key_press)?, editor, self);

                if let Some(focus) = &mut self.focus {
                    focus.updated_at = Instant::now();
                }

                update
            }
            _ => None,
        }
    }

    /// Returns the current [`InputMethod`] of the [`State`] for the given [`Editor`].
    pub fn input_method<'a>(
        &'a self,
        editor: &impl Editor,
        position: Point,
    ) -> InputMethod<&'a str> {
        let Some(Focus {
            is_window_focused: true,
            ..
        }) = &self.focus
        else {
            return InputMethod::Disabled;
        };

        let translation = position - Point::ORIGIN;

        let cursor = match editor.selection() {
            Selection::Caret(position) => position,
            Selection::Range(ranges) => ranges.first().cloned().unwrap_or_default().position(),
        };

        let line_height = editor.caret_height();

        let position = cursor + translation;

        InputMethod::Enabled {
            cursor: Rectangle::new(position, Size::new(1.0, f32::from(line_height))),
            purpose: input_method::Purpose::Normal,
            preedit: self.preedit.as_ref().map(input_method::Preedit::as_ref),
        }
    }

    /// Draws the given [`Editor`] with the current [`State`].
    pub fn draw<Renderer: text::Renderer>(
        &self,
        editor: &Renderer::Editor,
        renderer: &mut Renderer,
        position: Point,
        clip_bounds: Rectangle,
        style: Style,
    ) {
        let bounds = Rectangle::new(position, editor.bounds());

        let Some(clip_bounds) = clip_bounds.intersection(&bounds) else {
            return;
        };

        if !editor.is_empty() {
            renderer.fill_editor(editor, position, style.value, clip_bounds);
        }

        if !self.is_focused() {
            return;
        }

        let translation = position - Point::ORIGIN;
        let caret_height = editor.caret_height();

        match editor.selection() {
            Selection::Caret(position) if self.is_cursor_visible() => {
                let cursor = Rectangle::new(
                    position + translation,
                    Size::new(
                        if renderer::CRISP {
                            (1.0 / renderer.hint_factor().unwrap_or(1.0)).max(1.0)
                        } else {
                            1.0
                        },
                        caret_height.into(),
                    ),
                );

                if let Some(clipped_cursor) = clip_bounds.intersection(&cursor) {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: clipped_cursor,
                            ..renderer::Quad::default()
                        },
                        style.value,
                    );
                }
            }
            Selection::Range(ranges) => {
                for range in ranges
                    .into_iter()
                    .filter_map(|range| clip_bounds.intersection(&(range + translation)))
                {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: range.round(),
                            ..renderer::Quad::default()
                        },
                        style.selection,
                    );
                }
            }
            Selection::Caret(_) => {
                // Drawing an empty quad helps some renderers to track the damage of the blinking cursor
                renderer.fill_quad(renderer::Quad::default(), Color::TRANSPARENT);
            }
        }
    }

    /// Returns whether the cursor of the [`Editor`] is visible.
    pub fn is_cursor_visible(&self) -> bool {
        self.focus.as_ref().is_some_and(Focus::is_cursor_visible)
    }
}

/// The visual style of an [`Editor`].
pub struct Style {
    /// The [`Color`] of the contents.
    pub value: Color,

    /// The background [`Color`] of any selection.
    pub selection: Color,
}

#[derive(Debug, Clone)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

impl Focus {
    const CURSOR_BLINK_INTERVAL_MILLIS: u128 = 500;

    fn now() -> Self {
        let now = Instant::now();

        Self {
            updated_at: now,
            now,
            is_window_focused: true,
        }
    }

    fn is_cursor_visible(&self) -> bool {
        self.is_window_focused
            && ((self.now - self.updated_at).as_millis() / Self::CURSOR_BLINK_INTERVAL_MILLIS)
                .is_multiple_of(2)
    }
}

impl State {
    /// Returns whether the [`Editor`] is currently focused or not.
    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }
}

impl Focusable for State {
    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    fn focus(&mut self) {
        self.focus = Some(Focus::now());
    }

    fn unfocus(&mut self) {
        self.focus = None;
    }
}

/// A binding to an action in the [`Editor`].
#[derive(Debug, Clone, PartialEq)]
pub enum Binding<Message> {
    /// Unfocus the [`Editor`].
    Unfocus,
    /// Copy the selection of the [`Editor`].
    Copy,
    /// Cut the selection of the [`Editor`].
    Cut,
    /// Paste the clipboard contents in the [`Editor`].
    Paste,
    /// Undo the last change peformed in the [`Editor`].
    Undo,
    /// Redo the last change undone in the [`Editor`].
    Redo,
    /// Apply a [`Motion`].
    Move(Motion),
    /// Select text with a given [`Motion`].
    Select(Motion),
    /// Select the word at the current cursor.
    SelectWord,
    /// Select the line at the current cursor.
    SelectLine,
    /// Select the entire buffer.
    SelectAll,
    /// Insert the given character.
    Insert(char),
    /// Break the current line.
    Enter,
    /// Delete the previous character.
    Backspace,
    /// Delete the word before the cursor.
    BackspaceWord,
    /// Delete the line before the cursor.
    BackspaceLine,
    /// Delete the next character.
    Delete,
    /// Delete the word after the cursor.
    DeleteWord,
    /// Delete the line after the cursor.
    DeleteLine,
    /// A sequence of bindings to execute.
    Sequence(Vec<Self>),
    /// Produce the given message.
    Custom(Message),
}

/// A key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    /// The original key pressed without modifiers applied to it.
    ///
    /// You should use this key for combinations (e.g. Ctrl+C).
    pub key: keyboard::Key,
    /// The key pressed with modifiers applied to it.
    ///
    /// You should use this key for any single key bindings (e.g. motions).
    pub modified_key: keyboard::Key,
    /// The physical key pressed.
    ///
    /// You should use this key for layout-independent bindings.
    pub physical_key: keyboard::key::Physical,
    /// The state of the keyboard modifiers.
    pub modifiers: keyboard::Modifiers,
    /// The text produced by the key press.
    pub text: Option<SmolStr>,
    /// Whether the [`Editor`] is focused.
    pub is_focused: bool,
}

impl<Message> Binding<Message> {
    /// Returns the default [`Binding`] for the given key press.
    pub fn from_key_press(event: KeyPress) -> Option<Self> {
        let KeyPress {
            key,
            modified_key,
            physical_key,
            modifiers,
            text,
            is_focused,
        } = event;

        if !is_focused {
            return None;
        }

        let combination = match key.to_latin(physical_key) {
            Some('c') if modifiers.command() => Some(Self::Copy),
            Some('x') if modifiers.command() => Some(Self::Cut),
            Some('v') if modifiers.command() && !modifiers.alt() => Some(Self::Paste),
            Some('a') if modifiers.command() => Some(Self::SelectAll),
            Some('z') if modifiers.command() => Some(Self::Undo),
            Some('y') if modifiers.command() => Some(Self::Redo),
            _ => None,
        };

        if let Some(binding) = combination {
            return Some(binding);
        }

        #[cfg(target_os = "macos")]
        let modified_key = convert_macos_shortcut(&key, modifiers).unwrap_or(modified_key);

        match modified_key.as_ref() {
            keyboard::Key::Named(key::Named::Enter) => Some(Self::Enter),
            keyboard::Key::Named(key::Named::Backspace) => Some(
                if modifiers.macos_command() || (modifiers.command() && modifiers.shift()) {
                    Self::BackspaceLine
                } else if modifiers.jump() {
                    Self::BackspaceWord
                } else {
                    Self::Backspace
                },
            ),
            keyboard::Key::Named(key::Named::Delete)
                if text.is_none() || text.as_deref() == Some("\u{7f}") =>
            {
                Some(
                    if modifiers.macos_command() || (modifiers.command() && modifiers.shift()) {
                        Self::DeleteLine
                    } else if modifiers.jump() {
                        Self::DeleteWord
                    } else {
                        Self::Delete
                    },
                )
            }
            keyboard::Key::Named(key::Named::Escape) => Some(Self::Unfocus),
            _ => {
                if let Some(text) = text {
                    let c = text.chars().find(|c| !c.is_control())?;

                    Some(Self::Insert(c))
                } else if let keyboard::Key::Named(named_key) = key.as_ref() {
                    let motion = motion(named_key)?;

                    let motion = if modifiers.macos_command() {
                        match motion {
                            Motion::Left => Motion::Home,
                            Motion::Right => Motion::End,
                            _ => motion,
                        }
                    } else {
                        motion
                    };

                    let motion = if modifiers.jump() {
                        motion.widen()
                    } else {
                        motion
                    };

                    Some(if modifiers.shift() {
                        Self::Select(motion)
                    } else {
                        Self::Move(motion)
                    })
                } else {
                    None
                }
            }
        }
    }
}

/// The update of an [`Editor`], returned by [`State::update`].
pub enum Update<Message> {
    /// An [`Action`] must be performed in the [`Editor`].
    Action(Action),
    /// The [`Editor`] just gained focus.
    Focus,
    /// The [`Editor`] just lost focus.
    Unfocus,
    /// The [`Editor`] changed its [`InputMethod`].
    InputMethod,
    /// A mouse press was released in the [`Editor`].
    Release,
    /// The [`Editor`] must copy some text to the clipboard.
    Copy(String),
    /// The [`Editor`] must paste the clipboard contents.
    Paste,
    /// The [`Editor`] must be redrawn at the given [`Instant`].
    RedrawAt(Instant),
    /// The [`Editor`] produced a custom message that must be published.
    Custom(Message),
    /// The [`Editor`] produced a sequence of updates.
    Sequence(Vec<Update<Message>>),
}

fn motion(key: key::Named) -> Option<Motion> {
    match key {
        key::Named::ArrowLeft => Some(Motion::Left),
        key::Named::ArrowRight => Some(Motion::Right),
        key::Named::ArrowUp => Some(Motion::Up),
        key::Named::ArrowDown => Some(Motion::Down),
        key::Named::Home => Some(Motion::Home),
        key::Named::End => Some(Motion::End),
        key::Named::PageUp => Some(Motion::PageUp),
        key::Named::PageDown => Some(Motion::PageDown),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn convert_macos_shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
) -> Option<keyboard::Key> {
    if modifiers != keyboard::Modifiers::CTRL {
        return None;
    }

    let key = match key.as_ref() {
        keyboard::Key::Character("b") => key::Named::ArrowLeft,
        keyboard::Key::Character("f") => key::Named::ArrowRight,
        keyboard::Key::Character("a") => key::Named::Home,
        keyboard::Key::Character("e") => key::Named::End,
        keyboard::Key::Character("h") => key::Named::Backspace,
        keyboard::Key::Character("d") => key::Named::Delete,
        _ => return None,
    };

    Some(keyboard::Key::Named(key))
}

impl<T: Editor> TextInput for T {
    fn text(&self) -> text::Fragment<'_> {
        text::Fragment::Owned(Editor::text(self))
    }

    fn move_cursor_to_front(&mut self) {
        let _ = self.perform(Action::Move(Motion::DocumentStart));
    }

    fn move_cursor_to_end(&mut self) {
        let _ = self.perform(Action::Move(Motion::DocumentEnd));
    }

    fn move_cursor_to(&mut self, position: text::Position) {
        self.move_to(Cursor {
            position,
            selection: None,
        });
    }

    fn select_all(&mut self) {
        let _ = self.perform(Action::SelectAll);
    }

    /// Selects from `start` (the anchor) to `end`, where the caret goes, as
    /// a text input does (enzICED).
    fn select_range(&mut self, start: text::Position, end: text::Position) {
        self.move_to(Cursor {
            position: end,
            selection: Some(start),
        });
    }

    fn accessible_text(&self) -> Option<crate::widget::operation::text_input::AccessibleText> {
        use crate::widget::operation::text_input::{AccessibleLine, AccessibleText};
        // A document this long is described another way (enzUI's virtual
        // text surface); copying it every frame would not scale.
        const MAX_LINES: usize = 4096;
        if self.line_count() > MAX_LINES {
            return None;
        }
        let lines = (0..self.line_count())
            .filter_map(|index| self.line(index))
            .map(|line| AccessibleLine::new(line.text.into_owned()))
            .collect();
        let cursor = self.cursor();
        // The selected range is exact even for word and line selections,
        // whose stored anchor is not one of its ends; the caret is at the
        // end the cursor is on.
        let selection = match self.selection_range() {
            Some(range) if cursor.position == range.start => (range.end, range.start),
            Some(range) => (range.start, range.end),
            None => (cursor.position, cursor.position),
        };
        Some(AccessibleText {
            lines,
            selection: Some(selection),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, index: usize) -> Position {
        Position { line, index }
    }

    fn insert(start: Position, end: Position, text: &str) -> EditItem {
        EditItem {
            start,
            end,
            text: text.to_owned(),
            kind: EditKind::Insert,
        }
    }

    fn delete(start: Position, end: Position, text: &str) -> EditItem {
        EditItem {
            start,
            end,
            text: text.to_owned(),
            kind: EditKind::Delete,
        }
    }

    /// A [`Position`] is a line plus a *byte* column, so the offsets have to be
    /// found by splitting lines exactly the way an editor does -- over every
    /// line ending, and over multibyte text.
    #[test]
    fn byte_offsets_follow_every_line_ending() {
        // The `LineIter` fixture: LF, CRLF, CR, LFCR and a final unterminated
        // line, plus multibyte columns.
        let text = "LF\nCRLF\r\nCR\rLFCR\n\rÜnï";

        assert_eq!(byte_offset(text, at(0, 0)), Some(0));
        assert_eq!(byte_offset(text, at(0, 2)), Some(2));
        assert_eq!(byte_offset(text, at(1, 0)), Some(3));
        assert_eq!(byte_offset(text, at(1, 4)), Some(7));
        assert_eq!(byte_offset(text, at(2, 0)), Some(9));
        assert_eq!(byte_offset(text, at(3, 0)), Some(12));
        assert_eq!(byte_offset(text, at(4, 0)), Some(18));

        // "Ünï": Ü is two bytes, n one, ï two.
        assert_eq!(byte_offset(text, at(4, 5)), Some(23));
        assert_eq!(&text[18..23], "Ünï");

        // Past the end of a line, past the last line, and inside a character
        // are all failures, not silently clamped answers.
        assert_eq!(byte_offset(text, at(0, 3)), None);
        assert_eq!(byte_offset(text, at(5, 0)), None);
        assert_eq!(byte_offset(text, at(4, 1)), None);
    }

    #[test]
    fn apply_replays_inserts_and_deletes_in_order() {
        let mut document = String::from("alpha\nbeta\ngamma");

        // Replacing a cross-line selection: delete then insert, where the
        // insert's position only makes sense after the delete was applied.
        let delta = EditDelta::new([
            delete(at(0, 2), at(2, 3), "pha\nbeta\ngam"),
            insert(at(0, 2), at(0, 3), "X"),
        ])
        .expect("two real items");

        assert!(delta.apply(&mut document));
        assert_eq!(document, "alXma");
        assert_eq!(delta.bytes(), "pha\nbeta\ngam".len() + 1);
        assert_eq!(delta.items().len(), 2);
    }

    #[test]
    fn apply_reports_a_document_that_has_drifted() {
        let mut document = String::from("alpha");
        let delta = EditDelta::new([insert(at(4, 0), at(4, 1), "X")]).expect("one item");

        assert!(
            !delta.apply(&mut document),
            "line 4 does not exist in a one-line document"
        );
        assert_eq!(document, "alpha", "a rejected delta must not corrupt it");
    }

    /// A deletion carries the text it removed, so a copy that has drifted is a
    /// detectable error rather than silent corruption.
    #[test]
    fn apply_rejects_a_deletion_whose_text_is_not_there() {
        let mut document = String::from("alpha\nbeta");
        let delta = EditDelta::new([delete(at(1, 0), at(1, 4), "BETA")]).expect("one item");

        assert!(!delta.apply(&mut document), "\"beta\" is not \"BETA\"");
        assert_eq!(document, "alpha\nbeta");

        let delta = EditDelta::new([delete(at(1, 0), at(1, 4), "beta")]).expect("one item");

        assert!(delta.apply(&mut document), "the real text is accepted");
        assert_eq!(document, "alpha\n");
    }

    /// The documented ambiguity: `\n\r` is one line ending, so a document that
    /// puts an LF line straight before a CR one addresses differently than the
    /// editor that produced it by editing. This pins which reading `apply`
    /// uses -- the one an editor uses when it is *given* the text.
    #[test]
    fn an_lf_followed_by_a_cr_is_one_line_ending() {
        let text = "a\n\rb";

        assert_eq!(byte_offset(text, at(1, 0)), Some(3));
        assert_eq!(byte_offset(text, at(2, 0)), None, "there is no third line");
    }

    /// An action that changes no bytes has no delta at all: an empty one would
    /// make every caller check `items()` before trusting `Some`, and would
    /// break the type's non-empty invariant (there is no `Default`).
    ///
    /// A change is bytes: an empty-text item is dropped whether its endpoints
    /// match or not. A range with no text is a producer bug (a deletion always
    /// carries the bytes it removed), and the constructor filters it out
    /// rather than surfacing an unreplayable claim.
    #[test]
    fn a_delta_that_changes_no_bytes_is_none() {
        assert_eq!(EditDelta::new([]), None);
        assert_eq!(EditDelta::new([insert(at(0, 0), at(0, 0), "")]), None);
        assert_eq!(EditDelta::new([delete(at(1, 2), at(1, 2), "")]), None);

        // An empty-text item with differing endpoints still moves no bytes:
        // an empty insert or delete changes nothing regardless of the range it
        // claims, and is dropped just like the matching-endpoint case above.
        assert_eq!(EditDelta::new([delete(at(0, 0), at(0, 1), "")]), None);
        assert_eq!(EditDelta::new([insert(at(0, 0), at(0, 3), "")]), None);

        // Several items that each move no bytes still add up to nothing.
        assert_eq!(
            EditDelta::new([
                insert(at(0, 0), at(0, 0), ""),
                delete(at(0, 0), at(0, 2), ""),
            ]),
            None
        );

        // A real item alongside empty ones survives with the empties dropped.
        let delta = EditDelta::new([
            insert(at(0, 0), at(0, 0), ""),
            insert(at(0, 0), at(0, 1), "X"),
            delete(at(0, 0), at(0, 4), ""),
        ])
        .expect("the real insert survives");

        assert_eq!(delta.items().len(), 1);
        assert_eq!(delta.bytes(), 1);
    }
}
