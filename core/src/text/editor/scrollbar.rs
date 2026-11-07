// Modified by enzUI contributors. See FORK.md.
//! Editor overlay geometry shared by native hit testing and owned paint.
use super::ScrollMetrics;
use crate::{Border, Color, Point, Rectangle, Size, renderer};

/// Logical pixel metrics; no theme callback survives capture.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// Painted thumb thickness in logical pixels.
    pub width: f32,
    /// Pointer band thickness, independent of the painted thumb.
    pub hit_width: f32,
    /// Thumb length floor before fitting to the available track.
    pub min_thumb_length: f32,
    /// Corner radius in logical pixels.
    pub radius: f32,
}
/// Hit band and thumb geometry, with absolute scroll mapping.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// Logical viewport coordinates used by pointer routing.
    pub band: Rectangle,
    /// Painted rectangle in the same coordinate space as the hit band.
    pub thumb: Rectangle,
    /// Distance available to the thumb after subtracting its length.
    pub travel: f32,
    /// Absolute content offset corresponding to the end of the track.
    pub max_offset: f32,
}
/// Frozen pointer/drag state; geometry and hover intersection are resolved at the sampled allocation.
#[derive(Debug, Clone, Copy)]
pub struct Paint {
    /// Native geometry tokens captured with the scene revision.
    pub metrics: Metrics,
    /// Explicit materialized-document mapping; arbitrary metric overrides are not retained.
    pub projection: Option<Projection>,
    /// Enables the lower-edge scrollbar in addition to the trailing-edge scrollbar.
    pub horizontal: bool,
    /// Nonnegative leading inset in logical pixels, including editor padding and gutter.
    pub horizontal_inset: f32,
    /// Last native pointer position in logical window coordinates.
    pub cursor: Option<Point>,
    /// Vertical then horizontal drag ownership; a drag keeps its thumb visible.
    pub dragged: [bool; 2],
    /// Base thumb ink before hover/drag opacity.
    pub ink: Color,
    /// Pointer-hover opacity in the unit interval.
    pub alpha: f32,
    /// Active-drag opacity in the unit interval.
    pub alpha_hover: f32,
    /// How far each rail end keeps from the rounded corners around the
    /// editor: vertical leading and trailing, then horizontal (enzICED, see
    /// [`crate::clip_shape`] and [`cleared`]).
    pub clearance: [f32; 4],
}
impl Paint {
    /// Rejects invalid dimensions, opacity or pointer/ink data before retained publication.
    pub fn valid(self) -> bool {
        [
            self.metrics.width,
            self.metrics.hit_width,
            self.metrics.min_thumb_length,
            self.metrics.radius,
            self.horizontal_inset,
            self.clearance[0],
            self.clearance[1],
            self.clearance[2],
            self.clearance[3],
        ]
        .into_iter()
        .all(|v| v.is_finite() && v >= 0.0)
            && [self.alpha, self.alpha_hover]
                .into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
            && [self.ink.r, self.ink.g, self.ink.b, self.ink.a]
                .into_iter()
                .all(f32::is_finite)
            && self
                .cursor
                .is_none_or(|p| p.x.is_finite() && p.y.is_finite())
    }
    /// Resolves the declared document mapping against freshly sampled editor metrics.
    pub fn projected_metrics(self, metrics: ScrollMetrics) -> Option<ScrollMetrics> {
        self.projection
            .map_or(Some(metrics), |projection| projection.apply(metrics))
    }
    /// Computes at most two overlay quads; hidden chrome produces no ink.
    pub fn quads(
        self,
        bounds: Rectangle,
        metrics: ScrollMetrics,
    ) -> [Option<(renderer::Quad, Color)>; 2] {
        let Some(metrics) = self.projected_metrics(metrics) else {
            return [None, None];
        };
        let bars = [
            vertical(cleared(bounds, self.clearance, true), metrics, self.metrics),
            self.horizontal
                .then(|| {
                    horizontal_inset(
                        cleared(bounds, self.clearance, false),
                        metrics,
                        self.metrics,
                        self.horizontal_inset,
                    )
                })
                .flatten(),
        ];
        std::array::from_fn(|index| {
            let geometry = bars[index]?;
            let dragged = self.dragged[index];
            (dragged || self.cursor.is_some_and(|p| geometry.band.contains(p))).then(|| {
                (
                    renderer::Quad {
                        bounds: geometry.thumb,
                        border: Border::default().rounded(self.metrics.radius),
                        ..Default::default()
                    },
                    self.ink.scale_alpha(if dragged {
                        self.alpha_hover
                    } else {
                        self.alpha
                    }),
                )
            })
        })
    }
}

/// `bounds` with the ends of its `vertical` (or horizontal) rail moved in by
/// `clearance` (vertical leading and trailing, then horizontal), to clear
/// rounded corners around the editor; the other axis is unchanged.
pub fn cleared(mut bounds: Rectangle, clearance: [f32; 4], vertical: bool) -> Rectangle {
    let sane = |value: f32| {
        if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        }
    };
    if vertical {
        let top = sane(clearance[0]).min(bounds.height);
        let bottom = sane(clearance[1]).min(bounds.height - top);
        bounds.y += top;
        bounds.height -= top + bottom;
    } else {
        let left = sane(clearance[2]).min(bounds.width);
        let right = sane(clearance[3]).min(bounds.width - left);
        bounds.x += left;
        bounds.width -= left + right;
    }
    bounds
}

/// Trailing-edge geometry, shortened when horizontal overflow occupies the corner.
pub fn vertical(bounds: Rectangle, metrics: ScrollMetrics, tokens: Metrics) -> Option<Geometry> {
    let max_offset = (metrics.content.height - metrics.viewport.height).max(0.0);
    if max_offset <= 0.0 || bounds.height <= 0.0 {
        return None;
    }
    let horizontal_inset = if metrics.content.width > metrics.viewport.width {
        tokens.hit_width
    } else {
        0.0
    };
    let track = (bounds.height - horizontal_inset).max(0.0);
    let length = (track * metrics.viewport.height / metrics.content.height)
        .max(tokens.min_thumb_length)
        .min(track);
    let travel = (track - length).max(0.0);
    let start = if max_offset > 0.0 {
        metrics.offset.y.clamp(0.0, max_offset) / max_offset * travel
    } else {
        0.0
    };
    let band = Rectangle::new(
        Point::new(bounds.x + bounds.width - tokens.hit_width, bounds.y),
        Size::new(tokens.hit_width, track),
    );
    Some(Geometry {
        band,
        thumb: Rectangle::new(
            Point::new(
                band.x + (tokens.hit_width - tokens.width) / 2.0,
                bounds.y + start,
            ),
            Size::new(tokens.width, length),
        ),
        travel,
        max_offset,
    })
}

/// Lower-edge geometry, shortened when vertical overflow occupies the corner.
pub fn horizontal(bounds: Rectangle, metrics: ScrollMetrics, tokens: Metrics) -> Option<Geometry> {
    horizontal_inset(bounds, metrics, tokens, 0.0)
}

/// Lower-edge geometry excluding a leading inset in logical pixels.
///
/// A fully excluded rail is absent. Negative or nonfinite insets are rejected,
/// and the trailing corner stays fixed.
pub fn horizontal_inset(
    mut bounds: Rectangle,
    metrics: ScrollMetrics,
    tokens: Metrics,
    leading: f32,
) -> Option<Geometry> {
    if !leading.is_finite() || leading < 0.0 {
        return None;
    }
    let leading = leading.min(bounds.width.max(0.0));
    bounds.x += leading;
    bounds.width -= leading;
    let max_offset = (metrics.content.width - metrics.viewport.width).max(0.0);
    if max_offset <= 0.0 || bounds.width <= 0.0 {
        return None;
    }
    let vertical_inset = if metrics.content.height > metrics.viewport.height {
        tokens.hit_width
    } else {
        0.0
    };
    let track = (bounds.width - vertical_inset).max(0.0);
    let length = (track * metrics.viewport.width / metrics.content.width)
        .max(tokens.min_thumb_length)
        .min(track);
    let travel = (track - length).max(0.0);
    let start = if max_offset > 0.0 {
        metrics.offset.x.clamp(0.0, max_offset) / max_offset * travel
    } else {
        0.0
    };
    let band = Rectangle::new(
        Point::new(bounds.x, bounds.y + bounds.height - tokens.hit_width),
        Size::new(track, tokens.hit_width),
    );
    Some(Geometry {
        band,
        thumb: Rectangle::new(
            Point::new(
                bounds.x + start,
                band.y + (tokens.hit_width - tokens.width) / 2.0,
            ),
            Size::new(length, tokens.width),
        ),
        travel,
        max_offset,
    })
}

/// Estimated document scroll space derived from one materialized editor slice.
/// The mean laid-out logical-line height is shared by native input and owned chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Projection {
    first_line: usize,
    total_lines: usize,
    materialized_lines: usize,
}
impl Projection {
    /// Returns `None` for empty populations or a slice starting outside the document.
    pub fn new(first_line: usize, total_lines: usize, materialized_lines: usize) -> Option<Self> {
        (materialized_lines > 0 && total_lines > 0 && first_line < total_lines).then_some(Self {
            first_line,
            total_lines,
            materialized_lines,
        })
    }
    /// Number of lines in the exact native editor model this projection describes.
    pub fn materialized_lines(self) -> usize {
        self.materialized_lines
    }
    /// Source-document origin of the native editor's first line.
    pub fn first_line(self) -> usize {
        self.first_line
    }
    /// Maps native metrics without reading a document or invoking a callback.
    pub fn apply(self, mut local: ScrollMetrics) -> Option<ScrollMetrics> {
        if [
            local.viewport.width,
            local.viewport.height,
            local.content.width,
            local.content.height,
            local.offset.x,
            local.offset.y,
            local.first_line_top,
        ]
        .into_iter()
        .any(|v| !v.is_finite() || v < 0.0)
            || local.first_line >= self.materialized_lines
        {
            return None;
        }
        let row = (local.content.height / self.materialized_lines as f32).max(1.0);
        let leading = self.first_line as f32 * row;
        local.first_line = local.first_line.checked_add(self.first_line)?;
        local.first_line_top += leading;
        local.offset.y += leading;
        local.content.height = self.total_lines as f32 * row;
        [
            local.content.height,
            local.first_line_top,
            local.offset.y,
            row,
        ]
        .into_iter()
        .all(|v| v.is_finite() && v >= 0.0)
        .then_some(local)
    }
    /// Whether replay can fill the viewport without exposing unmaterialized document rows.
    pub fn covers(self, local: ScrollMetrics) -> bool {
        self.first_line.saturating_add(self.materialized_lines) >= self.total_lines
            || local.offset.y + local.viewport.height <= local.content.height + 0.5
    }
}
