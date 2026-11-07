// Modified by enzUI contributors. See FORK.md.
//! Rule paint geometry shared by widget drawing and owned replay.

use crate::Rectangle;

/// Allocation of the painted span along a rule's long axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillMode {
    /// Occupies the entire available span.
    Full,
    /// Centres a rounded percentage span; values at least 100 occupy the full span.
    Percent(f32),
    /// Insets both ends by the same logical-pixel distance.
    Padded(u16),
    /// Insets the leading and trailing ends independently, in logical pixels.
    AsymmetricPadding(u16, u16),
}

impl FillMode {
    /// Returns leading offset and painted length, preserving native percentage rounding.
    pub fn fill(&self, space: f32) -> (f32, f32) {
        match *self {
            Self::Full => (0.0, space),
            Self::Percent(percent) => {
                if percent >= 100.0 {
                    (0.0, space)
                } else {
                    let length = (space * percent / 100.0).round();
                    (((space - length) / 2.0).round(), length)
                }
            }
            Self::Padded(0) => (0.0, space),
            Self::Padded(padding) => {
                let padding = f32::from(padding);
                let mut length = space - padding * 2.0;
                if length < 0.0 {
                    length = 0.0;
                }
                (padding, length)
            }
            Self::AsymmetricPadding(first, second) => {
                let first = f32::from(first);
                let mut length = space - first - f32::from(second);
                if length < 0.0 {
                    length = 0.0;
                }
                (first, length)
            }
        }
    }
}

/// Immutable draw policy; hint scale comes from the renderer presenting each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    /// Selects height as the long axis instead of width.
    pub vertical: bool,
    /// Placement of the painted span within the allocation.
    pub fill: FillMode,
    /// Floors painted dimensions to one physical pixel before renderer snapping.
    pub snap: bool,
}

impl Paint {
    /// Computes the native paint rectangle; `hint_factor` is physical pixels per logical pixel.
    pub fn bounds(self, allocation: Rectangle, hint_factor: f32) -> Rectangle {
        let mut bounds = if self.vertical {
            let (offset, height) = self.fill.fill(allocation.height);
            Rectangle {
                y: allocation.y + offset,
                height,
                ..allocation
            }
        } else {
            let (offset, width) = self.fill.fill(allocation.width);
            Rectangle {
                x: allocation.x + offset,
                width,
                ..allocation
            }
        };
        if self.snap {
            let unit = 1.0 / hint_factor;
            bounds.width = bounds.width.max(unit);
            bounds.height = bounds.height.max(unit);
        }
        bounds
    }

    /// Whether the policy can be admitted into finite owned geometry.
    pub fn is_valid(self) -> bool {
        !matches!(self.fill, FillMode::Percent(value) if !value.is_finite() || value < 0.0)
    }
}
