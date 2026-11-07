// Modified by enzUI contributors. See FORK.md.
//! Overlays that leave their window, presented in popup surfaces (enzICED).
//!
//! When the open overlays of a window (menus, lists, tooltips) reach past its
//! edges, the window's own surface would clip them. The runtime then asks the
//! event loop for an `xdg_popup` child covering the overlays, draws the
//! window without them and the overlays alone into the popup, and delivers
//! the popup's input to the window as if it had landed there. Overlays that
//! fit, and platforms without popups, keep drawing inside the window.

use crate::core::{Color, Rectangle, Size, Vector, window};
use crate::graphics::{Compositor, Viewport, compositor};

use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Room left around the overlays for their shadows, in logical pixels.
pub(crate) const SHADOW_MARGIN: f32 = 24.0;

/// The popup area a window's overlays need, or `None` when they fit inside
/// the window (or there are none).
pub(crate) fn wanted_area(overlays: Option<Rectangle>, window: Size) -> Option<Rectangle> {
    let overlays = overlays?;
    let inside = overlays.x >= -0.5
        && overlays.y >= -0.5
        && overlays.x + overlays.width <= window.width + 0.5
        && overlays.y + overlays.height <= window.height + 0.5;
    if inside || overlays.width <= 0.0 || overlays.height <= 0.0 {
        return None;
    }
    // Whole pixels, so a sub-pixel change of the overlays does not recreate
    // the popup.
    let area = overlays.expand(SHADOW_MARGIN);
    let x = area.x.floor();
    let y = area.y.floor();
    Some(Rectangle {
        x,
        y,
        width: (area.x + area.width).ceil() - x,
        height: (area.y + area.height).ceil() - y,
    })
}

/// A request to the event loop for a popup over `area` of `parent`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Request {
    pub parent: window::Id,
    pub parent_raw: winit::window::WindowId,
    /// In the parent's winit logical coordinates.
    pub area: Rectangle,
    /// What the overlays are placed against, in the same coordinates.
    pub anchor: Option<Rectangle>,
    /// Whether the popup takes the input grab.
    pub grab: bool,
}

/// Where a popup goes: the area it covers and what it is placed against, in
/// the parent's iced logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub area: Rectangle,
    pub anchor: Option<Rectangle>,
    /// Whether an overlay asks for the input grab ([`Overlay::grabs`]).
    ///
    /// [`Overlay::grabs`]: crate::core::Overlay::grabs
    pub grab: bool,
}

struct Popup<C: Compositor> {
    raw: Arc<winit::window::Window>,
    surface: C::Surface,
    renderer: C::Renderer,
    /// The overlay area shown, in the parent's iced logical coordinates.
    area: Rectangle,
    anchor: Option<Rectangle>,
    grab: bool,
}

enum Slot<C: Compositor> {
    /// Asked the event loop for a popup over this area.
    Pending(Placement),
    Ready(Popup<C>),
}

/// The overflow popups of every window.
pub(crate) struct Overflow<C: Compositor> {
    slots: FxHashMap<window::Id, Slot<C>>,
    by_raw: FxHashMap<winit::window::WindowId, window::Id>,
    /// Windows whose popup the compositor dismissed: their overlays are
    /// closing and get no new popup until they have.
    dismissed: rustc_hash::FxHashSet<window::Id>,
}

impl<C: Compositor> Default for Overflow<C> {
    fn default() -> Self {
        Self {
            slots: FxHashMap::default(),
            by_raw: FxHashMap::default(),
            dismissed: rustc_hash::FxHashSet::default(),
        }
    }
}

impl<C: Compositor> Overflow<C> {
    /// Reconciles `parent`'s popup with the area its overlays now need.
    /// Returns a creation request to send, and whether the overlays are
    /// detached from the parent (drawn by [`Self::draw`] instead).
    pub(crate) fn reconcile(
        &mut self,
        parent: window::Id,
        parent_raw: &winit::window::Window,
        wanted: Option<Placement>,
        to_winit: f32,
    ) -> (Option<Request>, bool) {
        let scale = |rect: Rectangle| Rectangle {
            x: rect.x * to_winit,
            y: rect.y * to_winit,
            width: rect.width * to_winit,
            height: rect.height * to_winit,
        };
        let request = |placement: Placement| Request {
            parent,
            parent_raw: parent_raw.id(),
            area: scale(placement.area),
            anchor: placement.anchor.map(scale),
            grab: placement.grab,
        };
        if self.dismissed.contains(&parent) {
            if wanted.is_none() {
                let _ = self.dismissed.remove(&parent);
            }
            return (None, false);
        }
        match (self.slots.get(&parent), wanted) {
            (None, None) => (None, false),
            (Some(_), None) => {
                self.remove(parent);
                (None, false)
            }
            (None, Some(area)) => {
                let _ = self.slots.insert(parent, Slot::Pending(area));
                (Some(request(area)), false)
            }
            (Some(Slot::Pending(_)), Some(area)) => {
                let _ = self.slots.insert(parent, Slot::Pending(area));
                (None, false)
            }
            (Some(Slot::Ready(popup)), Some(wanted))
                if popup.area == wanted.area
                    && popup.anchor == wanted.anchor
                    && popup.grab == wanted.grab =>
            {
                (None, true)
            }
            (Some(Slot::Ready(_)), Some(area)) => {
                // Moved, resized or grabbing now: a popup is placed once, so
                // replace it. The event loop destroys the old popup before it
                // creates the new one.
                self.remove(parent);
                let _ = self.slots.insert(parent, Slot::Pending(area));
                (Some(request(area)), false)
            }
        }
    }

    /// Adopts a popup the event loop created for `parent`, if `parent` still
    /// wants that area; otherwise the popup is dropped, which unmaps it.
    pub(crate) fn adopt(
        &mut self,
        parent: window::Id,
        area: Rectangle,
        to_winit: f32,
        raw: Arc<winit::window::Window>,
        compositor: &mut C,
        settings: crate::core::renderer::Settings,
    ) -> bool {
        let wanted = match self.slots.get(&parent) {
            Some(Slot::Pending(wanted)) => *wanted,
            _ => return false,
        };
        let asked = Rectangle {
            x: wanted.area.x * to_winit,
            y: wanted.area.y * to_winit,
            width: wanted.area.width * to_winit,
            height: wanted.area.height * to_winit,
        };
        if asked != area {
            return false;
        }
        let size = raw.inner_size();
        let surface = compositor.create_surface(raw.clone(), size.width, size.height);
        let renderer = compositor.create_renderer(settings);
        let _ = self.by_raw.insert(raw.id(), parent);
        let _ = self.slots.insert(
            parent,
            Slot::Ready(Popup {
                raw,
                surface,
                renderer,
                area: wanted.area,
                anchor: wanted.anchor,
                grab: wanted.grab,
            }),
        );
        true
    }

    /// The window a popup belongs to and the offset from popup to window
    /// coordinates, in iced logical pixels.
    pub(crate) fn route(&self, raw: winit::window::WindowId) -> Option<(window::Id, Vector)> {
        let parent = *self.by_raw.get(&raw)?;
        match self.slots.get(&parent)? {
            Slot::Ready(popup) => Some((parent, Vector::new(popup.area.x, popup.area.y))),
            Slot::Pending(_) => None,
        }
    }

    /// The compositor dismissed `parent`'s popup: forget it, and give the
    /// overlays no new one until they have closed.
    pub(crate) fn dismissed(&mut self, parent: window::Id) {
        self.remove(parent);
        let _ = self.dismissed.insert(parent);
    }

    /// Whether `parent` shows its overlays in a popup now.
    pub(crate) fn has_popup(&self, parent: window::Id) -> bool {
        matches!(self.slots.get(&parent), Some(Slot::Ready(_)))
    }

    /// Forgets the popups of windows `alive` no longer reports.
    pub(crate) fn retain(&mut self, mut alive: impl FnMut(window::Id) -> bool) {
        let gone: Vec<_> = self
            .slots
            .keys()
            .copied()
            .filter(|id| !alive(*id))
            .collect();
        for parent in gone {
            self.remove(parent);
            let _ = self.slots.remove(&parent);
        }
        self.dismissed.retain(|id| alive(*id));
    }

    /// Forgets `parent`'s popup; dropping it unmaps the surface.
    pub(crate) fn remove(&mut self, parent: window::Id) {
        if let Some(Slot::Ready(popup)) = self.slots.remove(&parent) {
            let _ = self.by_raw.remove(&popup.raw.id());
        }
    }

    /// Paints `parent`'s overlays into its popup and presents it.
    pub(crate) fn present(
        &mut self,
        parent: window::Id,
        compositor: &mut C,
        scale: crate::core::renderer::Scale,
        cursor_icon: Option<winit::window::Cursor>,
        draw: impl FnOnce(&mut C::Renderer, Rectangle),
    ) -> Result<(), compositor::SurfaceError> {
        let Some(Slot::Ready(popup)) = self.slots.get_mut(&parent) else {
            return Ok(());
        };
        draw(&mut popup.renderer, popup.area);
        let size = popup.raw.inner_size();
        let viewport =
            Viewport::with_physical_size(crate::core::Size::new(size.width, size.height), scale);
        if let Some(icon) = cursor_icon {
            popup.raw.set_cursor(icon);
        }
        let raw = popup.raw.clone();
        compositor.present(
            &mut popup.renderer,
            &mut popup.surface,
            &viewport,
            Color::TRANSPARENT,
            || raw.pre_present_notify(),
        )
    }

    /// Reconfigures a popup surface after the compositor resized it.
    pub(crate) fn resized(
        &mut self,
        raw: winit::window::WindowId,
        compositor: &mut C,
        width: u32,
        height: u32,
    ) {
        let Some(parent) = self.by_raw.get(&raw) else {
            return;
        };
        if let Some(Slot::Ready(popup)) = self.slots.get_mut(parent) {
            compositor.configure_surface(&mut popup.surface, width.max(1), height.max(1));
        }
    }
}

/// Converts a popup's input to its window's coordinates.
pub(crate) fn translate(
    event: winit::event::WindowEvent,
    offset: Vector,
    scale_factor: f32,
) -> winit::event::WindowEvent {
    use winit::event::WindowEvent;
    let shift = |position: winit::dpi::PhysicalPosition<f64>| winit::dpi::PhysicalPosition {
        x: position.x + f64::from(offset.x * scale_factor),
        y: position.y + f64::from(offset.y * scale_factor),
    };
    match event {
        WindowEvent::CursorMoved {
            device_id,
            position,
        } => WindowEvent::CursorMoved {
            device_id,
            position: shift(position),
        },
        WindowEvent::Touch(mut touch) => {
            touch.location = shift(touch.location);
            WindowEvent::Touch(touch)
        }
        other => other,
    }
}

/// Whether a popup's event is input its window should receive.
pub(crate) fn is_input(event: &winit::event::WindowEvent) -> bool {
    use winit::event::WindowEvent;
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::Ime(_)
            | WindowEvent::PinchGesture { .. }
            | WindowEvent::PanGesture { .. }
    )
}

/// The parent-relative placement of a popup over `area`, placed against
/// `anchor` when the overlays have one: then the compositor flips it to the
/// other side of the anchor when it does not fit, as it slides it otherwise.
#[cfg(all(target_os = "linux", feature = "wayland"))]
pub(crate) fn popup_attributes(
    parent: winit::window::WindowId,
    area: Rectangle,
    anchor: Option<Rectangle>,
    grab: bool,
) -> winit::platform::wayland::PopupAttributes {
    use winit::platform::wayland::{PopupAttributes, PopupConstraint, PopupEdge};
    let positioner = positioner(area, anchor);
    PopupAttributes {
        parent,
        anchor_rect: positioner.anchor_rect,
        size: positioner.size,
        anchor: if positioner.anchored {
            PopupEdge::BottomLeft
        } else {
            PopupEdge::TopLeft
        },
        gravity: PopupEdge::BottomRight,
        offset: positioner.offset,
        constraint_adjustment: if positioner.anchored {
            PopupConstraint::FLIP_Y
                .union(PopupConstraint::SLIDE_X)
                .union(PopupConstraint::SLIDE_Y)
        } else {
            PopupConstraint::SLIDE_X.union(PopupConstraint::SLIDE_Y)
        },
        // Only overlays that ask for it (menus, not tooltips); the backend
        // also needs a recent press and no other grabbing popup.
        grab,
    }
}

/// An `xdg_positioner` in whole logical pixels.
#[cfg(any(test, all(target_os = "linux", feature = "wayland")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Positioner {
    anchor_rect: (i32, i32, i32, i32),
    size: (i32, i32),
    offset: (i32, i32),
    /// Placed from the anchor's bottom-left corner, rather than at the
    /// area's own top-left.
    anchored: bool,
}

#[cfg(any(test, all(target_os = "linux", feature = "wayland")))]
#[allow(
    clippy::cast_possible_truncation,
    reason = "logical pixels of a window fit i32"
)]
fn positioner(area: Rectangle, anchor: Option<Rectangle>) -> Positioner {
    let size = (
        area.width.round().max(1.0) as i32,
        area.height.round().max(1.0) as i32,
    );
    match anchor {
        // The popup's top-left sits `offset` from the anchor's bottom-left.
        // Flipping mirrors anchor and gravity but, per the protocol, not the
        // offset, so the anchor is narrowed around the trigger's middle
        // until the offset is about zero: the popup then keeps the same
        // distance from the trigger on either side of it.
        Some(anchor) => {
            let middle = anchor.y + anchor.height / 2.0;
            let reach = area.y - middle;
            let half = reach.max(0.5);
            let rect = (
                anchor.x.round() as i32,
                (middle - half).round() as i32,
                anchor.width.round().max(1.0) as i32,
                (2.0 * half).round().max(1.0) as i32,
            );
            Positioner {
                anchor_rect: rect,
                size,
                offset: (
                    area.x.round() as i32 - rect.0,
                    area.y.round() as i32 - (rect.1 + rect.3),
                ),
                anchored: true,
            }
        }
        None => Positioner {
            anchor_rect: (area.x.round() as i32, area.y.round() as i32, size.0, size.1),
            size,
            offset: (0, 0),
            anchored: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Point;

    fn origin(area: Rectangle) -> Point {
        Point::new(area.x, area.y)
    }

    #[test]
    fn overlays_inside_the_window_need_no_popup() {
        let window = Size::new(400.0, 300.0);
        assert_eq!(wanted_area(None, window), None);
        assert_eq!(
            wanted_area(
                Some(Rectangle::new(
                    Point::new(10.0, 10.0),
                    Size::new(100.0, 50.0)
                )),
                window
            ),
            None
        );
    }

    #[test]
    fn overlays_past_an_edge_need_a_popup_with_shadow_room() {
        let window = Size::new(400.0, 300.0);
        let menu = Rectangle::new(Point::new(300.0, 250.0), Size::new(160.0, 200.5));
        let area = wanted_area(Some(menu), window).expect("past the bottom right");
        assert_eq!(origin(area), Point::new(276.0, 226.0));
        assert_eq!(area.size(), Size::new(208.0, 249.0));
    }

    #[test]
    fn popup_input_lands_where_the_overlay_is_in_the_window() {
        let moved = translate(
            winit::event::WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(10.0, 4.0),
            },
            Vector::new(276.0, 226.0),
            2.0,
        );
        match moved {
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                assert_eq!((position.x, position.y), (562.0, 456.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_anchored_popup_hangs_from_its_anchor() {
        // A menu 4 px below a 100 x 30 button, its popup 24 px larger for
        // the shadow: the popup starts 20 px above the button's bottom.
        let button = Rectangle::new(Point::new(50.0, 200.0), Size::new(100.0, 30.0));
        let area = Rectangle::new(Point::new(26.0, 210.0), Size::new(208.0, 180.0));
        let anchored = positioner(area, Some(button));
        let (x, y, width, height) = anchored.anchor_rect;
        assert_eq!((x, width), (50, 100));
        assert_eq!(anchored.size, (208, 180));
        assert!(anchored.anchored);
        // Below: the popup's top is where the area's top is.
        assert_eq!(y + height + anchored.offset.1, 210);
        assert_eq!(x + anchored.offset.0, 26);
        // Flipped above (anchor and gravity mirrored, offset kept, as the
        // protocol says): the menu ends about as far above the button's top
        // as it starts below its bottom.
        let popup_bottom = y + anchored.offset.1;
        let menu_bottom = popup_bottom - 24;
        let gap_above = 200 - menu_bottom;
        assert!(
            (gap_above - 4).abs() <= 12,
            "{gap_above} px above the button against 4 px below"
        );

        let free = positioner(area, None);
        assert_eq!(
            (free.anchor_rect, free.offset, free.anchored),
            ((26, 210, 208, 180), (0, 0), false)
        );
    }
}
