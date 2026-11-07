// Modified by enzUI contributors. See FORK.md.
//! Native raster/vector allocation and placement in logical pixels.
use super::Limits;
use crate::{ContentFit, Length, Point, Rectangle, Rotation, Size, Vector};

/// Intrinsic media geometry; crop is applied before rotation and fitting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Media {
    /// Decoded, orientation-corrected dimensions, not encoded file dimensions.
    pub source: Size<u32>,
    /// Outer allocation strategies.
    pub sizing: Size<Length>,
    /// Source-pixel region; ordinary image clipping keeps the original resource.
    pub crop: Option<Rectangle<u32>>,
    /// Native aspect-ratio strategy.
    pub fit: ContentFit,
    /// Floating rotation preserves layout size; solid rotation changes it.
    pub rotation: Rotation,
    /// Uses available maximum limits before shrinking requested axes.
    pub expand: bool,
}
impl Media {
    /// Intrinsic cropped size; position is accounted for during drawing.
    pub fn cropped(self) -> Size {
        self.crop.map_or(
            Size::new(self.source.width as f32, self.source.height as f32),
            |region| {
                Size::new(
                    region.width.min(self.source.width) as f32,
                    region.height.min(self.source.height) as f32,
                )
            },
        )
    }

    /// Resolves the same fit/shrink allocation used by native image and SVG widgets.
    pub fn allocation(self, limits: &Limits) -> Size {
        let rotated = self.rotation.apply(self.cropped());
        let bounds = if self.expand {
            limits
                .width(self.sizing.width)
                .height(self.sizing.height)
                .max()
        } else {
            limits.resolve(self.sizing.width, self.sizing.height, rotated)
        };
        let fitted = self.fit.fit(rotated, bounds);
        Size {
            width: if matches!(self.sizing.width, Length::Fit | Length::Shrink) {
                bounds.width.min(fitted.width)
            } else {
                bounds.width
            },
            height: if matches!(self.sizing.height, Length::Fit | Length::Shrink) {
                bounds.height.min(fitted.height)
            } else {
                bounds.height
            },
        }
    }

    /// Fits the full resource around its source crop without scaling borders or siblings.
    pub fn drawing_bounds(self, bounds: Rectangle, scale: f32) -> Rectangle {
        // A source that is still loading has no size to fit: it draws
        // nothing, centred in its allocation, rather than at `NaN`.
        if !self.is_resolved() {
            return Rectangle::new(bounds.center(), Size::ZERO);
        }
        let cropped = self.cropped();
        let rotated = self.rotation.apply(cropped);
        let fitted = self.fit.fit(rotated, bounds.size());
        let fit_scale = Vector::new(fitted.width / rotated.width, fitted.height / rotated.height);
        let size = cropped * fit_scale * scale;
        let (offset, size) = if let Some(region) = self.crop {
            let x = region.x.min(self.source.width) as f32;
            let y = region.y.min(self.source.height) as f32;
            let size = size
                * Vector::new(
                    self.source.width as f32 / cropped.width,
                    self.source.height as f32 / cropped.height,
                );
            let scale = Vector::new(
                size.width / self.source.width as f32,
                size.height / self.source.height as f32,
            );
            let offset = match self.fit {
                ContentFit::None => Vector::new(x * scale.x, y * scale.y),
                _ => Vector::new(
                    ((self.source.width as f32 - cropped.width) / 2.0 - x) * scale.x,
                    ((self.source.height as f32 - cropped.height) / 2.0 - y) * scale.y,
                ),
            };
            (offset, size)
        } else {
            (Vector::ZERO, size)
        };
        let position = match self.fit {
            ContentFit::None => Point::new(
                bounds.x + (rotated.width - fitted.width) / 2.0,
                bounds.y + (rotated.height - fitted.height) / 2.0,
            ),
            _ => Point::new(
                bounds.center_x() - size.width / 2.0,
                bounds.center_y() - size.height / 2.0,
            ),
        };
        Rectangle::new(position + offset, size)
    }

    /// Capture refuses missing or degenerate resources; this does not validate decoder budgets.
    pub fn is_resolved(self) -> bool {
        let size = self.cropped();
        self.source.width > 0
            && self.source.height > 0
            && size.width > 0.0
            && size.height > 0.0
            && self.rotation.radians().0.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image whose file is still decoding measures as zero. Its drawing
    /// bounds are empty and finite, so a scene that shows it stays valid.
    ///
    /// Sabotage: drop the unresolved guard -- `Contain` divides zero by zero
    /// and the bounds are `NaN`.
    #[test]
    fn an_image_still_loading_draws_nothing_at_finite_bounds() {
        let media = Media {
            source: Size::new(0, 0),
            sizing: Size::new(Length::Fixed(16.0), Length::Fixed(16.0)),
            crop: None,
            fit: ContentFit::Contain,
            rotation: Rotation::default(),
            expand: false,
        };
        let bounds = Rectangle::new(Point::new(287.4, 229.0), Size::new(16.0, 16.0));

        let drawn = media.drawing_bounds(bounds, 1.0);

        assert_eq!(drawn, Rectangle::new(bounds.center(), Size::ZERO));
    }
}
