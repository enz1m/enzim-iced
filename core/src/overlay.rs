// Modified by enzUI contributors. See FORK.md.
//! Display interactive elements on top of other widgets.
mod element;
mod group;

pub use element::Element;
pub use group::Group;

use crate::mouse;
use crate::renderer;
use crate::widget;
use crate::widget::Tree;
use crate::{Event, Layout, Rectangle, Shell, Size, Vector};

/// An interactive component that can be displayed on top of other widgets.
pub trait Overlay<Message, Theme, Renderer>
where
    Renderer: crate::Renderer,
{
    /// Draws the [`Overlay`] using the associated `Renderer`.
    ///
    /// Implementors are expected to call
    /// [`Renderer::with_layer`](renderer::Renderer::with_layer) themselves,
    /// so that their contents are properly clipped.
    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    );

    /// Applies a [`widget::Operation`] to the [`Overlay`].
    fn operate(&mut self, _renderer: &Renderer, _operation: &mut dyn widget::Operation) {}

    /// Processes a runtime [`Event`].
    ///
    /// By default, it does nothing.
    fn update(
        &mut self,
        _event: &Event,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _shell: &mut Shell<'_, Message>,
    ) {
    }

    /// Returns the current [`mouse::Interaction`] of the [`Overlay`].
    ///
    /// By default, it returns [`mouse::Interaction::None`].
    fn mouse_interaction(
        &self,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }

    /// Returns the nested overlays of the [`Overlay`].
    fn overlay<'a>(
        &'a mut self,
        _renderer: &Renderer,
    ) -> Vec<Element<'a, Message, Theme, Renderer>> {
        Vec::new()
    }

    /// The index of the overlay.
    ///
    /// Overlays with a higher index will be rendered on top of overlays with
    /// a lower index.
    ///
    /// By default, it returns `1.0`.
    fn index(&self) -> f32 {
        1.0
    }

    /// The [`layout::Node`](crate::layout::Node) this [`Overlay`] was laid
    /// out to, if it keeps one.
    ///
    /// enzICED: introspection for tests and diagnostics. An overlay that lays
    /// itself out once at creation can report that node; a wrapper forwards
    /// its content's. By default, it returns `None`.
    fn node(&self) -> Option<&crate::layout::Node> {
        None
    }

    /// The area this [`Overlay`] draws in, in window coordinates (enzICED).
    ///
    /// A runtime that shows overlays reaching past the window in popup
    /// surfaces sizes the popup to it. By default, it is the bounds of
    /// [`Self::node`]; an overlay laid out as a window-sized node that holds
    /// its surfaces reports the surfaces instead.
    fn area(&self) -> Option<Rectangle> {
        self.node().map(crate::layout::Node::bounds)
    }

    /// The rectangle this [`Overlay`] is placed against, in window
    /// coordinates: the button a menu drops from, the field a list belongs
    /// to, the point a context menu was asked for (enzICED).
    ///
    /// A runtime that shows overlays in popup surfaces of their own places
    /// them against it, so the compositor can flip a menu above its button
    /// when it does not fit below. By default, it returns `None`.
    fn anchor(&self) -> Option<Rectangle> {
        None
    }

    /// Whether this [`Overlay`], shown in a popup surface of its own, takes
    /// the input grab (enzICED): keyboard focus, and dismissal by a click in
    /// another application. A menu opened by a click grabs; a tooltip or
    /// other hover feedback must not, since a grab needs a recent press and
    /// a grabbing popup has to be the topmost one. By default, it returns
    /// `false`.
    fn grabs(&self) -> bool {
        false
    }
}

static POPUP_REACH: std::sync::RwLock<Option<Size>> = std::sync::RwLock::new(None);

/// How far, in logical pixels, an anchored overlay such as a menu, list or
/// tooltip may extend past each edge of its window (enzICED).
///
/// `Some` when the runtime presents overlays that leave the window in popup
/// surfaces of their own (`xdg_popup` on Wayland), so they are not clipped at
/// the window edge; `None` where overlays must stay inside the window.
#[must_use]
pub fn popup_reach() -> Option<Size> {
    *POPUP_REACH
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sets [`popup_reach`]; the runtime calls this.
pub fn set_popup_reach(reach: Option<Size>) {
    *POPUP_REACH
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = reach;
}

/// Returns the overlays of the given [`Element`] children.
///
/// This method will generally only be used by advanced users that are
/// implementing the [`Widget`](crate::Widget) trait.
pub fn from_children<'a, Message, Theme, Renderer>(
    children: &'a mut [crate::Element<'_, Message, Theme, Renderer>],
    tree: &'a mut Tree,
    layout: Layout,
    renderer: &Renderer,
    viewport: &Rectangle,
    translation: Vector,
    window: Size,
) -> Vec<Element<'a, Message, Theme, Renderer>>
where
    Renderer: crate::Renderer,
{
    children
        .iter_mut()
        .zip(layout.iter_mut(&mut tree.children))
        .flat_map(|(child, (layout, state))| {
            child
                .as_widget_mut()
                .overlay(state, layout, renderer, viewport, translation, window)
        })
        .collect()
}
