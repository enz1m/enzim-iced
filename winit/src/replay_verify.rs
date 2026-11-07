// Modified by enzUI contributors. See FORK.md.
//! Opt-in check that every frame the UI thread replays matches a full draw.
//!
//! With `ICED_REPLAY_VERIFY` set, a replayed frame is also drawn in full and
//! both are rendered offscreen and compared; the drawn frame is presented.
//! Each frame that differs is reported on standard error with the number of
//! differing pixels and their bounds. When the variable names a directory,
//! both renderings are also written there as raw RGBA
//! (`<frame>_<replayed|drawn>_<width>x<height>.rgba`). It doubles the work of
//! every replayed frame; the render lane is not verified, so run with
//! `ICED_DISABLE_ANIMATION_LANE=1` to route every replay through this check.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// A colour channel must differ by more than this for a pixel to count.
const TOLERANCE: u8 = 24;

static VERIFIED: AtomicU64 = AtomicU64::new(0);
static MISMATCHED: AtomicU64 = AtomicU64::new(0);

fn setting() -> Option<&'static Option<PathBuf>> {
    static SETTING: OnceLock<Option<Option<PathBuf>>> = OnceLock::new();
    SETTING
        .get_or_init(|| {
            let value = std::env::var_os("ICED_REPLAY_VERIFY")?;
            let directory = PathBuf::from(&value);
            Some(directory.is_dir().then_some(directory))
        })
        .as_ref()
}

/// Whether replayed frames are verified.
pub(crate) fn enabled() -> bool {
    setting().is_some()
}

/// Compares a replayed frame with the full draw of the same frame.
pub(crate) fn compare(replayed: &[u8], drawn: &[u8], width: u32, height: u32) {
    let frame = VERIFIED.fetch_add(1, Ordering::Relaxed);
    let mut pixels = 0_u64;
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for (index, (a, b)) in replayed
        .chunks_exact(4)
        .zip(drawn.chunks_exact(4))
        .enumerate()
    {
        if a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > TOLERANCE) {
            pixels += 1;
            let x = index as u32 % width.max(1);
            let y = index as u32 / width.max(1);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    if pixels == 0 {
        return;
    }
    let mismatched = MISMATCHED.fetch_add(1, Ordering::Relaxed) + 1;
    eprintln!(
        "replay-verify: frame {frame}: {pixels} pixels differ in ({left}, {top})..({right}, {bottom}); \
         {mismatched} of {} replayed frames differed",
        frame + 1
    );
    if let Some(Some(directory)) = setting() {
        for (name, bytes) in [("replayed", replayed), ("drawn", drawn)] {
            let _ = std::fs::write(
                directory.join(format!("{frame:06}_{name}_{width}x{height}.rgba")),
                bytes,
            );
        }
    }
}
