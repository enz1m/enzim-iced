// Modified by enzUI contributors. See FORK.md.
use crate::alignment;
use crate::{Padding, Point, Radians, Size, Vector};

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rectangle<T = f32> {
    /// X coordinate of the top-left corner.
    pub x: T,

    /// Y coordinate of the top-left corner.
    pub y: T,

    /// Width of the rectangle.
    pub width: T,

    /// Height of the rectangle.
    pub height: T,
}

impl<T> Rectangle<T>
where
    T: Default + Copy,
{
    /// Creates a new [`Rectangle`] with its top-left corner at the origin
    /// and with the provided [`Size`].
    pub fn with_size(size: Size<T>) -> Self {
        Self {
            x: T::default(),
            y: T::default(),
            width: size.width,
            height: size.height,
        }
    }

    /// Returns the [`Size`] of the [`Rectangle`].
    pub const fn size(&self) -> Size<T> {
        Size::new(self.width, self.height)
    }
}

impl Rectangle<f32> {
    /// A rectangle starting at negative infinity and with infinite width and height.
    pub const INFINITE: Self = Self::new(
        Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        Size::INFINITE,
    );

    /// Creates a new [`Rectangle`] with its top-left corner in the given
    /// [`Point`] and with the provided [`Size`].
    pub const fn new(top_left: Point, size: Size) -> Self {
        Self {
            x: top_left.x,
            y: top_left.y,
            width: size.width,
            height: size.height,
        }
    }

    /// Creates a new square [`Rectangle`] with the center at the origin and
    /// with the given radius.
    pub fn with_radius(radius: f32) -> Self {
        Self {
            x: -radius,
            y: -radius,
            width: radius * 2.0,
            height: radius * 2.0,
        }
    }

    /// Creates a new axis-aligned [`Rectangle`] from the given vertices; returning the
    /// rotation in [`Radians`] that must be applied to the axis-aligned [`Rectangle`]
    /// to obtain the desired result.
    pub fn with_vertices(
        top_left: Point,
        top_right: Point,
        bottom_left: Point,
    ) -> (Rectangle, Radians) {
        let width = (top_right.x - top_left.x).hypot(top_right.y - top_left.y);

        let height = (bottom_left.x - top_left.x).hypot(bottom_left.y - top_left.y);

        let rotation = (top_right.y - top_left.y).atan2(top_right.x - top_left.x);

        let rotation = if rotation < 0.0 {
            2.0 * std::f32::consts::PI + rotation
        } else {
            rotation
        };

        let position = {
            let center = Point::new(
                (top_right.x + bottom_left.x) / 2.0,
                (top_right.y + bottom_left.y) / 2.0,
            );

            let rotation = -rotation - std::f32::consts::PI * 2.0;

            Point::new(
                center.x + (top_left.x - center.x) * rotation.cos()
                    - (top_left.y - center.y) * rotation.sin(),
                center.y
                    + (top_left.x - center.x) * rotation.sin()
                    + (top_left.y - center.y) * rotation.cos(),
            )
        };

        (
            Rectangle::new(position, Size::new(width, height)),
            Radians(rotation),
        )
    }

    /// Returns the [`Point`] at the center of the [`Rectangle`].
    pub fn center(&self) -> Point {
        Point::new(self.center_x(), self.center_y())
    }

    /// Returns the X coordinate of the [`Point`] at the center of the
    /// [`Rectangle`].
    pub fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    /// Returns the Y coordinate of the [`Point`] at the center of the
    /// [`Rectangle`].
    pub fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }

    /// Returns the position of the top left corner of the [`Rectangle`].
    pub fn position(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// Returns the area of the [`Rectangle`].
    pub fn area(&self) -> f32 {
        self.width * self.height
    }

    /// Returns true if the given [`Point`] is contained in the [`Rectangle`].
    /// Excludes the right and bottom edges.
    pub fn contains(&self, point: Point) -> bool {
        self.x <= point.x
            && point.x < self.x + self.width
            && self.y <= point.y
            && point.y < self.y + self.height
    }

    /// Returns the minimum distance from the given [`Point`] to any of the edges
    /// of the [`Rectangle`].
    pub fn distance(&self, point: Point) -> f32 {
        let center = self.center();

        let distance_x = ((point.x - center.x).abs() - self.width / 2.0).max(0.0);

        let distance_y = ((point.y - center.y).abs() - self.height / 2.0).max(0.0);

        distance_x.hypot(distance_y)
    }

    /// Computes the offset that must be applied to the [`Rectangle`] to be placed
    /// inside the given `container`.
    pub fn offset(&self, container: &Rectangle) -> Vector {
        let Some(intersection) = self.intersection(container) else {
            return Vector::ZERO;
        };

        let left = intersection.x - self.x;
        let top = intersection.y - self.y;

        Vector::new(
            if left > 0.0 {
                left
            } else {
                intersection.x + intersection.width - self.x - self.width
            },
            if top > 0.0 {
                top
            } else {
                intersection.y + intersection.height - self.y - self.height
            },
        )
    }

    /// Returns true if the current [`Rectangle`] is within the given
    /// `container`. Includes the right and bottom edges.
    pub fn is_within(&self, container: &Rectangle) -> bool {
        self.x >= container.x
            && self.y >= container.y
            && self.x + self.width <= container.x + container.width
            && self.y + self.height <= container.y + container.height
    }

    /// Computes the intersection with the given [`Rectangle`].
    pub fn intersection(&self, other: &Rectangle<f32>) -> Option<Rectangle<f32>> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);

        // An infinite extent reaches `+inf` whatever its origin; summing
        // `NEG_INFINITY + INFINITY` would be NaN, so `INFINITE` would not
        // intersect itself.
        let far = |origin: f32, extent: f32| {
            if extent == f32::INFINITY {
                f32::INFINITY
            } else {
                origin + extent
            }
        };
        let lower_right_x = far(self.x, self.width).min(far(other.x, other.width));
        let lower_right_y = far(self.y, self.height).min(far(other.y, other.height));

        let width = lower_right_x - x;
        let height = lower_right_y - y;

        if width > 0.0 && height > 0.0 {
            Some(Rectangle {
                x,
                y,
                width,
                height,
            })
        } else {
            None
        }
    }

    /// Returns whether the [`Rectangle`] intersects with the given one.
    pub fn intersects(&self, other: &Self) -> bool {
        self.intersection(other).is_some()
    }

    /// Computes the union with the given [`Rectangle`].
    pub fn union(&self, other: &Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);

        let lower_right_x = (self.x + self.width).max(other.x + other.width);
        let lower_right_y = (self.y + self.height).max(other.y + other.height);

        let width = lower_right_x - x;
        let height = lower_right_y - y;

        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    /// Rounds the [`Rectangle`] coordinates.
    pub fn round(self) -> Self {
        let top_left = self.position().round();
        let bottom_right = (self.position() + Vector::from(self.size())).round();

        Self {
            x: top_left.x,
            y: top_left.y,
            width: bottom_right.x - top_left.x,
            height: bottom_right.y - top_left.y,
        }
    }

    /// Snaps the [`Rectangle`] to __unsigned__ integer coordinates.
    pub fn snap(self) -> Option<Rectangle<u32>> {
        let rounded = self.round();

        if rounded.width < 1.0 || rounded.height < 1.0 {
            return None;
        }

        Some(Rectangle {
            x: rounded.x as u32,
            y: rounded.y as u32,
            width: rounded.width as u32,
            height: rounded.height as u32,
        })
    }

    /// Computes the logical [`Rectangle`] that ends up rounded
    /// after the given `scale_factor` is applied to it.
    pub fn hint(self, scale_factor: f32) -> Self {
        (self * scale_factor).round() / scale_factor
    }

    /// Expands the [`Rectangle`] a given amount.
    pub fn expand(self, padding: impl Into<Padding>) -> Self {
        let padding = padding.into();

        Self {
            x: self.x - padding.left,
            y: self.y - padding.top,
            width: self.width + padding.x(),
            height: self.height + padding.y(),
        }
    }

    /// Shrinks the [`Rectangle`] a given amount.
    pub fn shrink(self, padding: impl Into<Padding>) -> Self {
        let padding = padding.into();

        Self {
            x: self.x + padding.left,
            y: self.y + padding.top,
            width: self.width - padding.x(),
            height: self.height - padding.y(),
        }
    }

    /// Rotates the [`Rectangle`] and returns the smallest [`Rectangle`]
    /// containing it.
    pub fn rotate(self, rotation: Radians) -> Self {
        let size = self.size().rotate(rotation);
        let position = Point::new(
            self.center_x() - size.width / 2.0,
            self.center_y() - size.height / 2.0,
        );

        Self::new(position, size)
    }

    /// Scales the [`Rectangle`] without changing its position, effectively
    /// "zooming" it.
    pub fn zoom(self, zoom: f32) -> Self {
        Self {
            x: self.x - (self.width * (zoom - 1.0)) / 2.0,
            y: self.y - (self.height * (zoom - 1.0)) / 2.0,
            width: self.width * zoom,
            height: self.height * zoom,
        }
    }

    /// Returns the top-left position to render an object of the given [`Size`].
    /// inside the [`Rectangle`] that is anchored to the edge or corner
    /// defined by the alignment arguments.
    pub fn anchor(
        &self,
        size: Size,
        align_x: impl Into<alignment::Horizontal>,
        align_y: impl Into<alignment::Vertical>,
    ) -> Point {
        let x = match align_x.into() {
            alignment::Horizontal::Left => self.x,
            alignment::Horizontal::Center => self.x + (self.width - size.width) / 2.0,
            alignment::Horizontal::Right => self.x + self.width - size.width,
        };

        let y = match align_y.into() {
            alignment::Vertical::Top => self.y,
            alignment::Vertical::Center => self.y + (self.height - size.height) / 2.0,
            alignment::Vertical::Bottom => self.y + self.height - size.height,
        };

        Point::new(x, y)
    }

    /// Returns the two endpoints of a chord through the rectangle's center
    /// at the given `angle`.
    ///
    /// The angle is measured clockwise from the negative y-axis, so `0`
    /// produces a vertical chord and `π/2` a horizontal one. The returned
    /// points are `(start, end)`, symmetric about [`Rectangle::center`].
    pub fn chord(&self, angle: impl Into<Radians>) -> (Point, Point) {
        use std::f32::consts::FRAC_PI_2;

        let angle = angle.into().0 - FRAC_PI_2;
        let r = Vector::new(f32::cos(angle), f32::sin(angle));

        // enzICED #536.2: SUM of the projected half-axes, not `max`.
        //
        // `chord` returns the endpoints of the gradient line for direction
        // `r`. For an axis-aligned rectangle the distance from the centre to
        // the supporting plane in direction `r` is the support function
        //
        //     |r.x| * w/2 + |r.y| * h/2
        //
        // i.e. the *sum* of the two projected half-axes. `max` is correct
        // only when one term is zero -- an axis-aligned direction (0/90/180/
        // 270 degrees), where `max` and the sum coincide (exactly at 90
        // degrees; to within the trig rounding of the near-zero cos/sin at
        // the others). For any diagonal direction `max` under-spans: the
        // endpoints fall short of
        // the far corners, so renderers pad those corners with the endpoint
        // colours and a 45-degree gradient on a square flattens into solid
        // bands at two corners. The sum makes every corner project onto the
        // returned segment within [0, 1] at every angle.
        let distance_to_rect = f32::abs(r.x * self.width / 2.0) + f32::abs(r.y * self.height / 2.0);

        let center = self.center();
        let start = center - r * distance_to_rect;
        let end = center + r * distance_to_rect;

        (start, end)
    }
}

impl std::ops::Mul<f32> for Rectangle<f32> {
    type Output = Self;

    fn mul(self, scale: f32) -> Self {
        Self {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
        }
    }
}

impl std::ops::Div<f32> for Rectangle<f32> {
    type Output = Self;

    fn div(self, scale: f32) -> Self {
        Self {
            x: self.x / scale,
            y: self.y / scale,
            width: self.width / scale,
            height: self.height / scale,
        }
    }
}

impl From<Rectangle<u32>> for Rectangle<f32> {
    fn from(rectangle: Rectangle<u32>) -> Rectangle<f32> {
        Rectangle {
            x: rectangle.x as f32,
            y: rectangle.y as f32,
            width: rectangle.width as f32,
            height: rectangle.height as f32,
        }
    }
}

impl<T> From<Rectangle<T>> for Size<T> {
    fn from(rectangle: Rectangle<T>) -> Size<T> {
        Size {
            width: rectangle.width,
            height: rectangle.height,
        }
    }
}

impl<T> std::ops::Add<Vector<T>> for Rectangle<T>
where
    T: std::ops::Add<Output = T>,
{
    type Output = Rectangle<T>;

    fn add(self, translation: Vector<T>) -> Self {
        Rectangle {
            x: self.x + translation.x,
            y: self.y + translation.y,
            ..self
        }
    }
}

impl<T> std::ops::AddAssign<Vector<T>> for Rectangle<T>
where
    T: std::ops::AddAssign,
{
    fn add_assign(&mut self, translation: Vector<T>) {
        self.x += translation.x;
        self.y += translation.y;
    }
}

impl<T> std::ops::Sub<Vector<T>> for Rectangle<T>
where
    T: std::ops::Sub<Output = T>,
{
    type Output = Rectangle<T>;

    fn sub(self, translation: Vector<T>) -> Self {
        Rectangle {
            x: self.x - translation.x,
            y: self.y - translation.y,
            ..self
        }
    }
}

impl<T> std::ops::SubAssign<Vector<T>> for Rectangle<T>
where
    T: std::ops::SubAssign,
{
    fn sub_assign(&mut self, translation: Vector<T>) {
        self.x -= translation.x;
        self.y -= translation.y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn rect(w: f32, h: f32) -> Rectangle<f32> {
        // Centre at (10, 20) to exercise the non-origin case.
        Rectangle {
            x: 10.0 - w / 2.0,
            y: 20.0 - h / 2.0,
            width: w,
            height: h,
        }
    }

    /// The old implementation, kept verbatim so the axis-aligned agreement
    /// claim is a *test* and not a comment.
    ///
    /// In *exact* arithmetic, at 0/90/180/270 degrees one of the two
    /// projected half-axes is zero, so `max` and the sum coincide. In `f32`
    /// the trig calls do not return exact zeros: `cos(-PI/2)` is `-4.37e-8`,
    /// not `0`, so at 0/180/270 degrees the sum adds a sub-micron term the
    /// `max` dropped (measured maximum discrepancy over these four angles on
    /// an 80x40 rect: 3.8e-6 px). 90 degrees is the one angle where the
    /// zeroed term (`sin(0)`) is *exactly* zero, so there the two agree
    /// byte-for-byte. Both facts are asserted below.
    fn chord_max(bounds: &Rectangle<f32>, angle: f32) -> (Point, Point) {
        let a = angle - std::f32::consts::FRAC_PI_2;
        let r = Vector::new(f32::cos(a), f32::sin(a));
        let distance_to_rect = f32::max(
            f32::abs(r.x * bounds.width / 2.0),
            f32::abs(r.y * bounds.height / 2.0),
        );
        let center = bounds.center();
        (center - r * distance_to_rect, center + r * distance_to_rect)
    }

    #[test]
    fn axis_aligned_angles_agree_with_the_max_formula() {
        // Both a square and a non-square rectangle. At axis-aligned angles
        // one projected half-axis is zero *in exact arithmetic*, so `max`
        // and the sum coincide; in `f32` they differ only by the trig
        // rounding error, which must stay well under a pixel.
        for bounds in [rect(100.0, 100.0), rect(80.0, 40.0)] {
            for angle in [
                0.0,
                std::f32::consts::FRAC_PI_2,
                PI,
                3.0 * std::f32::consts::FRAC_PI_2,
            ] {
                let (s_new, e_new) = bounds.chord(Radians(angle));
                let (s_old, e_old) = chord_max(&bounds, angle);
                // Sub-pixel: the only difference is the |cos/sin| ~= 1e-7
                // residual times the half-axis, ~= 4e-6 px at most here.
                let tol = 1e-4;
                assert!(
                    (s_new.x - s_old.x).abs() < tol
                        && (s_new.y - s_old.y).abs() < tol
                        && (e_new.x - e_old.x).abs() < tol
                        && (e_new.y - e_old.y).abs() < tol,
                    "axis-aligned endpoints moved by more than {tol} at angle \
                     {angle}: new=({s_new:?},{e_new:?}) old=({s_old:?},{e_old:?})"
                );
            }
        }
    }

    #[test]
    fn ninety_degrees_is_byte_identical_to_the_max_formula() {
        // 90 degrees is the one axis-aligned angle whose zeroed term
        // (`sin(0)`) is *exactly* zero in f32, so `max` and the sum are
        // bit-for-bit identical. This pins the strongest form of the
        // "axis-aligned cases do not move" guarantee where it is achievable.
        for bounds in [rect(100.0, 100.0), rect(80.0, 40.0)] {
            let angle = std::f32::consts::FRAC_PI_2;
            let (s_new, e_new) = bounds.chord(Radians(angle));
            let (s_old, e_old) = chord_max(&bounds, angle);
            assert_eq!(s_new.x.to_bits(), s_old.x.to_bits(), "start.x moved");
            assert_eq!(s_new.y.to_bits(), s_old.y.to_bits(), "start.y moved");
            assert_eq!(e_new.x.to_bits(), e_old.x.to_bits(), "end.x moved");
            assert_eq!(e_new.y.to_bits(), e_old.y.to_bits(), "end.y moved");
        }
    }

    /// Project point `p` onto the segment `(start, end)` and return the
    /// parameter t such that p_proj = start + t * (end - start). For every
    /// corner of the rectangle t must lie in [0, 1], i.e. the corner's
    /// gradient coordinate is between the two endpoint colours -- no corner
    /// is left outside the span to be padded with an endpoint colour.
    fn project_t(start: Point, end: Point, p: Point) -> f32 {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let len2 = dx * dx + dy * dy;
        ((p.x - start.x) * dx + (p.y - start.y) * dy) / len2
    }

    #[test]
    fn every_corner_projects_within_the_segment_at_all_angles() {
        // The property that "remove the class" demands: at *every* angle,
        // every corner projects into [0, 1]. `max` fails this for diagonals.
        for bounds in [rect(100.0, 100.0), rect(120.0, 40.0), rect(40.0, 120.0)] {
            let corners = [
                Point::new(bounds.x, bounds.y),
                Point::new(bounds.x + bounds.width, bounds.y),
                Point::new(bounds.x, bounds.y + bounds.height),
                Point::new(bounds.x + bounds.width, bounds.y + bounds.height),
            ];
            // Sweep a full turn in 5-degree steps.
            for step in 0..72 {
                let angle = step as f32 * (PI / 36.0);
                let (start, end) = bounds.chord(Radians(angle));
                for corner in corners {
                    let t = project_t(start, end, corner);
                    assert!(
                        (-1e-4..=1.0 + 1e-4).contains(&t),
                        "corner {corner:?} projects to t={t} (out of [0,1]) at angle {angle} for {bounds:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn diagonal_gradient_spans_the_full_square() {
        // 45 degrees on a square: the endpoints must reach the opposite
        // corners. With `max` the half-length is max(|rx|*50,|ry|*50) =
        // 0.707*50 ~= 35.4, so the chord stops short and the far corners
        // project to t > 1. The sum formula gives |rx|*50 + |ry|*50 =
        // 0.707*50 + 0.707*50 ~= 70.71 half-length -- exactly the
        // centre-to-corner distance -- so the two far corners sit at
        // t = 0 and t = 1.
        let bounds = rect(100.0, 100.0);
        let angle = std::f32::consts::FRAC_PI_4; // 45 degrees
        let (start, end) = bounds.chord(Radians(angle));

        // The far corners of a 45-degree chord are top-left and
        // bottom-right (or the other pair depending on sign); at least two
        // corners must land at the endpoints (t ~= 0 and t ~= 1).
        let corners = [
            Point::new(bounds.x, bounds.y),
            Point::new(bounds.x + bounds.width, bounds.y),
            Point::new(bounds.x, bounds.y + bounds.height),
            Point::new(bounds.x + bounds.width, bounds.y + bounds.height),
        ];
        let ts: Vec<f32> = corners.iter().map(|c| project_t(start, end, *c)).collect();
        let min_t = ts.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_t = ts.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            min_t <= 1e-4,
            "no corner reaches the start endpoint: min t = {min_t}, ts = {ts:?}"
        );
        assert!(
            (max_t - 1.0).abs() <= 1e-4,
            "no corner reaches the end endpoint: max t = {max_t}, ts = {ts:?}"
        );

        // The chord spans corner to corner: full length sqrt(2)*100.
        let len = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
        let expected = std::f32::consts::SQRT_2 * 100.0;
        assert!(
            (len - expected).abs() <= 1e-3,
            "diagonal chord length = {len}, expected {expected}"
        );
    }

    #[test]
    fn zero_dimension_does_not_produce_nan() {
        // Zero width/height is expressible intent (a degenerate rectangle)
        // and must degrade cleanly, never to NaN.
        for bounds in [rect(0.0, 100.0), rect(100.0, 0.0), rect(0.0, 0.0)] {
            for step in 0..8 {
                let angle = step as f32 * (PI / 4.0);
                let (start, end) = bounds.chord(Radians(angle));
                assert!(
                    start.x.is_finite()
                        && start.y.is_finite()
                        && end.x.is_finite()
                        && end.y.is_finite(),
                    "non-finite endpoint at angle {angle} for {bounds:?}: {start:?} {end:?}"
                );
            }
        }
    }

    #[test]
    fn infinite_bounds_intersect_as_the_other_operand() {
        let infinite = Rectangle::INFINITE;
        assert_eq!(infinite.intersection(&infinite), Some(infinite));

        let finite = Rectangle::new(Point::new(-12.0, 30.0), Size::new(50.0, 40.0));
        assert_eq!(infinite.intersection(&finite), Some(finite));
        assert_eq!(finite.intersection(&infinite), Some(finite));

        let travelled = infinite + Vector::new(-12.0, 30.0);
        assert_eq!(travelled.intersection(&infinite), Some(infinite));

        let apart = Rectangle::new(Point::new(100.0, 100.0), Size::new(10.0, 10.0));
        assert_eq!(finite.intersection(&apart), None);
    }
}
