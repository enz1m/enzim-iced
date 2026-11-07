// Modified by enzUI contributors. See FORK.md.
//! Text editors display a multi-line text input for text editing.
//!
//! # Example
//! ```no_run
//! # mod iced { pub mod widget { pub use iced_widget::*; } pub use iced_widget::Renderer; pub use iced_widget::core::*; }
//! # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
//! #
//! use iced::widget::text_editor;
//!
//! struct State {
//!    content: text_editor::Content,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     Edit(text_editor::Action)
//! }
//!
//! fn view(state: &State) -> Element<'_, Message> {
//!     text_editor(&state.content)
//!         .placeholder("Type something here...")
//!         .on_action(Message::Edit)
//!         .into()
//! }
//!
//! fn update(state: &mut State, message: Message) {
//!     match message {
//!         Message::Edit(action) => {
//!             state.content.perform(action);
//!         }
//!     }
//! }
//! ```
use crate::core::clipboard;
use crate::core::input_method;
use crate::core::keyboard;
use crate::core::keyboard::key;
use crate::core::layout::{self, Layout};
use crate::core::mouse;
use crate::core::renderer;
use crate::core::text::editor::Editor as _;
use crate::core::text::highlighter;
use crate::core::text::parser::{self, Parser};
use crate::core::text::{self, LineHeight, Wrapping};
use crate::core::theme;
use crate::core::time::{Duration, Instant};
use crate::core::touch;
use crate::core::widget::operation;
use crate::core::widget::{self, Widget};
use crate::core::window;
use crate::core::{
    Background, Border, Color, Element, Event, Font, InputMethod, Length, Padding, Pixels, Point,
    Rectangle, Shell, Size, SmolStr, Theme, Vector,
};

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::ops;
use std::ops::DerefMut;
use std::sync::Arc;

pub use text::editor::{
    Action, Cursor, Edit, EditDelta, EditItem, EditKind, Line, LineEnding, Motion, Position,
    ReplaceBatch, Replacement, ScrollMetrics, Selection,
};

/// A multi-line text input.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } pub use iced_widget::Renderer; pub use iced_widget::core::*; }
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// #
/// use iced::widget::text_editor;
///
/// struct State {
///    content: text_editor::Content,
/// }
///
/// #[derive(Debug, Clone)]
/// enum Message {
///     Edit(text_editor::Action)
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     text_editor(&state.content)
///         .placeholder("Type something here...")
///         .on_action(Message::Edit)
///         .into()
/// }
///
/// fn update(state: &mut State, message: Message) {
///     match message {
///         Message::Edit(action) => {
///             state.content.perform(action);
///         }
///     }
/// }
/// ```
pub struct TextEditor<'a, Highlighter, Message, Theme = crate::Theme, Renderer = crate::Renderer>
where
    Highlighter: text::Parser,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    id: Option<widget::Id>,
    content: &'a Content<Renderer>,
    placeholder: Option<text::Fragment<'a>>,
    font: Option<Font>,
    text_size: Option<Pixels>,
    line_height: LineHeight,
    width: Length,
    height: Length,
    padding: Padding,
    gutter: Option<text::editor::gutter::Gutter>,
    wrapping: Wrapping,
    class: Theme::Class<'a>,
    key_binding: Option<Box<dyn Fn(KeyPress) -> Option<Binding<Message>> + 'a>>,
    on_edit: Option<Box<dyn Fn(Action) -> Message + 'a>>,
    on_drag_update: Option<Box<dyn Fn(Cursor) -> Message + 'a>>,
    highlighter_settings: Highlighter::Settings,
    highlighter_format: fn(&Highlighter::Output, &Theme) -> highlighter::Style,
    last_status: Option<Status>,
    background_highlights: Vec<(ops::Range<text::editor::Position>, Color)>,
    retain_selection: bool,
    line_alignment: Option<Box<dyn Fn(usize) -> text::Alignment + 'a>>,
    exclusions: Vec<text::editor::Exclusion>,
}

impl<'a, Message, Theme, Renderer> TextEditor<'a, parser::PlainText, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Creates new [`TextEditor`] with the given [`Content`].
    pub fn new(content: &'a Content<Renderer>) -> Self {
        Self {
            id: None,
            content,
            placeholder: None,
            font: None,
            text_size: None,
            line_height: LineHeight::default(),
            width: Length::Fill,
            height: Length::Shrink,
            padding: Padding::new(5.0),
            gutter: None,
            wrapping: Wrapping::default(),
            class: <Theme as Catalog>::default(),
            key_binding: None,
            on_edit: None,
            on_drag_update: None,
            highlighter_settings: (),
            highlighter_format: |_highlight, _theme| highlighter::Style::default(),
            last_status: None,
            background_highlights: Vec::new(),
            retain_selection: false,
            line_alignment: None,
            exclusions: Vec::new(),
        }
    }
}

impl<'a, Highlighter, Message, Renderer>
    TextEditor<'a, Highlighter, Message, crate::Theme, Renderer>
where
    Highlighter: text::Parser,
    Renderer: text::Renderer,
{
    /// Highlights the [`TextEditor`] using the given syntax, colored from the
    /// theme's palette (upstream #3449).
    #[cfg(feature = "highlighter")]
    pub fn highlight(
        self,
        syntax: &str,
    ) -> TextEditor<'a, iced_highlighter::Parser, Message, crate::Theme, Renderer> {
        self.highlight_with::<iced_highlighter::Parser>(
            iced_highlighter::Settings {
                token: syntax.to_owned(),
            },
            |code, theme| code.highlight(theme),
        )
    }
}

impl<'a, Highlighter, Message, Theme, Renderer>
    TextEditor<'a, Highlighter, Message, Theme, Renderer>
where
    Highlighter: text::Parser,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Sets the [`Id`](widget::Id) of the [`TextEditor`].
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Sets the placeholder of the [`TextEditor`].
    pub fn placeholder(mut self, placeholder: impl text::IntoFragment<'a>) -> Self {
        self.placeholder = Some(placeholder.into_fragment());
        self
    }

    /// Sets the width of the [`TextEditor`].
    pub fn width(mut self, width: impl Into<Pixels>) -> Self {
        self.width = Length::from(width.into());
        self
    }

    /// Sets the height of the [`TextEditor`].
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the message that should be produced when some action is performed in
    /// the [`TextEditor`].
    ///
    /// If this method is not called, the [`TextEditor`] will be disabled.
    pub fn on_action(mut self, on_edit: impl Fn(Action) -> Message + 'a) -> Self {
        self.on_edit = Some(Box::new(on_edit));
        self
    }

    /// Applies pointer-drag selection directly to [`Content`] at redraw time
    /// and produces a notification containing the resulting cursor.
    ///
    /// This is an opt-in low-latency path for editors whose application
    /// reducer does substantial unrelated bookkeeping. The ordinary
    /// [`on_action`](Self::on_action) contract remains the default: without
    /// this callback, each drag is published as [`Action::Drag`] for the
    /// application to perform. With it, a drag is performed exactly once by
    /// the widget immediately before paint and is **not** also sent to
    /// `on_action`; the callback is notification only and must not perform the
    /// drag again. Click, release ordering, keyboard selection, autoscroll and
    /// all editing actions keep their existing paths.
    pub fn on_drag_update(mut self, on_drag_update: impl Fn(Cursor) -> Message + 'a) -> Self {
        self.on_drag_update = Some(Box::new(on_drag_update));
        self
    }

    /// Sets the [`Font`] of the [`TextEditor`].
    ///
    /// [`Font`]: text::Font
    pub fn font(mut self, font: impl Into<Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    /// Sets the text size of the [`TextEditor`].
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.text_size = Some(size.into());
        self
    }

    /// Sets the [`text::LineHeight`] of the [`TextEditor`].
    pub fn line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    /// Sets the [`Padding`] of the [`TextEditor`].
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the [`Wrapping`] strategy of the [`TextEditor`].
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// Highlights the [`TextEditor`] with the given [`Highlighter`] and
    /// a strategy to turn its highlights into some text format.
    pub fn highlight_with<H: text::Parser>(
        self,
        settings: H::Settings,
        to_format: fn(&H::Output, &Theme) -> highlighter::Style,
    ) -> TextEditor<'a, H, Message, Theme, Renderer> {
        TextEditor {
            id: self.id,
            content: self.content,
            placeholder: self.placeholder,
            font: self.font,
            text_size: self.text_size,
            line_height: self.line_height,
            width: self.width,
            height: self.height,
            padding: self.padding,
            gutter: self.gutter,
            wrapping: self.wrapping,
            class: self.class,
            key_binding: self.key_binding,
            on_edit: self.on_edit,
            on_drag_update: self.on_drag_update,
            highlighter_settings: settings,
            highlighter_format: to_format,
            last_status: self.last_status,
            background_highlights: self.background_highlights,
            retain_selection: self.retain_selection,
            line_alignment: self.line_alignment,
            exclusions: self.exclusions,
        }
    }

    /// Whether `point`, in the editor's text space, is over the selection.
    fn selection_contains(&self, point: Point) -> bool {
        let internal = self.content.0.borrow();
        let Some(range) = internal.editor.selection_range() else {
            return false;
        };
        if range.start == range.end {
            return false;
        }
        internal
            .editor
            .highlight_ranges(&[(range.start, range.end)])
            .into_iter()
            .flatten()
            .any(|rect| rect.contains(point))
    }

    /// Sets background highlight runs to paint under the editor text.
    ///
    /// Each entry is a `(Range<Position>, Color)` — the range is painted
    /// as coloured rectangles behind the text. Typical uses: search-match
    /// highlighting, lint markers, diff ranges.
    pub fn background_highlights(
        mut self,
        runs: Vec<(ops::Range<text::editor::Position>, Color)>,
    ) -> Self {
        self.background_highlights = runs;
        self
    }

    /// Keeps a range selection painted while the editor is unfocused
    /// (enzICED), for an owner whose popup or dialog takes focus over the
    /// same selection. The caret is still hidden. Off by default.
    pub fn retain_selection(mut self, retain: bool) -> Self {
        self.retain_selection = retain;
        self
    }

    /// Sets the rectangles the text flows around (see
    /// [`text::editor::Exclusion`]).
    ///
    /// Each wrapped row takes the widest gap they leave at its height, and a
    /// row they leave no usable gap for moves below them. The content, its
    /// cursor and its history are untouched; the caller draws whatever the
    /// rectangles stand for, where [`Content::exclusion_bounds`] places them.
    pub fn exclusions(
        mut self,
        exclusions: impl IntoIterator<Item = text::editor::Exclusion>,
    ) -> Self {
        self.exclusions = exclusions.into_iter().collect();
        self
    }

    /// Sets the [`text::Alignment`] of each logical line, by index.
    ///
    /// The editor asks `alignment` for every line it holds on the pass that
    /// installs its bounds, so one closure states the alignment of the whole
    /// document and a caller does not track which lines changed. An editor
    /// built without this takes [`text::Alignment::Default`] for every line,
    /// which also releases an alignment a previous element installed; that
    /// release costs one pass over the lines it had aligned, and nothing at all
    /// for an editor that was never aligned.
    ///
    /// Alignment moves the glyphs, and the caret, the selection rectangles and
    /// the hit test move with them. Line breaking is unaffected: it is decided
    /// by the editor's bounds.
    pub fn line_alignment(mut self, alignment: impl Fn(usize) -> text::Alignment + 'a) -> Self {
        self.line_alignment = Some(Box::new(alignment));
        self
    }

    /// Sets the closure to produce key bindings on key presses.
    ///
    /// See [`Binding`] for the list of available bindings.
    pub fn key_binding(
        mut self,
        key_binding: impl Fn(KeyPress) -> Option<Binding<Message>> + 'a,
    ) -> Self {
        self.key_binding = Some(Box::new(key_binding));
        self
    }

    /// Sets the style of the [`TextEditor`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Adds a leading line-number gutter positioned by this editor's own wrapping and scroll state.
    /// Its width is included in layout, pointer coordinates, selection and input-method geometry.
    pub fn gutter(mut self, gutter: text::editor::gutter::Gutter) -> Self {
        self.gutter = Some(gutter);
        self
    }

    fn content_padding(&self) -> Padding {
        self.gutter
            .map_or(self.padding, |gutter| gutter.content_padding(self.padding))
    }

    /// Sets the style class of the [`TextEditor`].
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// The lines around the cursor (up to about half the platform's limit
    /// on each side), for the input method.
    fn surrounding(&self) -> Option<input_method::Surrounding> {
        let internal = self.content.0.borrow();
        let editor = &internal.editor;
        let cursor = editor.cursor();
        let half = input_method::Surrounding::MAX_BYTES / 2;
        let (mut first, mut last) = (cursor.position.line, cursor.position.line);
        let mut before = 0;
        while first > 0 && before < half {
            first -= 1;
            before += editor.line(first).map_or(0, |line| line.text.len() + 1);
        }
        let mut after = 0;
        while last + 1 < editor.line_count() && after < half {
            last += 1;
            after += editor.line(last).map_or(0, |line| line.text.len() + 1);
        }
        let mut text = String::new();
        let mut starts = Vec::with_capacity(last - first + 1);
        for index in first..=last {
            let line = editor.line(index)?;
            starts.push(text.len());
            text.push_str(&line.text);
            if index < last {
                text.push_str(match line.ending.as_str() {
                    "" => "\n",
                    ending => ending,
                });
            }
        }
        let at = |position: text::Position| {
            (first..=last)
                .contains(&position.line)
                .then(|| starts[position.line - first] + position.index)
        };
        let caret = at(cursor.position)?;
        let anchor = cursor.selection.and_then(at).unwrap_or(caret);
        Some(input_method::Surrounding::around(&text, caret, anchor))
    }

    fn input_method<'b>(
        &self,
        state: &'b State<Highlighter>,
        renderer: &Renderer,
        layout: Layout,
    ) -> InputMethod<&'b str> {
        let Some(Focus {
            is_window_focused: true,
            ..
        }) = &state.focus
        else {
            return InputMethod::Disabled;
        };

        let bounds = layout.bounds();
        let internal = self.content.0.borrow_mut();

        let text_bounds = bounds.shrink(self.content_padding());
        let translation = text_bounds.position() - Point::ORIGIN;

        let cursor = match internal.editor.selection() {
            Selection::Caret(position) => position,
            Selection::Range(ranges) => ranges.first().cloned().unwrap_or_default().position(),
        };

        let line_height = self
            .line_height
            .to_absolute(self.text_size.unwrap_or_else(|| renderer.text_size()));

        let position = cursor + translation;

        InputMethod::Enabled {
            cursor: Rectangle::new(position, Size::new(1.0, f32::from(line_height))),
            purpose: input_method::Purpose::Normal,
            preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
        }
    }
}

/// The content of a [`TextEditor`].
pub struct Content<R = crate::Renderer>(RefCell<Internal<R>>)
where
    R: text::Renderer;

struct Internal<R>
where
    R: text::Renderer,
{
    editor: R::Editor,
}

impl<R> Content<R>
where
    R: text::Renderer,
{
    /// Creates an empty [`Content`].
    pub fn new() -> Self {
        Self::with_text("")
    }

    /// Creates a [`Content`] with the given text.
    pub fn with_text(text: &str) -> Self {
        Self(RefCell::new(Internal {
            editor: R::Editor::with_text(text),
        }))
    }

    /// Performs an [`Action`] on the [`Content`], returning the exact
    /// [`EditDelta`] it produced.
    ///
    /// The return value may be ignored. It exists so that a caller keeping its
    /// own copy of the document -- a search index, a save snapshot, an
    /// application model -- learns exactly which bytes changed, in
    /// `O(changed text)`, rather than calling [`Content::text`] after every
    /// keystroke, which walks and copies the whole document (enzUI-ICED #1805).
    ///
    /// Recording that change still costs whatever the caller's store costs:
    /// [`EditDelta::apply`] replays onto a contiguous [`String`] for
    /// convenience and shifts its suffix, so a caller that needs bounded work
    /// per edit applies the same items to a rope or chunked store. What is gone
    /// is the mandatory full flatten, not every copy.
    ///
    /// Actions that change no text (caret motions, selections, clicks, drags,
    /// scrolls) and editing actions that turn out to be no-ops return [`None`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// Message::Edit(action) => {
    ///     if let Some(delta) = self.content.perform(action) {
    ///         let _ = delta.apply(&mut self.mirror);
    ///     }
    /// }
    /// ```
    pub fn perform(&mut self, action: Action) -> Option<EditDelta> {
        let internal = self.0.get_mut();

        internal.editor.perform(action)
    }

    /// Moves the current cursor to reflect the given one.
    pub fn move_to(&mut self, cursor: Cursor) {
        let internal = self.0.get_mut();

        internal.editor.move_to(cursor);
    }

    /// Returns the current cursor position of the [`Content`].
    pub fn cursor(&self) -> Cursor {
        self.0.borrow().editor.cursor()
    }

    /// Where each exclusion of the last layout sat, in the order they were
    /// set, in the coordinates of [`ScrollMetrics::offset`]; see
    /// [`text::Editor::exclusion_bounds`].
    pub fn exclusion_bounds(&self) -> Vec<Option<Rectangle>> {
        self.0.borrow().editor.exclusion_bounds()
    }

    /// Returns the amount of lines of the [`Content`].
    pub fn line_count(&self) -> usize {
        self.0.borrow().editor.line_count()
    }

    /// Returns the editor's real laid-out scroll geometry.
    ///
    /// The offset is absolute in content coordinates and includes wrapped
    /// rows. Calling this may finish shaping uncached lines, but does not edit
    /// the buffer or change its scroll position.
    pub fn scroll_metrics(&self) -> ScrollMetrics {
        self.0.borrow_mut().editor.scroll_metrics()
    }

    /// Metrics resolved before the editor submitted its current text primitive; reading them cannot invalidate that primitive.
    pub fn cached_scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.0.borrow().editor.cached_scroll_metrics()
    }

    /// Visible logical-line heads for the allocated text viewport height, without shaping another paragraph.
    pub fn line_number_rows(
        &self,
        height: f32,
    ) -> Result<Vec<text::editor::gutter::Row>, renderer::presentation::Error> {
        self.0.borrow().editor.line_number_rows(height)
    }

    /// Identifies the editor behind metrics without copying document bytes or retaining editing state.
    pub fn presentation_identity(&self) -> Option<std::sync::Arc<()>> {
        self.0.borrow().editor.presentation_identity()
    }

    /// Returns the text of the line at the given index, if it exists.
    pub fn line(&self, index: usize) -> Option<Line<'_>> {
        let internal = self.0.borrow();
        let line = internal.editor.line(index)?;

        Some(Line {
            text: Cow::Owned(line.text.into_owned()),
            ending: line.ending,
        })
    }

    /// Returns an iterator of the text of the lines in the [`Content`].
    pub fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        (0..)
            .map(|i| self.line(i))
            .take_while(Option::is_some)
            .flatten()
    }

    /// Returns the text of the [`Content`].
    pub fn text(&self) -> String {
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

    /// Returns the selected text of the [`Content`].
    pub fn selection(&self) -> Option<String> {
        self.0.borrow().editor.copy()
    }

    /// Returns the logical bounds of the current selection without copying any
    /// selected text or producing paint rectangles.
    pub fn selection_range(&self) -> Option<ops::Range<Position>> {
        self.0.borrow().editor.selection_range()
    }

    /// Performs a widget-owned pointer action through interior mutability.
    ///
    /// Kept private so application-owned edits continue to require
    /// `&mut Content`; only the opt-in redraw-time drag path may mutate through the
    /// shared reference held by the widget.
    fn perform_pointer_action(&self, action: Action) {
        let _ = self.0.borrow_mut().editor.perform(action);
    }

    /// Returns the laid-out rectangles covering each `(start, end)` range,
    /// in the editor's text space (its top-left, with scrolling applied), as
    /// [`Editor::highlight_ranges`](text::Editor::highlight_ranges) does.
    /// Empty before the [`Content`] is first laid out.
    pub fn highlight_ranges(&self, ranges: &[(Position, Position)]) -> Vec<Vec<Rectangle>> {
        self.0.borrow().editor.highlight_ranges(ranges)
    }

    /// Returns the kind of [`LineEnding`] used for separating lines in the [`Content`].
    pub fn line_ending(&self) -> Option<LineEnding> {
        Some(self.line(0)?.ending)
    }

    /// Returns whether or not the the [`Content`] is empty.
    pub fn is_empty(&self) -> bool {
        self.0.borrow().editor.is_empty()
    }
}

impl<Renderer> Clone for Content<Renderer>
where
    Renderer: text::Renderer,
{
    fn clone(&self) -> Self {
        Self::with_text(&self.text())
    }
}

impl<Renderer> Default for Content<Renderer>
where
    Renderer: text::Renderer,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Renderer> fmt::Debug for Content<Renderer>
where
    Renderer: text::Renderer,
    Renderer::Editor: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let internal = self.0.borrow();

        f.debug_struct("Content")
            .field("editor", &internal.editor)
            .finish()
    }
}

/// The state of a [`TextEditor`].
#[derive(Debug)]
pub struct State<Highlighter: text::Parser> {
    focus: Option<Focus>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    drag_click: Option<mouse::click::Kind>,
    // enzUI-ICED #1802 — late-latch drag coalescing.
    //
    // Within one `UserInterface::update` batch every queued `CursorMoved`
    // reaches the widget with the SAME latest `cursor` (see
    // `runtime/src/user_interface.rs`), so `cursor.position_in(bounds)`
    // resolves to one identical local point for all of them. Before this
    // field, the widget published N identical `Action::Drag(same_point)`
    // per batch, each triggering a full `graphics::text::Editor::perform`
    // (shape_until_cursor + selection geometry) — visible drag lag.
    //
    // Design: every `CursorMoved` during an active drag OVERWRITES this
    // endpoint and requests a redraw but publishes nothing. On the next
    // `RedrawRequested` (immediately before paint) the widget refreshes the
    // stored point from the redraw's current window cursor, then flushes one
    // selection update: the default publishes `Action::Drag(latest)`, while
    // `on_drag_update` performs it here and publishes only a notification.
    // This guarantees:
    //
    //  1. At most one `perform(Drag)` per painted frame — bounded cost.
    //  2. The painted frame always shows the NEWEST endpoint, not the
    //     oldest — no trailing selection even when frame callbacks
    //     throttle `RedrawRequested` behind multiple input batches.
    //  3. `ButtonReleased` flushes any pending endpoint before ending
    //     the drag, so the final position is never lost.
    //  4. Edge autoscroll: even without a fresh `CursorMoved`, a redraw
    //     that finds a live drag and an out-of-inner-bounds cursor
    //     re-arms `pending_drag` from that cursor and requests another
    //     redraw, so cosmic-text's `shape_until_scroll` advances the
    //     viewport on every frame the pointer stays past the edge.
    //  5. Focus loss ends the drag: no release follows it, so both this
    //     endpoint and `drag_click` are cleared and nothing is published
    //     or re-armed afterwards.
    //
    // Coordinate-space sound: uses only `cursor.position_from(bounds.position())`,
    // which a translating parent (scrollable) shifts consistently.
    pending_drag: Option<Point>,
    // enzICED: a press inside the selection, waiting to see whether it
    // becomes a drag of the selected text; see `text::selection_drag`.
    text_drag: Option<TextDrag>,
    // enzUI-ICED #1862 — held-edge autoscroll loop termination.
    //
    // The re-arm block above republishes an out-of-vertical-bounds pointer
    // through `pending_drag` on every redraw while the button is held. At
    // BOF/EOF cosmic-text clamps the scroll and the graphics editor's `Drag`
    // reveals nothing further, but the widget kept requesting another frame
    // unconditionally: the 60 fps redraw/shape/selection loop then spins
    // forever while the pointer sits past the edge.
    //
    // Compare an observable "did the drag advance" snapshot -- scroll offset
    // plus selection endpoint -- across held-edge flushes. If the state is
    // identical to the one recorded at the last held-edge flush, the drag
    // made no progress and the next frame is not scheduled. A fresh
    // `CursorMoved` (direction reversal, or the pointer returning inside),
    // `ButtonReleased`, focus loss or a later same-point continuation after a
    // layout/content change all reset the snapshot and let continuation
    // resume.
    //
    // The snapshot must survive both the default and the direct drag paths.
    // Direct mode performs synchronously and the post-perform state is what
    // this frame stores; default mode publishes an `Action::Drag` for the
    // application to perform after this batch, so the snapshot is the state
    // observed at publish time and the *next* frame's snapshot reflects
    // whether the application's queued perform advanced anything.
    edge_autoscroll_last: Option<(f32, Position)>,
    // The text last offered as the primary selection, so an unchanged
    // selection is not offered again on every event.
    primary_offered: Option<String>,
    // A middle click is waiting for the primary selection to paste.
    pasting_primary: bool,
    partial_scroll: f32,
    last_theme: RefCell<Option<ThemeSnapshot>>,
    highlighter: RefCell<Highlighter>,
    highlighter_settings: Highlighter::Settings,
    highlighter_format_address: usize,
}

#[derive(Debug)]
struct ThemeSnapshot {
    name: String,
    mode: theme::Mode,
    base: theme::Style,
    seed: Option<theme::palette::Seed>,
    palette: Option<theme::palette::Palette>,
}

#[derive(Debug, Clone)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

impl Focus {
    fn now() -> Self {
        let now = Instant::now();

        Self {
            updated_at: now,
            now,
            is_window_focused: true,
        }
    }

    /// Whether the caret shows now. It blinks with the desktop's
    /// [`preferences`](crate::core::preferences) and is solid once they say
    /// it has settled (blinking off, reduced motion, or the blink timeout).
    fn is_cursor_visible(&self) -> bool {
        let preferences = crate::core::preferences::get();
        let elapsed = self.now - self.updated_at;
        let phase = preferences.blink_phase().as_millis().max(1);
        self.is_window_focused
            && (preferences.caret_settled(elapsed)
                || (elapsed.as_millis() / phase).is_multiple_of(2))
    }

    /// When the caret next changes: its next blink edge, or the moment it
    /// stops blinking. `None` once it is solid.
    fn next_blink_redraw(&self) -> Option<Instant> {
        let preferences = crate::core::preferences::get();
        let elapsed = self.now - self.updated_at;
        if preferences.caret_settled(elapsed) {
            return None;
        }
        let phase = preferences.blink_phase().as_millis().max(1);
        let elapsed = elapsed.as_millis();
        let until_edge = phase - elapsed % phase;
        let until_solid = preferences.cursor_blink_timeout.as_millis() - elapsed;
        Some(self.now + Duration::from_millis(until_edge.min(until_solid) as u64))
    }
}

impl<Highlighter: text::Parser> State<Highlighter> {
    /// Returns whether the [`TextEditor`] is currently focused or not.
    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }
}

impl<Highlighter: text::Parser> operation::Focusable for State<Highlighter> {
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

impl<Highlighter, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TextEditor<'_, Highlighter, Message, Theme, Renderer>
where
    Highlighter: text::Parser,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State<Highlighter>>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::interactive(State {
            focus: None,
            preedit: None,
            last_click: None,
            drag_click: None,
            pending_drag: None,
            text_drag: None,
            edge_autoscroll_last: None,
            primary_offered: None,
            pasting_primary: false,
            partial_scroll: 0.0,
            last_theme: RefCell::default(),
            highlighter: RefCell::new(Highlighter::new(&self.highlighter_settings)),
            highlighter_settings: self.highlighter_settings.clone(),
            highlighter_format_address: self.highlighter_format as usize,
        })
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut widget::Tree, renderer: &Renderer, limits: &layout::Limits) {
        let mut internal = self.content.0.borrow_mut();
        let state = tree.state.downcast_mut::<State<Highlighter>>();

        if state.highlighter_format_address != self.highlighter_format as usize {
            state.highlighter.borrow_mut().change_line(0);

            state.highlighter_format_address = self.highlighter_format as usize;
        }

        if state.highlighter_settings != self.highlighter_settings {
            state
                .highlighter
                .borrow_mut()
                .update(&self.highlighter_settings);

            state.highlighter_settings = self.highlighter_settings.clone();
        }

        let parent_limits = *limits;
        let limits = limits.width(self.width).height(self.height);

        // The retained editor outlives the element, so every layout states the
        // whole document's alignment including its absence; the editor answers
        // an absence it never installed without touching a line. Before
        // `update`, so a line whose alignment changed reshapes once with its
        // bounds.
        match &self.line_alignment {
            Some(alignment) => internal
                .editor
                .set_line_alignment(&|index| alignment(index)),
            None => internal.editor.clear_line_alignment(),
        }
        internal.editor.set_exclusions(&self.exclusions);

        internal.editor.update(
            limits.shrink(self.content_padding()).max(),
            self.font.unwrap_or_else(|| renderer.font()),
            self.text_size.unwrap_or_else(|| renderer.text_size()),
            self.line_height,
            self.wrapping,
            // This widget aligns per line (`line_alignment`), never the whole
            // editor at once.
            text::Alignment::Default,
            renderer.hint_factor(),
            state.highlighter.borrow_mut().deref_mut(),
        );

        let minimum = if matches!(self.height, Length::Shrink | Length::Fit) {
            internal.editor.min_bounds()
        } else {
            Size::ZERO
        };
        layout::Node::new(layout::editor_size(
            &limits,
            self.height,
            self.content_padding(),
            minimum,
        ))
        .with_editor_presentation(
            parent_limits,
            Size::new(self.width, self.height),
            self.content_padding(),
        )
        .apply(tree);
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Some(on_edit) = self.on_edit.as_ref() else {
            return;
        };

        let state = tree.state.downcast_mut::<State<Highlighter>>();
        let is_redraw = matches!(event, Event::Window(window::Event::RedrawRequested(_now)),);

        // A finished selection becomes the primary selection, the text a
        // middle click pastes elsewhere. The content reflects the actions the
        // application applied after the previous event.
        if state.focus.is_some() && state.drag_click.is_none() {
            match self.content.selection().filter(|text| !text.is_empty()) {
                Some(text) if state.primary_offered.as_deref() != Some(text.as_str()) => {
                    shell.write_primary(clipboard::Content::Text(text.clone()));
                    state.primary_offered = Some(text);
                }
                Some(_) => {}
                None => state.primary_offered = None,
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
                if clipboard::primary_paste_enabled() =>
            {
                if let Some(position) = cursor.position_in(layout.bounds()) {
                    let padding = self.content_padding();
                    let position = position - Vector::new(padding.left, padding.top);
                    state.focus = Some(Focus::now());
                    state.pasting_primary = true;
                    // Paste where the pointer is: move the caret there first.
                    shell.publish(on_edit(Action::Click(position, mouse::click::Kind::Single)));
                    shell.read_primary(clipboard::Kind::Text);
                    shell.capture_event();
                    return;
                }
            }
            Event::Clipboard(clipboard::Event::ReadPrimary(result)) => {
                if std::mem::take(&mut state.pasting_primary)
                    && let Ok(content) = result
                    && let clipboard::Content::Text(text) = content.as_ref()
                {
                    shell.publish(on_edit(Action::Edit(Edit::Paste(Arc::new(text.clone())))));
                    shell.capture_event();
                }
                return;
            }
            Event::Window(window::Event::Unfocused) => {
                if let Some(focus) = &mut state.focus {
                    focus.is_window_focused = false;
                }

                // enzUI-ICED #1802: the drag is over and its release will
                // never be delivered here. Both halves of the drag state have
                // to go: a retained `pending_drag` would still be refreshed
                // and published by the next `RedrawRequested` below, and a
                // retained `drag_click` would let later pointer motion latch a
                // fresh endpoint and keep extending the selection with no
                // button held.
                state.pending_drag = None;
                state.drag_click = None;
                // enzUI-ICED #1862: the hold is cancelled; the snapshot
                // must not survive to gate a later, unrelated hold.
                state.edge_autoscroll_last = None;
            }
            Event::Window(window::Event::Focused) => {
                if let Some(focus) = &mut state.focus {
                    focus.is_window_focused = true;
                    focus.updated_at = Instant::now();

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                // enzUI-ICED #1802: flush the late-latched drag endpoint
                // immediately before paint. A raw CursorMoved can update
                // winit's window cursor after the widget latched endpoint A
                // but before AboutToWait dispatches endpoint B to widgets. If
                // RedrawRequested overtakes that dispatch, publishing only A
                // paints the exact previous-waypoint frame seen in the
                // hold-zero product captures. `cursor` is the current window
                // cursor supplied to this redraw, so refresh the pending point
                // from it before publishing.
                //
                // Use `position_from`, not `position_in`: an active drag may
                // legitimately sit beyond an edge while autoscrolling. The
                // graphics editor owns the conservative out-of-bounds path.
                //
                // `pending_drag` is the whole precondition, and it is the only
                // one that can fail: `Update::from_event` produces `Drag` only
                // for `click::Kind::Single`, so nothing else can ever latch an
                // endpoint. A click that never moved, and any frame after the
                // release flush, therefore have nothing pending and must
                // publish nothing -- otherwise every blink frame would drag
                // the caret to the current cursor.
                if state.pending_drag.is_some()
                    && let Some(position) = cursor.position_from(layout.bounds().position())
                {
                    state.pending_drag = Some(
                        position
                            - Vector::new(self.content_padding().left, self.content_padding().top),
                    );
                }

                // enzUI-ICED #1802: autoscroll continuation. A pointer that
                // sits stationary past the editor's inner text bounds emits
                // no further `CursorMoved`, so `pending_drag` is empty after
                // the flush; cosmic-text's `shape_until_scroll` then advances
                // the viewport exactly ONCE and stops until a fresh event
                // arrives. Re-arm the pending endpoint from the current
                // cursor whenever a live single-button drag has no pending
                // point AND the local position is outside the inner text
                // area VERTICALLY, and request another redraw so the next
                // frame flushes again. `drag_click` becomes `None` on
                // release, so this loop always terminates; a pointer that
                // returns in-bounds takes the normal `Update::Drag` path
                // from the next `CursorMoved`.
                //
                // enzUI-ICED #1862: only VERTICAL overshoot re-arms. This
                // editor does not autoscroll horizontally, so a pointer that
                // is only past the left/right edge cannot advance the drag;
                // arming a redraw for it would spin the 60 fps loop for no
                // work. A `CursorMoved` outside vertical bounds still stores
                // its point through `Update::Drag` (the whole payload is
                // needed for the endpoint), and the flush below handles
                // its edge-vs-inside classification without re-consulting
                // this block.
                //
                // The out-of-bounds check consults `editor.bounds()` -- the
                // exact inner text size `Editor::perform` uses when it
                // detects overshoot -- instead of `layout.bounds() - padding`.
                // A `Length::Shrink` (or `Fit`) editor's layout node is
                // `.expand(padding.y())` past the shrunk inner size fed to
                // `Editor::update`, so `layout.bounds() - padding` overstates
                // the true inner rectangle by `padding.y()`. That gap let a
                // sub-row overshoot below the real editor bottom sit inside
                // the widget's (wrong) inner rectangle: the graphics editor
                // performed one autoscroll step from the initial
                // `Update::Drag`, but the re-arm block refused to relaunch
                // the loop because it thought the pointer was still in
                // bounds. Anchoring both layers on the same size closes that
                // 10 px dead zone for Shrink layouts and remains identical
                // for `Fill`/`Fixed` layouts, where `editor.bounds()` and
                // `layout.bounds() - padding` already agree.
                let editor_bounds = self.content.0.borrow().editor.bounds();
                if state.pending_drag.is_none()
                    && matches!(state.drag_click, Some(mouse::click::Kind::Single))
                    && let Some(cursor_local) = cursor.position_from(layout.bounds().position())
                {
                    let local = cursor_local
                        - Vector::new(self.content_padding().left, self.content_padding().top);
                    if local.y < 0.0 || local.y > editor_bounds.height {
                        state.pending_drag = Some(local);
                    }
                }

                if let Some(position) = state.pending_drag.take() {
                    // enzUI-ICED #1862: capture the observable state *before*
                    // this frame's drag. In direct mode the `perform` below
                    // runs synchronously, so we can compare pre and post in
                    // the same frame. In default mode the app performs the
                    // published `Action::Drag` after this batch, so the
                    // "advance" signal comes from comparing this frame's
                    // pre-state to the *previous* held-edge flush's stored
                    // snapshot (which the app has by now performed against).
                    // Either way, if the state did not move between
                    // consecutive held-edge flushes, BOF/EOF has clamped and
                    // the next frame must not be scheduled.
                    let pre_scroll = self.content.scroll_metrics().offset.y;
                    let pre_cursor = self.content.cursor().position;

                    if let Some(on_drag_update) = self.on_drag_update.as_ref() {
                        // The selection is now part of THIS frame before the
                        // application message queue is drained. This avoids an
                        // extra Elm round-trip at the exact input/redraw race
                        // where a fast pointer otherwise paints the previous
                        // endpoint. The callback is notification only.
                        self.content.perform_pointer_action(Action::Drag(position));
                        shell.publish(on_drag_update(self.content.cursor()));
                    } else {
                        shell.publish(on_edit(Action::Drag(position)));
                    }

                    // enzUI-ICED #1802/#1862: keep the autoscroll loop alive
                    // ONLY when the drag is still advancing. Position is in
                    // the widget's inner-text local space; outside the inner
                    // rectangle VERTICALLY means cosmic-text just took the
                    // out-of-bounds `shape_until_scroll` path, and another
                    // frame is needed for the next scroll step. Horizontal-
                    // only overshoot never triggers this editor's autoscroll,
                    // so it must not schedule a redraw.
                    //
                    // At BOF/EOF the scroll clamps and the endpoint stops
                    // moving even though the pointer is still past the edge.
                    // Detect that by comparing scroll offset + selection
                    // endpoint against the pre-perform snapshot (direct
                    // mode) and against the previous held-edge flush's
                    // recorded state (default mode). If both agree, the
                    // drag made no progress this frame and no progress
                    // will be made on any further frame until the pointer
                    // event stream changes -- stop.
                    if matches!(state.drag_click, Some(mouse::click::Kind::Single))
                        && (position.y < 0.0 || position.y > editor_bounds.height)
                    {
                        let post_scroll = self.content.scroll_metrics().offset.y;
                        let post_cursor = self.content.cursor().position;
                        let now_snapshot = (post_scroll, post_cursor);

                        // Direct mode: same-frame advance signal.
                        let advanced_this_frame =
                            post_scroll != pre_scroll || post_cursor != pre_cursor;
                        // Default mode: cross-frame advance signal against
                        // the previous held-edge flush. `None` means this
                        // is the first held-edge flush of the current
                        // hold; always schedule so the perform (in either
                        // mode) gets at least one chance to advance.
                        let advanced_since_last_flush = state
                            .edge_autoscroll_last
                            .map(|prev| prev != (pre_scroll, pre_cursor))
                            .unwrap_or(true);

                        if advanced_this_frame || advanced_since_last_flush {
                            shell.request_redraw();
                        }
                        state.edge_autoscroll_last = Some(now_snapshot);
                    } else {
                        // In-bounds flush (a normal `CursorMoved` inside the
                        // viewport): the next held-edge continuation, if any,
                        // starts a fresh snapshot chain so a later reversal
                        // and re-crossing of an edge is not measured against
                        // stale state from a previous edge visit.
                        state.edge_autoscroll_last = None;
                    }
                }

                if let Some(focus) = &mut state.focus
                    && focus.is_window_focused
                {
                    focus.now = *now;

                    if let Some(at) = focus.next_blink_redraw() {
                        shell.request_redraw_at(at);
                    }
                }
            }
            Event::Clipboard(clipboard::Event::Read(Ok(content))) => {
                if let clipboard::Content::Text(text) = content.as_ref()
                    && let Some(focus) = &mut state.focus
                    && focus.is_window_focused
                {
                    shell.publish(on_edit(Action::Edit(Edit::Paste(Arc::new(text.clone())))));
                }
            }
            _ => {}
        }

        if let Some(update) = Update::from_event(
            event,
            state,
            layout.bounds(),
            self.content_padding(),
            cursor,
            self.key_binding.as_deref(),
        ) {
            match update {
                Update::Click(click)
                    if matches!(click.kind(), mouse::click::Kind::Single)
                        && crate::core::text::selection_drag::is_available()
                        && self.selection_contains(click.position()) =>
                {
                    // Wait: a drag carries the selection away, a release
                    // places the cursor.
                    state.focus = Some(Focus::now());
                    state.last_click = Some(click);
                    state.drag_click = None;
                    state.pending_drag = None;
                    state.text_drag = Some(TextDrag::Pending {
                        origin: click.position(),
                    });
                    shell.capture_event();
                }
                Update::TextDrag(position) => {
                    match state.text_drag {
                        Some(TextDrag::Pending { origin })
                            if origin.distance(position)
                                >= crate::core::text::selection_drag::THRESHOLD =>
                        {
                            let started = self.content.selection().is_some_and(|text| {
                                crate::core::text::selection_drag::begin(
                                    crate::core::text::selection_drag::SelectionDrag { text },
                                )
                            });
                            if started {
                                state.text_drag = Some(TextDrag::Carrying);
                            } else {
                                // Declined: select from the press as usual.
                                state.text_drag = None;
                                state.drag_click = Some(mouse::click::Kind::Single);
                                state.pending_drag = Some(position);
                                shell.publish(on_edit(Action::Click(
                                    origin,
                                    mouse::click::Kind::Single,
                                )));
                                shell.request_redraw();
                            }
                        }
                        _ => {}
                    }
                    shell.capture_event();
                }
                Update::Click(click) => {
                    let action = match click.kind() {
                        mouse::click::Kind::Single => {
                            Action::Click(click.position(), mouse::click::Kind::Single)
                        }
                        mouse::click::Kind::Double => Action::SelectWord,
                        mouse::click::Kind::Triple => Action::SelectLine,
                    };

                    state.focus = Some(Focus::now());
                    state.last_click = Some(click);
                    state.drag_click = Some(click.kind());
                    // enzUI-ICED #1802: a fresh click starts a fresh drag
                    // train; discard any stale pending endpoint.
                    state.pending_drag = None;
                    // enzUI-ICED #1862: the previous drag's autoscroll
                    // snapshot is meaningless for this new hold.
                    state.edge_autoscroll_last = None;

                    shell.publish(on_edit(action));
                    shell.capture_event();
                }
                Update::Drag(position) => {
                    // enzUI-ICED #1802: overwrite the pending endpoint.
                    // Every `CursorMoved` in the batch resolves to the
                    // same `cursor.position_from(..)` so this is
                    // idempotent within a batch, but across throttled
                    // batches it always retains the NEWEST point.
                    // Nothing is published here; the `RedrawRequested`
                    // handler above flushes the latest value once,
                    // immediately before paint.
                    state.pending_drag = Some(position);
                    shell.request_redraw();
                }
                Update::Release if state.text_drag.is_some() => {
                    // A press inside the selection that never moved is a click.
                    if let Some(TextDrag::Pending { origin }) = state.text_drag.take() {
                        shell.publish(on_edit(Action::Click(origin, mouse::click::Kind::Single)));
                    }
                }
                Update::Release => {
                    // enzUI-ICED #1802: flush any pending endpoint so
                    // the final drag position is applied before the
                    // drag ends.  Without this, a `CursorMoved` +
                    // `ButtonReleased` batch would lose the endpoint.
                    if let Some(position) = state.pending_drag.take() {
                        if let Some(on_drag_update) = self.on_drag_update.as_ref() {
                            self.content.perform_pointer_action(Action::Drag(position));
                            shell.publish(on_drag_update(self.content.cursor()));
                        } else {
                            shell.publish(on_edit(Action::Drag(position)));
                        }
                    }
                    state.drag_click = None;
                    // enzUI-ICED #1862: end of hold, snapshot resets so a
                    // later hold starts a fresh advance chain.
                    state.edge_autoscroll_last = None;
                }
                Update::Scroll(lines) => {
                    let bounds = self.content.0.borrow().editor.bounds();

                    if bounds.height >= i32::MAX as f32 {
                        return;
                    }

                    // At the edge it is asked to scroll past -- content that
                    // fits is at both -- the wheel belongs to the scroll view
                    // around the editor.
                    let metrics = self.content.0.borrow_mut().editor.scroll_metrics();
                    let room = metrics.content.height - metrics.viewport.height;
                    let at_top = metrics.offset.y <= 0.5;
                    let at_bottom = metrics.offset.y >= room - 0.5;
                    if (lines > 0.0 && at_bottom) || (lines < 0.0 && at_top) {
                        state.partial_scroll = 0.0;
                        return;
                    }

                    let lines = lines + state.partial_scroll;
                    state.partial_scroll = lines.fract();

                    shell.publish(on_edit(Action::Scroll {
                        lines: lines as i32,
                    }));
                    shell.capture_event();
                }
                Update::InputMethod(update) => match update {
                    Ime::Toggle(is_open) => {
                        state.preedit = is_open.then(input_method::Preedit::new);

                        shell.request_redraw();
                    }
                    Ime::Preedit { content, selection } => {
                        state.preedit = Some(input_method::Preedit {
                            content,
                            selection,
                            text_size: self.text_size,
                        });

                        shell.request_redraw();
                    }
                    Ime::Commit(text) => {
                        shell.publish(on_edit(Action::Edit(Edit::Paste(Arc::new(text)))));
                    }
                },
                Update::Binding(binding) => {
                    fn apply_binding<H: text::Parser, R: text::Renderer, Message>(
                        binding: Binding<Message>,
                        content: &Content<R>,
                        state: &mut State<H>,
                        on_edit: &dyn Fn(Action) -> Message,
                        shell: &mut Shell<'_, Message>,
                    ) {
                        let mut publish = |action| shell.publish(on_edit(action));

                        match binding {
                            Binding::Unfocus => {
                                state.focus = None;
                                state.drag_click = None;
                                state.pending_drag = None;
                                state.edge_autoscroll_last = None;
                            }
                            Binding::Copy => {
                                if let Some(selection) = content.selection() {
                                    shell.write_clipboard(clipboard::Content::Text(selection));
                                }
                            }
                            Binding::Cut => {
                                if let Some(selection) = content.selection() {
                                    shell.write_clipboard(clipboard::Content::Text(selection));
                                    shell.publish(on_edit(Action::Edit(Edit::Delete)));
                                }
                            }
                            Binding::Paste => {
                                // TODO: Debounce (?)
                                shell.read_clipboard(clipboard::Kind::Text);
                            }
                            Binding::Undo => {
                                publish(Action::Edit(Edit::Undo));
                            }
                            Binding::Redo => {
                                publish(Action::Edit(Edit::Redo));
                            }
                            Binding::Move(motion) => {
                                publish(Action::Move(motion));
                            }
                            Binding::Select(motion) => {
                                publish(Action::Select(motion));
                            }
                            Binding::SelectWord => {
                                publish(Action::SelectWord);
                            }
                            Binding::SelectLine => {
                                publish(Action::SelectLine);
                            }
                            Binding::SelectAll => {
                                publish(Action::SelectAll);
                            }
                            Binding::Insert(c) => {
                                publish(Action::Edit(Edit::Insert(c)));
                            }
                            Binding::Enter => {
                                publish(Action::Edit(Edit::Enter));
                            }
                            Binding::Backspace => {
                                publish(Action::Edit(Edit::Backspace));
                            }
                            Binding::Delete => {
                                publish(Action::Edit(Edit::Delete));
                            }
                            Binding::Sequence(sequence) => {
                                for binding in sequence {
                                    apply_binding(binding, content, state, on_edit, shell);
                                }
                            }
                            Binding::Custom(message) => {
                                shell.publish(message);
                            }
                        }
                    }

                    if !matches!(binding, Binding::Unfocus) {
                        shell.capture_event();
                    }

                    apply_binding(binding, self.content, state, on_edit, shell);

                    if let Some(focus) = &mut state.focus {
                        focus.updated_at = Instant::now();
                    }
                }
            }
        }

        let status = {
            let is_disabled = self.on_edit.is_none();
            let is_hovered = cursor.is_over(layout.bounds());

            if is_disabled {
                Status::Disabled
            } else if state.focus.is_some() {
                Status::Focused { is_hovered }
            } else if is_hovered {
                Status::Hovered
            } else {
                Status::Active
            }
        };

        if is_redraw {
            self.last_status = Some(status);

            let input_method = self.input_method(state, renderer, layout);
            let enabled = matches!(input_method, InputMethod::Enabled { .. });
            match enabled.then(|| self.surrounding()).flatten() {
                Some(surrounding) => {
                    shell.request_input_method_with_surrounding(&input_method, surrounding);
                }
                None => shell.request_input_method(&input_method),
            }
        } else if self
            .last_status
            .is_some_and(|last_status| status != last_status)
        {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _defaults: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        let mut internal = self.content.0.borrow_mut();
        let state = tree.state.downcast_ref::<State<Highlighter>>();

        let font = self.font.unwrap_or_else(|| renderer.font());

        let name = theme.name();
        let mode = theme.mode();
        let base = theme.base();
        let seed = theme.seed();
        let palette = theme.palette();

        if state.last_theme.borrow().as_ref().is_none_or(|last| {
            last.name != name
                || last.mode != mode
                || last.base != base
                || last.seed != seed
                || last.palette != palette
        }) {
            state.highlighter.borrow_mut().change_line(0);
            let _ = state.last_theme.replace(Some(ThemeSnapshot {
                name: name.to_owned(),
                mode,
                base,
                seed,
                palette,
            }));
        }

        let node = tree.editor_presentation_node();
        if let Some((node, height)) = node.and_then(|node| {
            renderer
                .editor_presentation_height(node)
                .map(|height| (node, height))
        }) && internal
            .editor
            .prepare_presentation(
                font,
                height,
                state.highlighter.borrow_mut().deref_mut(),
                |highlight| (self.highlighter_format)(&highlight, theme),
            )
            .is_err()
        {
            renderer.reject_layout_paint(node);
        }

        internal.editor.highlight(
            font,
            state.highlighter.borrow_mut().deref_mut(),
            |highlight| (self.highlighter_format)(&highlight, theme),
        );

        let style = theme.style(&self.class, self.last_status.unwrap_or(Status::Active));
        // Resolve before publishing weak text; post-draw readers must not replace its backing allocation.
        let _ = internal.editor.scroll_metrics();

        let paint = text::editor::paint::Paint {
            bounds,
            padding: self.content_padding(),
            gutter: self.gutter,
            background: style.background,
            border: style.border,
            value: style.value,
            placeholder_color: style.placeholder,
            selection_color: style.selection,
            placeholder: self
                .placeholder
                .as_ref()
                .map(|value| std::borrow::Cow::Borrowed(value.as_ref())),
            font,
            size: self.text_size.unwrap_or_else(|| renderer.text_size()),
            line_height: self.line_height,
            wrapping: self.wrapping,
            hint_factor: renderer.hint_factor(),
            focused: state.focus.is_some(),
            caret_visible: state.focus.as_ref().is_some_and(Focus::is_cursor_visible),
            caret_width: crate::text_input::caret_width(renderer.hint_factor()),
            highlights: std::borrow::Cow::Borrowed(&self.background_highlights),
            retain_selection: self.retain_selection,
        };
        renderer.draw_editor_view(node, &internal.editor, &paint);
    }

    fn mouse_interaction(
        &self,
        _tree: &widget::Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let is_disabled = self.on_edit.is_none();

        if cursor.is_over(layout.bounds()) {
            if is_disabled {
                mouse::Interaction::NotAllowed
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::default()
        }
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout,
        _viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        if operation.is_native_reconciliation() {
            if let Some(node) = tree.editor_presentation_node()
                && operation.needs_editor_reconciliation(node)
            {
                let viewport = layout.bounds().shrink(self.content_padding()).size();
                let mut internal = self.content.0.borrow_mut();
                let success =
                    renderer.reconcile_presented_editor(node, &mut internal.editor, viewport);
                operation.editor_reconciled(node, success);
            }
            return;
        }
        let state = tree.state.downcast_mut::<State<Highlighter>>();

        operation.focusable(self.id.as_ref(), layout.bounds(), state);
        // enzICED: cursor operations and assistive technology reach the
        // editor's text and selection.
        operation.text_input(
            self.id.as_ref(),
            layout.bounds(),
            &mut self.content.0.borrow_mut().editor,
        );
        if let Some(gutter) = self.gutter {
            let bounds = gutter.bounds(layout.bounds(), self.content_padding());
            let mut internal = self.content.0.borrow_mut();
            let scroll = internal.editor.scroll_metrics();
            if let Ok(rows) = internal.editor.line_number_rows(bounds.height) {
                let mut view = text::editor::gutter::View {
                    bounds,
                    rows,
                    scroll,
                    gutter,
                };
                operation.custom(self.id.as_ref(), bounds, &mut view);
            }
        }
    }
}

impl<'a, Highlighter, Message, Theme, Renderer>
    From<TextEditor<'a, Highlighter, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Highlighter: text::Parser,
    Message: 'a,
    Theme: Catalog + 'a,
    Renderer: text::Renderer,
{
    fn from(text_editor: TextEditor<'a, Highlighter, Message, Theme, Renderer>) -> Self {
        Self::new(text_editor)
    }
}

/// A binding to an action in the [`TextEditor`].
#[derive(Debug, Clone, PartialEq)]
pub enum Binding<Message> {
    /// Unfocus the [`TextEditor`].
    Unfocus,
    /// Copy the selection of the [`TextEditor`].
    Copy,
    /// Cut the selection of the [`TextEditor`].
    Cut,
    /// Paste the clipboard contents in the [`TextEditor`].
    Paste,
    /// Undo the last change peformed in the [`TextEditor`].
    Undo,
    /// Redo the last change undone in the [`TextEditor`].
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
    /// Delete the next character.
    Delete,
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
    /// The current [`Status`] of the [`TextEditor`].
    pub status: Status,
}

impl<Message> Binding<Message> {
    /// Returns the default [`Binding`] for the given key press.
    ///
    /// Printable Control or Logo chords do not fall through to insertion.
    /// Control+Alt remains available for AltGr text instead of editing shortcuts.
    pub fn from_key_press(event: KeyPress) -> Option<Self> {
        let KeyPress {
            key,
            modified_key,
            physical_key,
            modifiers,
            text,
            status,
        } = event;

        if !matches!(status, Status::Focused { .. }) {
            return None;
        }

        let command = modifiers.command() && !modifiers.alt();
        let combination = match key.to_latin(physical_key) {
            Some('c') if command => Some(Self::Copy),
            Some('x') if command => Some(Self::Cut),
            Some('v') if command => Some(Self::Paste),
            Some('a') if command => Some(Self::SelectAll),
            Some('z') if command => Some(Self::Undo),
            Some('y') if command => Some(Self::Redo),
            _ => None,
        };

        if let Some(binding) = combination {
            return Some(binding);
        }

        #[cfg(target_os = "macos")]
        let modified_key = convert_macos_shortcut(&key, modifiers).unwrap_or(modified_key);

        match modified_key.as_ref() {
            keyboard::Key::Named(key::Named::Enter) => Some(Self::Enter),
            keyboard::Key::Named(key::Named::Backspace) => Some(Self::Backspace),
            keyboard::Key::Named(key::Named::Delete)
                if text.is_none() || text.as_deref() == Some("\u{7f}") =>
            {
                Some(Self::Delete)
            }
            keyboard::Key::Named(key::Named::Escape) => Some(Self::Unfocus),
            _ => {
                if let Some(text) = text {
                    if modifiers.logo() || (modifiers.control() && !modifiers.alt()) {
                        return None;
                    }
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

/// A press inside the selection (enzICED).
#[derive(Debug, Clone, Copy, PartialEq)]
enum TextDrag {
    /// Pressed at `origin`, in the editor's text space; not moved far yet.
    Pending { origin: Point },
    /// The platform carries the selection.
    Carrying,
}

enum Update<Message> {
    Click(mouse::Click),
    Drag(Point),
    /// The pointer moved while a press inside the selection waits.
    TextDrag(Point),
    Release,
    Scroll(f32),
    InputMethod(Ime),
    Binding(Binding<Message>),
}

enum Ime {
    Toggle(bool),
    Preedit {
        content: String,
        selection: Option<ops::Range<usize>>,
    },
    Commit(String),
}

impl<Message> Update<Message> {
    fn from_event<H: Parser>(
        event: &Event,
        state: &State<H>,
        bounds: Rectangle,
        padding: Padding,
        cursor: mouse::Cursor,
        key_binding: Option<&dyn Fn(KeyPress) -> Option<Binding<Message>>>,
    ) -> Option<Self> {
        let binding = |binding| Some(Update::Binding(binding));

        // Upstream iced #3442: a finger drives the editor exactly like the
        // left button. The runtime already moves the cursor with the finger,
        // so press, drag and release take the mouse paths below.
        let touch = match event {
            Event::Touch(touch::Event::FingerPressed { .. }) => Some(Event::Mouse(
                mouse::Event::ButtonPressed(mouse::Button::Left),
            )),
            Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                Some(Event::Mouse(mouse::Event::CursorMoved {
                    position: *position,
                }))
            }
            Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }) => {
                Some(Event::Mouse(mouse::Event::ButtonReleased(
                    mouse::Button::Left,
                )))
            }
            _ => None,
        };
        let event = touch.as_ref().unwrap_or(event);

        match event {
            Event::Mouse(event) => match event {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    if let Some(cursor_position) = cursor.position_in(bounds) {
                        let cursor_position =
                            cursor_position - Vector::new(padding.left, padding.top);

                        let click = mouse::Click::new(
                            cursor_position,
                            mouse::Button::Left,
                            state.last_click,
                        );

                        Some(Update::Click(click))
                    } else if state.focus.is_some() {
                        binding(Binding::Unfocus)
                    } else {
                        None
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => Some(Update::Release),
                mouse::Event::CursorMoved { .. } if state.text_drag.is_some() => {
                    let cursor_position = cursor.position_from(bounds.position())?
                        - Vector::new(padding.left, padding.top);
                    Some(Update::TextDrag(cursor_position))
                }
                mouse::Event::CursorMoved { .. } => match state.drag_click {
                    Some(mouse::click::Kind::Single) => {
                        // Upstream iced #3455: relative to the editor's
                        // origin, not only while over it, so a pointer that
                        // leaves sideways keeps extending the selection. The
                        // redraw re-arm covers only vertical overshoot (#1862).
                        let cursor_position = cursor.position_from(bounds.position())?
                            - Vector::new(padding.left, padding.top);

                        Some(Update::Drag(cursor_position))
                    }
                    _ => None,
                },
                mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                    Some(Update::Scroll(match delta {
                        mouse::ScrollDelta::Lines { y, .. } => {
                            if y.abs() > 0.0 {
                                y.signum() * -(y.abs() * 4.0).max(1.0)
                            } else {
                                0.0
                            }
                        }
                        mouse::ScrollDelta::Pixels { y, .. } => -y / 4.0,
                    }))
                }
                _ => None,
            },
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => Some(
                    Update::InputMethod(Ime::Toggle(matches!(event, input_method::Event::Opened))),
                ),
                input_method::Event::Preedit(content, selection) if state.focus.is_some() => {
                    Some(Update::InputMethod(Ime::Preedit {
                        content: content.clone(),
                        selection: selection.clone(),
                    }))
                }
                input_method::Event::Commit(content) if state.focus.is_some() => {
                    Some(Update::InputMethod(Ime::Commit(content.clone())))
                }
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
                let status = if state.focus.is_some() {
                    Status::Focused {
                        is_hovered: cursor.is_over(bounds),
                    }
                } else {
                    Status::Active
                };

                let key_press = KeyPress {
                    key: key.clone(),
                    modified_key: modified_key.clone(),
                    physical_key: *physical_key,
                    modifiers: *modifiers,
                    text: text.clone(),
                    status,
                };

                if let Some(key_binding) = key_binding {
                    key_binding(key_press)
                } else {
                    Binding::from_key_press(key_press)
                }
                .map(Self::Binding)
            }
            _ => None,
        }
    }
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

/// The possible status of a [`TextEditor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The [`TextEditor`] can be interacted with.
    Active,
    /// The [`TextEditor`] is being hovered.
    Hovered,
    /// The [`TextEditor`] is focused.
    Focused {
        /// Whether the [`TextEditor`] is hovered, while focused.
        is_hovered: bool,
    },
    /// The [`TextEditor`] cannot be interacted with.
    Disabled,
}

/// The appearance of a text input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The [`Background`] of the text input.
    pub background: Background,
    /// The [`Border`] of the text input.
    pub border: Border,
    /// The [`Color`] of the placeholder of the text input.
    pub placeholder: Color,
    /// The [`Color`] of the value of the text input.
    pub value: Color,
    /// The [`Color`] of the selection of the text input.
    pub selection: Color,
}

/// The theme catalog of a [`TextEditor`].
pub trait Catalog: theme::Base {
    /// The item class of the [`Catalog`].
    type Class<'a>;

    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`TextEditor`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style {
        class(self, status)
    }
}

/// The default style of a [`TextEditor`].
pub fn default(theme: &Theme, status: Status) -> Style {
    let palette = theme.palette();

    let active = Style {
        background: Background::Color(palette.background.base.color),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: palette.background.strong.color,
        },
        placeholder: palette.secondary.base.color,
        value: palette.background.base.text,
        selection: palette.primary.weak.color,
    };

    match status {
        Status::Active => active,
        Status::Hovered => Style {
            border: Border {
                color: palette.background.base.text,
                ..active.border
            },
            ..active
        },
        Status::Focused { .. } => Style {
            border: Border {
                color: palette.primary.strong.color,
                ..active.border
            },
            ..active
        },
        Status::Disabled => Style {
            background: Background::Color(palette.background.weak.color),
            value: active.placeholder,
            placeholder: palette.background.strongest.color,
            ..active
        },
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn convert_macos_shortcut(
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

impl<H: text::Parser> widget::tree::Interactive for State<H> {
    fn cancel_interaction(&mut self) {
        self.drag_click = None;
        self.pending_drag = None;
        self.edge_autoscroll_last = None;
        self.last_click = None;
        self.partial_scroll = 0.0;
    }
}

#[cfg(test)]
mod blink_tests {
    use super::*;

    fn focus_after(elapsed: Duration) -> Focus {
        let updated_at = Instant::now();
        Focus {
            updated_at,
            now: updated_at + elapsed,
            is_window_focused: true,
        }
    }

    /// The caret blinks for ten seconds after the last caret activity, its
    /// last frame lands exactly when blinking ends, and after that it is solid
    /// and asks for nothing.
    #[test]
    fn the_caret_stops_blinking_after_ten_seconds() {
        let hidden = focus_after(Duration::from_millis(700));
        assert!(!hidden.is_cursor_visible());
        assert_eq!(
            hidden.next_blink_redraw(),
            Some(hidden.updated_at + Duration::from_millis(1000))
        );

        let last = focus_after(Duration::from_millis(9_800));
        assert_eq!(
            last.next_blink_redraw(),
            Some(last.updated_at + Duration::from_secs(10)),
            "the last frame lands when blinking ends"
        );

        // 10.7 s is a hidden half-cycle if the blink went on.
        let settled = focus_after(Duration::from_millis(10_700));
        assert!(settled.is_cursor_visible(), "a settled caret is solid");
        assert_eq!(settled.next_blink_redraw(), None);
    }
    /// The desktop's blink time sets the phase; blinking off or reduced
    /// motion leaves the caret solid from the start, asking for no frames.
    #[test]
    fn the_caret_follows_the_desktop_blink_preferences() {
        use crate::core::preferences::{self, Preferences};
        let hidden = focus_after(Duration::from_millis(700));
        let slow = Preferences {
            cursor_blink_time: Duration::from_millis(2000),
            ..Preferences::DEFAULT
        };
        preferences::scoped(slow, || {
            assert!(
                hidden.is_cursor_visible(),
                "700 ms is in the first shown phase"
            );
            assert_eq!(
                hidden.next_blink_redraw(),
                Some(hidden.updated_at + Duration::from_millis(1000))
            );
        });
        for still in [
            Preferences {
                cursor_blink: false,
                ..Preferences::DEFAULT
            },
            Preferences {
                reduced_motion: true,
                ..Preferences::DEFAULT
            },
        ] {
            preferences::scoped(still, || {
                assert!(hidden.is_cursor_visible(), "{still:?}");
                assert_eq!(hidden.next_blink_redraw(), None, "{still:?}");
            });
        }
    }
}
