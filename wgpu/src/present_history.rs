// Modified by enzUI contributors. See FORK.md.

use crate::core::{Rectangle, Size};
use std::collections::VecDeque;

const HISTORY_LIMIT: usize = 8;

#[derive(Clone, Copy)]
struct Frame {
    generation: u64,
    serial: u64,
    previous_serial: Option<u64>,
}

impl Frame {
    #[cfg_attr(
        all(feature = "vulkan", target_os = "linux"),
        expect(unsafe_code, reason = "reads the native acquisition stamp")
    )]
    fn from_texture(_target: &wgpu::Texture) -> Option<Self> {
        #[cfg(all(feature = "vulkan", target_os = "linux"))]
        {
            // SAFETY: the texture remains alive and the HAL guard is used only to read metadata.
            let raw = unsafe { _target.as_hal::<wgpu::hal::api::Vulkan>() }?;
            let stamp = raw.preserved_surface_frame()?;
            Some(Self {
                generation: stamp.generation,
                serial: stamp.serial,
                previous_serial: stamp.previous_serial,
            })
        }
        #[cfg(not(all(feature = "vulkan", target_os = "linux")))]
        None
    }
}

#[derive(Debug, Default)]
pub(crate) struct History {
    last: Option<(u64, u64)>,
    damage: VecDeque<Option<Rectangle<u32>>>,
}

impl History {
    pub(crate) fn copy_region(
        &mut self,
        target: &wgpu::Texture,
        size: Size<u32>,
        damage: Option<&[[u32; 4]]>,
    ) -> (Rectangle<u32>, bool) {
        let frame = Frame::from_texture(target);
        let can_hint =
            frame.is_none_or(|frame| frame.previous_serial.is_some() && self.is_contiguous(frame));
        let damage = frame.and(damage).map(|rects| {
            rects
                .iter()
                .fold(Rectangle::default(), |area, &[x, y, width, height]| {
                    union(
                        area,
                        Rectangle {
                            x,
                            y,
                            width,
                            height,
                        },
                    )
                })
        });
        let region = self
            .record(frame, damage)
            // An unchanged image still submits a copy to wait on the acquire semaphore.
            .map(|area| {
                if area.width == 0 {
                    Rectangle {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    }
                } else {
                    area
                }
            })
            .unwrap_or(Rectangle::with_size(size));
        (region, can_hint)
    }

    fn is_contiguous(&self, frame: Frame) -> bool {
        self.last == Some((frame.generation, frame.serial - 1))
    }

    fn record(
        &mut self,
        frame: Option<Frame>,
        damage: Option<Rectangle<u32>>,
    ) -> Option<Rectangle<u32>> {
        let Some(frame) = frame else {
            self.last = None;
            self.damage.clear();
            return None;
        };
        let contiguous = self.is_contiguous(frame);
        if !contiguous {
            self.damage.clear();
        }

        let repair = frame.previous_serial.and_then(|previous| {
            let newer =
                usize::try_from(frame.serial.checked_sub(previous)?.checked_sub(1)?).ok()?;
            if !contiguous || newer > self.damage.len() {
                return None;
            }
            self.damage
                .iter()
                .rev()
                .take(newer)
                .try_fold(damage?, |area, changed| Some(union(area, (*changed)?)))
        });
        self.last = Some((frame.generation, frame.serial));
        if self.damage.len() == HISTORY_LIMIT {
            let _ = self.damage.pop_front();
        }
        self.damage
            .push_back(if contiguous { damage } else { None });
        repair
    }
}

fn union(a: Rectangle<u32>, b: Rectangle<u32>) -> Rectangle<u32> {
    if a.width == 0 {
        return b;
    }
    if b.width == 0 {
        return a;
    }
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rectangle {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

#[cfg(all(feature = "vulkan", target_os = "linux"))]
pub(crate) fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| {
        std::env::var_os("ICED_DISABLE_PARTIAL_PRESENT").as_deref()
            == Some(std::ffi::OsStr::new("1"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(generation: u64, serial: u64, previous: u64) -> Option<Frame> {
        Some(Frame {
            generation,
            serial,
            previous_serial: (previous > 0).then_some(previous),
        })
    }

    fn rect(x: u32, width: u32) -> Option<Rectangle<u32>> {
        Some(Rectangle {
            x,
            y: 3,
            width,
            height: 8,
        })
    }

    #[test]
    fn repairs_damage_since_this_image_was_presented() {
        let mut history = History::default();
        assert_eq!(history.record(frame(1, 1, 0), None), None);
        assert_eq!(history.record(frame(1, 2, 0), rect(4, 2)), None);
        assert_eq!(history.record(frame(1, 3, 1), rect(10, 2)), rect(4, 8));
        assert_eq!(history.record(frame(1, 4, 3), rect(9, 1)), rect(9, 1));
    }

    #[test]
    fn full_damage_requires_a_full_copy_until_the_image_catches_up() {
        let mut history = History::default();
        assert_eq!(history.record(frame(1, 1, 0), None), None);
        assert_eq!(history.record(frame(1, 2, 1), None), None);
        assert_eq!(history.record(frame(1, 3, 1), rect(2, 2)), None);
        assert_eq!(history.record(frame(1, 4, 2), rect(4, 2)), rect(2, 4));
    }

    #[test]
    fn generation_change_invalidates_matching_serials() {
        let mut history = History::default();
        assert_eq!(history.record(frame(1, 1, 0), None), None);
        assert_eq!(history.record(frame(1, 2, 1), rect(4, 2)), rect(4, 2));
        assert_eq!(history.record(frame(2, 3, 2), rect(4, 2)), None);
        assert_eq!(history.record(frame(2, 4, 3), rect(4, 2)), rect(4, 2));
    }

    #[test]
    fn interleaved_renderers_require_full_copies_across_the_handoff() {
        let mut ui = History::default();
        let mut lane = History::default();
        assert_eq!(ui.record(frame(1, 1, 0), None), None);
        assert_eq!(ui.record(frame(1, 2, 1), rect(4, 2)), rect(4, 2));
        assert_eq!(lane.record(frame(1, 3, 2), rect(8, 2)), None);
        assert_eq!(ui.record(frame(1, 4, 2), rect(4, 2)), None);
        assert_eq!(ui.record(frame(1, 5, 3), rect(4, 2)), None);
        assert_eq!(ui.record(frame(1, 6, 4), rect(4, 2)), rect(4, 2));
    }

    #[test]
    fn repair_requires_damage_for_every_frame_since_the_previous_present() {
        let mut history = History::default();
        assert_eq!(history.record(frame(1, 1, 0), None), None);
        for serial in 2..=10 {
            assert_eq!(
                history.record(frame(1, serial, serial - 1), rect(4, 2)),
                rect(4, 2)
            );
        }
        assert_eq!(history.record(frame(1, 11, 2), rect(4, 2)), rect(4, 2));
        assert_eq!(history.record(frame(1, 12, 2), rect(4, 2)), None);
        assert_eq!(history.record(None, rect(4, 2)), None);
        assert_eq!(history.record(frame(1, 13, 12), rect(4, 2)), None);
    }
}
