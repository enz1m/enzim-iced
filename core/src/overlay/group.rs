// Modified by enzUI contributors. See FORK.md.
use crate::event;
use crate::mouse;
use crate::overlay;
use crate::renderer;
use crate::widget;
use crate::{Event, Shell};

/// The bounds an overlay's layer asks for: larger than any window, so the
/// enclosing clip (the window) decides (enzICED).
const OVERLAY_LAYER: crate::Rectangle = crate::Rectangle {
    x: -1.0e6,
    y: -1.0e6,
    width: 2.0e6,
    height: 2.0e6,
};

/// A container of nested overlays.
pub struct Group<'a, Message, Theme, Renderer> {
    children: Vec<overlay::Element<'a, Message, Theme, Renderer>>,
}

fn sort_overlays<'a, Message, Theme, Renderer>(
    children: &mut [overlay::Element<'a, Message, Theme, Renderer>],
) where
    Renderer: renderer::Renderer,
{
    use std::cmp;

    children.sort_by(|a, b| {
        a.as_overlay()
            .index()
            .partial_cmp(&b.as_overlay().index())
            .unwrap_or(cmp::Ordering::Equal)
    });
}

impl<'a, Message, Theme, Renderer> Group<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    /// Creates a [`Group`] container for the given overlays.
    ///
    /// The overlays are sorted by their
    /// [`index`](crate::Overlay::index).
    pub fn new(mut children: Vec<overlay::Element<'a, Message, Theme, Renderer>>) -> Self {
        sort_overlays(&mut children);

        Self { children }
    }

    /// Draws the [`Group`] overlay using the associated `Renderer`.
    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        cursor: mouse::Cursor,
    ) {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            renderer: &mut Renderer,
            theme: &Theme,
            style: &renderer::Style,
            cursor: mouse::Cursor,
        ) where
            Renderer: renderer::Renderer,
        {
            for element in children {
                // TODO: Get rid of cursor argument in `draw`
                let is_over = cursor.position().is_some_and(|cursor_position| {
                    let nested = element.as_overlay_mut().overlay(renderer);

                    !nested.is_empty()
                        && Group::new(nested)
                            .mouse_interaction(mouse::Cursor::Available(cursor_position), renderer)
                            != mouse::Interaction::None
                });

                // enzICED: each overlay draws in a layer of its own, above
                // the content beneath it. Within one layer every fill comes
                // before every glyph, so the window's text showed through a
                // popup that opened no layer itself. The window clips the
                // layer.
                renderer.with_layer(OVERLAY_LAYER, |renderer| {
                    element.as_overlay().draw(
                        renderer,
                        theme,
                        style,
                        if is_over {
                            mouse::Cursor::Unavailable
                        } else {
                            cursor
                        },
                    );
                });

                let mut nested = element.as_overlay_mut().overlay(renderer);

                if !nested.is_empty() {
                    sort_overlays(&mut nested);

                    recurse(&mut nested, renderer, theme, style, cursor);
                }
            }
        }

        recurse(&mut self.children, renderer, theme, style, cursor);
    }

    /// The smallest rectangle holding every overlay of the group, nested
    /// ones included, that reports its area, in window coordinates
    /// (enzICED); see [`overlay::Overlay::area`]. `None` when no overlay
    /// reports one.
    pub fn bounds(&mut self, renderer: &Renderer) -> Option<crate::Rectangle> {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            renderer: &Renderer,
            bounds: &mut Option<crate::Rectangle>,
        ) where
            Renderer: renderer::Renderer,
        {
            for element in children {
                if let Some(own) = element.as_overlay().area() {
                    *bounds = Some(match *bounds {
                        Some(bounds) => bounds.union(&own),
                        None => own,
                    });
                }
                let mut nested = element.as_overlay_mut().overlay(renderer);
                recurse(&mut nested, renderer, bounds);
            }
        }

        let mut bounds = None;
        recurse(&mut self.children, renderer, &mut bounds);
        bounds
    }

    /// The anchor of the first overlay of the group, nested ones included,
    /// that reports one (enzICED); see [`overlay::Overlay::anchor`].
    pub fn anchor(&mut self, renderer: &Renderer) -> Option<crate::Rectangle> {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            renderer: &Renderer,
        ) -> Option<crate::Rectangle>
        where
            Renderer: renderer::Renderer,
        {
            for element in children {
                if let Some(anchor) = element.as_overlay().anchor() {
                    return Some(anchor);
                }
                let mut nested = element.as_overlay_mut().overlay(renderer);
                if let Some(anchor) = recurse(&mut nested, renderer) {
                    return Some(anchor);
                }
            }
            None
        }

        recurse(&mut self.children, renderer)
    }

    /// Whether any overlay of the group, nested ones included, takes the
    /// input grab (enzICED); see [`overlay::Overlay::grabs`].
    pub fn grabs(&mut self, renderer: &Renderer) -> bool {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            renderer: &Renderer,
        ) -> bool
        where
            Renderer: renderer::Renderer,
        {
            children.iter_mut().any(|element| {
                element.as_overlay().grabs() || {
                    let mut nested = element.as_overlay_mut().overlay(renderer);
                    recurse(&mut nested, renderer)
                }
            })
        }

        recurse(&mut self.children, renderer)
    }

    /// Applies a [`widget::Operation`] to the [`Group`] overlay.
    pub fn operate(&mut self, renderer: &Renderer, operation: &mut dyn widget::Operation) {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            renderer: &Renderer,
            operation: &mut dyn widget::Operation,
        ) where
            Renderer: renderer::Renderer,
        {
            for element in children {
                let overlay = element.as_overlay_mut();

                overlay.operate(renderer, operation);

                let mut nested = overlay.overlay(renderer);

                if !nested.is_empty() {
                    sort_overlays(&mut nested);

                    recurse(&mut nested, renderer, operation);
                }
            }
        }

        recurse(&mut self.children, renderer, operation);
    }

    /// Processes a runtime [`Event`].
    pub fn update(
        &mut self,
        event: &Event,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
    ) {
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            event: &Event,
            cursor: mouse::Cursor,
            renderer: &Renderer,
            shell: &mut Shell<'_, Message>,
        ) -> bool
        where
            Renderer: renderer::Renderer,
        {
            let mut is_over = false;

            for element in children {
                if shell.event_status() != event::Status::Ignored {
                    return is_over;
                }

                let overlay = element.as_overlay_mut();

                let nested = overlay.overlay(renderer);
                let nested_is_over = (!nested.is_empty())
                    .then_some(nested)
                    .map(|mut nested| {
                        sort_overlays(&mut nested);

                        recurse(&mut nested, event, cursor, renderer, shell)
                    })
                    .unwrap_or_default();

                if shell.event_status() != event::Status::Ignored {
                    return nested_is_over || is_over;
                }

                let child_is_over = nested_is_over
                    || cursor.position().is_some_and(|cursor_position| {
                        overlay
                            .mouse_interaction(mouse::Cursor::Available(cursor_position), renderer)
                            != mouse::Interaction::None
                    });

                overlay.update(
                    event,
                    if nested_is_over {
                        mouse::Cursor::Unavailable
                    } else {
                        cursor
                    },
                    renderer,
                    shell,
                );

                is_over |= child_is_over;
            }

            is_over
        }

        let _ = recurse(&mut self.children, event, cursor, renderer, shell);
    }

    /// Returns the current [`mouse::Interaction`] of the [`Group`] overlay.
    pub fn mouse_interaction(
        &mut self,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        // #910: the topmost overlay wins. Children are sorted by index, so
        // they are walked from the last (highest) to the first, and a nested
        // overlay sits above the one that opened it. The first non-`None`
        // interaction is the overlay the user can see. Ranking by `max()`
        // follows the `Interaction` enum's declaration order instead, so a
        // buried text field's `Text` would beat an open dropdown's `Pointer`
        // even though the dropdown covers the field.
        fn recurse<Message, Theme, Renderer>(
            children: &mut [overlay::Element<'_, Message, Theme, Renderer>],
            cursor: mouse::Cursor,
            renderer: &Renderer,
        ) -> mouse::Interaction
        where
            Renderer: renderer::Renderer,
        {
            children
                .iter_mut()
                .rev()
                .find_map(|element| {
                    let overlay = element.as_overlay_mut();
                    let interaction = overlay.mouse_interaction(cursor, renderer);

                    let mut nested = overlay.overlay(renderer);
                    let nested_interaction = if nested.is_empty() {
                        mouse::Interaction::None
                    } else {
                        sort_overlays(&mut nested);

                        recurse(&mut nested, cursor, renderer)
                    };

                    [nested_interaction, interaction]
                        .into_iter()
                        .find(|interaction| *interaction != mouse::Interaction::None)
                })
                .unwrap_or_default()
        }

        recurse(&mut self.children, cursor, renderer)
    }
}
