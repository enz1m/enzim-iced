// Modified by enzUI contributors. See FORK.md.
//! Track mouse clicks.
use crate::mouse::Button;
use crate::time::Instant;
use crate::{Point, Transformation};

use std::ops::Mul;

/// A mouse click.
#[derive(Debug, Clone, Copy)]
pub struct Click {
    kind: Kind,
    button: Button,
    position: Point,
    time: Instant,
}

/// The kind of mouse click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A single click
    Single,

    /// A double click
    Double,

    /// A triple click
    Triple,
}

impl Kind {
    fn next(self) -> Kind {
        match self {
            Kind::Single => Kind::Double,
            Kind::Double => Kind::Triple,
            Kind::Triple => Kind::Double,
        }
    }
}

impl Click {
    /// Creates a new [`Click`] with the given position and previous last
    /// [`Click`].
    pub fn new(position: Point, button: Button, previous: Option<Click>) -> Click {
        let time = Instant::now();

        let kind = if let Some(previous) = previous {
            if previous.is_consecutive(position, time) && button == previous.button {
                previous.kind.next()
            } else {
                Kind::Single
            }
        } else {
            Kind::Single
        };

        Click {
            kind,
            button,
            position,
            time,
        }
    }

    /// Returns the [`Kind`] of [`Click`].
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Returns the position of the [`Click`].
    pub fn position(&self) -> Point {
        self.position
    }

    fn is_consecutive(&self, new_position: Point, time: Instant) -> bool {
        let duration = if time > self.time {
            Some(time - self.time)
        } else {
            None
        };

        let preferences = crate::preferences::get();
        self.position.distance(new_position) < preferences.double_click_distance
            && duration.is_some_and(|duration| duration <= preferences.double_click_time)
    }
}

impl Mul<Transformation> for Click {
    type Output = Click;

    fn mul(self, transformation: Transformation) -> Click {
        Click {
            kind: self.kind,
            button: self.button,
            position: self.position * transformation,
            time: self.time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::{self, Preferences};
    use std::time::Duration;

    #[test]
    fn a_double_click_follows_the_desktop_time_and_distance() {
        let first = Click {
            kind: Kind::Single,
            button: Button::Left,
            position: Point::ORIGIN,
            time: Instant::now(),
        };
        let after = |millis| first.time + Duration::from_millis(millis);
        assert!(first.is_consecutive(Point::new(3.0, 0.0), after(350)));
        assert!(!first.is_consecutive(Point::ORIGIN, after(450)));

        let slow = Preferences {
            double_click_time: Duration::from_millis(700),
            double_click_distance: 2.0,
            ..Preferences::DEFAULT
        };
        preferences::scoped(slow, || {
            assert!(first.is_consecutive(Point::ORIGIN, after(650)));
            assert!(!first.is_consecutive(Point::new(3.0, 0.0), after(100)));
        });
    }
}
