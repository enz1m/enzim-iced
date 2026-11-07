// Modified by enzUI contributors. See FORK.md.
use crate::{Point, Size};

/// A display connected to the system (enzICED); see `window::monitors`.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    /// A human-readable name, such as the connector (`DP-1`) or model.
    pub name: Option<String>,
    /// The top-left corner in the desktop's layout, in physical pixels.
    /// Wayland does not reveal it and reports the origin.
    pub position: Point<i32>,
    /// The resolution in physical pixels.
    pub size: Size<u32>,
    /// The ratio of physical to logical pixels.
    pub scale_factor: f64,
    /// The refresh rate in millihertz (`60_000` for 60 Hz), when known.
    pub refresh_rate_millihertz: Option<u32>,
}

impl Monitor {
    /// The size in logical pixels.
    pub fn logical_size(&self) -> Size {
        let scale = if self.scale_factor > 0.0 {
            self.scale_factor
        } else {
            1.0
        };
        Size::new(
            (f64::from(self.size.width) / scale) as f32,
            (f64::from(self.size.height) / scale) as f32,
        )
    }

    /// The refresh rate in hertz, when known.
    pub fn refresh_rate(&self) -> Option<f32> {
        self.refresh_rate_millihertz
            .map(|millihertz| millihertz as f32 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_size_divides_by_the_scale() {
        let monitor = Monitor {
            name: Some("DP-1".into()),
            position: Point::new(0, 0),
            size: Size::new(2560, 1440),
            scale_factor: 1.25,
            refresh_rate_millihertz: Some(143_912),
        };
        assert_eq!(monitor.logical_size(), Size::new(2048.0, 1152.0));
        assert_eq!(monitor.refresh_rate(), Some(143.912));
    }
}
