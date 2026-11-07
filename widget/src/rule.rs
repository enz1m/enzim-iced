// Modified by enzUI contributors. See FORK.md.
//! Rules divide space horizontally or vertically.
//!
//! # Example
//! ```no_run
//! # mod iced { pub mod widget { pub use iced_widget::*; } }
//! # pub type State = ();
//! # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
//! use iced::widget::rule;
//!
//! #[derive(Clone)]
//! enum Message {
//!     // ...,
//! }
//!
//! fn view(state: &State) -> Element<'_, Message> {
//!     rule::horizontal(2).into()
//! }
//! ```
use crate::core;
use crate::core::border;
use crate::core::layout;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::widget::Tree;
use crate::core::{Color, Element, Layout, Length, Pixels, Rectangle, Size, Theme, Widget};

/// Creates a new horizontal [`Rule`] with the given height.
pub fn horizontal<'a, Theme>(height: impl Into<Pixels>) -> Rule<'a, Theme>
where
    Theme: Catalog,
{
    Rule {
        thickness: Length::Fixed(height.into().0),
        is_vertical: false,
        class: Theme::default(),
    }
}

/// Creates a new vertical [`Rule`] with the given width.
pub fn vertical<'a, Theme>(width: impl Into<Pixels>) -> Rule<'a, Theme>
where
    Theme: Catalog,
{
    Rule {
        thickness: Length::Fixed(width.into().0),
        is_vertical: true,
        class: Theme::default(),
    }
}

/// Display a horizontal or vertical rule for dividing content.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } }
/// # pub type State = ();
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// use iced::widget::rule;
///
/// #[derive(Clone)]
/// enum Message {
///     // ...,
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     rule::horizontal(2).into()
/// }
/// ```
pub struct Rule<'a, Theme = crate::Theme>
where
    Theme: Catalog,
{
    thickness: Length,
    is_vertical: bool,
    class: Theme::Class<'a>,
}

impl<'a, Theme> Rule<'a, Theme>
where
    Theme: Catalog,
{
    /// Sets the style of the [`Rule`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`Rule`].
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Rule<'_, Theme>
where
    Renderer: core::Renderer,
    Theme: Catalog,
{
    fn size(&self) -> Size<Length> {
        if self.is_vertical {
            Size {
                width: self.thickness,
                height: Length::Fill,
            }
        } else {
            Size {
                width: Length::Fill,
                height: self.thickness,
            }
        }
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) {
        let size = <Self as Widget<(), Theme, Renderer>>::size(self);

        layout::paint_box(limits, size.width, size.height).apply(tree);
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let style = theme.style(&self.class);

        let paint = layout::rule::Paint {
            vertical: self.is_vertical,
            fill: style.fill_mode,
            snap: style.snap,
        };
        let bounds = paint.bounds(bounds, renderer.hint_factor().unwrap_or(1.0));

        layout.fill_presentation_rule(
            _tree,
            renderer,
            renderer::Quad {
                bounds,
                border: border::rounded(style.radius),
                snap: style.snap,
                ..renderer::Quad::default()
            },
            style.color.into(),
            paint,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Rule<'a, Theme>> for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a + Catalog,
    Renderer: 'a + core::Renderer,
{
    fn from(rule: Rule<'a, Theme>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(rule)
    }
}

/// The appearance of a rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The color of the rule.
    pub color: Color,
    /// The radius of the line corners.
    pub radius: border::Radius,
    /// The [`FillMode`] of the rule.
    pub fill_mode: FillMode,
    /// Whether the rule should be snapped to the pixel grid.
    pub snap: bool,
}

pub use crate::core::layout::rule::FillMode;

/// The theme catalog of a [`Rule`].
pub trait Catalog: Sized {
    /// The item class of the [`Catalog`].
    type Class<'a>;

    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`Rule`].
///
/// This is just a boxed closure: `Fn(&Theme, Status) -> Style`.
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> Style {
        class(self)
    }
}

/// The default styling of a [`Rule`].
pub fn default(theme: &Theme) -> Style {
    let palette = theme.palette();

    Style {
        color: palette.background.strong.color,
        radius: 0.0.into(),
        fill_mode: FillMode::Full,
        snap: true,
    }
}

/// A [`Rule`] styling using the weak background color.
pub fn weak(theme: &Theme) -> Style {
    let palette = theme.palette();

    Style {
        color: palette.background.weak.color,
        radius: 0.0.into(),
        fill_mode: FillMode::Full,
        snap: true,
    }
}
