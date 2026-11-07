// Modified by enzUI contributors. See FORK.md.
//! Operate on widgets that have text input.
use crate::Rectangle;
use crate::text;
use crate::widget::Id;
use crate::widget::operation::Operation;

/// The internal state of a widget that has text input.
pub trait TextInput {
    /// Returns the current _visible_ text of the text input.
    ///
    /// Normally, this is either its value or its placeholder.
    fn text(&self) -> text::Fragment<'_>;

    /// Moves the cursor of the text input to the front of the input text.
    fn move_cursor_to_front(&mut self);

    /// Moves the cursor of the text input to the end of the input text.
    fn move_cursor_to_end(&mut self);

    /// Moves the cursor of the text input to an arbitrary location.
    fn move_cursor_to(&mut self, location: text::Position);

    /// Selects all the content of the text input.
    fn select_all(&mut self);

    /// Selects the given content range of the text input.
    fn select_range(&mut self, start: text::Position, end: text::Position);

    /// The laid-out value and selection, for assistive technology
    /// (enzICED). `None` when the input does not describe it.
    fn accessible_text(&self) -> Option<AccessibleText> {
        None
    }
}

/// The text of an input as assistive technology reads it (enzICED): its
/// lines, where each character sits, and the selection.
///
/// A character here is a grapheme cluster, the smallest unit the caret
/// moves over.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccessibleText {
    /// The lines, top to bottom.
    pub lines: Vec<AccessibleLine>,
    /// The selection anchor and the caret; equal for a bare caret. `None`
    /// when the input has no caret.
    pub selection: Option<(text::Position, text::Position)>,
}

/// One line of an [`AccessibleText`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccessibleLine {
    /// The line's text, without its line break.
    pub text: String,
    /// The UTF-8 length of each character, in order.
    pub characters: Vec<usize>,
    /// Each character's horizontal offset from the line's start and its
    /// width, in logical pixels, when the input knows its layout.
    pub positions: Option<Vec<(f32, f32)>>,
    /// The line's box relative to the input's text origin, when known.
    pub bounds: Option<Rectangle>,
}

impl AccessibleLine {
    /// A line of `text` with its characters and no layout.
    pub fn new(text: impl Into<String>) -> Self {
        use unicode_segmentation::UnicodeSegmentation;
        let text = text.into();
        let characters = text.graphemes(true).map(str::len).collect();
        Self {
            text,
            characters,
            positions: None,
            bounds: None,
        }
    }

    /// The character index holding byte `index` (the line's length at the
    /// end).
    pub fn character_at_byte(&self, index: usize) -> usize {
        let mut start = 0;
        for (i, length) in self.characters.iter().enumerate() {
            if index < start + length {
                return i;
            }
            start += length;
        }
        self.characters.len()
    }

    /// The byte index where character `index` starts (the line's length past
    /// the end).
    pub fn byte_of_character(&self, index: usize) -> usize {
        self.characters.iter().take(index).sum()
    }
}

/// Produces an [`Operation`] that moves the cursor of the widget with the given [`Id`] to the
/// front.
pub fn move_cursor_to_front<T>(target: Id) -> impl Operation<T> {
    struct MoveCursor {
        target: Id,
    }

    impl<T> Operation<T> for MoveCursor {
        fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
            match id {
                Some(id) if id == &self.target => {
                    state.move_cursor_to_front();
                }
                _ => {}
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    MoveCursor { target }
}

/// Produces an [`Operation`] that moves the cursor of the widget with the given [`Id`] to the
/// end.
pub fn move_cursor_to_end<T>(target: Id) -> impl Operation<T> {
    struct MoveCursor {
        target: Id,
    }

    impl<T> Operation<T> for MoveCursor {
        fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
            match id {
                Some(id) if id == &self.target => {
                    state.move_cursor_to_end();
                }
                _ => {}
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    MoveCursor { target }
}

/// Produces an [`Operation`] that moves the cursor of the widget with the given [`Id`] to the
/// provided position.
pub fn move_cursor_to<T>(target: Id, position: text::Position) -> impl Operation<T> {
    struct MoveCursor {
        target: Id,
        position: text::Position,
    }

    impl<T> Operation<T> for MoveCursor {
        fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
            match id {
                Some(id) if id == &self.target => {
                    state.move_cursor_to(self.position);
                }
                _ => {}
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    MoveCursor { target, position }
}

/// Produces an [`Operation`] that selects all the content of the widget with the given [`Id`].
pub fn select_all<T>(target: Id) -> impl Operation<T> {
    struct MoveCursor {
        target: Id,
    }

    impl<T> Operation<T> for MoveCursor {
        fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
            match id {
                Some(id) if id == &self.target => {
                    state.select_all();
                }
                _ => {}
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    MoveCursor { target }
}

/// Produces an [`Operation`] that selects the given content range of the widget with the given [`Id`].
pub fn select_range<T>(
    target: Id,
    start: text::Position,
    end: text::Position,
) -> impl Operation<T> {
    struct SelectRange {
        target: Id,
        start: text::Position,
        end: text::Position,
    }

    impl<T> Operation<T> for SelectRange {
        fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
            match id {
                Some(id) if id == &self.target => {
                    state.select_range(self.start, self.end);
                }
                _ => {}
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    SelectRange { target, start, end }
}
