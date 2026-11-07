// Modified by enzUI contributors. See FORK.md.
//! Allocation-relative one-pixel borders, retaining stroke metrics through reflow.

use super::{Path, Stroke};
use crate::core::{Color, Point, Size, border::Radius};

/// Contour edges included in a border path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The leading edge and its two adjoining rounded corners.
    Leading,
    /// The trailing edge and its two adjoining rounded corners.
    Trailing,
    /// The complete closed contour.
    Outline,
}

/// A finite declarative contour; reflow regenerates vertices, never scales stroke width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    /// Edges included in the contour.
    pub edge: Edge,
    /// Extend a single-edge contour across the top to the opposite side.
    pub top: bool,
    /// Top-left, top-right, bottom-left and bottom-right radii in logical pixels.
    pub corners: [f32; 4],
    /// Stroke ink, including alpha.
    pub color: Color,
}
impl Border {
    /// Checks the finite nonnegative radius and finite color contract.
    pub fn is_valid(self) -> bool {
        self.corners.into_iter().all(|r| r.is_finite() && r >= 0.0)
            && [self.color.r, self.color.g, self.color.b, self.color.a]
                .into_iter()
                .all(f32::is_finite)
    }

    /// Builds at most two cubic corners or one closed rounded rectangle, at a fixed one-pixel stroke width.
    pub fn path(self, bounds: Size) -> Option<Path> {
        if !self.is_valid()
            || !bounds.width.is_finite()
            || !bounds.height.is_finite()
            || bounds.width < 1.0
            || bounds.height < 1.0
        {
            return None;
        }
        let [tl, tr, bl, br] = self.corners;
        let clamp = |r: f32| r.min(bounds.width / 2.0).min(bounds.height / 2.0);
        if self.edge == Edge::Outline {
            return Some(Path::rounded_rectangle(
                Point::new(0.5, 0.5),
                Size::new(
                    (bounds.width - 1.0).max(0.0),
                    (bounds.height - 1.0).max(0.0),
                ),
                Radius {
                    top_left: clamp(tl),
                    top_right: clamp(tr),
                    bottom_left: clamp(bl),
                    bottom_right: clamp(br),
                },
            ));
        }
        let (r_top, r_bottom, x, inward) = match self.edge {
            Edge::Leading => (clamp(tl), clamp(bl), 0.5, 1.0),
            Edge::Trailing => (clamp(tr), clamp(br), bounds.width - 0.5, -1.0),
            Edge::Outline => unreachable!(),
        };
        let k = 0.552_284_8;
        let top_start = if self.top {
            if self.edge == Edge::Leading {
                bounds.width
            } else {
                0.0
            }
        } else {
            x + inward * r_top
        };
        let bottom_line_y = if r_bottom > 0.0 {
            bounds.height - r_bottom
        } else {
            bounds.height - 0.5
        };
        Some(Path::new(|builder| {
            builder.move_to(Point::new(top_start, 0.5));
            if self.top {
                builder.line_to(Point::new(x + inward * r_top, 0.5));
            }
            if r_top > 0.0 {
                builder.bezier_curve_to(
                    Point::new(x + inward * r_top * (1.0 - k), 0.5),
                    Point::new(x, r_top * (1.0 - k)),
                    Point::new(x, r_top),
                );
            } else {
                builder.line_to(Point::new(x, 0.5));
            }
            builder.line_to(Point::new(x, bottom_line_y));
            if r_bottom > 0.0 {
                builder.bezier_curve_to(
                    Point::new(x, bounds.height - r_bottom * (1.0 - k)),
                    Point::new(x + inward * r_bottom * (1.0 - k), bounds.height - 0.5),
                    Point::new(x + inward * r_bottom, bounds.height - 0.5),
                );
            }
        }))
    }
}

/// Draws the same contour on backends without owned geometry capture.
pub fn draw<R: super::Renderer>(renderer: &mut R, bounds: crate::core::Rectangle, border: Border) {
    let mut frame = super::Frame::new(renderer, bounds.size());
    if let Some(path) = border.path(bounds.size()) {
        frame.stroke(
            &path,
            Stroke::default().with_width(1.0).with_color(border.color),
        );
    }
    renderer.with_translation(crate::core::Vector::new(bounds.x, bounds.y), |renderer| {
        renderer.draw_geometry(frame.into_geometry());
    });
}
