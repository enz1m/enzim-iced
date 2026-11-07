// Modified by enzUI contributors. See FORK.md.
//! Scrollables let users navigate an endless amount of content with a scrollbar.
//!
//! # Example
//! ```no_run
//! # mod iced { pub mod widget { pub use iced_widget::*; } }
//! # pub type State = ();
//! # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
//! use iced::widget::{column, scrollable, space};
//!
//! enum Message {
//!     // ...
//! }
//!
//! fn view(state: &State) -> Element<'_, Message> {
//!     scrollable(column![
//!         "Scroll me!",
//!         space().height(3000),
//!         "You did it!",
//!     ]).into()
//! }
//! ```
use crate::container;
use crate::core::alignment;
use crate::core::border::{self, Border};
use crate::core::keyboard;
use crate::core::layout;
use crate::core::layout::Measure as _;
use crate::core::mouse;
use crate::core::overlay;
use crate::core::renderer;
use crate::core::text;
use crate::core::time::{Duration, Instant};
use crate::core::touch;
use crate::core::widget;
use crate::core::widget::operation::{self, Animation, Operation};
use crate::core::widget::tree::{self, Tree};
use crate::core::window;
use crate::core::{
    self, Background, Color, Element, Event, InputMethod, Layout, Length, Padding, Pixels, Point,
    Rectangle, Shadow, Shell, Size, Theme, Vector, Widget,
};

use std::cell::Cell;

pub use operation::scrollable::{AbsoluteOffset, RelativeOffset};

/// A widget that can vertically display an infinite amount of content with a
/// scrollbar.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub use iced_widget::*; } }
/// # pub type State = ();
/// # pub type Element<'a, Message> = iced_widget::core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;
/// use iced::widget::{column, scrollable, space};
///
/// enum Message {
///     // ...
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     scrollable(column![
///         "Scroll me!",
///         space().height(3000),
///         "You did it!",
///     ]).into()
/// }
/// ```
///
/// # Scrollbars
///
/// A scrollbar has two interactive parts: the *scroller* (the draggable
/// thumb) and the *rail* (the track it slides on).
///
/// * Pressing and dragging the **scroller** moves it 1:1 with the pointer.
/// * Pressing the **rail** scrolls one page (animated like a wheel scroll
///   when [`Self::smooth_scroll`] is enabled). While the button is held, the
///   scroller then slides toward the pointer at a constant speed, stopping
///   when its edge reaches the pointer; moving the pointer moves the target
///   with it. This mirrors the track autoscroll of most toolkits.
/// * `Shift`-clicking the **rail** instead jumps the scroller so that its
///   center is under the pointer, and then drags it from there.
///
/// With [`Self::click_to_scroll`] enabled, the behavior of the rail press
/// and the `Shift`-click is inverted.
pub struct Scrollable<'a, Message, Theme = crate::Theme, Renderer = crate::Renderer>
where
    Theme: Catalog,
    Renderer: core::Renderer,
{
    id: Option<widget::Id>,
    width: Length,
    height: Length,
    direction: Direction,
    auto_scroll: bool,
    smooth_scroll: bool,
    /// Resolves [`Self::smooth_scroll`] from the theme at each draw (enzICED).
    smooth_scroll_policy: Option<Box<dyn Fn(&Theme) -> bool + 'a>>,
    click_to_scroll: bool,
    content: Element<'a, Message, Theme, Renderer>,
    on_scroll: Option<Box<dyn Fn(Scroll) -> Action<Message> + 'a>>,
    /// Whether a horizontal-only viewport change notifies `on_scroll` on the
    /// next redraw (enzUI). `true` for compatibility; vertical-only wrappers
    /// disable it with [`Scrollable::on_vertical_scroll`] so a responsive
    /// width relayout does not manufacture an application message.
    notify_horizontal_viewport_changes: bool,
    class: Theme::Class<'a>,
    initial_offset: Option<AbsoluteOffset>,
    /// Where a layout that changes the content's size puts the vertical
    /// offset; see [`Scrollable::anchor`] (enzICED).
    anchor: Option<Box<dyn Fn(Size, Size) -> Option<f32> + 'a>>,
}

impl<'a, Message, Theme, Renderer> Scrollable<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Creates a new vertical [`Scrollable`].
    pub fn new(content: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self::with_direction(content, Direction::default())
    }

    /// Creates a new [`Scrollable`] with the given [`Direction`].
    pub fn with_direction(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        direction: impl Into<Direction>,
    ) -> Self {
        Scrollable {
            id: None,
            width: Length::Fit,
            height: Length::Fit,
            direction: direction.into(),
            auto_scroll: false,
            // enzICED: off by default; see `smooth_scroll`.
            smooth_scroll: false,
            smooth_scroll_policy: None,
            click_to_scroll: false,
            content: content.into(),
            on_scroll: None,
            class: Theme::default(),
            initial_offset: None,
            notify_horizontal_viewport_changes: true,
            anchor: None,
        }
    }

    /// Seed the offset a **freshly constructed** [`Scrollable`] state starts at.
    ///
    /// This is applied exactly once, the first time a given widget state lays
    /// out, and never again for the life of that state. It is therefore *not*
    /// a controlled/bound offset: after the first pass the scrollable owns its
    /// position and ordinary gestures, `scroll_to` operations and the scrollbar
    /// are completely unaffected. Re-applying every frame would fight the user
    /// mid-drag, which is why the seed is state-scoped rather than prop-scoped.
    ///
    /// Note this is keyed on the state's first layout, not on the value: a
    /// caller that passes 0 at mount and a real offset later (i.e. the user
    /// scrolled and the application recorded it) is never re-seeded, because
    /// that state has already had its first layout.
    ///
    /// # Why this exists
    ///
    /// iced keys retained widget state by tree position, so re-parenting a
    /// scrollable — mounting a modal that wraps the page, collapsing a split
    /// pane — throws its state away and the replacement starts at the top even
    /// though the application still knows where the user was. The usual repair,
    /// a post-layout `scroll_to` operation, cannot help in exactly the case
    /// that matters most: a modal host makes its background inert, and an inert
    /// subtree is deliberately skipped by `operate`, so the operation never
    /// reaches the scrollable and the page paints at 0 behind the scrim.
    ///
    /// Layout, unlike `operate`, always traverses. Seeding here therefore makes
    /// an application-owned offset authoritative at precisely the moment the
    /// widget state is reconstructed, including while inert, without giving up
    /// the widget's ownership of the offset at any other time.
    pub fn initial_offset(mut self, offset: AbsoluteOffset) -> Self {
        self.initial_offset = Some(offset);
        self
    }

    /// Keeps a logical position in view while the content reflows (enzICED).
    ///
    /// Whenever a layout changes the content's size, `anchor` receives the
    /// laid-out content size and the viewport size and returns the vertical
    /// offset that keeps the caller's anchor where it was (a document page's
    /// top at the new width), or `None` to leave the offset alone. The offset
    /// is applied and drawn in that same layout, so a reflow that runs every
    /// frame (a sidebar sliding beside the document) never paints a frame at
    /// the stale offset. A layout that does not change the content's size
    /// leaves the offset to the user, so scrolling is never overridden.
    pub fn anchor(mut self, anchor: impl Fn(Size, Size) -> Option<f32> + 'a) -> Self {
        self.anchor = Some(Box::new(anchor));
        self
    }

    /// Makes the [`Scrollable`] scroll horizontally, with default [`Scrollbar`] settings.
    pub fn horizontal(self) -> Self {
        self.direction(Direction::Horizontal(Scrollbar::default()))
    }

    /// Sets the [`Direction`] of the [`Scrollable`].
    pub fn direction(mut self, direction: impl Into<Direction>) -> Self {
        self.direction = direction.into();
        self
    }

    /// Sets the [`widget::Id`] of the [`Scrollable`].
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Sets the width of the [`Scrollable`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Scrollable`].
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets a handler to call when the [`Scrollable`] is scrolled.
    ///
    /// The function takes a [`Scroll`], which contains the [`Viewport`] of
    /// the [`Scrollable`] and the [`Source`] of the scroll that caused the
    /// notification.
    pub fn on_scroll<T>(mut self, f: impl Fn(Scroll) -> T + 'a) -> Self
    where
        T: Into<Action<Message>>,
    {
        self.on_scroll = Some(Box::new(move |scroll| f(scroll).into()));
        self
    }

    /// Calls `f` after vertical scroll or vertical viewport/extent changes
    /// (enzUI).
    ///
    /// Unlike [`Scrollable::on_scroll`], a redraw that changes only horizontal
    /// bounds does not publish a message. This is useful for vertical virtual
    /// lists: responsive width layout remains inside the retained UI, while a
    /// real wheel, drag, touch, keyboard, programmatic offset, viewport-height,
    /// or content-height change is still reported.
    #[must_use]
    pub fn on_vertical_scroll<T>(mut self, f: impl Fn(Scroll) -> T + 'a) -> Self
    where
        T: Into<Action<Message>>,
    {
        self.on_scroll = Some(Box::new(move |scroll| f(scroll).into()));
        self.notify_horizontal_viewport_changes = false;
        self
    }

    /// Anchors the vertical [`Scrollable`] direction to the top.
    pub fn anchor_top(self) -> Self {
        self.anchor_y(Anchor::Start)
    }

    /// Anchors the vertical [`Scrollable`] direction to the bottom.
    pub fn anchor_bottom(self) -> Self {
        self.anchor_y(Anchor::End)
    }

    /// Anchors the horizontal [`Scrollable`] direction to the left.
    pub fn anchor_left(self) -> Self {
        self.anchor_x(Anchor::Start)
    }

    /// Anchors the horizontal [`Scrollable`] direction to the right.
    pub fn anchor_right(self) -> Self {
        self.anchor_x(Anchor::End)
    }

    /// Sets the [`Anchor`] of the horizontal direction of the [`Scrollable`], if applicable.
    pub fn anchor_x(mut self, alignment: Anchor) -> Self {
        match &mut self.direction {
            Direction::Horizontal(horizontal) | Direction::Both { horizontal, .. } => {
                horizontal.alignment = alignment;
            }
            Direction::Vertical { .. } => {}
        }

        self
    }

    /// Sets the [`Anchor`] of the vertical direction of the [`Scrollable`], if applicable.
    pub fn anchor_y(mut self, alignment: Anchor) -> Self {
        match &mut self.direction {
            Direction::Vertical(vertical) | Direction::Both { vertical, .. } => {
                vertical.alignment = alignment;
            }
            Direction::Horizontal { .. } => {}
        }

        self
    }

    /// Embeds the [`Scrollbar`] into the [`Scrollable`], instead of floating on top of the
    /// content.
    ///
    /// The `spacing` provided will be used as space between the [`Scrollbar`] and the contents
    /// of the [`Scrollable`].
    pub fn spacing(mut self, new_spacing: impl Into<Pixels>) -> Self {
        match &mut self.direction {
            Direction::Horizontal(scrollbar) | Direction::Vertical(scrollbar) => {
                scrollbar.spacing = Some(new_spacing.into().0);
            }
            Direction::Both { .. } => {}
        }

        self
    }

    /// Adds padding at the ends of the [`Scrollbar`]s of the [`Scrollable`].
    ///
    /// The `padding` provided will be used as space at the top and bottom of a
    /// vertical [`Scrollbar`], and at the left and right ends of a horizontal
    /// [`Scrollbar`], when they are visible.
    ///
    /// Unlike [`Self::spacing`], the padding does not affect the layout of the
    /// [`Scrollable`].
    pub fn padding(mut self, new_padding: impl Into<Pixels>) -> Self {
        let padding = new_padding.into().0;

        match &mut self.direction {
            Direction::Horizontal(scrollbar) | Direction::Vertical(scrollbar) => {
                *scrollbar = scrollbar.padding(padding);
            }
            Direction::Both {
                horizontal,
                vertical,
            } => {
                *horizontal = horizontal.padding(padding);
                *vertical = vertical.padding(padding);
            }
        }

        self
    }

    /// Inset scrollbar tracks independently of content layout, in logical pixels.
    /// Top/bottom affect vertical tracks; left/right affect horizontal tracks.
    /// Negative and nonfinite values become zero; oversized insets clamp to the
    /// available track, leading edge first. A collapsed track has no pointer target.
    pub fn scrollbar_insets(mut self, insets: impl Into<Padding>) -> Self {
        let insets = insets.into();
        match &mut self.direction {
            Direction::Vertical(bar) => *bar = bar.padding_ends(insets.top, insets.bottom),
            Direction::Horizontal(bar) => *bar = bar.padding_ends(insets.left, insets.right),
            Direction::Both {
                horizontal,
                vertical,
            } => {
                *horizontal = horizontal.padding_ends(insets.left, insets.right);
                *vertical = vertical.padding_ends(insets.top, insets.bottom);
            }
        }
        self
    }

    /// Sets whether the user should be allowed to auto-scroll the [`Scrollable`]
    /// with the middle mouse button.
    ///
    /// By default, it is disabled.
    pub fn auto_scroll(mut self, auto_scroll: bool) -> Self {
        self.auto_scroll = auto_scroll;
        self
    }

    /// Sets whether wheel scrolling should be smoothed out over time, instead of
    /// moving the [`Scrollable`] immediately.
    ///
    /// When enabled, discrete scrolls (e.g. from a mouse wheel) move a target
    /// scroll offset — carrying the momentum of the wheel notches — and the
    /// [`Scrollable`] eases towards it over a few frames. High-precision
    /// scrolls (e.g. from a touchpad), which are already smooth, are always
    /// applied immediately.
    ///
    /// The setting also acts as the default behavior of the scroll operations
    /// (`snap_to`, `scroll_to`, `scroll_by`): operations with
    /// [`Animation::Auto`] scroll smoothly if and only if it is enabled.
    ///
    /// By default, it is disabled (enzICED; upstream enables it). An
    /// animated scroll moves over later frames, so a toolkit must first
    /// route it through its motion policy (reduced motion, frame
    /// presentation); until then every wheel step lands at once.
    pub fn smooth_scroll(mut self, smooth_scroll: bool) -> Self {
        self.smooth_scroll = smooth_scroll;
        self
    }

    /// Decides [`Self::smooth_scroll`] from the theme instead of a fixed flag
    /// (enzICED), so a toolkit can route it through its motion policy.
    ///
    /// The policy runs at every draw and its answer holds until the next
    /// one; before the first draw the [`Scrollable`] scrolls immediately. A
    /// smooth scroll still in flight when the policy turns it off lands on
    /// its target at once instead of finishing the animation.
    pub fn smooth_scroll_policy(mut self, policy: impl Fn(&Theme) -> bool + 'a) -> Self {
        self.smooth_scroll_policy = Some(Box::new(policy));
        self
    }

    /// The smooth scrolling setting in force for this `state`.
    fn resolved_smooth_scroll(&self, state: &State) -> bool {
        match self.smooth_scroll_policy {
            Some(_) => state.themed_smooth_scroll.get().unwrap_or(false),
            None => self.smooth_scroll,
        }
    }

    /// Sets whether a plain click on the scrollbar rail should jump the
    /// scroller to the click position, instead of scrolling one page.
    ///
    /// By default, a press on the rail scrolls one page (and autoscrolls
    /// while the button is held), while a `Shift`-click jumps the scroller
    /// so that its center is under the pointer and drags it from there.
    ///
    /// When enabled, the behavior is inverted: a plain click jumps the
    /// scroller so that its center is under the pointer (and drags it from
    /// there), while a `Shift`-click scrolls one page (and autoscrolls
    /// while the button is held).
    ///
    /// By default, it is disabled.
    pub fn click_to_scroll(mut self, click_to_scroll: bool) -> Self {
        self.click_to_scroll = click_to_scroll;
        self
    }

    /// Sets the style of this [`Scrollable`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`Scrollable`].
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

/// The direction of [`Scrollable`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// Vertical scrolling
    Vertical(Scrollbar),
    /// Horizontal scrolling
    Horizontal(Scrollbar),
    /// Both vertical and horizontal scrolling
    Both {
        /// The properties of the vertical scrollbar.
        vertical: Scrollbar,
        /// The properties of the horizontal scrollbar.
        horizontal: Scrollbar,
    },
}

impl Direction {
    /// enzICED: how far each rail end must move in so its painted scroller
    /// clears the rounded corners of the clips around the scrollable (a
    /// rounded window or pane, see [`crate::core::clip_shape`]): vertical
    /// leading and trailing, then horizontal. Measured where those clips are
    /// known (event handling) and kept, so layout, drawing and owned
    /// presentation all lay the rails out alike.
    fn corner_clearance(self, bounds: Rectangle, content: Size) -> [f32; 4] {
        let shown_x = self.horizontal().filter(|_| content.width > bounds.width);
        let shown_y = self.vertical().filter(|_| content.height > bounds.height);
        let mut clearance = [0.0; 4];
        if let Some(bar) = shown_y {
            let below = shown_x.map_or(0.0, |h| h.width.max(h.scroller_width) + h.margin);
            let band = bar.width.max(bar.scroller_width) + 2.0 * bar.margin;
            let side = bounds.x + bounds.width - band / 2.0 - bar.scroller_width / 2.0;
            let (leading, trailing) = crate::core::clip_shape::rail_ends(
                true,
                (side, side + bar.scroller_width),
                (bounds.y, bounds.y + (bounds.height - below).max(0.0)),
            );
            clearance[0] = leading;
            clearance[1] = trailing;
        }
        if let Some(bar) = shown_x {
            let beside = shown_y.map_or(0.0, |v| v.width.max(v.scroller_width) + 2.0 * v.margin);
            let band = bar.width.max(bar.scroller_width) + 2.0 * bar.margin;
            let side = bounds.y + bounds.height - band / 2.0 - bar.scroller_width / 2.0;
            let (leading, trailing) = crate::core::clip_shape::rail_ends(
                false,
                (side, side + bar.scroller_width),
                (bounds.x, bounds.x + (bounds.width - beside).max(0.0)),
            );
            clearance[2] = leading;
            clearance[3] = trailing;
        }
        clearance
    }

    /// These rails with their ends moved in by at least `clearance` (see
    /// [`Self::corner_clearance`]).
    fn with_clearance(mut self, clearance: [f32; 4]) -> Self {
        let pad = |bar: &mut Scrollbar, leading: f32, trailing: f32| {
            bar.padding = [bar.padding[0].max(leading), bar.padding[1].max(trailing)];
        };
        match &mut self {
            Self::Vertical(bar) => pad(bar, clearance[0], clearance[1]),
            Self::Horizontal(bar) => pad(bar, clearance[2], clearance[3]),
            Self::Both {
                vertical,
                horizontal,
            } => {
                pad(vertical, clearance[0], clearance[1]);
                pad(horizontal, clearance[2], clearance[3]);
            }
        }
        self
    }

    // Owned presentation receives the same rail sizing as native interaction.
    fn config(&self) -> layout::scroll::Config {
        let convert = |bar: &Scrollbar| layout::scroll::Bar {
            width: bar.width,
            margin: bar.margin,
            scroller_width: bar.scroller_width,
            scroller_min_length: bar.scroller_min_length,
            from_end: bar.alignment == Anchor::End,
            spacing: bar.spacing,
            padding: bar.padding,
        };
        layout::scroll::Config {
            horizontal: self.horizontal().map(convert),
            vertical: self.vertical().map(convert),
        }
    }

    /// Returns the horizontal [`Scrollbar`], if any.
    pub fn horizontal(&self) -> Option<&Scrollbar> {
        match self {
            Self::Horizontal(scrollbar) => Some(scrollbar),
            Self::Both { horizontal, .. } => Some(horizontal),
            Self::Vertical(_) => None,
        }
    }

    /// Returns the vertical [`Scrollbar`], if any.
    pub fn vertical(&self) -> Option<&Scrollbar> {
        match self {
            Self::Vertical(scrollbar) => Some(scrollbar),
            Self::Both { vertical, .. } => Some(vertical),
            Self::Horizontal(_) => None,
        }
    }

    fn align(&self, delta: Vector) -> Vector {
        let horizontal_alignment = self.horizontal().map(|p| p.alignment).unwrap_or_default();

        let vertical_alignment = self.vertical().map(|p| p.alignment).unwrap_or_default();

        let align = |alignment: Anchor, delta: f32| match alignment {
            Anchor::Start => delta,
            Anchor::End => -delta,
        };

        Vector::new(
            align(horizontal_alignment, delta.x),
            align(vertical_alignment, delta.y),
        )
    }
}

impl Default for Direction {
    fn default() -> Self {
        Self::Vertical(Scrollbar::default())
    }
}

/// A scrollbar within a [`Scrollable`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scrollbar {
    width: f32,
    margin: f32,
    scroller_width: f32,
    scroller_min_length: f32,
    alignment: Anchor,
    spacing: Option<f32>,
    padding: [f32; 2],
}

impl Default for Scrollbar {
    fn default() -> Self {
        Self {
            width: 10.0,
            margin: 0.0,
            scroller_width: 10.0,
            scroller_min_length: 2.0,
            alignment: Anchor::Start,
            spacing: None,
            padding: [0.0; 2],
        }
    }
}

impl Scrollbar {
    /// Creates new [`Scrollbar`] for use in a [`Scrollable`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a [`Scrollbar`] with zero width to allow a [`Scrollable`] to scroll without a visible
    /// scroller.
    pub fn hidden() -> Self {
        Self::default().width(0).scroller_width(0)
    }

    /// Sets the scrollbar width of the [`Scrollbar`] .
    pub fn width(mut self, width: impl Into<Pixels>) -> Self {
        self.width = width.into().0.max(0.0);
        self
    }

    /// Sets the scrollbar margin of the [`Scrollbar`] .
    pub fn margin(mut self, margin: impl Into<Pixels>) -> Self {
        self.margin = margin.into().0;
        self
    }

    /// Sets the scroller width of the [`Scrollbar`] .
    pub fn scroller_width(mut self, scroller_width: impl Into<Pixels>) -> Self {
        self.scroller_width = scroller_width.into().0.max(0.0);
        self
    }

    /// Sets the minimum length of the scroller along its movement axis
    /// (enzUI #1833).
    ///
    /// The requested minimum is clamped to the available track. When the
    /// proportional scroller is shorter than this minimum, its position is
    /// mapped over the remaining travel so it still reaches both ends exactly.
    /// A scroller that fills a short track has no drag travel and ignores drag
    /// movement until the track can provide travel again.
    pub fn scroller_min_length(mut self, min_length: impl Into<Pixels>) -> Self {
        self.scroller_min_length = min_length.into().0.max(0.0);
        self
    }

    /// Sets the [`Anchor`] of the [`Scrollbar`] .
    pub fn anchor(mut self, alignment: Anchor) -> Self {
        self.alignment = alignment;
        self
    }

    /// Sets whether the [`Scrollbar`] should be embedded in the [`Scrollable`], using
    /// the given spacing between itself and the contents.
    ///
    /// An embedded [`Scrollbar`] will always be displayed, will take layout space,
    /// and will not float over the contents.
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = Some(spacing.into().0);
        self
    }

    /// Sets the padding of the [`Scrollbar`].
    ///
    /// The padding is added at the top and bottom of the scrollbar (or at the
    /// left and right ends for a horizontal [`Scrollbar`]) when it is visible.
    ///
    /// Unlike [`Self::margin`] and [`Self::spacing`], the padding does not
    /// affect the layout of the [`Scrollable`].
    pub fn padding(self, padding: impl Into<Pixels>) -> Self {
        let padding = padding.into().0;
        self.padding_ends(padding, padding)
    }

    /// Inset the leading and trailing track ends without moving the content.
    /// Values are logical pixels along the travel axis, independent of the
    /// content anchor. Negative and nonfinite values become zero. Oversized
    /// insets clamp to the available track, leading edge first.
    pub fn padding_ends(mut self, leading: impl Into<Pixels>, trailing: impl Into<Pixels>) -> Self {
        self.padding = [leading.into().0, trailing.into().0].map(|value| {
            if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            }
        });
        self
    }
}

/// The anchor of the scroller of the [`Scrollable`] relative to its [`Viewport`]
/// on a given axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Scroller is anchored to the start of the [`Viewport`].
    #[default]
    Start,
    /// Content is aligned to the end of the [`Viewport`].
    End,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Scrollable<'_, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::interactive(State::new())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));

        let state = tree.state.downcast_mut::<State>();

        if state.last_id != self.id {
            *state = State {
                last_id: self.id.clone(),
                ..State::default()
            };
        }

        let size = self.content.as_widget().size();

        self.width = self.width.stack(size.width);
        self.height = self.height.stack(size.height);
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) {
        // Consume `initial_offset` on the FIRST layout of a widget state, and
        // never again for that state's life.
        //
        // `seeded` deliberately means "this state has laid out before", not "a
        // seed was available", and is flipped even when no seed was given. That
        // distinction matters: a caller whose offset is 0 at mount and non-zero
        // later (the normal case -- the user simply scrolled) must NOT be seeded
        // on that later frame, or the seed would snap a live gesture back to the
        // last published offset. Only a state reconstructed by a tree reset is
        // unseeded, which is exactly when the caller's offset should win.
        //
        // An over-large seed is safe: `Offset::absolute` clamps against the
        // measured content on read, so a stale offset past a now-shorter list
        // resolves to its end rather than scrolling into empty space.
        {
            let state = tree.state.downcast_mut::<State>();

            if !state.seeded {
                state.seeded = true;

                if let Some(initial) = self.initial_offset {
                    if initial.y > 0.0 {
                        state.offset_y = Offset::Absolute(initial.y);
                    }

                    if initial.x > 0.0 {
                        state.offset_x = Offset::Absolute(initial.x);
                    }
                }
            }
        }

        // enzUI-ICED #2795: the shared scroll allocation rule, so owned
        // presentation reflows this scrollable exactly as native layout does.
        let visible = tree.state.downcast_ref::<State>().is_scrollbar_visible;
        let direction = self
            .direction
            .with_clearance(tree.state.downcast_ref::<State>().corner_clearance);
        let result = direction.config().resolve(
            limits,
            Size::new(self.width, self.height),
            visible,
            |limits| {
                Ok::<_, std::convert::Infallible>(self.content.as_widget_mut().measure(
                    &mut tree.children[0],
                    renderer,
                    limits,
                ))
            },
        );
        let (node, visible) = match result {
            Ok(result) => result,
            Err(error) => match error {},
        };
        let state = tree.state.downcast_mut::<State>();
        state.is_scrollbar_visible = visible;

        // enzUI-ICED #1510: a relayout that changes this viewport or its
        // content (the content grew on a rebuild at the same window size)
        // is answered on the next redraw: a snapped target re-resolves and
        // the change is reported. So it owes that frame a route.
        let geometry = (node.size(), node.children()[0].size());
        let content_changed = state
            .laid_out
            .is_none_or(|(_, content)| content != geometry.1);
        if content_changed
            && let Some(anchor) = &self.anchor
            && let Some(y) = anchor(geometry.1, geometry.0)
            && y.is_finite()
        {
            // Drawn now, not at the next redraw: the frame this layout paints
            // already shows the anchor where it was.
            state.offset_y = Offset::Absolute(y.max(0.0));
            state.target = None;
            state.last_translation =
                state.translation(self.direction, Rectangle::with_size(geometry.0), geometry.1);
        }
        if state.laid_out != Some(geometry) {
            state.laid_out = Some(geometry);
            // The next frame reports the change; the interest `diff` cached
            // predates this layout (enzICED).
            state.layout_notice = true;
            tree.raise_frame_interest();
        }

        node.with_scroll_presentation(
            *limits,
            Size::new(self.width, self.height),
            self.content.as_widget().size(),
            direction.config(),
        )
        .apply(tree);
    }

    // enzUI-ICED #1510: this scrollable needs presented frames while it
    // animates itself (middle-button autoscroll, or a smooth scroll heading
    // for its target), or while its content does.
    fn frame_interest(&self, tree: &Tree) -> bool {
        let state = tree.state.downcast_ref::<State>();

        // enzUI-ICED #2231: a descendant may cache "no frame interest"
        // because it was offscreen (a viewport-gated animation), and moving
        // the offset gives it no chance to notice. Upstream also pins the
        // drawn translation to the last presented frame, advanced only by a
        // redraw that reaches this scrollable. So while the content owes a
        // dispatch with a new viewport, this scrollable stays on the route.
        // A scrollable that has never dispatched its content owes its first
        // redraw too: that frame pins the translation and reports the initial
        // layout (`Source::Content`).
        state.pending_content_route
            || state.layout_notice
            || state.status_pending
            || state.dispatched_translation.is_none()
            || matches!(
                state.interaction,
                Interaction::AutoScrolling { .. } | Interaction::RailHeld(_)
            )
            || state.target.is_some()
            || state.hover_hold.is_some()
            || tree.children.first().is_none_or(Tree::has_frame_interest)
    }

    // enzICED: a frame that reaches this scrollable only to answer its own
    // state (a layout notice, an animation step) is routed into the content
    // by the content's frame interest, instead of walking it; a content that
    // owes a dispatch with a new translation still gets the event in full.
    fn update_frame(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        {
            let state = tree.state.downcast_mut::<State>();
            let released = state.release_hover_hold(event, cursor);
            if released {
                shell.request_redraw();
            }
            state.full_content_dispatch =
                released || state.pending_content_route || state.dispatched_translation.is_none();
        }

        self.update(tree, event, layout, cursor, renderer, shell, viewport);

        tree.state.downcast_mut::<State>().full_content_dispatch = true;
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout,
        viewport: &Rectangle,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let scroll_node = tree.scroll_presentation_node();
        let content_layout = layout.children(&tree.children).next().unwrap();
        let state = tree.state.downcast_mut::<State>();
        let direction = self.direction.with_clearance(state.corner_clearance);

        // `Animation::Auto` scroll operations resolve against this
        state.smooth_scroll = self.resolved_smooth_scroll(state);
        state.notify_horizontal_viewport_changes = self.notify_horizontal_viewport_changes;

        let bounds = layout.bounds();
        let content_bounds = content_layout.bounds();
        let content = content_layout.size();
        // enzICED: an operation observes the live translation, not the last
        // presented one. Operations run between frames (a `scroll_to` and the
        // reveal or accessibility pass after it), and must see the state the
        // next frame presents; `update` and `draw` stay pinned to the last
        // drawn frame, so they still agree with each other.
        let translation = state.translation(self.direction, bounds, content);
        let viewport = viewport.intersection(&bounds).unwrap_or_default() + translation;

        if operation.is_native_reconciliation() {
            // enzUI-ICED #2795: reconcile against the presented scroll state.
            if let Some(node) = scroll_node
                && operation.needs_scroll_reconciliation(node)
            {
                let input = state.scroll_input(renderer.hint_factor().unwrap_or(1.0));
                let result = renderer.reconcile_presented_scroll(
                    node,
                    direction.config(),
                    input,
                    bounds.size(),
                    content,
                );
                if let Some(visible) = result {
                    state.is_scrollbar_visible = visible;
                    state.pending_content_route = true;
                }
                operation.scroll_reconciled(node, result.is_some());
            }
        } else {
            operation.scrollable(self.id.as_ref(), bounds, content, translation, state);
        }
        operation.container(self.id.as_ref(), content_bounds, &viewport);

        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                content_layout,
                &viewport,
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        state.notify_horizontal_viewport_changes = self.notify_horizontal_viewport_changes;
        // Hover returns with a repaint.
        if state.release_hover_hold(event, cursor) {
            shell.request_redraw();
        }
        let smooth_scroll = self.resolved_smooth_scroll(state);
        if !smooth_scroll {
            state.settle();
        }
        let bounds = layout.bounds();
        let cursor_over_scrollable = cursor.position_over(bounds);

        let child = layout.children(&tree.children).next().unwrap();
        let content = child.size();

        // enzICED: rails keep clear of the rounded corners around them,
        // measured here, where the clips are known (see `corner_clearance`).
        let clearance = self.direction.corner_clearance(bounds, content);
        if clearance != state.corner_clearance {
            state.corner_clearance = clearance;
            shell.invalidate_layout();
            shell.request_redraw();
        }
        let direction = self.direction.with_clearance(state.corner_clearance);

        let translation = state.last_translation;
        let scrollbars = Scrollbars::new(translation, direction, bounds, content);
        let mouse_over_scrollbar = scrollbars.is_mouse_over(cursor);
        let last_offsets = (state.offset_x, state.offset_y);

        let interact = state.interact(
            event,
            bounds,
            content,
            cursor,
            cursor_over_scrollable,
            &scrollbars,
            mouse_over_scrollbar,
            direction,
            smooth_scroll,
            self.click_to_scroll,
        );

        if let Some(scroll) = interact.scroll
            && let Some(on_scroll) = &self.on_scroll
        {
            on_scroll(scroll).perform(state, bounds, content, shell);

            if let Event::Window(window::Event::RedrawRequested(_now)) = event {
                state.last_translation = state.translation(self.direction, bounds, content);
            }
        }

        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            state.layout_notice = false;
        }

        if interact.capture {
            shell.capture_event();
        }

        if interact.invalidate_layout {
            shell.invalidate_layout();
        }

        if interact.request_redraw {
            shell.request_redraw();
        }

        if !interact.stop {
            if let Some(Content { cursor }) = interact.content {
                let had_input_method = shell.input_method().is_enabled();

                let viewport =
                    bounds.intersection(viewport).unwrap_or_default() + state.last_translation;

                if state.full_content_dispatch {
                    self.content.as_widget_mut().update(
                        &mut tree.children[0],
                        event,
                        child,
                        cursor,
                        renderer,
                        shell,
                        &viewport,
                    );
                    state.dispatched_translation = Some(state.last_translation);
                } else if tree.children[0].has_frame_interest() {
                    self.content.as_widget_mut().update_frame(
                        &mut tree.children[0],
                        event,
                        child,
                        cursor,
                        renderer,
                        shell,
                        &viewport,
                    );
                }
                tree.children[0].refresh_frame_interest(self.content.as_widget());

                if !had_input_method
                    && let InputMethod::Enabled { cursor, .. } = shell.input_method_mut()
                {
                    *cursor -= state.last_translation;
                }
            }

            let update = state.update(
                event,
                bounds,
                content,
                cursor,
                cursor_over_scrollable,
                mouse_over_scrollbar,
                direction,
                smooth_scroll,
                self.auto_scroll,
                shell.is_event_captured(),
            );

            if let Some(scroll) = update.scroll
                && let Some(on_scroll) = &self.on_scroll
            {
                on_scroll(scroll).perform(state, bounds, content, shell);
            }

            if update.capture {
                shell.capture_event();
            }

            if update.invalidate_layout {
                shell.invalidate_layout();
            }

            if update.request_redraw {
                shell.request_redraw();
            }

            if let Some(until) = update.hover_hold_until {
                shell.schedule_redraw_at(until);
            }
        }

        let status = if let Some(axis) = state.interaction.axis() {
            Status::Dragged {
                is_horizontal_scrollbar_dragged: axis == Axis::X,
                is_vertical_scrollbar_dragged: axis == Axis::Y,
                is_horizontal_scrollbar_disabled: scrollbars.is_x_disabled(),
                is_vertical_scrollbar_disabled: scrollbars.is_y_disabled(),
            }
        } else if cursor_over_scrollable.is_some() {
            Status::Hovered {
                is_horizontal_scrollbar_hovered: mouse_over_scrollbar == Some(Axis::X),
                is_vertical_scrollbar_hovered: mouse_over_scrollbar == Some(Axis::Y),
                is_horizontal_scrollbar_disabled: scrollbars.is_x_disabled(),
                is_vertical_scrollbar_disabled: scrollbars.is_y_disabled(),
            }
        } else {
            Status::Active {
                is_horizontal_scrollbar_disabled: scrollbars.is_x_disabled(),
                is_vertical_scrollbar_disabled: scrollbars.is_y_disabled(),
            }
        };

        if let Event::Window(window::Event::RedrawRequested(_now)) = event {
            state.last_status = Some(status);
            state.status_pending = false;
        } else if state
            .last_status
            .is_some_and(|last_status| last_status != status)
        {
            // Hovering or leaving the rail changes only what the next frame
            // paints; the interest `diff` cached predates it (enzICED).
            state.status_pending = true;
        }

        if last_offsets != (state.offset_x, state.offset_y)
            || state
                .last_status
                .is_some_and(|last_status| last_status != status)
        {
            shell.request_redraw();
        }

        // enzUI-ICED #2231: one-shot re-entry route. The translation the
        // content would be dispatched with now is compared against the one it
        // was last actually given, and the translation the next frame draws
        // against the one pinned. A wheel scroll, a scrollbar drag and a
        // keyboard scroll all move the content after its dispatch in this very
        // pass, so they differ and the subtree is routed one more frame. That
        // frame's dispatch makes them equal again and the route is dropped. A
        // relayout alone (the pane around this scrollable resizing) leaves the
        // translation, so it does not walk the content every frame (enzICED).
        let translation = state.translation(self.direction, bounds, content);
        state.pending_content_route = state.dispatched_translation != Some(translation)
            || translation != state.last_translation;

        // enzICED: an offset moved outside this pass (an operation run by a
        // widget during its own update, a `scroll_to` task) is only presented
        // by the next `RedrawRequested`, which commits the translation. Ask
        // for that frame, or the move waits for an unrelated event.
        if translation != state.last_translation {
            shell.request_redraw();
        }
        if state.status_pending {
            tree.raise_frame_interest();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let direction = self.direction.with_clearance(state.corner_clearance);
        if let Some(policy) = &self.smooth_scroll_policy {
            state.themed_smooth_scroll.set(Some(policy(theme)));
        }
        let bounds = layout.bounds();

        let content_layout = layout.children(&tree.children).next().unwrap();
        let content = content_layout.size();
        let content_bounds = content_layout.bounds();

        // enzUI-ICED #2795: an owned-presentation capture may record this
        // scrollable while it is offscreen.
        let node = tree.scroll_presentation_node();
        let capture_viewport = node.and_then(|node| renderer.layout_capture_viewport(node));

        let Some(viewport) = viewport.intersection(&bounds).or_else(|| {
            capture_viewport.map(|_| Rectangle {
                x: bounds.x.max(viewport.x),
                y: bounds.y.max(viewport.y),
                width: 0.0,
                height: 0.0,
            })
        }) else {
            return;
        };

        let translation = state.last_translation;
        let scrollbars = Scrollbars::new(translation, direction, bounds, content);
        let cursor_over_scrollable = cursor.position_over(bounds);
        let mouse_over_scrollbar = scrollbars.is_mouse_over(cursor);

        let cursor = match cursor_over_scrollable {
            Some(cursor_position) if mouse_over_scrollbar.is_none() => {
                state.content_cursor(cursor_position + translation)
            }
            _ => cursor.obstruct() + translation,
        };

        let style = theme.style(
            &self.class,
            state.last_status.unwrap_or(Status::Active {
                is_horizontal_scrollbar_disabled: false,
                is_vertical_scrollbar_disabled: false,
            }),
        );

        // enzUI-ICED #2795: the background and the rails are painted by the
        // renderer from the shared scroll geometry, so owned presentation can
        // repaint them for any offset.
        let rail = |rail: Rail| renderer::presentation::scroll_paint::Rail {
            background: rail.background,
            border: rail.border,
            thumb: rail.scroller.background,
            thumb_border: rail.scroller.border,
        };
        let paint = renderer::presentation::scroll_paint::Paint {
            background: container::background_quad(&style.container, bounds),
            rails: [rail(style.horizontal_rail), rail(style.vertical_rail)],
            gap: style.gap,
        };
        let config = direction.config();
        let input = state.scroll_input(renderer.hint_factor().unwrap_or(1.0));
        let capture = node.and_then(|node| {
            renderer.begin_scroll_view(node, bounds, content_bounds, config, input, paint)
        });

        if let Some((quad, fill)) = paint.background(bounds) {
            renderer.fill_quad(quad, fill);
        } else if capture.is_some() {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(bounds.position(), Size::ZERO),
                    ..Default::default()
                },
                Color::TRANSPARENT,
            );
        }

        // Draw inner content
        if scrollbars.active() || capture.is_some() {
            let draw_content = |renderer: &mut Renderer| {
                renderer.with_translation(
                    -translation.hint(renderer.hint_factor().unwrap_or(1.0)),
                    |renderer| {
                        self.content.as_widget().draw(
                            &tree.children[0],
                            renderer,
                            theme,
                            defaults,
                            content_layout,
                            cursor,
                            &capture.unwrap_or(viewport + translation),
                        );
                    },
                );
            };

            if let Some(node) = node {
                renderer.with_layout_layer(
                    renderer::LayoutLayer::Scroll {
                        node,
                        chrome: false,
                    },
                    viewport,
                    None,
                    draw_content,
                );
            } else {
                renderer.with_layer(viewport, draw_content);
            }

            let draw_chrome = |renderer: &mut Renderer| {
                renderer.draw_scroll_chrome(node, bounds, content_bounds, config, input, paint);
            };

            if let Some(node) = node {
                let chrome_bounds = Rectangle {
                    width: (viewport.width + 2.0).min(bounds.width + 2.0),
                    height: (viewport.height + 2.0).min(bounds.height + 2.0),
                    ..viewport
                };

                renderer.with_layout_layer(
                    renderer::LayoutLayer::Scroll { node, chrome: true },
                    chrome_bounds,
                    None,
                    draw_chrome,
                );
            } else if scrollbars.is_any_floating() {
                // Upstream: only floating rails need a layer above the content.
                renderer.with_layer(viewport, draw_chrome);
            } else {
                draw_chrome(renderer);
            }
        } else {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                defaults,
                content_layout,
                cursor,
                &(viewport + translation),
            );
        }

        if capture.is_some() {
            renderer.end_scroll_view(node.unwrap());
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let direction = self.direction.with_clearance(state.corner_clearance);

        if state.interaction.scrollers_grabbed() {
            return mouse::Interaction::Idle;
        }

        let Some(viewport) = viewport.intersection(&bounds) else {
            return mouse::Interaction::None;
        };

        let cursor_over_scrollable = cursor.position_over(bounds);
        let content_layout = layout.children(&tree.children).next().unwrap();
        let content = content_layout.size();

        let translation = state.last_translation;
        let scrollbars = Scrollbars::new(translation, direction, bounds, content);
        let mouse_over_scrollbar = scrollbars.is_mouse_over(cursor);

        // The scrollbar is a control of its own: over it the scrollable answers for
        // itself, so an ancestor's cursor (a window's resize edge) does not show.
        if mouse_over_scrollbar.is_some() {
            return mouse::Interaction::Idle;
        }

        let cursor = match cursor_over_scrollable {
            Some(cursor_position) if mouse_over_scrollbar.is_none() => {
                state.content_cursor(cursor_position + translation)
            }
            _ => cursor.obstruct() + translation,
        };

        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            content_layout,
            cursor,
            &(viewport + translation),
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
        window: Size,
    ) -> Vec<overlay::Element<'b, Message, Theme, Renderer>> {
        let state = tree.state.downcast_ref::<State>();
        let direction = self.direction.with_clearance(state.corner_clearance);
        let bounds = layout.bounds();
        let content_layout = layout.children(&tree.children).next().unwrap();
        let content = content_layout.size();
        let viewport = viewport.intersection(&bounds).unwrap_or(*viewport);

        let offset = state.last_translation;

        let overlay = self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            content_layout,
            renderer,
            &(viewport + offset),
            translation - offset,
            window,
        );

        let icon = if let Interaction::AutoScrolling { origin, .. } = state.interaction {
            let scrollbars = Scrollbars::new(offset, direction, bounds, content);

            Some(overlay::Element::new(Box::new(AutoScrollIcon {
                origin,
                vertical: scrollbars.y.is_some(),
                horizontal: scrollbars.x.is_some(),
                class: &self.class,
            })))
        } else {
            None
        };

        overlay.into_iter().chain(icon).collect()
    }
}

struct AutoScrollIcon<'a, Class> {
    origin: Point,
    vertical: bool,
    horizontal: bool,
    class: &'a Class,
}

impl<Class> AutoScrollIcon<'_, Class> {
    const SIZE: f32 = 40.0;
    const DOT: f32 = Self::SIZE / 10.0;
    const PADDING: f32 = Self::SIZE / 10.0;
}

impl<Message, Theme, Renderer> core::Overlay<Message, Theme, Renderer>
    for AutoScrollIcon<'_, Theme::Class<'_>>
where
    Renderer: text::Renderer,
    Theme: Catalog,
{
    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        _cursor: mouse::Cursor,
    ) {
        let bounds = Rectangle::new(
            self.origin - Vector::new(Self::SIZE, Self::SIZE) / 2.0,
            Size::new(Self::SIZE, Self::SIZE),
        );

        let style = theme
            .style(
                self.class,
                Status::Active {
                    is_horizontal_scrollbar_disabled: false,
                    is_vertical_scrollbar_disabled: false,
                },
            )
            .auto_scroll;

        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: style.border,
                    shadow: style.shadow,
                    snap: false,
                },
                style.background,
            );

            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        bounds.center() - Vector::new(Self::DOT, Self::DOT) / 2.0,
                        Size::new(Self::DOT, Self::DOT),
                    ),
                    border: border::rounded(bounds.width),
                    snap: false,
                    ..renderer::Quad::default()
                },
                style.icon,
            );

            let arrow = core::Text {
                content: String::new(),
                bounds: bounds.size(),
                size: Pixels::from(12),
                line_height: text::LineHeight::from(1.0),
                font: Renderer::ICON_FONT,
                align_x: text::Alignment::Center,
                align_y: alignment::Vertical::Center,
                shaping: text::Shaping::Basic,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: None,
            };

            if self.vertical {
                renderer.fill_text(
                    core::Text {
                        content: Renderer::SCROLL_UP_ICON.to_string(),
                        align_y: alignment::Vertical::Top,
                        ..arrow
                    },
                    Point::new(bounds.center_x(), bounds.y + Self::PADDING),
                    style.icon,
                    bounds,
                );

                renderer.fill_text(
                    core::Text {
                        content: Renderer::SCROLL_DOWN_ICON.to_string(),
                        align_y: alignment::Vertical::Bottom,
                        ..arrow
                    },
                    Point::new(
                        bounds.center_x(),
                        bounds.y + bounds.height - Self::PADDING - 0.5,
                    ),
                    style.icon,
                    bounds,
                );
            }

            if self.horizontal {
                renderer.fill_text(
                    core::Text {
                        content: Renderer::SCROLL_LEFT_ICON.to_string(),
                        align_x: text::Alignment::Left,
                        ..arrow
                    },
                    Point::new(bounds.x + Self::PADDING + 1.0, bounds.center_y() + 1.0),
                    style.icon,
                    bounds,
                );

                renderer.fill_text(
                    core::Text {
                        content: Renderer::SCROLL_RIGHT_ICON.to_string(),
                        align_x: text::Alignment::Right,
                        ..arrow
                    },
                    Point::new(
                        bounds.x + bounds.width - Self::PADDING - 1.0,
                        bounds.center_y() + 1.0,
                    ),
                    style.icon,
                    bounds,
                );
            }
        });
    }

    fn index(&self) -> f32 {
        f32::MAX
    }
}

impl<'a, Message, Theme, Renderer> From<Scrollable<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a + Catalog,
    Renderer: 'a + text::Renderer,
{
    fn from(
        text_input: Scrollable<'a, Message, Theme, Renderer>,
    ) -> Element<'a, Message, Theme, Renderer> {
        Element::new(text_input)
    }
}

/// The new position of the pointer of a held rail, if it moved.
///
/// A moved pointer re-evaluates the autoscroll's pause and stop conditions,
/// so it wakes the animation with a redraw.
fn rail_moved(event: &Event, cursor: mouse::Cursor, last: Point) -> Option<Point> {
    if matches!(
        event,
        Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. })
    ) {
        cursor
            .observe()
            .position()
            .filter(|position| *position != last)
    } else {
        None
    }
}

/// The content update that [`State::interact`] requests to be delegated to the
/// child widget.
#[derive(Debug)]
struct Content {
    /// The [`mouse::Cursor`] to pass to the child widget
    cursor: mouse::Cursor,
}

/// The outcome of [`State::interact`]: the [`Shell`] effects to materialize and
/// whether the event must be delegated to the content.
#[derive(Debug, Default)]
struct Interact {
    /// The scroll notification to publish, if the scroll changed: the final
    /// state after the event, coalescing any intermediate changes
    scroll: Option<Scroll>,

    /// Whether the event must be captured
    capture: bool,

    /// Whether the layout must be invalidated
    invalidate_layout: bool,

    /// Whether a redraw must be requested
    request_redraw: bool,

    /// Whether the interaction stopped early: the event must not be delegated
    /// to the content, nor processed by [`State::update`]
    stop: bool,

    /// If `Some`, the event must be delegated to the content with the given
    /// cursor and viewport
    content: Option<Content>,
}

/// The outcome of [`State::update`]: the [`Shell`] effects to materialize
#[derive(Debug, Default)]
struct Update {
    /// The scroll notification to publish, if the scroll changed: the final
    /// state after the event, coalescing any intermediate changes
    scroll: Option<Scroll>,

    /// Whether the event must be captured
    capture: bool,

    /// Whether the layout must be invalidated
    invalidate_layout: bool,

    /// Whether a redraw must be requested
    request_redraw: bool,

    /// When a wheel step began or extended the hover hold, the instant it
    /// runs out, so the scrollable wakes to find the pointer again.
    hover_hold_until: Option<Instant>,
}

#[derive(Debug, Clone)]
struct State {
    offset_y: Offset,
    offset_x: Offset,
    smooth_scroll: bool,
    /// The last answer of [`Scrollable::smooth_scroll_policy`], latched at
    /// draw; `None` before the first draw (enzICED).
    themed_smooth_scroll: Cell<Option<bool>>,
    /// Mirrors [`Scrollable::notify_horizontal_viewport_changes`] (enzUI).
    notify_horizontal_viewport_changes: bool,
    /// How far each rail end moves in to clear the rounded corners around
    /// the scrollable: vertical leading and trailing, then horizontal
    /// (enzICED; see [`Direction::corner_clearance`]).
    corner_clearance: [f32; 4],
    target: Option<Target>,
    source: Option<Source>,
    segment: Segment,
    last_frame: Option<Instant>,
    last_translation: Vector,
    interaction: Interaction,
    keyboard_modifiers: keyboard::Modifiers,
    last_notified: Option<Viewport>,
    last_scrolled: Option<Instant>,
    is_scrollbar_visible: bool,
    last_status: Option<Status>,
    // A status change that only the next redraw can store, so the redraw
    // must reach this scrollable even when nothing else wants a frame.
    status_pending: bool,
    last_id: Option<widget::Id>,
    /// Whether this state has laid out at least once, and so has had its
    /// chance to consume [`Scrollable::initial_offset`]. `false` on every
    /// freshly constructed state, which is what lets the seed fire again after
    /// a tree reset rebuilds the widget, and only then (enzUI #1799).
    seeded: bool,
    /// The translation the content was last actually dispatched with
    /// (enzUI-ICED #2231). An offset move changes it, and a descendant gated
    /// on its viewport must see the new one; a relayout alone does not.
    dispatched_translation: Option<Vector>,
    /// Whether the content still owes one dispatch with the current viewport,
    /// recomputed at the end of every `update` and consulted by
    /// [`Widget::frame_interest`] (enzUI-ICED #2231).
    pending_content_route: bool,
    /// The viewport and content sizes of the last layout (enzUI-ICED #1510).
    laid_out: Option<(Size, Size)>,
    /// Whether a layout changed this scrollable's geometry since the last
    /// `RedrawRequested`, which answers it with a notification (enzICED).
    layout_notice: bool,
    /// Whether the content gets every event in full. `update_frame` clears it
    /// for a frame the content owes nothing, which then reaches only the
    /// content's own frame routes (enzICED).
    full_content_dispatch: bool,
    /// While wheel scrolling moves the content under a still pointer, hover
    /// is held off until this instant; the point is where the pointer was
    /// held, so a real move ends the hold early.
    hover_hold: Option<(Instant, Point)>,
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    start: Point,
    started: Instant,
    duration: f32,
    slope: f32,
}

#[derive(Debug, Clone, Copy)]
enum Interaction {
    None,
    ScrollerGrabbed(Axis, f32),
    TouchScrolling(Point),
    AutoScrolling {
        origin: Point,
        current: Point,
        last_frame: Option<Instant>,
    },
    RailHeld(RailHeld),
}

impl Interaction {
    /// The axis of the active scrollbar interaction — a scroller drag or a
    /// held rail — if any.
    fn axis(&self) -> Option<Axis> {
        match self {
            Interaction::ScrollerGrabbed(axis, _) => Some(*axis),
            Interaction::RailHeld(rail) => Some(rail.axis),
            _ => None,
        }
    }

    /// Whether the user is interacting with a scrollbar: dragging a
    /// scroller or holding a rail pressed.
    fn scrollers_grabbed(&self) -> bool {
        self.axis().is_some()
    }

    /// The axis and the fraction of the scroller grabbed, if a scroller is
    /// being dragged.
    fn scroller_grabbed(&self) -> Option<(Axis, f32)> {
        let Interaction::ScrollerGrabbed(axis, grabbed_at) = self else {
            return None;
        };

        Some((*axis, *grabbed_at))
    }
}

/// The state of a [`Scrollable`] whose rail is being held pressed.
///
/// A plain rail press scrolls one page (smoothly) and, after
/// [`State::RAIL_AUTOSCROLL_DELAY`], autoscrolls at a constant velocity
/// until the scroller reaches the pointer, mirroring Chromium's track
/// autoscroll (`cc::ScrollbarController`).
#[derive(Debug, Clone, Copy)]
struct RailHeld {
    /// The axis the rail was pressed on
    axis: Axis,

    /// The direction of the autoscroll, in offset units (`+1.0` or
    /// `-1.0`), fixed when the rail was pressed
    direction: f32,

    /// Which side of the scroller the rail was pressed on, in cursor
    /// units: `+1.0` below it, `-1.0` above it.
    ///
    /// This is independent of the anchor's mirroring, and selects the
    /// scroller edge the autoscroll stops at (its leading edge) and the
    /// track part that pauses it.
    cursor_direction: f32,

    /// The autoscroll velocity, in pixels per second
    velocity: f32,

    /// When the rail was pressed
    pressed_at: Instant,

    /// The latest pointer position
    pointer: Point,

    /// The last frame the autoscroll was stepped for, guarding against
    /// stepping twice for the same instant
    last_frame: Option<Instant>,
}

/// The outcome of stepping a held rail's autoscroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RailStep {
    /// The offset moved; the scroll should be notified and redrawn
    Moved,

    /// The autoscroll has not started yet (within the press delay); frames
    /// must keep coming
    Waiting,

    /// Nothing can move until the pointer moves again
    Idle,
}

/// The axis of the [`Scrollable`] content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Axis {
    /// The horizontal axis.
    X,

    /// The vertical axis.
    Y,
}

impl Axis {
    /// The length of the given size along this axis.
    fn length(self, size: impl Into<Size>) -> f32 {
        let size = size.into();
        match self {
            Axis::X => size.width,
            Axis::Y => size.height,
        }
    }

    /// The coordinate of the given `point` along this axis.
    fn coordinate(self, point: Point) -> f32 {
        match self {
            Axis::X => point.x,
            Axis::Y => point.y,
        }
    }

    /// A [`Vector`] with the given `value` on this axis and `0.0` on the
    /// other.
    fn vector(self, value: f32) -> Vector {
        match self {
            Axis::X => Vector::new(value, 0.0),
            Axis::Y => Vector::new(0.0, value),
        }
    }
}

// enzUI-ICED: cancelling a gesture (e.g. a presentation handoff) drops any
// grabbed scroller, held rail or autoscroll.
impl tree::Interactive for State {
    fn cancel_interaction(&mut self) {
        self.interaction = Interaction::None;
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            offset_y: Offset::Absolute(0.0),
            offset_x: Offset::Absolute(0.0),
            smooth_scroll: false,
            themed_smooth_scroll: Cell::new(None),
            notify_horizontal_viewport_changes: true,
            corner_clearance: [0.0; 4],
            target: None,
            source: None,
            segment: Segment {
                start: Point::ORIGIN,
                started: Instant::now(),
                duration: 0.0,
                slope: 0.0,
            },
            last_frame: None,
            last_translation: Vector::ZERO,
            interaction: Interaction::None,
            keyboard_modifiers: keyboard::Modifiers::default(),
            last_notified: None,
            last_scrolled: None,
            is_scrollbar_visible: true,
            last_status: None,
            status_pending: false,
            last_id: None,
            seeded: false,
            dispatched_translation: None,
            pending_content_route: false,
            laid_out: None,
            layout_notice: false,
            full_content_dispatch: true,
            hover_hold: None,
        }
    }
}

impl operation::Scrollable for State {
    fn snap_to(
        &mut self,
        offset: RelativeOffset<Option<f32>>,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
    ) {
        State::snap_to(self, offset, animation, bounds, content, Source::Operation);
    }

    fn scroll_to(
        &mut self,
        offset: AbsoluteOffset<Option<f32>>,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
    ) {
        State::scroll_to(self, offset, animation, bounds, content, Source::Operation);
    }

    fn scroll_by(
        &mut self,
        offset: AbsoluteOffset,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
    ) {
        State::scroll_by(self, offset, animation, bounds, content);
    }
}

/// An offset of a [`Scrollable`], in either absolute or relative units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Offset {
    /// An absolute offset, in pixels.
    Absolute(f32),

    /// A relative offset, as a fraction of the scrollable range.
    Relative(f32),
}

impl Offset {
    /// Returns whether this offset is snapped, i.e. relative.
    pub fn is_snapped(self) -> bool {
        matches!(self, Offset::Relative(_))
    }

    fn absolute(self, viewport: f32, content: f32) -> f32 {
        match self {
            Offset::Absolute(absolute) => absolute.min((content - viewport).max(0.0)),
            Offset::Relative(percentage) => ((content - viewport) * percentage).max(0.0),
        }
    }

    fn translation(self, viewport: f32, content: f32, alignment: Anchor) -> f32 {
        let offset = self.absolute(viewport, content);

        match alignment {
            Anchor::Start => offset,
            Anchor::End => ((content - viewport).max(0.0) - offset).max(0.0),
        }
    }
}

/// The target of an in-progress smooth scroll.
///
/// The offsets are what the scroll settles at, so relative (snapped)
/// offsets stay snapped.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// The X offset of the target, in [`Offset`] units.
    pub x: Offset,

    /// The Y offset of the target, in [`Offset`] units.
    pub y: Offset,

    /// The resolution of the offsets at the current bounds, the point the
    /// running segment eases towards.
    pub destination: Vector,
}

impl Target {
    /// Resolves the given offsets into a [`Target`].
    fn new(x: Offset, y: Offset, bounds: Rectangle, content: Size) -> Self {
        let destination = Vector::new(
            x.absolute(bounds.width, content.width),
            y.absolute(bounds.height, content.height),
        );

        Self { x, y, destination }
    }

    /// A [`Target`] that settles at the given absolute `destination`.
    fn absolute(destination: Vector) -> Self {
        Self {
            x: Offset::Absolute(destination.x),
            y: Offset::Absolute(destination.y),
            destination,
        }
    }
}

/// The source of a scroll notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The user scrolled with a mouse wheel or touchpad.
    Wheel,

    /// The user scrolled by swiping with a touch.
    Touch,

    /// The user scrolled by dragging the scrollbar's scroller.
    Scrollbar,

    /// The user scrolled by auto-scrolling with the middle mouse button.
    AutoScroll,

    /// A scroll operation (`scroll_to`, `snap_to` or `scroll_by`) scrolled.
    Operation,

    /// A scroll [`Action`] scrolled.
    Action,

    /// The size of the content changed.
    Content,

    /// The size of the [`Scrollable`] changed.
    Resize,
}

/// An event describing a scroll of a [`Scrollable`].
#[derive(Debug, Clone, Copy)]
pub struct Scroll {
    /// The [`Viewport`] of the [`Scrollable`].
    pub viewport: Viewport,

    /// The original [`Viewport`] before the scroll event, if any.
    pub origin: Option<Viewport>,

    /// The [`Source`] of the scroll.
    pub source: Source,

    /// The [`Target`] of the scroll.
    pub target: Option<Target>,
}

impl Scroll {
    /// Returns the [`Viewport`] that this scroll settles at.
    ///
    /// While a smooth scrolling animation is in progress, the returned
    /// [`Viewport`] carries the [`Target`]'s offsets — the offsets the
    /// scroll settles at — instead of the current ones.
    pub fn destination(&self) -> Viewport {
        Viewport {
            x: self
                .target
                .map(|target| target.x)
                .unwrap_or(self.viewport.x),
            y: self
                .target
                .map(|target| target.y)
                .unwrap_or(self.viewport.y),
            ..self.viewport
        }
    }
}

/// The current [`Viewport`] of the [`Scrollable`].
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    /// The current X offset, in [`Offset`] units.
    pub x: Offset,

    /// The current Y offset, in [`Offset`] units.
    pub y: Offset,

    /// The bounds of the [`Scrollable`].
    pub bounds: Rectangle,

    /// The size of the content of the [`Scrollable`].
    pub content: Size,
}

impl Viewport {
    /// Returns the end of the [`Viewport`].
    pub fn end(&self) -> AbsoluteOffset {
        self.absolute_offset() + self.distance_to_end()
    }

    /// Returns a new [`Viewport`] aligned with the coordinates of the
    /// given one.
    pub fn slide(self, other: Self) -> Self {
        Self {
            x: other.x,
            y: other.y,
            ..self
        }
    }

    /// Returns the distance from the current scroll position to the end of
    /// the content, in pixels, per axis.
    ///
    /// This is the amount of content that can still be scrolled into view:
    /// it is zero when the content fits, or when the scroll is at the end.
    pub fn distance_to_end(&self) -> Vector {
        let AbsoluteOffset { x, y } = self.absolute_offset();

        Vector::new(
            (self.content.width - self.bounds.width - x).max(0.0),
            (self.content.height - self.bounds.height - y).max(0.0),
        )
    }

    /// Returns the [`AbsoluteOffset`] of the current [`Viewport`].
    pub fn absolute_offset(&self) -> AbsoluteOffset {
        let x = self.x.absolute(self.bounds.width, self.content.width);
        let y = self.y.absolute(self.bounds.height, self.content.height);

        AbsoluteOffset { x, y }
    }

    /// Returns the [`AbsoluteOffset`] of the current [`Viewport`], but with its
    /// alignment reversed.
    ///
    /// This method can be useful to switch the alignment of a [`Scrollable`]
    /// while maintaining its scrolling position.
    pub fn absolute_offset_reversed(&self) -> AbsoluteOffset {
        let AbsoluteOffset { x, y } = self.absolute_offset();

        AbsoluteOffset {
            x: (self.content.width - self.bounds.width).max(0.0) - x,
            y: (self.content.height - self.bounds.height).max(0.0) - y,
        }
    }

    /// Returns the [`RelativeOffset`] of the current [`Viewport`].
    pub fn relative_offset(&self) -> RelativeOffset {
        let AbsoluteOffset { x, y } = self.absolute_offset();

        let x = x / (self.content.width - self.bounds.width);
        let y = y / (self.content.height - self.bounds.height);

        RelativeOffset { x, y }
    }

    /// Returns the bounds of the current [`Viewport`].
    ///
    /// Fork compat: upstream made [`Viewport::bounds`] a field.
    pub fn bounds(&self) -> Rectangle {
        self.bounds
    }

    /// Returns the content bounds of the current [`Viewport`], placed at the
    /// origin of its bounds.
    ///
    /// Fork compat: upstream keeps only the content [`Size`].
    pub fn content_bounds(&self) -> Rectangle {
        Rectangle::new(self.bounds.position(), self.content)
    }
}

/// An action to perform in response to a [`Scroll`].
///
/// This is the return type of the [`Scrollable::on_scroll`] handler. It lets
/// the handler react to a scroll notification by driving the [`Scrollable`]
/// further, or by publishing a message.
#[derive(Debug)]
pub enum Action<Message> {
    /// Do nothing.
    None,

    /// Scroll to the given [`AbsoluteOffset`], with the given [`Animation`].
    ///
    /// An axis set to `None` keeps its current position.
    ScrollTo(AbsoluteOffset<Option<f32>>, Animation),

    /// Snap to the given [`RelativeOffset`], with the given [`Animation`].
    ///
    /// An axis set to `None` keeps its current position.
    SnapTo(RelativeOffset<Option<f32>>, Animation),

    /// Publish the given message.
    Custom(Message),
}

impl<Message> Action<Message> {
    fn perform(
        self,
        state: &mut State,
        bounds: Rectangle,
        content: Size,
        shell: &mut Shell<'_, Message>,
    ) {
        match self {
            Action::None => {}
            Action::ScrollTo(absolute_offset, animation) => {
                state.scroll_to(absolute_offset, animation, bounds, content, Source::Action);
            }
            Action::SnapTo(relative_offset, animation) => {
                state.snap_to(relative_offset, animation, bounds, content, Source::Action);
            }
            Action::Custom(message) => {
                shell.publish(message);
            }
        }
    }
}

impl<Message> From<Message> for Action<Message> {
    fn from(message: Message) -> Self {
        Self::Custom(message)
    }
}

impl<Message> From<Option<Message>> for Action<Message> {
    fn from(message: Option<Message>) -> Self {
        match message {
            Some(message) => Self::Custom(message),
            None => Self::None,
        }
    }
}

/// The distance (in logical pixels) scrolled per wheel line.
///
/// Chromium scrolls a fixed 120 CSS pixels per classic wheel notch,
/// independent of the page's line height: the OS delta is normalized to
/// 120 units (`ui::MouseWheelEvent::kWheelDelta`) and passed through 1:1.
///
/// This value assumes the platform reports one line per notch (e.g. X11).
/// On platforms that report three lines per notch, `40.0` (Chromium's
/// `cc::kPixelsPerLineStep`) is the equivalent value.
const WHEEL_PX_PER_LINE: f32 = 120.0;

/// How long hover stays suspended after the last wheel step that moved the
/// content.
///
/// Content scrolled under a still pointer would otherwise hover every item it
/// passes, and each hover fade repaints the window, so no scrolled frame
/// could be replayed. Browsers suspend hover while scrolling the same way.
/// The hold outlasts the gap between the steps of one wheel gesture, and a
/// real pointer move ends it at once.
const HOVER_HOLD: Duration = Duration::from_millis(250);

// The smooth scrolling behavior below (the easing curve, the animation
// durations, and the velocity-preserving retargeting) is derived from
// the Chromium project's wheel scroll animation
// (`cc/animation/scroll_offset_animation_curve.{h,cc}`), which is
// licensed under the BSD 3-Clause license:
// <https://chromium.googlesource.com/chromium/src/+/main/LICENSE>

/// The control points of the smooth scrolling easing curve.
///
/// This is the standard ease-in-out cubic bezier, as used by Chromium for
/// wheel scrolling: the scroll starts from rest, peaks mid-way, and
/// settles on the target at rest.
const SMOOTH_SCROLL_BEZIER_X1: f32 = 0.42;
const SMOOTH_SCROLL_BEZIER_X2: f32 = 0.58;

/// The divisor of the smooth scrolling animation duration, matching
/// Chromium's.
const SMOOTH_SCROLL_DURATION_DIVISOR: f32 = 60.0;

/// The delay (in seconds) between a wheel event and the next drawn frame.
///
/// Wheel events are processed between frames, and the smooth scrolling
/// animation is only stepped when the next frame is drawn. Starting the
/// animation one nominal frame *before* the event, where a nominal frame
/// is one unit of `SMOOTH_SCROLL_DURATION_DIVISOR`, ensures that the
/// first drawn frame already shows some progress, instead of repeating
/// the previous one.
///
/// A full frame is used rather than the mean delay of half a frame: it
/// guarantees that the first drawn frame visibly moves, even when the
/// event arrives right after a frame. The animation then settles one
/// frame early, which is imperceptible since the curve ends at rest.
const SMOOTH_SCROLL_FRAME_DELAY: f32 = 1.0 / SMOOTH_SCROLL_DURATION_DIVISOR;

/// The distances (in pixels) at which the smooth scrolling duration ramp
/// starts and ends.
const SMOOTH_SCROLL_DURATION_RAMP_START: f32 = WHEEL_PX_PER_LINE;
const SMOOTH_SCROLL_DURATION_RAMP_END: f32 = 480.0;

/// The shortest and longest smooth scrolling animation durations, in
/// `SMOOTH_SCROLL_DURATION_DIVISOR` units, matching Chromium's.
///
/// The duration is *inversely* proportional to the distance within these
/// bounds: short scrolls get a longer (softer) animation, while long
/// scrolls get a shorter (snappier) one.
const SMOOTH_SCROLL_DURATION_MIN: f32 = 6.0;
const SMOOTH_SCROLL_DURATION_MAX: f32 = 12.0;

/// The factor applied to the time it would take to cover the new distance
/// at the current velocity when retargeting a running animation, matching
/// Chromium's.
///
/// Bounding the new duration by this keeps a fast scroll from "rubber
/// banding" when a small new delta is added at high velocity.
const SMOOTH_SCROLL_RETARGET_VELOCITY_BOUND: f32 = 2.5;

/// The clamp for the initial slope of a retargeted animation, matching
/// Chromium's.
const SMOOTH_SCROLL_SLOPE_CLAMP: f32 = 1000.0;

// The held-rail behavior below (the press delay, the autoscroll
// velocity, and the page step) is derived from the Chromium
// project's compositor scrollbar controller (`cc/input/
// scrollbar_controller.{h,cc}` and `cc/input/scrollbar.h`), which is
// licensed under the BSD 3-Clause license:
// <https://chromium.googlesource.com/chromium/src/+/main/LICENSE>

/// The delay between a rail press and the start of the held-rail
/// autoscroll, matching Chromium's `cc::kInitialAutoscrollTimerDelay`.
///
/// During the delay, only the initial page step (animated like a wheel
/// scroll) is applied; a quick click therefore scrolls exactly one page.
const RAIL_AUTOSCROLL_DELAY: Duration = Duration::from_millis(250);

/// The factor converting a rail page step into the held-rail autoscroll
/// velocity, matching Chromium's `cc::kAutoscrollMultiplier`.
///
/// Chromium's main thread autoscroll applies the page step every 50 ms;
/// the equivalent constant velocity is the step times 20.
const RAIL_AUTOSCROLL_MULTIPLIER: f32 = 20.0;

/// The fraction of the viewport covered by a rail click's initial page
/// step, matching Chromium's `cc::kMinFractionToStepWhenPaging` (the
/// non-Mac page step).
const RAIL_PAGE_STEP_FRACTION: f32 = 0.875;

/// The distance (in pixels) within which the auto-scroll (middle mouse
/// button) movement is ignored.
const AUTOSCROLL_DEADZONE: f32 = 20.0;

/// The exponent of the auto-scroll velocity curve, which makes the
/// auto-scroll accelerate with the distance from the origin.
const AUTOSCROLL_SMOOTHNESS: f32 = 1.5;

impl State {
    /// The distance (in pixels) of a rail click's initial page step, given
    /// the length of the scrollable viewport.
    fn rail_page_step(viewport: f32) -> f32 {
        (viewport * RAIL_PAGE_STEP_FRACTION).max(1.0)
    }

    fn new() -> Self {
        State::default()
    }

    /// Where this scrollable is headed: the pending smooth-scroll target, or
    /// the current absolute offset (enzUI #562).
    fn destination(&self, bounds: Rectangle, content: Size) -> Point {
        match self.target {
            Some(target) => Point::new(target.destination.x, target.destination.y),
            None => Point::new(
                self.offset_x.absolute(bounds.width, content.width),
                self.offset_y.absolute(bounds.height, content.height),
            ),
        }
    }

    fn scroll(&mut self, delta: Vector<f32>, bounds: Rectangle, content: Size) {
        self.cancel();

        if bounds.height < content.height {
            self.offset_y = Offset::Absolute(
                (self.offset_y.absolute(bounds.height, content.height) + delta.y)
                    .clamp(0.0, content.height - bounds.height),
            );
        }

        if bounds.width < content.width {
            self.offset_x = Offset::Absolute(
                (self.offset_x.absolute(bounds.width, content.width) + delta.x)
                    .clamp(0.0, content.width - bounds.width),
            );
        }
    }

    /// Moves the *target* scroll offset by `delta`, for smooth scrolling.
    ///
    /// The delta is accumulated onto the pending target, if any, so that
    /// quick wheel movements do not lose their (not yet scrolled) distance;
    /// the target is then animated via [`State::scroll_smoothly_to`].
    fn scroll_smoothly(
        &mut self,
        delta: Vector<f32>,
        bounds: Rectangle,
        content: Size,
        now: Instant,
    ) {
        let current = Point::new(
            self.offset_x.absolute(bounds.width, content.width),
            self.offset_y.absolute(bounds.height, content.height),
        );

        // Accumulate onto the pending target, if any, so that quick wheel
        // movements do not lose their (not yet scrolled) distance
        let target = match self.target {
            Some(target) => Vector::new(
                Self::clamp_offset(target.destination.x + delta.x, bounds.width, content.width),
                Self::clamp_offset(
                    target.destination.y + delta.y,
                    bounds.height,
                    content.height,
                ),
            ),
            None => Vector::new(
                Self::clamp_offset(current.x + delta.x, bounds.width, content.width),
                Self::clamp_offset(current.y + delta.y, bounds.height, content.height),
            ),
        };

        self.scroll_smoothly_to(Target::absolute(target), bounds, content, now);
    }

    /// Scrolls smoothly to the given `target`.
    ///
    /// The target replaces any pending one. If a segment is already running,
    /// it is retargeted from the current position and velocity, so that
    /// scrolling flows instead of restarting.
    ///
    /// The target is then eased towards on each frame, via [`State::step`],
    /// with an ease-in-out animation whose duration depends on the distance:
    /// short scrolls get a longer (softer) animation, while long scrolls get a
    /// shorter (snappier) one.
    fn scroll_smoothly_to(
        &mut self,
        target: Target,
        bounds: Rectangle,
        content: Size,
        now: Instant,
    ) {
        let current = Point::new(
            self.offset_x.absolute(bounds.width, content.width),
            self.offset_y.absolute(bounds.height, content.height),
        );

        // Nothing to animate: the content fits, or we're already at the target
        if target.destination.x == current.x && target.destination.y == current.y {
            self.offset_x = target.x;
            self.offset_y = target.y;
            self.target = None;
            self.last_frame = None;
            return;
        }

        let Some(ongoing) = self.target else {
            // A fresh segment starts early so its first frame moves; retargets use the sampled clock.
            let now = now - Duration::from_secs_f32(SMOOTH_SCROLL_FRAME_DELAY);
            let distance = (target.destination.x - current.x)
                .abs()
                .max((target.destination.y - current.y).abs());

            self.target = Some(target);
            self.segment = Segment {
                start: current,
                started: now,
                duration: Self::smooth_scroll_duration(distance),
                slope: 0.0,
            };
            self.last_frame = Some(now);
            return;
        };

        // The target is unchanged: keep the running segment as is
        if ongoing.destination == target.destination {
            return;
        }

        self.retarget(
            target,
            self.last_frame.map_or(now, |sampled| now.max(sampled)),
        );
    }

    /// Retargets the running segment towards `target`, from the current
    /// position and velocity, so that scrolling flows instead of restarting.
    fn retarget(&mut self, target: Target, now: Instant) {
        // Retarget the running segment from the current position, preserving
        // the current velocity
        let start = self.animated_position(now);
        let velocity = self.animated_velocity(now);
        let new = Vector::new(
            target.destination.x - start.x,
            target.destination.y - start.y,
        );

        // The signed dimension with the largest magnitude, like Chromium's
        let max_dimension = if new.x.abs() > new.y.abs() {
            new.x
        } else {
            new.y
        };

        // The new duration, bounded so that a small delta added at high
        // velocity does not "rubber band"; a bound with the wrong sign means
        // the new delta is against the current motion, and does not apply
        let mut duration = Self::smooth_scroll_duration(max_dimension.abs());
        if velocity.abs() > 0.01 {
            let bound = SMOOTH_SCROLL_RETARGET_VELOCITY_BOUND * max_dimension / velocity;

            if bound > 0.0 {
                duration = duration.min(bound);
            }
        }

        if max_dimension.abs() < 0.01 || duration < 0.01 {
            // The new target is right on top of us: end the animation now
            self.offset_x = target.x;
            self.offset_y = target.y;
            self.target = None;
            self.last_frame = None;
            return;
        }

        // Adjust the initial slope of the new segment so that it starts with
        // the current velocity
        let slope = (velocity * (duration / max_dimension))
            .clamp(-SMOOTH_SCROLL_SLOPE_CLAMP, SMOOTH_SCROLL_SLOPE_CLAMP);

        self.target = Some(target);
        self.segment = Segment {
            start,
            started: now,
            duration,
            slope,
        };
        self.last_frame = Some(now);
    }

    /// The cursor the content sees at `position`.
    ///
    /// While a smooth scroll moves the content under a still pointer, or a
    /// wheel gesture holds hover off ([`HOVER_HOLD`]), the cursor levitates:
    /// hover effects stay put until the motion settles, and wheel events
    /// still reach nested scrollables through [`mouse::Cursor::land`].
    fn content_cursor(&self, position: Point) -> mouse::Cursor {
        let cursor = mouse::Cursor::Available(position);

        if self.target.is_some() || self.hover_hold.is_some() {
            cursor.levitate()
        } else {
            cursor
        }
    }

    /// Ends the hover hold when this frame reaches its end, or when the
    /// pointer really moved or left. Returns whether it ended: the content
    /// then needs this event in full, with the pointer landed again.
    fn release_hover_hold(&mut self, event: &Event, cursor: mouse::Cursor) -> bool {
        let Some((until, held_at)) = self.hover_hold else {
            return false;
        };

        let release = match event {
            Event::Window(window::Event::RedrawRequested(now)) => *now >= until,
            Event::Mouse(mouse::Event::CursorMoved { .. }) => cursor
                .observe()
                .position()
                .is_none_or(|position| position.distance(held_at) > 0.5),
            Event::Mouse(mouse::Event::CursorLeft) => true,
            _ => false,
        };

        if release {
            self.hover_hold = None;
        }

        release
    }

    /// The cursor the content sees for `event` at `position`.
    ///
    /// Presses and touches land a levitating [`content_cursor`]: hover stays
    /// suspended during a smooth scroll, but a click made before the motion
    /// settles still reaches the control under the pointer.
    ///
    /// [`content_cursor`]: Self::content_cursor
    fn content_event_cursor(&self, position: Point, event: &Event) -> mouse::Cursor {
        let cursor = self.content_cursor(position);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::ButtonReleased(_))
            | Event::Touch(_) => cursor.land(),
            _ => cursor,
        }
    }

    /// Steps the smooth scrolling animation forward, towards the target
    /// offset.
    ///
    /// Returns `true` if the animation is still in progress.
    fn step(&mut self, now: Instant, bounds: Rectangle, content: Size) -> bool {
        let Some(target) = self.target else {
            return false;
        };

        // The bounds and content may have changed while the animation is
        // running; re-resolve the target, and retarget if it moved
        let resolved = Target::new(target.x, target.y, bounds, content);
        if resolved.destination != target.destination {
            self.retarget(resolved, now);
        }

        let Some(target) = self.target else {
            return false;
        };
        let started = self.segment.started;

        let t = (now - started).as_secs_f32();
        let progress = (t / self.segment.duration).clamp(0.0, 1.0);

        if progress >= 1.0 {
            // Settled exactly on the target, keeping relative (snapped)
            // offsets
            self.offset_x = target.x;
            self.offset_y = target.y;
            self.target = None;
            self.last_frame = None;
            return false;
        }

        let bez = Self::smooth_scroll_progress(progress, self.segment.slope);
        self.offset_x = Offset::Absolute(
            self.segment.start.x + (target.destination.x - self.segment.start.x) * bez,
        );
        self.offset_y = Offset::Absolute(
            self.segment.start.y + (target.destination.y - self.segment.start.y) * bez,
        );

        self.last_frame = Some(now);
        true
    }

    /// Cancels any in-progress smooth scrolling animation, keeping the current
    /// offsets.
    ///
    /// This ensures that direct manipulation (scrollbar drags, touch scrolling,
    /// auto-scrolling, programmatic scrolling) always takes priority over a
    /// pending wheel animation.
    fn cancel(&mut self) {
        self.target = None;
        self.last_frame = None;
    }

    /// Lands any in-progress smooth scrolling animation on its target at once
    /// (enzICED), for when smooth scrolling is turned off mid-flight.
    fn settle(&mut self) {
        if let Some(target) = self.target.take() {
            self.offset_x = target.x;
            self.offset_y = target.y;
            self.last_frame = None;
        }
    }

    /// Clamps an absolute scroll offset to the range in which the given content
    /// can be scrolled inside the given viewport.
    fn clamp_offset(offset: f32, viewport: f32, content: f32) -> f32 {
        if content > viewport {
            offset.clamp(0.0, content - viewport)
        } else {
            0.0
        }
    }

    /// The animated position at `now`, or [`Point::ORIGIN`] if a segment is
    /// not running.
    fn animated_position(&self, now: Instant) -> Point {
        let Some(target) = self.target else {
            return Point::ORIGIN;
        };

        let started = self.segment.started;

        let t = (now - started).as_secs_f32();
        let progress = (t / self.segment.duration).clamp(0.0, 1.0);
        let bez = Self::smooth_scroll_progress(progress, self.segment.slope);

        Point::new(
            self.segment.start.x + (target.destination.x - self.segment.start.x) * bez,
            self.segment.start.y + (target.destination.y - self.segment.start.y) * bez,
        )
    }

    /// The animated velocity at `now`, in pixels per second along the
    /// segment's largest dimension, or `0.0` if a segment is not running.
    fn animated_velocity(&self, now: Instant) -> f32 {
        let Some(target) = self.target else {
            return 0.0;
        };

        let started = self.segment.started;

        let t = (now - started).as_secs_f32();
        let progress = (t / self.segment.duration).clamp(0.0, 1.0);

        if progress >= 1.0 {
            return 0.0;
        }

        let dx = target.destination.x - self.segment.start.x;
        let dy = target.destination.y - self.segment.start.y;
        let max_dimension = if dx.abs() > dy.abs() { dx } else { dy };

        Self::smooth_scroll_curve_slope(progress, self.segment.slope) * max_dimension
            / self.segment.duration
    }

    /// The duration (in seconds) of a smooth scrolling segment covering the
    /// given distance (in pixels).
    ///
    /// The duration is inversely proportional to the distance within a ramp:
    /// short scrolls get a longer (softer) animation, while long scrolls get a
    /// shorter (snappier) one.
    fn smooth_scroll_duration(distance: f32) -> f32 {
        let slope = (SMOOTH_SCROLL_DURATION_MIN - SMOOTH_SCROLL_DURATION_MAX)
            / (SMOOTH_SCROLL_DURATION_RAMP_END - SMOOTH_SCROLL_DURATION_RAMP_START);
        let offset = SMOOTH_SCROLL_DURATION_MAX - SMOOTH_SCROLL_DURATION_RAMP_START * slope;

        (offset + distance * slope).clamp(SMOOTH_SCROLL_DURATION_MIN, SMOOTH_SCROLL_DURATION_MAX)
            / SMOOTH_SCROLL_DURATION_DIVISOR
    }

    /// The progress of the smooth scrolling easing curve at the given time
    /// progress (in `[0, 1]`), with the given initial slope.
    ///
    /// The curve is an ease-in-out cubic bezier whose initial control point is
    /// scaled by `slope` (a slope of `0.0` is the plain ease-in-out curve).
    fn smooth_scroll_progress(time: f32, slope: f32) -> f32 {
        if time <= 0.0 {
            return 0.0;
        }

        if time >= 1.0 {
            return 1.0;
        }

        let s = Self::bezier_solve(time);
        let y1 = SMOOTH_SCROLL_BEZIER_X1 * slope;
        let os = 1.0 - s;

        // y(s), with y2 = 1
        3.0 * y1 * s * os * os + 3.0 * s * s * os + s * s * s
    }

    /// The slope of the smooth scrolling easing curve (dy/dx) at the given
    /// time progress (in `[0, 1]`), with the given initial slope.
    fn smooth_scroll_curve_slope(time: f32, slope: f32) -> f32 {
        let s = if time <= 0.0 {
            0.0
        } else if time >= 1.0 {
            1.0
        } else {
            Self::bezier_solve(time)
        };

        let x1 = SMOOTH_SCROLL_BEZIER_X1;
        let x2 = SMOOTH_SCROLL_BEZIER_X2;
        let y1 = x1 * slope;
        let os = 1.0 - s;

        // x'(s) and y'(s), with y2 = 1
        let x_prime =
            3.0 * x1 * os * (1.0 - 3.0 * s) + 3.0 * x2 * s * (2.0 - 3.0 * s) + 3.0 * s * s;
        let y_prime = 3.0 * y1 * os * (1.0 - 3.0 * s) + 3.0 * s * (2.0 - 3.0 * s) + 3.0 * s * s;

        y_prime / x_prime
    }

    /// Solves `x(s) = time` for `s` in `[0, 1]`, for the smooth scrolling
    /// bezier's x-axis.
    ///
    /// The x-axis is strictly increasing for the control points used here, so
    /// bisection converges unconditionally.
    fn bezier_solve(time: f32) -> f32 {
        let x1 = SMOOTH_SCROLL_BEZIER_X1;
        let x2 = SMOOTH_SCROLL_BEZIER_X2;

        let x = |s: f32| {
            let os = 1.0 - s;
            3.0 * x1 * s * os * os + 3.0 * x2 * s * s * os + s * s * s
        };

        let mut low = 0.0;
        let mut high = 1.0;

        for _ in 0..40 {
            let mid = (low + high) / 2.0;

            if x(mid) < time {
                low = mid;
            } else {
                high = mid;
            }
        }

        (low + high) / 2.0
    }

    /// Builds a [`Scroll`] notification for the current viewport, if it changed
    /// since the last one, and records that the user scrolled.
    fn notify_scroll(&mut self, bounds: Rectangle, content: Size) -> Option<Scroll> {
        let notification = self.notify_viewport(bounds, content);

        if notification.is_some() {
            self.last_scrolled = Some(Instant::now());
        }

        notification
    }

    /// Like [`State::notify_viewport`], but a change of horizontal geometry or
    /// offset alone is not reported (enzUI): a vertical-only observer still
    /// gets its viewport height and content extent changes, while a
    /// responsive width-only relayout does not become an application message
    /// and a second UI generation.
    fn notify_vertical_viewport(&mut self, bounds: Rectangle, content: Size) -> Option<Scroll> {
        if let Some(last_notified) = self.last_notified {
            let unchanged =
                |a: f32, b: f32| (a - b).abs() <= f32::EPSILON || (a.is_nan() && b.is_nan());

            let current = Viewport {
                x: self.offset_x,
                y: self.offset_y,
                bounds,
                content,
            };

            if unchanged(
                last_notified.absolute_offset().y,
                current.absolute_offset().y,
            ) && unchanged(last_notified.bounds.height, bounds.height)
                && unchanged(last_notified.content.height, content.height)
            {
                self.last_notified = Some(current);
                return None;
            }
        }

        self.notify_viewport(bounds, content)
    }

    /// Builds a [`Scroll`] notification for the current viewport, if it changed
    /// since the last one, recording it so that it is not reported again.
    fn notify_viewport(&mut self, bounds: Rectangle, content: Size) -> Option<Scroll> {
        if content.width <= bounds.width && content.height <= bounds.height {
            return None;
        }

        let viewport = Viewport {
            x: self.offset_x,
            y: self.offset_y,
            bounds,
            content,
        };

        // Don't publish redundant viewports to shell
        if let Some(last_notified) = self.last_notified {
            let last_absolute_offset = last_notified.absolute_offset();
            let current_absolute_offset = viewport.absolute_offset();

            let unchanged =
                |a: f32, b: f32| (a - b).abs() <= f32::EPSILON || (a.is_nan() && b.is_nan());

            // enzUI: a viewport that only moved (a sliding panel moves it every
            // frame) has not scrolled.
            if last_notified.bounds.size() == bounds.size()
                && last_notified.content == content
                && unchanged(last_absolute_offset.x, current_absolute_offset.x)
                && unchanged(last_absolute_offset.y, current_absolute_offset.y)
            {
                self.last_notified = Some(viewport);
                return None;
            }
        }

        // The notification's source: the scroll in progress or pending, or,
        // when the scroll position changed on its own, what changed in the
        // layout
        let source = if self.target.is_some() {
            // An in-flight animation: report the segment's source, keeping it
            // so that the settle notification reports it too
            self.source
        } else {
            // A settled or pending scroll: consume the source
            self.source.take()
        }
        .unwrap_or_else(|| {
            if let Some(last_notified) = self.last_notified {
                if last_notified.content != content {
                    Source::Content
                } else {
                    Source::Resize
                }
            } else {
                // The first notification: the content was laid out
                Source::Content
            }
        });

        let origin = self.last_notified;
        self.last_notified = Some(viewport);

        Some(Scroll {
            viewport,
            origin,
            source,
            target: self.target,
        })
    }

    /// Jumps the scroll to the given `percentage` along the given axis,
    /// snapping it to the nearest logical position.
    fn scroll_to_percentage(
        &mut self,
        axis: Axis,
        percentage: f32,
        bounds: Rectangle,
        content: Size,
    ) {
        self.cancel();

        match axis {
            Axis::X => self.offset_x = Offset::Relative(percentage.clamp(0.0, 1.0)),
            Axis::Y => self.offset_y = Offset::Relative(percentage.clamp(0.0, 1.0)),
        }

        self.unsnap(axis, bounds, content);
    }

    /// Presses the rail of the given axis at `cursor_position`, starting
    /// the held-rail autoscroll.
    ///
    /// This mirrors Chromium's track press (`cc::ScrollbarController::
    /// HandlePointerDown`): a page step is applied immediately, animated
    /// like a wheel scroll (or immediately when smooth scrolling is
    /// disabled), and, while the button is held, a constant-velocity
    /// autoscroll runs after a delay until the scroller reaches the
    /// pointer (stepped on each frame, via [`Self::step_rail`]).
    fn press_rail(
        &mut self,
        axis: Axis,
        scrollbar: &internals::Scrollbar,
        cursor_position: Point,
        smooth: bool,
        now: Instant,
        bounds: Rectangle,
        content: Size,
    ) {
        let Some(scroller) = scrollbar.scroller else {
            return;
        };

        // Which side of the scroller was pressed, in cursor space: valid
        // for both anchors, as the scroller's bounds already account for
        // the `Anchor::End` mirroring
        let cursor_direction =
            if axis.coordinate(cursor_position) < axis.coordinate(scroller.bounds.position()) {
                -1.0
            } else {
                1.0
            };

        // The offset at which the scroller's leading edge (toward the
        // pointer) reaches the pointer: a `grabbed_at` of `0.0` is the
        // scroller's start edge, `1.0` its end edge
        let grabbed_at = if cursor_direction < 0.0 { 0.0 } else { 1.0 };
        let Some(percentage) = scrollbar.scroll_percentage(axis, grabbed_at, cursor_position)
        else {
            return;
        };
        let stop = Offset::Relative(percentage).absolute(axis.length(bounds), axis.length(content));

        // The direction, in offset units: the offset-space sign of the stop
        // line, which accounts for the anchor's mirroring
        let direction = (stop - self.axis_offset(axis, bounds, content)).signum();
        if direction == 0.0 {
            // The pointer is on the stop line: there is nothing to scroll
            return;
        }

        let page_step = Self::rail_page_step(axis.length(bounds));

        // The initial page step: animated like a wheel scroll, or applied
        // immediately when smooth scrolling is disabled
        if smooth {
            self.scroll_smoothly(axis.vector(direction * page_step), bounds, content, now);
        } else {
            self.scroll(axis.vector(direction * page_step), bounds, content);
        }

        self.interaction = Interaction::RailHeld(RailHeld {
            axis,
            direction,
            cursor_direction,
            velocity: page_step * RAIL_AUTOSCROLL_MULTIPLIER,
            pressed_at: now,
            pointer: cursor_position,
            last_frame: None,
        });
    }

    /// Steps the held-rail autoscroll of the given axis on a frame.
    ///
    /// Returns `None` if the scrollbar disappeared while the rail was held,
    /// in which case the interaction must be dropped (like Chromium does
    /// when its scrollbar is unregistered); otherwise, the outcome of the
    /// step.
    fn step_rail(
        &mut self,
        rail: &mut RailHeld,
        now: Instant,
        scrollbar: Option<&internals::Scrollbar>,
        bounds: Rectangle,
        content: Size,
    ) -> Option<RailStep> {
        let axis = rail.axis;

        let scrollbar = scrollbar?;

        // The autoscroll starts after the press delay, while the initial
        // page step is still easing
        if now - rail.pressed_at < RAIL_AUTOSCROLL_DELAY {
            return Some(RailStep::Waiting);
        }

        // `last_frame` guards against stepping twice for the same instant
        if rail.last_frame == Some(now) {
            return Some(RailStep::Waiting);
        }

        // The time since the last step; the first step starts from the
        // nominal start of the autoscroll, not from the press
        let last_frame = rail.last_frame;
        rail.last_frame = Some(now);
        let time_delta = last_frame.map_or(
            (now - rail.pressed_at) - RAIL_AUTOSCROLL_DELAY,
            |last_frame| now - last_frame,
        );
        if time_delta.is_zero() {
            return Some(RailStep::Waiting);
        }

        let scroller = scrollbar.scroller?;

        let current = self.axis_offset(axis, bounds, content);

        // The offset at which the scroller's leading edge (the edge on the
        // pressed side, in cursor space) reaches the pointer
        let grabbed_at = if rail.cursor_direction > 0.0 {
            1.0
        } else {
            0.0
        };
        let stop = Offset::Relative(scrollbar.scroll_percentage(axis, grabbed_at, rail.pointer)?)
            .absolute(axis.length(bounds), axis.length(content));

        // The autoscroll pauses while the pointer is on the rail but no
        // longer on the pressed side of the (moving) scroller, and
        // continues while the pointer is off the rail, mirroring Chromium's
        // behavior when the pointer leaves the scrollbar layer
        let on_pressed_side = if rail.cursor_direction > 0.0 {
            axis.coordinate(rail.pointer)
                > axis.coordinate(scroller.bounds.position()) + axis.length(scroller.bounds)
        } else {
            axis.coordinate(rail.pointer) < axis.coordinate(scroller.bounds.position())
        };
        if scrollbar.total_bounds.contains(rail.pointer) && !on_pressed_side {
            return Some(RailStep::Idle);
        }

        // ... and it stops while the scroller's leading edge has reached
        // the pointer
        let remaining = if rail.direction > 0.0 {
            stop - current
        } else {
            current - stop
        };
        if remaining <= 0.0 {
            return Some(RailStep::Idle);
        }

        // A constant-velocity step, clamped so that the scroller does not
        // cross the pointer within the frame
        let delta = rail.direction * (rail.velocity * time_delta.as_secs_f32()).min(remaining);

        self.scroll(axis.vector(delta), bounds, content);

        Some(if self.axis_offset(axis, bounds, content) != current {
            RailStep::Moved
        } else {
            // The step was clamped away (e.g. at the end of the content):
            // nothing can move until the pointer moves again
            RailStep::Idle
        })
    }

    /// Snaps the scroll to the given [`RelativeOffset`], with the given
    /// [`Animation`].
    fn snap_to(
        &mut self,
        offset: RelativeOffset<Option<f32>>,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
        source: Source,
    ) {
        self.source = Some(source);

        if !self.should_scroll_smoothly(animation) {
            self.cancel();

            if let Some(x) = offset.x {
                self.offset_x = Offset::Relative(x.clamp(0.0, 1.0));
            }

            if let Some(y) = offset.y {
                self.offset_y = Offset::Relative(y.clamp(0.0, 1.0));
            }

            return;
        }

        // Snap the targeted axes relative to the content, and keep the
        // current offsets of the axes the snap does not target
        let x = offset
            .x
            .map(|x| Offset::Relative(x.clamp(0.0, 1.0)))
            .unwrap_or(self.offset_x);
        let y = offset
            .y
            .map(|y| Offset::Relative(y.clamp(0.0, 1.0)))
            .unwrap_or(self.offset_y);

        self.scroll_smoothly_to(
            Target::new(x, y, bounds, content),
            bounds,
            content,
            Instant::now(),
        );
    }

    /// Scrolls to the given [`AbsoluteOffset`], with the given [`Animation`].
    fn scroll_to(
        &mut self,
        offset: AbsoluteOffset<Option<f32>>,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
        source: Source,
    ) {
        self.source = Some(source);

        if !self.should_scroll_smoothly(animation) {
            self.cancel();

            if let Some(x) = offset.x {
                self.offset_x = Offset::Absolute(x.max(0.0));
            }

            if let Some(y) = offset.y {
                self.offset_y = Offset::Absolute(y.max(0.0));
            }

            return;
        }

        // Scroll the targeted axes to their clamped absolute offsets, and
        // keep the current offsets of the axes the scroll does not target
        let x = offset
            .x
            .map(|x| Offset::Absolute(Self::clamp_offset(x, bounds.width, content.width)))
            .unwrap_or(self.offset_x);
        let y = offset
            .y
            .map(|y| Offset::Absolute(Self::clamp_offset(y, bounds.height, content.height)))
            .unwrap_or(self.offset_y);

        self.scroll_smoothly_to(
            Target::new(x, y, bounds, content),
            bounds,
            content,
            Instant::now(),
        );
    }

    /// Scrolls by the provided [`AbsoluteOffset`], with the given
    /// [`Animation`].
    fn scroll_by(
        &mut self,
        offset: AbsoluteOffset,
        animation: Animation,
        bounds: Rectangle,
        content: Size,
    ) {
        self.source = Some(Source::Operation);

        let delta = Vector::new(offset.x, offset.y);

        if self.should_scroll_smoothly(animation) {
            self.scroll_smoothly(delta, bounds, content, Instant::now());
        } else {
            self.scroll(delta, bounds, content);
        }
    }

    /// Whether the given [`Animation`] requests a smooth scroll, given the
    /// widget's smooth scrolling setting.
    fn should_scroll_smoothly(&self, animation: Animation) -> bool {
        match animation {
            Animation::Auto => self.smooth_scroll,
            Animation::Instant => false,
            Animation::Smooth => true,
        }
    }

    /// Handles the interaction of the user with the [`Scrollable`], up to (but
    /// not including) the delegation of the event to the content.
    ///
    /// Returns the [`Interact`] effects to materialize on the [`Shell`] and
    /// whether the event must be delegated to the content.
    fn interact(
        &mut self,
        event: &Event,
        bounds: Rectangle,
        content: Size,
        cursor: mouse::Cursor,
        cursor_over_scrollable: Option<Point>,
        scrollbars: &Scrollbars,
        mouse_over_scrollbar: Option<Axis>,
        direction: Direction,
        smooth_scroll: bool,
        click_to_scroll: bool,
    ) -> Interact {
        let mut interact = Interact::default();

        // Clear the scroll transaction, if the user is no longer scrolling
        if let Some(last_scrolled) = self.last_scrolled {
            let clear_transaction = match event {
                Event::Mouse(
                    mouse::Event::ButtonPressed(_)
                    | mouse::Event::ButtonReleased(_)
                    | mouse::Event::CursorLeft,
                ) => true,
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    last_scrolled.elapsed() > Duration::from_millis(100)
                }
                _ => last_scrolled.elapsed() > Duration::from_millis(1500),
            };

            if clear_transaction {
                self.last_scrolled = None;
            }
        }

        let mut translation = self.last_translation;

        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            // Step the smooth scrolling animation, if any; `last_frame`
            // guards against stepping twice for the same instant
            if self.last_frame != Some(*now)
                && self.step(*now, bounds, content)
                && let Some(notification) = self.notify_scroll(bounds, content)
            {
                interact.scroll = Some(notification);
            }

            translation = self.translation(direction, bounds, content);
            self.last_translation = translation;

            // Step the held-rail autoscroll, if any; the geometry is rebuilt
            // from the current translation, so that the pause and stop
            // conditions are evaluated against the scroller as of this frame
            if let Interaction::RailHeld(mut rail) = self.interaction {
                let scrollbars = Scrollbars::new(translation, direction, bounds, content);

                let scrollbar = match rail.axis {
                    Axis::X => scrollbars.x.as_ref(),
                    Axis::Y => scrollbars.y.as_ref(),
                };

                let step = self
                    .step_rail(&mut rail, *now, scrollbar, bounds, content)
                    .map(|step| (Interaction::RailHeld(rail), step));

                match step {
                    Some((interaction, step)) => {
                        self.interaction = interaction;

                        match step {
                            RailStep::Moved => {
                                self.source = Some(Source::Scrollbar);

                                if let Some(notification) = self.notify_scroll(bounds, content) {
                                    interact.scroll = Some(notification);
                                }

                                interact.request_redraw = true;
                            }
                            RailStep::Waiting => interact.request_redraw = true,
                            RailStep::Idle => {}
                        }
                    }
                    // The scrollbar disappeared while the rail was held: drop
                    // the interaction, like Chromium does when its scrollbar
                    // is unregistered
                    None => self.interaction = Interaction::None,
                }
            }

            if self.target.is_some() {
                interact.request_redraw = true;
            } else if let Interaction::AutoScrolling {
                origin,
                current,
                last_frame,
            } = self.interaction
            {
                if last_frame == Some(*now) {
                    interact.request_redraw = true;
                } else {
                    self.interaction = Interaction::AutoScrolling {
                        origin,
                        current,
                        last_frame: None,
                    };

                    let mut delta = current - origin;

                    if delta.x.abs() < AUTOSCROLL_DEADZONE {
                        delta.x = 0.0;
                    }

                    if delta.y.abs() < AUTOSCROLL_DEADZONE {
                        delta.y = 0.0;
                    }

                    if delta.x != 0.0 || delta.y != 0.0 {
                        let time_delta =
                            last_frame.map_or(Duration::ZERO, |last_frame| *now - last_frame);

                        let scroll_factor = time_delta.as_secs_f32();

                        self.scroll(
                            direction.align(Vector::new(
                                delta.x.signum()
                                    * delta.x.abs().powf(AUTOSCROLL_SMOOTHNESS)
                                    * scroll_factor,
                                delta.y.signum()
                                    * delta.y.abs().powf(AUTOSCROLL_SMOOTHNESS)
                                    * scroll_factor,
                            )),
                            bounds,
                            content,
                        );

                        self.source = Some(Source::AutoScroll);

                        let notification = self.notify_scroll(bounds, content);
                        let has_scrolled = notification.is_some();

                        if let Some(notification) = notification {
                            interact.scroll = Some(notification);
                        }

                        if has_scrolled || time_delta.is_zero() {
                            self.interaction = Interaction::AutoScrolling {
                                origin,
                                current,
                                last_frame: Some(*now),
                            };

                            interact.request_redraw = true;
                        }
                    }
                }
            }

            if interact.scroll.is_none() {
                interact.scroll = if self.notify_horizontal_viewport_changes {
                    self.notify_viewport(bounds, content)
                } else {
                    self.notify_vertical_viewport(bounds, content)
                };
            }

            // A source that did not produce a notification must not be
            // attributed to a later one
            if self.target.is_none() {
                self.source = None;
            }
        }

        // A held rail: track the pointer for the autoscroll's pause and stop
        // conditions; a moved pointer re-evaluates them, waking the animation
        if let Interaction::RailHeld(mut rail) = self.interaction
            && let Some(position) = rail_moved(event, cursor, rail.pointer)
        {
            rail.pointer = position;
            self.interaction = Interaction::RailHeld(rail);
            interact.request_redraw = true;
        }

        // A scroller being dragged follows the pointer 1:1, until the button
        // is released
        if let Some((axis, scroller_grabbed_at)) = self.interaction.scroller_grabbed() {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. })
                | Event::Touch(touch::Event::FingerMoved { .. }) => {
                    if let Some(scrollbar) = scrollbars.scrollbar(axis) {
                        let Some(cursor_position) = cursor.observe().position() else {
                            interact.stop = true;
                            return interact;
                        };

                        if let Some(percentage) =
                            scrollbar.scroll_percentage(axis, scroller_grabbed_at, cursor_position)
                        {
                            self.scroll_to_percentage(axis, percentage, bounds, content);

                            self.source = Some(Source::Scrollbar);

                            if let Some(notification) = self.notify_scroll(bounds, content) {
                                interact.scroll = Some(notification);
                            }
                        }

                        interact.capture = true;
                    }
                }
                _ => {}
            }
        } else if let Some(axis) = mouse_over_scrollbar {
            // Otherwise, a press on a scrollbar under the cursor grabs its
            // scroller, jump-drags it (with `Shift`, or with a plain press
            // when `click_to_scroll` is enabled), or presses the rail
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerPressed { .. }) => {
                    let Some(cursor_position) = cursor.position() else {
                        interact.stop = true;
                        return interact;
                    };

                    if let Some((scrollbar, hit)) = scrollbars.hit(axis, cursor_position) {
                        match hit {
                            // The scroller: jump to the click position and
                            // drag it from there
                            Hit::Scroller { grabbed_at } => {
                                if let Some(percentage) =
                                    scrollbar.scroll_percentage(axis, grabbed_at, cursor_position)
                                {
                                    self.scroll_to_percentage(axis, percentage, bounds, content);
                                }

                                self.interaction = Interaction::ScrollerGrabbed(axis, grabbed_at);
                            }
                            // A "jump click" on the rail — a `Shift`-click by
                            // default, or a plain click when `click_to_scroll`
                            // is enabled: jump the scroller to the click
                            // position and drag it from there, like
                            // Chromium's `Shift`+click on the track
                            Hit::Rail if self.keyboard_modifiers.shift() != click_to_scroll => {
                                if let Some(percentage) =
                                    scrollbar.scroll_percentage(axis, 0.5, cursor_position)
                                {
                                    self.scroll_to_percentage(axis, percentage, bounds, content);
                                }

                                self.interaction = Interaction::ScrollerGrabbed(axis, 0.5);
                            }
                            // A paging rail press — a plain click by default,
                            // or a `Shift`-click when `click_to_scroll` is
                            // enabled: a page step (animated like a wheel
                            // scroll) and, while the button is held, a
                            // constant-velocity autoscroll until the scroller
                            // reaches the pointer, like Chromium's track
                            // autoscroll
                            Hit::Rail => {
                                self.press_rail(
                                    axis,
                                    scrollbar,
                                    cursor_position,
                                    smooth_scroll,
                                    Instant::now(),
                                    bounds,
                                    content,
                                );

                                interact.request_redraw = true;
                            }
                        }

                        self.source = Some(Source::Scrollbar);

                        if let Some(notification) = self.notify_scroll(bounds, content) {
                            interact.scroll = Some(notification);
                        }

                        interact.capture = true;
                    }
                }
                _ => {}
            }
        }

        if matches!(self.interaction, Interaction::AutoScrolling { .. })
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::WheelScrolled { .. })
                    | Event::Touch(_)
                    | Event::Keyboard(_)
            )
        {
            self.interaction = Interaction::None;
            interact.capture = true;
            interact.invalidate_layout = true;
            interact.request_redraw = true;
            interact.stop = true;
            return interact;
        }

        if self.last_scrolled.is_none()
            || !matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
        {
            let cursor = match cursor_over_scrollable {
                Some(cursor_position)
                    if mouse_over_scrollbar.is_none() && !self.interaction.scrollers_grabbed() =>
                {
                    self.content_event_cursor(cursor_position + translation, event)
                }
                _ => cursor.obstruct() + translation,
            };

            interact.content = Some(Content { cursor });
        }

        interact
    }

    /// Handles the part of the interaction that happens after the event is
    /// delegated to the content.
    ///
    /// Returns the [`Update`] effects to materialize on the [`Shell`].
    fn update(
        &mut self,
        event: &Event,
        bounds: Rectangle,
        content: Size,
        cursor: mouse::Cursor,
        cursor_over_scrollable: Option<Point>,
        mouse_over_scrollbar: Option<Axis>,
        direction: Direction,
        smooth_scroll: bool,
        auto_scroll: bool,
        is_event_captured: bool,
    ) -> Update {
        let mut update = Update::default();

        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. })
        ) {
            self.interaction = Interaction::None;
            return update;
        }

        if is_event_captured {
            return update;
        }

        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if !cursor.land().is_over(bounds) {
                    return update;
                }

                // Whole wheel notches tween; fractional line steps from a
                // high-resolution wheel arrive often enough to be smooth
                // already, so they scroll directly like pixel deltas.
                let (delta, smooth) = match *delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        let is_shift_pressed = self.keyboard_modifiers.shift();

                        // macOS automatically inverts the axes when Shift is
                        // pressed
                        let (x, y) = if cfg!(target_os = "macos") && is_shift_pressed {
                            (y, x)
                        } else {
                            (x, y)
                        };

                        let movement = if !is_shift_pressed {
                            Vector::new(x, y)
                        } else {
                            Vector::new(y, x)
                        };

                        (
                            -movement * WHEEL_PX_PER_LINE,
                            x.abs() >= 0.5 || y.abs() >= 0.5,
                        )
                    }
                    // Pixel deltas (e.g. from high-precision touchpads) are
                    // already smooth, so scrolling them immediately avoids
                    // double-smoothing them
                    mouse::ScrollDelta::Pixels { x, y } => (-Vector::new(x, y), false),
                };

                let delta = direction.align(delta);

                // enzUI #562: where the scroll is headed before and after this
                // delta, so the capture below means "this scrollable consumed
                // it". A nested child pinned at its edge then hands the wheel
                // to its ancestor.
                let before = self.destination(bounds, content);

                if smooth_scroll && smooth {
                    self.scroll_smoothly(delta, bounds, content, Instant::now());
                } else {
                    self.scroll(delta, bounds, content);
                }

                self.source = Some(Source::Wheel);

                if let Some(notification) = self.notify_scroll(bounds, content) {
                    update.scroll = Some(notification);
                }

                // enzUI #562: capture only when the delta moved this
                // scrollable. Capturing for the whole in-transaction window
                // (upstream: `last_scrolled` or a pending target) trapped wheel
                // events in a child that could not move any further, so the
                // ancestor never scrolled.
                if self.destination(bounds, content) != before {
                    update.capture = true;

                    if let Some(position) = cursor.observe().position() {
                        let until = Instant::now() + HOVER_HOLD;
                        self.hover_hold = Some((until, position));
                        update.hover_hold_until = Some(until);
                    }
                }

                if self.target.is_some() {
                    update.request_redraw = true;
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
                if auto_scroll && matches!(self.interaction, Interaction::None) =>
            {
                let Some(origin) = cursor_over_scrollable else {
                    return update;
                };

                self.interaction = Interaction::AutoScrolling {
                    origin,
                    current: origin,
                    last_frame: None,
                };

                update.capture = true;
                update.invalidate_layout = true;
                update.request_redraw = true;
            }
            Event::Touch(event)
                if matches!(self.interaction, Interaction::TouchScrolling(_))
                    || mouse_over_scrollbar.is_none() =>
            {
                match event {
                    touch::Event::FingerPressed { .. } => {
                        let Some(position) = cursor_over_scrollable else {
                            return update;
                        };

                        self.interaction = Interaction::TouchScrolling(position);
                        update.capture = true;
                    }
                    touch::Event::FingerMoved { .. } => {
                        let Some(cursor_position) = cursor.position() else {
                            return update;
                        };

                        let Interaction::TouchScrolling(scroll_box_touched_at) = self.interaction
                        else {
                            // An edge-bound child hands off the gesture; adoption starts at zero delta.
                            let Some(position) = cursor_over_scrollable else {
                                return update;
                            };

                            self.interaction = Interaction::TouchScrolling(position);
                            update.capture = true;
                            return update;
                        };

                        let delta = Vector::new(
                            scroll_box_touched_at.x - cursor_position.x,
                            scroll_box_touched_at.y - cursor_position.y,
                        );

                        let before = self.destination(bounds, content);

                        self.scroll(direction.align(delta), bounds, content);

                        self.interaction = Interaction::TouchScrolling(cursor_position);
                        self.source = Some(Source::Touch);

                        if let Some(notification) = self.notify_scroll(bounds, content) {
                            update.scroll = Some(notification);
                        }

                        // enzUI #562: capture only when the finger moved this
                        // scrollable, so an edge-bound child hands the rest of
                        // the gesture to its ancestor.
                        if self.destination(bounds, content) != before {
                            update.capture = true;
                        }
                    }
                    _ => {
                        update.capture = true;
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Interaction::AutoScrolling {
                    origin, last_frame, ..
                } = self.interaction
                {
                    let delta = *position - origin;

                    self.interaction = Interaction::AutoScrolling {
                        origin,
                        current: *position,
                        last_frame,
                    };

                    if (delta.x.abs() >= AUTOSCROLL_DEADZONE
                        || delta.y.abs() >= AUTOSCROLL_DEADZONE)
                        && last_frame.is_none()
                    {
                        update.request_redraw = true;
                    }
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                self.keyboard_modifiers = *modifiers;
            }
            _ => {}
        }

        update
    }

    /// Materializes the given axis's offset into an absolute one, so that it
    /// is no longer snapped to a logical position.
    fn unsnap(&mut self, axis: Axis, bounds: Rectangle, content: Size) {
        match axis {
            Axis::X => {
                self.offset_x =
                    Offset::Absolute(self.offset_x.absolute(bounds.width, content.width));
            }
            Axis::Y => {
                self.offset_y =
                    Offset::Absolute(self.offset_y.absolute(bounds.height, content.height));
            }
        }
    }

    /// The absolute offset of the given axis, in pixels.
    fn axis_offset(&self, axis: Axis, bounds: Rectangle, content: Size) -> f32 {
        match axis {
            Axis::X => self.offset_x.absolute(bounds.width, content.width),
            Axis::Y => self.offset_y.absolute(bounds.height, content.height),
        }
    }

    /// Returns the scrolling translation of the [`State`], given a [`Direction`],
    /// the bounds of the [`Scrollable`] and its contents.
    /// Draw-time scroll state for the shared scroll geometry and owned
    /// presentation (enzUI-ICED #2795).
    fn scroll_input(&self, scale: f32) -> layout::scroll::Input {
        let convert = |offset: Offset| match offset {
            Offset::Absolute(value) => layout::scroll::Offset::Absolute(value),
            Offset::Relative(value) => layout::scroll::Offset::Relative(value),
        };

        layout::scroll::Input {
            offsets: [convert(self.offset_x), convert(self.offset_y)],
            visible: self.is_scrollbar_visible,
            scale,
        }
    }

    fn translation(&self, direction: Direction, bounds: Rectangle, content: Size) -> Vector {
        Vector::new(
            if let Some(horizontal) = direction.horizontal() {
                self.offset_x
                    .translation(bounds.width, content.width, horizontal.alignment)
            } else {
                0.0
            },
            if let Some(vertical) = direction.vertical() {
                self.offset_y
                    .translation(bounds.height, content.height, vertical.alignment)
            } else {
                0.0
            },
        )
    }
}

/// The part of a [`Scrollbar`] hit by a cursor position.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    /// The scroller (thumb) of the [`Scrollbar`], grabbed at the given
    /// fraction of its length.
    Scroller {
        /// The fraction of the scroller's length, from its start, at which
        /// it was grabbed.
        grabbed_at: f32,
    },

    /// The rail (track) of the [`Scrollbar`], outside of the scroller.
    Rail,
}

#[derive(Debug)]
/// State of both [`Scrollbar`]s.
struct Scrollbars {
    y: Option<internals::Scrollbar>,
    x: Option<internals::Scrollbar>,
}

impl Scrollbars {
    /// Create y and/or x scrollbar(s) if content is overflowing the [`Scrollable`] bounds.
    fn new(translation: Vector, direction: Direction, bounds: Rectangle, content: Size) -> Self {
        let show_scrollbar_x = direction
            .horizontal()
            .filter(|_scrollbar| content.width > bounds.width);

        let show_scrollbar_y = direction
            .vertical()
            .filter(|_scrollbar| content.height > bounds.height);

        let y_scrollbar = if let Some(vertical) = show_scrollbar_y {
            let Scrollbar {
                width,
                margin,
                scroller_width,
                spacing,
                padding,
                scroller_min_length,
                ..
            } = *vertical;

            let x_scrollbar_height =
                show_scrollbar_x.map_or(0.0, |h| h.width.max(h.scroller_width) + h.margin);

            let total_scrollbar_width = width.max(scroller_width) + 2.0 * margin;

            let available = (bounds.height - x_scrollbar_height).max(0.0);
            let leading = padding[0].min(available);
            let trailing = padding[1].min(available - leading);
            let scrollbar_height = (available - leading - trailing).max(0.0);

            let total_scrollbar_bounds = Rectangle {
                x: bounds.x + bounds.width - total_scrollbar_width,
                y: bounds.y + leading,
                width: total_scrollbar_width,
                height: scrollbar_height,
            };

            let scrollbar_bounds = Rectangle {
                x: bounds.x + bounds.width - total_scrollbar_width / 2.0 - width / 2.0,
                y: bounds.y + leading,
                width,
                height: scrollbar_height,
            };

            let ratio = bounds.height / content.height;

            let scroller = if ratio >= 1.0 || scrollbar_height <= 0.0 {
                None
            } else {
                // enzUI #1833: keep extremely long content usable without
                // changing the endpoint contract: once the proportional length
                // is clamped, map the content offset onto the clamp-adjusted
                // travel.
                let scroller_height = (scrollbar_bounds.height * ratio)
                    .max(scroller_min_length)
                    .min(scrollbar_bounds.height);
                let max_scroll = (content.height - bounds.height).max(0.0);
                let travel = (scrollbar_bounds.height - scroller_height).max(0.0);
                let scroller_offset = if max_scroll > 0.0 {
                    (translation.y / max_scroll).clamp(0.0, 1.0) * travel
                } else {
                    0.0
                };

                let scroller_bounds = Rectangle {
                    x: bounds.x + bounds.width - total_scrollbar_width / 2.0 - scroller_width / 2.0,
                    y: (scrollbar_bounds.y + scroller_offset).max(0.0),
                    width: scroller_width,
                    height: scroller_height,
                };

                Some(internals::Scroller {
                    bounds: scroller_bounds,
                })
            };

            Some(internals::Scrollbar {
                total_bounds: total_scrollbar_bounds,
                bounds: scrollbar_bounds,
                scroller,
                alignment: vertical.alignment,
                disabled: content.height <= bounds.height,
                floating: spacing.is_none(),
            })
        } else {
            None
        };

        let x_scrollbar = if let Some(horizontal) = show_scrollbar_x {
            let Scrollbar {
                width,
                margin,
                scroller_width,
                spacing,
                padding,
                scroller_min_length,
                ..
            } = *horizontal;

            let scrollbar_y_width =
                y_scrollbar.map_or(0.0, |scrollbar| scrollbar.total_bounds.width);

            let total_scrollbar_height = width.max(scroller_width) + 2.0 * margin;

            let available = (bounds.width - scrollbar_y_width).max(0.0);
            let leading = padding[0].min(available);
            let trailing = padding[1].min(available - leading);
            let scrollbar_width = (available - leading - trailing).max(0.0);

            let total_scrollbar_bounds = Rectangle {
                x: bounds.x + leading,
                y: bounds.y + bounds.height - total_scrollbar_height,
                width: scrollbar_width,
                height: total_scrollbar_height,
            };

            let scrollbar_bounds = Rectangle {
                x: bounds.x + leading,
                y: bounds.y + bounds.height - total_scrollbar_height / 2.0 - width / 2.0,
                width: scrollbar_width,
                height: width,
            };

            let ratio = bounds.width / content.width;

            let scroller = if ratio >= 1.0 || scrollbar_width <= 0.0 {
                None
            } else {
                // enzUI #1833: horizontal parity with the vertical minimum and
                // endpoint mapping above.
                let scroller_length = (scrollbar_bounds.width * ratio)
                    .max(scroller_min_length)
                    .min(scrollbar_bounds.width);
                let max_scroll = (content.width - bounds.width).max(0.0);
                let travel = (scrollbar_bounds.width - scroller_length).max(0.0);
                let scroller_offset = if max_scroll > 0.0 {
                    (translation.x / max_scroll).clamp(0.0, 1.0) * travel
                } else {
                    0.0
                };

                let scroller_bounds = Rectangle {
                    x: (scrollbar_bounds.x + scroller_offset).max(0.0),
                    y: bounds.y + bounds.height
                        - total_scrollbar_height / 2.0
                        - scroller_width / 2.0,
                    width: scroller_length,
                    height: scroller_width,
                };

                Some(internals::Scroller {
                    bounds: scroller_bounds,
                })
            };

            Some(internals::Scrollbar {
                total_bounds: total_scrollbar_bounds,
                bounds: scrollbar_bounds,
                scroller,
                alignment: horizontal.alignment,
                disabled: content.width <= bounds.width,
                floating: spacing.is_none(),
            })
        } else {
            None
        };

        Self {
            y: y_scrollbar,
            x: x_scrollbar,
        }
    }

    /// The [`internals::Scrollbar`] of the given axis, if it is shown.
    fn scrollbar(&self, axis: Axis) -> Option<&internals::Scrollbar> {
        match axis {
            Axis::X => self.x.as_ref(),
            Axis::Y => self.y.as_ref(),
        }
    }

    /// The [`Axis`] of the scrollbar the given `cursor` is over, if any.
    ///
    /// The scrollbars' total bounds don't overlap, so the cursor is over at
    /// most one of them; the vertical one takes precedence.
    fn is_mouse_over(&self, cursor: mouse::Cursor) -> Option<Axis> {
        let cursor_position = cursor.position()?;

        if self
            .y
            .as_ref()
            .is_some_and(|scrollbar| scrollbar.is_mouse_over(cursor_position))
        {
            Some(Axis::Y)
        } else if self
            .x
            .as_ref()
            .is_some_and(|scrollbar| scrollbar.is_mouse_over(cursor_position))
        {
            Some(Axis::X)
        } else {
            None
        }
    }

    fn is_y_disabled(&self) -> bool {
        self.y.map(|y| y.disabled).unwrap_or(false)
    }

    fn is_x_disabled(&self) -> bool {
        self.x.map(|x| x.disabled).unwrap_or(false)
    }

    /// The [`internals::Scrollbar`] of the given axis and the part of it hit by
    /// the given cursor position, if any.
    fn hit(&self, axis: Axis, cursor_position: Point) -> Option<(&internals::Scrollbar, Hit)> {
        let scrollbar = match axis {
            Axis::X => self.x.as_ref(),
            Axis::Y => self.y.as_ref(),
        }?;
        let scroller = scrollbar.scroller?;

        if !scrollbar.total_bounds.contains(cursor_position) {
            return None;
        }

        // enzICED: the scroller is hit anywhere across the rail's band, not
        // only on its painted width. A thin thumb inside a wider proximity band
        // (enzUI #2746) is grabbed by a press level with it; only a press past
        // either end of the thumb, along the scroll axis, presses the rail.
        let along = axis.coordinate(cursor_position);
        let start = axis.coordinate(scroller.bounds.position());
        let on_scroller = (start..=start + axis.length(scroller.bounds)).contains(&along);

        Some((
            scrollbar,
            if on_scroller {
                Hit::Scroller {
                    grabbed_at: (axis.coordinate(cursor_position)
                        - axis.coordinate(scroller.bounds.position()))
                        / axis.length(scroller.bounds),
                }
            } else {
                Hit::Rail
            },
        ))
    }

    fn is_any_floating(&self) -> bool {
        self.y.is_some_and(|scrollbar| scrollbar.floating)
            || self.x.is_some_and(|scrollbar| scrollbar.floating)
    }

    fn active(&self) -> bool {
        self.y.is_some() || self.x.is_some()
    }
}

pub(super) mod internals {
    use crate::core::{Point, Rectangle};

    use super::Anchor;

    #[derive(Debug, Copy, Clone)]
    pub struct Scrollbar {
        pub total_bounds: Rectangle,
        pub bounds: Rectangle,
        pub scroller: Option<Scroller>,
        pub alignment: Anchor,
        pub disabled: bool,
        pub floating: bool,
    }

    impl Scrollbar {
        pub fn is_mouse_over(&self, cursor_position: Point) -> bool {
            self.total_bounds.width > 0.0
                && self.total_bounds.height > 0.0
                && self.total_bounds.contains(cursor_position)
        }

        /// Returns the scrolled percentage from the cursor position, along
        /// the given axis.
        ///
        /// A scroller that fills its track has no representable drag travel
        /// (enzUI #1833). Returning `None` keeps pressing or dragging it a
        /// no-op instead of dividing by zero and jumping the content to an
        /// endpoint.
        pub fn scroll_percentage(
            &self,
            axis: super::Axis,
            grabbed_at: f32,
            cursor_position: Point,
        ) -> Option<f32> {
            let scroller = self.scroller?;
            let travel = axis.length(self.bounds) - axis.length(scroller.bounds);

            if travel <= 0.0 {
                return None;
            }

            let percentage = (axis.coordinate(cursor_position)
                - axis.coordinate(self.bounds.position())
                - axis.length(scroller.bounds) * grabbed_at)
                / travel;

            Some(match self.alignment {
                Anchor::Start => percentage,
                Anchor::End => 1.0 - percentage,
            })
        }
    }

    /// The handle of a [`Scrollbar`].
    #[derive(Debug, Clone, Copy)]
    pub struct Scroller {
        /// The bounds of the [`Scroller`].
        pub bounds: Rectangle,
    }
}

/// The possible status of a [`Scrollable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The [`Scrollable`] can be interacted with.
    Active {
        /// Whether or not the horizontal scrollbar is disabled meaning the content isn't overflowing.
        is_horizontal_scrollbar_disabled: bool,
        /// Whether or not the vertical scrollbar is disabled meaning the content isn't overflowing.
        is_vertical_scrollbar_disabled: bool,
    },
    /// The [`Scrollable`] is being hovered.
    Hovered {
        /// Indicates if the horizontal scrollbar is being hovered.
        is_horizontal_scrollbar_hovered: bool,
        /// Indicates if the vertical scrollbar is being hovered.
        is_vertical_scrollbar_hovered: bool,
        /// Whether or not the horizontal scrollbar is disabled meaning the content isn't overflowing.
        is_horizontal_scrollbar_disabled: bool,
        /// Whether or not the vertical scrollbar is disabled meaning the content isn't overflowing.
        is_vertical_scrollbar_disabled: bool,
    },
    /// The [`Scrollable`] is being dragged.
    Dragged {
        /// Indicates if the horizontal scrollbar is being dragged.
        is_horizontal_scrollbar_dragged: bool,
        /// Indicates if the vertical scrollbar is being dragged.
        is_vertical_scrollbar_dragged: bool,
        /// Whether or not the horizontal scrollbar is disabled meaning the content isn't overflowing.
        is_horizontal_scrollbar_disabled: bool,
        /// Whether or not the vertical scrollbar is disabled meaning the content isn't overflowing.
        is_vertical_scrollbar_disabled: bool,
    },
}

/// The appearance of a scrollable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The [`container::Style`] of a scrollable.
    pub container: container::Style,
    /// The vertical [`Rail`] appearance.
    pub vertical_rail: Rail,
    /// The horizontal [`Rail`] appearance.
    pub horizontal_rail: Rail,
    /// The [`Background`] of the gap between a horizontal and vertical scrollbar.
    pub gap: Option<Background>,
    /// The appearance of the [`AutoScroll`] overlay.
    pub auto_scroll: AutoScroll,
}

/// The appearance of the scrollbar of a scrollable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rail {
    /// The [`Background`] of a scrollbar.
    pub background: Option<Background>,
    /// The [`Border`] of a scrollbar.
    pub border: Border,
    /// The appearance of the [`Scroller`] of a scrollbar.
    pub scroller: Scroller,
}

/// The appearance of the scroller of a scrollable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scroller {
    /// The [`Background`] of the scroller.
    pub background: Background,
    /// The [`Border`] of the scroller.
    pub border: Border,
}

/// The appearance of the autoscroll overlay of a scrollable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoScroll {
    /// The [`Background`] of the [`AutoScroll`] overlay.
    pub background: Background,
    /// The [`Border`] of the [`AutoScroll`] overlay.
    pub border: Border,
    /// Thje [`Shadow`] of the [`AutoScroll`] overlay.
    pub shadow: Shadow,
    /// The [`Color`] for the arrow icons of the [`AutoScroll`] overlay.
    pub icon: Color,
}

/// The theme catalog of a [`Scrollable`].
pub trait Catalog {
    /// The item class of the [`Catalog`].
    type Class<'a>;

    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`Scrollable`].
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

/// The default style of a [`Scrollable`].
pub fn default(theme: &Theme, status: Status) -> Style {
    let palette = theme.palette();

    let scrollbar = Rail {
        background: Some(palette.background.weak.color.into()),
        border: border::rounded(2),
        scroller: Scroller {
            background: palette.background.strongest.color.into(),
            border: border::rounded(2),
        },
    };

    let auto_scroll = AutoScroll {
        background: palette.background.base.color.scale_alpha(0.9).into(),
        border: border::rounded(u32::MAX)
            .width(1)
            .color(palette.background.base.text.scale_alpha(0.8)),
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(0.7),
            offset: Vector::ZERO,
            blur_radius: 2.0,
        },
        icon: palette.background.base.text.scale_alpha(0.8),
    };

    match status {
        Status::Active { .. } => Style {
            container: container::Style::default(),
            vertical_rail: scrollbar,
            horizontal_rail: scrollbar,
            gap: None,
            auto_scroll,
        },
        Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } => {
            let hovered_scrollbar = Rail {
                scroller: Scroller {
                    background: palette.primary.strong.color.into(),
                    ..scrollbar.scroller
                },
                ..scrollbar
            };

            Style {
                container: container::Style::default(),
                vertical_rail: if is_vertical_scrollbar_hovered {
                    hovered_scrollbar
                } else {
                    scrollbar
                },
                horizontal_rail: if is_horizontal_scrollbar_hovered {
                    hovered_scrollbar
                } else {
                    scrollbar
                },
                gap: None,
                auto_scroll,
            }
        }
        Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } => {
            let dragged_scrollbar = Rail {
                scroller: Scroller {
                    background: palette.primary.base.color.into(),
                    ..scrollbar.scroller
                },
                ..scrollbar
            };

            Style {
                container: container::Style::default(),
                vertical_rail: if is_vertical_scrollbar_dragged {
                    dragged_scrollbar
                } else {
                    scrollbar
                },
                horizontal_rail: if is_horizontal_scrollbar_dragged {
                    dragged_scrollbar
                } else {
                    scrollbar
                },
                gap: None,
                auto_scroll,
            }
        }
    }
}

#[cfg(test)]
mod scroll_timing_tests {
    use super::*;

    #[test]
    fn content_hover_is_suspended_until_smooth_scroll_settles() {
        let bounds = Rectangle::with_size(Size::new(800.0, 200.0));
        let content = Size::new(800.0, 3000.0);
        let pointer = Point::new(400.0, 100.0);
        let started = Instant::now();
        let mut state = State::new();
        assert_eq!(
            state.content_cursor(pointer),
            mouse::Cursor::Available(pointer)
        );

        state.scroll_smoothly(Vector::new(0.0, 120.0), bounds, content, started);
        assert!(state.step(started + Duration::from_millis(40), bounds, content));
        let moving = state.content_cursor(pointer);
        assert_eq!(moving.position(), None);
        assert_eq!(moving.land(), mouse::Cursor::Available(pointer));

        assert!(!state.step(started + Duration::from_secs(1), bounds, content));
        assert_eq!(
            state.content_cursor(pointer),
            mouse::Cursor::Available(pointer)
        );
    }

    #[test]
    fn presses_reach_content_during_smooth_scroll() {
        let bounds = Rectangle::with_size(Size::new(800.0, 200.0));
        let content = Size::new(800.0, 3000.0);
        let pointer = Point::new(400.0, 100.0);
        let started = Instant::now();
        let mut state = State::new();
        state.scroll_smoothly(Vector::new(0.0, 120.0), bounds, content, started);
        assert!(state.step(started + Duration::from_millis(40), bounds, content));

        let moved = Event::Mouse(mouse::Event::CursorMoved { position: pointer });
        assert_eq!(state.content_event_cursor(pointer, &moved).position(), None);

        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ] {
            assert_eq!(
                state.content_event_cursor(pointer, &event),
                mouse::Cursor::Available(pointer)
            );
        }
    }

    #[test]
    fn immediate_scrolling_keeps_content_hover() {
        let bounds = Rectangle::with_size(Size::new(800.0, 200.0));
        let content = Size::new(800.0, 3000.0);
        let pointer = Point::new(400.0, 100.0);
        let mut state = State::new();
        state.scroll(Vector::new(0.0, 40.0), bounds, content);
        assert_eq!(
            state.content_cursor(pointer),
            mouse::Cursor::Available(pointer)
        );
    }

    fn wheel(state: &mut State, dy: f32, pointer: Point) -> Update {
        let bounds = Rectangle::with_size(Size::new(800.0, 200.0));
        let content = Size::new(800.0, 3000.0);
        let cursor = mouse::Cursor::Available(pointer);
        state.update(
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: dy },
            }),
            bounds,
            content,
            cursor,
            Some(pointer),
            None,
            Direction::default(),
            false,
            false,
            false,
        )
    }

    fn redraw(at: Instant) -> Event {
        Event::Window(window::Event::RedrawRequested(at))
    }

    #[test]
    fn wheel_scrolling_holds_content_hover_until_the_gesture_rests() {
        let pointer = Point::new(400.0, 100.0);
        let cursor = mouse::Cursor::Available(pointer);
        let mut state = State::new();

        let until = wheel(&mut state, -40.0, pointer)
            .hover_hold_until
            .expect("a wheel step that moved the content holds hover");
        let held = state.content_cursor(pointer);
        assert_eq!(held.position(), None);
        assert_eq!(held.land(), cursor);

        let early = until - Duration::from_millis(1);
        assert!(!state.release_hover_hold(&redraw(early), cursor));
        assert_eq!(state.content_cursor(pointer).position(), None);

        assert!(state.release_hover_hold(&redraw(until), cursor));
        assert_eq!(state.content_cursor(pointer), cursor);
    }

    #[test]
    fn wheel_retarget_preserves_position_at_input_time() {
        let bounds = Rectangle::with_size(Size::new(800.0, 200.0));
        let content = Size::new(800.0, 3000.0);
        let started = Instant::now();
        let mut state = State::new();
        let mut control = State::new();
        for scroll in [&mut state, &mut control] {
            scroll.scroll_smoothly(Vector::new(0.0, 120.0), bounds, content, started);
            assert!(scroll.step(started + Duration::from_millis(40), bounds, content));
        }
        let input = started + Duration::from_millis(60);
        assert!(control.step(input, bounds, content));
        let expected = control.offset_y.absolute(bounds.height, content.height);
        state.scroll_smoothly(Vector::new(0.0, 60.0), bounds, content, input);
        assert!(state.step(input, bounds, content));
        let actual = state.offset_y.absolute(bounds.height, content.height);
        assert!(
            (actual - expected).abs() < 0.001,
            "retarget changed the input-time position: {expected} -> {actual}"
        );
        assert!(!state.step(started + Duration::from_secs(1), bounds, content));
        assert_eq!(
            state.offset_y.absolute(bounds.height, content.height),
            180.0
        );
    }
}
