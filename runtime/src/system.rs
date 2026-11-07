// Modified by enzUI contributors. See FORK.md.
//! Access the native system.
use crate::core::theme;
use crate::futures::futures::channel::oneshot;
use crate::futures::subscription::{self, Subscription};
use crate::task::{self, Task};

/// An operation to be performed on the system.
#[derive(Debug)]
pub enum Action {
    /// Send available system information.
    GetInformation(oneshot::Sender<Information>),

    /// Send the current system theme mode.
    GetTheme(oneshot::Sender<theme::Mode>),

    /// Notify to the runtime that the system theme has changed.
    NotifyTheme(theme::Mode),

    /// Send the current system reduced-motion preference.
    ///
    /// `true` means the user has requested that the system minimize the
    /// amount of non-essential motion it uses (the OS accessibility
    /// "reduce motion" / "reduce animations" setting).
    GetReducedMotion(oneshot::Sender<bool>),

    /// Notify to the runtime that the system reduced-motion preference has
    /// changed.
    NotifyReducedMotion(bool),
}

/// Contains information about the system (e.g. system name, processor, memory, graphics adapter).
#[derive(Clone, Debug)]
pub struct Information {
    /// The operating system name
    pub system_name: Option<String>,
    /// Operating system kernel version
    pub system_kernel: Option<String>,
    /// Long operating system version
    ///
    /// Examples:
    /// - MacOS 10.15 Catalina
    /// - Windows 10 Pro
    /// - Ubuntu 20.04 LTS (Focal Fossa)
    pub system_version: Option<String>,
    /// Short operating system version number
    pub system_short_version: Option<String>,
    /// Detailed processor model information
    pub cpu_brand: String,
    /// The number of physical cores on the processor
    pub cpu_cores: Option<usize>,
    /// Total RAM size, in bytes
    pub memory_total: u64,
    /// Memory used by this process, in bytes
    pub memory_used: Option<u64>,
    /// Underlying graphics backend for rendering
    pub graphics_backend: String,
    /// Model information for the active graphics adapter
    pub graphics_adapter: String,
}

/// Returns available system information.
pub fn information() -> Task<Information> {
    task::oneshot(|channel| crate::Action::System(Action::GetInformation(channel)))
}

/// Returns the current system theme.
pub fn theme() -> Task<theme::Mode> {
    task::oneshot(|sender| crate::Action::System(Action::GetTheme(sender)))
}

/// Subscribes to system theme changes.
pub fn theme_changes() -> Subscription<theme::Mode> {
    #[derive(Hash)]
    struct ThemeChanges;

    subscription::filter_map(ThemeChanges, |event| {
        let subscription::Event::SystemThemeChanged(mode) = event else {
            return None;
        };

        Some(mode)
    })
}

/// Returns the current system reduced-motion preference.
///
/// `true` means the user has requested reduced motion.
pub fn reduced_motion() -> Task<bool> {
    task::oneshot(|sender| crate::Action::System(Action::GetReducedMotion(sender)))
}

/// Subscribes to system reduced-motion preference changes.
pub fn reduced_motion_changes() -> Subscription<bool> {
    #[derive(Hash)]
    struct ReducedMotionChanges;

    subscription::filter_map(ReducedMotionChanges, |event| {
        let subscription::Event::SystemReducedMotionChanged(reduced_motion) = event else {
            return None;
        };

        Some(reduced_motion)
    })
}
