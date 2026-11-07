// Modified by enzUI contributors. See FORK.md.
//! Parse text.
use std::ops::Range;

/// One line-level splice of an edit, in the coordinates the previous splice in
/// the same slice left behind (enzICED).
///
/// This is the line-granular projection of the editor's own
/// [`EditDelta`](crate::text::editor::EditDelta) items: an insertion of `k` line
/// breaks at line `l` is `{ at: l, removed: 1, inserted: 1 + k }`; a deletion
/// spanning `k` line breaks is `{ at: l, removed: 1 + k, inserted: 1 }`. Ordered
/// application is the composition, so disjoint edits whose suffixes shift by
/// different partial amounts stay correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineEdit {
    /// First line the splice replaces.
    pub at: usize,
    /// Existing line slots removed at `at`.
    pub removed: usize,
    /// New line slots inserted at `at`.
    pub inserted: usize,
}

/// A type capable of parsing text, producing some `Output`.
///
/// A [`Parser`] parses lines in sequence. When a line changes,
/// it must be notified and the lines after the changed one must be fed
/// again to the [`Parser`].
pub trait Parser: 'static {
    /// The settings to configure the [`Parser`].
    type Settings: PartialEq + Clone;

    /// The output of this [`Parser`].
    type Output;

    /// The parse iterator type.
    type Iterator<'a>: Iterator<Item = (Range<usize>, Self::Output)>
    where
        Self: 'a;

    /// Creates a new [`Parser`] from its [`Self::Settings`].
    fn new(settings: &Self::Settings) -> Self;

    /// Updates the [`Parser`] with some new [`Self::Settings`].
    fn update(&mut self, new_settings: &Self::Settings);

    /// Notifies the [`Parser`] that the line at the given index has changed.
    fn change_line(&mut self, line: usize);

    /// Notifies the [`Parser`] of the exact ordered line-level edits made
    /// since the previous parse (enzICED).
    ///
    /// `edits` are in ascending application order: each addresses the document
    /// the previous one left. The default preserves the historical single-line
    /// signal by forwarding the topmost edited line to
    /// [`change_line`](Self::change_line), so parsers that keep no cross-line
    /// state are unaffected. A stateful parser overrides this to re-key its
    /// checkpoints and reuse the unedited suffix.
    fn note_edits(&mut self, edits: &[LineEdit]) {
        if let Some(topmost) = edits.iter().map(|edit| edit.at).min() {
            self.change_line(topmost);
        }
    }

    /// Parses the given line.
    ///
    /// If a line changed prior to this, the first line provided here will be the
    /// line that changed.
    fn parse_line(&mut self, line: &str) -> Self::Iterator<'_>;

    /// Whether this parser supplies theme-independent font and size attributes during layout.
    /// Defaults to `false`; painting still uses the caller's full style formatter.
    fn has_layout_style(&self) -> bool {
        false
    }

    /// Font and size attributes needed before intrinsic editor measurement.
    /// Colors and decorations belong to the paint formatter.
    fn layout_style(_output: &Self::Output) -> super::highlighter::Style {
        super::highlighter::Style::default()
    }

    /// Returns the current line of the [`Parser`].
    ///
    /// If `change_line` has been called, this will normally be the least index
    /// that changed.
    fn current_line(&self) -> usize;

    /// Whether output is independent of preceding input and the logical
    /// document line index (enzICED).
    fn line_agnostic(&self) -> bool {
        false
    }

    /// Tries to move directly to `line` without parsing every preceding line
    /// (enzICED).
    ///
    /// This must return `true` only when the parser can produce exactly the
    /// same output at `line` without consuming the skipped input, and it must
    /// then report `line` from [`current_line`]. Returning `false` must leave
    /// the parser untouched. A stateful parser should keep the default and
    /// return `false`; an independent per-line tokenizer can update its
    /// bookkeeping and return `true`.
    ///
    /// Text editors use this seam when a cursor jump moves the viewport
    /// thousands of lines, keeping the next paint bounded by visible work. They
    /// are expected to ask only when the alternative is replaying skipped
    /// lines, so a steady frame that scrolled nothing costs nothing here.
    ///
    /// [`current_line`]: Self::current_line
    fn seek_line(&mut self, _line: usize) -> bool {
        false
    }
}

/// A parser that produces no output.
#[derive(Debug, Clone, Copy)]
pub struct PlainText;

impl Parser for PlainText {
    type Settings = ();
    type Output = ();

    type Iterator<'a> = std::iter::Empty<(Range<usize>, ())>;

    fn new(_settings: &Self::Settings) -> Self {
        Self
    }

    fn update(&mut self, _new_settings: &Self::Settings) {}

    fn change_line(&mut self, _line: usize) {}

    fn parse_line(&mut self, _line: &str) -> Self::Iterator<'_> {
        std::iter::empty()
    }

    fn line_agnostic(&self) -> bool {
        true
    }

    fn current_line(&self) -> usize {
        usize::MAX
    }
}
