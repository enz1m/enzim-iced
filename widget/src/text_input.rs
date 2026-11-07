// Modified by enzUI contributors. See FORK.md.
//! Text inputs display fields that can be filled with text.
//!
//! # Example
//! ```no_run
//! # mod iced { pub mod widget { pub use iced_widget::*; } pub use iced_widget::Renderer; pub use iced_widget::core::*; }
//! # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
//! #
//! use iced::widget::text_input;
//!
//! struct State {
//!    content: String,
//! }
//!
//! #[derive(Debug, Clone)]
//! enum Message {
//!     ContentChanged(String)
//! }
//!
//! fn view(state: &State) -> Element<'_, Message> {
//!     text_input("Type something here...", &state.content)
//!         .on_input(Message::ContentChanged)
//!         .into()
//! }
//!
//! fn update(state: &mut State, message: Message) {
//!     match message {
//!         Message::ContentChanged(content) => {
//!             state.content = content;
//!         }
//!     }
//! }
//! ```
mod editor;
mod value;

pub mod cursor;

pub use cursor::Cursor;
pub use value::Value;

use editor::Editor;

use crate::core::alignment;
use crate::core::clipboard;
use crate::core::input_method;
use crate::core::keyboard;
use crate::core::keyboard::key;
use crate::core::layout;
use crate::core::mouse::{self, click};
use crate::core::renderer;
use crate::core::text::paragraph::{self, Paragraph as _};
use crate::core::text::{self, Text};
use crate::core::time::{Duration, Instant};
use crate::core::touch;
use crate::core::widget;
use crate::core::widget::operation::{self, Operation};
use crate::core::widget::tree::{self, Tree};
use crate::core::window;
use crate::core::{
    Alignment, Background, Border, Color, Element, Event, Font, InputMethod, Layout, Length,
    Padding, Pixels, Point, Rectangle, Shell, Size, Theme, Vector, Widget,
};

use unicode_segmentation::UnicodeSegmentation;

/// A field that can be filled with text.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } pub use iced_widget::Renderer; pub use iced_widget::core::*; }
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// #
/// use iced::widget::text_input;
///
/// struct State {
///    content: String,
/// }
///
/// #[derive(Debug, Clone)]
/// enum Message {
///     ContentChanged(String)
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     text_input("Type something here...", &state.content)
///         .on_input(Message::ContentChanged)
///         .into()
/// }
///
/// fn update(state: &mut State, message: Message) {
///     match message {
///         Message::ContentChanged(content) => {
///             state.content = content;
///         }
///     }
/// }
/// ```
pub struct TextInput<'a, Message, Theme = crate::Theme, Renderer = crate::Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    id: Option<widget::Id>,
    placeholder: String,
    value: Value,
    is_secure: bool,
    /// enzICED: the first click into the unfocused input selects it all.
    select_all_on_focus: bool,
    purpose: input_method::Purpose,
    font: Option<Font>,
    width: Length,
    padding: Padding,
    size: Option<Pixels>,
    line_height: text::LineHeight,
    alignment: alignment::Horizontal,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_paste: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_submit: Option<Message>,
    icon: Option<Icon<Font>>,
    class: Theme::Class<'a>,
    last_status: Option<Status>,
    /// The renderer is chosen by type only: since upstream dropped the `Font`
    /// generic (#3451), no field names it.
    renderer: std::marker::PhantomData<fn() -> Renderer>,
}

/// The default [`Padding`] of a [`TextInput`].
pub const DEFAULT_PADDING: Padding = Padding::new(5.0);

impl<'a, Message, Theme, Renderer> TextInput<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Creates a new [`TextInput`] with the given placeholder and
    /// its current value.
    pub fn new(placeholder: &str, value: &str) -> Self {
        TextInput {
            id: None,
            placeholder: String::from(placeholder),
            value: Value::new(value),
            is_secure: false,
            select_all_on_focus: false,
            purpose: input_method::Purpose::Normal,
            font: None,
            width: Length::Fill,
            padding: DEFAULT_PADDING,
            size: None,
            line_height: text::LineHeight::default(),
            alignment: alignment::Horizontal::Left,
            on_input: None,
            on_paste: None,
            on_submit: None,
            icon: None,
            class: Theme::default(),
            last_status: None,
            renderer: std::marker::PhantomData,
        }
    }

    /// Sets the [`widget::Id`] of the [`TextInput`].
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Tells the input method and on-screen keyboard what the text is for:
    /// a number, an email address, a phone number, and so on. A secure input
    /// always reports [`input_method::Purpose::Secure`].
    pub fn purpose(mut self, purpose: input_method::Purpose) -> Self {
        self.purpose = purpose;
        self
    }

    /// enzICED: whether the click that focuses the [`TextInput`] selects its
    /// whole value (an address bar) instead of placing the caret; later
    /// clicks and drags behave as usual.
    pub fn select_all_on_focus(mut self, select_all: bool) -> Self {
        self.select_all_on_focus = select_all;
        self
    }

    /// Converts the [`TextInput`] into a secure password input.
    pub fn secure(mut self, is_secure: bool) -> Self {
        self.is_secure = is_secure;
        self
    }

    /// Sets the message that should be produced when some text is typed into
    /// the [`TextInput`].
    ///
    /// If this method is not called, the [`TextInput`] will be disabled.
    pub fn on_input(mut self, on_input: impl Fn(String) -> Message + 'a) -> Self {
        self.on_input = Some(Box::new(on_input));
        self
    }

    /// Sets the message that should be produced when some text is typed into
    /// the [`TextInput`], if `Some`.
    ///
    /// If `None`, the [`TextInput`] will be disabled.
    pub fn on_input_maybe(mut self, on_input: Option<impl Fn(String) -> Message + 'a>) -> Self {
        self.on_input = on_input.map(|f| Box::new(f) as _);
        self
    }

    /// Sets the message that should be produced when the [`TextInput`] is
    /// focused and the enter key is pressed.
    pub fn on_submit(mut self, message: Message) -> Self {
        self.on_submit = Some(message);
        self
    }

    /// Sets the message that should be produced when the [`TextInput`] is
    /// focused and the enter key is pressed, if `Some`.
    pub fn on_submit_maybe(mut self, on_submit: Option<Message>) -> Self {
        self.on_submit = on_submit;
        self
    }

    /// Sets the message that should be produced when some text is pasted into
    /// the [`TextInput`].
    pub fn on_paste(mut self, on_paste: impl Fn(String) -> Message + 'a) -> Self {
        self.on_paste = Some(Box::new(on_paste));
        self
    }

    /// Sets the message that should be produced when some text is pasted into
    /// the [`TextInput`], if `Some`.
    pub fn on_paste_maybe(mut self, on_paste: Option<impl Fn(String) -> Message + 'a>) -> Self {
        self.on_paste = on_paste.map(|f| Box::new(f) as _);
        self
    }

    /// Sets the [`Font`] of the [`TextInput`].
    ///
    /// [`Font`]: text::Font
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Sets the [`Icon`] of the [`TextInput`].
    pub fn icon(mut self, icon: Icon<Font>) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets the width of the [`TextInput`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the [`Padding`] of the [`TextInput`].
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the text size of the [`TextInput`].
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the [`text::LineHeight`] of the [`TextInput`].
    pub fn line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    /// Sets the horizontal alignment of the [`TextInput`].
    pub fn align_x(mut self, alignment: impl Into<alignment::Horizontal>) -> Self {
        self.alignment = alignment.into();
        self
    }

    /// Sets the style of the [`TextInput`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`TextInput`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Lays out the [`TextInput`], overriding its [`Value`] if provided.
    ///
    /// [`Renderer`]: text::Renderer
    pub fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
        value: Option<&Value>,
    ) {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let value = value.unwrap_or(&self.value);

        let font = self.font.unwrap_or_else(|| renderer.font());
        let text_size = self.size.unwrap_or_else(|| renderer.text_size());
        let outer_limits = *limits;
        let padding = self.padding.fit(Size::ZERO, limits.max());
        let height = self.line_height.to_absolute(text_size);

        let limits = limits.width(self.width).shrink(padding);
        let text_bounds = limits.resolve(self.width, height, Size::ZERO);

        let placeholder_text = Text {
            font,
            line_height: self.line_height,
            content: self.placeholder.as_str(),
            bounds: Size::new(f32::INFINITY, text_bounds.height),
            size: text_size,
            align_x: text::Alignment::Default,
            align_y: alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
            ellipsis: text::Ellipsis::None,
            hint_factor: renderer.hint_factor(),
        };

        let _ = state.placeholder.update(placeholder_text);

        let secure_value = self.is_secure.then(|| value.secure());
        let value = secure_value.as_ref().unwrap_or(value);

        let _ = state.value.update(Text {
            content: &value.to_string(),
            ..placeholder_text
        });

        // `Plain::update` keys on `Text.content`; a steady preedit reshapes zero times.
        if let Some(composition) = active_composition(value, state) {
            let _ = state.composition.update(Text {
                content: &composition.text,
                ..placeholder_text
            });
        }

        if let Some(icon) = &self.icon {
            let mut content = [0; 4];

            let icon_text = Text {
                line_height: self.line_height,
                content: icon.code_point.encode_utf8(&mut content) as &_,
                font: icon.font,
                size: icon.size.unwrap_or_else(|| renderer.text_size()),
                bounds: Size::new(f32::INFINITY, text_bounds.height),
                align_x: text::Alignment::Center,
                align_y: alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            };

            let _ = state.icon.update(icon_text);
        }
        let (paint, _) = self.presentation_paint(state, renderer, value);
        state.presentation.set(paint);
        // The paragraph selected here must match the one `draw` selects so the
        // presentation cache invalidates on preedit changes.
        let paragraph = if active_composition(value, state).is_some() {
            &state.composition
        } else if value.is_empty() && state.preedit.as_ref().is_none_or(|p| p.content.is_empty()) {
            &state.placeholder
        } else {
            &state.value
        };
        layout::input::node(
            &outer_limits,
            layout::input::Shape {
                width: self.width,
                padding: self.padding,
                line_height: height.0,
                icon: self.icon.as_ref().map(|icon| layout::input::Icon {
                    width: state.icon.min_width(),
                    spacing: icon.spacing,
                    leading: matches!(icon.side, Side::Left),
                }),
            },
            state.presentation.clone(),
            paragraph.presentation_node(),
            state.icon.presentation_node(),
        )
        .apply(tree);
    }

    fn presentation_paint(
        &self,
        state: &State<Renderer::Paragraph>,
        renderer: &Renderer,
        value: &Value,
    ) -> (layout::input::Paint, bool) {
        use layout::input::{Caret, Paint};
        let composition = active_composition(value, state);
        let paragraph = if composition.is_some() {
            state.composition.raw()
        } else if value.is_empty() && state.preedit.as_ref().is_none_or(|p| p.content.is_empty()) {
            state.placeholder.raw()
        } else {
            state.value.raw()
        };
        let mut paint = Paint {
            text_width: paragraph.min_width(),
            alignment: self.alignment,
            scroll_anchor: None,
            caret: None,
            blink: None,
        };
        let mut selecting = false;
        if let Some(focus) = state.is_focused.as_ref().filter(|f| f.is_window_focused) {
            let x = |index| {
                paragraph
                    .grapheme_position(0, index)
                    .unwrap_or(Point::ORIGIN)
                    .x
            };
            if let Some(composition) = composition.as_ref() {
                // Composition owns the visual span, so no selection quad is painted here.
                let position = x(composition.caret_grapheme_index);
                paint.scroll_anchor = Some(position);
                if self.on_input.is_some() {
                    let clock = layout::input::Blink {
                        epoch: focus.updated_at,
                        interval: Duration::from_millis(cursor_blink_interval_millis() as u64),
                    };
                    let caret = Caret {
                        x: position,
                        width: caret_width(renderer.hint_factor()),
                    };
                    if blink_settled(focus.updated_at, focus.now) {
                        paint.caret = Some(caret);
                    } else {
                        paint.blink = Some((caret, clock));
                        paint.caret = clock.visible(focus.now).then_some(caret);
                    }
                }
            } else {
                match state.cursor.state(value) {
                    cursor::State::Index(index) => {
                        let position = x(index);
                        paint.scroll_anchor = Some(position);
                        if self.on_input.is_some() {
                            let clock = layout::input::Blink {
                                epoch: focus.updated_at,
                                interval: Duration::from_millis(
                                    cursor_blink_interval_millis() as u64
                                ),
                            };
                            let caret = Caret {
                                x: position,
                                width: caret_width(renderer.hint_factor()),
                            };
                            if blink_settled(focus.updated_at, focus.now) {
                                paint.caret = Some(caret);
                            } else {
                                paint.blink = Some((caret, clock));
                                paint.caret = clock.visible(focus.now).then_some(caret);
                            }
                        }
                    }
                    cursor::State::Selection { start, end } => {
                        let left = x(start.min(end));
                        let right = x(start.max(end));
                        paint.scroll_anchor = Some(x(end));
                        paint.caret = Some(Caret {
                            x: left,
                            width: right - left,
                        });
                        selecting = true;
                    }
                }
            }
        }
        (paint, selecting)
    }

    /// Offers a finished selection as the primary selection, the text a
    /// middle click pastes elsewhere. A selection still being dragged, an
    /// unchanged one and a secure field's are not offered.
    fn offer_primary_selection(
        &self,
        state: &mut State<Renderer::Paragraph>,
        shell: &mut Shell<'_, Message>,
    ) {
        if self.is_secure || state.is_focused.is_none() || state.is_dragging.is_some() {
            return;
        }
        let selected = state
            .cursor
            .selection(&self.value)
            .map(|(start, end)| self.value.select(start, end).to_string())
            .filter(|text| !text.is_empty());
        match selected {
            Some(text) if state.primary_offered.as_deref() != Some(text.as_str()) => {
                shell.write_primary(clipboard::Content::Text(text.clone()));
                state.primary_offered = Some(text);
            }
            Some(_) => {}
            None => state.primary_offered = None,
        }
    }

    fn dispatch_event(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let text_area = text_area(layout, tree);
        let caret_node = caret_node(layout, tree);
        let update_cache = |state, value| {
            replace_paragraph(
                renderer,
                state,
                text_area,
                value,
                self.font,
                self.size,
                self.line_height,
            );
        };

        match &event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = state::<Renderer>(tree);
                let cursor_before = state.cursor;
                let was_focused = state.is_focused.is_some();

                let click_position = cursor.position_over(layout.bounds());

                state.is_focused = if click_position.is_some() {
                    let now = Instant::now();

                    Some(Focus {
                        updated_at: now,
                        now,
                        is_window_focused: true,
                    })
                } else {
                    None
                };

                if let Some(cursor_position) = click_position {
                    let text_layout = Layout::new(text_area.size()).move_to(text_area.position());

                    let target = {
                        let text_bounds = text_layout.bounds();

                        let alignment_offset = alignment_offset(
                            text_bounds.width,
                            state.value.raw().min_width(),
                            self.alignment,
                        );

                        cursor_position.x - text_bounds.x - alignment_offset
                    };

                    let click =
                        mouse::Click::new(cursor_position, mouse::Button::Left, state.last_click);

                    match click.kind() {
                        click::Kind::Single if self.select_all_on_focus && !was_focused => {
                            state.cursor.select_all(&self.value);
                            state.is_dragging = None;
                        }
                        click::Kind::Single => {
                            let position = if target > 0.0 {
                                let value = if self.is_secure {
                                    self.value.secure()
                                } else {
                                    self.value.clone()
                                };

                                find_cursor_position(text_layout.bounds(), &value, state, target)
                            } else {
                                None
                            }
                            .unwrap_or(0);

                            let in_selection = !self.is_secure
                                && !state.keyboard_modifiers.shift()
                                && crate::core::text::selection_drag::is_available()
                                && state.cursor.selection(&self.value).is_some_and(
                                    |(start, end)| start <= position && position < end,
                                );

                            if in_selection {
                                // Wait: a drag carries the selection away,
                                // a release places the cursor (enzICED).
                                state.is_dragging = Some(Drag::Text {
                                    origin: cursor_position,
                                    position,
                                });
                            } else {
                                if state.keyboard_modifiers.shift() {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), position);
                                } else {
                                    state.cursor.move_to(position);
                                }

                                state.is_dragging = Some(Drag::Select);
                            }
                        }
                        click::Kind::Double => {
                            if self.is_secure {
                                state.cursor.select_all(&self.value);

                                state.is_dragging = None;
                            } else {
                                let position = find_cursor_position(
                                    text_layout.bounds(),
                                    &self.value,
                                    state,
                                    target,
                                )
                                .unwrap_or(0);

                                state.cursor.select_range(
                                    self.value.previous_start_of_word(position),
                                    self.value.next_end_of_word(position),
                                );

                                state.is_dragging = Some(Drag::SelectWords { anchor: position });
                            }
                        }
                        click::Kind::Triple => {
                            state.cursor.select_all(&self.value);
                            state.is_dragging = None;
                        }
                    }

                    state.last_click = Some(click);

                    if cursor_before != state.cursor {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) => {
                let Some(cursor_position) = cursor.position_over(layout.bounds()) else {
                    return;
                };
                if self.on_input.is_none() || !clipboard::primary_paste_enabled() {
                    return;
                }
                let state = state::<Renderer>(tree);
                let now = Instant::now();
                state.is_focused = Some(Focus {
                    updated_at: now,
                    now,
                    is_window_focused: true,
                });

                // Paste where the pointer is, like the primary paste of
                // every other Linux text field.
                let text_layout = Layout::new(text_area.size()).move_to(text_area.position());
                let text_bounds = text_layout.bounds();
                let alignment_offset = alignment_offset(
                    text_bounds.width,
                    state.value.raw().min_width(),
                    self.alignment,
                );
                let target = cursor_position.x - text_bounds.x - alignment_offset;
                let position = if target > 0.0 {
                    find_cursor_position(text_bounds, &self.value, state, target)
                } else {
                    None
                }
                .unwrap_or(0);
                state.cursor.move_to(position);
                state.pasting_primary = true;

                shell.read_primary(clipboard::Kind::Text);
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Clipboard(clipboard::Event::ReadPrimary(result)) => {
                let state = state::<Renderer>(tree);
                if !std::mem::take(&mut state.pasting_primary) {
                    return;
                }
                let (Ok(content), Some(on_input)) = (result, &self.on_input) else {
                    return;
                };
                let clipboard::Content::Text(text) = content.as_ref() else {
                    return;
                };

                let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                editor.paste(Value::new(text));

                let message = if let Some(paste) = &self.on_paste {
                    (paste)(editor.contents())
                } else {
                    (on_input)(editor.contents())
                };
                shell.publish(message);
                shell.capture_event();

                if let Some(focus) = &mut state.is_focused {
                    focus.updated_at = Instant::now();
                }
                update_cache(state, &self.value);
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                let state = state::<Renderer>(tree);
                // A press inside the selection that never moved is a click.
                if let Some(Drag::Text { position, .. }) = state.is_dragging.take() {
                    state.cursor.move_to(position);
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                let state = state::<Renderer>(tree);

                match state.is_dragging {
                    // The platform carries the selection: nothing to select.
                    Some(Drag::Carrying) => {
                        shell.capture_event();
                        return;
                    }
                    Some(Drag::Text {
                        origin,
                        position: press,
                    }) => {
                        if origin.distance(*position) < crate::core::text::selection_drag::THRESHOLD
                        {
                            shell.capture_event();
                            return;
                        }
                        let text = state
                            .cursor
                            .selection(&self.value)
                            .map(|(start, end)| self.value.select(start, end).to_string());
                        let started = text.is_some_and(|text| {
                            crate::core::text::selection_drag::begin(
                                crate::core::text::selection_drag::SelectionDrag { text },
                            )
                        });
                        if started {
                            state.is_dragging = Some(Drag::Carrying);
                            shell.capture_event();
                            return;
                        }
                        // Declined: select from the press, as without a selection.
                        state.cursor.move_to(press);
                        state.is_dragging = Some(Drag::Select);
                    }
                    _ => {}
                }

                if let Some(is_dragging) = &state.is_dragging {
                    let text_layout = Layout::new(text_area.size()).move_to(text_area.position());

                    let target = {
                        let text_bounds = text_layout.bounds();

                        let alignment_offset = alignment_offset(
                            text_bounds.width,
                            state.value.raw().min_width(),
                            self.alignment,
                        );

                        position.x - text_bounds.x - alignment_offset
                    };

                    let value = if self.is_secure {
                        self.value.secure()
                    } else {
                        self.value.clone()
                    };

                    let position =
                        find_cursor_position(text_layout.bounds(), &value, state, target)
                            .unwrap_or(0);

                    let selection_before = state.cursor.selection(&value);

                    match is_dragging {
                        Drag::Select => {
                            state
                                .cursor
                                .select_range(state.cursor.start(&value), position);
                        }
                        Drag::Text { .. } | Drag::Carrying => {}
                        Drag::SelectWords { anchor } => {
                            if position < *anchor {
                                state.cursor.select_range(
                                    self.value.previous_start_of_word(position),
                                    self.value.next_end_of_word(*anchor),
                                );
                            } else {
                                state.cursor.select_range(
                                    self.value.previous_start_of_word(*anchor),
                                    self.value.next_end_of_word(position),
                                );
                            }
                        }
                    }

                    if let Some(focus) = &mut state.is_focused {
                        focus.updated_at = Instant::now();
                    }

                    if selection_before != state.cursor.selection(&value) {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                text,
                modified_key,
                physical_key,
                ..
            }) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &mut state.is_focused {
                    let modifiers = state.keyboard_modifiers;

                    match key.to_latin(*physical_key) {
                        Some('c') if state.keyboard_modifiers.command() && !self.is_secure => {
                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                shell.write_clipboard(clipboard::Content::Text(
                                    self.value.select(start, end).to_string(),
                                ));
                            }

                            shell.capture_event();
                            return;
                        }
                        Some('x') if state.keyboard_modifiers.command() && !self.is_secure => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                shell.write_clipboard(clipboard::Content::Text(
                                    self.value.select(start, end).to_string(),
                                ));
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            update_cache(state, &self.value);
                            return;
                        }
                        Some('v')
                            if state.keyboard_modifiers.command()
                                && !state.keyboard_modifiers.alt() =>
                        {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            let content = match &state.is_pasting {
                                Some(Paste::Pasting(content)) => content,
                                Some(Paste::Reading { .. }) => return,
                                None => {
                                    shell.read_clipboard(clipboard::Kind::Text);
                                    state.is_pasting = Some(Paste::Reading { released: false });
                                    return;
                                }
                            };

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.paste(content.clone());

                            let message = if let Some(paste) = &self.on_paste {
                                (paste)(editor.contents())
                            } else {
                                (on_input)(editor.contents())
                            };
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            update_cache(state, &self.value);
                            return;
                        }
                        Some('z' | 'y') if state.keyboard_modifiers.command() => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };
                            let redo = state.keyboard_modifiers.shift()
                                || key.to_latin(*physical_key) == Some('y');
                            let current = Snapshot {
                                value: self.value.to_string(),
                                cursor: state.cursor,
                            };
                            let restored = if redo {
                                state.history.redo.pop().inspect(|_| {
                                    state.history.undo.push(current);
                                })
                            } else {
                                state.history.undo.pop().inspect(|_| {
                                    state.history.redo.push(current);
                                })
                            };
                            if let Some(snapshot) = restored {
                                self.value = Value::new(&snapshot.value);
                                state.cursor = snapshot.cursor;
                                state.history.restoring = true;
                                state.history.typing_at = None;
                                shell.publish((on_input)(snapshot.value));
                                focus.updated_at = Instant::now();
                                update_cache(state, &self.value);
                            }
                            shell.capture_event();
                            return;
                        }
                        Some('a') if state.keyboard_modifiers.command() => {
                            let cursor_before = state.cursor;

                            state.cursor.select_all(&self.value);

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                            return;
                        }
                        _ => {}
                    }

                    // enzICED: a command chord's printable text (Ctrl+0, Super+=)
                    // is a shortcut, not typing; AltGr (Ctrl+Alt) still types.
                    let chord = modifiers.logo() || (modifiers.control() && !modifiers.alt());
                    if let Some(text) = text.as_ref().filter(|_| !chord) {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        state.is_pasting = None;

                        if let Some(c) = text.chars().next().filter(|c| !c.is_control()) {
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);

                            editor.insert(c);

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            update_cache(state, &self.value);
                            return;
                        }
                    }

                    #[cfg(target_os = "macos")]
                    let macos_shortcut = crate::text_editor::convert_macos_shortcut(key, modifiers);

                    #[cfg(target_os = "macos")]
                    let modified_key = macos_shortcut.as_ref().unwrap_or(modified_key);

                    match modified_key.as_ref() {
                        keyboard::Key::Named(key::Named::Enter) => {
                            if let Some(on_submit) = self.on_submit.clone() {
                                shell.publish(on_submit);
                                shell.capture_event();
                            }
                        }
                        keyboard::Key::Named(key::Named::Backspace) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state.cursor.selection(&self.value).is_none() {
                                if (self.is_secure && modifiers.jump()) || modifiers.macos_command()
                                {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else if modifiers.jump() {
                                    state.cursor.select_left_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.backspace();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Delete) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state.cursor.selection(&self.value).is_none() {
                                if (self.is_secure && modifiers.jump()) || modifiers.macos_command()
                                {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else if modifiers.jump() {
                                    state.cursor.select_right_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            focus.updated_at = Instant::now();
                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Home) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state
                                    .cursor
                                    .select_range(state.cursor.start(&self.value), 0);
                            } else {
                                state.cursor.move_to(0);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::End) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state.cursor.select_range(
                                    state.cursor.start(&self.value),
                                    self.value.len(),
                                );
                            } else {
                                state.cursor.move_to(self.value.len());
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowLeft) => {
                            let cursor_before = state.cursor;

                            if (self.is_secure && modifiers.jump()) || modifiers.macos_command() {
                                if modifiers.shift() {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else {
                                    state.cursor.move_to(0);
                                }
                            } else if modifiers.jump() {
                                // Words run the other way in right-to-left text.
                                let rightward = !self.is_secure
                                    && is_right_to_left(state.value.raw(), &self.value);
                                match (modifiers.shift(), rightward) {
                                    (true, false) => state.cursor.select_left_by_words(&self.value),
                                    (false, false) => state.cursor.move_left_by_words(&self.value),
                                    (true, true) => state.cursor.select_right_by_words(&self.value),
                                    (false, true) => state.cursor.move_right_by_words(&self.value),
                                }
                            } else if !self.is_secure
                                && move_visually(
                                    state.value.raw(),
                                    &mut state.cursor,
                                    &self.value,
                                    false,
                                    modifiers.shift(),
                                )
                            {
                            } else if modifiers.shift() {
                                state.cursor.select_left(&self.value);
                            } else {
                                state.cursor.move_left(&self.value);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowRight) => {
                            let cursor_before = state.cursor;

                            if (self.is_secure && modifiers.jump()) || modifiers.macos_command() {
                                if modifiers.shift() {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else {
                                    state.cursor.move_to(self.value.len());
                                }
                            } else if modifiers.jump() {
                                // Words run the other way in right-to-left text.
                                let leftward = !self.is_secure
                                    && is_right_to_left(state.value.raw(), &self.value);
                                match (modifiers.shift(), leftward) {
                                    (true, false) => {
                                        state.cursor.select_right_by_words(&self.value);
                                    }
                                    (false, false) => state.cursor.move_right_by_words(&self.value),
                                    (true, true) => state.cursor.select_left_by_words(&self.value),
                                    (false, true) => state.cursor.move_left_by_words(&self.value),
                                }
                            } else if !self.is_secure
                                && move_visually(
                                    state.value.raw(),
                                    &mut state.cursor,
                                    &self.value,
                                    true,
                                    modifiers.shift(),
                                )
                            {
                            } else if modifiers.shift() {
                                state.cursor.select_right(&self.value);
                            } else {
                                state.cursor.move_right(&self.value);
                            }

                            if cursor_before != state.cursor {
                                focus.updated_at = Instant::now();

                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::Escape) => {
                            // enzUI-ICED #1087: Escape is only ours when we
                            // have something to cancel.
                            //
                            // A focused input mid-composition owns the key:
                            // an IME pre-edit with content, an in-flight
                            // clipboard paste, or a pointer drag-selection
                            // are all operations Escape aborts, and an
                            // ancestor must not also act on the same press.
                            //
                            // With none of those in flight, Escape merely
                            // unfocuses — a state change nothing outside can
                            // see. Capturing it anyway made an enclosing
                            // surface (a modal dialog, a drawer, a popover)
                            // undismissable while a text field held focus,
                            // which is a keyboard trap: the field ate the
                            // first press and did nothing visible with it.
                            // `text_editor`'s `Binding::Unfocus` has always
                            // declined to capture here; this is the same rule
                            // for the single-line input.
                            let is_composing = state
                                .preedit
                                .as_ref()
                                .is_some_and(|preedit| !preedit.content.is_empty())
                                || state.is_pasting.is_some()
                                || state.is_dragging.is_some();

                            state.is_focused = None;
                            state.is_dragging = None;
                            state.is_pasting = None;

                            state.keyboard_modifiers = keyboard::Modifiers::default();

                            if is_composing {
                                shell.capture_event();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                let state = state::<Renderer>(tree);

                if state.is_focused.is_some()
                    && let keyboard::Key::Character("v") = key.as_ref()
                {
                    shell.capture_event();
                }

                // A read still in flight is the paste the press asked for:
                // it lands once, whenever the clipboard answers (enzICED).
                state.is_pasting = match state.is_pasting {
                    Some(Paste::Reading { .. }) => Some(Paste::Reading { released: true }),
                    _ => None,
                };
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                let state = state::<Renderer>(tree);

                state.keyboard_modifiers = *modifiers;
            }
            Event::Clipboard(clipboard::Event::Read(result)) => {
                let Some(on_input) = &self.on_input else {
                    return;
                };

                let state = state::<Renderer>(tree);

                let Some(Paste::Reading { released }) = state.is_pasting else {
                    return;
                };

                let Some(focus) = &mut state.is_focused else {
                    return;
                };

                // A failed or non-text read ends the paste, so the next
                // press asks again (enzICED).
                state.is_pasting = None;

                if let Ok(content) = result
                    && let clipboard::Content::Text(text) = content.as_ref()
                {
                    if !released {
                        state.is_pasting = Some(Paste::Pasting(Value::new(text)));
                    }

                    let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                    editor.paste(Value::new(text));

                    let message = if let Some(paste) = &self.on_paste {
                        (paste)(editor.contents())
                    } else {
                        (on_input)(editor.contents())
                    };
                    shell.publish(message);
                    shell.capture_event();

                    focus.updated_at = Instant::now();
                    update_cache(state, &self.value);
                    return;
                }
            }
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => {
                    let state = state::<Renderer>(tree);

                    state.preedit = matches!(event, input_method::Event::Opened)
                        .then(input_method::Preedit::new);

                    shell.request_redraw();
                }
                input_method::Event::Preedit(content, selection) => {
                    let state = state::<Renderer>(tree);

                    if state.is_focused.is_some() {
                        state.preedit = Some(input_method::Preedit {
                            content: content.to_owned(),
                            selection: selection.clone(),
                            text_size: self.size,
                        });

                        // Reshape at the event edge so an `input_method`
                        // request issued in the same update batch measures
                        // against the current preedit, not a stale empty
                        // composition paragraph carried in from earlier layout.
                        reshape_composition(
                            renderer,
                            state,
                            text_area,
                            &self.value,
                            self.font,
                            self.size,
                            self.line_height,
                        );

                        shell.request_redraw();
                    }
                }
                input_method::Event::Commit(text) => {
                    let state = state::<Renderer>(tree);

                    if let Some(focus) = &mut state.is_focused {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                        editor.paste(Value::new(text));

                        focus.updated_at = Instant::now();
                        state.is_pasting = None;

                        let message = (on_input)(editor.contents());
                        shell.publish(message);
                        shell.capture_event();

                        update_cache(state, &self.value);
                    }
                }
            },
            Event::Window(window::Event::Unfocused) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = false;
                }
            }
            Event::Window(window::Event::Focused) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = true;
                    focus.updated_at = Instant::now();

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &mut state.is_focused
                    && focus.is_window_focused
                {
                    if matches!(state.cursor.state(&self.value), cursor::State::Index(_)) {
                        let clock = layout::input::Blink {
                            epoch: focus.updated_at,
                            interval: Duration::from_millis(cursor_blink_interval_millis() as u64),
                        };
                        let settled = blink_settled(focus.updated_at, *now);
                        // A settled caret publishes no clock, so nothing else
                        // can own its paint.
                        let owned = !settled
                            && caret_node
                                .is_some_and(|node| shell.presentation_owns_blink(node, clock));
                        let blink_phase = |at: Instant| {
                            if blink_settled(focus.updated_at, at) {
                                return 0;
                            }
                            at.saturating_duration_since(focus.updated_at).as_millis()
                                / cursor_blink_interval_millis()
                                % 2
                        };
                        // A freshly built widget (`last_status` still `None`) needs no
                        // extra frame: a rebuilt interface is always drawn, so asking
                        // again here turned every rebuild of a focused input into
                        // one more immediate frame (a continuous loop wherever the
                        // view is rebuilt per frame).
                        if focus.now < focus.updated_at
                            || (!owned && blink_phase(focus.now) != blink_phase(*now))
                        {
                            shell.request_redraw();
                        }
                        focus.now = *now;

                        let elapsed = (*now - focus.updated_at).as_millis();
                        let millis_until_redraw = (cursor_blink_interval_millis()
                            - elapsed % cursor_blink_interval_millis())
                        .min(cursor_blink_timeout_millis().saturating_sub(elapsed));

                        if !owned && !settled {
                            shell.schedule_redraw_at(
                                *now + Duration::from_millis(millis_until_redraw as u64),
                            );
                        }
                    }

                    let input_method = self.input_method(state, text_area, &self.value);
                    let enabled = matches!(input_method, InputMethod::Enabled { .. });
                    match enabled.then(|| self.surrounding(state)).flatten() {
                        Some(surrounding) => {
                            shell.request_input_method_with_surrounding(&input_method, surrounding);
                        }
                        None => shell.request_input_method(&input_method),
                    }
                }
            }
            _ => {}
        }

        let state = state::<Renderer>(tree);
        let is_disabled = self.on_input.is_none();

        let status = if is_disabled {
            Status::Disabled
        } else if state.is_focused() {
            Status::Focused {
                is_hovered: cursor.is_over(layout.bounds()),
            }
        } else if cursor.is_over(layout.bounds()) {
            Status::Hovered
        } else {
            Status::Active
        };

        // Idle inputs are pruned from frame dispatch, so paint cannot wait for that event.
        let status_changed = self.last_status.replace(status) != Some(status);
        if status_changed && !matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            shell.request_redraw();
        }
    }

    /// The text around the cursor, for the input method; none for a secure
    /// field.
    fn surrounding(&self, state: &State<Renderer::Paragraph>) -> Option<input_method::Surrounding> {
        if self.is_secure {
            return None;
        }
        let (anchor, cursor) = match state.cursor.state(&self.value) {
            cursor::State::Index(index) => (index, index),
            cursor::State::Selection { start, end } => (start, end),
        };
        let byte = |index| self.value.until(index).to_string().len();
        Some(input_method::Surrounding::around(
            &self.value.to_string(),
            byte(cursor),
            byte(anchor),
        ))
    }

    fn input_method<'b>(
        &self,
        state: &'b State<Renderer::Paragraph>,
        text_bounds: Rectangle,
        value: &Value,
    ) -> InputMethod<&'b str> {
        let Some(Focus {
            is_window_focused: true,
            ..
        }) = &state.is_focused
        else {
            return InputMethod::Disabled;
        };

        let secure_value = self.is_secure.then(|| value.secure());
        let value = secure_value.as_ref().unwrap_or(value);

        // The IME candidate rectangle must sit under the visible caret, which
        // during composition is inside the composition paragraph at the
        // preedit-derived index; the model `Cursor` and `Value` stay untouched.
        let (text, caret_index) = if let Some(composition) = active_composition(value, state) {
            (state.composition.raw(), composition.caret_grapheme_index)
        } else {
            let caret_index = match state.cursor.state(value) {
                cursor::State::Index(position) => position,
                cursor::State::Selection { start, end } => start.min(end),
            };
            (state.value.raw(), caret_index)
        };

        let (cursor_x, scroll_offset) =
            measure_cursor_and_scroll_offset(text, text_bounds, caret_index);

        let alignment_offset =
            alignment_offset(text_bounds.width, text.min_width(), self.alignment);

        let x = (text_bounds.x + cursor_x).floor() - scroll_offset + alignment_offset;

        InputMethod::Enabled {
            cursor: Rectangle::new(
                Point::new(x, text_bounds.y),
                Size::new(1.0, text_bounds.height),
            ),
            purpose: if self.is_secure {
                input_method::Purpose::Secure
            } else {
                self.purpose
            },
            // The widget rasters the composed text inline; forwarding a copy
            // would let the winit over-the-spot overlay repaint the same glyphs.
            preedit: None,
        }
    }

    /// Draws the [`TextInput`] with the given [`Renderer`], overriding its
    /// [`Value`] if provided.
    ///
    /// [`Renderer`]: text::Renderer
    pub fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        layout: Layout,
        _cursor: mouse::Cursor,
        value: Option<&Value>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        let value = value.unwrap_or(&self.value);
        let secure_value = self.is_secure.then(|| value.secure());
        let value = secure_value.as_ref().unwrap_or(value);
        let text_layout = layout.synthetic(tree).child(0);
        let text_bounds = text_layout.bounds();
        let style = theme.style(&self.class, self.last_status.unwrap_or(Status::Disabled));
        layout.fill_presentation_quad(
            tree,
            renderer,
            renderer::Quad {
                bounds: layout.bounds(),
                border: style.border,
                ..renderer::Quad::default()
            },
            style.background,
        );

        if self.icon.is_some() && layout.synthetic(tree).child(1).bounds().height > 0.0 {
            let icon_layout = layout.synthetic(tree).child(1);
            let icon = state.icon.raw();
            let recorded = renderer.start_layout_recording(state.icon.presentation_node());
            renderer.fill_paragraph(
                icon,
                icon_layout.bounds().anchor(
                    icon.min_bounds(),
                    Alignment::Center,
                    Alignment::Center,
                ),
                style.icon,
                *viewport,
            );
            if recorded {
                renderer.mark_recording(renderer::NodeId::STATIC);
            }
        }

        let (paint, selecting) = self.presentation_paint(state, renderer, value);
        let composing = active_composition(value, state).is_some();
        let paragraph = if composing {
            &state.composition
        } else if value.is_empty() && state.preedit.as_ref().is_none_or(|p| p.content.is_empty()) {
            &state.placeholder
        } else {
            &state.value
        };
        let scope = text_layout.clip_presentation_node();
        let capture = scope.and_then(|id| renderer.layout_clip_capture(id));
        let Some(mask) = capture
            .map(|value| value.mask)
            .or_else(|| text_bounds.intersection(viewport))
        else {
            return;
        };
        let material_viewport = capture.map_or(mask, |value| value.viewport);
        let draw = |renderer: &mut Renderer| {
            if text_bounds.height == 0.0 {
                return;
            }
            // An empty quad preserves blink-off damage tracking in backends that need it.
            let caret_bounds = paint.caret.map_or(
                Rectangle::new(text_bounds.position(), Size::ZERO),
                |caret| Rectangle {
                    x: text_bounds.x + caret.x + paint.offset(text_bounds.width),
                    y: text_bounds.y,
                    width: caret.width,
                    height: text_bounds.height,
                },
            );
            if let Some((caret, clock)) = paint.blink {
                let caret_layout = text_layout.child(0).child(1);
                let quad = renderer::Quad {
                    bounds: Rectangle {
                        x: text_bounds.x + caret.x + paint.offset(text_bounds.width),
                        y: text_bounds.y,
                        width: caret.width,
                        height: text_bounds.height,
                    },
                    ..renderer::Quad::default()
                };
                if let Some(node) = caret_layout.container_presentation_node() {
                    renderer.fill_layout_blink(
                        node,
                        quad,
                        style.value.into(),
                        clock,
                        state.is_focused.as_ref().unwrap().now,
                    );
                } else {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: caret_bounds,
                            ..quad
                        },
                        if paint.caret.is_some() {
                            style.value
                        } else {
                            Color::TRANSPARENT
                        },
                    );
                }
            } else {
                text_layout.child(0).child(1).fill_presentation_quad(
                    renderer,
                    renderer::Quad {
                        bounds: caret_bounds,
                        ..renderer::Quad::default()
                    },
                    if paint.caret.is_none() {
                        Color::TRANSPARENT
                    } else if selecting {
                        style.selection
                    } else {
                        style.value
                    }
                    .into(),
                );
            }
            let recorded = renderer.start_layout_recording(paragraph.presentation_node());
            renderer.fill_paragraph(
                paragraph.raw(),
                text_bounds.anchor(paragraph.min_bounds(), Alignment::Start, Alignment::Center)
                    + Vector::new(paint.offset(text_bounds.width), 0.0),
                // Composing text paints in the value colour even for an empty
                // model value; it is user input, not the placeholder.
                if composing || !value.is_empty() {
                    style.value
                } else {
                    style.placeholder
                },
                material_viewport,
            );
            if recorded {
                renderer.mark_recording(renderer::NodeId::STATIC);
            }
        };
        if let Some(scope) = scope {
            renderer.with_layout_layer(renderer::LayoutLayer::Clip(scope), mask, None, draw);
        } else {
            renderer.with_layer(mask, draw);
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TextInput<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::interactive(State::<Renderer::Paragraph>::new())
    }

    fn diff(&mut self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

        if self.on_input.is_none() {
            state.is_pasting = None;
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: Length::Shrink,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        self.layout(tree, renderer, limits, None);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        _viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

        operation.text_input(self.id.as_ref(), layout.bounds(), state);
        operation.focusable(self.id.as_ref(), layout.bounds(), state);
        state
            .presentation
            .set(self.presentation_paint(state, renderer, &self.value).0);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let before = Snapshot {
            value: self.value.to_string(),
            cursor: state::<Renderer>(tree).cursor,
        };
        {
            let history = &mut state::<Renderer>(tree).history;
            // The application changed the value itself: the history no longer
            // describes it.
            if history
                .expected
                .as_ref()
                .is_some_and(|expected| *expected != before.value)
            {
                history.clear();
            }
            history.restoring = false;
        }
        let typing = matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed { text: Some(text), .. })
                if text.chars().all(|c| !c.is_whitespace())
        );

        self.dispatch_event(tree, event, layout, cursor, renderer, shell, _viewport);

        let after = self.value.to_string();
        let history = &mut state::<Renderer>(tree).history;
        if history.restoring {
            history.expected = Some(after);
        } else if after != before.value {
            history.record(before, &after, typing, Instant::now());
        }
        self.offer_primary_selection(state::<Renderer>(tree), shell);
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        state
            .presentation
            .set(self.presentation_paint(state, renderer, &self.value).0);
    }

    fn frame_interest(&self, tree: &Tree) -> bool {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();

        self.last_status.is_none()
            || (state
                .is_focused
                .as_ref()
                .is_some_and(|focus| focus.is_window_focused)
                && matches!(state.cursor.state(&self.value), cursor::State::Index(_)))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.draw(tree, renderer, theme, layout, cursor, None, viewport);
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            if self.on_input.is_none() {
                mouse::Interaction::Idle
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message, Theme, Renderer> From<TextInput<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(
        text_input: TextInput<'a, Message, Theme, Renderer>,
    ) -> Element<'a, Message, Theme, Renderer> {
        Element::new(text_input)
    }
}

/// The content of the [`Icon`].
#[derive(Debug, Clone)]
pub struct Icon<Font> {
    /// The font that will be used to display the `code_point`.
    pub font: Font,
    /// The unicode code point that will be used as the icon.
    pub code_point: char,
    /// The font size of the content.
    pub size: Option<Pixels>,
    /// The spacing between the [`Icon`] and the text in a [`TextInput`].
    pub spacing: f32,
    /// The side of a [`TextInput`] where to display the [`Icon`].
    pub side: Side,
}

/// The side of a [`TextInput`].
#[derive(Debug, Clone)]
pub enum Side {
    /// The left side of a [`TextInput`].
    Left,
    /// The right side of a [`TextInput`].
    Right,
}

/// The state of a [`TextInput`].
#[derive(Debug, Default)]
pub struct State<P: text::Paragraph> {
    presentation: std::sync::Arc<layout::input::LivePaint>,
    value: paragraph::Plain<P>,
    placeholder: paragraph::Plain<P>,
    // Displayed text while a preedit is active; empty and never consulted
    // otherwise. The model `Value` and `Cursor` stay untouched until commit
    // or cancel.
    composition: paragraph::Plain<P>,
    icon: paragraph::Plain<P>,
    is_focused: Option<Focus>,
    is_dragging: Option<Drag>,
    is_pasting: Option<Paste>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    cursor: Cursor,
    keyboard_modifiers: keyboard::Modifiers,
    // The text last offered as the primary selection, so an unchanged
    // selection is not offered again on every event.
    primary_offered: Option<String>,
    // A middle click is waiting for the primary selection to paste.
    pasting_primary: bool,
    history: History,
    // TODO: Add stateful horizontal scrolling offset
}

/// How many edits a text input can undo.
const HISTORY_LIMIT: usize = 100;

/// Consecutive typing within this long is one undo step.
const TYPING_MERGE: std::time::Duration = std::time::Duration::from_secs(1);

/// The value and caret before one edit.
#[derive(Debug, Clone)]
struct Snapshot {
    value: String,
    cursor: Cursor,
}

/// Undo and redo for the edits this input made itself.
///
/// The input is controlled: the application owns the value. An edit the input
/// publishes is recorded; a value that changes for any other reason (the
/// application replaced or rejected it) clears the history, because restoring
/// across it would overwrite the application's own change.
#[derive(Debug, Clone, Default)]
struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The value after the last recorded edit or restore.
    expected: Option<String>,
    /// When the last typing edit happened, to merge a typing run.
    typing_at: Option<Instant>,
    /// The current update restored a snapshot; do not record it as an edit.
    restoring: bool,
}

impl History {
    fn record(&mut self, before: Snapshot, after: &str, typing: bool, now: Instant) {
        let merge = typing
            && self.redo.is_empty()
            && self
                .typing_at
                .is_some_and(|at| now.duration_since(at) < TYPING_MERGE)
            && !self.undo.is_empty();
        if !merge {
            self.undo.push(before);
            if self.undo.len() > HISTORY_LIMIT {
                let _ = self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing_at = typing.then_some(now);
        self.expected = Some(after.to_owned());
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl<P: text::Paragraph + Clone> Clone for State<P> {
    fn clone(&self) -> Self {
        let presentation = std::sync::Arc::new(layout::input::LivePaint::default());
        presentation.set(self.presentation.get());
        Self {
            presentation,
            value: self.value.clone(),
            placeholder: self.placeholder.clone(),
            composition: self.composition.clone(),
            icon: self.icon.clone(),
            is_focused: self.is_focused.clone(),
            is_dragging: self.is_dragging.clone(),
            is_pasting: self.is_pasting.clone(),
            preedit: self.preedit.clone(),
            last_click: self.last_click,
            cursor: self.cursor,
            keyboard_modifiers: self.keyboard_modifiers,
            primary_offered: self.primary_offered.clone(),
            pasting_primary: self.pasting_primary,
            history: self.history.clone(),
        }
    }
}

fn state<Renderer: text::Renderer>(tree: &mut Tree) -> &mut State<Renderer::Paragraph> {
    tree.state.downcast_mut::<State<Renderer::Paragraph>>()
}

#[derive(Debug, Clone)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

#[derive(Debug, Clone)]
enum Drag {
    Select,
    SelectWords {
        anchor: usize,
    },
    /// Pressed inside the selection at `origin` (cursor index `position`):
    /// a drag of the selection, or a click once released (enzICED).
    Text {
        origin: Point,
        position: usize,
    },
    /// The selection is being dragged by the platform.
    Carrying,
}

#[derive(Debug, Clone)]
enum Paste {
    /// The clipboard was asked for its text; `released` once a key went up
    /// before the answer came (enzICED).
    Reading { released: bool },
    /// The text pasted while the chord is held, repeated without a new read.
    Pasting(Value),
}

impl<P: text::Paragraph> State<P> {
    /// Creates a new [`State`], representing an unfocused [`TextInput`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the [`TextInput`] is currently focused or not.
    pub fn is_focused(&self) -> bool {
        self.is_focused.is_some()
    }

    /// Returns the [`Cursor`] of the [`TextInput`].
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Focuses the [`TextInput`].
    pub fn focus(&mut self) {
        let now = Instant::now();

        self.is_focused = Some(Focus {
            updated_at: now,
            now,
            is_window_focused: true,
        });

        self.move_cursor_to_end();
    }

    /// Unfocuses the [`TextInput`].
    pub fn unfocus(&mut self) {
        self.is_focused = None;
    }

    /// Moves the [`Cursor`] of the [`TextInput`] to the front of the input text.
    pub fn move_cursor_to_front(&mut self) {
        self.cursor.move_to(0);
    }

    /// Moves the [`Cursor`] of the [`TextInput`] to the end of the input text.
    pub fn move_cursor_to_end(&mut self) {
        self.cursor.move_to(usize::MAX);
    }

    /// Moves the [`Cursor`] of the [`TextInput`] to an arbitrary location.
    pub fn move_cursor_to(&mut self, position: usize) {
        self.cursor.move_to(position);
    }

    /// Selects all the content of the [`TextInput`].
    pub fn select_all(&mut self) {
        self.cursor.select_range(0, usize::MAX);
    }

    /// Selects the given range of the content of the [`TextInput`].
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.cursor.select_range(start, end);
    }
}

impl<P: text::Paragraph> operation::Focusable for State<P> {
    fn is_focused(&self) -> bool {
        State::is_focused(self)
    }

    fn focus(&mut self) {
        State::focus(self);
    }

    fn unfocus(&mut self) {
        State::unfocus(self);
    }
}

impl<P: text::Paragraph> operation::TextInput for State<P> {
    fn text(&self) -> text::Fragment<'_> {
        std::borrow::Cow::Borrowed(if self.value.content().is_empty() {
            self.placeholder.content()
        } else {
            self.value.content()
        })
    }

    fn move_cursor_to_front(&mut self) {
        State::move_cursor_to_front(self);
    }

    fn move_cursor_to_end(&mut self) {
        State::move_cursor_to_end(self);
    }

    fn move_cursor_to(&mut self, position: text::Position) {
        let position = Value::new(self.value.content()).grapheme_at_byte(position.index);

        State::move_cursor_to(self, position);
    }

    fn select_all(&mut self) {
        State::select_all(self);
    }

    fn select_range(&mut self, start: text::Position, end: text::Position) {
        let value = Value::new(self.value.content());

        State::select_range(
            self,
            value.grapheme_at_byte(start.index),
            value.grapheme_at_byte(end.index),
        );
    }

    /// The value as one line, each character's place in the shaped
    /// paragraph (from the text's start, before scrolling and padding), and
    /// the selection with its anchor first (enzICED).
    fn accessible_text(&self) -> Option<operation::text_input::AccessibleText> {
        use operation::text_input::{AccessibleLine, AccessibleText};
        let content = self.value.content();
        let mut line = AccessibleLine::new(content);
        let paragraph = self.value.raw();
        let count = line.characters.len();
        let x = |index: usize| paragraph.grapheme_position(0, index).map(|point| point.x);
        line.positions = (0..count)
            .map(|index| {
                let start = x(index)?;
                let end = x(index + 1).unwrap_or(start);
                Some((start.min(end), (end - start).abs()))
            })
            .collect();
        let size = paragraph.min_bounds();
        line.bounds = Some(Rectangle::new(Point::ORIGIN, size));
        let value = Value::new(content);
        let at = |grapheme: usize| text::Position {
            line: 0,
            index: line.byte_of_character(grapheme),
        };
        let selection = match self.cursor.state(&value) {
            cursor::State::Index(index) => (at(index), at(index)),
            cursor::State::Selection { start, end } => (at(start), at(end)),
        };
        Some(AccessibleText {
            lines: vec![line],
            selection: Some(selection),
        })
    }
}

/// Whether `value` holds a right-to-left script, so arrow keys must follow
/// the shaped line instead of the logical order.
fn has_right_to_left(value: &Value) -> bool {
    value.to_string().chars().any(|c| {
        matches!(
            u32::from(c),
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF
        )
    })
}

/// Whether the shaped line runs right to left: its logical start is drawn
/// to the right of its logical end.
fn is_right_to_left(paragraph: &impl text::Paragraph, value: &Value) -> bool {
    if !has_right_to_left(value) {
        return false;
    }
    let x = |index| paragraph.grapheme_position(0, index).map(|point| point.x);
    matches!((x(0), x(value.len())), (Some(start), Some(end)) if start > end)
}

/// The caret index drawn nearest to `from` on its left or right, if any.
fn visual_neighbor(
    paragraph: &impl text::Paragraph,
    len: usize,
    from: usize,
    rightward: bool,
) -> Option<usize> {
    let here = paragraph.grapheme_position(0, from)?.x;
    (0..=len)
        .filter(|&index| index != from)
        .filter_map(|index| Some((index, paragraph.grapheme_position(0, index)?.x)))
        .filter(|&(_, x)| {
            if rightward {
                x > here + 0.01
            } else {
                x < here - 0.01
            }
        })
        .min_by(|a, b| (a.1 - here).abs().total_cmp(&(b.1 - here).abs()))
        .map(|(index, _)| index)
}

/// Moves (or, with `extend`, selects) one caret stop to the left or right on
/// screen for text containing a right-to-left script, where logical and
/// visual order differ. Returns `false`, changing nothing, for text without
/// one, so purely left-to-right text keeps logical movement.
fn move_visually(
    paragraph: &impl text::Paragraph,
    cursor: &mut Cursor,
    value: &Value,
    rightward: bool,
    extend: bool,
) -> bool {
    if !has_right_to_left(value) {
        return false;
    }
    let (anchor, head) = match cursor.state(value) {
        cursor::State::Index(index) => (index, index),
        cursor::State::Selection { start, end } => (start, end),
    };
    if !extend && anchor != head {
        // A selection collapses to its edge on the side of the arrow.
        let x = |index| {
            paragraph
                .grapheme_position(0, index)
                .map_or(0.0, |point| point.x)
        };
        let anchor_is_right = x(anchor) > x(head);
        let target = if anchor_is_right == rightward {
            anchor
        } else {
            head
        };
        cursor.move_to(target);
        return true;
    }
    let Some(target) = visual_neighbor(paragraph, value.len(), head, rightward) else {
        return true;
    };
    if extend {
        cursor.select_range(anchor, target);
    } else {
        cursor.move_to(target);
    }
    true
}

fn offset<P: text::Paragraph>(text_bounds: Rectangle, value: &Value, state: &State<P>) -> f32 {
    if state.is_focused() {
        let cursor = state.cursor();

        let focus_position = match cursor.state(value) {
            cursor::State::Index(i) => i,
            cursor::State::Selection { end, .. } => end,
        };

        let (_, offset) =
            measure_cursor_and_scroll_offset(state.value.raw(), text_bounds, focus_position);

        offset
    } else {
        0.0
    }
}

fn measure_cursor_and_scroll_offset(
    paragraph: &impl text::Paragraph,
    text_bounds: Rectangle,
    cursor_index: usize,
) -> (f32, f32) {
    let grapheme_position = paragraph
        .grapheme_position(0, cursor_index)
        .unwrap_or(Point::ORIGIN);

    let offset = ((grapheme_position.x + 5.0) - text_bounds.width).max(0.0);

    (grapheme_position.x, offset)
}

/// Computes the position of the text cursor at the given X coordinate of
/// a [`TextInput`].
fn find_cursor_position<P: text::Paragraph>(
    text_bounds: Rectangle,
    value: &Value,
    state: &State<P>,
    x: f32,
) -> Option<usize> {
    let offset = offset(text_bounds, value, state);
    let value = value.to_string();

    let char_offset = state
        .value
        .raw()
        .hit_test(Point::new(x + offset, text_bounds.height / 2.0))
        .map(text::Hit::cursor)?;

    Some(
        unicode_segmentation::UnicodeSegmentation::graphemes(
            &value[..char_offset.min(value.len())],
            true,
        )
        .count(),
    )
}

// Reshapes `state.composition` when a preedit is active. Called at the
// preedit event edge so the composition paragraph is current before the same
// update batch constructs `InputMethod`; the ordinary layout path continues
// to keep it current for steady-state redraws.
fn reshape_composition<Renderer>(
    renderer: &Renderer,
    state: &mut State<Renderer::Paragraph>,
    text_bounds: Rectangle,
    value: &Value,
    font: Option<Font>,
    text_size: Option<Pixels>,
    line_height: text::LineHeight,
) where
    Renderer: text::Renderer,
{
    let Some(composition) = active_composition(value, state) else {
        return;
    };
    let font = font.unwrap_or_else(|| renderer.font());
    let text_size = text_size.unwrap_or_else(|| renderer.text_size());

    state.composition = paragraph::Plain::new(Text {
        font,
        line_height,
        content: composition.text,
        bounds: Size::new(f32::INFINITY, text_bounds.height),
        size: text_size,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
        ellipsis: text::Ellipsis::None,
        hint_factor: renderer.hint_factor(),
    });
}

/// The editable text viewport of an input's synthetic layout; its whole
/// bounds before a layout declared one.
fn text_area(layout: Layout, tree: &Tree) -> Rectangle {
    layout
        .synthetic(tree)
        .children()
        .next()
        .map_or(layout.bounds(), |text| text.bounds())
}

/// The owned caret of an input's synthetic layout, if it declared one.
fn caret_node(layout: Layout, tree: &Tree) -> Option<renderer::NodeId> {
    layout
        .synthetic(tree)
        .children()
        .next()?
        .children()
        .next()?
        .children()
        .nth(1)?
        .container_presentation_node()
}

fn replace_paragraph<Renderer>(
    renderer: &Renderer,
    state: &mut State<Renderer::Paragraph>,
    text_bounds: Rectangle,
    value: &Value,
    font: Option<Font>,
    text_size: Option<Pixels>,
    line_height: text::LineHeight,
) where
    Renderer: text::Renderer,
{
    let font = font.unwrap_or_else(|| renderer.font());
    let text_size = text_size.unwrap_or_else(|| renderer.text_size());

    state.value = paragraph::Plain::new(Text {
        font,
        line_height,
        content: value.to_string(),
        bounds: Size::new(f32::INFINITY, text_bounds.height),
        size: text_size,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
        ellipsis: text::Ellipsis::None,
        hint_factor: renderer.hint_factor(),
    });
}

/// One blink phase (shown or hidden), from the desktop's
/// [`preferences`](crate::core::preferences).
fn cursor_blink_interval_millis() -> u128 {
    crate::core::preferences::get()
        .blink_phase()
        .as_millis()
        .max(1)
}

/// After this long without caret activity the caret stays solid and the input
/// stops asking for blink frames, as GTK's `cursor-blink-timeout`.
fn cursor_blink_timeout_millis() -> u128 {
    crate::core::preferences::get()
        .cursor_blink_timeout
        .as_millis()
}

/// Whether the caret has stopped blinking at `at`: solid, with no blink clock.
/// A caret that does not blink (the desktop turned blinking off, or asked for
/// reduced motion) is always settled.
fn blink_settled(updated_at: Instant, at: Instant) -> bool {
    crate::core::preferences::get().caret_settled(at.saturating_duration_since(updated_at))
}

/// The caret's width: the desktop's, and at least one physical pixel.
pub(crate) fn caret_width(hint_factor: Option<f32>) -> f32 {
    let width = crate::core::preferences::get().caret_width;
    if renderer::CRISP {
        (1.0 / hint_factor.unwrap_or(1.0)).max(width)
    } else {
        width
    }
}

/// The possible status of a [`TextInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The [`TextInput`] can be interacted with.
    Active,
    /// The [`TextInput`] is being hovered.
    Hovered,
    /// The [`TextInput`] is focused.
    Focused {
        /// Whether the [`TextInput`] is hovered, while focused.
        is_hovered: bool,
    },
    /// The [`TextInput`] cannot be interacted with.
    Disabled,
}

/// The appearance of a text input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The [`Background`] of the text input.
    pub background: Background,
    /// The [`Border`] of the text input.
    pub border: Border,
    /// The [`Color`] of the icon of the text input.
    pub icon: Color,
    /// The [`Color`] of the placeholder of the text input.
    pub placeholder: Color,
    /// The [`Color`] of the value of the text input.
    pub value: Color,
    /// The [`Color`] of the selection of the text input.
    pub selection: Color,
}

/// The theme catalog of a [`TextInput`].
pub trait Catalog: Sized {
    /// The item class of the [`Catalog`].
    type Class<'a>;

    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`TextInput`].
///
/// This is just a boxed closure: `Fn(&Theme, Status) -> Style`.
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

/// The default style of a [`TextInput`].
pub fn default(theme: &Theme, status: Status) -> Style {
    let palette = theme.palette();

    let active = Style {
        background: Background::Color(palette.background.base.color),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: palette.background.strong.color,
        },
        icon: palette.background.weak.text,
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

fn alignment_offset(
    text_bounds_width: f32,
    text_min_width: f32,
    alignment: alignment::Horizontal,
) -> f32 {
    if text_min_width > text_bounds_width {
        0.0
    } else {
        match alignment {
            alignment::Horizontal::Left => 0.0,
            alignment::Horizontal::Center => (text_bounds_width - text_min_width) / 2.0,
            alignment::Horizontal::Right => text_bounds_width - text_min_width,
        }
    }
}

impl<P: text::Paragraph + 'static> tree::Interactive for State<P> {
    fn cancel_interaction(&mut self) {
        self.is_dragging = None;
        self.is_pasting = None;
        self.last_click = None;
    }
}

// Inline preedit composition against a `Value`. The widget rasters the
// composed text in place of the value or placeholder while a preedit is
// active; the model `Value` and `Cursor` are untouched until commit or
// cancel, so `input_method::Event::Commit` and `Closed` remain authoritative.
#[derive(Debug)]
struct Composition {
    // Head + preedit + tail, in that layout.
    text: String,
    // Visual caret only; never written back to the model `Cursor`.
    caret_grapheme_index: usize,
}

impl Composition {
    // Visual caret sits at `preedit.selection.start`, or at the end of
    // `preedit.content` when the IME sent no selection. Byte offsets are
    // floored to a UTF-8 boundary before segmentation, so a fixture sending
    // a mid-scalar offset does not panic.
    fn build(value: &Value, cursor: &Cursor, preedit: &input_method::Preedit) -> Self {
        let (repl_start, repl_end) = match cursor.state(value) {
            cursor::State::Index(i) => (i, i),
            cursor::State::Selection { start, end } => (start.min(end), start.max(end)),
        };
        let head = value.until(repl_start).to_string();
        let tail = value.select(repl_end, value.len()).to_string();

        let mut text = String::with_capacity(head.len() + preedit.content.len() + tail.len());
        text.push_str(&head);
        text.push_str(&preedit.content);
        text.push_str(&tail);

        let preedit_caret_byte = preedit
            .selection
            .as_ref()
            .map(|range| range.start)
            .unwrap_or(preedit.content.len())
            .min(preedit.content.len());

        let mut byte = preedit_caret_byte;
        while byte > 0 && !preedit.content.is_char_boundary(byte) {
            byte -= 1;
        }

        let preedit_head_graphemes =
            UnicodeSegmentation::graphemes(&preedit.content[..byte], true).count();

        Self {
            text,
            caret_grapheme_index: repl_start + preedit_head_graphemes,
        }
    }
}

// `Some` only while focused with an active window: a preedit that survives
// a focus transfer must not paint into another field's frame.
fn active_composition<P: text::Paragraph>(value: &Value, state: &State<P>) -> Option<Composition> {
    let focus = state.is_focused.as_ref()?;
    if !focus.is_window_focused {
        return None;
    }
    let preedit = state.preedit.as_ref()?;
    if preedit.content.is_empty() {
        return None;
    }
    Some(Composition::build(value, &state.cursor, preedit))
}

#[cfg(test)]
mod preference_tests {
    use super::*;
    use crate::core::preferences::{self, Preferences};

    #[test]
    fn the_caret_follows_the_desktop_blink_and_width() {
        let start = Instant::now();
        let soon = start + Duration::from_millis(700);
        assert!(!blink_settled(start, soon), "the caret blinks by default");
        assert!(blink_settled(start, start + Duration::from_secs(10)));
        let still = Preferences {
            cursor_blink: false,
            caret_width: 2.0,
            ..Preferences::DEFAULT
        };
        preferences::scoped(still, || {
            assert!(blink_settled(start, soon), "blinking off: solid at once");
            assert_eq!(caret_width(Some(1.0)), 2.0);
        });
        let calm = Preferences {
            reduced_motion: true,
            ..Preferences::DEFAULT
        };
        preferences::scoped(calm, || assert!(blink_settled(start, soon)));
        assert_eq!(caret_width(Some(1.0)), 1.0);
    }
}
