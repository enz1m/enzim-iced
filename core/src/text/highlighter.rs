// Modified by enzUI contributors. See FORK.md.
//! Highlight text.
use crate::font;
use crate::{Color, Font};

/// A type that describes how to highlight an `Input`
/// with some [`Style`].
pub trait Highlighter<Input, Theme = crate::Theme> {
    /// A unique identifier for the highlighter.
    fn id(&self) -> &str;

    /// Returns the [`Style`] of the given `Input`.
    fn highlight(&self, input: Input, theme: &Theme) -> Style;
}

/// The style of some highlighted text.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Style {
    /// The [`Color`] of the text.
    pub color: Option<Color>,
    /// The [`font::Style`] of the text.
    pub style: Option<font::Style>,
    /// Replaces the editor family, weight and style for this run.
    /// An explicit [`style`](Self::style) overrides this font's style.
    pub font: Option<Font>,
    /// Per-run size in logical pixels; absent or invalid sizes inherit the editor size.
    /// Row height follows the editor's base line-height ratio.
    pub size: Option<crate::Pixels>,
    /// Whether the text carries an underline (enzICED).
    pub underline: bool,
    /// Whether the text carries a strike-through line (enzICED).
    pub strikethrough: bool,
}

impl<T, Input, Theme> Highlighter<Input, Theme> for T
where
    T: Fn(Input, &Theme) -> Style,
{
    fn id(&self) -> &str {
        std::any::type_name_of_val(self) // Hack: Best effort
    }

    fn highlight(&self, input: Input, theme: &Theme) -> Style {
        (self)(input, theme)
    }
}

#[cfg(test)]
mod tests {
    use super::Style;

    // Every other producer reaches these fields through `..Style::default()`.
    #[test]
    fn style_decorations_default_off() {
        let style = Style::default();

        assert!(style.font.is_none());
        assert!(!style.underline);
        assert!(!style.strikethrough);
    }
}
