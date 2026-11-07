// Modified by enzUI contributors. See FORK.md.
//! Shared scroll allocation and offset rules for native layout and owned presentation.
use super::{Limits, Node};
use crate::{Length, Padding, Size};

/// Measures one scroll child with unbounded limits only along enabled axes.
///
/// Axis order is horizontal, vertical. Intrinsic errors propagate before padded
/// positioning; child-owned side effects are not rolled back.
pub fn resolve<E>(
    limits: &Limits,
    sizing: Size<Length>,
    axes: [bool; 2],
    padding: Padding,
    measure: impl FnOnce(&Limits) -> Result<Node, E>,
) -> Result<Node, E> {
    super::try_positioned(
        limits,
        sizing.width,
        sizing.height,
        padding,
        |limits| {
            // Upstream #3472: the scroll axes keep the viewport as a maximum
            // *hint* and are marked infinite, so the content may exceed it.
            // enzICED: fluid content is also compressed along them, as before
            // the merge: a `Fill` child on a scroll axis hugs its intrinsic
            // extent. Upstream inherits compression instead, which grows a
            // `Fill` child to at least the viewport, and to infinity wherever
            // a wrapper rebuilds its limits from `Limits::max()`.
            let child = Limits::with_flags(
                limits.min,
                limits.max,
                Size::new(
                    limits.compression.width || axes[0],
                    limits.compression.height || axes[1],
                ),
                Size::new(
                    limits.infinite.width || axes[0],
                    limits.infinite.height || axes[1],
                ),
            );
            measure(&child)
        },
        |child, _| child,
    )
}

/// Scroll position expressed in pixels or as a fraction of the remaining content extent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Offset {
    /// Logical pixels from the selected anchor.
    Absolute(f32),
    /// Fraction of content extent beyond the viewport.
    Relative(f32),
}
impl Offset {
    /// Resolves against the current content extent, including after responsive reflow.
    pub fn absolute(self, viewport: f32, content: f32) -> f32 {
        match self {
            Self::Absolute(value) => value.min((content - viewport).max(0.0)),
            Self::Relative(value) => ((content - viewport) * value).max(0.0),
        }
    }
    /// Resolves the translation from the start edge; `from_end` reverses the anchor.
    pub fn translation(self, viewport: f32, content: f32, from_end: bool) -> f32 {
        let offset = self.absolute(viewport, content);
        if from_end {
            ((content - viewport).max(0.0) - offset).max(0.0)
        } else {
            offset
        }
    }
}

/// Immutable scrollbar sizing, independent of interaction state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// Rail thickness in logical pixels.
    pub width: f32,
    /// Outer rail margin.
    pub margin: f32,
    /// Thumb thickness in logical pixels.
    pub scroller_width: f32,
    /// Minimum thumb travel-axis length, clamped to the track.
    pub scroller_min_length: f32,
    /// Whether offset zero is anchored at the content end.
    pub from_end: bool,
    /// Embedded rail spacing in logical pixels; `None` floats over content.
    pub spacing: Option<f32>,
    /// Leading and trailing track insets in logical pixels, independent of content layout.
    pub padding: [f32; 2],
}
/// Enabled scroll axes and their rail sizing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Horizontal scrolling, if enabled.
    pub horizontal: Option<Bar>,
    /// Vertical scrolling, if enabled.
    pub vertical: Option<Bar>,
}
/// Draw-time scroll state, captured independently of layout metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Input {
    /// Horizontal and vertical offsets, preserving absolute/relative semantics.
    pub offsets: [Offset; 2],
    /// Embedded-rail measurement seed from the last successful layout.
    pub visible: bool,
    /// Paint rounding scale; positive and finite for retained capture.
    pub scale: f32,
}
impl Config {
    /// Resolves the content translation against current allocations.
    ///
    /// It is fractional in logical pixels, like upstream's smooth-scrolling
    /// `scrollable`. Paint quantizes it to physical pixels
    /// ([`paint_translation`](Self::paint_translation)).
    pub fn translation(self, input: Input, viewport: Size, content: Size) -> crate::Vector {
        crate::Vector::new(
            self.horizontal.map_or(0.0, |bar| {
                input.offsets[0].translation(viewport.width, content.width, bar.from_end)
            }),
            self.vertical.map_or(0.0, |bar| {
                input.offsets[1].translation(viewport.height, content.height, bar.from_end)
            }),
        )
    }
    /// Quantizes the content translation to the renderer's physical pixels.
    pub fn paint_translation(self, input: Input, viewport: Size, content: Size) -> crate::Vector {
        self.translation(input, viewport, content).hint(input.scale)
    }
    /// Uses the native two-pass embedded-rail rule; errors do not publish visibility.
    pub fn resolve<E>(
        self,
        limits: &Limits,
        sizing: Size<Length>,
        visible: bool,
        mut measure: impl FnMut(&Limits) -> Result<Node, E>,
    ) -> Result<(Node, bool), E> {
        let embedded = match (self.horizontal, self.vertical) {
            (Some(bar), None) => bar
                .spacing
                .map(|spacing| (false, bar.width + bar.margin * 2.0 + spacing)),
            (None, Some(bar)) => bar
                .spacing
                .map(|spacing| (true, bar.width + bar.margin * 2.0 + spacing)),
            _ => None,
        };
        let mut pass = |visible| {
            let padding = embedded.map_or(Padding::ZERO, |(vertical, amount)| {
                if visible {
                    if vertical {
                        Padding {
                            right: amount,
                            ..Padding::ZERO
                        }
                    } else {
                        Padding {
                            bottom: amount,
                            ..Padding::ZERO
                        }
                    }
                } else {
                    Padding::ZERO
                }
            });
            resolve(
                limits,
                sizing,
                [self.horizontal.is_some(), self.vertical.is_some()],
                padding,
                &mut measure,
            )
        };
        let first = pass(visible)?;
        let Some((vertical, _)) = embedded else {
            return Ok((first, visible));
        };
        let next = if vertical {
            first.children()[0].size().height > first.size().height
        } else {
            first.children()[0].size().width > first.size().width
        };
        if next == visible {
            Ok((first, next))
        } else {
            Ok((pass(next)?, next))
        }
    }
    /// Rejects nonfinite or negative rail metrics; it does not bound content work.
    pub fn valid(self) -> bool {
        self.horizontal.into_iter().chain(self.vertical).all(|bar| {
            [
                bar.width,
                bar.margin,
                bar.scroller_width,
                bar.scroller_min_length,
                bar.padding[0],
                bar.padding[1],
            ]
            .into_iter()
            .all(|v| v.is_finite() && v >= 0.0)
                && bar.spacing.is_none_or(|v| v.is_finite() && v >= 0.0)
        })
    }
}
impl Input {
    /// Requires finite nonnegative offsets and a finite, strictly positive paint scale.
    pub fn valid(self) -> bool {
        self.scale.is_finite()
            && self.scale > 0.0
            && self.offsets.iter().all(|offset| match offset {
                Offset::Absolute(v) | Offset::Relative(v) => v.is_finite() && *v >= 0.0,
            })
    }
}
/// Native rail/thumbnail geometry at one sampled allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RailGeometry {
    /// Interactive rail plus margins.
    pub total_bounds: crate::Rectangle,
    /// Painted rail allocation.
    pub bounds: crate::Rectangle,
    /// Painted thumb allocation.
    pub scroller: Option<crate::Rectangle>,
    /// Reversed anchor used by drag mapping.
    pub from_end: bool,
    /// Whether content fits the viewport along this axis.
    pub disabled: bool,
    /// Whether the rail floats over the content (no embedded spacing).
    pub floating: bool,
}
/// Horizontal and vertical rail geometry from the shared native calculation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bars {
    /// Horizontal rail.
    pub x: Option<RailGeometry>,
    /// Vertical rail.
    pub y: Option<RailGeometry>,
}

impl Bars {
    /// Computes both rails without letting minimum thumb sizes shorten endpoint reachability.
    pub fn new(
        config: Config,
        input: Input,
        bounds: crate::Rectangle,
        content_bounds: crate::Rectangle,
    ) -> Self {
        use crate::Rectangle;
        let translation = config.translation(input, bounds.size(), content_bounds.size());

        let show_scrollbar_x = config
            .horizontal
            .as_ref()
            .filter(|_scrollbar| content_bounds.width > bounds.width);

        let show_scrollbar_y = config
            .vertical
            .as_ref()
            .filter(|_scrollbar| content_bounds.height > bounds.height);

        let y_scrollbar = if let Some(vertical) = show_scrollbar_y {
            let Bar {
                width,
                margin,
                scroller_width,
                scroller_min_length,
                spacing,
                padding,
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

            let ratio = bounds.height / content_bounds.height;

            let scroller = if ratio >= 1.0 || scrollbar_height <= 0.0 {
                None
            } else {
                let scroller_height = (scrollbar_bounds.height * ratio)
                    .max(scroller_min_length)
                    .min(scrollbar_bounds.height);
                let max_scroll = (content_bounds.height - bounds.height).max(0.0);
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

                Some(scroller_bounds)
            };

            Some(RailGeometry {
                total_bounds: total_scrollbar_bounds,
                bounds: scrollbar_bounds,
                scroller,
                from_end: vertical.from_end,
                disabled: content_bounds.height <= bounds.height,
                floating: spacing.is_none(),
            })
        } else {
            None
        };

        let x_scrollbar = if let Some(horizontal) = show_scrollbar_x {
            let Bar {
                width,
                margin,
                scroller_width,
                scroller_min_length,
                spacing,
                padding,
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

            let ratio = bounds.width / content_bounds.width;

            let scroller = if ratio >= 1.0 || scrollbar_width <= 0.0 {
                None
            } else {
                let scroller_length = (scrollbar_bounds.width * ratio)
                    .max(scroller_min_length)
                    .min(scrollbar_bounds.width);
                let max_scroll = (content_bounds.width - bounds.width).max(0.0);
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

                Some(scroller_bounds)
            };

            Some(RailGeometry {
                total_bounds: total_scrollbar_bounds,
                bounds: scrollbar_bounds,
                scroller,
                from_end: horizontal.from_end,
                disabled: content_bounds.width <= bounds.width,
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
}
