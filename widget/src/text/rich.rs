// Modified by enzUI contributors. See FORK.md.
use crate::core::alignment;
use crate::core::border;
use crate::core::layout;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::text::{Paragraph, Span};
use crate::core::widget::text::{
    self, Alignment, Catalog, Ellipsis, LineHeight, Shaping, Style, StyleFn, Wrapping,
};
use crate::core::widget::tree::{self, Tree};
use crate::core::{
    self, Background, Border, Color, Element, Event, Font, Layout, Length, Pixels, Point,
    Rectangle, Shell, Size, Vector, Widget,
};

use std::cell::RefCell;

/// Resolves a [`Span::color_key`] against the live theme.
///
/// [`Span::color_key`]: core::text::Span::color_key
type ColorResolver<'a, Theme> = Box<dyn Fn(&Theme, u64) -> Option<Color> + 'a>;

/// Paint overrides for a linked span, independent of its font and hit geometry.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinkStyle {
    /// Override foreground ink; `None` preserves the span's resolved colour.
    pub color: Option<Color>,
    /// Add an underline; authored span underlines remain visible.
    pub underline: bool,
}

type LinkStyleResolver<'a, Theme> = Box<dyn Fn(&Theme, usize, bool) -> LinkStyle + 'a>;

/// A bunch of [`Rich`] text.
pub struct Rich<'a, Link, Message, Theme = crate::Theme>
where
    Link: Clone + 'static,
    Theme: Catalog,
{
    spans: Box<dyn AsRef<[Span<'a, Link>]> + 'a>,
    size: Option<Pixels>,
    line_height: Option<LineHeight>,
    width: Length,
    height: Length,
    font: Option<Font>,
    align_x: Alignment,
    align_y: alignment::Vertical,
    wrapping: Wrapping,
    ellipsis: Ellipsis,
    class: Theme::Class<'a>,
    hovered_link: Option<usize>,
    on_link_click: Option<Box<dyn Fn(Link) -> Message + 'a>>,
    resolve_color: Option<ColorResolver<'a, Theme>>,
    link_style: Option<LinkStyleResolver<'a, Theme>>,
}

impl<'a, Link, Message, Theme> Rich<'a, Link, Message, Theme>
where
    Link: Clone + 'static,
    Theme: Catalog,
{
    /// Creates a new empty [`Rich`] text.
    pub fn new() -> Self {
        Self {
            spans: Box::new([]),
            size: None,
            line_height: None,
            width: Length::Fit,
            height: Length::Fit,
            font: None,
            align_x: Alignment::Default,
            align_y: alignment::Vertical::Top,
            wrapping: Wrapping::default(),
            ellipsis: Ellipsis::default(),
            class: Theme::default(),
            hovered_link: None,
            on_link_click: None,
            resolve_color: None,
            link_style: None,
        }
    }

    /// Creates a new [`Rich`] text with the given text spans.
    pub fn with_spans(spans: impl AsRef<[Span<'a, Link>]> + 'a) -> Self {
        Self {
            spans: Box::new(spans),
            ..Self::new()
        }
    }

    /// Sets the default size of the [`Rich`] text.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the default [`LineHeight`] of the [`Rich`] text.
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.line_height = Some(line_height.into());
        self
    }

    /// Sets the default font of the [`Rich`] text.
    pub fn font(mut self, font: impl Into<Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    /// Sets the width of the [`Rich`] text boundaries.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Rich`] text boundaries.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Centers the [`Rich`] text, both horizontally and vertically.
    pub fn center(self) -> Self {
        self.align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
    }

    /// Sets the [`alignment::Horizontal`] of the [`Rich`] text.
    pub fn align_x(mut self, alignment: impl Into<Alignment>) -> Self {
        self.align_x = alignment.into();
        self
    }

    /// Sets the [`alignment::Vertical`] of the [`Rich`] text.
    pub fn align_y(mut self, alignment: impl Into<alignment::Vertical>) -> Self {
        self.align_y = alignment.into();
        self
    }

    /// Sets the [`Wrapping`] strategy of the [`Rich`] text.
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// Sets the [`Ellipsis`] strategy of the [`Rich`] text.
    pub fn ellipsis(mut self, ellipsis: Ellipsis) -> Self {
        self.ellipsis = ellipsis;
        self
    }

    /// Sets the message that will be produced when a link of the [`Rich`] text
    /// is clicked.
    ///
    /// If the spans of the [`Rich`] text contain no links, you may need to call
    /// this method with `on_link_click(never)` in order for the compiler to infer
    /// the proper `Link` generic type.
    pub fn on_link_click(mut self, on_link_click: impl Fn(Link) -> Message + 'a) -> Self {
        self.on_link_click = Some(Box::new(on_link_click));
        self
    }

    /// Resolve linked-span ink and underline at draw time using its index and hover state.
    /// The override replaces automatic hover underlining, preserves authored underlines,
    /// and leaves activation, fonts, selection and paragraph geometry unchanged.
    #[must_use]
    pub fn link_style(mut self, resolve: impl Fn(&Theme, usize, bool) -> LinkStyle + 'a) -> Self {
        self.link_style = Some(Box::new(resolve));
        self
    }

    /// Sets the resolver used to turn a [`Span::color_key`] into a [`Color`]
    /// against the **live theme**, at draw time.
    ///
    /// # Why this exists
    ///
    /// A [`Span`] carries its colour as a baked [`Color`], chosen when the view
    /// is built. A *semantic* colour — "muted", "accent" — is really a lookup
    /// into the active theme's palette, so baking it pins the run to whichever
    /// theme happened to be active at build time, and a later theme switch
    /// leaves it painting the old palette's ink on the new palette's
    /// background. [`style`] cannot rescue it: that colour applies to the whole
    /// paragraph and is only consulted where [`Span::color`] is `None`.
    ///
    /// Give each semantic run a [`Span::color_key`] and install a resolver
    /// here, and the colour is resolved per draw from the `&Theme` in hand.
    ///
    /// The resolver returning `None` for a key means "no opinion": the span
    /// falls back to its own [`Span::color`], and then to [`style`].
    ///
    /// Spans without a [`Span::color_key`] are untouched, so installing a
    /// resolver never changes the appearance of existing text.
    ///
    /// # Cost
    ///
    /// Colour does not affect text metrics, so re-resolving cannot move a
    /// glyph. The paragraph is re-shaped only when the *resolved* colours
    /// actually change — i.e. on a theme switch — not on every frame.
    ///
    /// [`Span`]: core::text::Span
    /// [`Span::color`]: core::text::Span::color
    /// [`Span::color_key`]: core::text::Span::color_key
    /// [`style`]: Self::style
    #[must_use]
    pub fn resolve_color(mut self, resolve: impl Fn(&Theme, u64) -> Option<Color> + 'a) -> Self {
        self.resolve_color = Some(Box::new(resolve));
        self
    }

    /// Sets the default style of the [`Rich`] text.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the default [`Color`] of the [`Rich`] text.
    pub fn color(self, color: impl Into<Color>) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.color_maybe(Some(color))
    }

    /// Sets the default [`Color`] of the [`Rich`] text, if `Some`.
    pub fn color_maybe(self, color: Option<impl Into<Color>>) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        let color = color.map(Into::into);

        self.style(move |_theme| Style { color })
    }

    /// Sets the default style class of the [`Rich`] text.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

impl<'a, Link, Message, Theme> Default for Rich<'a, Link, Message, Theme>
where
    Link: Clone + 'a,
    Theme: Catalog,
{
    fn default() -> Self {
        Self::new()
    }
}

struct State<Link, P: Paragraph> {
    spans: Vec<Span<'static, Link>>,
    span_pressed: Option<usize>,
    paragraph: P,
    /// Bumped whenever `paragraph` is re-shaped or resized, so a cached
    /// [`Themed`] paragraph built from an older layout can be detected as
    /// stale.
    version: u64,
    /// The paragraph re-shaped with theme-resolved span colours baked in.
    ///
    /// Written from `draw`, which only has `&Tree`, hence the [`RefCell`] —
    /// the same shape `qr_code` and `markdown` already use to cache
    /// theme-dependent work.
    themed: RefCell<Option<Themed<P>>>,
}

/// A paragraph re-shaped with theme-resolved span colours, plus the inputs it
/// was built from so it can be reused until one of them changes.
struct Themed<P> {
    colors: Vec<Option<Color>>,
    version: u64,
    paragraph: P,
}

impl<Link, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Rich<'_, Link, Message, Theme>
where
    Link: Clone + 'static,
    Theme: Catalog,
    Renderer: core::text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link, Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link, _> {
            spans: Vec::new(),
            span_pressed: None,
            paragraph: Renderer::Paragraph::default(),
            version: 0,
            themed: RefCell::new(None),
        })
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        tree.size = layout(
            tree.state
                .downcast_mut::<State<Link, Renderer::Paragraph>>(),
            renderer,
            limits,
            self.width,
            self.height,
            self.spans.as_ref().as_ref(),
            self.line_height,
            self.size,
            self.font,
            self.align_x,
            self.align_y,
            self.wrapping,
            self.ellipsis,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if !layout.bounds().intersects(viewport) {
            return;
        }

        let state = tree
            .state
            .downcast_ref::<State<Link, Renderer::Paragraph>>();

        let style = theme.style(&self.class);

        let spans = self.spans.as_ref().as_ref();
        let link_styles: Vec<Option<LinkStyle>> = match &self.link_style {
            Some(resolve) => spans
                .iter()
                .enumerate()
                .map(|(index, span)| {
                    span.link
                        .as_ref()
                        .map(|_| resolve(theme, index, self.hovered_link == Some(index)))
                })
                .collect(),
            None => Vec::new(),
        };

        // Rebaking resolved ink preserves the metrics established during layout.
        let resolved: Vec<Option<Color>> =
            if self.resolve_color.is_some() || self.link_style.is_some() {
                spans
                    .iter()
                    .enumerate()
                    .map(|(index, span)| {
                        link_styles
                            .get(index)
                            .copied()
                            .flatten()
                            .and_then(|style| style.color)
                            .or_else(|| {
                                self.resolve_color.as_ref().and_then(|resolve| {
                                    span.color_key.and_then(|key| resolve(theme, key))
                                })
                            })
                    })
                    .collect()
            } else {
                Vec::new()
            };

        if resolved.iter().any(Option::is_some) {
            let mut themed = state.themed.borrow_mut();

            let is_current = themed
                .as_ref()
                .is_some_and(|themed| themed.version == state.version && themed.colors == resolved);

            if !is_current {
                let recolored: Vec<_> = spans
                    .iter()
                    .zip(&resolved)
                    .map(|(span, resolved)| {
                        let mut span = span.clone();
                        span.color = resolved.or(span.color);
                        span
                    })
                    .collect();

                *themed = Some(Themed {
                    colors: resolved.clone(),
                    version: state.version,
                    paragraph: Renderer::Paragraph::with_spans(core::Text {
                        content: recolored.as_slice(),
                        bounds: state.paragraph.bounds(),
                        size: state.paragraph.size(),
                        line_height: state.paragraph.line_height(),
                        font: state.paragraph.font(),
                        align_x: state.paragraph.align_x(),
                        align_y: state.paragraph.align_y(),
                        shaping: state.paragraph.shaping(),
                        wrapping: state.paragraph.wrapping(),
                        ellipsis: state.paragraph.ellipsis(),
                        hint_factor: state.paragraph.hint_factor(),
                    }),
                });
            }
        } else {
            // No key resolved: drop any cache so a later theme switch back does
            // not resurrect a stale paragraph.
            let _ = state.themed.borrow_mut().take();
        }

        for (index, span) in spans.iter().enumerate() {
            let is_hovered_link = self.on_link_click.is_some() && Some(index) == self.hovered_link;
            let underline = span.underline
                || link_styles
                    .get(index)
                    .copied()
                    .flatten()
                    .map_or(is_hovered_link, |style| style.underline);

            if span.highlight.is_some() || underline || span.strikethrough {
                // Span regions are relative to the paragraph's top, which
                // `text::draw` places by the vertical alignment (enzICED).
                let bounds = layout.bounds();
                let height = state.paragraph.min_bounds().height;
                let shift = match state.paragraph.align_y() {
                    alignment::Vertical::Top => 0.0,
                    alignment::Vertical::Center => (bounds.height - height) / 2.0,
                    alignment::Vertical::Bottom => bounds.height - height,
                };
                let translation = layout.position() - Point::ORIGIN + Vector::new(0.0, shift);
                let regions = state.paragraph.span_bounds(index);

                if let Some(highlight) = span.highlight {
                    // enzICED: a keyed highlight follows the live theme.
                    let background = match (&self.resolve_color, span.highlight_key) {
                        (Some(resolve), Some(key)) => {
                            resolve(theme, key).map_or(highlight.background, Background::from)
                        }
                        _ => highlight.background,
                    };

                    for (i, bounds) in regions.iter().enumerate() {
                        let starts = i == 0;
                        let ends = i + 1 == regions.len();

                        // The horizontal padding belongs to the start and end
                        // of the span, not to each of its lines
                        let left = if starts { span.padding.left } else { 0.0 };
                        let right = if ends { span.padding.right } else { 0.0 };

                        let bounds = Rectangle::new(
                            bounds.position() - Vector::new(left, span.padding.top),
                            bounds.size() + Size::new(left + right, span.padding.y()),
                        );

                        let radius = border::Radius {
                            top_left: highlight.border.radius.top_left * f32::from(starts),
                            bottom_left: highlight.border.radius.bottom_left * f32::from(starts),
                            top_right: highlight.border.radius.top_right * f32::from(ends),
                            bottom_right: highlight.border.radius.bottom_right * f32::from(ends),
                        };

                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: bounds + translation,
                                border: Border {
                                    radius,
                                    ..highlight.border
                                },
                                ..Default::default()
                            },
                            background,
                        );
                    }
                }

                if underline || span.strikethrough {
                    let size = span.size.or(self.size).unwrap_or(renderer.text_size());

                    let line_height = span
                        .line_height
                        .or(self.line_height)
                        .unwrap_or_else(|| renderer.line_height())
                        .to_absolute(size);

                    let color = resolved
                        .get(index)
                        .copied()
                        .flatten()
                        .or(span.color)
                        .or(style.color)
                        .unwrap_or(defaults.text_color);

                    let baseline =
                        translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);

                    if underline {
                        for bounds in &regions {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline,
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }

                    if span.strikethrough {
                        for bounds in &regions {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline
                                            - Vector::new(0.0, size.0 / 2.0),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }
                }
            }
        }

        let themed = state.themed.borrow();

        text::draw(
            renderer,
            defaults,
            layout.bounds(),
            themed
                .as_ref()
                .map_or(&state.paragraph, |themed| &themed.paragraph),
            style,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        _viewport: &Rectangle,
        _renderer: &Renderer,
        operation: &mut dyn core::widget::Operation,
    ) {
        let state = tree
            .state
            .downcast_ref::<State<Link, Renderer::Paragraph>>();
        let plain_text: String = state.spans.iter().map(|span| span.text.as_ref()).collect();
        operation.selectable_text(None, layout.bounds(), &plain_text, &state.paragraph);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if self.on_link_click.is_none() && self.link_style.is_none() {
            return;
        }

        let was_hovered = self.hovered_link;

        if let Some(position) = cursor.position_in(layout.bounds()) {
            let state = tree
                .state
                .downcast_ref::<State<Link, Renderer::Paragraph>>();

            self.hovered_link = state.paragraph.hit_span(position).and_then(|span| {
                if self.spans.as_ref().as_ref().get(span)?.link.is_some() {
                    Some(span)
                } else {
                    None
                }
            });
        } else {
            self.hovered_link = None;
        }

        if was_hovered != self.hovered_link {
            shell.request_redraw();
        }

        let Some(on_link_clicked) = &self.on_link_click else {
            return;
        };

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let state = tree
                    .state
                    .downcast_mut::<State<Link, Renderer::Paragraph>>();

                if self.hovered_link.is_some() {
                    state.span_pressed = self.hovered_link;
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let state = tree
                    .state
                    .downcast_mut::<State<Link, Renderer::Paragraph>>();

                match state.span_pressed {
                    Some(span) if Some(span) == self.hovered_link => {
                        if let Some(link) = self
                            .spans
                            .as_ref()
                            .as_ref()
                            .get(span)
                            .and_then(|span| span.link.clone())
                        {
                            shell.publish(on_link_clicked(link));
                        }
                    }
                    _ => {}
                }

                state.span_pressed = None;
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.hovered_link.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

fn layout<Link, Renderer>(
    state: &mut State<Link, Renderer::Paragraph>,
    renderer: &Renderer,
    limits: &layout::Limits,
    width: Length,
    height: Length,
    spans: &[Span<'_, Link>],
    line_height: Option<LineHeight>,
    size: Option<Pixels>,
    font: Option<Font>,
    align_x: Alignment,
    align_y: alignment::Vertical,
    wrapping: Wrapping,
    ellipsis: Ellipsis,
) -> Size
where
    Link: Clone,
    Renderer: core::text::Renderer,
{
    layout::sized(limits, width, height, |limits| {
        let bounds = limits.bounds();

        let size = size.unwrap_or_else(|| renderer.text_size());
        let font = font.unwrap_or_else(|| renderer.font());
        let line_height = line_height.unwrap_or_else(|| renderer.line_height());

        let text_with_spans = || core::Text {
            content: spans,
            bounds,
            size,
            line_height,
            font,
            align_x,
            align_y,
            shaping: Shaping::Advanced,
            wrapping,
            ellipsis,
            hint_factor: renderer.hint_factor(),
        };

        if state.spans != spans {
            state.paragraph = Renderer::Paragraph::with_spans(text_with_spans());
            state.spans = spans.iter().cloned().map(Span::to_static).collect();
            state.version = state.version.wrapping_add(1);
        } else {
            match state.paragraph.compare(core::Text {
                content: (),
                bounds,
                size,
                line_height,
                font,
                align_x,
                align_y,
                shaping: Shaping::Advanced,
                wrapping,
                ellipsis,
                hint_factor: renderer.hint_factor(),
            }) {
                core::text::Difference::None => {}
                core::text::Difference::Bounds => {
                    state.paragraph.resize(bounds);
                    state.version = state.version.wrapping_add(1);
                }
                core::text::Difference::Shape => {
                    state.paragraph = Renderer::Paragraph::with_spans(text_with_spans());
                    state.version = state.version.wrapping_add(1);
                }
            }
        }

        state.paragraph.min_bounds()
    })
}

impl<'a, Link, Message, Theme> FromIterator<Span<'a, Link>> for Rich<'a, Link, Message, Theme>
where
    Link: Clone + 'a,
    Theme: Catalog,
{
    fn from_iter<T: IntoIterator<Item = Span<'a, Link>>>(spans: T) -> Self {
        Self::with_spans(spans.into_iter().collect::<Vec<_>>())
    }
}

impl<'a, Link, Message, Theme, Renderer> From<Rich<'a, Link, Message, Theme>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Link: Clone + 'a,
    Theme: Catalog + 'a,
    Renderer: core::text::Renderer + 'a,
{
    fn from(text: Rich<'a, Link, Message, Theme>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(text)
    }
}
