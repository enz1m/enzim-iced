// Modified by enzUI contributors. See FORK.md.
use iced_core as core;

pub(crate) fn activation_platform(
    display: core::window::raw_window_handle::RawDisplayHandle,
) -> Option<core::window::ActivationPlatform> {
    use core::window::ActivationPlatform;
    use core::window::raw_window_handle::RawDisplayHandle;
    match display {
        RawDisplayHandle::Wayland(_) => Some(ActivationPlatform::Wayland),
        RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => Some(ActivationPlatform::X11),
        _ => None,
    }
}

pub(crate) fn activation_request_matches<Serial: PartialEq>(
    candidate_window: core::window::Id,
    candidate_serial: &Serial,
    window: core::window::Id,
    serial: &Serial,
) -> bool {
    candidate_window == window && candidate_serial == serial
}

pub(crate) fn activation_request_expired(
    expires_at: core::time::Instant,
    now: core::time::Instant,
) -> bool {
    now >= expires_at
}

pub(crate) fn activate_surface_once<Error>(
    token: &mut Option<core::window::ActivationToken>,
    platform: Option<core::window::ActivationPlatform>,
    submit: impl FnOnce(winit::window::ActivationToken) -> Result<(), Error>,
) -> Result<(), core::window::ActivationError> {
    let token = token
        .take()
        .ok_or(core::window::ActivationError::Unsupported)?;
    let raw = platform
        .and_then(|platform| token.into_raw_for(platform))
        .ok_or(core::window::ActivationError::Unsupported)?;
    submit(winit::window::ActivationToken::from_raw(raw))
        .map_err(|_| core::window::ActivationError::Unsupported)
}
