// Modified by enzUI contributors. See FORK.md.
//! Input preferences the desktop sets for every toolkit (enzICED).
//!
//! Double-click time and distance, drag threshold, caret blinking, caret
//! width, and glyph hinting and antialiasing are user settings on Linux
//! desktops (GNOME's
//! `org.gnome.desktop.peripherals.mouse` and `org.gnome.desktop.interface`,
//! KDE's `kdeglobals`). One process-wide [`Preferences`] holds them; widgets
//! read [`get`] when they need a value, and the application (in enzUI, its
//! desktop-settings reader) calls [`set`] when the desktop reports one.
//! [`scoped`] overrides them on one thread, for tests.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{PoisonError, RwLock, RwLockWriteGuard};
use std::time::Duration;

/// The input preferences widgets follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preferences {
    /// The longest time between two presses of a double click (inclusive).
    pub double_click_time: Duration,
    /// How far the pointer may move between two presses of a double click,
    /// in logical pixels (exclusive).
    pub double_click_distance: f32,
    /// How far the pointer moves with a button held before a drag starts,
    /// in logical pixels.
    pub drag_threshold: f32,
    /// Whether the text caret blinks.
    pub cursor_blink: bool,
    /// One full blink cycle: the caret shows for half and hides for half.
    pub cursor_blink_time: Duration,
    /// How long after the last caret activity the caret stops blinking and
    /// stays visible.
    pub cursor_blink_timeout: Duration,
    /// The text caret's width, in logical pixels.
    pub caret_width: f32,
    /// Whether the user asked for reduced motion; the caret does not blink.
    pub reduced_motion: bool,
    /// Whether glyphs are hinted, when the desktop says; the renderer asks
    /// fontconfig otherwise.
    pub text_hinting: Option<bool>,
    /// Whether glyphs are antialiased, when the desktop says; the renderer
    /// asks fontconfig otherwise. Subpixel (RGB) antialiasing is drawn as
    /// grayscale, as GTK 4 does.
    pub text_antialias: Option<bool>,
}

impl Preferences {
    /// The values used where the desktop sets none.
    pub const DEFAULT: Self = Self {
        double_click_time: Duration::from_millis(400),
        double_click_distance: 6.0,
        drag_threshold: 4.0,
        cursor_blink: true,
        cursor_blink_time: Duration::from_millis(1000),
        cursor_blink_timeout: Duration::from_secs(10),
        caret_width: 1.0,
        reduced_motion: false,
        text_hinting: None,
        text_antialias: None,
    };

    /// Whether the caret blinks: blinking is on, its cycle is not zero, and
    /// the user did not ask for reduced motion.
    pub fn caret_blinks(&self) -> bool {
        self.cursor_blink
            && !self.reduced_motion
            && self.cursor_blink_time >= Duration::from_millis(2)
    }

    /// How long the caret stays shown, or hidden, in one blink phase.
    pub fn blink_phase(&self) -> Duration {
        self.cursor_blink_time / 2
    }

    /// Whether a caret that last moved `elapsed` ago is solid: it does not
    /// blink, or its blink timeout has passed.
    pub fn caret_settled(&self, elapsed: Duration) -> bool {
        !self.caret_blinks() || elapsed >= self.cursor_blink_timeout
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self::DEFAULT
    }
}

static CURRENT: RwLock<Preferences> = RwLock::new(Preferences::DEFAULT);
static TEXT_RASTERIZATION: AtomicU8 =
    AtomicU8::new(encode_text_rasterization(&Preferences::DEFAULT));

const fn encode_text_rasterization(preferences: &Preferences) -> u8 {
    const fn encode(value: Option<bool>) -> u8 {
        match value {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        }
    }

    encode(preferences.text_hinting) | (encode(preferences.text_antialias) << 2)
}

thread_local! {
    static SCOPED: RefCell<Option<Preferences>> = const { RefCell::new(None) };
}

/// The preferences in force on this thread.
pub fn get() -> Preferences {
    SCOPED
        .with(|scoped| *scoped.borrow())
        .unwrap_or_else(|| *CURRENT.read().unwrap_or_else(PoisonError::into_inner))
}

/// Returns the hinting and antialiasing overrides in force on this thread.
pub fn text_rasterization() -> (Option<bool>, Option<bool>) {
    const fn decode(value: u8) -> Option<bool> {
        match value {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    SCOPED.with(|scoped| {
        if let Some(preferences) = scoped.borrow().as_ref() {
            return (preferences.text_hinting, preferences.text_antialias);
        }

        let encoded = TEXT_RASTERIZATION.load(Ordering::Relaxed);
        (decode(encoded & 0b11), decode(encoded >> 2))
    })
}

/// Replaces the process-wide preferences.
pub fn set(preferences: Preferences) {
    update(|current| *current = preferences);
}

/// Changes the process-wide preferences in place.
pub fn update(change: impl FnOnce(&mut Preferences)) {
    struct PublishRasterization(RwLockWriteGuard<'static, Preferences>);

    impl Drop for PublishRasterization {
        fn drop(&mut self) {
            TEXT_RASTERIZATION.store(encode_text_rasterization(&self.0), Ordering::Relaxed);
        }
    }

    let mut current = PublishRasterization(CURRENT.write().unwrap_or_else(PoisonError::into_inner));
    change(&mut current.0);
}

/// Runs `f` with [`get`] answering `preferences` on this thread.
pub fn scoped<R>(preferences: Preferences, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Preferences>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            SCOPED.with(|scoped| *scoped.borrow_mut() = previous);
        }
    }
    let _restore = Restore(SCOPED.with(|scoped| scoped.borrow_mut().replace(preferences)));
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rasterization_tracks_updates_and_scoped_overrides() {
        let previous = get();
        set(Preferences {
            text_hinting: Some(false),
            text_antialias: None,
            ..previous
        });
        let initial = text_rasterization();

        let (scoped_before, scoped_after) = scoped(
            Preferences {
                text_hinting: None,
                text_antialias: Some(false),
                ..previous
            },
            || {
                let before = text_rasterization();
                update(|preferences| preferences.text_antialias = Some(true));
                (before, text_rasterization())
            },
        );
        let updated = text_rasterization();
        set(previous);

        assert_eq!(initial, (Some(false), None));
        assert_eq!(scoped_before, (None, Some(false)));
        assert_eq!(scoped_after, scoped_before);
        assert_eq!(updated, (Some(false), Some(true)));
    }
}
