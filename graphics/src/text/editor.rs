// Modified by enzUI contributors. See FORK.md.
//! Draw and edit text.
use crate::core::text::editor::{
    self, Action, Cursor, Direction, Edit, EditDelta, EditItem, EditKind, Motion, Position,
    ReplaceBatch, ScrollMetrics, Selection,
};
use crate::core::text::highlighter;
use crate::core::text::parser::{LineEdit, Parser};
use crate::core::text::{Alignment, LineHeight, Wrapping};
use crate::core::{Color, Font, Pixels, Point, Rectangle, Size, Vector};
use crate::text;

use cosmic_text::Edit as _;

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::{self, Arc, RwLock};

pub mod snapshot;

pub mod instrument {
    //! Counters for the viewport-bounded work an [`Editor`](super::Editor)
    //! performs (enzUI-ICED #1803/#1809).
    //!
    //! A selection that grows offscreen, or a pointer/scroll action in a
    //! multi-MiB document, must not grow the work per action. That is a
    //! *counter* claim before it is a wall-clock one: these counters expose
    //! both iced's visible work and cosmic-text's dirty discovery/shaping, so
    //! a fast machine cannot hide a whole-buffer scan.
    use std::cell::Cell;

    thread_local! {
        static LOGICAL_LINES_VISITED: Cell<u64> = const { Cell::new(0) };
        static VISUAL_ROWS_SHAPED: Cell<u64> = const { Cell::new(0) };
        static SELECTION_QUADS: Cell<u64> = const { Cell::new(0) };
        static BYTES_COPIED: Cell<u64> = const { Cell::new(0) };
        static SCROLL_INDEX_UPDATES: Cell<u64> = const { Cell::new(0) };
        static DELTA_ITEMS_RETURNED: Cell<u64> = const { Cell::new(0) };
        static DELTA_BYTES_RETURNED: Cell<u64> = const { Cell::new(0) };
        static HIGHLIGHT_LINES_VISITED: Cell<u64> = const { Cell::new(0) };
        static RESET_ARM_TAKEN: Cell<u64> = const { Cell::new(0) };
        static PENDING_EDITS_CAPACITY: Cell<usize> = const { Cell::new(0) };
        static ALIGNMENT_LINES_VISITED: Cell<u64> = const { Cell::new(0) };
    }

    /// A snapshot of the editor instrumentation counters.
    ///
    /// The counters are per-thread: an editor does its work on the thread that
    /// drives it, so a caller measuring one editor is never charged for another
    /// thread's frames.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Counters {
        /// Logical lines whose geometry was walked (selection painting and
        /// scroll-index refinement).
        pub logical_lines_visited: u64,
        /// Visual (wrapped) rows whose laid-out extent was read.
        pub visual_rows_shaped: u64,
        /// Selection rectangles handed to the renderer.
        pub selection_quads: u64,
        /// Bytes of selected text materialized for the clipboard.
        pub bytes_copied: u64,
        /// Sparse scroll-index corrections written (an estimate refined into a
        /// measured height, or a measured line re-measured to a new height).
        /// A fresh index seeds nothing, so this stays O(visible rows) per frame.
        pub scroll_index_updates: u64,
        /// Items of edit delta handed back by
        /// [`perform`](crate::core::text::Editor::perform).
        pub delta_items_returned: u64,
        /// Bytes of edit text copied out of the editor and handed back by
        /// [`perform`](crate::core::text::Editor::perform).
        ///
        /// This is the number enzUI-ICED #1805 exists for: a one-character edit
        /// of a 40 MiB document must move a handful of bytes, and a caret
        /// motion, drag or scroll must move none.
        pub delta_bytes_returned: u64,
        /// Calls into cosmic-text's shaping entry points.
        pub cosmic_shape_calls: u64,
        /// Conservative whole-buffer dirty-discovery passes.
        pub cosmic_dirty_scan_calls: u64,
        /// Logical lines inspected by cosmic-text dirty discovery.
        pub cosmic_dirty_lines_inspected: u64,
        /// Existing layouts invalidated by buffer-wide style/metric changes.
        pub cosmic_layouts_invalidated: u64,
        /// Logical lines whose shape or layout cache was rebuilt.
        pub cosmic_lines_shaped: u64,
        /// Visible-window shaping passes cosmic-text did not skip.
        ///
        /// A frame that changed nothing must announce nothing, so this is the
        /// counter that separates "the editor told the buffer it was dirty"
        /// from "the buffer had nothing to do" (enzUI-ICED #1802).
        pub cosmic_visible_shape_passes: u64,
        /// Logical lines inspected while installing or releasing a per-line
        /// alignment.
        ///
        /// An editor that never took an alignment must record zero here for
        /// every layout, and releasing one must cost a single pass.
        pub alignment_lines_visited: u64,
        /// Logical lines handed to the highlighter and re-attributed by
        /// [`highlight`](crate::core::text::Editor::highlight).
        ///
        /// A cursor jump must bound this by the visible window instead of the
        /// document, and a steady frame that scrolled nothing must record zero
        /// (enzUI-ICED #1802).
        pub highlight_lines_visited: u64,
        /// Times the update arm delivered a queued whole-document reset by
        /// calling `change_line(0)` on the highlighter after seeing
        /// `pending_full_reset == true` (enzUI-ICED #2802).
        ///
        /// This is the sole observable that discriminates the arm-ordering
        /// state machine: without the reset-first arm a subsequent edit's
        /// descriptors would silently displace the queued reset and this
        /// counter would stay at zero across the buggy sequence.
        pub reset_arm_taken: u64,
    }

    impl Counters {
        /// Returns the work recorded since `earlier`.
        pub fn since(self, earlier: Self) -> Self {
            Self {
                logical_lines_visited: self
                    .logical_lines_visited
                    .saturating_sub(earlier.logical_lines_visited),
                visual_rows_shaped: self
                    .visual_rows_shaped
                    .saturating_sub(earlier.visual_rows_shaped),
                selection_quads: self.selection_quads.saturating_sub(earlier.selection_quads),
                bytes_copied: self.bytes_copied.saturating_sub(earlier.bytes_copied),
                scroll_index_updates: self
                    .scroll_index_updates
                    .saturating_sub(earlier.scroll_index_updates),
                delta_items_returned: self
                    .delta_items_returned
                    .saturating_sub(earlier.delta_items_returned),
                delta_bytes_returned: self
                    .delta_bytes_returned
                    .saturating_sub(earlier.delta_bytes_returned),
                cosmic_shape_calls: self
                    .cosmic_shape_calls
                    .saturating_sub(earlier.cosmic_shape_calls),
                cosmic_dirty_scan_calls: self
                    .cosmic_dirty_scan_calls
                    .saturating_sub(earlier.cosmic_dirty_scan_calls),
                cosmic_dirty_lines_inspected: self
                    .cosmic_dirty_lines_inspected
                    .saturating_sub(earlier.cosmic_dirty_lines_inspected),
                cosmic_layouts_invalidated: self
                    .cosmic_layouts_invalidated
                    .saturating_sub(earlier.cosmic_layouts_invalidated),
                cosmic_lines_shaped: self
                    .cosmic_lines_shaped
                    .saturating_sub(earlier.cosmic_lines_shaped),
                cosmic_visible_shape_passes: self
                    .cosmic_visible_shape_passes
                    .saturating_sub(earlier.cosmic_visible_shape_passes),
                alignment_lines_visited: self
                    .alignment_lines_visited
                    .saturating_sub(earlier.alignment_lines_visited),
                highlight_lines_visited: self
                    .highlight_lines_visited
                    .saturating_sub(earlier.highlight_lines_visited),
                reset_arm_taken: self.reset_arm_taken.saturating_sub(earlier.reset_arm_taken),
            }
        }
    }

    /// Reads the current counters of the calling thread.
    pub fn counters() -> Counters {
        let cosmic = cosmic_text::instrument::counters();

        Counters {
            logical_lines_visited: LOGICAL_LINES_VISITED.get(),
            visual_rows_shaped: VISUAL_ROWS_SHAPED.get(),
            selection_quads: SELECTION_QUADS.get(),
            bytes_copied: BYTES_COPIED.get(),
            scroll_index_updates: SCROLL_INDEX_UPDATES.get(),
            delta_items_returned: DELTA_ITEMS_RETURNED.get(),
            delta_bytes_returned: DELTA_BYTES_RETURNED.get(),
            cosmic_shape_calls: cosmic.shape_calls,
            cosmic_dirty_scan_calls: cosmic.dirty_scan_calls,
            cosmic_dirty_lines_inspected: cosmic.dirty_lines_inspected,
            cosmic_layouts_invalidated: cosmic.layouts_invalidated,
            cosmic_lines_shaped: cosmic.lines_shaped,
            cosmic_visible_shape_passes: cosmic.visible_shape_passes,
            alignment_lines_visited: ALIGNMENT_LINES_VISITED.get(),
            highlight_lines_visited: HIGHLIGHT_LINES_VISITED.get(),
            reset_arm_taken: RESET_ARM_TAKEN.get(),
        }
    }

    /// Resets every counter of the calling thread to zero.
    pub fn reset() {
        LOGICAL_LINES_VISITED.set(0);
        VISUAL_ROWS_SHAPED.set(0);
        SELECTION_QUADS.set(0);
        BYTES_COPIED.set(0);
        SCROLL_INDEX_UPDATES.set(0);
        DELTA_ITEMS_RETURNED.set(0);
        DELTA_BYTES_RETURNED.set(0);
        HIGHLIGHT_LINES_VISITED.set(0);
        RESET_ARM_TAKEN.set(0);
        ALIGNMENT_LINES_VISITED.set(0);
        cosmic_text::instrument::reset();
    }

    fn add(counter: &'static std::thread::LocalKey<Cell<u64>>, amount: usize) {
        counter.set(counter.get().saturating_add(amount as u64));
    }

    pub(super) fn logical_lines_visited(amount: usize) {
        add(&LOGICAL_LINES_VISITED, amount);
    }

    pub(super) fn visual_rows_shaped(amount: usize) {
        add(&VISUAL_ROWS_SHAPED, amount);
    }

    pub(super) fn selection_quads(amount: usize) {
        add(&SELECTION_QUADS, amount);
    }

    pub(super) fn bytes_copied(amount: usize) {
        add(&BYTES_COPIED, amount);
    }

    pub(super) fn scroll_index_updates(amount: usize) {
        add(&SCROLL_INDEX_UPDATES, amount);
    }

    pub(super) fn delta_returned(items: usize, bytes: usize) {
        add(&DELTA_ITEMS_RETURNED, items);
        add(&DELTA_BYTES_RETURNED, bytes);
    }

    pub(super) fn highlight_lines_visited(amount: usize) {
        add(&HIGHLIGHT_LINES_VISITED, amount);
    }

    /// Records one call to the reset arm in the update dispatch
    /// (enzUI-ICED #2802).
    pub(super) fn reset_arm_taken() {
        RESET_ARM_TAKEN.with(|c| c.set(c.get() + 1));
    }

    /// Records the current allocated capacity of the graphics editor's
    /// ephemeral `pending_edits` queue after any operation that may change
    /// it (perform push / preflight fallback / update drain / reset arm)
    /// (enzUI-ICED #2802).
    pub(super) fn set_pending_edits_capacity(capacity: usize) {
        PENDING_EDITS_CAPACITY.with(|c| c.set(capacity));
    }

    /// Returns the last recorded allocated capacity of the graphics
    /// editor's ephemeral `pending_edits` queue on this thread
    /// (enzUI-ICED #2802).
    ///
    /// This is a *snapshot*, not a monotonic counter. It is intended for
    /// asserting the bounded-allocation invariant
    /// (`capacity <= MAX_PENDING_EDITS`, and `capacity == 0` after a
    /// preflight-overflow or a whole-document reset) from downstream
    /// tests. It is not part of `Counters` because `Counters::since`
    /// subtracts field pairs and "capacity now minus capacity then"
    /// is not a meaningful delta.
    #[doc(hidden)]
    pub fn pending_edits_capacity() -> usize {
        PENDING_EDITS_CAPACITY.with(std::cell::Cell::get)
    }

    pub(super) fn alignment_lines_visited(amount: usize) {
        add(&ALIGNMENT_LINES_VISITED, amount);
    }
}

/// A multi-line text editor.
#[derive(Debug, PartialEq)]
pub struct Editor(Option<Arc<Internal>>);

struct Internal {
    identity: Arc<()>,
    revision: Arc<()>,
    editor: cosmic_text::Editor<'static>,
    selection: RwLock<Option<Selection>>,
    history: History,
    font: Font,
    bounds: Size,
    /// The whole-editor alignment the widget last asked for (upstream), and
    /// how many lines had it applied.
    alignment: Alignment,
    aligned_lines: usize,
    topmost_line_changed: Option<usize>,
    /// Ordered line-level descriptors of every edit made since the last
    /// update consumed the queue.
    ///
    /// This is a per-frame ephemeral event batch, not a duplicate
    /// history stream: the cosmic-text buffer inside `editor` remains the
    /// sole edit-history owner; this queue is bridge state between
    /// `perform` (app-called) and `update` (widget-called with the
    /// highlighter in hand). Bounded by `MAX_PENDING_EDITS`
    /// (enzUI-ICED #2802).
    pending_edits: Vec<LineEdit>,
    /// Set when the descriptor stream became untrustable (a font or
    /// `FontSystem` version reset, or `MAX_PENDING_EDITS` overflow) and
    /// the next update must call `change_line(0)`. A separate flag is
    /// required because `topmost_line_changed = Some(0)` from an ordinary
    /// edit at line 0 is a partial invalidation whose descriptor must
    /// still be forwarded; a single-encoded sentinel could not distinguish
    /// the two, and a subsequent edit's assignment to
    /// `topmost_line_changed` would demote a queued reset to a partial
    /// position (enzUI-ICED #2802).
    pending_full_reset: bool,
    hint: bool,
    hint_factor: f32,
    version: text::Version,
    scroll_index: Option<ScrollIndex>,
    resolved_scroll_metrics: Option<ScrollMetrics>,
    line_alignment_installed: bool,
    /// The rectangles the text flows around, in logical pixels; applied to
    /// the buffer in physical pixels on `update`, where the hint factor is
    /// known.
    exclusions: Vec<editor::Exclusion>,
    /// Where each exclusion sat after the last update, in logical pixels.
    exclusion_bounds: Vec<Option<Rectangle>>,
    /// The contiguous logical-line range whose attributes the last
    /// [`highlight`](crate::core::text::Editor::highlight) call installed.
    ///
    /// Attributes live on the buffer lines, so a window that was already
    /// highlighted needs no work when it scrolls back into view. This record
    /// is what lets the seek seam below stay off the steady-state path: it is
    /// trimmed by the highlighter's own `current_line` invalidation, so an
    /// edit, theme change or settings change still re-attributes (#1802).
    highlighted: Option<Range<usize>>,
    /// The logical lines the last update showed, and how many lines have come
    /// into view since off-screen layouts were last released (enzICED #1).
    visible_lines: Range<usize>,
    exposed_lines: usize,
}

impl Editor {
    /// Creates a new empty [`Editor`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the buffer of the [`Editor`].
    pub fn buffer(&self) -> &cosmic_text::Buffer {
        buffer_from_editor(&self.internal().editor)
    }

    /// Creates a [`Weak`] reference to the [`Editor`].
    ///
    /// This is useful to avoid cloning the [`Editor`] when
    /// referential guarantees are unnecessary. For instance,
    /// when creating a rendering tree.
    pub fn downgrade(&self) -> Weak {
        let editor = self.internal();

        Weak {
            raw: Arc::downgrade(editor),
            bounds: editor.bounds,
        }
    }

    fn internal(&self) -> &Arc<Internal> {
        self.0
            .as_ref()
            .expect("Editor should always be initialized")
    }

    fn with_internal_mut<T>(&mut self, f: impl FnOnce(&mut Internal) -> T) -> T {
        self.with_geometry_mut(|internal| {
            internal.revision = Arc::new(());
            f(internal)
        })
        .expect("Editor cannot have multiple strong references")
    }

    fn with_geometry_mut<T>(&mut self, f: impl FnOnce(&mut Internal) -> T) -> Result<T, ()> {
        let editor = self.0.take().expect("Editor should always be initialized");

        let mut internal = match Arc::try_unwrap(editor) {
            Ok(internal) => internal,
            Err(shared) => {
                self.0 = Some(shared);
                return Err(());
            }
        };
        let cleared = match internal.selection.try_write() {
            Ok(mut selection) => {
                let _ = selection.take();
                true
            }
            Err(_) => false,
        };
        if !cleared {
            self.0 = Some(Arc::new(internal));
            return Err(());
        }

        internal.resolved_scroll_metrics = None;
        let result = f(&mut internal);

        self.0 = Some(Arc::new(internal));

        Ok(result)
    }
}

impl editor::Editor for Editor {
    fn with_text(text: &str) -> Self {
        let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics {
            font_size: 1.0,
            line_height: 1.0,
        });

        let font_system = text::font_system().write().expect("Write font system");

        buffer.set_text(
            text,
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Advanced,
            None,
        );

        // enzUI-ICED #1804: this buffer is private to `Internal`. Nothing in
        // this crate's public surface hands it out mutably -- `Editor::buffer`
        // returns `&Buffer`, `buffer_mut_from_editor` is private to this
        // module -- so every mutation of `lines` either goes through
        // cosmic-text's own APIs or is announced below with
        // `note_encapsulated_line_mutation`. That promise lets shaping stop
        // reading the whole document on every frame looking for a change it
        // will now be told about.
        buffer.set_lines_encapsulated(true);

        // No `shape_until_scroll` here (enzUI-ICED #1803). The buffer has no
        // size yet, so `height_opt` is `None` and cosmic-text would lay out
        // *every* line of the document -- at a 1.0 px placeholder font size that
        // the first `update` throws away. `set_text` still parses the lines and
        // their attributes; glyph layout waits for real bounds.

        Editor(Some(Arc::new(Internal {
            editor: cosmic_text::Editor::new(buffer),
            version: font_system.version(),
            scroll_index: None,
            ..Default::default()
        })))
    }

    fn is_empty(&self) -> bool {
        let buffer = self.buffer();

        buffer.lines.is_empty() || (buffer.lines.len() == 1 && buffer.lines[0].text().is_empty())
    }

    fn line(&self, index: usize) -> Option<editor::Line<'_>> {
        self.buffer().lines.get(index).map(|line| editor::Line {
            text: Cow::Borrowed(line.text()),
            ending: match line.ending() {
                cosmic_text::LineEnding::Lf => editor::LineEnding::Lf,
                cosmic_text::LineEnding::CrLf => editor::LineEnding::CrLf,
                cosmic_text::LineEnding::Cr => editor::LineEnding::Cr,
                cosmic_text::LineEnding::LfCr => editor::LineEnding::LfCr,
                cosmic_text::LineEnding::None => editor::LineEnding::None,
            },
        })
    }

    fn line_count(&self) -> usize {
        self.buffer().lines.len()
    }

    fn copy(&self) -> Option<String> {
        let selection = self.internal().editor.copy_selection();

        instrument::bytes_copied(selection.as_ref().map(String::len).unwrap_or(0));

        selection
    }

    fn selection_range(&self) -> Option<Range<editor::Position>> {
        self.internal()
            .editor
            .selection_bounds()
            .map(|(start, end)| {
                editor::Position {
                    line: start.line,
                    index: start.index,
                }..editor::Position {
                    line: end.line,
                    index: end.index,
                }
            })
    }

    fn selection(&self) -> editor::Selection {
        let internal = self.internal();

        if let Ok(Some(cursor)) = internal.selection.read().as_deref() {
            return cursor.clone();
        }

        let cursor = internal.editor.cursor();
        let buffer = buffer_from_editor(&internal.editor);
        let cursor = match internal.editor.selection_bounds() {
            Some((start, end)) => Selection::Range(selection_regions(
                buffer,
                start,
                end,
                viewport_height(internal.bounds, internal.hint_factor),
                internal.hint_factor,
            )),
            None => caret_point(buffer, cursor, internal.hint_factor)
                .map(Selection::Caret)
                .unwrap_or_else(|| Selection::Range(Vec::new())),
        };

        *internal.selection.write().expect("Write to cursor cache") = Some(cursor.clone());

        cursor
    }

    fn cursor(&self) -> Cursor {
        let editor = &self.internal().editor;

        let position = {
            let cursor = editor.cursor();

            Position {
                line: cursor.line,
                index: cursor.index,
            }
        };

        let selection = match editor.selection() {
            cosmic_text::Selection::None => None,
            cosmic_text::Selection::Normal(cursor)
            | cosmic_text::Selection::Line(cursor)
            | cosmic_text::Selection::Word(cursor) => Some(Position {
                line: cursor.line,
                index: cursor.index,
            }),
        };

        Cursor {
            position,
            selection,
        }
    }

    fn perform(&mut self, action: Action) -> Option<EditDelta> {
        let mut font_system = text::font_system().write().expect("Write font system");

        // Explicit scrolling preserves an off-screen selection without revealing its caret.
        let is_scroll = matches!(
            action,
            Action::Scroll { .. } | Action::ScrollTo { .. } | Action::ScrollToLine { .. }
        );

        // Any text edit can change a line's laid-out height *and* retire the
        // widest row (deleting the longest line). The sparse index tracks the
        // horizontal extent as a running maximum that never shrinks, so it is
        // reseeded from estimates after an edit -- O(1) now -- and the visible
        // rows re-measured below, rather than leaving a phantom width or a
        // stale per-line height behind. See enzUI-ICED #1803.
        let is_edit = matches!(action, Action::Edit(_));

        // A hit on a *fully visible* row cannot require reveal. The two far
        // edges need different insets because they are revealed by different
        // mechanisms, and using the caret nudge for both was wrong (#1809
        // review):
        //
        // * horizontally, `shape_until_cursor` nudges the scroll by at most
        //   `CURSOR_WIDTH` (it clamps to `0.0..=CURSOR_WIDTH`, so a caret at the
        //   left edge is never revealed leftwards) -- two pixels is exact;
        // * vertically, the reveal is a whole row: `update` sizes the buffer to
        //   the exact widget bounds, so the last shaped row is normally cut off
        //   by the viewport, and `Buffer::shape_until_cursor` scrolls that row
        //   fully into view (`total_height > height + scroll.vertical`). One
        //   full row is therefore the smallest honest inset: a hit at `y` sits
        //   in a row that ends at or before `y + row_height`, so the conserva-
        //   tive path keeps every partially visible row -- not just the last
        //   two pixels of it.
        //
        // The tallest visible row bounds caret reveal when spans override the base metrics.
        // Event coordinates use logical pixels rather than hinted buffer pixels.
        //
        // Everything outside this inset rectangle keeps cosmic-text's
        // conservative action + shape-until-cursor path (autoscroll included).
        const CURSOR_REVEAL_MARGIN: f32 = 2.0;
        let internal = self.internal();
        let bounds = internal.bounds;
        let row_reveal_margin = buffer_from_editor(&internal.editor)
            .layout_runs()
            .map(|run| run.line_height)
            .fold(0.0, f32::max)
            / internal.hint_factor;
        let prepared_pointer = match &action {
            // The prepared-layout fast path covers single clicks and drags;
            // a double or triple click selects a word or line through
            // cosmic-text's conservative path.
            Action::Click(position, crate::core::mouse::click::Kind::Single)
            | Action::Drag(position) => {
                row_reveal_margin.is_finite()
                    && row_reveal_margin > 0.0
                    && position.x >= 0.0
                    && position.y >= 0.0
                    && position.x + CURSOR_REVEAL_MARGIN <= bounds.width
                    && position.y + row_reveal_margin <= bounds.height
            }
            _ => false,
        };

        self.with_internal_mut(|internal| {
            let lines_before = buffer_from_editor(&internal.editor).lines.len();
            let editor = &mut internal.editor;

            // The exact change this action applied, once it is known. Undo and
            // redo replay a change they already own, so they fill this in
            // themselves; every other edit gets it from `finish_change` below.
            let mut applied: Option<cosmic_text::Change> = None;
            let cursor_before = EditorState::capture(editor);

            match action {
                // Motion events
                Action::Move(motion) => {
                    if let Some((start, end)) = editor.selection_bounds() {
                        editor.set_selection(cosmic_text::Selection::None);

                        match motion {
                            // These motions are performed as-is even when a selection
                            // is present
                            Motion::Home
                            | Motion::End
                            | Motion::DocumentStart
                            | Motion::DocumentEnd => {
                                editor.action(
                                    font_system.raw(),
                                    cosmic_text::Action::Motion(to_motion(motion)),
                                );
                            }
                            // Other motions simply move the cursor to one end of the selection
                            _ => editor.set_cursor(match motion.direction() {
                                Direction::Left => start,
                                Direction::Right => end,
                            }),
                        }
                    } else {
                        editor.action(
                            font_system.raw(),
                            cosmic_text::Action::Motion(to_motion(motion)),
                        );
                    }

                    let cursor = cosmic_text::Cursor {
                        affinity: cosmic_text::Affinity::Before,
                        ..editor.cursor()
                    };

                    editor.set_cursor(cursor);
                }

                // Selection events
                Action::Select(motion) => {
                    let cursor = editor.cursor();

                    if editor.selection_bounds().is_none() {
                        editor.set_selection(cosmic_text::Selection::Normal(cursor));
                    }

                    editor.action(
                        font_system.raw(),
                        cosmic_text::Action::Motion(to_motion(motion)),
                    );

                    // Deselect if selection matches cursor position
                    if let Some((start, end)) = editor.selection_bounds()
                        && start.line == end.line
                        && start.index == end.index
                    {
                        editor.set_selection(cosmic_text::Selection::None);
                    }
                }
                Action::SelectWord => {
                    let cursor = editor.cursor();

                    editor.set_selection(cosmic_text::Selection::Word(cursor));
                }
                Action::SelectLine => {
                    let cursor = editor.cursor();

                    editor.set_selection(cosmic_text::Selection::Line(cursor));
                }
                Action::SelectAll => {
                    let buffer = buffer_from_editor(editor);

                    if buffer.lines.len() > 1
                        || buffer
                            .lines
                            .first()
                            .is_some_and(|line| !line.text().is_empty())
                    {
                        let cursor = editor.cursor();

                        editor.set_selection(cosmic_text::Selection::Normal(cosmic_text::Cursor {
                            line: 0,
                            index: 0,
                            ..cursor
                        }));

                        editor.action(
                            font_system.raw(),
                            cosmic_text::Action::Motion(cosmic_text::Motion::BufferEnd),
                        );
                    }
                }

                // Editing events
                Action::Edit(edit) => {
                    let topmost_line_before_edit = editor
                        .selection_bounds()
                        .map(|(start, _)| start)
                        .unwrap_or_else(|| editor.cursor())
                        .line;

                    if !matches!(edit, Edit::Undo | Edit::Redo) {
                        editor.start_change();
                    }

                    match edit {
                        Edit::Insert(c) => {
                            editor.action(font_system.raw(), cosmic_text::Action::Insert(c));
                        }
                        Edit::Paste(text) => {
                            editor.insert_string(&text, None);
                        }
                        Edit::Indent => {
                            editor.action(font_system.raw(), cosmic_text::Action::Indent);
                        }
                        Edit::Unindent => {
                            editor.action(font_system.raw(), cosmic_text::Action::Unindent);
                        }
                        Edit::Enter => {
                            editor.action(font_system.raw(), cosmic_text::Action::Enter);
                        }
                        Edit::Backspace => {
                            editor.action(font_system.raw(), cosmic_text::Action::Backspace);
                        }
                        Edit::Delete => {
                            editor.action(font_system.raw(), cosmic_text::Action::Delete);
                        }
                        // Upstream: select to the PreviousWord boundary, then backspace.
                        Edit::BackspaceWord => {
                            if editor.selection() == cosmic_text::Selection::None {
                                editor
                                    .set_selection(cosmic_text::Selection::Normal(editor.cursor()));

                                editor.action(
                                    font_system.raw(),
                                    cosmic_text::Action::Motion(cosmic_text::Motion::PreviousWord),
                                );
                            }

                            editor.action(font_system.raw(), cosmic_text::Action::Backspace);
                        }
                        // Upstream: select to the Home boundary, then backspace.
                        Edit::BackspaceLine => {
                            if editor.selection() == cosmic_text::Selection::None {
                                editor
                                    .set_selection(cosmic_text::Selection::Normal(editor.cursor()));

                                editor.action(
                                    font_system.raw(),
                                    cosmic_text::Action::Motion(cosmic_text::Motion::Home),
                                );
                            }

                            editor.action(font_system.raw(), cosmic_text::Action::Backspace);
                        }
                        // Upstream: select to the NextWord boundary, then delete.
                        Edit::DeleteWord => {
                            if editor.selection() == cosmic_text::Selection::None {
                                editor
                                    .set_selection(cosmic_text::Selection::Normal(editor.cursor()));

                                editor.action(
                                    font_system.raw(),
                                    cosmic_text::Action::Motion(cosmic_text::Motion::NextWord),
                                );
                            }

                            editor.action(font_system.raw(), cosmic_text::Action::Delete);
                        }
                        // Upstream: select to the End boundary, then delete.
                        Edit::DeleteLine => {
                            if editor.selection() == cosmic_text::Selection::None {
                                editor
                                    .set_selection(cosmic_text::Selection::Normal(editor.cursor()));

                                editor.action(
                                    font_system.raw(),
                                    cosmic_text::Action::Motion(cosmic_text::Motion::End),
                                );
                            }

                            editor.action(font_system.raw(), cosmic_text::Action::Delete);
                        }
                        Edit::Replace(batch) => {
                            if valid_replace_batch(editor, &batch) {
                                apply_replace_batch(editor, &batch);
                            }
                        }
                        Edit::Undo => {
                            if let Some(entry) = internal.history.undo().cloned() {
                                let mut change = entry.change;
                                change.reverse();

                                // The delta is the *reversed* change, i.e. what
                                // was actually applied to the buffer -- and
                                // only if `apply_change` accepted it.
                                if editor.apply_change(&change) {
                                    entry.before.restore(editor);
                                    clamp_cursors(editor);
                                    applied = Some(change);
                                }
                            }
                        }
                        Edit::Redo => {
                            if let Some(entry) = internal.history.redo().cloned() {
                                let change = entry.change;

                                if editor.apply_change(&change) {
                                    entry.after.restore(editor);
                                    clamp_cursors(editor);
                                    applied = Some(change);
                                }
                            }
                        }
                    }

                    let cursor = editor.cursor();
                    let selection_start = editor
                        .selection_bounds()
                        .map(|(start, _)| start)
                        .unwrap_or(cursor);

                    internal.topmost_line_changed =
                        Some(selection_start.line.min(topmost_line_before_edit));
                }

                // Mouse events
                Action::Click(position, kind) => {
                    let scroll = buffer_from_editor(editor).scroll();

                    let x = ((position.x + scroll.horizontal) * internal.hint_factor) as i32;
                    let y = (position.y * internal.hint_factor) as i32;

                    // Upstream: double and triple clicks are cosmic-text's own
                    // word and line selection.
                    let action = match kind {
                        crate::core::mouse::click::Kind::Single => {
                            cosmic_text::Action::Click { x, y }
                        }
                        crate::core::mouse::click::Kind::Double => {
                            cosmic_text::Action::DoubleClick { x, y }
                        }
                        crate::core::mouse::click::Kind::Triple => {
                            cosmic_text::Action::TripleClick { x, y }
                        }
                    };

                    if prepared_pointer {
                        editor.action_with_prepared_layout(font_system.raw(), action);
                    } else {
                        editor.action(font_system.raw(), action);
                    };
                }
                Action::Drag(position) => {
                    let scroll = buffer_from_editor(editor).scroll();

                    // enzUI-ICED #1802 — held-edge autoscroll continuation.
                    //
                    // The widget's `RedrawRequested` re-arm keeps performing
                    // this Drag every frame while the pointer sits past the
                    // editor's inner top/bottom, but a Drag whose `y` is beyond
                    // the laid-out viewport hit-clamps to the last VISIBLE row:
                    // `shape_until_cursor` then reveals nothing and the viewport
                    // never advances (the stationary-edge stall proven for
                    // #1802). Detect the overshoot here — the lowest shared
                    // owner of the buffer scroll — and perform a BOUNDED explicit
                    // scroll toward the pointer, then hit the row the scroll just
                    // revealed at the edge so the selection cursor follows.
                    // cosmic-text's `shape_until_scroll` clamps scroll at
                    // document top/bottom, so this stops exactly at BOF/EOF. An
                    // in-bounds drag never overshoots, so it keeps the exact
                    // prepared-hit late-latch fast path unchanged.
                    let mut hit_y = position.y;
                    if row_reveal_margin.is_finite() && row_reveal_margin > 0.0 {
                        let overshoot_below = position.y - bounds.height;
                        let (overshoot, dir) = if overshoot_below > 0.0 {
                            (overshoot_below, 1.0_f32)
                        } else if position.y < 0.0 {
                            (-position.y, -1.0_f32)
                        } else {
                            (0.0, 0.0_f32)
                        };
                        if overshoot > 0.0 {
                            // One line at the edge, more the deeper the pointer
                            // sits past it, capped so a pointer parked far
                            // outside cannot jump the viewport unboundedly.
                            const MAX_AUTOSCROLL_LINES: f32 = 4.0;
                            let lines = (1.0 + (overshoot / row_reveal_margin).floor())
                                .min(MAX_AUTOSCROLL_LINES);
                            let cosmic_line_h = buffer_from_editor(editor).metrics().line_height;
                            editor.action(
                                font_system.raw(),
                                cosmic_text::Action::Scroll {
                                    pixels: dir * lines * cosmic_line_h,
                                },
                            );
                            // Clamp the hit into the viewport so the cursor lands
                            // on the freshly revealed edge row, extending the
                            // selection there rather than re-clamping to the old
                            // last-visible line.
                            hit_y = if dir > 0.0 {
                                (bounds.height - row_reveal_margin * 0.5).max(0.0)
                            } else {
                                row_reveal_margin * 0.5
                            };
                        }
                    }

                    let action = cosmic_text::Action::Drag {
                        x: ((position.x + scroll.horizontal) * internal.hint_factor) as i32,
                        y: (hit_y * internal.hint_factor) as i32,
                    };

                    if prepared_pointer {
                        editor.action_with_prepared_layout(font_system.raw(), action);
                    } else {
                        editor.action(font_system.raw(), action);
                    };

                    // Deselect if selection matches cursor position
                    if let Some((start, end)) = editor.selection_bounds()
                        && start.line == end.line
                        && start.index == end.index
                    {
                        editor.set_selection(cosmic_text::Selection::None);
                    }
                }
                Action::Scroll { lines } => {
                    editor.action(
                        font_system.raw(),
                        cosmic_text::Action::Scroll {
                            pixels: lines as f32 * buffer_from_editor(editor).metrics().line_height,
                        },
                    );
                }
                Action::ScrollToLine { line, offset } => {
                    // Resolve pending caret motion before installing an explicit viewport anchor.
                    editor.shape_as_needed(font_system.raw(), false);
                    let hint_factor = internal.hint_factor;
                    let buffer = buffer_mut_from_editor(editor);
                    if buffer.lines.is_empty() {
                        return None;
                    }
                    let line = line.min(buffer.lines.len().saturating_sub(1));
                    buffer.set_scroll(cosmic_text::Scroll {
                        line,
                        vertical: 0.0,
                        horizontal: 0.0,
                    });
                    buffer.shape_until_scroll(font_system.raw(), false);
                    let window = viewport_height(internal.bounds, hint_factor);
                    refine_scroll_index(&mut internal.scroll_index, buffer, window);
                    let content_width = scroll_index(&mut internal.scroll_index, buffer).width;
                    let horizontal = finite_nonnegative(offset.x * hint_factor)
                        .min((content_width - internal.bounds.width * hint_factor).max(0.0));
                    let row_height = buffer.metrics().line_height;
                    let height = buffer.lines[line]
                        .layout_opt()
                        .as_ref()
                        .map(|rows| {
                            rows.iter()
                                .map(|row| row.line_height(row_height))
                                .sum::<f32>()
                        })
                        .unwrap_or(row_height);
                    let vertical =
                        finite_nonnegative(offset.y * hint_factor).min((height - 0.5).max(0.0));
                    buffer.set_scroll(cosmic_text::Scroll {
                        line,
                        vertical,
                        horizontal,
                    });
                    buffer.shape_until_scroll(font_system.raw(), false);
                }
                Action::ScrollTo { offset } => {
                    let hint_factor = internal.hint_factor;
                    let viewport = internal.bounds * hint_factor;
                    let window = viewport_height(internal.bounds, hint_factor);
                    let index =
                        scroll_index(&mut internal.scroll_index, buffer_from_editor(&*editor));
                    let x = finite_nonnegative(offset.x * hint_factor)
                        .min((index.width - viewport.width).max(0.0));
                    let requested_y = finite_nonnegative(offset.y * hint_factor);
                    let max_y = (index.content_height() - viewport.height).max(0.0);

                    // End anchoring (enzUI-ICED #1803): a scrollbar dragged to
                    // the bottom asks for the maximum offset the *last* metrics
                    // reported. As the tail's one-row estimates refine into
                    // wrapped multi-row heights the content grows, so a literal
                    // absolute offset would now land short of EOF. Detect a
                    // request at (or past) the current maximum and re-pin it to
                    // the recomputed maximum on every pass; an ordinary
                    // in-document offset stays exactly where it was asked.
                    let anchor_end = requested_y >= max_y - 0.5;

                    // Bounded refine passes, not a whole-document layout: each
                    // pass lands on the line the current (partly estimated)
                    // geometry maps the target to, shapes that window and
                    // refines it, so the next pass re-solves against exact
                    // heights. An ordinary offset converges in two passes; an
                    // end anchor chases the maximum as the tail's one-row
                    // estimates refine into wrapped multi-row heights, and the
                    // loop stops as soon as a pass changes nothing -- capped so
                    // a pathological fixture can never make it unbounded.
                    let mut settled = None;

                    for _ in 0..6 {
                        let index =
                            scroll_index(&mut internal.scroll_index, buffer_from_editor(&*editor));
                        let target_y = if anchor_end {
                            (index.content_height() - viewport.height).max(0.0)
                        } else {
                            requested_y.min((index.content_height() - viewport.height).max(0.0))
                        };
                        let (line, vertical) = index.line_at(target_y);

                        // Converged: this pass would re-apply the same scroll,
                        // so the geometry has stopped moving under us.
                        if settled == Some((line, (vertical * 64.0) as i64)) {
                            break;
                        }
                        settled = Some((line, (vertical * 64.0) as i64));

                        let buffer = buffer_mut_from_editor(editor);

                        buffer.set_scroll(cosmic_text::Scroll {
                            line,
                            vertical,
                            horizontal: x,
                        });
                        buffer.shape_until_scroll(font_system.raw(), false);

                        refine_scroll_index(
                            &mut internal.scroll_index,
                            buffer_from_editor(&*editor),
                            window,
                        );
                    }
                }
            }

            // Exposing the change the backend already computed is the whole of
            // enzUI-ICED #1805: the alternative for a caller that mirrors the
            // document is flattening the editor after every keystroke, which is
            // O(document) per character.
            let mut delta = applied.as_ref().and_then(to_delta);

            if let Some(change) = editor.finish_change()
                && !change.items.is_empty()
            {
                delta = to_delta(&change);
                internal.history.push(HistoryEntry {
                    change,
                    before: cursor_before,
                    after: EditorState::capture(editor),
                });
            }

            if let Some(delta) = &delta {
                instrument::delta_returned(delta.items().len(), delta.bytes());
                // Line-level forwarding (enzUI-ICED #2802). A queued
                // whole-document reset already invalidates everything from
                // line 0; further descriptor pushes are dropped by the
                // reset arm at update time. Skipping the push here keeps
                // `pending_edits.is_empty()` while the reset is queued and
                // avoids re-tripping the overflow cap on every subsequent
                // burst.
                if !internal.pending_full_reset {
                    // Preflight overflow check (enzUI-ICED #2802): decide
                    // whether this delta would push us past the cap BEFORE
                    // materializing any descriptors. Reactive checks
                    // (`push_line_edits` then compare `len`) let one huge
                    // multi-line `Action` — e.g. `SelectAll` + `Indent`,
                    // which emits one `EditItem` per selected line and
                    // therefore one delta of arbitrary size — allocate the
                    // full descriptor list before the cap trips.
                    // `checked_add` folds arithmetic overflow into the
                    // exceed branch, so a saturated `usize` still resets
                    // rather than wrapping. On overflow we
                    // **replace** the queue with a fresh `Vec::new()`
                    // instead of `clear()`ing it, because `Vec::clear`
                    // retains the backing allocation; the whole point of
                    // the fallback is to shed the allocation, not to keep
                    // it around empty.
                    let incoming = delta.items().len();
                    let would_exceed = internal
                        .pending_edits
                        .len()
                        .checked_add(incoming)
                        .is_none_or(|sum| sum > MAX_PENDING_EDITS);
                    if would_exceed {
                        internal.pending_full_reset = true;
                        internal.pending_edits = Vec::new();
                    } else {
                        push_line_edits(delta, &mut internal.pending_edits);
                    }
                }
                instrument::set_pending_edits_capacity(internal.pending_edits.capacity());
            }

            if is_scroll {
                editor.shape_as_needed(font_system.raw(), false);
                pin_wrapped_scroll(editor);
            } else if !prepared_pointer {
                // An in-bounds prepared hit returned a cursor from the visible
                // layout itself. Running `shape_until_cursor` here used to call
                // cosmic shaping twice more while revealing nothing. Keep this
                // path for edits, keyboard motion, edge clicks and out-of-bounds
                // drags, all of which can need cursor reveal or autoscroll.
                shape_until_cursor(editor, &mut font_system.raw);
            }

            // An edit moves absolute offsets (added/removed lines) or changes a
            // line's height/width; either way the sparse index is reseeded from
            // estimates (O(1)) and the visible rows re-measured below, so a
            // deleted long line cannot leave a phantom extent behind.
            if is_edit || buffer_from_editor(&internal.editor).lines.len() != lines_before {
                internal.scroll_index = None;
            }

            if !prepared_pointer {
                let window = viewport_height(internal.bounds, internal.hint_factor);

                refine_scroll_index(
                    &mut internal.scroll_index,
                    buffer_from_editor(&internal.editor),
                    window,
                );
            }

            delta
        })
    }

    fn move_to(&mut self, cursor: Cursor) {
        self.with_internal_mut(|internal| {
            // TODO: Expose `Affinity`
            internal.editor.set_cursor(cosmic_text::Cursor {
                line: cursor.position.line,
                index: cursor.position.index,
                affinity: cosmic_text::Affinity::Before,
            });

            if let Some(selection) = cursor.selection {
                internal
                    .editor
                    .set_selection(cosmic_text::Selection::Normal(cosmic_text::Cursor {
                        line: selection.line,
                        index: selection.index,
                        affinity: cosmic_text::Affinity::Before,
                    }));
            }
        });
    }

    fn bounds(&self) -> Size {
        self.internal().bounds
    }

    fn presentation_identity(&self) -> Option<Arc<()>> {
        Some(self.internal().identity.clone())
    }

    fn scroll_metrics(&mut self) -> ScrollMetrics {
        if let Some(metrics) = self.internal().resolved_scroll_metrics {
            return metrics;
        }
        self.with_internal_mut(|internal| {
            let hint_factor = internal.hint_factor;
            let viewport = internal.bounds;
            let window = viewport_height(viewport, hint_factor);

            // Reading the scrollbar geometry is not a reason to lay out the
            // document (enzUI-ICED #1803): refine the index over the rows the
            // viewport already shaped, and report estimates for the rest.
            refine_scroll_index(
                &mut internal.scroll_index,
                buffer_from_editor(&internal.editor),
                window,
            );

            let scroll = buffer_from_editor(&internal.editor).scroll();
            let index = internal
                .scroll_index
                .as_ref()
                .expect("scroll index was refined above");

            let first_line_top = index.offset_of(scroll.line);

            let metrics = ScrollMetrics {
                viewport,
                content: Size::new(index.width, index.content_height()) * (1.0 / hint_factor),
                offset: Vector::new(
                    scroll.horizontal / hint_factor,
                    (first_line_top + scroll.vertical) / hint_factor,
                ),
                // enzUI-ICED #1804: the first visible logical line and where it
                // starts, so a gutter beside the editor can render the visible
                // rows instead of the whole document. Both come from the same
                // scroll index the offset above is derived from, so they cannot
                // disagree with it.
                first_line: scroll.line,
                first_line_top: first_line_top / hint_factor,
            };
            internal.resolved_scroll_metrics = Some(metrics);
            metrics
        })
    }

    fn cached_scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.internal().resolved_scroll_metrics
    }

    fn line_number_rows(
        &self,
        height: f32,
    ) -> Result<
        Vec<crate::core::text::editor::gutter::Row>,
        crate::core::renderer::presentation::Error,
    > {
        let internal = self.internal();
        line_number_rows(
            buffer_from_editor(&internal.editor),
            height * internal.hint_factor,
            internal.hint_factor,
            0,
        )
    }

    fn min_bounds(&self) -> Size {
        let internal = self.internal();

        let (bounds, _has_rtl) = text::measure(buffer_from_editor(&internal.editor));

        bounds * (1.0 / internal.hint_factor)
    }

    fn overwrite(&mut self, new_text: &str) {
        self.with_internal_mut(|internal| {
            let mut font_system = text::font_system().write().expect("Write font system");

            let cursor = internal.editor.cursor();
            let buffer = buffer_mut_from_editor(&mut internal.editor);

            // The configured font, since `update` reapplies it only when it
            // changes.
            buffer.set_text(
                new_text,
                &text::to_attributes(internal.font),
                cosmic_text::Shaping::Advanced,
                text::to_align(internal.alignment),
            );

            let line = cursor.line.min(buffer.lines.len().saturating_sub(1));

            let new_cursor = cosmic_text::Cursor {
                line,
                index: buffer
                    .lines
                    .get(line)
                    .map(|line| line.text().floor_char_boundary(cursor.index))
                    .unwrap_or_default(),
                affinity: if !buffer.lines.is_empty() {
                    cursor.affinity
                } else {
                    cosmic_text::Affinity::Before
                },
            };

            internal.editor.set_cursor(new_cursor);

            // enzICED: the whole document was replaced, so every parser
            // checkpoint, queued edit descriptor, per-line alignment and
            // cached geometry is stale.
            internal.pending_full_reset = true;
            internal.pending_edits = Vec::new();
            internal.scroll_index = None;
            internal.highlighted = None;
            internal.line_alignment_installed = false;
            internal.aligned_lines = buffer_from_editor(&internal.editor).lines.len();

            shape_until_cursor(&mut internal.editor, &mut font_system.raw);
        });
    }

    fn font(&self) -> Font {
        self.internal().font
    }

    fn text_size(&self) -> Pixels {
        let internal = self.internal();

        Pixels(buffer_from_editor(&internal.editor).metrics().font_size / internal.hint_factor)
    }

    fn line_height(&self) -> LineHeight {
        let internal = self.internal();

        LineHeight::Absolute(Pixels(
            buffer_from_editor(&internal.editor).metrics().line_height / internal.hint_factor,
        ))
    }

    fn caret_height(&self) -> Pixels {
        let internal = self.internal();
        let buffer = buffer_from_editor(&internal.editor);
        Pixels(
            caret_row_height(buffer, internal.editor.cursor(), internal.hint_factor)
                .unwrap_or(buffer.metrics().line_height / internal.hint_factor),
        )
    }

    fn hint_factor(&self) -> Option<f32> {
        let internal = self.internal();

        internal.hint.then_some(internal.hint_factor)
    }

    fn update(
        &mut self,
        new_bounds: Size,
        new_font: Font,
        new_size: Pixels,
        new_line_height: LineHeight,
        new_wrapping: Wrapping,
        new_alignment: Alignment,
        new_hint_factor: Option<f32>,
        new_highlighter: &mut impl Parser,
    ) {
        self.with_internal_mut(|internal| {
            // Resizing retains a visible caret without overriding a wheel viewport away from it.
            let keep_caret_visible = new_bounds != internal.bounds
                && caret_point(
                    buffer_from_editor(&internal.editor),
                    internal.editor.cursor(),
                    internal.hint_factor,
                )
                .is_some_and(|point| {
                    let buffer = buffer_from_editor(&internal.editor);
                    let row =
                        caret_row_height(buffer, internal.editor.cursor(), internal.hint_factor)
                            .unwrap_or(buffer.metrics().line_height / internal.hint_factor);
                    point.x >= 0.0
                        && point.x <= internal.bounds.width
                        && point.y >= 0.0
                        && point.y + row <= internal.bounds.height + 0.5
                });
            // Upstream's whole-editor alignment. Per-line alignment
            // (`set_line_alignment`) wins while installed. Otherwise every
            // line takes `new_alignment`, reapplied only when it or the line
            // count changes, never per frame.
            let line_count = buffer_from_editor(&internal.editor).lines.len();

            if !internal.line_alignment_installed
                && (new_alignment != internal.alignment
                    || (new_alignment != Alignment::Default
                        && line_count != internal.aligned_lines))
            {
                let align = text::to_align(new_alignment);
                let buffer = buffer_mut_from_editor(&mut internal.editor);
                let mut changed = false;

                for line in buffer.lines.iter_mut() {
                    changed |= line.set_align(align);
                }

                if changed {
                    buffer.note_encapsulated_line_mutation();
                }

                internal.aligned_lines = line_count;
            }

            internal.alignment = new_alignment;

            let mut font_system = text::font_system().write().expect("Write font system");
            let mut scroll_index_changed = font_system.version() != internal.version
                || new_font != internal.font
                || new_bounds.width != internal.bounds.width;

            let buffer = buffer_mut_from_editor(&mut internal.editor);

            if font_system.version() != internal.version {
                log::trace!("Updating `FontSystem` of `Editor`...");

                for line in buffer.lines.iter_mut() {
                    line.reset();
                }
                // Announced, not discovered (enzUI-ICED #1804).
                buffer.note_encapsulated_line_mutation();

                internal.version = font_system.version();
                internal.topmost_line_changed = Some(0);
                // Whole-document reclassify supersedes any queued edits
                // (enzUI-ICED #2802). Replace the queue rather than
                // `clear()` so the fallback also sheds the backing
                // allocation on this path — a font-system version bump is
                // one of the two callers of the fallback, alongside
                // preflight overflow, and both must produce the same
                // bounded-capacity end state.
                internal.pending_full_reset = true;
                internal.pending_edits = Vec::new();
                instrument::set_pending_edits_capacity(0);
            }

            if new_font != internal.font {
                log::trace!("Updating font of `Editor`...");

                for line in buffer.lines.iter_mut() {
                    let _ = line.set_attrs_list(cosmic_text::AttrsList::new(&text::to_attributes(
                        new_font,
                    )));
                }
                buffer.note_encapsulated_line_mutation();

                internal.font = new_font;
                internal.topmost_line_changed = Some(0);
                // Whole-document reclassify supersedes any queued edits
                // (enzUI-ICED #2802). Replace, not `clear()`, for the
                // same reason as the version-bump site above.
                internal.pending_full_reset = true;
                internal.pending_edits = Vec::new();
                instrument::set_pending_edits_capacity(0);
            }

            let metrics = buffer.metrics();
            let new_line_height = new_line_height.to_absolute(new_size);
            let mut hinting_changed = false;

            let new_hint_factor = text::hint_factor(new_size, new_hint_factor);

            if new_hint_factor != internal.hint.then_some(internal.hint_factor) {
                scroll_index_changed = true;
                internal.hint = new_hint_factor.is_some();
                internal.hint_factor = new_hint_factor.unwrap_or(1.0);

                buffer.set_hinting(if internal.hint {
                    cosmic_text::Hinting::Enabled
                } else {
                    cosmic_text::Hinting::Disabled
                });

                hinting_changed = true;
            }

            if new_size.0 != metrics.font_size
                || new_line_height.0 != metrics.line_height
                || hinting_changed
            {
                scroll_index_changed = true;
                log::trace!("Updating `Metrics` of `Editor`...");

                buffer.set_metrics(cosmic_text::Metrics::new(
                    new_size.0 * internal.hint_factor,
                    new_line_height.0 * internal.hint_factor,
                ));
            }

            let new_wrap = text::to_wrap(new_wrapping);

            if new_wrap != buffer.wrap() {
                scroll_index_changed = true;
                log::trace!("Updating `Wrap` strategy of `Editor`...");

                buffer.set_wrap(new_wrap);
            }

            let grew = new_bounds.height > internal.bounds.height;
            if new_bounds != internal.bounds || hinting_changed {
                log::trace!("Updating size of `Editor`...");

                buffer.set_size(
                    Some(new_bounds.width * internal.hint_factor),
                    Some(new_bounds.height * internal.hint_factor),
                );

                internal.bounds = new_bounds;
            }

            if !(internal.exclusions.is_empty() && buffer.exclusions().is_empty()) {
                let factor = internal.hint_factor;
                let width = internal.bounds.width;
                let exclusions: Vec<_> = internal
                    .exclusions
                    .iter()
                    .map(|e| cosmic_text::Exclusion {
                        x: if e.trailing {
                            width - e.bounds.x - e.bounds.width
                        } else {
                            e.bounds.x
                        } * factor,
                        y: e.bounds.y * factor,
                        width: e.bounds.width * factor,
                        height: e.bounds.height * factor,
                        line: e.line,
                    })
                    .collect();
                if buffer.exclusions() != exclusions.as_slice() {
                    buffer.set_exclusions(&exclusions);
                    scroll_index_changed = true;
                }
            }

            buffer.shape_until_scroll(font_system.raw(), false);

            if scroll_index_changed {
                internal.scroll_index = None;
            }

            // Forward edits to the highlighter (enzUI-ICED #2802). Three
            // arms in order:
            //
            // 1. Reset arm: a queued whole-document reset dominates any
            //    queued descriptors. Without this ordering, a subsequent
            //    edit that arrived before the next `update` would
            //    re-populate `pending_edits`, the descriptor arm would
            //    forward only the tail, and `pending_full_reset` would be
            //    silently dropped, leaving pre-reset checkpoints stale
            //    above the tail edit.
            // 2. Descriptor arm: forward the ordered line-level edits.
            //    An ordinary edit at line 0 sets `topmost_line_changed =
            //    Some(0)` here too; its descriptor must still be forwarded
            //    so the re-keyed suffix stays reusable
            //    (`top_insertion_keeps_the_suffix_seekable`).
            // 3. Partial `change_line` arm: the historical single-line
            //    signal, preserved for callers that only set
            //    `topmost_line_changed` (default `note_edits` still calls
            //    `change_line` via the trait default).
            if internal.pending_full_reset {
                internal.pending_full_reset = false;
                // Reset arm sheds the allocation, mirroring the
                // perform-side fallback (enzUI-ICED #2802). A stale
                // capacity here would defeat the point of the reset,
                // which is to invalidate everything and free the
                // per-frame bridge state.
                internal.pending_edits = Vec::new();
                let _ = internal.topmost_line_changed.take();
                instrument::reset_arm_taken();
                instrument::set_pending_edits_capacity(0);
                new_highlighter.change_line(0);
            } else if !internal.pending_edits.is_empty() {
                new_highlighter.note_edits(&internal.pending_edits);
                // Normal drain: keep the capacity so back-to-back edit
                // bursts don't churn the allocator. The cap on the
                // perform-side preflight bounds this at
                // `MAX_PENDING_EDITS` descriptors.
                internal.pending_edits.clear();
                let _ = internal.topmost_line_changed.take();
                instrument::set_pending_edits_capacity(internal.pending_edits.capacity());
            } else if let Some(topmost_line_changed) = internal.topmost_line_changed.take() {
                log::trace!(
                    "Notifying highlighter of line \
                    change: {topmost_line_changed}"
                );

                new_highlighter.change_line(topmost_line_changed);
            }

            // Where the exclusions landed, for whoever draws what they stand
            // for; anchored ones follow their lines.
            internal.exclusion_bounds.clear();
            if !internal.exclusions.is_empty() {
                let factor = internal.hint_factor;
                let resolved = buffer_mut_from_editor(&mut internal.editor)
                    .resolved_exclusions(font_system.raw());
                internal
                    .exclusion_bounds
                    .extend(resolved.into_iter().map(|e| {
                        e.map(|e| {
                            Rectangle::new(
                                Point::new(e.x / factor, e.y / factor),
                                Size::new(e.width / factor, e.height / factor),
                            )
                        })
                    }));
            }

            internal.editor.shape_as_needed(font_system.raw(), false);
            if keep_caret_visible {
                shape_until_cursor(&mut internal.editor, &mut font_system.raw);
            }
            pin_wrapped_scroll(&mut internal.editor);

            let window = viewport_height(internal.bounds, internal.hint_factor);
            if grew {
                settle_scroll_after_growth(&mut internal.editor, window);
            }

            refine_scroll_index(
                &mut internal.scroll_index,
                buffer_from_editor(&internal.editor),
                window,
            );
            release_offscreen_layouts(internal, window);
        });
        if new_highlighter.has_layout_style() {
            highlight_layout(self, new_font, new_highlighter);
        }
    }

    fn prepare_presentation<H: Parser>(
        &mut self,
        font: Font,
        maximum_height: f32,
        highlighter: &mut H,
        format: impl Fn(H::Output) -> highlighter::Style,
    ) -> Result<(), crate::core::renderer::presentation::Error> {
        use crate::core::renderer::presentation::Error;
        let range = self.snapshot_range(maximum_height)?;
        if !highlighter.line_agnostic() {
            if self
                .internal()
                .highlighted
                .as_ref()
                .is_some_and(|covered| covered.start <= range.start && covered.end >= range.end)
                && highlighter.current_line() >= range.end
            {
                return Ok(());
            }
            if highlighter.current_line() != range.start && !highlighter.seek_line(range.start) {
                return Err(Error::UnsupportedBackend);
            }
        }
        let base_metrics = self.buffer().metrics();
        let hint_factor = self.internal().hint_factor;
        let staged = (|| {
            let mut staged = Vec::with_capacity(range.len());
            let mut spans = 0usize;
            let mut bytes = 0usize;
            for line in &self.buffer().lines[range.clone()] {
                bytes = bytes.checked_add(line.text().len()).ok_or(Error::Budget)?;
                let base = text::to_attributes(font);
                snapshot::attribute_budget(&base, &mut bytes)?;
                let mut attributes = cosmic_text::AttrsList::new(&base);
                for (span, highlight) in highlighter.parse_line(line.text()) {
                    spans += 1;
                    if spans > crate::core::renderer::presentation::MAX_ITEMS {
                        return Err(Error::Budget);
                    }
                    let format = format(highlight);
                    let mut attrs = cosmic_text::Attrs {
                        color_opt: format.color.map(text::to_color),
                        ..text::to_attributes(format.font.unwrap_or(font))
                    };
                    if let Some(metrics) = run_metrics(format.size, base_metrics, hint_factor) {
                        attrs = attrs.metrics(metrics);
                    }
                    if let Some(style) = format.style {
                        attrs.style = text::to_style(style);
                    }
                    if format.underline {
                        attrs = attrs.underline(cosmic_text::UnderlineStyle::Single);
                    }
                    if format.strikethrough {
                        attrs = attrs.strikethrough();
                    }
                    snapshot::attribute_budget(&attrs, &mut bytes)?;
                    attributes.add_span(span, &attrs);
                }
                staged.push(attributes);
            }
            Ok(staged)
        })();
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                highlighter.change_line(range.start);
                return Err(error);
            }
        };
        self.with_internal_mut(|internal| {
            let mut fonts = text::font_system().write().map_err(|_| Error::FontAssets)?;
            let mut mutated = false;
            for (line, attributes) in buffer_mut_from_editor(&mut internal.editor).lines
                [range.clone()]
            .iter_mut()
            .zip(staged)
            {
                mutated |= line.set_attrs_list(attributes);
            }
            if mutated {
                buffer_mut_from_editor(&mut internal.editor).note_encapsulated_line_mutation();
            }
            buffer_mut_from_editor(&mut internal.editor).shape_until_scroll(fonts.raw(), false);
            internal.highlighted = Some(range);
            Ok(())
        })
    }

    fn highlight<H: Parser>(
        &mut self,
        font: Font,
        highlighter: &mut H,
        format_highlight: impl Fn(H::Output) -> highlighter::Style,
    ) {
        let internal = self.internal();
        let buffer = buffer_from_editor(&internal.editor);

        let scroll = buffer.scroll();
        let window = visible_row_window(
            internal.bounds.height * internal.hint_factor,
            buffer.metrics().line_height,
        );

        let last_visible_line = if highlighter.has_layout_style() {
            buffer
                .layout_runs()
                .last()
                .map_or(scroll.line, |run| run.line_i)
        } else {
            last_laid_out_visible_line(
                scroll.line,
                buffer.lines.len(),
                window,
                buffer.lines[scroll.line..]
                    .iter()
                    .map(|line| line.layout_opt().map(Vec::len)),
            )
        };

        // enzUI-ICED #1802. `highlight` runs on EVERY draw, so this decision
        // is on the drag/blink hot path and must stay off it when nothing
        // moved.
        //
        //  * `current_line <= last_visible_line` is the ordinary incremental
        //    case: the loop below re-attributes at most the visible window,
        //    and no seek is wanted or needed.
        //  * `current_line < scroll.line` means the highlighter is BEHIND the
        //    viewport -- the cursor jump (SelectAll, DocumentEnd, go-to-line)
        //    that used to replay every offscreen line on the UI thread. A
        //    stateful parser must still replay to reconstruct its state; an
        //    independent-line highlighter can opt into seeking straight to the
        //    visible window.
        //  * `current_line > last_visible_line` means the highlighter is AHEAD.
        //    That is the steady state after any window was highlighted, but it
        //    is also what a seek leaves behind, so it is not by itself proof
        //    that these lines were ever attributed. `highlighted` records the
        //    window that was, trimmed by the highlighter's own invalidation,
        //    and only an uncovered window pays for a fresh seek.
        let current_line = highlighter.current_line();
        let covered = internal.highlighted.clone().and_then(|covered| {
            let end = covered.end.min(current_line);

            (covered.start < end).then_some(covered.start..end)
        });
        let is_covered = covered
            .as_ref()
            .is_some_and(|covered| covered.start <= scroll.line && covered.end > last_visible_line);

        let current_line = if current_line >= scroll.line && current_line <= last_visible_line {
            current_line
        } else if is_covered {
            return;
        } else if highlighter.seek_line(scroll.line) {
            debug_assert_eq!(
                highlighter.current_line(),
                scroll.line,
                "a highlighter that reports a successful seek must be at that line"
            );

            scroll.line
        } else {
            debug_assert_eq!(
                highlighter.current_line(),
                current_line,
                "a highlighter that refuses to seek must not move"
            );

            current_line
        };

        if current_line > last_visible_line {
            return;
        }

        let editor = self.0.take().expect("Editor should always be initialized");

        let mut internal =
            Arc::try_unwrap(editor).expect("Editor cannot have multiple strong references");

        let mut font_system = text::font_system().write().expect("Write font system");

        let attributes = text::to_attributes(font);
        let base_metrics = buffer_from_editor(&internal.editor).metrics();
        let hint_factor = internal.hint_factor;
        let mut geometry_changed = false;
        let mut mutated = false;

        for line in &mut buffer_mut_from_editor(&mut internal.editor).lines
            [current_line..=last_visible_line]
        {
            geometry_changed |= line
                .attrs_list()
                .spans_iter()
                .any(|(_, attrs)| attrs.metrics_opt.is_some());
            let mut list = cosmic_text::AttrsList::new(&attributes);

            for (range, highlight) in highlighter.parse_line(line.text()) {
                let format = format_highlight(highlight);
                // Font and size changes invalidate cached row geometry.
                geometry_changed |=
                    format.font.is_some() || format.style.is_some() || format.size.is_some();

                if format.color.is_some()
                    || format.font.is_some()
                    || format.style.is_some()
                    || format.size.is_some()
                    || format.underline
                    || format.strikethrough
                {
                    let mut span_attributes = cosmic_text::Attrs {
                        color_opt: format.color.map(text::to_color),
                        ..if let Some(font) = format.font {
                            text::to_attributes(font)
                        } else {
                            attributes.clone()
                        }
                    };

                    if let Some(metrics) = run_metrics(format.size, base_metrics, hint_factor) {
                        span_attributes = span_attributes.metrics(metrics);
                    }
                    if let Some(style) = format.style {
                        span_attributes.style = text::to_style(style);
                    }

                    if format.underline {
                        span_attributes =
                            span_attributes.underline(cosmic_text::UnderlineStyle::Single);
                    }

                    if format.strikethrough {
                        span_attributes = span_attributes.strikethrough();
                    }

                    list.add_span(range, &span_attributes);
                }
            }

            mutated |= line.set_attrs_list(list);
        }

        instrument::highlight_lines_visited(last_visible_line + 1 - current_line);

        // Record what is attributed now, so a later frame that shows the same
        // window does no work. Merge only when the two ranges actually touch;
        // a disjoint window would otherwise claim lines nobody highlighted.
        let visited = current_line..last_visible_line + 1;
        internal.highlighted = Some(match covered {
            Some(covered) if covered.start <= visited.end && visited.start <= covered.end => {
                covered.start.min(visited.start)..covered.end.max(visited.end)
            }
            _ => visited,
        });

        // Only announce a mutation that happened. Attributes are recomputed
        // from the same text and format here, so an unchanged window must not
        // dirty the buffer: `shape_until_scroll` would then run a visible
        // shaping pass on a frame with nothing to shape (#1802).
        if mutated {
            internal.revision = Arc::new(());
            internal.resolved_scroll_metrics = None;
            internal.selection = RwLock::new(None);
            buffer_mut_from_editor(&mut internal.editor).note_encapsulated_line_mutation();
        }

        internal.editor.shape_as_needed(font_system.raw(), false);
        pin_wrapped_scroll(&mut internal.editor);
        if geometry_changed && mutated {
            internal.scroll_index = None;
        }

        self.0 = Some(Arc::new(internal));
    }

    fn highlight_ranges(
        &self,
        ranges: &[(editor::Position, editor::Position)],
    ) -> Vec<Vec<Rectangle>> {
        let internal = self.internal();
        let buffer = buffer_from_editor(&internal.editor);
        ranges
            .iter()
            .map(|(start, end)| {
                if start.line > end.line || (start.line == end.line && start.index >= end.index) {
                    return Vec::new();
                }
                selection_regions(
                    buffer,
                    cosmic_text::Cursor::new(start.line, start.index),
                    cosmic_text::Cursor::new(end.line, end.index),
                    internal.bounds.height * internal.hint_factor,
                    internal.hint_factor,
                )
            })
            .collect()
    }

    fn set_line_alignment(&mut self, alignment: &dyn Fn(usize) -> Alignment) {
        self.with_internal_mut(|internal| {
            let buffer = buffer_mut_from_editor(&mut internal.editor);
            let mut changed = false;
            let mut installed = false;

            for (index, line) in buffer.lines.iter_mut().enumerate() {
                let align = text::to_align(alignment(index));

                installed |= align.is_some();
                changed |= line.set_align(align);
            }

            instrument::alignment_lines_visited(buffer.lines.len());

            if changed {
                // Encapsulated lines are invisible to `resolve_dirty`, so a
                // direct mutation must be announced or the line never
                // reshapes. Only the layout moved: the text and its attributes
                // are untouched, so the highlighter keeps its state and no line
                // change is announced to it.
                buffer.note_encapsulated_line_mutation();
            }

            internal.line_alignment_installed = installed;
        });
    }

    fn exclusion_bounds(&self) -> Vec<Option<Rectangle>> {
        self.internal().exclusion_bounds.clone()
    }

    fn set_exclusions(&mut self, exclusions: &[editor::Exclusion]) {
        if self.internal().exclusions != exclusions {
            self.with_internal_mut(|internal| {
                internal.exclusions.clear();
                internal.exclusions.extend_from_slice(exclusions);
            });
        }
    }

    fn clear_line_alignment(&mut self) {
        if !self.internal().line_alignment_installed {
            return;
        }

        self.with_internal_mut(|internal| {
            let buffer = buffer_mut_from_editor(&mut internal.editor);
            let mut changed = false;

            for line in buffer.lines.iter_mut() {
                changed |= line.set_align(None);
            }

            instrument::alignment_lines_visited(buffer.lines.len());

            if changed {
                buffer.note_encapsulated_line_mutation();
            }

            internal.line_alignment_installed = false;
        });
    }

    fn decorations(&self) -> Vec<editor::Decoration> {
        let internal = self.internal();
        let mut decorations = Vec::new();
        visit_decorations(
            buffer_from_editor(&internal.editor),
            internal.hint_factor,
            |decoration| {
                decorations.push(decoration);
                Ok::<(), std::convert::Infallible>(())
            },
        )
        .unwrap_or_else(|never| match never {});
        decorations
    }
}

fn from_cosmic_color(color: cosmic_text::Color) -> Color {
    let [r, g, b, a] = color.as_rgba();

    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

impl Default for Editor {
    fn default() -> Self {
        Self(Some(Arc::new(Internal::default())))
    }
}

impl PartialEq for Internal {
    fn eq(&self, other: &Self) -> bool {
        self.font == other.font
            && self.bounds == other.bounds
            && buffer_from_editor(&self.editor).metrics()
                == buffer_from_editor(&other.editor).metrics()
    }
}

impl Default for Internal {
    fn default() -> Self {
        Self {
            identity: Arc::new(()),
            revision: Arc::new(()),
            editor: cosmic_text::Editor::new({
                let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics {
                    font_size: 1.0,
                    line_height: 1.0,
                });
                // The same promise as in `with_text` (enzUI-ICED #1804).
                buffer.set_lines_encapsulated(true);
                buffer
            }),
            selection: RwLock::new(None),
            history: History::new(),
            font: Font::default(),
            bounds: Size::ZERO,
            topmost_line_changed: None,
            pending_edits: Vec::new(),
            pending_full_reset: false,
            scroll_index: None,
            resolved_scroll_metrics: None,
            hint: false,
            hint_factor: 1.0,
            version: text::Version::default(),
            line_alignment_installed: false,
            exclusions: Vec::new(),
            exclusion_bounds: Vec::new(),
            alignment: Alignment::Default,
            aligned_lines: 0,
            highlighted: None,
            visible_lines: 0..0,
            exposed_lines: 0,
        }
    }
}

impl fmt::Debug for Internal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Internal")
            .field("font", &self.font)
            .field("bounds", &self.bounds)
            .finish()
    }
}

/// A weak reference to an [`Editor`].
#[derive(Debug, Clone)]
pub struct Weak {
    raw: sync::Weak<Internal>,
    /// The bounds of the [`Editor`].
    pub bounds: Size,
}

impl Weak {
    /// Tries to update the reference into an [`Editor`].
    pub fn upgrade(&self) -> Option<Editor> {
        self.raw.upgrade().map(Some).map(Editor)
    }
}

impl PartialEq for Weak {
    fn eq(&self, other: &Self) -> bool {
        match (self.raw.upgrade(), other.raw.upgrade()) {
            (Some(p1), Some(p2)) => p1 == p2,
            _ => false,
        }
    }
}

fn visit_decorations<E>(
    buffer: &cosmic_text::Buffer,
    hint_factor: f32,
    mut emit: impl FnMut(editor::Decoration) -> Result<(), E>,
) -> Result<(), E> {
    let scroll = buffer.scroll();
    let scale = 1.0 / hint_factor;
    for run in buffer.layout_runs() {
        for span in run.decorations {
            let Some(glyphs) = run.glyphs.get(span.glyph_range.clone()) else {
                continue;
            };

            // An RTL run stores its glyphs right to left, so a span's
            // extent is a min/max, not its first and last glyph.
            let mut left = f32::INFINITY;
            let mut right = f32::NEG_INFINITY;

            for glyph in glyphs {
                left = left.min(glyph.x);
                right = right.max(glyph.x + glyph.w);
            }

            let width = right - left;

            if width.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                continue;
            }

            let x = (left - scroll.horizontal) * scale;
            let width = width * scale;
            let data = &span.data;
            let decoration = &data.text_decoration;
            let run_color = span.color_opt.map(from_cosmic_color);
            let font_size = span.font_size;

            let mut line = |metrics: cosmic_text::DecorationMetrics, color| {
                // Offset and thickness are the font's own, in EM units.
                let thickness = (metrics.thickness * font_size).max(1.0).ceil();
                let y = run.line_y - metrics.offset * font_size;

                emit(editor::Decoration {
                    bounds: Rectangle {
                        x,
                        y: y * scale,
                        width,
                        height: thickness * scale,
                    },
                    color,
                })
            };

            if decoration.underline != cosmic_text::UnderlineStyle::None {
                line(
                    data.underline_metrics,
                    decoration
                        .underline_color_opt
                        .map(from_cosmic_color)
                        .or(run_color),
                )?;
            }

            if decoration.strikethrough {
                line(
                    data.strikethrough_metrics,
                    decoration
                        .strikethrough_color_opt
                        .map(from_cosmic_color)
                        .or(run_color),
                )?;
            }
        }
    }

    Ok(())
}

fn highlight_layout<P: Parser>(editor: &mut Editor, font: Font, parser: &mut P) {
    loop {
        let previous = parser.current_line();
        crate::core::text::Editor::highlight(editor, font, parser, |output| {
            P::layout_style(&output)
        });
        let last_visible = editor
            .buffer()
            .layout_runs()
            .last()
            .map_or(0, |run| run.line_i);
        if parser.current_line() > last_visible || parser.current_line() == previous {
            break;
        }
    }
    // The paint formatter must still resolve live colors for the attributed window.
    parser.change_line(0);
}

fn run_metrics(
    size: Option<Pixels>,
    base: cosmic_text::Metrics,
    hint_factor: f32,
) -> Option<cosmic_text::Metrics> {
    let size = size?.0 * hint_factor;
    if !size.is_finite() || size <= 0.0 {
        return None;
    }
    let height = size * (base.line_height / base.font_size);
    if !height.is_finite() || height <= 0.0 {
        return None;
    }
    Some(cosmic_text::Metrics::new(size, height))
}

fn caret_row_height(
    buffer: &cosmic_text::Buffer,
    cursor: cosmic_text::Cursor,
    hint_factor: f32,
) -> Option<f32> {
    buffer
        .layout_runs()
        .filter(|run| run.line_i == cursor.line)
        .find(|run| run.cursor_position(&cursor).is_some())
        .map(|run| run.line_height / hint_factor)
}

fn caret_point(
    buffer: &cosmic_text::Buffer,
    cursor: cosmic_text::Cursor,
    hint_factor: f32,
) -> Option<Point> {
    buffer
        .layout_runs()
        .filter(|run| run.line_i == cursor.line)
        .find_map(|run| {
            let mut x = run.cursor_position(&cursor)?;
            if run.glyphs.is_empty() {
                x += buffer
                    .lines
                    .get(run.line_i)?
                    .layout_opt()
                    .as_ref()?
                    .first()?
                    .align_offset;
            }
            Some(Point::new(
                (x - buffer.scroll().horizontal) / hint_factor,
                run.line_top / hint_factor,
            ))
        })
}

// Only cached rows intersecting the viewport contribute selection geometry.
fn selection_regions(
    buffer: &cosmic_text::Buffer,
    start: cosmic_text::Cursor,
    end: cosmic_text::Cursor,
    viewport_height: f32,
    hint_factor: f32,
) -> Vec<Rectangle> {
    let line_height = buffer.metrics().line_height;
    let scroll = buffer.scroll();

    let mut regions = Vec::new();
    let mut visited = 0;
    let mut rows_seen = 0;

    let mut y = -scroll.vertical;
    let mut index = scroll.line;

    while index < start.line && y < viewport_height {
        let Some(layout) = buffer
            .lines
            .get(index)
            .and_then(cosmic_text::BufferLine::layout_opt)
        else {
            break;
        };

        visited += 1;
        y += layout
            .iter()
            .map(|row| row.line_height(line_height))
            .sum::<f32>();
        index += 1;
    }

    index = index.max(start.line);

    while index <= end.line && y < viewport_height {
        let Some(line) = buffer.lines.get(index) else {
            break;
        };

        let Some(layout) = line.layout_opt() else {
            // Not laid out for this viewport: it cannot be painted this frame,
            // and every line after it is further away.
            break;
        };

        visited += 1;

        let from = if index == start.line { start.index } else { 0 };
        let to = if index == end.line {
            end.index
        } else {
            line.text().len()
        };
        let variable_metrics = line.attrs_list().defaults().metrics_opt.is_some()
            || line
                .attrs_list()
                .spans_iter()
                .any(|(_, attrs)| attrs.metrics_opt.is_some());
        // Rows that skipped space below an exclusion are not uniform either.
        let first_row =
            if !variable_metrics && y < 0.0 && layout.iter().all(|row| row.top_gap == 0.0) {
                ((-y / line_height).floor() as usize).min(layout.len())
            } else {
                0
            };
        y += first_row as f32 * line_height;
        for row in layout.iter().skip(first_row) {
            if y >= viewport_height {
                break;
            }
            rows_seen += 1;
            let height = row.line_height(line_height);
            if y >= viewport_height {
                break;
            }
            if y + height > 0.0 {
                let (x, width) = row_extent(row, from, to);
                if width > 0.0 {
                    // Below the space a row skipped to clear an exclusion.
                    regions.push(
                        Rectangle {
                            x: x - scroll.horizontal,
                            width,
                            y: y + row.top_gap,
                            height: height - row.top_gap,
                        } * (1.0 / hint_factor),
                    );
                }
            }
            y += height;
        }
        index += 1;
    }

    instrument::logical_lines_visited(visited);
    instrument::visual_rows_shaped(rows_seen);
    instrument::selection_quads(regions.len());

    regions
}

/// The `(x, width)` of the `from..to` byte range on one laid-out row.
fn row_extent(visual_line: &cosmic_text::LayoutLine, from: usize, to: usize) -> (f32, f32) {
    let start = visual_line
        .glyphs
        .first()
        .map(|glyph| glyph.start)
        .unwrap_or(0);
    let end = visual_line
        .glyphs
        .last()
        .map(|glyph| glyph.end)
        .unwrap_or(0);

    let range = start.max(from)..end.min(to);

    // Every extent below is measured from the row's own origin, so a selection
    // rectangle and a background run sit under the glyphs they cover on an
    // aligned row instead of at the column's leading edge.
    let origin = visual_line.align_offset;

    if range.is_empty() {
        (0.0, 0.0)
    } else if range.start == start && range.end == end {
        (origin, visual_line.w)
    } else {
        let first_glyph = visual_line
            .glyphs
            .iter()
            .position(|glyph| range.start <= glyph.start)
            .unwrap_or(0);

        let mut glyphs = visual_line.glyphs.iter();

        let x: f32 = glyphs.by_ref().take(first_glyph).map(|glyph| glyph.w).sum();

        let width: f32 = glyphs
            .take_while(|glyph| range.end > glyph.start)
            .map(|glyph| glyph.w)
            .sum();

        (x + origin, width)
    }
}

fn to_motion(motion: Motion) -> cosmic_text::Motion {
    match motion {
        Motion::Left => cosmic_text::Motion::Left,
        Motion::Right => cosmic_text::Motion::Right,
        Motion::Up => cosmic_text::Motion::Up,
        Motion::Down => cosmic_text::Motion::Down,
        Motion::WordLeft => cosmic_text::Motion::LeftWord,
        Motion::WordRight => cosmic_text::Motion::RightWord,
        Motion::Home => cosmic_text::Motion::Home,
        Motion::End => cosmic_text::Motion::End,
        Motion::PageUp => cosmic_text::Motion::PageUp,
        Motion::PageDown => cosmic_text::Motion::PageDown,
        Motion::DocumentStart => cosmic_text::Motion::BufferStart,
        Motion::DocumentEnd => cosmic_text::Motion::BufferEnd,
    }
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// A deterministic splitmix64 hash. Used only to give a line a *stable*,
/// well-distributed treap priority, so the tree stays balanced without an RNG
/// and two runs over the same fixture build the identical shape.
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// One measured line: a node of the correction treap.
///
/// `delta` is the line's laid-out height *minus the one-row estimate* -- the
/// only thing that has to be stored, since every unmeasured line contributes
/// exactly the estimate. `subtree_delta` caches the sum of `delta` over this
/// node's subtree so a prefix sum and its inverse are both O(log measured).
#[derive(Debug, Clone)]
struct ScrollNode {
    line: usize,
    delta: f64,
    subtree_delta: f64,
    priority: u64,
    left: Option<u32>,
    right: Option<u32>,
}

// Sparse measured corrections preserve O(1) construction and O(log measured lines) seeks.
// Positive measured heights keep offsets invertible when corrections are negative.
#[derive(Debug, Clone)]
struct ScrollIndex {
    /// Measured lines only, as treap nodes keyed by line number. Empty until
    /// the viewport visits a line.
    nodes: Vec<ScrollNode>,
    /// Root of the treap, or `None` while no line has been measured.
    root: Option<u32>,
    /// The number of logical lines the baseline covers.
    line_count: usize,
    /// The height assumed for a line that has not been laid out yet.
    estimate: f32,
    /// The widest laid-out row observed since the index was (re)seeded. Reset
    /// with the index on a text edit so a deleted long line leaves no phantom
    /// horizontal extent.
    width: f32,
}

impl ScrollIndex {
    /// Creates an all-estimate index. O(1): no per-line allocation.
    fn new(line_count: usize, estimate: f32) -> Self {
        let estimate = if estimate.is_finite() && estimate > 0.0 {
            estimate
        } else {
            1.0
        };

        Self {
            nodes: Vec::new(),
            root: None,
            line_count,
            estimate,
            width: 0.0,
        }
    }

    /// The number of measured lines currently stored (a test bound).
    #[cfg(test)]
    fn measured_len(&self) -> usize {
        self.nodes.len()
    }

    fn subtree_delta(&self, node: Option<u32>) -> f64 {
        node.map_or(0.0, |i| self.nodes[i as usize].subtree_delta)
    }

    /// Recomputes a node's cached subtree sum from its children.
    fn pull(&mut self, i: u32) {
        let (left, right, delta) = {
            let n = &self.nodes[i as usize];
            (n.left, n.right, n.delta)
        };
        let sum = delta + self.subtree_delta(left) + self.subtree_delta(right);
        self.nodes[i as usize].subtree_delta = sum;
    }

    fn rotate_right(&mut self, i: u32) -> u32 {
        let l = self.nodes[i as usize]
            .left
            .expect("rotate_right needs a left child");
        self.nodes[i as usize].left = self.nodes[l as usize].right;
        self.nodes[l as usize].right = Some(i);
        self.pull(i);
        self.pull(l);
        l
    }

    fn rotate_left(&mut self, i: u32) -> u32 {
        let r = self.nodes[i as usize]
            .right
            .expect("rotate_left needs a right child");
        self.nodes[i as usize].right = self.nodes[r as usize].left;
        self.nodes[r as usize].left = Some(i);
        self.pull(i);
        self.pull(r);
        r
    }

    /// Inserts `line` with `delta`, or updates it in place, returning the new
    /// subtree root. Sets `changed` when a node was created or its delta moved.
    fn insert(&mut self, node: Option<u32>, line: usize, delta: f64, changed: &mut bool) -> u32 {
        let Some(i) = node else {
            let priority = mix64(line as u64);
            self.nodes.push(ScrollNode {
                line,
                delta,
                subtree_delta: delta,
                priority,
                left: None,
                right: None,
            });
            *changed = true;
            return (self.nodes.len() - 1) as u32;
        };

        let key = self.nodes[i as usize].line;

        if line == key {
            if (self.nodes[i as usize].delta - delta).abs() > f64::EPSILON {
                self.nodes[i as usize].delta = delta;
                *changed = true;
            }
            self.pull(i);
            return i;
        }

        if line < key {
            let left = self.nodes[i as usize].left;
            let new_left = self.insert(left, line, delta, changed);
            self.nodes[i as usize].left = Some(new_left);
            self.pull(i);
            if self.nodes[new_left as usize].priority > self.nodes[i as usize].priority {
                self.rotate_right(i)
            } else {
                i
            }
        } else {
            let right = self.nodes[i as usize].right;
            let new_right = self.insert(right, line, delta, changed);
            self.nodes[i as usize].right = Some(new_right);
            self.pull(i);
            if self.nodes[new_right as usize].priority > self.nodes[i as usize].priority {
                self.rotate_left(i)
            } else {
                i
            }
        }
    }

    /// Records the exact laid-out `height` of `line`, returning whether the
    /// index changed. Inserts a correction node only for a measured line.
    fn observe(&mut self, line: usize, height: f32) -> bool {
        if line >= self.line_count || !height.is_finite() || height <= 0.0 {
            return false;
        }

        // Positive measured heights keep offsets invertible even when span metrics shrink a row.
        let delta = f64::from(height) - f64::from(self.estimate);

        let mut changed = false;
        let root = self.root;
        let new_root = self.insert(root, line, delta, &mut changed);
        self.root = Some(new_root);
        changed
    }

    /// Records the width of a laid-out row. The extent only ever grows while an
    /// index is alive; it is reset with the index on a text edit.
    fn observe_width(&mut self, width: f32) {
        if width.is_finite() {
            self.width = self.width.max(width);
        }
    }

    fn estimate64(&self) -> f64 {
        f64::from(self.estimate)
    }

    /// Sum of the *corrections* of measured lines strictly before `line`.
    fn prefix_delta(&self, line: usize) -> f64 {
        let mut sum = 0.0_f64;
        let mut node = self.root;

        while let Some(i) = node {
            let n = &self.nodes[i as usize];
            if n.line < line {
                sum += self.subtree_delta(n.left) + n.delta;
                node = n.right;
            } else {
                node = n.left;
            }
        }

        sum
    }

    /// The sum of the heights of the lines before `line`: the baseline plus the
    /// corrections of the measured lines that precede it.
    fn offset_of(&self, line: usize) -> f32 {
        let line = line.min(self.line_count);
        (line as f64 * self.estimate64() + self.prefix_delta(line)) as f32
    }

    fn content_height(&self) -> f32 {
        (self.line_count as f64 * self.estimate64() + self.subtree_delta(self.root)) as f32
    }

    /// The line owning absolute offset `y`, and the offset inside it.
    ///
    /// The exact inverse of [`Self::offset_of`]: it finds the largest *measured*
    /// line whose offset is at or before `y` (an O(log measured) treap descent
    /// accumulating subtree corrections), then walks the estimate-only gap after
    /// it arithmetically. So an absolute `ScrollTo` round-trips even when the
    /// destination is thousands of never-measured lines away.
    fn line_at(&self, y: f32) -> (usize, f32) {
        if self.line_count == 0 {
            return (0, 0.0);
        }

        let est = self.estimate64();
        let y = f64::from(y.max(0.0));

        // Rightmost measured key whose start offset is <= y. `acc` is the
        // correction sum of every key strictly left of the current subtree.
        let mut acc = 0.0_f64;
        let mut node = self.root;
        let mut cand_key = 0_usize;
        let mut cand_prefix = 0.0_f64;
        let mut cand_delta = 0.0_f64;

        while let Some(i) = node {
            let n = &self.nodes[i as usize];
            let left_sum = self.subtree_delta(n.left);
            let offset_k = n.line as f64 * est + acc + left_sum;

            if offset_k <= y {
                cand_key = n.line;
                cand_prefix = acc + left_sum;
                cand_delta = n.delta;
                acc = acc + left_sum + n.delta;
                node = n.right;
            } else {
                node = n.left;
            }
        }

        let last = self.line_count - 1;
        let offset_cand = cand_key as f64 * est + cand_prefix;
        // Lines after `cand_key` up to the next measured key are estimate-only,
        // so only `cand_delta` corrects offsets past it.
        let offset_next = (cand_key as f64 + 1.0) * est + cand_prefix + cand_delta;

        if y < offset_next {
            (cand_key.min(last), (y - offset_cand).max(0.0) as f32)
        } else {
            let extra = y - offset_next;
            let steps = (extra / est).floor();
            let line = cand_key + 1 + steps as usize;

            if line >= self.line_count {
                let line_top = last as f64 * est + cand_prefix + cand_delta;
                (last, (y - line_top).max(0.0) as f32)
            } else {
                (line, (extra - steps * est).max(0.0) as f32)
            }
        }
    }
}

/// The viewport height the editor paints into, in buffer (hinted) coordinates.
///
/// An editor that has not been laid out, or one laid out with infinite limits
/// (a `Shrink` editor), has no viewport to bound work by: cosmic-text shapes
/// every line for it anyway, so report an unbounded window rather than paint
/// nothing.
fn viewport_height(bounds: Size, hint_factor: f32) -> f32 {
    let height = bounds.height * hint_factor;

    if height.is_finite() && height > 0.0 {
        height
    } else {
        f32::INFINITY
    }
}

/// Returns the index, creating it (all estimates, no shaping, O(1)) or reseeding
/// it when the line count or the row estimate changed.
///
/// Reseeding is O(1) too -- an empty tree over the new baseline -- so this is
/// safe to call from a per-frame scroll-metrics read. It records no work: an
/// empty index has measured nothing, and `scroll_index_updates` counts only
/// real refinements, so first metrics on a hundred-thousand-line document costs
/// O(visible rows), not O(document).
fn scroll_index<'a>(
    index: &'a mut Option<ScrollIndex>,
    buffer: &cosmic_text::Buffer,
) -> &'a mut ScrollIndex {
    let line_height = buffer.metrics().line_height;
    let lines = buffer.lines.len();

    let needs_new = match index.as_ref() {
        Some(index) => index.line_count != lines || index.estimate != line_height,
        None => true,
    };

    if needs_new {
        *index = Some(ScrollIndex::new(lines, line_height));
    }

    index.as_mut().expect("scroll index was just created")
}

/// Refines the scroll index over the rows the viewport can show, from the
/// layouts cosmic-text has already cached.
///
/// This is the whole point of the index: it is O(visible rows), it never calls
/// `line_layout`, and it stops at the first line the buffer has not laid out --
/// a line we cannot measure is a line we do not claim to know.
/// How many lines may come into view before the layouts of lines far from the
/// viewport are released (enzICED #1).
///
/// cosmic-text keeps every visited line's shaping and layout, and invalidating
/// them keeps their allocations for reuse, so random navigation through a long
/// document grew memory without bound (about 20 KB per visited line). Shaping a
/// line again is cheap next to that, and the scroll index keeps its measured
/// height, so released lines cost nothing until they are shown again.
const RETAINED_EXPOSED_LINES: usize = 1_024;

/// Releases the caches of lines away from the viewport once enough new lines
/// have come into view.
///
/// The viewport, one viewport above and below it, and the cursor and
/// selection anchor lines keep their layouts. The sweep visits every line, so
/// it runs only after [`RETAINED_EXPOSED_LINES`] newly exposed lines, which
/// keeps its cost proportional to the shaping that made it necessary.
fn release_offscreen_layouts(internal: &mut Internal, viewport_height: f32) {
    let buffer = buffer_from_editor(&internal.editor);
    let visible = visible_lines(buffer, viewport_height);
    let previous = &internal.visible_lines;
    let overlap = visible
        .end
        .min(previous.end)
        .saturating_sub(visible.start.max(previous.start));
    internal.exposed_lines += visible.len() - overlap;
    internal.visible_lines = visible.clone();

    if internal.exposed_lines <= RETAINED_EXPOSED_LINES {
        return;
    }

    let margin = visible.len().max(1);
    let kept = visible.start.saturating_sub(margin)..visible.end.saturating_add(margin);
    let cursor = internal.editor.cursor().line;
    let anchor = match internal.editor.selection() {
        cosmic_text::Selection::None => cursor,
        cosmic_text::Selection::Normal(anchor)
        | cosmic_text::Selection::Line(anchor)
        | cosmic_text::Selection::Word(anchor) => anchor.line,
    };

    // Only caches are dropped; text, attributes and geometry are unchanged, so
    // no mutation is announced and the visible frame is not reshaped.
    let buffer = buffer_mut_from_editor(&mut internal.editor);

    for (index, line) in buffer.lines.iter_mut().enumerate() {
        // A line that flows around an exclusion stays: the lines below it
        // find their tops through it.
        if !kept.contains(&index)
            && index != cursor
            && index != anchor
            && !line.flows()
            && line.holds_caches()
        {
            line.release_caches();
        }
    }

    internal.exposed_lines = kept.len().min(buffer.lines.len());
}

/// The logical lines a viewport of `viewport_height` shows from the buffer's
/// scroll position, as far as they are laid out.
fn visible_lines(buffer: &cosmic_text::Buffer, viewport_height: f32) -> Range<usize> {
    let default_line_height = buffer.metrics().line_height;
    let scroll = buffer.scroll();
    let mut y = -scroll.vertical;
    let mut line = scroll.line.min(buffer.lines.len());
    let start = line;

    while line < buffer.lines.len() && y < viewport_height {
        let Some(layout) = buffer.lines[line].layout_opt() else {
            break;
        };

        y += layout
            .iter()
            .map(|row| row.line_height(default_line_height))
            .sum::<f32>();
        line += 1;
    }

    start..line
}

fn refine_scroll_index(
    index: &mut Option<ScrollIndex>,
    buffer: &cosmic_text::Buffer,
    viewport_height: f32,
) {
    let index = scroll_index(index, buffer);
    refine_scroll_index_at(index, buffer, viewport_height, 0);
}

fn refine_scroll_index_at(
    index: &mut ScrollIndex,
    buffer: &cosmic_text::Buffer,
    viewport_height: f32,
    first_line: usize,
) {
    let default_line_height = buffer.metrics().line_height;
    let scroll = buffer.scroll();
    let mut y = -scroll.vertical;
    let mut line = scroll.line;
    let mut visited = 0;
    let mut rows_seen = 0;
    let mut updated = 0;

    while line < buffer.lines.len() && y < viewport_height {
        let Some(layout) = buffer.lines[line].layout_opt() else {
            break;
        };

        visited += 1;
        rows_seen += layout.len();

        let mut height = 0.0;

        for row in layout {
            height += row.line_height(default_line_height);
            index.observe_width(row.w);
        }

        if index.observe(first_line + line, height) {
            updated += 1;
        }

        y += height;
        line += 1;
    }

    instrument::logical_lines_visited(visited);
    instrument::visual_rows_shaped(rows_seen);
    instrument::scroll_index_updates(updated);
}

/// The number of visual rows a viewport of `height` can show when a row is
/// `line_height` tall.
///
/// The exact-multiple case is the one that bites. Viewport heights are commonly
/// derived as `rows * line_height` (a host that rounds an editor to a whole
/// number of rows produces exactly that), and in `f32` dividing that product
/// back by `line_height` can land a hair *above* the integer:
/// `29.0 * (14.0 * 1.3) / (14.0 * 1.3) == 29.000002`. A bare `ceil()` then asks
/// for one row more than the viewport can show -- and, crucially, one row more
/// than [`cosmic_text::Buffer::shape_until_scroll`] laid out, because that
/// accumulates row heights and stops once the total passes the same height.
/// Folding the rounding error away with a relative tolerance keeps the two
/// counts in agreement.
///
/// The tolerance is relative so it scales with the row count, and it is ~200x
/// the worst rounding error `f32` can accumulate here while costing at most a
/// hundredth of a row of highlight window -- a sliver far below one pixel.
fn visible_row_window(height: f32, line_height: f32) -> i32 {
    const TOLERANCE: f32 = 1.0e-4;

    let rows = height / line_height;

    if !rows.is_finite() {
        // An unbounded viewport (a `Shrink` editor laid out with infinite
        // limits) or a degenerate line height: keep the saturating cast that
        // made every shaped line visible.
        return rows as i32;
    }

    (rows - rows * TOLERANCE).ceil().max(1.0) as i32
}

/// The index of the last visible line, given the row count of each line from
/// `scroll_line` onwards and a `window` of visible rows.
///
/// `rows` yields `None` for a line whose layout is not cached. Such a line was
/// not laid out for the current viewport by
/// [`cosmic_text::Buffer::shape_until_scroll`], so it cannot be painted this
/// frame and is therefore not visible: the window ends at the line before it.
/// This used to be an `expect("Line layout should be cached")`, i.e. a `panic!`
/// in a render path, reachable by resizing a window by one pixel with a long
/// file open (enzUI-ICED #1552) whenever [`visible_row_window`] overran the
/// laid-out lines. The arithmetic no longer overruns, but the invariant is
/// cosmic-text's to keep, not ours to assert with the process: a line we cannot
/// measure is a line we do not highlight.
fn last_laid_out_visible_line(
    scroll_line: usize,
    total_lines: usize,
    mut window: i32,
    rows: impl Iterator<Item = Option<usize>>,
) -> usize {
    for (i, line_rows) in rows.enumerate() {
        let Some(line_rows) = line_rows else {
            return scroll_line + i.saturating_sub(1);
        };

        let line_rows = line_rows as i32;

        if window > line_rows {
            window -= line_rows;
        } else {
            return scroll_line + i;
        }
    }

    total_lines.saturating_sub(1)
}

fn buffer_from_editor<'a, 'b>(editor: &'a impl cosmic_text::Edit<'b>) -> &'a cosmic_text::Buffer
where
    'b: 'a,
{
    match editor.buffer_ref() {
        cosmic_text::BufferRef::Owned(buffer) => buffer,
        cosmic_text::BufferRef::Borrowed(buffer) => buffer,
        cosmic_text::BufferRef::Arc(buffer) => buffer,
    }
}

fn buffer_mut_from_editor<'a, 'b>(
    editor: &'a mut impl cosmic_text::Edit<'b>,
) -> &'a mut cosmic_text::Buffer
where
    'b: 'a,
{
    match editor.buffer_ref_mut() {
        cosmic_text::BufferRef::Owned(buffer) => buffer,
        cosmic_text::BufferRef::Borrowed(buffer) => buffer,
        cosmic_text::BufferRef::Arc(_buffer) => unreachable!(),
    }
}

fn shape_until_cursor(
    editor: &mut cosmic_text::Editor<'static>,
    font_system: &mut cosmic_text::FontSystem,
) {
    let cursor = editor.cursor();
    let buffer = buffer_mut_from_editor(editor);

    buffer.shape_until_cursor(font_system, cursor, false);

    if let Some((x, _)) = editor.cursor_position() {
        let buffer = buffer_mut_from_editor(editor);
        let scroll = buffer.scroll();
        let (width, _) = buffer.size();

        const CURSOR_WIDTH: f32 = 2.0; // TODO: Configurable!

        buffer.set_scroll(cosmic_text::Scroll {
            horizontal: scroll.horizontal
                + (x as f32 + CURSOR_WIDTH - scroll.horizontal - width.unwrap_or_default())
                    .clamp(0.0, CURSOR_WIDTH),
            ..scroll
        });
    }

    pin_wrapped_scroll(editor);
}

/// Keeps a wrapping buffer from scrolling sideways (enzICED).
///
/// cosmic-text reveals the caret's glyph horizontally whatever the wrap mode.
/// A space typed at a wrap point stays on its row and hangs past the right
/// edge, so a wrapped paragraph scrolled left by that overhang and stayed
/// there, cutting off the first letters of every row.
/// After the viewport grows, scroll back so no blank space sits below the last
/// line: an auto-growing input that scrolled while it was short would
/// otherwise keep its first lines hidden above a gap.
///
/// Only a fully shaped buffer is measured; one with unshaped lines (a long
/// document) keeps its scroll.
fn settle_scroll_after_growth(editor: &mut cosmic_text::Editor<'static>, window: f32) {
    let buffer = buffer_mut_from_editor(editor);
    let scroll = buffer.scroll();
    if scroll.line == 0 && scroll.vertical <= 0.0 {
        return;
    }
    let line_height = buffer.metrics().line_height;
    let mut heights = Vec::with_capacity(buffer.lines.len());
    for line in &buffer.lines {
        let Some(layout) = line.layout_opt() else {
            return;
        };
        heights.push(if layout.is_empty() {
            line_height
        } else {
            layout.iter().map(|row| row.line_height(line_height)).sum()
        });
    }
    let total: f32 = heights.iter().sum();
    let position: f32 = heights.iter().take(scroll.line).sum::<f32>() + scroll.vertical;
    let furthest = (total - window).max(0.0);
    if position <= furthest {
        return;
    }
    let mut line = 0;
    let mut start = 0.0;
    while line + 1 < heights.len() && start + heights[line] <= furthest {
        start += heights[line];
        line += 1;
    }
    buffer.set_scroll(cosmic_text::Scroll {
        line,
        vertical: furthest - start,
        ..scroll
    });
}

fn pin_wrapped_scroll(editor: &mut cosmic_text::Editor<'static>) {
    let buffer = buffer_mut_from_editor(editor);
    let scroll = buffer.scroll();
    if buffer.wrap() != cosmic_text::Wrap::None && scroll.horizontal != 0.0 {
        buffer.set_scroll(cosmic_text::Scroll {
            horizontal: 0.0,
            ..scroll
        });
    }
}

/// Pulls the cursor and the selection back inside the buffer after a change
/// was replayed onto it.
///
/// `cosmic_text::Edit::apply_change` moves text without touching either (its
/// own source says "TODO: edit cursor if needed?"), so undoing an edit can
/// leave a selection addressing a column that no longer exists. The next
/// edit then splits a line outside a character boundary and cosmic-text
/// *panics* (`assertion failed: self.is_char_boundary(at)` in
/// `BufferLine::split_off`).
///
/// Found by the generated #1805 scenarios, from five ordinary actions:
/// indent, select, undo, then type. A stale selection is not a delta problem,
/// but a crash on the path this issue reports deltas from is one this adapter
/// has to close.
///
/// **Why the guard lives here.** The invariant belongs to whoever moves the
/// text, i.e. `apply_change`, and cosmic-text explicitly does not own it. That
/// code is an unvendored crates.io dependency (`cosmic-text 0.19`), so this
/// adapter -- the one iced layer that calls `apply_change`, immediately after
/// it -- is the lowest layer we can make truthful. If the fix is upstreamed,
/// this becomes a no-op rather than a divergence: it only ever moves a cursor
/// that is already outside the buffer.
fn clamp_cursors(editor: &mut cosmic_text::Editor<'static>) {
    let cursor = clamp_cursor(buffer_from_editor(editor), editor.cursor());

    editor.set_cursor(cursor);

    let selection = match editor.selection() {
        cosmic_text::Selection::None => cosmic_text::Selection::None,
        cosmic_text::Selection::Normal(cursor) => {
            cosmic_text::Selection::Normal(clamp_cursor(buffer_from_editor(editor), cursor))
        }
        cosmic_text::Selection::Line(cursor) => {
            cosmic_text::Selection::Line(clamp_cursor(buffer_from_editor(editor), cursor))
        }
        cosmic_text::Selection::Word(cursor) => {
            cosmic_text::Selection::Word(clamp_cursor(buffer_from_editor(editor), cursor))
        }
    };

    editor.set_selection(selection);
}

/// The nearest position inside `buffer` to `cursor`: an existing line, and a
/// character boundary within it.
fn clamp_cursor(buffer: &cosmic_text::Buffer, cursor: cosmic_text::Cursor) -> cosmic_text::Cursor {
    let line = cursor.line.min(buffer.lines.len().saturating_sub(1));
    let text = buffer.lines[line].text();
    let mut index = cursor.index.min(text.len());

    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }

    cosmic_text::Cursor {
        line,
        index,
        ..cursor
    }
}

fn position_le(left: Position, right: Position) -> bool {
    (left.line, left.index) <= (right.line, right.index)
}

/// Validate the whole batch before changing a byte. This makes malformed,
/// overlapping, or stale plans atomic no-ops instead of partially applying a
/// prefix and then discovering that a later range no longer addresses the
/// document it was planned against.
fn valid_replace_batch(editor: &cosmic_text::Editor<'static>, batch: &ReplaceBatch) -> bool {
    let buffer = buffer_from_editor(editor);
    let mut previous_end = None;

    !batch.replacements.is_empty()
        && batch.replacements.iter().all(|replacement| {
            let start = replacement.range.start;
            let end = replacement.range.end;
            let valid_position = |position: Position| {
                buffer.lines.get(position.line).is_some_and(|line| {
                    position.index <= line.text().len()
                        && line.text().is_char_boundary(position.index)
                })
            };
            let ordered = position_le(start, end)
                && previous_end.is_none_or(|previous| position_le(previous, start));
            previous_end = Some(end);

            valid_position(start) && valid_position(end) && ordered
        })
}

/// Apply a pre-validated batch from the end of the document to the beginning.
/// Later ranges therefore cannot shift the coordinates of earlier ones. All
/// deletes and inserts are captured by the single surrounding
/// `start_change`/`finish_change` pair and become one history entry.
fn apply_replace_batch(editor: &mut cosmic_text::Editor<'static>, batch: &ReplaceBatch) {
    for replacement in batch.replacements.iter().rev() {
        let start = cosmic_text::Cursor {
            line: replacement.range.start.line,
            index: replacement.range.start.index,
            affinity: cosmic_text::Affinity::Before,
        };
        let end = cosmic_text::Cursor {
            line: replacement.range.end.line,
            index: replacement.range.end.index,
            affinity: cosmic_text::Affinity::Before,
        };

        editor.set_selection(cosmic_text::Selection::None);
        editor.set_cursor(start);

        if start != end {
            editor.set_cursor(end);
            editor.set_selection(cosmic_text::Selection::Normal(start));
            let _ = editor.delete_selection();
        }

        if !replacement.text.is_empty() {
            editor.insert_string(&replacement.text, None);
        }
    }

    let cursor = batch.cursor_after;
    editor.set_cursor(cosmic_text::Cursor {
        line: cursor.position.line,
        index: cursor.position.index,
        affinity: cosmic_text::Affinity::Before,
    });
    editor.set_selection(
        cursor
            .selection
            .map_or(cosmic_text::Selection::None, |selection| {
                cosmic_text::Selection::Normal(cosmic_text::Cursor {
                    line: selection.line,
                    index: selection.index,
                    affinity: cosmic_text::Affinity::Before,
                })
            }),
    );
    clamp_cursors(editor);
}

/// Translates a finished `cosmic-text` change into the renderer-neutral
/// [`EditDelta`] the [`editor::Editor`] trait reports.
///
/// Item order is preserved: each item's cursors describe the buffer as the
/// previous item left it, so a caller replaying them onto its own copy of the
/// document lands on the same bytes. The only copying here is the changed text
/// itself -- the delta is `O(changed text)`, never `O(document)`.
fn to_delta(change: &cosmic_text::Change) -> Option<EditDelta> {
    EditDelta::new(change.items.iter().map(|item| EditItem {
        start: to_position(item.start),
        end: to_position(item.end),
        text: item.text.clone(),
        kind: if item.insert {
            EditKind::Insert
        } else {
            EditKind::Delete
        },
    }))
}

fn to_position(cursor: cosmic_text::Cursor) -> Position {
    Position {
        line: cursor.line,
        index: cursor.index,
    }
}

#[derive(Debug, Clone)]
struct EditorState {
    cursor: cosmic_text::Cursor,
    selection: cosmic_text::Selection,
}

impl EditorState {
    fn capture(editor: &cosmic_text::Editor<'static>) -> Self {
        Self {
            cursor: editor.cursor(),
            selection: editor.selection(),
        }
    }

    fn restore(self, editor: &mut cosmic_text::Editor<'static>) {
        editor.set_cursor(self.cursor);
        editor.set_selection(self.selection);
    }
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    change: cosmic_text::Change,
    before: EditorState,
    after: EditorState,
}

/// The undo steps an editor keeps (enzICED #1).
///
/// Every change owns its inserted and deleted text, so an unbounded history
/// retained a document-sized payload for each replace-all or large paste. The
/// oldest steps are forgotten first; the newest step is always kept, however
/// large, so the last edit can always be undone.
const MAX_HISTORY_STEPS: usize = 1_000;

/// The text, in bytes, the undo steps of one editor may retain together.
const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct History {
    changes: Vec<HistoryEntry>,
    current: usize,
    /// The text the retained changes own, in bytes.
    bytes: usize,
}

/// The text a change owns, in bytes.
fn change_bytes(change: &cosmic_text::Change) -> usize {
    change.items.iter().map(|item| item.text.len()).sum()
}

impl History {
    fn new() -> Self {
        Self::default()
    }

    fn undo(&mut self) -> Option<&HistoryEntry> {
        if self.current == 0 {
            return None;
        }

        self.current = self.current.saturating_sub(1);
        self.changes.get(self.current)
    }

    fn redo(&mut self) -> Option<&HistoryEntry> {
        if self.current >= self.changes.len() {
            return None;
        }

        let change = self.changes.get(self.current);
        self.current += 1;
        change
    }

    fn push(&mut self, change: HistoryEntry) {
        for redo in self.changes.drain(self.current..) {
            self.bytes -= change_bytes(&redo.change);
        }
        self.bytes += change_bytes(&change.change);
        if let Some(last) = self.changes.last_mut()
            && extends_typing(&last.change, &change.change)
        {
            let (run, next) = (&mut last.change.items[0], &change.change.items[0]);
            run.text.push_str(&next.text);
            run.end = next.end;
            last.after = change.after;
            return;
        }
        self.changes.push(change);
        self.current += 1;
        self.forget_oldest();
    }

    /// Forgets the oldest steps until the history is within its bounds.
    fn forget_oldest(&mut self) {
        let mut forgotten = 0;

        while self.changes.len() - forgotten > 1
            && (self.changes.len() - forgotten > MAX_HISTORY_STEPS
                || self.bytes > MAX_HISTORY_BYTES)
        {
            self.bytes -= change_bytes(&self.changes[forgotten].change);
            forgotten += 1;
        }

        if forgotten > 0 {
            drop(self.changes.drain(..forgotten));
            self.current -= forgotten;
        }
    }
}

/// Whether `next` continues the typing in `last`, so both undo as one step.
///
/// A run is one inserted character after another, each where the previous
/// ended. It breaks at a line break and where a word ends, so an undo takes
/// back the last word typed; replacing a selection, a paste or a deletion is
/// a step of its own.
fn extends_typing(last: &cosmic_text::Change, next: &cosmic_text::Change) -> bool {
    let ([run], [typed]) = (last.items.as_slice(), next.items.as_slice()) else {
        return false;
    };
    let mut chars = typed.text.chars();
    let (Some(character), None) = (chars.next(), chars.next()) else {
        return false;
    };
    let word_ends = character.is_whitespace()
        && run
            .text
            .chars()
            .next_back()
            .is_some_and(|previous| !previous.is_whitespace());
    run.insert
        && typed.insert
        && character != '\n'
        && !run.text.contains('\n')
        && !word_ends
        && run.end.line == typed.start.line
        && run.end.index == typed.start.index
}

/// Soft cap on `pending_edits` before the overflow fallback fires
/// (enzUI-ICED #2802). 8_192 entries * 24 B per entry ≈ 192 KiB, chosen to
/// absorb any realistic burst between two draws (interactive typing produces
/// one descriptor per action; paste/undo one per contiguous `EditItem`).
const MAX_PENDING_EDITS: usize = 8_192;

/// Distils one `EditDelta`'s items into ordered
/// [`LineEdit`](crate::core::text::LineEdit)
/// descriptors (enzUI-ICED #2802).
///
/// Each item's `text` includes the line endings that the buffer actually
/// stored, so counting `\n` gives the number of new line breaks the item
/// added or removed. The output is appended in the order the items apply,
/// which is the coordinate space each subsequent item was authored in.
fn push_line_edits(delta: &EditDelta, out: &mut Vec<LineEdit>) {
    // Reserve the exact incoming count so `push` inside the loop never
    // grows capacity by doubling (enzUI-ICED #2802). The perform-side
    // preflight has already established `out.len() + items.len() <=
    // MAX_PENDING_EDITS`, so this keeps `out.capacity() <=
    // MAX_PENDING_EDITS` too.
    out.reserve_exact(delta.items().len());
    for item in delta.items() {
        let at = item.start.line;
        let newlines = item.text.matches('\n').count();
        let edit = match item.kind {
            EditKind::Insert => LineEdit {
                at,
                removed: 1,
                inserted: 1 + newlines,
            },
            EditKind::Delete => LineEdit {
                at,
                removed: 1 + newlines,
                inserted: 1,
            },
        };
        out.push(edit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::text::Editor as _;

    fn layout(editor: &mut Editor, width: f32) {
        editor.update(
            Size::new(width, 80.0),
            Font::default(),
            Pixels(16.0),
            LineHeight::Relative(1.25),
            Wrapping::Word,
            Alignment::Default,
            None,
            &mut crate::core::text::parser::PlainText,
        );
    }

    /// Names the given interior ranges of every line, so a decoration can
    /// never cover a whole line by accident.
    struct DecorationHighlighter {
        ranges: Vec<Range<usize>>,
        current_line: usize,
    }

    impl Parser for DecorationHighlighter {
        type Settings = Vec<Range<usize>>;
        type Output = ();
        type Iterator<'a> = std::vec::IntoIter<(Range<usize>, ())>;

        fn new(settings: &Self::Settings) -> Self {
            Self {
                ranges: settings.clone(),
                current_line: 0,
            }
        }

        fn update(&mut self, new_settings: &Self::Settings) {
            self.ranges = new_settings.clone();
            self.current_line = 0;
        }

        fn change_line(&mut self, line: usize) {
            self.current_line = self.current_line.min(line);
        }

        fn parse_line(&mut self, _line: &str) -> Self::Iterator<'_> {
            self.current_line += 1;

            self.ranges
                .iter()
                .map(|range| (range.clone(), ()))
                .collect::<Vec<_>>()
                .into_iter()
        }

        fn current_line(&self) -> usize {
            self.current_line
        }
    }

    fn decorate(editor: &mut Editor, ranges: &[Range<usize>], format: highlighter::Style) {
        let mut highlighter = DecorationHighlighter::new(&ranges.to_vec());

        editor.highlight(Font::default(), &mut highlighter, |()| format);
    }

    fn selection_rectangles(editor: &Editor) -> Vec<Rectangle> {
        match editor.selection() {
            Selection::Range(regions) => regions,
            Selection::Caret(position) => vec![Rectangle::new(position, Size::new(0.0, 0.0))],
        }
    }

    /// Sabotage: drop `row.top_gap` from `selection_regions`; the first
    /// rectangle of the blocked case starts in the skipped space.
    #[test]
    fn the_selection_follows_rows_around_exclusions() {
        // A viewport tall enough for the whole paragraph, so selecting it all
        // does not scroll.
        fn layout(editor: &mut Editor, width: f32) {
            editor.update(
                Size::new(width, 400.0),
                Font::default(),
                Pixels(16.0),
                LineHeight::Relative(1.25),
                Wrapping::Word,
                Alignment::Default,
                None,
                &mut crate::core::text::parser::PlainText,
            );
        }
        let text = "word ".repeat(60);
        let mut editor = Editor::with_text(&text);
        editor.set_exclusions(&[editor::Exclusion::new(Rectangle::new(
            Point::ORIGIN,
            Size::new(120.0, 40.0),
        ))]);
        layout(&mut editor, 400.0);
        let _ = editor.perform(Action::SelectAll);
        let beside = selection_rectangles(&editor);
        for row in &beside {
            if row.y < 40.0 {
                assert!(row.x >= 119.5, "a row beside the exclusion: {beside:?}");
            } else {
                assert!(row.x < 1.0, "a row below it: {beside:?}");
            }
        }
        assert!(beside.iter().any(|row| row.y >= 40.0), "{beside:?}");

        editor.set_exclusions(&[editor::Exclusion::new(Rectangle::new(
            Point::new(10.0, 0.0),
            Size::new(380.0, 30.0),
        ))]);
        layout(&mut editor, 400.0);
        let below = selection_rectangles(&editor);
        assert!(
            below[0].y >= 29.5 && below[0].x < 1.0,
            "a row left no gap starts below the exclusion: {below:?}"
        );

        editor.set_exclusions(&[]);
        layout(&mut editor, 400.0);
        let plain = selection_rectangles(&editor);
        assert!(
            plain[0].y < 1.0 && plain.iter().all(|row| row.x < 1.0),
            "{plain:?}"
        );
        // A trailing exclusion is measured from the right edge.
        editor.set_exclusions(&[editor::Exclusion::new(Rectangle::new(
            Point::ORIGIN,
            Size::new(120.0, 40.0),
        ))
        .from_trailing_edge()]);
        layout(&mut editor, 400.0);
        let trailing = selection_rectangles(&editor);
        assert!(
            trailing[0].x < 1.0 && trailing[0].x + trailing[0].width <= 280.5,
            "{trailing:?}"
        );
        assert_eq!(
            editor.exclusion_bounds()[0].map(|bounds| bounds.x),
            Some(280.0)
        );

        // An anchored exclusion is reported at its line's top.
        let mut editor = Editor::with_text(&format!("Short.\n{text}"));
        editor.set_exclusions(&[editor::Exclusion::anchored(
            1,
            Rectangle::new(Point::new(0.0, 4.0), Size::new(120.0, 40.0)),
        )]);
        layout(&mut editor, 400.0);
        assert_eq!(
            editor.exclusion_bounds(),
            vec![Some(Rectangle::new(
                Point::new(0.0, 24.0),
                Size::new(120.0, 40.0)
            ))],
            "line 1 starts at 20"
        );
    }

    /// Sabotage: drop the `strikethrough` arm of `decorations`; the count
    /// assertion reddens.
    #[test]
    fn underline_and_strikethrough_land_under_and_through_the_highlighted_run() {
        let mut editor = Editor::with_text("plain styled plain");
        layout(&mut editor, 600.0);

        decorate(
            &mut editor,
            std::slice::from_ref(&(6..12)),
            highlighter::Style {
                underline: true,
                strikethrough: true,
                ..highlighter::Style::default()
            },
        );

        let decorations = editor.decorations();
        assert_eq!(decorations.len(), 2, "one underline and one strike-through");

        let underline = decorations[0];
        let strikethrough = decorations[1];

        let line_height = editor.buffer().metrics().line_height;
        let baseline = editor
            .buffer()
            .layout_runs()
            .next()
            .expect("the fixture has one visible run")
            .line_y;
        let text_width = editor.min_bounds().width;

        for decoration in [underline, strikethrough] {
            assert!(
                decoration.color.is_none(),
                "a run that named no ink takes the editor's own"
            );
            assert!(
                decoration.bounds.height >= 1.0,
                "a decoration thinner than a pixel would paint nothing: {decoration:?}"
            );
            assert!(
                decoration.bounds.x > 0.0,
                "the styled run starts after \"plain \": {decoration:?}"
            );
            assert!(
                decoration.bounds.x + decoration.bounds.width < text_width,
                "the styled run ends before \" plain\": {decoration:?}"
            );
            assert!(
                decoration.bounds.y > 0.0
                    && decoration.bounds.y + decoration.bounds.height <= line_height,
                "a decoration belongs to its own line box: {decoration:?}"
            );
        }

        assert!(
            strikethrough.bounds.y < baseline,
            "a strike-through crosses the glyphs"
        );
        assert!(
            underline.bounds.y >= baseline,
            "an underline sits below the baseline"
        );
        assert_eq!(underline.bounds.x, strikethrough.bounds.x);
        assert_eq!(underline.bounds.width, strikethrough.bounds.width);
    }

    #[test]
    fn each_decorated_span_reports_its_own_lines_in_visual_order() {
        let mut editor = Editor::with_text("plain styled plain");
        layout(&mut editor, 600.0);

        decorate(
            &mut editor,
            &[0..5, 6..12],
            highlighter::Style {
                underline: true,
                strikethrough: true,
                ..highlighter::Style::default()
            },
        );

        let decorations = editor.decorations();
        assert_eq!(
            decorations.len(),
            4,
            "two spans, each underlined and struck"
        );

        let [
            first_underline,
            first_strikethrough,
            second_underline,
            second_strikethrough,
        ] = decorations[..]
        else {
            unreachable!("the length is asserted above")
        };

        assert!(first_underline.bounds.y > first_strikethrough.bounds.y);
        assert!(second_underline.bounds.y > second_strikethrough.bounds.y);
        assert_eq!(first_underline.bounds.x, first_strikethrough.bounds.x);
        assert_eq!(second_underline.bounds.x, second_strikethrough.bounds.x);
        assert!(
            first_underline.bounds.x + first_underline.bounds.width <= second_underline.bounds.x,
            "the spans are disjoint and reported left to right: {decorations:?}"
        );
    }

    /// Negative control: a widget painting this list cannot rule undecorated
    /// text.
    #[test]
    fn undecorated_text_lays_out_no_decoration() {
        let mut editor = Editor::with_text("plain styled plain");
        layout(&mut editor, 600.0);

        assert!(editor.decorations().is_empty());

        decorate(
            &mut editor,
            std::slice::from_ref(&(6..12)),
            highlighter::Style::default(),
        );

        assert!(
            editor.decorations().is_empty(),
            "a format that names no decoration must not produce one"
        );
    }

    /// Sabotage: let `decorations` reshape the buffer through
    /// `buffer_mut_from_editor`; the click assertion reddens.
    #[test]
    fn decorations_do_not_move_the_caret_selection_or_hit_testing() {
        let click = Point::new(60.0, 6.0);

        let mut editor = Editor::with_text("plain styled plain");
        layout(&mut editor, 600.0);

        let _ = editor.perform(Action::Click(
            click,
            crate::core::mouse::click::Kind::Single,
        ));
        let clicked = editor.cursor();

        let _ = editor.perform(Action::Select(Motion::Right));
        let selected = editor.cursor();
        let selection = selection_rectangles(&editor);
        assert!(!selection.is_empty(), "the fixture selects something");

        decorate(
            &mut editor,
            std::slice::from_ref(&(6..12)),
            highlighter::Style {
                underline: true,
                strikethrough: true,
                ..highlighter::Style::default()
            },
        );

        assert_eq!(editor.decorations().len(), 2, "the run is decorated");
        assert_eq!(editor.cursor(), selected, "the caret did not move");
        assert_eq!(
            selection_rectangles(&editor),
            selection,
            "the selection geometry did not move"
        );

        let _ = editor.perform(Action::Click(
            click,
            crate::core::mouse::click::Kind::Single,
        ));
        assert_eq!(
            editor.cursor(),
            clicked,
            "the same point must still hit the same byte"
        );
    }

    fn align_lines(editor: &mut Editor, alignment: Alignment, width: f32) {
        editor.set_line_alignment(&|_index| alignment);
        layout(editor, width);
    }

    fn caret_x(editor: &Editor) -> f32 {
        match editor.selection() {
            Selection::Caret(position) => position.x,
            Selection::Range(regions) => regions
                .first()
                .map(|region| region.x)
                .expect("a range selection has a region"),
        }
    }

    #[test]
    fn a_centred_line_lays_its_glyphs_out_from_the_middle_of_the_column() {
        let mut left = Editor::with_text("short line");
        layout(&mut left, 600.0);
        let left_first = first_glyph_x(&left).expect("the fixture has glyphs");

        let mut centred = Editor::with_text("short line");
        align_lines(&mut centred, Alignment::Center, 600.0);
        let centred_first = first_glyph_x(&centred).expect("the fixture has glyphs");

        assert!(
            centred_first > left_first,
            "centring must move the first glyph right: {centred_first} vs {left_first}"
        );

        let mut right = Editor::with_text("short line");
        align_lines(&mut right, Alignment::Right, 600.0);
        let right_first = first_glyph_x(&right).expect("the fixture has glyphs");

        assert!(
            right_first > centred_first,
            "right must move it further than centre: {right_first} vs {centred_first}"
        );
    }

    #[test]
    fn the_caret_of_a_centred_line_follows_its_glyphs() {
        let mut left = Editor::with_text("short line");
        layout(&mut left, 600.0);
        let _ = left.perform(Action::Move(Motion::End));
        let left_caret = caret_x(&left);

        let mut centred = Editor::with_text("short line");
        align_lines(&mut centred, Alignment::Center, 600.0);
        let _ = centred.perform(Action::Move(Motion::End));
        let centred_caret = caret_x(&centred);

        assert!(
            centred_caret > left_caret,
            "the caret must move with the glyphs: {centred_caret} vs {left_caret}"
        );
    }

    #[test]
    fn the_selection_rectangle_of_a_centred_line_sits_under_its_glyphs() {
        let mut centred = Editor::with_text("short line");
        align_lines(&mut centred, Alignment::Center, 600.0);

        let _ = centred.perform(Action::Move(Motion::Home));
        for _ in 0..5 {
            let _ = centred.perform(Action::Select(Motion::Right));
        }

        let regions = match centred.selection() {
            Selection::Range(regions) => regions,
            Selection::Caret(_) => panic!("the fixture selects five characters"),
        };
        let first = regions.first().expect("one region");
        let glyph = first_glyph_x(&centred).expect("the fixture has glyphs");

        assert!(
            (first.x - glyph).abs() < 1.0,
            "the rectangle must start where the glyphs do: {} vs {glyph}",
            first.x
        );
    }

    // An empty row carries no glyph to read a position off, so its caret can
    // only come from the row's own recorded shift.
    #[test]
    fn the_caret_of_an_empty_centred_line_sits_at_the_middle_of_the_column() {
        let mut centred = Editor::with_text("");
        align_lines(&mut centred, Alignment::Center, 600.0);

        let caret = caret_x(&centred);

        assert!(
            caret > 100.0,
            "an empty centred line puts its caret in the column, not at 0: {caret}"
        );
    }

    // The hit test reads absolute glyph positions and the caret is summed from
    // the row's origin: two paths to the same point, which only agree if both
    // account for the shift.
    #[test]
    fn clicking_a_centred_line_puts_the_caret_where_it_was_clicked() {
        let mut centred = Editor::with_text("short line");
        align_lines(&mut centred, Alignment::Center, 600.0);

        let glyph = first_glyph_x(&centred).expect("the fixture has glyphs");
        let click = Point::new(glyph + 1.0, 6.0);
        let _ = centred.perform(Action::Click(
            click,
            crate::core::mouse::click::Kind::Single,
        ));

        let caret = caret_x(&centred);
        assert!(
            (caret - glyph).abs() < 12.0,
            "a click just inside the first glyph must leave the caret there: \
             {caret} vs {glyph}"
        );
    }

    fn first_glyph_x(editor: &Editor) -> Option<f32> {
        let internal = editor.internal();
        let buffer = buffer_from_editor(&internal.editor);

        buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|glyph| glyph.x))
    }

    /// Logical selection metadata must preserve the backend's expanded bounds
    /// without materialising the selected bytes. A full-document selection is
    /// deliberately large enough that accidentally calling `copy()` cannot
    /// hide behind a tiny fixture.
    ///
    /// Sabotage: call `self.copy()` from `selection_range`; the byte counter is
    /// nonzero and this gate reddens.
    #[test]
    fn selection_range_reports_bounds_without_copying_text() {
        let text = (0..20_000)
            .map(|i| format!("line {i:05} café\n"))
            .collect::<String>();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 600.0);
        let _ = editor.perform(Action::SelectAll);

        instrument::reset();
        let range = editor.selection_range().expect("SelectAll has bounds");
        let counters = instrument::counters();

        assert_eq!(range.start, Position { line: 0, index: 0 });
        assert_eq!(
            range.end,
            Position {
                line: 20_000,
                index: 0,
            },
            "the trailing newline opens the final empty editor line"
        );
        assert_eq!(
            counters.bytes_copied, 0,
            "selection metadata must not materialise the selected text: {counters:?}"
        );

        // Positive control: the existing clipboard API still materialises the
        // exact selection and charges every byte.
        let copied = editor.copy().expect("clipboard copy");
        assert_eq!(copied, text);
        assert_eq!(instrument::counters().bytes_copied, text.len() as u64);

        // Expanded selection kinds must report their real semantic bounds,
        // not the unexpanded cursor/anchor pair a caller could already read.
        let _ = editor.perform(Action::Move(Motion::DocumentStart));
        editor.move_to(Cursor {
            position: Position { line: 0, index: 7 },
            selection: None,
        });
        let _ = editor.perform(Action::SelectWord);
        assert_eq!(
            editor.selection_range(),
            Some(Position { line: 0, index: 5 }..Position { line: 0, index: 10 })
        );

        let _ = editor.perform(Action::Move(Motion::DocumentStart));
        editor.move_to(Cursor {
            position: Position { line: 1, index: 3 },
            selection: None,
        });
        let _ = editor.perform(Action::SelectLine);
        assert_eq!(
            editor.selection_range(),
            Some(
                Position { line: 1, index: 0 }..Position {
                    line: 1,
                    index: "line 00001 café".len(),
                }
            )
        );
    }

    /// `(text_size, line_height_relative)` pairs whose product spans the cases
    /// that matter: `14.0 * 1.3` and `13.0 * 1.3` are not representable in
    /// `f32` (18.199999 and 16.900002), `16.0 * 1.25` is exact. The bug this
    /// fixture set exists for is an exact-divisibility condition, so a single
    /// line height could pass by luck.
    const METRICS: [(f32, f32); 3] = [(14.0, 1.3), (13.0, 1.3), (16.0, 1.25)];

    /// A highlighter that records every line it is asked to highlight and never
    /// advances, so one `highlight` call reports the whole visible window.
    #[derive(Default)]
    struct Recorder {
        seen: Vec<String>,
    }

    impl Parser for Recorder {
        type Settings = ();
        type Output = ();
        type Iterator<'a> = std::iter::Empty<(std::ops::Range<usize>, ())>;

        fn new(_settings: &()) -> Self {
            Self::default()
        }

        fn update(&mut self, _new_settings: &()) {}

        fn change_line(&mut self, _line: usize) {}

        fn parse_line(&mut self, line: &str) -> Self::Iterator<'_> {
            self.seen.push(line.to_owned());
            std::iter::empty()
        }

        fn current_line(&self) -> usize {
            0
        }
    }

    /// A highlighter whose lines are independent, like enzUI's editor
    /// tokenizer. It records direct seeks so a cursor jump to EOF cannot pass
    /// by merely returning no highlight spans.
    #[derive(Default)]
    struct IndependentRecorder {
        current_line: usize,
        seen: Vec<String>,
        seeks: Vec<usize>,
    }

    impl Parser for IndependentRecorder {
        type Settings = ();
        type Output = ();
        type Iterator<'a> = std::iter::Empty<(std::ops::Range<usize>, ())>;

        fn new(_settings: &()) -> Self {
            Self::default()
        }

        fn update(&mut self, _new_settings: &()) {}

        fn change_line(&mut self, line: usize) {
            self.current_line = self.current_line.min(line);
        }

        fn parse_line(&mut self, line: &str) -> Self::Iterator<'_> {
            self.seen.push(line.to_owned());
            self.current_line += 1;
            std::iter::empty()
        }

        fn current_line(&self) -> usize {
            self.current_line
        }

        fn seek_line(&mut self, line: usize) -> bool {
            self.current_line = line;
            self.seeks.push(line);
            true
        }
    }

    fn document(lines: usize) -> String {
        (0..lines)
            .map(|i| format!("line {i} of the document"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sized(editor: &mut Editor, height: f32) {
        editor.update(
            Size::new(400.0, height),
            Font::default(),
            Pixels(13.0),
            LineHeight::Relative(1.3),
            Wrapping::Word,
            Alignment::Default,
            None,
            &mut crate::core::text::parser::PlainText,
        );
    }

    /// An input that grows a line at a time keeps its first line in view: typed
    /// while one line tall, the new line scrolls the field; grown to fit both,
    /// it shows both from the top.
    ///
    /// Sabotage: drop the `settle_scroll_after_growth` call in `update`; the
    /// first line stays scrolled out above a blank row.
    #[test]
    fn growing_the_viewport_brings_scrolled_out_lines_back() {
        let row = 17.0;
        let mut editor = Editor::with_text("");
        sized(&mut editor, row);
        for character in "first".chars() {
            let _ = editor.perform(Action::Edit(Edit::Insert(character)));
        }
        let _ = editor.perform(Action::Edit(Edit::Enter));
        sized(&mut editor, row);
        let _ = editor.perform(Action::Edit(Edit::Insert('x')));
        sized(&mut editor, 2.0 * row);
        let metrics = editor.scroll_metrics();
        assert!(
            metrics.content.height <= metrics.viewport.height,
            "{metrics:?}"
        );
        assert_eq!(metrics.offset.y, 0.0, "{metrics:?}");
    }

    /// Lays out `text` in a `height` tall viewport and returns how many lines
    /// `highlight` fed the highlighter, i.e. `last_visible_line + 1`.
    fn highlighted_lines(text: &str, height: f32, size: f32, relative: f32) -> usize {
        let mut editor = Editor::with_text(text);
        let mut highlighter = Recorder::default();

        editor.update(
            Size::new(600.0, height),
            Font::default(),
            Pixels(size),
            LineHeight::Relative(relative),
            Wrapping::Word,
            Alignment::Default,
            None,
            &mut highlighter,
        );

        editor.highlight(Font::default(), &mut highlighter, |_| {
            highlighter::Style::default()
        });

        highlighter.seen.len()
    }

    fn draw_highlight<H: Parser>(editor: &mut Editor, highlighter: &mut H) -> instrument::Counters {
        let ((), counters) = measured(|| {
            editor.highlight(Font::default(), highlighter, |_| {
                highlighter::Style::default()
            });
        });

        counters
    }

    /// A whole-document selection moves the cursor and viewport directly to
    /// EOF. An independent-line highlighter must seek to that viewport instead
    /// of replaying every offscreen line on the UI thread. The latter is the
    /// sustained CPU loop observed after Ctrl+A in the real Text window.
    ///
    /// `highlight` runs on every draw, so the same gate also pins the two
    /// costs a seek must not introduce: a steady frame must feed the
    /// highlighter nothing at all, and a frame that recomputes identical
    /// attributes must not announce a mutation that makes cosmic-text run a
    /// visible shaping pass.
    ///
    /// Sabotage 1: remove the `seek_line(scroll.line)` branch in `highlight`.
    /// The post-Ctrl+A draw then visits nearly all 10,000 lines.
    /// Sabotage 2: drop the `is_covered` early return (seek on every draw).
    /// The steady-state draw then visits a whole viewport instead of zero.
    /// Sabotage 3: restore `mutated = true` in the attribute loop. The
    /// re-attributed unchanged viewport then runs a visible shaping pass.
    #[test]
    fn independent_highlighter_seeks_after_select_all_viewport_jump() {
        let text = document(10_000);
        let mut editor = Editor::with_text(&text);
        let mut highlighter = IndependentRecorder::default();

        editor.update(
            Size::new(600.0, 80.0),
            Font::default(),
            Pixels(16.0),
            LineHeight::Relative(1.25),
            Wrapping::Word,
            Alignment::Default,
            None,
            &mut highlighter,
        );
        let _ = draw_highlight(&mut editor, &mut highlighter);
        highlighter.seen.clear();
        highlighter.seeks.clear();

        let _ = editor.perform(Action::SelectAll);
        let visible_start = editor.buffer().scroll().line;
        assert!(
            visible_start > 9_000,
            "fixture must jump close to EOF, landed at {visible_start}"
        );

        let jumped = draw_highlight(&mut editor, &mut highlighter);

        assert_eq!(
            highlighter.seeks,
            vec![visible_start],
            "the independent highlighter must seek exactly to the new viewport"
        );
        assert!(
            jumped.highlight_lines_visited <= BUDGET,
            "Ctrl+A visited {} lines for a {ROWS}-row viewport: {jumped:?}",
            jumped.highlight_lines_visited
        );
        assert_eq!(
            highlighter.seen.len() as u64,
            jumped.highlight_lines_visited,
            "the counter must charge exactly the lines the highlighter saw"
        );
        assert_eq!(
            highlighter.seen.first().map(String::as_str),
            Some(
                text.lines()
                    .nth(visible_start)
                    .expect("visible line exists")
            ),
            "highlighting must resume at the visible line after the seek"
        );

        // Steady state: nothing scrolled, nothing changed. The seam must not
        // put a viewport of retokenization on every drag/blink frame, so this
        // draw must feed the highlighter no lines and seek nowhere.
        highlighter.seen.clear();
        let seeks_before = highlighter.seeks.len();
        let steady = draw_highlight(&mut editor, &mut highlighter);
        assert_eq!(
            steady.highlight_lines_visited, 0,
            "an unchanged viewport was re-highlighted: {steady:?}"
        );
        assert!(highlighter.seen.is_empty(), "{:?}", highlighter.seen);
        assert_eq!(
            highlighter.seeks.len(),
            seeks_before,
            "a steady frame must not even ask to seek"
        );
        assert_eq!(
            steady.cosmic_visible_shape_passes, 0,
            "a steady frame reached cosmic-text shaping: {steady:?}"
        );

        // A theme/format change invalidates the highlighter (`change_line(0)`
        // is what `text_editor::draw` calls). The same visible window is
        // recomputed -- with identical attributes, because the format did not
        // really change. Work is expected; announcing a buffer mutation that
        // costs a visible shaping pass is not.
        highlighter.change_line(0);
        let reattributed = draw_highlight(&mut editor, &mut highlighter);
        assert!(
            (1..=BUDGET).contains(&reattributed.highlight_lines_visited),
            "an invalidated viewport must be re-attributed, bounded: {reattributed:?}"
        );
        assert_eq!(
            reattributed.cosmic_visible_shape_passes, 0,
            "identical attributes announced a mutation and shaped: {reattributed:?}"
        );
        assert_eq!(
            reattributed.cosmic_layouts_invalidated, 0,
            "identical attributes reset a line layout: {reattributed:?}"
        );

        // The skipped middle was deliberately not cached. A later backwards
        // jump must seek and highlight that viewport too, rather than treating
        // the high current-line cursor as proof it was already processed.
        highlighter.seen.clear();
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::ZERO,
        });
        let back = draw_highlight(&mut editor, &mut highlighter);
        assert_eq!(highlighter.seeks.last(), Some(&0));
        assert!(
            (1..=BUDGET).contains(&back.highlight_lines_visited),
            "a backwards jump must re-highlight its viewport, bounded: {back:?}"
        );
        assert_eq!(
            highlighter.seen.first().map(String::as_str),
            Some("line 0 of the document")
        );
    }

    /// enzUI-ICED #1552: a viewport whose height is an exact multiple of the
    /// line height used to panic with "Line layout should be cached", because
    /// the `ceil()` row window overran the lines `shape_until_scroll` laid out
    /// by one. A user resizing a window by one pixel with a long file open hit
    /// it in a real binary.
    #[test]
    fn highlight_survives_viewport_height_that_is_an_exact_multiple_of_line_height() {
        let text = document(200);

        for (size, relative) in METRICS {
            let line_height = size * relative;

            for rows in [7_usize, 29, 45, 58] {
                let height = rows as f32 * line_height;
                let highlighted = highlighted_lines(&text, height, size, relative);

                assert_eq!(
                    highlighted, rows,
                    "a {height} px viewport ({rows} x {line_height} px rows, \
                     text_size {size}) must highlight exactly its visible rows"
                );
            }
        }
    }

    /// The fix must not have moved the failure by one pixel: a viewport one
    /// pixel either side of the exact multiple still highlights, and reports the
    /// row count the arithmetic promises (`n` rows for `n * lh - 1`, one more
    /// for `n * lh + 1`).
    #[test]
    fn highlight_survives_one_pixel_either_side_of_an_exact_multiple() {
        let text = document(200);

        for (size, relative) in METRICS {
            let line_height = size * relative;

            for rows in [7_usize, 29, 45] {
                let exact = rows as f32 * line_height;

                assert_eq!(
                    highlighted_lines(&text, exact - 1.0, size, relative),
                    rows,
                    "one pixel short of {rows} whole rows still shows {rows} rows \
                     (text_size {size}, line height {line_height})"
                );
                assert_eq!(
                    highlighted_lines(&text, exact + 1.0, size, relative),
                    rows + 1,
                    "one pixel past {rows} whole rows reveals a sliver of the next \
                     row (text_size {size}, line height {line_height})"
                );
            }
        }
    }

    /// The clamp must not truncate the normal case: a document shorter than the
    /// viewport is highlighted to its last line.
    #[test]
    fn highlight_covers_a_document_shorter_than_the_viewport() {
        for (size, relative) in METRICS {
            let line_height = size * relative;

            for lines in [1_usize, 8] {
                let text = document(lines);

                for height in [29.0 * line_height, 29.0 * line_height + 1.0] {
                    assert_eq!(
                        highlighted_lines(&text, height, size, relative),
                        lines,
                        "a {lines} line document in a {height} px viewport must be \
                         highlighted whole (text_size {size})"
                    );
                }
            }
        }
    }

    /// Owns the arithmetic half of the fix (`visible_row_window`), independently
    /// of the defensive branch in `last_laid_out_visible_line`.
    #[test]
    fn visible_row_window_does_not_overrun_an_exact_multiple() {
        for (size, relative) in METRICS {
            let line_height = size * relative;

            for rows in [1_i32, 7, 29, 45, 58, 137] {
                let exact = rows as f32 * line_height;

                assert_eq!(
                    visible_row_window(exact, line_height),
                    rows,
                    "{rows} x {line_height} px rows fit in {exact} px exactly \
                     (text_size {size}); a bare ceil() of the f32 quotient asks \
                     for one row more than the buffer laid out"
                );
                assert_eq!(
                    visible_row_window(exact - 1.0, line_height),
                    rows,
                    "a row shows partially one pixel short of {rows} whole rows"
                );
                assert_eq!(
                    visible_row_window(exact + 1.0, line_height),
                    rows + 1,
                    "a sliver of the next row shows one pixel past {rows} whole rows"
                );
            }

            // An unbounded viewport (a `Shrink` editor laid out with infinite
            // limits) must keep every shaped line visible, and a viewport too
            // short for a single row still shows the line it is scrolled to.
            assert_eq!(visible_row_window(f32::INFINITY, line_height), i32::MAX);
            assert_eq!(visible_row_window(0.0, line_height), 1);
        }
    }

    /// Owns the defensive half of the fix: a line whose layout is not cached
    /// ends the visible window instead of panicking in a render path.
    #[test]
    fn last_laid_out_visible_line_stops_at_the_last_laid_out_line() {
        // The reproduced #1552 state: 29 laid-out lines, a 30 row window
        // (200 line document, 29 x 18.199999 px viewport).
        let laid_out_then_missing = |laid_out: usize, total: usize| {
            std::iter::repeat_n(Some(1), laid_out)
                .chain(std::iter::repeat_n(None, total - laid_out))
        };

        assert_eq!(
            last_laid_out_visible_line(0, 200, 30, laid_out_then_missing(29, 200)),
            28,
            "an overrunning window must end at the last laid-out line, not panic"
        );

        // Same shortfall, scrolled: indices are absolute.
        assert_eq!(
            last_laid_out_visible_line(96, 200, 30, laid_out_then_missing(29, 104)),
            96 + 28
        );

        // Nothing laid out at all: the scrolled-to line is the only answer that
        // stays inside the buffer.
        assert_eq!(
            last_laid_out_visible_line(96, 200, 30, laid_out_then_missing(0, 104)),
            96
        );

        // A window that ends inside the laid-out lines is untouched, including
        // when a logical line wraps into several rows...
        assert_eq!(
            last_laid_out_visible_line(0, 200, 30, laid_out_then_missing(40, 200)),
            29
        );
        assert_eq!(
            last_laid_out_visible_line(0, 200, 30, std::iter::repeat_n(Some(3), 40)),
            9
        );

        // ...and a document shorter than the window ends at its last line.
        assert_eq!(
            last_laid_out_visible_line(0, 8, 30, std::iter::repeat_n(Some(1), 8)),
            7
        );
    }

    /// Typing a wrapped paragraph never scrolls it sideways. A space typed at
    /// a wrap point hangs past the right edge, and cosmic-text revealed it by
    /// scrolling every row left (#3263).
    ///
    /// Sabotage: make `pin_wrapped_scroll` return early and this reddens.
    #[test]
    fn typing_a_wrapped_paragraph_keeps_it_unscrolled_sideways() {
        let mut editor = Editor::with_text("");
        layout(&mut editor, 120.0);

        for character in "Hello there, this is a long line of text that wraps ".chars() {
            let _ = editor.perform(Action::Edit(Edit::Insert(character)));
            layout(&mut editor, 120.0);
            let horizontal = buffer_from_editor(&editor.internal().editor)
                .scroll()
                .horizontal;
            assert_eq!(
                horizontal, 0.0,
                "after typing {character:?} the wrapped text scrolled sideways"
            );
        }
    }

    #[test]
    fn text_editor_scroll_metrics_cache_invalidates_after_typing_and_width_change() {
        let mut editor = Editor::with_text("alpha beta gamma delta");
        layout(&mut editor, 300.0);
        let initial = editor.scroll_metrics();

        let _ = editor.perform(Action::Move(Motion::DocumentEnd));
        for character in " more words typed into the document".repeat(8).chars() {
            let _ = editor.perform(Action::Edit(Edit::Insert(character)));
        }
        layout(&mut editor, 300.0);
        let typed = editor.scroll_metrics();
        assert!(
            typed.content.height > initial.content.height,
            "typing must invalidate cached content extent: initial={initial:?}, typed={typed:?}"
        );

        layout(&mut editor, 90.0);
        let narrow = editor.scroll_metrics();
        assert_eq!(narrow.viewport.width, 90.0);
        assert!(
            narrow.content.height > typed.content.height,
            "changing width must invalidate cached wrapping: typed={typed:?}, narrow={narrow:?}"
        );

        let target = (narrow.content.height - narrow.viewport.height) * 0.6;
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, target),
        });
        let scrolled = editor.scroll_metrics();
        assert!(
            (scrolled.offset.y - target).abs() < 0.01,
            "absolute scroll and therefore the adapter thumb must track refreshed geometry: {scrolled:?}"
        );
    }

    // ---------------------------------------------------------------------
    // enzUI-ICED #1803: viewport-bounded selection paint and scroll metrics.
    // ---------------------------------------------------------------------

    /// The viewport every #1803 fixture is laid out in: 80 px tall, 16 px text
    /// on 1.25 relative line height, i.e. 20 px rows and a four-row window.
    const ROWS: usize = 4;
    const ROW_HEIGHT: f32 = 20.0;

    /// A per-frame work budget. Four visible rows can need one quad each, plus
    /// the row partially scrolled in at either edge; anything beyond that is
    /// work the viewport cannot show.
    const BUDGET: u64 = (ROWS + 2) as u64;

    /// A deterministic multi-line fixture: 100 000 lines, ~2.4 MiB.
    fn huge_document() -> String {
        document(100_000)
    }

    /// A generated 40 MiB many-line document for the ignored release probe.
    fn forty_mib_document() -> String {
        const LINE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
        const BYTES: usize = 40 * 1024 * 1024;

        LINE.repeat(BYTES.div_ceil(LINE.len()))
    }

    /// The same size, but multibyte: byte indices and glyph counts diverge, so a
    /// row-range or byte-range slice that is off by a character panics here
    /// rather than silently painting the wrong run.
    fn huge_multibyte_document(lines: usize) -> String {
        (0..lines)
            .map(|i| format!("ligne {i} — Ünïcødé ✓ 日本語のテキスト"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn measured<T>(f: impl FnOnce() -> T) -> (T, instrument::Counters) {
        let before = instrument::counters();
        let value = f();

        (value, instrument::counters().since(before))
    }

    fn assert_prepared_pointer_does_no_cosmic_discovery(text: &str, label: &str) {
        let mut editor = Editor::with_text(text);
        layout(&mut editor, 300.0);

        for action in [
            Action::Click(
                Point::new(40.0, 10.0),
                crate::core::mouse::click::Kind::Single,
            ),
            Action::Drag(Point::new(120.0, 60.0)),
        ] {
            let action_name = format!("{action:?}");
            let (_, counters) = measured(|| editor.perform(action));

            assert_eq!(
                counters.cosmic_dirty_scan_calls, 0,
                "{label} {action_name} started cosmic dirty discovery: {counters:?}"
            );
            assert_eq!(
                counters.cosmic_dirty_lines_inspected, 0,
                "{label} {action_name} inspected logical lines for dirtiness: {counters:?}"
            );
            assert_eq!(
                counters.cosmic_shape_calls, 0,
                "{label} {action_name} redundantly entered cosmic shaping: {counters:?}"
            );
            assert_eq!(
                counters.cosmic_lines_shaped, 0,
                "{label} {action_name} rebuilt already-visible layout: {counters:?}"
            );
            assert_eq!(
                counters.logical_lines_visited, 0,
                "{label} {action_name} redundantly revisited the scroll index: {counters:?}"
            );
            assert_eq!(
                counters.visual_rows_shaped, 0,
                "{label} {action_name} traversed wrapped rows: {counters:?}"
            );
            assert_eq!(
                counters.scroll_index_updates, 0,
                "{label} {action_name} rewrote unchanged index entries: {counters:?}"
            );
        }

        assert!(
            matches!(editor.selection(), Selection::Range(_)),
            "the prepared drag must still create a selection"
        );
    }

    /// Pointer hit-testing is a read of the prepared viewport, not a request to
    /// rediscover dirty state. The exact zero remains flat across three orders
    /// of magnitude and a single enormous wrapped line.
    #[test]
    fn in_bounds_pointer_actions_skip_cosmic_dirty_discovery_and_reshaping_1809() {
        assert_prepared_pointer_does_no_cosmic_discovery(&document(1_000), "1k lines");
        assert_prepared_pointer_does_no_cosmic_discovery(&huge_document(), "100k lines");
        assert_prepared_pointer_does_no_cosmic_discovery(
            &"wrap this mixed אבג é paragraph across many rows ".repeat(20_000),
            "one huge wrapped line",
        );

        // Outside the prepared rectangle we deliberately retain the old path:
        // repeated drags below the viewport reveal later rows and autoscroll.
        let mut editor = Editor::with_text(&document(200));
        layout(&mut editor, 300.0);
        let _ = editor.perform(Action::Click(
            Point::new(40.0, 10.0),
            crate::core::mouse::click::Kind::Single,
        ));
        let before = editor.scroll_metrics().offset.y;
        let (_, outside) = measured(|| {
            for _ in 0..4 {
                let _ = editor.perform(Action::Drag(Point::new(40.0, 100.0)));
            }
        });
        let after = editor.scroll_metrics().offset.y;

        assert!(
            outside.cosmic_shape_calls > 0,
            "an out-of-bounds drag must retain conservative shaping: {outside:?}"
        );
        assert!(
            after > before,
            "repeated below-viewport drag must autoscroll: {before} -> {after}"
        );
    }

    /// The prepared rectangle is *inset*, and the inset is the part that rots
    /// silently: a caret landing against the right edge still wants
    /// `shape_until_cursor`'s horizontal nudge, and a hit anywhere in a
    /// partially visible bottom row still wants its whole-row reveal. Both far
    /// edges must keep the conservative path, and the interior must still skip
    /// it — without the interior control an always-conservative editor would
    /// pass this test.
    #[test]
    fn pointer_actions_on_the_viewport_edge_keep_the_conservative_path_1809() {
        // `layout` gives the editor 300 x 80 bounds (four 20 px rows).
        let mut editor = Editor::with_text(&document(200));
        layout(&mut editor, 300.0);

        let (_, interior) = measured(|| {
            editor.perform(Action::Click(
                Point::new(150.0, 40.0),
                crate::core::mouse::click::Kind::Single,
            ))
        });

        assert_eq!(
            interior.cosmic_shape_calls, 0,
            "an interior click must still use the prepared layout: {interior:?}"
        );

        for (label, position) in [
            ("right edge", Point::new(299.0, 40.0)),
            ("bottom row", Point::new(150.0, 61.0)),
            ("bottom edge", Point::new(150.0, 79.0)),
            ("bottom-right corner", Point::new(299.0, 79.0)),
        ] {
            let (_, counters) = measured(|| {
                editor.perform(Action::Click(
                    position,
                    crate::core::mouse::click::Kind::Single,
                ))
            });

            assert!(
                counters.cosmic_shape_calls > 0,
                "a click on the {label} must keep the conservative \
                 reveal path: {counters:?}"
            );
        }
    }

    /// The vertical inset is a whole row, and this is the case a two-pixel
    /// margin got wrong: iced sizes the buffer to the exact widget bounds, so
    /// a viewport that is not a whole number of rows ends in a row the user can
    /// only half see. `shape_until_cursor` scrolls that row into view, so a
    /// click on it must NOT take the prepared path — anywhere on it, not just
    /// in its last two pixels.
    #[test]
    fn a_click_on_a_partially_visible_bottom_row_still_reveals_it_1809() {
        // 70 px of viewport at a 20 px row height: rows 1-3 are whole and the
        // fourth is half visible (60.0 ..= 70.0 of an 80 px row band).
        let mut editor = Editor::with_text(&document(200));
        editor.update(
            Size::new(300.0, 70.0),
            Font::default(),
            Pixels(16.0),
            LineHeight::Relative(1.25),
            Wrapping::Word,
            Alignment::Default,
            None,
            &mut crate::core::text::parser::PlainText,
        );

        let (_, whole_row) = measured(|| {
            editor.perform(Action::Click(
                Point::new(150.0, 45.0),
                crate::core::mouse::click::Kind::Single,
            ))
        });
        assert_eq!(
            whole_row.cosmic_shape_calls, 0,
            "a click on the last WHOLE row must still use the prepared \
             layout, or this test would pass on an always-conservative \
             editor: {whole_row:?}"
        );

        // 65.0 is inside the half-visible row and more than two pixels from the
        // bottom edge: the old caret-sized margin called this "prepared".
        let scroll_before = editor.scroll_metrics().offset.y;
        let (_, partial_row) = measured(|| {
            editor.perform(Action::Click(
                Point::new(150.0, 65.0),
                crate::core::mouse::click::Kind::Single,
            ))
        });

        assert!(
            partial_row.cosmic_shape_calls > 0,
            "a click inside the partially visible bottom row must keep the \
             conservative reveal path: {partial_row:?}"
        );
        assert!(
            editor.scroll_metrics().offset.y > scroll_before,
            "the conservative path must actually reveal the half-visible row \
             (offset stayed at {scroll_before})"
        );
    }

    /// enzUI-ICED #1804: the two halves of the encapsulation contract.
    ///
    /// A buffer whose `lines` are public keeps discovering an *offscreen-above*
    /// mutation by scanning, because its global geometry depends on that line
    /// and nobody promised otherwise. A buffer whose owner promised
    /// encapsulation is not scanned at all -- and announcing the same
    /// offscreen-above mutation gets the same redraw, so the promise buys
    /// bounded cost without trading away global correctness.
    #[test]
    fn encapsulated_lines_replace_discovery_without_losing_offscreen_geometry_1804() {
        let mut font_system = text::font_system().write().expect("Write font system");

        let build = |font_system: &mut cosmic_text::FontSystem| {
            let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics::new(16.0, 20.0));
            buffer.set_size(Some(200.0), Some(60.0));
            buffer.set_wrap(cosmic_text::Wrap::Word);
            buffer.set_text(
                &document(60),
                &cosmic_text::Attrs::new(),
                cosmic_text::Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(font_system, false);
            let _ = buffer.line_layout(font_system, 0).expect("line 0 lays out");
            let mut scroll = buffer.scroll();
            scroll.line = 55;
            buffer.set_scroll(scroll);
            buffer.shape_until_scroll(font_system, false);
            buffer.set_redraw(false);
            buffer
        };
        let rewrite_line_0 = |buffer: &mut cosmic_text::Buffer| {
            let long = "word ".repeat(60);
            let changed = buffer.lines[0].set_text(
                &long,
                cosmic_text::LineEnding::Lf,
                cosmic_text::AttrsList::new(&cosmic_text::Attrs::new()),
            );
            assert!(changed, "the fixture must actually rewrite line 0");
        };

        // The public contract: found by searching, and reported.
        let mut public = build(font_system.raw());
        rewrite_line_0(&mut public);
        cosmic_text::instrument::reset();
        public.shape_until_scroll(font_system.raw(), false);
        let scanned = cosmic_text::instrument::counters();
        assert!(
            public.redraw(),
            "a public buffer must still report an offscreen-above mutation"
        );
        assert!(
            scanned.dirty_lines_inspected > 0,
            "the public path must keep discovering it: {scanned:?}"
        );

        // The encapsulated contract: announced, not searched -- and a frame
        // with no mutation at all inspects nothing.
        let mut owned = build(font_system.raw());
        owned.set_lines_encapsulated(true);
        cosmic_text::instrument::reset();
        owned.shape_until_scroll(font_system.raw(), false);
        let quiet = cosmic_text::instrument::counters();
        assert_eq!(
            quiet.dirty_lines_inspected, 0,
            "an encapsulated buffer must not read the document to look for \
             changes it is told about: {quiet:?}"
        );
        assert!(
            !owned.redraw(),
            "nothing changed, so nothing is due: {quiet:?}"
        );

        rewrite_line_0(&mut owned);
        owned.note_encapsulated_line_mutation();
        cosmic_text::instrument::reset();
        owned.shape_until_scroll(font_system.raw(), false);
        let announced = cosmic_text::instrument::counters();
        assert!(
            owned.redraw(),
            "an announced offscreen-above mutation must be reported exactly \
             like a discovered one"
        );
        assert_eq!(
            announced.dirty_lines_inspected, 0,
            "announcing must not fall back to scanning: {announced:?}"
        );
    }

    /// The fast owner contract does not weaken the ordinary cosmic-text API:
    /// a caller that directly invalidates a public line and then uses the
    /// conservative shaping entry point still gets that line rebuilt.
    #[test]
    fn direct_buffer_line_invalidation_is_still_discovered_and_reshaped_1809() {
        let mut font_system = text::font_system().write().expect("Write font system");
        let mut buffer = cosmic_text::Buffer::new_empty(cosmic_text::Metrics::new(16.0, 20.0));
        buffer.set_size(Some(300.0), Some(80.0));
        buffer.set_text(
            "externally invalidated\nsecond line",
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system.raw(), false);
        assert!(buffer.lines[0].layout_opt().is_some());

        buffer.lines[0].reset_shaping();
        assert!(buffer.lines[0].layout_opt().is_none());
        cosmic_text::instrument::reset();
        buffer.shape_until_scroll(font_system.raw(), false);
        let counters = cosmic_text::instrument::counters();

        assert_eq!(counters.dirty_scan_calls, 1, "{counters:?}");
        assert_eq!(
            counters.dirty_lines_inspected, 1,
            "discovery should stop on the externally invalidated first line: {counters:?}"
        );
        assert_eq!(counters.lines_shaped, 1, "{counters:?}");
        assert!(
            buffer.lines[0].layout_opt().is_some(),
            "external invalidation must be rebuilt by the conservative API"
        );

        // A real layout mutation is reported separately from discovery and
        // from cache rebuilds, so metrics/font/wrap regressions are visible.
        cosmic_text::instrument::reset();
        buffer.set_size(Some(240.0), Some(80.0));
        buffer.shape_until_scroll(font_system.raw(), false);
        let relayout = cosmic_text::instrument::counters();
        assert_eq!(relayout.dirty_lines_inspected, 0, "{relayout:?}");
        assert_eq!(relayout.layouts_invalidated, 2, "{relayout:?}");
        assert_eq!(relayout.lines_shaped, 2, "{relayout:?}");
    }

    fn regions(editor: &Editor) -> Vec<Rectangle> {
        match editor.selection() {
            Selection::Range(regions) => regions,
            Selection::Caret(point) => {
                panic!("expected a selection range, got a caret at {point:?}")
            }
        }
    }

    /// Every rectangle a frame paints has to intersect the text bounds; the
    /// widget clips to them, so a quad outside is work that produced no pixel.
    fn assert_inside_viewport(regions: &[Rectangle], what: &str) {
        for region in regions {
            assert!(
                region.y + region.height > -ROW_HEIGHT && region.y < 80.0 + ROW_HEIGHT,
                "{what}: quad {region:?} lies outside the 80 px viewport"
            );
        }
    }

    /// A whole-document selection must cost what the viewport shows, not what
    /// the selection covers -- the O(selection length) walk in `selection()`
    /// this replaces produced one rectangle per selected visual row.
    #[test]
    fn selection_paint_is_bounded_by_the_viewport_not_by_the_selection() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        // One selected line, entirely visible: the reference cost.
        let _ = editor.perform(Action::Click(
            Point::new(10.0, 10.0),
            crate::core::mouse::click::Kind::Single,
        ));
        let _ = editor.perform(Action::Select(Motion::End));
        let (one_line, small) = measured(|| regions(&editor));

        assert_eq!(one_line.len(), 1, "a one-line selection paints one quad");

        // The whole document, tail of the selection in view.
        let _ = editor.perform(Action::SelectAll);
        let (whole, huge) = measured(|| regions(&editor));

        assert_eq!(
            editor.copy().map(|text| text.len()),
            Some(text.len()),
            "bounding the geometry must not bound the selection: copy still \
             yields every selected byte"
        );
        assert!(
            !whole.is_empty(),
            "the visible rows of a whole-document selection must still paint"
        );
        assert_inside_viewport(&whole, "whole-document selection");

        assert!(
            huge.selection_quads <= BUDGET,
            "a 100 000 line selection produced {} quads for a {ROWS} row \
             viewport (one-line selection: {})",
            huge.selection_quads,
            small.selection_quads
        );
        assert!(
            huge.logical_lines_visited <= BUDGET,
            "a 100 000 line selection walked {} logical lines for a {ROWS} row \
             viewport",
            huge.logical_lines_visited
        );

        // ...and it stays flat while the selection grows offscreen: dragging the
        // pointer inside the viewport must not cost more because the anchor is
        // now 100 000 lines away.
        let mut per_drag = Vec::new();

        for step in 0..4 {
            let _ = editor.perform(Action::Drag(Point::new(40.0, 8.0 + step as f32)));
            let (painted, counters) = measured(|| regions(&editor));

            assert_inside_viewport(&painted, "drag with an offscreen anchor");
            per_drag.push(counters);
        }

        for counters in &per_drag {
            assert!(
                counters.selection_quads <= BUDGET && counters.logical_lines_visited <= BUDGET,
                "a pointer drag with an offscreen selection anchor must stay \
                 O(visible rows): {counters:?}"
            );
        }
    }

    /// Scrolling a whole-document selection to the top, the middle and the tail
    /// must each paint the rows in view and nothing else, on both a many-line
    /// and a multibyte document.
    #[test]
    fn selection_paint_stays_bounded_at_the_top_middle_and_tail() {
        for text in [huge_document(), huge_multibyte_document(40_000)] {
            let mut editor = Editor::with_text(&text);
            layout(&mut editor, 300.0);
            let _ = editor.perform(Action::SelectAll);

            let content_height = editor.scroll_metrics().content.height;

            for fraction in [0.0, 0.5, 0.999] {
                let _ = editor.perform(Action::ScrollTo {
                    offset: Vector::new(0.0, content_height * fraction),
                });

                let (painted, counters) = measured(|| regions(&editor));

                assert_inside_viewport(&painted, "scrolled whole-document selection");
                assert!(
                    !painted.is_empty(),
                    "a whole-document selection must paint at {fraction} of the way down"
                );
                assert!(
                    counters.selection_quads <= BUDGET,
                    "{} quads at {fraction} of a whole-document selection",
                    counters.selection_quads
                );
            }
        }
    }

    /// A single logical line wrapped into thousands of rows is the other shape
    /// of the same defect: one line, so bounding *lines* is not enough -- the
    /// row range inside the line has to be bounded too.
    #[test]
    fn selection_paint_bounds_the_rows_of_one_huge_wrapped_line() {
        let text = "wrap this paragraph across a great many visual rows ".repeat(20_000);
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let _ = editor.perform(Action::SelectAll);
        let (painted, counters) = measured(|| regions(&editor));

        assert_eq!(editor.line_count(), 1, "fixture must be one logical line");
        assert!(!painted.is_empty(), "the visible rows must still paint");
        assert_inside_viewport(&painted, "one huge wrapped line");
        assert!(
            counters.selection_quads <= BUDGET,
            "{} quads for a {ROWS} row viewport over one wrapped line",
            counters.selection_quads
        );
    }

    /// Bounding must not move the geometry: a selection that fits keeps its
    /// exact first-row and last-row x positions, its row pitch and its
    /// reversed-drag equivalence.
    #[test]
    fn a_selection_that_fits_keeps_its_exact_geometry() {
        let mut editor = Editor::with_text("alpha beta\ngamma delta\nepsilon zeta");
        layout(&mut editor, 300.0);

        // Forward: head inside line 0, tail inside line 2.
        editor.move_to(Cursor {
            position: Position { line: 2, index: 6 },
            selection: Some(Position { line: 0, index: 6 }),
        });
        let forward = regions(&editor);

        assert_eq!(
            forward.len(),
            3,
            "three selected lines paint three quads: {forward:?}"
        );
        assert!(
            forward[0].x > 0.0,
            "the first row must start at the selection head, not at the margin: {forward:?}"
        );
        assert!(
            forward[1].x == 0.0 && forward[1].width > forward[2].width,
            "a fully selected middle row spans the whole line: {forward:?}"
        );
        assert_eq!(
            forward[2].x, 0.0,
            "the last row starts at the margin: {forward:?}"
        );
        assert!(
            forward[2].width > 0.0 && forward[2].width < forward[1].width,
            "the last row stops at the selection tail: {forward:?}"
        );

        for (i, region) in forward.iter().enumerate() {
            assert!(
                (region.y - i as f32 * ROW_HEIGHT).abs() < 0.01,
                "row {i} of the selection sits at {}, expected {}",
                region.y,
                i as f32 * ROW_HEIGHT
            );
        }

        // Reversed: the same range, dragged upwards.
        editor.move_to(Cursor {
            position: Position { line: 0, index: 6 },
            selection: Some(Position { line: 2, index: 6 }),
        });
        let reversed = regions(&editor);

        assert_eq!(
            reversed, forward,
            "a reversed selection paints the same rectangles"
        );
    }

    /// Clipboard text is a separate path from paint. Selecting a whole document
    /// must copy nothing until something asks for the bytes.
    #[test]
    fn selecting_copies_no_bytes_until_the_clipboard_asks() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let ((), selecting) = measured(|| {
            let _ = editor.perform(Action::SelectAll);
            let _ = regions(&editor);
            let _ = editor.perform(Action::Drag(Point::new(40.0, 8.0)));
            let _ = regions(&editor);
        });

        assert_eq!(
            selecting.bytes_copied, 0,
            "selecting and painting must not materialize the selected text"
        );

        let (copied, asking) = measured(|| editor.copy());

        assert!(asking.bytes_copied > 1_000_000, "{asking:?}");
        assert_eq!(
            copied.map(|text| text.len()),
            Some(asking.bytes_copied as usize)
        );
    }

    /// `with_text` must not lay out the document: the buffer has no bounds yet,
    /// so `shape_until_scroll` there shaped every line at a placeholder font
    /// size the first `update` throws away.
    #[test]
    fn with_text_shapes_nothing_and_update_shapes_only_the_viewport() {
        let text = huge_document();
        let editor = Editor::with_text(&text);

        let shaped = editor
            .buffer()
            .lines
            .iter()
            .filter(|line| line.layout_opt().is_some())
            .count();

        assert_eq!(
            shaped,
            0,
            "`with_text` laid out {shaped} of {} lines before any bounds existed",
            editor.line_count()
        );

        let mut editor = editor;
        layout(&mut editor, 300.0);

        let shaped = editor
            .buffer()
            .lines
            .iter()
            .filter(|line| line.layout_opt().is_some())
            .count();

        assert!(
            shaped > 0,
            "the first layout must shape the visible rows of the document"
        );
        assert!(
            shaped <= ROWS + 2,
            "the first layout shaped {shaped} lines for a {ROWS} row viewport"
        );
    }

    fn history_entry(text: String) -> HistoryEntry {
        let state = EditorState {
            cursor: cosmic_text::Cursor::new(0, 0),
            selection: cosmic_text::Selection::None,
        };

        HistoryEntry {
            change: cosmic_text::Change {
                items: vec![cosmic_text::ChangeItem {
                    start: cosmic_text::Cursor::new(0, 0),
                    end: cosmic_text::Cursor::new(0, text.len()),
                    text,
                    insert: false,
                }],
            },
            before: state.clone(),
            after: state,
        }
    }

    /// Undo keeps the newest [`MAX_HISTORY_STEPS`] steps and the newest of
    /// them undo in order (enzICED #1).
    ///
    /// Sabotage: return early from `History::forget_oldest`; all steps stay.
    #[test]
    fn history_forgets_the_oldest_steps_beyond_its_step_bound() {
        let mut history = History::new();

        for step in 0..MAX_HISTORY_STEPS + 250 {
            history.push(history_entry(format!("step {step}")));
        }

        assert_eq!(history.changes.len(), MAX_HISTORY_STEPS);
        assert_eq!(
            history
                .undo()
                .map(|entry| entry.change.items[0].text.clone()),
            Some(format!("step {}", MAX_HISTORY_STEPS + 249))
        );

        let mut undone = 1;
        let mut oldest = None;
        while let Some(entry) = history.undo() {
            oldest = Some(entry.change.items[0].text.clone());
            undone += 1;
        }

        assert_eq!(undone, MAX_HISTORY_STEPS);
        assert_eq!(oldest, Some("step 250".to_owned()));
        assert!(history.redo().is_some(), "forgetting must keep redo intact");
    }

    /// Large replacements are bounded by bytes, and the newest step stays
    /// undoable even when it alone exceeds the byte bound (enzICED #1).
    ///
    /// Sabotage: drop the byte condition in `History::forget_oldest`.
    #[test]
    fn history_bounds_retained_text_and_keeps_the_newest_step() {
        let mut history = History::new();
        let large = MAX_HISTORY_BYTES / 4 + 1;

        for _ in 0..8 {
            history.push(history_entry("x".repeat(large)));
        }

        let retained = history
            .changes
            .iter()
            .map(|entry| change_bytes(&entry.change))
            .sum::<usize>();
        assert_eq!(retained, history.bytes);
        assert!(
            history.bytes <= MAX_HISTORY_BYTES,
            "{} bytes retained by {} steps",
            history.bytes,
            history.changes.len()
        );
        assert_eq!(history.changes.len(), 3);

        history.push(history_entry("y".repeat(MAX_HISTORY_BYTES + 1)));
        assert_eq!(history.changes.len(), 1);
        assert_eq!(history.bytes, MAX_HISTORY_BYTES + 1);
        assert!(history.undo().is_some());

        // A new step after an undo truncates redo, which releases its bytes.
        history.push(history_entry("z".to_owned()));
        assert_eq!(history.bytes, 1);
        assert_eq!(history.changes.len(), 1);
    }

    /// Visiting distant parts of a long document keeps a bounded number of
    /// line caches: off-screen lines are released, while the viewport and the
    /// caret line keep theirs (enzICED #1).
    ///
    /// Sabotage: return early from `release_offscreen_layouts`; every visited
    /// line keeps its caches and the bound fails.
    #[test]
    fn distant_navigation_keeps_a_bounded_set_of_line_caches() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let holding = |editor: &Editor| {
            editor
                .buffer()
                .lines
                .iter()
                .filter(|line| line.holds_caches())
                .count()
        };
        let mut visited = 0;

        for step in 1..=1_500 {
            let line = step * 487 % 100_000;
            let _ = editor.perform(Action::ScrollToLine {
                line,
                offset: Vector::ZERO,
            });
            layout(&mut editor, 300.0);
            visited += ROWS;

            let buffer = editor.buffer();
            let top = buffer.scroll().line;
            assert!(
                buffer.lines[top].layout_opt().is_some(),
                "step {step}: the first visible line {top} lost its layout"
            );
        }

        let held = holding(&editor);
        // A jump shapes a few lines past the viewport that the exposure count
        // does not see, so the bound is a multiple of the retained count.
        assert!(
            visited > 4 * RETAINED_EXPOSED_LINES,
            "the walk must expose far more lines than are retained"
        );
        assert!(
            held <= 2 * RETAINED_EXPOSED_LINES,
            "{held} of 100 000 lines hold caches after visiting {visited}"
        );
        assert!(
            editor.buffer().lines[0].holds_caches(),
            "the caret line must keep its layout while it is off-screen"
        );
    }

    /// Scroll metrics must not lay out the document either, and must stay
    /// monotonic and bidirectional as unknown lines become known.
    #[test]
    fn scroll_metrics_refine_incrementally_and_stay_monotonic() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let (first, reading) = measured(|| editor.scroll_metrics());

        assert!(
            reading.logical_lines_visited <= BUDGET,
            "reading the scrollbar geometry visited {} logical lines of a \
             100 000 line document",
            reading.logical_lines_visited
        );
        assert!(
            first.content.height > first.viewport.height,
            "a 100 000 line document must overflow its viewport: {first:?}"
        );

        // Down: the offset grows, the content extent never shrinks, and each
        // step costs the viewport.
        let mut previous = first;
        let mut offsets = vec![first.offset.y];

        for _ in 0..12 {
            let (_, step) = measured(|| editor.perform(Action::Scroll { lines: 3 }));
            let metrics = editor.scroll_metrics();

            assert!(
                step.logical_lines_visited <= BUDGET * 3,
                "one wheel scroll visited {} logical lines: {metrics:?}",
                step.logical_lines_visited
            );
            assert!(
                metrics.content.height >= previous.content.height - 0.01,
                "refining unknown lines must not shrink the content extent \
                 (thumb oscillation): {previous:?} -> {metrics:?}"
            );
            assert!(
                metrics.offset.y > previous.offset.y,
                "scrolling down must increase the absolute offset: \
                 {previous:?} -> {metrics:?}"
            );

            offsets.push(metrics.offset.y);
            previous = metrics;
        }

        // Up: the same offsets, in reverse, back to the top.
        for _ in 0..12 {
            let _ = editor.perform(Action::Scroll { lines: -3 });
            let metrics = editor.scroll_metrics();

            assert!(
                metrics.offset.y < previous.offset.y,
                "scrolling up must decrease the absolute offset: \
                 {previous:?} -> {metrics:?}"
            );
            assert!(
                metrics.content.height >= previous.content.height - 0.01,
                "scrolling back up must not shrink the content extent: \
                 {previous:?} -> {metrics:?}"
            );

            previous = metrics;
        }

        assert!(
            previous.offset.y.abs() < 0.01,
            "scrolling back up must land on the top again: {previous:?}"
        );
    }

    #[test]
    fn logical_line_scroll_default_empty_model_is_a_noop() {
        let mut editor = Editor::default();
        assert_eq!(editor.line_count(), 0);
        assert!(
            editor
                .perform(Action::ScrollToLine {
                    line: usize::MAX,
                    offset: Vector::new(f32::MAX, f32::MAX),
                })
                .is_none()
        );
        assert_eq!(editor.line_count(), 0);
        layout(&mut editor, 300.0);
        assert_eq!(editor.scroll_metrics().first_line, 0);
        let mut text = Editor::with_text("");
        layout(&mut text, 300.0);
        assert!(
            text.perform(Action::ScrollToLine {
                line: usize::MAX,
                offset: Vector::new(f32::MAX, f32::MAX),
            })
            .is_none()
        );
        assert_eq!(text.scroll_metrics().first_line, 0);
    }

    #[test]
    fn logical_line_scroll_mixed_wrap_preserves_selection_and_fractional_offset() {
        let text = (0..1200)
            .map(|line| {
                format!(
                    "{line} {}",
                    "wrapped words ".repeat(if line % 3 == 0 { 100 } else { 1 })
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 360.0);
        for target in [900, 400, 1000, 200] {
            editor.move_to(Cursor {
                position: editor::Position {
                    line: 1100,
                    index: 1,
                },
                selection: Some(editor::Position { line: 0, index: 0 }),
            });
            let selection = editor.selection_range();
            for width in [360.0, 180.0, 360.0] {
                let _ = editor.perform(Action::ScrollToLine {
                    line: target,
                    offset: Vector::new(0.0, 12.5),
                });
                layout(&mut editor, width);
                let metrics = editor.scroll_metrics();
                assert_eq!(
                    metrics.first_line, target,
                    "logical target must survive mixed wrapping: {metrics:?}"
                );
                assert!(
                    (metrics.offset.y - metrics.first_line_top - 12.5).abs() < 0.05,
                    "fractional visual offset: {metrics:?}"
                );
                assert_eq!(editor.selection_range(), selection);
            }
        }
    }

    #[test]
    fn logical_line_scroll_chunked_frame_retains_leading_overscan() {
        for target in [900_usize, 400, 1000, 200] {
            let first = target - 20;
            let text = (first..first + 80)
                .map(|line| {
                    format!(
                        "{line} {}",
                        "wrapped words ".repeat(if line % 3 == 0 { 100 } else { 1 })
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut editor = Editor::with_text(&text);
            layout(&mut editor, 360.0);
            let (_, work) = measured(|| {
                editor.perform(Action::ScrollToLine {
                    line: target - first,
                    offset: Vector::new(0.0, 7.25),
                })
            });
            let local = editor.scroll_metrics();
            let global = editor::scrollbar::Projection::new(first, 1200, 80)
                .unwrap()
                .apply(local)
                .unwrap();
            assert_eq!(
                global.first_line, target,
                "chunked restore must retain overscan: {global:?}"
            );
            assert!((global.offset.y - global.first_line_top - 7.25).abs() < 0.05);
            assert!(
                work.logical_lines_visited <= 24,
                "restore visited {} logical lines",
                work.logical_lines_visited
            );
            assert!(
                work.cosmic_lines_shaped <= 24,
                "restore shaped {} paragraphs",
                work.cosmic_lines_shaped
            );
        }
    }

    #[test]
    fn logical_line_scroll_unwrapped_horizontal_offset_is_clamped() {
        let text = (0..100)
            .map(|line| format!("{line} {}", "wide words ".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::with_text(&text);
        editor.update(
            Size::new(300.0, 80.0),
            Font::default(),
            Pixels(16.0),
            LineHeight::Relative(1.25),
            Wrapping::None,
            Alignment::Default,
            None,
            &mut crate::core::text::parser::PlainText,
        );
        let _ = editor.perform(Action::ScrollToLine {
            line: 50,
            offset: Vector::new(55.25, 7.25),
        });
        let metrics = editor.scroll_metrics();
        assert_eq!(metrics.first_line, 50);
        assert!((metrics.offset.x - 55.25).abs() < 0.05);
        assert!((metrics.offset.y - metrics.first_line_top - 7.25).abs() < 0.05);
        let _ = editor.perform(Action::ScrollToLine {
            line: 50,
            offset: Vector::new(f32::MAX, 0.0),
        });
        let metrics = editor.scroll_metrics();
        assert!(metrics.offset.x > 300.0);
        assert!(
            (metrics.offset.x - (metrics.content.width - metrics.viewport.width)).abs() < 0.05,
            "horizontal extent {metrics:?}"
        );
    }

    #[test]
    fn logical_line_scroll_outside_model_clamps_without_shaping_the_document() {
        let mut editor = Editor::with_text(&huge_document());
        layout(&mut editor, 300.0);
        let (_, work) = measured(|| {
            editor.perform(Action::ScrollToLine {
                line: usize::MAX,
                offset: Vector::ZERO,
            })
        });
        let metrics = editor.scroll_metrics();
        assert!(metrics.first_line >= 99_994 && metrics.first_line < 100_000);
        assert!(work.logical_lines_visited <= 24);
        assert!(work.cosmic_lines_shaped <= 24);
        let _ = editor.perform(Action::ScrollToLine {
            line: 99_999,
            offset: Vector::ZERO,
        });
        assert_eq!(metrics.first_line, editor.scroll_metrics().first_line);
    }

    #[test]
    fn logical_line_scroll_invalid_offsets_stay_bounded() {
        let mut editor = Editor::with_text(&huge_document());
        layout(&mut editor, 300.0);
        for offset in [
            Vector::new(f32::NAN, f32::INFINITY),
            Vector::new(-10.0, -1.0),
            Vector::new(0.0, f32::MAX),
        ] {
            let (_, work) = measured(|| {
                editor.perform(Action::ScrollToLine {
                    line: 50_000,
                    offset,
                })
            });
            let metrics = editor.scroll_metrics();
            assert_eq!(metrics.first_line, 50_000);
            assert!(metrics.offset.x.is_finite() && metrics.offset.y.is_finite());
            assert!(metrics.offset.y - metrics.first_line_top < ROW_HEIGHT);
            assert!(work.logical_lines_visited <= 24);
            assert!(work.cosmic_lines_shaped <= 24);
        }
    }

    /// An absolute `ScrollTo` must land where it was asked to, stay there when
    /// repeated, and remain ordered across targets -- the scrollbar drags a
    /// thumb through this.
    #[test]
    fn absolute_scroll_to_lands_and_is_stable_and_ordered() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let max_offset =
            editor.scroll_metrics().content.height - editor.scroll_metrics().viewport.height;
        let mut landed = Vec::new();

        for fraction in [0.0, 0.1, 0.37, 0.5, 0.83, 1.0] {
            let target = max_offset * fraction;

            let ((), step) = measured(|| {
                let _ = editor.perform(Action::ScrollTo {
                    offset: Vector::new(0.0, target),
                });
            });

            let metrics = editor.scroll_metrics();

            assert!(
                step.logical_lines_visited <= BUDGET * 4,
                "an absolute scroll visited {} logical lines of a 100 000 line \
                 document",
                step.logical_lines_visited
            );
            assert!(
                (metrics.offset.y - target).abs() <= ROW_HEIGHT,
                "ScrollTo({target}) landed at {} ({metrics:?})",
                metrics.offset.y
            );

            // Repeating the same absolute request must not move the view.
            let _ = editor.perform(Action::ScrollTo {
                offset: Vector::new(0.0, target),
            });
            let again = editor.scroll_metrics();

            assert!(
                (again.offset.y - metrics.offset.y).abs() < 0.01,
                "a repeated ScrollTo({target}) moved the view from {} to {} \
                 (geometry oscillation)",
                metrics.offset.y,
                again.offset.y
            );

            landed.push(again.offset.y);
        }

        for pair in landed.windows(2) {
            assert!(
                pair[1] > pair[0],
                "absolute scroll targets must stay ordered: {landed:?}"
            );
        }
    }

    /// Measured entries the sparse index is currently storing.
    fn measured_entries(editor: &Editor) -> usize {
        editor
            .0
            .as_ref()
            .expect("editor is initialized")
            .scroll_index
            .as_ref()
            .map_or(0, ScrollIndex::measured_len)
    }

    /// A many-logical-line fixture whose every line wraps into several visual
    /// rows at the 300 px test width, so an offscreen line's one-row estimate
    /// is a genuine *under*-estimate that only becomes exact once visited --
    /// the case a single huge line (exact after one layout) cannot exercise.
    fn wrapped_document(lines: usize) -> String {
        (0..lines)
            .map(|i| {
                format!(
                    "paragraph {i}: {}",
                    "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The whole point of the sparse index: constructing it and reading the
    /// first scroll metrics must cost the *viewport*, not the document. Two
    /// documents that differ only in length must record the identical, bounded
    /// number of stored corrections and index updates.
    #[test]
    fn first_scroll_metrics_is_independent_of_document_length() {
        let mut costs = Vec::new();

        for lines in [2_000_usize, 100_000] {
            instrument::reset();

            let text = document(lines);
            let mut editor = Editor::with_text(&text);
            layout(&mut editor, 300.0);
            let _ = editor.scroll_metrics();

            let counters = instrument::counters();
            let entries = measured_entries(&editor);

            assert!(
                counters.scroll_index_updates <= BUDGET,
                "first metrics of a {lines}-line document wrote {} sparse \
                 corrections (must be O(visible rows), not O(document))",
                counters.scroll_index_updates
            );
            assert!(
                entries as u64 <= BUDGET,
                "the index of a {lines}-line document stored {entries} entries \
                 (must be O(visible rows), not O(document))"
            );

            costs.push((counters.scroll_index_updates, entries));
        }

        assert_eq!(
            costs[0], costs[1],
            "first-metrics index cost must not grow with document length: \
             2 000-line {:?} vs 100 000-line {:?}",
            costs[0], costs[1]
        );
    }

    /// A pointer drag that grows a selection across the whole document, and the
    /// scroll steps that chase it, must each touch a bounded number of sparse
    /// index entries -- the counter the earlier dense/Fenwick index blew.
    #[test]
    fn pointer_and_scroll_work_touch_a_bounded_number_of_index_entries() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);
        let _ = editor.perform(Action::SelectAll);

        let before = measured_entries(&editor);

        // Selecting the whole 100 000-line document must not have populated the
        // index with more than the windows it actually shaped (top and, via
        // `BufferEnd`, the tail): the store is O(visited rows), not O(document).
        assert!(
            before as u64 <= BUDGET * 4,
            "select-all on a 100 000-line document stored {before} sparse index \
             entries (must be O(visited rows), not O(document))"
        );

        for _ in 0..8 {
            let (_, step) = measured(|| editor.perform(Action::Scroll { lines: 3 }));

            assert!(
                step.scroll_index_updates <= BUDGET * 3,
                "one wheel scroll wrote {} sparse corrections",
                step.scroll_index_updates
            );
        }

        // Storage grows only by the rows the moves actually revealed, never by
        // the offscreen tail of the selection.
        let grew = measured_entries(&editor) - before;
        assert!(
            grew as u64 <= BUDGET * 8,
            "eight scrolls added {grew} index entries (should be O(revealed rows))"
        );
    }

    /// Many offscreen wrapped lines that refine from one-row estimates to
    /// multi-row heights must not make a drag to the bottom land short of EOF,
    /// while an ordinary in-document offset stays put. This is the end-anchor
    /// the issue asked for, on the fixture that actually exercises it.
    #[test]
    fn wrapped_refinement_keeps_a_bottom_drag_pinned_to_end() {
        let text = wrapped_document(5_000);
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let first = editor.scroll_metrics();
        assert!(
            first.content.height > first.viewport.height,
            "the fixture must overflow its viewport: {first:?}"
        );

        // The single drag that end anchoring exists for: ask for the bottom the
        // *current* metrics report, while the tail is still one-row estimates.
        // Inside this one `ScrollTo` the tail refines from 1 row to several and
        // the content grows, so a literal absolute offset would now stop far
        // above EOF. End anchoring re-pins to the recomputed maximum, so a
        // single drag lands on EOF -- the view is scrolled as far as it goes
        // and the last line sits at the viewport bottom.
        let max0 = first.content.height - first.viewport.height;
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, max0),
        });

        let landed = editor.scroll_metrics();
        assert!(
            landed.content.height > first.content.height + ROW_HEIGHT,
            "the fixture must actually refine the tail upward for this to test \
             anything: {} -> {}",
            first.content.height,
            landed.content.height
        );
        let gap = landed.content.height - (landed.offset.y + landed.viewport.height);
        assert!(
            gap.abs() <= ROW_HEIGHT,
            "a single drag to the bottom of a refining wrapped document landed \
             {gap} px from EOF (end anchoring lost): {landed:?}"
        );

        // Dragging to the recomputed bottom again converges and stays put; the
        // content extent never shrinks under the refinements.
        let mut previous_content = landed.content.height;
        let mut previous_offset = landed.offset.y;

        for _ in 0..5 {
            let metrics = editor.scroll_metrics();
            let max = metrics.content.height - metrics.viewport.height;

            let _ = editor.perform(Action::ScrollTo {
                offset: Vector::new(0.0, max),
            });

            let after = editor.scroll_metrics();
            assert!(
                after.content.height >= previous_content - 0.01,
                "refining the tail shrank the content extent: {previous_content} \
                 -> {} ({after:?})",
                after.content.height
            );
            previous_offset = after.offset.y;
            previous_content = after.content.height;
        }

        let settled = editor.scroll_metrics();
        let settled_gap = settled.content.height - (settled.offset.y + settled.viewport.height);
        assert!(
            settled_gap.abs() <= ROW_HEIGHT,
            "repeated bottom drags drifted from EOF: {settled_gap} px ({settled:?})"
        );
        assert!(
            (settled.offset.y - previous_offset).abs() <= ROW_HEIGHT,
            "a repeated bottom drag moved the view: {} -> {}",
            previous_offset,
            settled.offset.y
        );

        // An ordinary in-document offset is NOT treated as an end anchor: it
        // stays exactly where asked and well clear of EOF.
        let mid_target = editor.scroll_metrics().content.height * 0.4;
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, mid_target),
        });
        let mid_a = editor.scroll_metrics();
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, mid_target),
        });
        let mid_b = editor.scroll_metrics();

        assert!(
            (mid_a.offset.y - mid_b.offset.y).abs() < 0.5,
            "a repeated mid-document ScrollTo oscillated: {} -> {}",
            mid_a.offset.y,
            mid_b.offset.y
        );
        assert!(
            mid_b.offset.y + mid_b.viewport.height < mid_b.content.height - ROW_HEIGHT,
            "a mid-document offset must not be pinned to EOF: {mid_b:?}"
        );
    }

    /// Deleting the widest line must not leave a phantom horizontal extent: the
    /// sparse index is reseeded on a text edit, so the reported content width
    /// tracks what is actually present.
    #[test]
    fn deleting_the_widest_line_drops_the_phantom_horizontal_extent() {
        // A single wide logical line, so the edit that empties it does NOT
        // change the line count -- the phantom can only be cleared by reseeding
        // the index on the edit itself, not by the add/remove-line path.
        let text = "x".repeat(4_000);
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let wide = editor.scroll_metrics().content.width;
        assert!(
            wide > 100.0,
            "the wide line must have a real extent: {wide}"
        );

        // Empty the line. The line count is unchanged (one line before and
        // after), so nothing but the edit-reseed can retire the old width.
        let _ = editor.perform(Action::SelectAll);
        let _ = editor.perform(Action::Edit(Edit::Delete));
        assert_eq!(
            editor.line_count(),
            1,
            "the edit must not change line count"
        );

        let narrow = editor.scroll_metrics().content.width;

        assert!(
            narrow < wide - 50.0,
            "emptying the widest line left a phantom horizontal extent: \
             {wide} -> {narrow}"
        );
    }

    /// The index itself: estimates first, refinements only ever growing, and
    /// `line_at` the exact inverse of `offset_of`.
    #[test]
    fn scroll_index_is_an_invertible_monotonic_estimate() {
        let mut index = ScrollIndex::new(1_000, 20.0);

        assert_eq!(index.content_height(), 20_000.0, "one row per unknown line");
        assert_eq!(index.offset_of(0), 0.0);
        assert_eq!(index.offset_of(500), 10_000.0);

        assert!(index.observe(3, 60.0), "a refinement changes the index");
        assert!(
            !index.observe(3, 60.0),
            "re-observing the same height does not"
        );
        assert_eq!(index.content_height(), 20_040.0);
        assert_eq!(index.offset_of(4), 3.0 * 20.0 + 60.0);

        // `line_at` is the inverse of `offset_of`, including inside a refined
        // line and past the end of the document.
        for line in [0_usize, 3, 4, 17, 999] {
            let offset = index.offset_of(line);

            assert_eq!(
                index.line_at(offset),
                (line, 0.0),
                "line {line} at absolute {offset}"
            );
        }

        assert_eq!(index.line_at(index.offset_of(3) + 25.0), (3, 25.0));
        assert_eq!(index.line_at(-5.0), (0, 0.0));

        let (last, vertical) = index.line_at(index.content_height() + 1_000.0);
        assert_eq!(last, 999, "past the end stays on the last line");
        assert!(vertical > 0.0);

        // Refinement is monotonic: visiting a line can only replace a one-row
        // estimate with one row or more, so the extent never shrinks and the
        // widths only grow.
        let mut index = ScrollIndex::new(1_000, 20.0);
        let mut height = index.content_height();

        for line in 0..1_000 {
            let _ = index.observe(line, 20.0 + (line % 3) as f32 * 20.0);
            index.observe_width(line as f32);

            let refined = index.content_height();

            assert!(
                refined >= height - 0.01,
                "refining line {line} shrank the content extent"
            );

            height = refined;
        }

        assert_eq!(index.width, 999.0);
    }

    /// Resident set size of this process, in MiB.
    fn rss_mib() -> f64 {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let pages: f64 = statm
            .split_whitespace()
            .nth(1)
            .and_then(|field| field.parse().ok())
            .unwrap_or(0.0);

        pages * 4096.0 / (1024.0 * 1024.0)
    }

    fn percentile(samples: &mut [f64], p: f64) -> f64 {
        samples.sort_by(f64::total_cmp);

        let index = ((samples.len() as f64 - 1.0) * p).round() as usize;

        samples[index]
    }

    /// A release-profile measurement of the two paths #1803 bounds, not a gate.
    ///
    /// ```sh
    /// cargo test -p iced_graphics --release --lib \
    ///     text::editor::tests::report_pointer_and_scroll_cost -- --ignored --nocapture
    /// ```
    ///
    /// Run it against this implementation and against the pre-#1803 behaviour
    /// (selection walked from the selection head over every line, `with_text`
    /// shaping the whole document, scroll metrics laying out every line) for the
    /// before/after pair. The deterministic counters in the gates above are what
    /// *fails*; this only says how much wall clock and RSS the bound is worth.
    #[test]
    #[ignore = "measurement, not a gate: run with --release --nocapture"]
    fn report_pointer_and_scroll_cost() {
        const FRAMES: usize = 200;

        /// Drag frames over a whole-document selection: `perform` and the paint
        /// geometry `selection()` builds, timed apart.
        fn drag_arm(text: &str) -> (Vec<f64>, Vec<f64>, instrument::Counters) {
            let mut editor = Editor::with_text(text);
            layout(&mut editor, 300.0);

            instrument::reset();
            let _ = editor.perform(Action::SelectAll);

            let mut performs = Vec::with_capacity(FRAMES);
            let mut paints = Vec::with_capacity(FRAMES);

            for frame in 0..FRAMES {
                let started = std::time::Instant::now();
                let _ = editor.perform(Action::Drag(Point::new(40.0, 8.0 + (frame % 60) as f32)));
                performs.push(started.elapsed().as_secs_f64() * 1.0e6);

                let started = std::time::Instant::now();
                let _ = editor.selection();
                paints.push(started.elapsed().as_secs_f64() * 1.0e6);
            }

            (performs, paints, instrument::counters())
        }

        let text = huge_document();
        let rss_before = rss_mib();

        let started = std::time::Instant::now();
        let mut editor = Editor::with_text(&text);
        let construction = started.elapsed();

        let started = std::time::Instant::now();
        layout(&mut editor, 300.0);
        let first_layout = started.elapsed();

        // Scrolling down from the top, so every frame reveals unshaped rows and
        // reads the scrollbar geometry -- the overlay scrollbar's real cost.
        instrument::reset();
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, 0.0),
        });

        let mut scrolls = Vec::with_capacity(FRAMES);

        for _ in 0..FRAMES {
            let started = std::time::Instant::now();
            let _ = editor.perform(Action::Scroll { lines: 3 });
            let _ = editor.scroll_metrics();
            scrolls.push(started.elapsed().as_secs_f64() * 1.0e6);
        }

        let scrolling = instrument::counters();

        let (mut huge_performs, mut huge_paints, pointer) = drag_arm(&text);
        let (mut small_performs, mut small_paints, _) = drag_arm(&document(1_000));
        let forty_mib = forty_mib_document();
        let (mut forty_performs, _, forty_pointer) = drag_arm(&forty_mib);
        let wrapped = "wrap this mixed אבג é paragraph across many rows ".repeat(20_000);
        let (mut wrapped_performs, _, wrapped_pointer) = drag_arm(&wrapped);
        let rss_after = rss_mib();

        println!(
            "#1803/#1809 measurement -- {} lines, {:.2} MiB of text, 80 px (4 row) viewport\n\
             with_text:              {construction:?}\n\
             first layout:           {first_layout:?}\n\
             scroll+metrics frame:   p50 {:.1} us  p95 {:.1} us  ({scrolling:?})\n\
             drag perform (2.5 MiB): p50 {:.1} us  p95 {:.1} us\n\
             drag paint   (2.5 MiB): p50 {:.1} us  p95 {:.1} us  ({pointer:?})\n\
             drag perform (1k line): p50 {:.1} us  p95 {:.1} us\n\
             drag paint   (1k line): p50 {:.1} us  p95 {:.1} us\n\
             drag perform (40 MiB):  p50 {:.1} us  p95 {:.1} us  ({forty_pointer:?})\n\
             drag perform (wrapped): p50 {:.1} us  p95 {:.1} us  ({wrapped_pointer:?})\n\
             RSS:                    {rss_before:.1} MiB -> {rss_after:.1} MiB",
            editor.line_count(),
            text.len() as f64 / (1024.0 * 1024.0),
            percentile(&mut scrolls, 0.5),
            percentile(&mut scrolls, 0.95),
            percentile(&mut huge_performs, 0.5),
            percentile(&mut huge_performs, 0.95),
            percentile(&mut huge_paints, 0.5),
            percentile(&mut huge_paints, 0.95),
            percentile(&mut small_performs, 0.5),
            percentile(&mut small_performs, 0.95),
            percentile(&mut small_paints, 0.5),
            percentile(&mut small_paints, 0.95),
            percentile(&mut forty_performs, 0.5),
            percentile(&mut forty_performs, 0.95),
            percentile(&mut wrapped_performs, 0.5),
            percentile(&mut wrapped_performs, 0.95),
        );
    }

    // ---------------------------------------------------------------------
    // enzUI-ICED #1805: exact edit deltas out of `perform`.
    // ---------------------------------------------------------------------

    /// The text an [`Editor`] holds, joined exactly the way
    /// `iced_widget::text_editor::Content::text` joins it (each line with its
    /// own ending, an interior `None` ending falling back to the default).
    /// This is the O(document) flattening #1805 exists to make unnecessary; the
    /// tests use it as the independent oracle the deltas are compared against.
    fn flatten(editor: &Editor) -> String {
        let lines = editor.line_count();
        let mut contents = String::new();

        for index in 0..lines {
            let line = editor.line(index).expect("line index is in range");

            contents.push_str(&line.text);

            if index + 1 < lines {
                contents.push_str(if line.ending == editor::LineEnding::None {
                    editor::LineEnding::default().as_str()
                } else {
                    line.ending.as_str()
                });
            }
        }

        contents
    }

    /// A caller's own copy of the document, maintained *only* from the deltas
    /// `perform` returns -- i.e. exactly what an app that cannot afford
    /// `flatten` per keystroke would keep.
    struct Mirror {
        text: String,
        deltas: usize,
        inserted: usize,
        deleted: usize,
        bytes: usize,
    }

    impl Mirror {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_owned(),
                deltas: 0,
                inserted: 0,
                deleted: 0,
                bytes: 0,
            }
        }

        /// [`Mirror::perform`] where the returned delta is not what the caller
        /// is asserting on -- the byte identity check inside still runs.
        fn step(&mut self, editor: &mut Editor, action: Action) {
            let _ = self.perform(editor, action);
        }

        /// Performs `action`, replays whatever delta came back onto the mirror,
        /// and proves the mirror is byte-identical to the editor afterwards.
        fn perform(&mut self, editor: &mut Editor, action: Action) -> Option<EditDelta> {
            let before = instrument::counters();
            let delta = editor.perform(action.clone());
            let counted = instrument::counters().since(before);

            assert_eq!(
                counted.delta_bytes_returned as usize,
                delta.as_ref().map_or(0, EditDelta::bytes),
                "the instrumented delta size must be the delta actually \
                 returned by {action:?}"
            );
            assert_eq!(
                counted.delta_items_returned as usize,
                delta.as_ref().map_or(0, |delta| delta.items().len()),
                "the instrumented item count must match {action:?}"
            );

            if !action.is_edit() {
                assert!(
                    delta.is_none(),
                    "{action:?} changes no text, so it must return no delta: \
                     {delta:?}"
                );
            }

            if let Some(delta) = &delta {
                self.deltas += 1;
                self.bytes += delta.bytes();

                for item in delta.items() {
                    match item.kind {
                        EditKind::Insert => self.inserted += 1,
                        EditKind::Delete => self.deleted += 1,
                    }
                }

                assert!(
                    delta.apply(&mut self.text),
                    "the delta of {action:?} does not address the document it \
                     was produced from: {delta:?}"
                );
            }

            let editor_text = flatten(editor);

            assert_eq!(
                self.text, editor_text,
                "after {action:?} the delta-maintained copy diverged from the \
                 editor (delta: {delta:?})"
            );

            delta
        }
    }

    /// A deterministic 64-bit stream: the scenarios have to be reproducible
    /// from their seed, or a failure cannot be re-run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // SplitMix64.
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        fn pick<'a, T>(&mut self, options: &'a [T]) -> &'a T {
            &options[self.below(options.len())]
        }
    }

    /// Characters whose bytes, chars and graphemes all disagree: ASCII, a
    /// two-byte letter, a combining mark that extends the *previous* grapheme,
    /// a CJK ideograph and an astral-plane emoji.
    const CHARS: [char; 10] = ['a', 'Z', ' ', '\t', 'é', '\u{0301}', '日', '✓', '🌍', '\n'];

    /// The pastes the generated scenarios use. They are LF-normalized on
    /// purpose: an [`EditItem`] addresses a line and a column, and a flattened
    /// document that puts an LF line straight before a CR one does not have a
    /// unique line structure (see [`EditDelta::apply`]). Exactness over every
    /// line ending is proven separately, on fixtures that stay unambiguous, by
    /// `pasting_line_endings_reports_the_exact_bytes`.
    const LF_PASTES: [&str; 5] = [
        "plain",
        "lf\nlf",
        "several\nlines\nat\nonce",
        "e\u{0301}\u{0301} \u{1F469}\u{200D}\u{1F467} \u{1F1E6}\u{1F1F7}",
        "\ttabbed\n\tlines\n",
    ];

    /// Pastes that carry every line ending cosmic-text knows about, plus
    /// multi-codepoint graphemes.
    const PASTES: [&str; 8] = [
        "plain",
        "lf\nlf",
        "crlf\r\ncrlf",
        "cr\rcr",
        "lfcr\n\rlfcr",
        "mixed\nx\r\ny\rz",
        "e\u{0301}\u{0301} \u{1F469}\u{200D}\u{1F467} \u{1F1E6}\u{1F1F7}",
        "\ttabbed\n\tlines\n",
    ];

    fn generated_actions(seed: u64, count: usize) -> Vec<Action> {
        let mut rng = Rng(seed);

        (0..count)
            .map(|_| match rng.below(20) {
                0..=4 => Action::Edit(Edit::Insert(*rng.pick(&CHARS))),
                5 => Action::Edit(Edit::Paste(Arc::new((*rng.pick(&LF_PASTES)).to_owned()))),
                6 => Action::Edit(Edit::Enter),
                7 => Action::Edit(Edit::Indent),
                8 => Action::Edit(Edit::Unindent),
                9 => Action::Edit(Edit::Backspace),
                10 => Action::Edit(Edit::Delete),
                11 => Action::Edit(Edit::Undo),
                12 => Action::Edit(Edit::Redo),
                13 => Action::Move(*rng.pick(&MOTIONS)),
                14 => Action::Select(*rng.pick(&MOTIONS)),
                15 => Action::SelectWord,
                16 => Action::SelectLine,
                17 => Action::Click(
                    Point::new(rng.below(300) as f32, rng.below(80) as f32),
                    crate::core::mouse::click::Kind::Single,
                ),
                18 => Action::Drag(Point::new(rng.below(300) as f32, rng.below(80) as f32)),
                _ => Action::Scroll {
                    lines: rng.below(7) as i32 - 3,
                },
            })
            .collect()
    }

    const MOTIONS: [Motion; 10] = [
        Motion::Left,
        Motion::Right,
        Motion::Up,
        Motion::Down,
        Motion::WordLeft,
        Motion::WordRight,
        Motion::Home,
        Motion::End,
        Motion::DocumentStart,
        Motion::DocumentEnd,
    ];

    /// The whole contract, on generated scenarios: whatever the editor does,
    /// replaying only the returned deltas reproduces its text byte for byte,
    /// after *every* action.
    #[test]
    fn generated_scenarios_replay_byte_for_byte_from_deltas_alone() {
        let fixtures = [
            "",
            "alpha beta gamma",
            "ascii\nlines\nof\ntext",
            "Ünïcødé ✓ 日本語のテキスト\ne\u{0301}\u{0301} combining\n\u{1F469}\u{200D}\u{1F467} family",
            "    indented\n\tby a tab\n        deeply",
        ];

        let mut totals = Mirror::new("");

        for (fixture_index, fixture) in fixtures.iter().enumerate() {
            for seed in 0..4_u64 {
                let mut editor = Editor::with_text(fixture);
                layout(&mut editor, 300.0);

                let mut mirror = Mirror::new(fixture);

                for action in generated_actions(seed * 31 + fixture_index as u64, 120) {
                    let _ = mirror.perform(&mut editor, action);
                }

                totals.deltas += mirror.deltas;
                totals.inserted += mirror.inserted;
                totals.deleted += mirror.deleted;
                totals.bytes += mirror.bytes;
            }
        }

        // Coverage floor: a generator that only moved the caret would satisfy
        // every assertion above without exercising a single delta.
        assert!(
            totals.deltas > 500,
            "the scenarios produced only {} deltas; they are not exercising \
             editing",
            totals.deltas
        );
        assert!(
            totals.inserted > 200 && totals.deleted > 100,
            "the scenarios must exercise both directions: {} inserts, {} \
             deletes",
            totals.inserted,
            totals.deleted
        );
        assert!(
            totals.bytes > 1_000,
            "only {} bytes of edit text were reported",
            totals.bytes
        );
    }

    /// Replacing a selection that spans lines is the multi-item case: one
    /// delete of the whole range, then one insert -- in that order, with the
    /// insert positioned in the document the delete left behind.
    #[test]
    fn replacing_a_cross_line_selection_returns_an_ordered_delete_and_insert() {
        let mut editor = Editor::with_text("alpha\nbeta\ngamma\ndelta");
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new("alpha\nbeta\ngamma\ndelta");

        editor.move_to(Cursor {
            position: Position { line: 2, index: 3 },
            selection: Some(Position { line: 0, index: 2 }),
        });

        let delta = mirror
            .perform(&mut editor, Action::Edit(Edit::Insert('!')))
            .expect("replacing a selection is an edit");

        let items = delta.items();

        assert_eq!(items.len(), 2, "delete then insert: {items:?}");
        assert_eq!(items[0].kind, EditKind::Delete);
        assert_eq!(items[0].start, Position { line: 0, index: 2 });
        assert_eq!(items[0].end, Position { line: 2, index: 3 });
        assert_eq!(items[0].text, "pha\nbeta\ngam");
        assert_eq!(items[1].kind, EditKind::Insert);
        assert_eq!(items[1].text, "!");
        assert_eq!(mirror.text, "al!ma\ndelta");
        assert_eq!(delta.bytes(), "pha\nbeta\ngam".len() + 1);
    }

    /// Pasting text that carries line endings must report the exact bytes,
    /// endings included, and a mirror fed only those bytes must stay identical
    /// -- including when the paste replaces a selection.
    #[test]
    fn pasting_line_endings_reports_the_exact_bytes() {
        for paste in PASTES {
            let mut editor = Editor::with_text("head\ntail");
            layout(&mut editor, 300.0);
            let mut mirror = Mirror::new("head\ntail");

            mirror.step(&mut editor, Action::Move(Motion::Right));
            mirror.step(&mut editor, Action::Move(Motion::Right));

            let delta = mirror
                .perform(
                    &mut editor,
                    Action::Edit(Edit::Paste(Arc::new(paste.to_owned()))),
                )
                .expect("a non-empty paste is an edit");

            assert_eq!(delta.items().len(), 1, "an unselected paste is one item");
            assert_eq!(delta.items()[0].kind, EditKind::Insert);
            assert_eq!(
                delta.items()[0].text,
                paste,
                "the delta must carry the pasted bytes verbatim"
            );

            // ...and again over a selection, which makes it two items.
            mirror.step(&mut editor, Action::SelectAll);
            let delta = mirror
                .perform(
                    &mut editor,
                    Action::Edit(Edit::Paste(Arc::new(paste.to_owned()))),
                )
                .expect("replacing a selection is an edit");

            assert_eq!(delta.items().len(), 2, "delete then insert: {delta:?}");
            assert_eq!(delta.items()[0].kind, EditKind::Delete);
            assert_eq!(delta.items()[1].kind, EditKind::Insert);
        }
    }

    /// Indent and unindent over a multi-line selection produce one item per
    /// line, in order; a mirror that reordered them would drift.
    #[test]
    fn indenting_a_selection_returns_one_item_per_line() {
        let text = "one\ntwo\nthree\nfour";
        let mut editor = Editor::with_text(text);
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new(text);

        editor.move_to(Cursor {
            position: Position { line: 2, index: 1 },
            selection: Some(Position { line: 0, index: 1 }),
        });

        let indent = mirror
            .perform(&mut editor, Action::Edit(Edit::Indent))
            .expect("indenting three lines is an edit");

        assert_eq!(
            indent.items().len(),
            3,
            "one insert per selected line: {indent:?}"
        );
        assert!(
            indent
                .items()
                .iter()
                .all(|item| item.kind == EditKind::Insert && item.text.trim().is_empty()),
            "every indent item inserts whitespace: {indent:?}"
        );
        // The indent width is the editor's tab width, not ours to assume.
        let indent_text = indent.items()[0].text.clone();

        assert_eq!(
            mirror.text,
            format!("{indent_text}one\n{indent_text}two\n{indent_text}three\nfour"),
            "each selected line gained exactly one indent"
        );

        let unindent = mirror
            .perform(&mut editor, Action::Edit(Edit::Unindent))
            .expect("unindenting three lines is an edit");

        assert_eq!(
            unindent.items().len(),
            3,
            "one delete per selected line: {unindent:?}"
        );
        assert!(
            unindent
                .items()
                .iter()
                .all(|item| item.kind == EditKind::Delete),
            "{unindent:?}"
        );
        assert_eq!(mirror.text, text, "indent then unindent is a round trip");
    }

    /// Undo must report the change it actually applied -- the *reversed* one --
    /// and redo the forward one. A delta that reported the stored (unreversed)
    /// change would drift a mirror on the first undo.
    #[test]
    fn typing_undoes_a_word_at_a_time() {
        let mut editor = Editor::with_text("");
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new("");
        for character in "hello world\nnext".chars() {
            let edit = if character == '\n' {
                Edit::Enter
            } else {
                Edit::Insert(character)
            };
            mirror.step(&mut editor, Action::Edit(edit));
        }
        assert_eq!(mirror.text, "hello world\nnext");

        let mut seen = Vec::new();
        while mirror
            .perform(&mut editor, Action::Edit(Edit::Undo))
            .is_some()
        {
            seen.push(mirror.text.clone());
            assert!(seen.len() < 10, "undo is not draining the history");
        }
        assert_eq!(seen, ["hello world\n", "hello world", "hello", ""]);

        for _ in 0..4 {
            let _ = mirror.perform(&mut editor, Action::Edit(Edit::Redo));
        }
        assert_eq!(
            mirror.text, "hello world\nnext",
            "a merged run redoes whole"
        );
    }

    #[test]
    fn moving_the_cursor_starts_a_new_undo_step() {
        let mut editor = Editor::with_text("");
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new("");
        for character in "abc".chars() {
            mirror.step(&mut editor, Action::Edit(Edit::Insert(character)));
        }
        mirror.step(&mut editor, Action::Move(Motion::Left));
        mirror.step(&mut editor, Action::Edit(Edit::Insert('X')));
        assert_eq!(mirror.text, "abXc");

        let _ = mirror.perform(&mut editor, Action::Edit(Edit::Undo));
        assert_eq!(
            mirror.text, "abc",
            "only the character typed after the move"
        );
    }

    #[test]
    fn undo_and_redo_report_the_change_they_applied() {
        let text = "alpha\nbeta";
        let mut editor = Editor::with_text(text);
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new(text);

        mirror.step(&mut editor, Action::Move(Motion::DocumentEnd));

        for character in "XYZ".chars() {
            mirror.step(&mut editor, Action::Edit(Edit::Insert(character)));
        }

        mirror.step(
            &mut editor,
            Action::Edit(Edit::Paste(Arc::new("one\ntwo".to_owned()))),
        );

        editor.move_to(Cursor {
            position: Position { line: 1, index: 3 },
            selection: Some(Position { line: 0, index: 1 }),
        });
        mirror.step(&mut editor, Action::Edit(Edit::Backspace));

        // Undo everything, then redo everything: the mirror is checked after
        // every single step, so any reversal error shows up immediately.
        let mut undos = 0;

        while let Some(delta) = mirror.perform(&mut editor, Action::Edit(Edit::Undo)) {
            assert!(!delta.items().is_empty());
            undos += 1;

            assert!(undos < 20, "undo is not draining the history");
        }

        assert_eq!(
            undos, 3,
            "one undo for the typed run, the paste and the backspace"
        );
        assert_eq!(mirror.text, text, "undoing everything restores the text");

        let mut redos = 0;

        while let Some(_delta) = mirror.perform(&mut editor, Action::Edit(Edit::Redo)) {
            redos += 1;

            assert!(redos < 20, "redo is not draining the history");
        }

        assert_eq!(redos, undos, "every undone change can be redone");

        // And an interleaved chain, undoing partway before typing again.
        for character in "12345".chars() {
            mirror.step(&mut editor, Action::Edit(Edit::Insert(character)));
        }

        for _ in 0..3 {
            mirror.step(&mut editor, Action::Edit(Edit::Undo));
        }

        mirror.step(&mut editor, Action::Edit(Edit::Insert('!')));

        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Redo))
                .is_none(),
            "typing after an undo truncates the redo branch, so redo has \
             nothing to apply and must return no delta"
        );
    }

    /// A range batch is one history entry, retains the history before it, and
    /// restores the exact pre-batch caret on undo. Sabotage: rebuild the editor
    /// from the resulting text, or push each replacement separately; the first
    /// undo no longer restores both matches and the preceding edit is lost or
    /// consumed at the wrong step.
    #[test]
    fn replace_batch_is_one_undo_step_and_preserves_prior_history() {
        let original = "alpha cat beta cat omega";
        let mut editor = Editor::with_text(original);
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new(original);

        mirror.step(&mut editor, Action::Move(Motion::DocumentEnd));
        mirror.step(&mut editor, Action::Edit(Edit::Insert('!')));
        editor.move_to(Cursor {
            position: Position { line: 0, index: 6 },
            selection: None,
        });
        let before_batch_cursor = editor.cursor();

        let batch = ReplaceBatch {
            replacements: vec![
                editor::Replacement::new(
                    Position { line: 0, index: 6 }..Position { line: 0, index: 9 },
                    "elephant",
                ),
                editor::Replacement::new(
                    Position { line: 0, index: 15 }..Position { line: 0, index: 18 },
                    "dog",
                ),
            ],
            cursor_after: Cursor {
                position: Position { line: 0, index: 11 },
                selection: None,
            },
        };
        let delta = mirror
            .perform(&mut editor, Action::Edit(Edit::Replace(Arc::new(batch))))
            .expect("the batch changes text");

        assert_eq!(delta.items().len(), 4, "two delete/insert pairs");
        assert_eq!(mirror.text, "alpha elephant beta dog omega!");
        assert_eq!(editor.cursor().position, Position { line: 0, index: 11 });

        mirror.step(&mut editor, Action::Edit(Edit::Undo));
        assert_eq!(mirror.text, "alpha cat beta cat omega!");
        assert_eq!(
            editor.cursor(),
            before_batch_cursor,
            "undo restores the caret that preceded the transaction"
        );

        mirror.step(&mut editor, Action::Edit(Edit::Undo));
        assert_eq!(
            mirror.text, original,
            "the edit before Replace All remains the next undo step"
        );
    }

    /// Validation is all-or-nothing. Sabotage: apply while validating; the
    /// first range mutates the buffer before the overlapping second range is
    /// rejected, and this test observes the partial edit.
    #[test]
    fn invalid_replace_batch_is_an_atomic_no_op() {
        let original = "abcdef";
        let mut editor = Editor::with_text(original);
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new(original);
        let batch = ReplaceBatch {
            replacements: vec![
                editor::Replacement::new(
                    Position { line: 0, index: 1 }..Position { line: 0, index: 4 },
                    "x",
                ),
                editor::Replacement::new(
                    Position { line: 0, index: 3 }..Position { line: 0, index: 5 },
                    "y",
                ),
            ],
            cursor_after: Cursor {
                position: Position { line: 0, index: 0 },
                selection: None,
            },
        };

        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Replace(Arc::new(batch))))
                .is_none(),
            "an overlapping plan is rejected"
        );
        assert_eq!(mirror.text, original, "no prefix may be applied");
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Undo))
                .is_none(),
            "a rejected batch must not enter history"
        );
    }

    /// A one-byte range replacement reports the same two changed bytes in a
    /// 2.4 MiB and a 24 MiB document. The deterministic counter, rather than a
    /// generous wall-clock threshold, proves the returned work does not scale
    /// with document size.
    #[test]
    fn replace_batch_delta_is_proportional_to_changed_bytes() {
        let mut costs = Vec::new();

        for lines in [100_000_usize, 1_000_000] {
            let text = document(lines);
            let mut editor = Editor::with_text(&text);
            layout(&mut editor, 300.0);
            let before = instrument::counters();
            let batch = ReplaceBatch {
                replacements: vec![editor::Replacement::new(
                    Position { line: 0, index: 0 }..Position { line: 0, index: 1 },
                    "z",
                )],
                cursor_after: Cursor {
                    position: Position { line: 0, index: 1 },
                    selection: None,
                },
            };

            let delta = editor
                .perform(Action::Edit(Edit::Replace(Arc::new(batch))))
                .expect("one byte changed");
            let counted = instrument::counters().since(before);

            assert_eq!(delta.bytes(), 2, "one deleted byte plus one inserted byte");
            assert_eq!(counted.bytes_copied, 0);
            costs.push((
                text.len(),
                counted.delta_items_returned,
                counted.delta_bytes_returned,
            ));
        }

        assert!(costs[1].0 > 20 * 1024 * 1024);
        assert_eq!(
            (costs[0].1, costs[0].2),
            (costs[1].1, costs[1].2),
            "range-edit delta work must not grow with document size: {costs:?}"
        );
    }

    /// Release-only 40 MiB timing control for the same range-edit path. The
    /// deterministic multi-size test above is the gate; this report catches
    /// regressions in the editor backend that counters cannot see.
    #[test]
    #[ignore = "measurement, not a gate: run with --release --nocapture"]
    fn report_replace_batch_cost_40_mib() {
        let text = forty_mib_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);
        let batch = ReplaceBatch {
            replacements: vec![editor::Replacement::new(
                Position { line: 0, index: 0 }..Position { line: 0, index: 1 },
                "z",
            )],
            cursor_after: Cursor {
                position: Position { line: 0, index: 1 },
                selection: None,
            },
        };
        let started = std::time::Instant::now();
        let delta = editor
            .perform(Action::Edit(Edit::Replace(Arc::new(batch))))
            .expect("one byte changed");

        assert_eq!(delta.bytes(), 2);
        println!(
            "#1885 replace batch -- {:.1} MiB, one byte: {:?}",
            text.len() as f64 / (1024.0 * 1024.0),
            started.elapsed()
        );
    }

    /// An edit that changes nothing returns nothing: a backspace at the very
    /// start, a delete at the very end, an undo with an empty history, a redo
    /// at the head of it. A mirror that trusted an empty `Some` would still be
    /// correct, but every caller would have to check.
    #[test]
    fn no_op_edits_and_non_edit_actions_return_no_delta() {
        let text = "solo";
        let mut editor = Editor::with_text(text);
        layout(&mut editor, 300.0);
        let mut mirror = Mirror::new(text);

        // Nothing has been edited yet: undo and redo are no-ops.
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Undo))
                .is_none(),
            "undo with an empty history must return no delta"
        );
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Redo))
                .is_none(),
            "redo with an empty history must return no delta"
        );

        // Backspace at the first position of the first line.
        mirror.step(&mut editor, Action::Move(Motion::DocumentStart));
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Backspace))
                .is_none(),
            "a backspace at the start of the document deletes nothing"
        );

        // Delete at the last position of the last line.
        mirror.step(&mut editor, Action::Move(Motion::DocumentEnd));
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Delete))
                .is_none(),
            "a delete at the end of the document deletes nothing"
        );

        // An unindent of a line with no indentation.
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Unindent))
                .is_none(),
            "unindenting an unindented line changes nothing"
        );

        // An empty paste.
        assert!(
            mirror
                .perform(
                    &mut editor,
                    Action::Edit(Edit::Paste(Arc::new(String::new())))
                )
                .is_none(),
            "pasting nothing changes nothing"
        );

        assert_eq!(mirror.text, text, "no-ops must not have moved a byte");

        // Positive control: the same editor does return a delta for a real
        // edit, so the assertions above are not passing on a dead path.
        assert!(
            mirror
                .perform(&mut editor, Action::Edit(Edit::Insert('x')))
                .is_some(),
            "a real insertion must return a delta"
        );
    }

    /// Pointer, caret and scroll work must return -- and copy -- nothing. The
    /// counter is what makes this a cost claim rather than a shape claim.
    #[test]
    fn caret_drag_and_scroll_actions_copy_no_bytes() {
        let text = huge_document();
        let mut editor = Editor::with_text(&text);
        layout(&mut editor, 300.0);

        let actions = [
            Action::Click(
                Point::new(20.0, 20.0),
                crate::core::mouse::click::Kind::Single,
            ),
            Action::Drag(Point::new(120.0, 60.0)),
            Action::Move(Motion::Down),
            Action::Select(Motion::DocumentEnd),
            Action::SelectWord,
            Action::SelectLine,
            Action::SelectAll,
            Action::Scroll { lines: 12 },
            Action::ScrollTo {
                offset: Vector::new(0.0, 4_000.0),
            },
        ];

        let before = instrument::counters();

        for action in actions {
            assert!(
                editor.perform(action.clone()).is_none(),
                "{action:?} is not an edit"
            );
        }

        let counted = instrument::counters().since(before);

        assert_eq!(
            (counted.delta_bytes_returned, counted.delta_items_returned),
            (0, 0),
            "non-edit actions over a {} line document returned edit bytes: \
             {counted:?}",
            editor.line_count()
        );
    }

    /// A one-character edit must cost the character, whatever the document
    /// weighs -- at the top, in the middle and at the tail. The counters are
    /// deterministic, so this cannot be passed by a fast machine, and the two
    /// document sizes must report the *identical* delta cost.
    #[test]
    fn a_one_character_edit_of_a_huge_document_returns_a_tiny_delta() {
        // ~2.4 MiB and ~24 MiB of text; the 40 MiB control lives in the
        // release measurement below, where it does not cost a debug test run
        // a minute of line construction.
        let mut per_size = Vec::new();

        for lines in [100_000_usize, 1_000_000] {
            let text = document(lines);
            let mut editor = Editor::with_text(&text);
            layout(&mut editor, 300.0);

            let mut costs = Vec::new();

            for fraction in [0.0, 0.5, 1.0] {
                let line = ((lines - 1) as f64 * fraction) as usize;

                editor.move_to(Cursor {
                    position: Position { line, index: 1 },
                    selection: None,
                });

                let before = instrument::counters();
                let delta = editor
                    .perform(Action::Edit(Edit::Insert('!')))
                    .expect("inserting a character is an edit");
                let counted = instrument::counters().since(before);

                assert_eq!(delta.bytes(), 1, "one character, one byte: {delta:?}");
                assert_eq!(delta.items().len(), 1);
                assert_eq!(
                    delta.items()[0].start,
                    Position { line, index: 1 },
                    "the delta must locate the edit, not the document"
                );

                costs.push((counted.delta_items_returned, counted.delta_bytes_returned));
            }

            assert!(
                costs.iter().all(|cost| *cost == (1, 1)),
                "a one-character edit of a {lines} line document reported \
                 {costs:?}"
            );

            per_size.push((text.len(), costs));
        }

        let (small_bytes, small) = &per_size[0];
        let (large_bytes, large) = &per_size[1];

        assert!(
            *large_bytes > 20 * 1024 * 1024 && large_bytes > &(small_bytes * 8),
            "the control must actually be an order of magnitude larger: \
             {small_bytes} vs {large_bytes} bytes"
        );
        assert_eq!(
            small, large,
            "delta cost must not scale with document size: {small_bytes} byte \
             document {small:?} vs {large_bytes} byte document {large:?}"
        );
    }

    /// The 40 MiB control, plus release timings for a one-character edit at the
    /// top, the middle and the tail. Not a gate: the counter gates above are
    /// what fails.
    ///
    /// ```sh
    /// cargo test -p iced_graphics --release --lib \
    ///     text::editor::tests::report_edit_delta_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measurement, not a gate: run with --release --nocapture"]
    fn report_edit_delta_cost() {
        const EDITS: usize = 200;

        // ~40 MiB: 1 000 000 lines averaging ~42 bytes.
        let text = document(1_700_000);
        let lines = text.lines().count();

        assert!(
            text.len() >= 40 * 1024 * 1024,
            "the control must be at least 40 MiB, got {} bytes",
            text.len()
        );

        let rss_before = rss_mib();
        let started = std::time::Instant::now();
        let mut editor = Editor::with_text(&text);
        let construction = started.elapsed();

        layout(&mut editor, 300.0);

        let mut report = Vec::new();

        for (name, fraction) in [("top", 0.0), ("middle", 0.5), ("tail", 1.0)] {
            let mut deltas = Vec::with_capacity(EDITS);
            let mut flattens = Vec::with_capacity(EDITS);
            let mut bytes = 0;

            for edit in 0..EDITS {
                let line = ((lines - 1) as f64 * fraction) as usize;

                editor.move_to(Cursor {
                    position: Position {
                        line,
                        index: 1 + edit % 4,
                    },
                    selection: None,
                });

                let started = std::time::Instant::now();
                let delta = editor.perform(Action::Edit(Edit::Insert('!')));
                deltas.push(started.elapsed().as_secs_f64() * 1.0e6);

                bytes += delta.map_or(0, |delta| delta.bytes());

                // The alternative an app has without #1805, sampled rarely
                // because it is O(document).
                if edit % 50 == 0 {
                    let started = std::time::Instant::now();
                    let flattened = flatten(&editor);
                    flattens.push(started.elapsed().as_secs_f64() * 1.0e6);
                    assert!(!flattened.is_empty());
                }
            }

            report.push((name, deltas, flattens, bytes));
        }

        let rss_after = rss_mib();

        println!(
            "#1805 measurement -- {lines} lines, {:.1} MiB of text\n\
             with_text: {construction:?}   RSS {rss_before:.1} -> {rss_after:.1} MiB",
            text.len() as f64 / (1024.0 * 1024.0),
        );

        for (name, mut deltas, mut flattens, bytes) in report {
            println!(
                "  {name:<6} one-char edit: p50 {:.1} us  p95 {:.1} us   \
                 delta bytes {bytes} for {EDITS} edits   \
                 (Content::text() equivalent: p50 {:.1} us)",
                percentile(&mut deltas, 0.5),
                percentile(&mut deltas, 0.95),
                percentile(&mut flattens, 0.5),
            );
        }
    }
    /// Undo must not leave the selection pointing at a column that no longer
    /// exists: the next edit deletes that selection, and a stale column makes
    /// cosmic-text split a line outside a character boundary and panic.
    ///
    /// The five actions below are ordinary keyboard work (indent, select, undo,
    /// type) and used to abort the process in
    /// `BufferLine::split_off`. Found by the generated #1805 scenarios.
    #[test]
    fn undo_leaves_the_cursor_and_selection_inside_the_document() {
        let mut editor = Editor::with_text("\n");
        layout(&mut editor, 300.0);

        let _ = editor.perform(Action::Click(
            Point::new(145.0, 27.0),
            crate::core::mouse::click::Kind::Single,
        ));
        let _ = editor.perform(Action::Edit(Edit::Indent));
        let _ = editor.perform(Action::Select(Motion::WordLeft));

        // The indentation this selection covers is about to disappear.
        let indented = editor.cursor();

        assert_eq!(
            indented.selection,
            Some(Position { line: 1, index: 8 }),
            "the fixture must select the indentation for this to test anything"
        );

        let _ = editor.perform(Action::Edit(Edit::Undo));
        let undone = editor.cursor();

        assert!(
            undone.position.line < editor.line_count(),
            "the cursor must stay inside the document: {undone:?}"
        );

        for position in [Some(undone.position), undone.selection]
            .into_iter()
            .flatten()
        {
            let line = editor
                .line(position.line)
                .expect("the position must address a line that exists");

            assert!(
                position.index <= line.text.len() && line.text.is_char_boundary(position.index),
                "{position:?} is outside line {:?} after an undo",
                line.text
            );
        }

        // The edit that used to panic.
        let _ = editor.perform(Action::Edit(Edit::Paste(Arc::new("e\u{301}".to_owned()))));

        assert_eq!(flatten(&editor), "\ne\u{301}");
    }
}

fn line_number_rows(
    buffer: &cosmic_text::Buffer,
    height: f32,
    hint_factor: f32,
    origin: usize,
) -> Result<Vec<crate::core::text::editor::gutter::Row>, crate::core::renderer::presentation::Error>
{
    use crate::core::renderer::presentation::Error;
    use crate::core::text::editor::gutter::Row;
    if !height.is_finite() || height < 0.0 || !hint_factor.is_finite() || hint_factor <= 0.0 {
        return Err(Error::Viewport);
    }
    let mut top = -buffer.scroll().vertical;
    let mut visited = 0usize;
    let mut heads = Vec::new();
    for (line_index, line) in buffer.lines.iter().enumerate().skip(buffer.scroll().line) {
        if top >= height {
            break;
        }
        let rows = line.layout_opt().ok_or(Error::UnsupportedBackend)?;
        for (row_index, row) in rows.iter().enumerate() {
            if top >= height {
                break;
            }
            visited += 1;
            if visited > 4096 {
                return Err(Error::Budget);
            }
            let row_height = row.line_height(buffer.metrics().line_height);
            if !row_height.is_finite() || row_height <= 0.0 {
                return Err(Error::Viewport);
            }
            if row_index == 0 && top + row_height > 0.0 {
                heads.push(Row {
                    line: origin.checked_add(line_index).ok_or(Error::Budget)?,
                    top: top / hint_factor,
                    height: row_height / hint_factor,
                });
            }
            top += row_height;
        }
    }
    Ok(heads)
}

#[cfg(test)]
mod inline_metrics_tests {
    use super::*;
    use crate::core::text::Editor as _;

    #[derive(Clone)]
    struct InlineMetrics {
        current: usize,
        runs: Vec<Vec<(Range<usize>, Pixels)>>,
    }

    impl Parser for InlineMetrics {
        type Settings = Vec<Vec<(Range<usize>, Pixels)>>;
        type Output = Pixels;
        type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Pixels)>;
        fn new(runs: &Self::Settings) -> Self {
            Self {
                current: 0,
                runs: runs.clone(),
            }
        }
        fn update(&mut self, runs: &Self::Settings) {
            self.runs = runs.clone();
            self.current = 0;
        }
        fn change_line(&mut self, line: usize) {
            self.current = self.current.min(line);
        }
        fn parse_line(&mut self, _: &str) -> Self::Iterator<'_> {
            let runs = self.runs.get(self.current).cloned().unwrap_or_default();
            self.current += 1;
            runs.into_iter()
        }
        fn current_line(&self) -> usize {
            self.current
        }
        fn seek_line(&mut self, line: usize) -> bool {
            self.current = line;
            true
        }
        fn has_layout_style(&self) -> bool {
            true
        }
        fn layout_style(size: &Pixels) -> highlighter::Style {
            highlighter::Style {
                size: Some(*size),
                ..highlighter::Style::default()
            }
        }
    }

    fn update(editor: &mut Editor, parser: &mut InlineMetrics, width: f32) {
        editor.update(
            Size::new(width, 200.0),
            Font::DEFAULT,
            Pixels(16.0),
            LineHeight::Relative(1.25),
            Wrapping::Word,
            Alignment::Default,
            None,
            parser,
        );
    }

    #[test]
    fn inline_metrics_layout_precedes_measurement_and_selected_glyphs_only() {
        let mut editor = Editor::with_text("aé中z");
        let mut parser = InlineMetrics::new(&vec![vec![(1..6, Pixels(32.0))]]);
        update(&mut editor, &mut parser, 400.0);
        let buffer = editor.buffer();
        let glyphs = &buffer.lines[0].layout_opt().unwrap()[0].glyphs;
        assert!(
            glyphs
                .iter()
                .any(|glyph| glyph.start >= 1 && glyph.end <= 6 && glyph.font_size == 32.0)
        );
        assert!(
            glyphs
                .iter()
                .filter(|glyph| glyph.start == 0 || glyph.start >= 6)
                .all(|glyph| glyph.font_size == 16.0)
        );
        assert!((editor.min_bounds().height - 40.0).abs() < 0.01);
        parser.update(&vec![vec![]]);
        update(&mut editor, &mut parser, 400.0);
        assert!((editor.min_bounds().height - 20.0).abs() < 0.01);
    }

    #[test]
    fn inline_metrics_variable_rows_caret_selection_highlights_and_scroll() {
        let mut editor = Editor::with_text("tiny\naé中z");
        let mut parser = InlineMetrics::new(&vec![
            vec![(0..4, Pixels(12.0))],
            vec![(1..6, Pixels(32.0))],
        ]);
        update(&mut editor, &mut parser, 400.0);
        assert!((editor.min_bounds().height - 55.0).abs() < 0.01);
        let _ = editor.perform(Action::Move(Motion::DocumentEnd));
        assert!((editor.caret_height().0 - 40.0).abs() < 0.01);
        let highlights = editor.highlight_ranges(&[(
            Position { line: 0, index: 0 },
            Position { line: 1, index: 7 },
        )]);
        assert_eq!(highlights[0].len(), 2);
        assert!((highlights[0][0].height - 15.0).abs() < 0.01);
        assert!((highlights[0][1].y - 15.0).abs() < 0.01);
        assert!((highlights[0][1].height - 40.0).abs() < 0.01);
        let _ = editor.perform(Action::SelectAll);
        let Selection::Range(selection) = editor.selection() else {
            panic!("selected two rows")
        };
        assert_eq!(selection, highlights[0]);
        let metrics = editor.scroll_metrics();
        assert!((metrics.content.height - 55.0).abs() < 0.01, "{metrics:?}");
    }

    #[test]
    fn inline_metrics_smaller_rows_attribute_the_whole_visible_window() {
        let text = std::iter::repeat_n("small", 40)
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::with_text(&text);
        let mut parser = InlineMetrics::new(&vec![vec![(0..5, Pixels(8.0))]; 40]);
        update(&mut editor, &mut parser, 400.0);
        let runs: Vec<_> = editor.buffer().layout_runs().collect();
        assert!(
            runs.len() >= 20,
            "small rows should fill the native window: {}",
            runs.len()
        );
        assert!(
            runs.iter()
                .flat_map(|run| run.glyphs)
                .all(|glyph| glyph.font_size == 8.0)
        );
    }

    #[test]
    fn inline_metrics_uniform_selection_skips_rows_above_the_actual_viewport() {
        let mut editor = Editor::with_text(&"words wrap across many rows ".repeat(1000));
        let mut parser = InlineMetrics::new(&vec![vec![]]);
        update(&mut editor, &mut parser, 200.0);
        let _ = editor.perform(Action::SelectAll);
        let _ = editor.perform(Action::ScrollTo {
            offset: Vector::new(0.0, 5000.0),
        });
        assert_eq!(editor.buffer().scroll().line, 0);
        assert!(editor.buffer().scroll().vertical > 1000.0);
        let before = instrument::counters();
        let Selection::Range(regions) = editor.selection() else {
            panic!("selected the paragraph")
        };
        let cost = instrument::counters().since(before);
        assert!(!regions.is_empty());
        assert!(
            regions
                .iter()
                .all(|region| region.y >= 0.0 && region.y + region.height <= 200.0)
        );
        assert!(
            cost.visual_rows_shaped <= 12,
            "uniform selection must skip offscreen rows: {cost:?}"
        );
    }

    #[test]
    fn inline_metrics_small_span_keeps_base_row_and_gutter_height() {
        let mut editor = Editor::with_text("a b\nlast");
        let mut parser = InlineMetrics::new(&vec![vec![(0..1, Pixels(8.0))], vec![]]);
        update(&mut editor, &mut parser, 400.0);
        let rows = line_number_rows(editor.buffer(), 200.0, 1.0, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert!((rows[0].height - 20.0).abs() < 0.01);
        assert!((rows[1].top - 20.0).abs() < 0.01);
    }

    #[test]
    fn inline_metrics_wrap_keeps_native_rows_and_refuses_unsupported_retained_metrics() {
        let mut editor = Editor::with_text("small WIDE WIDE WIDE ending");
        let mut parser = InlineMetrics::new(&vec![vec![(6..20, Pixels(32.0))]]);
        update(&mut editor, &mut parser, 120.0);
        let rows = editor.buffer().lines[0].layout_opt().unwrap();
        assert!(rows.len() >= 3);
        let expected: f32 = rows.iter().map(|row| row.line_height(20.0)).sum();
        assert!((editor.min_bounds().height - expected).abs() < 0.01);
        assert!(matches!(
            editor.snapshot(200.0),
            Err(crate::core::renderer::presentation::Error::UnsupportedBackend)
        ));
        assert!((editor.min_bounds().height - expected).abs() < 0.01);
        parser.update(&vec![vec![]]);
        update(&mut editor, &mut parser, 120.0);
        assert!(editor.snapshot(200.0).is_ok());
    }
}

#[cfg(test)]
mod inline_index_tests {
    use super::ScrollIndex;

    #[test]
    fn inline_metrics_signed_corrections_keep_absolute_seek_invertible() {
        let mut index = ScrollIndex::new(5, 20.0);
        assert!(index.observe(0, 5.0));
        assert!(index.observe(1, 40.0));
        assert!(index.observe(3, 10.0));
        for (line, top) in [0.0, 5.0, 45.0, 65.0, 75.0].into_iter().enumerate() {
            assert!((index.offset_of(line) - top).abs() < 0.001);
            assert_eq!(index.line_at(top + 0.5), (line, 0.5));
        }
        assert!((index.content_height() - 95.0).abs() < 0.001);
    }
}
