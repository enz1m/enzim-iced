// Modified by enzUI contributors. See FORK.md.
//! enzICED: the rounded clips around the widget being visited.
//!
//! A widget that masks its children to a rounded rectangle (a window drawn
//! with rounded corners, a rounded pane) publishes that shape while its
//! children handle events. A child that reaches into a corner of its own
//! accord, like a scrollbar rail running along a pane's edge, measures the
//! shapes around it there and keeps clear of their arcs instead of being cut
//! by them.

use crate::Rectangle;
use crate::border::Radius;
use std::cell::RefCell;

thread_local! {
    static SHAPES: RefCell<Vec<(Rectangle, Radius)>> = const { RefCell::new(Vec::new()) };
}

/// Runs `visit` with `bounds`, rounded by `radius`, among the shapes around
/// it.
pub fn within<R>(bounds: Rectangle, radius: Radius, visit: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            SHAPES.with(|shapes| {
                let _ = shapes.borrow_mut().pop();
            });
        }
    }
    SHAPES.with(|shapes| shapes.borrow_mut().push((bounds, radius)));
    let _pop = Pop;
    visit()
}

/// How far the gap between a rail and an arc it would touch is kept.
const GAP: f32 = 2.0;

/// How far the two ends of a straight rail must move in to clear the corners
/// of every shape around it: a vertical rail spans `cross` horizontally and
/// runs over `along` vertically; a horizontal one the other way round.
/// Returns the leading and trailing distances, each zero when the end is
/// clear.
pub fn rail_ends(vertical: bool, cross: (f32, f32), along: (f32, f32)) -> (f32, f32) {
    SHAPES.with(|shapes| {
        shapes
            .borrow()
            .iter()
            .map(|(bounds, radius)| ends_within(*bounds, *radius, vertical, cross, along))
            .fold((0.0_f32, 0.0_f32), |a, b| (a.0.max(b.0), a.1.max(b.1)))
    })
}

/// [`rail_ends`] for one shape.
fn ends_within(
    bounds: Rectangle,
    radius: Radius,
    vertical: bool,
    cross: (f32, f32),
    along: (f32, f32),
) -> (f32, f32) {
    // In the rail's own axes: `near`/`far` are the shape's edges along it,
    // `low`/`high` across it, and each end has a corner at either side.
    let (near, far, low, high) = if vertical {
        (
            bounds.y,
            bounds.y + bounds.height,
            bounds.x,
            bounds.x + bounds.width,
        )
    } else {
        (
            bounds.x,
            bounds.x + bounds.width,
            bounds.y,
            bounds.y + bounds.height,
        )
    };
    let (lead_low, lead_high, trail_low, trail_high) = if vertical {
        (
            radius.top_left,
            radius.top_right,
            radius.bottom_left,
            radius.bottom_right,
        )
    } else {
        (
            radius.top_left,
            radius.bottom_left,
            radius.top_right,
            radius.bottom_right,
        )
    };
    // How far in from the shape's edge along the rail an arc of radius `r`
    // reaches, `into` the corner's square across the rail.
    let reach = |r: f32, into: f32| -> f32 {
        if !(r.is_finite() && r > 0.0 && into > 0.0) {
            return 0.0;
        }
        let into = into.min(r);
        r - (r * r - into * into).sqrt() + GAP
    };
    let at_end = |r_low: f32, r_high: f32| {
        reach(r_low, (low + r_low) - cross.0).max(reach(r_high, cross.1 - (high - r_high)))
    };
    let lead_edge = near + at_end(lead_low, lead_high);
    let trail_edge = far - at_end(trail_low, trail_high);
    (
        (lead_edge - along.0).max(0.0),
        (along.1 - trail_edge).max(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Rectangle {
        Rectangle::new(crate::Point::ORIGIN, crate::Size::new(600.0, 500.0))
    }

    /// A rail along a rounded edge moves in at both ends until it clears the
    /// arcs; one away from the corners, or under square ones, does not.
    ///
    /// Sabotage: return `(0.0, 0.0)` from `ends_within`. The rail by the
    /// rounded edge keeps its ends.
    #[test]
    fn a_rail_by_a_rounded_edge_clears_its_corners() {
        let rounded = Radius::from(14.0);
        let (lead, trail) = within(window(), rounded, || {
            rail_ends(true, (590.0, 596.0), (0.0, 500.0))
        });
        // The arc at x = 596 is 4 px in from the side: it reaches
        // 14 - sqrt(14² - 10²) ≈ 4.2 px down from the top.
        assert!((lead - (4.2 + GAP)).abs() < 0.1, "{lead}");
        assert!((trail - lead).abs() < 0.01, "{trail}");

        let inside = within(window(), rounded, || {
            rail_ends(true, (500.0, 506.0), (0.0, 500.0))
        });
        assert_eq!(inside, (0.0, 0.0), "away from the corners");
        let square = within(window(), Radius::from(0.0), || {
            rail_ends(true, (590.0, 596.0), (0.0, 500.0))
        });
        assert_eq!(square, (0.0, 0.0), "square corners");
        let horizontal = within(window(), rounded, || {
            rail_ends(false, (490.0, 496.0), (0.0, 600.0))
        });
        assert!(horizontal.0 > 4.0 && horizontal.1 > 4.0, "{horizontal:?}");
        assert_eq!(
            rail_ends(true, (590.0, 596.0), (0.0, 500.0)),
            (0.0, 0.0),
            "no shape"
        );
    }
}
