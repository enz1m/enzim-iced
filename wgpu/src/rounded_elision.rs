// Modified by enzUI contributors. See FORK.md.
//! Rounded clip groups whose mask would change nothing (enzUI-ICED #3406).
//!
//! A rounded clip opens an offscreen group so that its content receives edge
//! coverage once, after it is composited (#3388). That costs a target, a clear
//! and a composite pass per clip, and a grid of rounded tiles multiplies them.
//! Quads and images already apply a layer's rounded clip themselves when no
//! group owns it, so a group is only needed where that per-primitive path
//! differs from one mask: where text or a mesh (clipped by rectangle) or two
//! overlapping primitives reach a corner, or where the content needs
//! compositing of its own. [`is_redundant`] proves the other cases and the group is dropped: a
//! row's label sitting clear of its arcs takes no group.

use crate::core::Rectangle;
use crate::core::border::Radius;
use crate::layer::{AlphaGroup, Layer};

/// How far inside a corner arc content may reach before the mask's
/// antialiasing could touch it, in logical pixels.
const ARC_MARGIN: f32 = 0.75;

/// Whether the rounded clip `group` changes no pixel of `layers`, the layers
/// painted inside it, compared with clipping each primitive by itself.
pub(crate) fn is_redundant(layers: &[Layer], group: &AlphaGroup) -> bool {
    let Some((clip, radius)) = group.clip else {
        return false;
    };
    if group.alpha < 1.0 || group.scale != 1.0 {
        return false;
    }

    let corners = Corners::of(clip, radius);
    let mut intruders = [0_u8; 4];

    for layer in layers {
        let innermost = layer.alpha_groups.last().map(|inner| inner.id);
        if innermost != Some(group.id)
            || !layer.materials.is_empty()
            || !layer.primitives.is_empty()
            || layer.has_pending()
        {
            return false;
        }

        // `rect` painted, except in the corners `clear` marks: a shape that
        // stays clear of the clip's arc there.
        let mut paint = |rect: Rectangle, clear: [bool; 4]| -> bool {
            let Some(visible) = rect.intersection(&layer.bounds) else {
                return true;
            };
            for (corner, count) in intruders.iter_mut().enumerate() {
                if !clear[corner] && corners.intrudes(corner, visible) {
                    *count += 1;
                    if *count > 1 {
                        return false;
                    }
                }
            }
            true
        };

        // Meshes and text are clipped by rectangle, so only those clear of
        // every arc may go without the mask.
        for item in &layer.triangles {
            let reach = match item {
                crate::triangle::Item::Group {
                    transformation,
                    meshes,
                } => meshes
                    .iter()
                    .map(|mesh| mesh.clip_bounds() * *transformation)
                    .reduce(|a, b| a.union(&b)),
                crate::triangle::Item::Cached {
                    transformation,
                    cache,
                } => cache
                    .batch()
                    .iter()
                    .map(|mesh| mesh.clip_bounds() * *transformation)
                    .reduce(|a, b| a.union(&b)),
            };
            let Some(visible) = reach.and_then(|reach| reach.intersection(&layer.bounds)) else {
                continue;
            };
            if (0..4).any(|corner| corners.intrudes(corner, visible)) {
                return false;
            }
        }
        // Text is clipped by rectangle, so only text clear of every arc may
        // go without the mask.
        for item in &layer.text {
            let crate::text::Item::Group {
                transformation,
                text,
            } = item
            else {
                return false;
            };
            for section in text {
                let reach = crate::layer::text_glyph_reach(section) * *transformation;
                let Some(visible) = reach.intersection(&layer.bounds) else {
                    continue;
                };
                if (0..4).any(|corner| corners.intrudes(corner, visible)) {
                    return false;
                }
            }
        }
        for quad in layer.quads.geometry() {
            let shadowed = quad.shadow_color.components()[3] > 0.0
                && (quad.shadow_blur_radius > 0.0 || quad.shadow_offset != [0.0, 0.0]);
            let rect = Rectangle::new(
                crate::core::Point::new(quad.position[0], quad.position[1]),
                crate::core::Size::new(quad.size[0], quad.size[1]),
            );
            let clear = std::array::from_fn(|corner| {
                corners.rounded_clear(corner, rect, quad.border_radius[corner])
            });
            if shadowed || !paint(rect, clear) {
                return false;
            }
        }

        #[cfg(any(feature = "image", feature = "svg"))]
        for image in &layer.images {
            let rect = match image {
                #[cfg(feature = "image")]
                crate::image::Image::Raster {
                    image,
                    bounds,
                    clip_bounds,
                } => {
                    if f32::from(image.rotation) != 0.0 {
                        return false;
                    }
                    bounds.intersection(clip_bounds)
                }
                #[cfg(feature = "svg")]
                crate::image::Image::Vector {
                    svg,
                    bounds,
                    clip_bounds,
                } => {
                    if f32::from(svg.rotation) != 0.0 {
                        return false;
                    }
                    bounds.intersection(clip_bounds)
                }
                #[allow(unreachable_patterns)]
                _ => return false,
            };
            if rect.is_some_and(|rect| !paint(rect, [false; 4])) {
                return false;
            }
        }
    }

    true
}

/// The four masked corner areas of a rounded rectangle: top-left, top-right,
/// bottom-right and bottom-left.
struct Corners {
    clip: Rectangle,
    radius: [f32; 4],
}

impl Corners {
    fn of(clip: Rectangle, radius: Radius) -> Self {
        Self {
            clip,
            radius: [
                radius.top_left,
                radius.top_right,
                radius.bottom_right,
                radius.bottom_left,
            ],
        }
    }

    /// Whether a rectangle `rect` inside the clip, rounded by `radius` at
    /// `corner`, stays clear of the clip's arc there by the mask's
    /// antialiasing: a card rounded more than the clip it sits in.
    ///
    /// Two arcs tangent to the same edges are closest where the clip's arc
    /// meets its edges, `radius - sqrt(radius² - (radius - r)²)` apart for a
    /// rectangle sharing the clip's corner (an inset one is further in).
    fn rounded_clear(&self, corner: usize, rect: Rectangle, radius: f32) -> bool {
        let r = self.radius[corner].max(0.0);
        let clip = self.clip;
        let inside = clip.x <= rect.x
            && clip.y <= rect.y
            && rect.x + rect.width <= clip.x + clip.width
            && rect.y + rect.height <= clip.y + clip.height;
        // The shader limits a radius to half the shorter side.
        let radius = radius.min(rect.width / 2.0).min(rect.height / 2.0);
        if !inside || radius <= r {
            return false;
        }
        radius - (radius * radius - (radius - r) * (radius - r)).sqrt() >= ARC_MARGIN
    }

    /// Whether `rect` reaches outside the arc of `corner`, where the mask
    /// removes or softens coverage.
    fn intrudes(&self, corner: usize, rect: Rectangle) -> bool {
        let r = self.radius[corner].max(0.0);
        if r <= 0.0 {
            return false;
        }
        let clip = self.clip;
        let (left, right) = (clip.x, clip.x + clip.width);
        let (top, bottom) = (clip.y, clip.y + clip.height);
        // The arc's centre, and the point of `rect` nearest the corner.
        let (centre, nearest) = match corner {
            0 => ((left + r, top + r), (rect.x, rect.y)),
            1 => ((right - r, top + r), (rect.x + rect.width, rect.y)),
            2 => (
                (right - r, bottom - r),
                (rect.x + rect.width, rect.y + rect.height),
            ),
            _ => ((left + r, bottom - r), (rect.x, rect.y + rect.height)),
        };
        let toward_x = if corner == 0 || corner == 3 {
            nearest.0 < centre.0
        } else {
            nearest.0 > centre.0
        };
        let toward_y = if corner < 2 {
            nearest.1 < centre.1
        } else {
            nearest.1 > centre.1
        };
        if !toward_x || !toward_y {
            return false;
        }
        let distance = ((nearest.0 - centre.0).powi(2) + (nearest.1 - centre.1).powi(2)).sqrt();
        distance > r - ARC_MARGIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Point, Size};

    fn corners() -> Corners {
        Corners::of(
            Rectangle::new(Point::new(10.0, 10.0), Size::new(100.0, 60.0)),
            Radius::new(12.0),
        )
    }

    #[test]
    fn content_inside_the_arcs_does_not_intrude() {
        let corners = corners();
        let inset = Rectangle::new(Point::new(16.0, 16.0), Size::new(88.0, 48.0));
        assert!((0..4).all(|corner| !corners.intrudes(corner, inset)));
    }

    #[test]
    fn content_filling_the_rectangle_intrudes_every_corner() {
        let corners = corners();
        let full = Rectangle::new(Point::new(10.0, 10.0), Size::new(100.0, 60.0));
        assert!((0..4).all(|corner| corners.intrudes(corner, full)));
    }

    #[test]
    fn a_bar_along_the_bottom_edge_intrudes_only_the_bottom_corners() {
        let corners = corners();
        let bar = Rectangle::new(Point::new(10.0, 64.0), Size::new(100.0, 6.0));
        assert_eq!(
            (0..4)
                .map(|corner| corners.intrudes(corner, bar))
                .collect::<Vec<_>>(),
            [false, false, true, true]
        );
    }
}
