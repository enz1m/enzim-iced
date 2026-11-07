// Modified by enzUI contributors. See FORK.md.
//! Native activation request and token-consumption checks.

#[path = "../src/activation_policy.rs"]
mod activation_policy;

use activation_policy::{
    activate_surface_once, activation_platform, activation_request_expired,
    activation_request_matches,
};
use iced_core as core;
use iced_core::time::{Duration, Instant};

#[test]
fn activation_request_context_and_surface_consumption_are_bounded() {
    let source = core::window::Id::unique();
    let other = core::window::Id::unique();
    assert!(activation_request_matches(source, &7_u64, source, &7));
    assert!(!activation_request_matches(source, &7_u64, source, &8));
    assert!(!activation_request_matches(source, &7_u64, other, &7));

    let now = Instant::now();
    let deadline = now + Duration::from_millis(5);
    assert!(!activation_request_expired(deadline, now));
    assert!(activation_request_expired(deadline, deadline));

    let mut token = Some(core::window::ActivationToken::from_raw("one-shot".into()));
    let mut submitted = Vec::new();
    assert_eq!(
        activate_surface_once(
            &mut token,
            Some(core::window::ActivationPlatform::Wayland),
            |token| {
                submitted.push(token.into_raw());
                Ok::<_, ()>(())
            }
        ),
        Ok(())
    );
    assert!(token.is_none());
    assert_eq!(submitted, ["one-shot"]);

    let mut called_without_token = false;
    assert_eq!(
        activate_surface_once(
            &mut token,
            Some(core::window::ActivationPlatform::Wayland),
            |_| {
                called_without_token = true;
                Ok::<_, ()>(())
            }
        ),
        Err(core::window::ActivationError::Unsupported)
    );
    assert!(!called_without_token);
}

#[test]
fn activation_platform_selection_preserves_explicit_context() {
    use core::window::raw_window_handle::{
        DrmDisplayHandle, RawDisplayHandle, WaylandDisplayHandle, XcbDisplayHandle,
        XlibDisplayHandle,
    };
    use core::window::{ActivationPlatform, ActivationToken};
    assert_eq!(
        activation_platform(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            std::ptr::NonNull::dangling()
        ))),
        Some(ActivationPlatform::Wayland)
    );
    assert_eq!(
        activation_platform(RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0))),
        Some(ActivationPlatform::X11)
    );
    assert_eq!(
        activation_platform(RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0))),
        Some(ActivationPlatform::X11)
    );
    assert_eq!(
        activation_platform(RawDisplayHandle::Drm(DrmDisplayHandle::new(0))),
        None
    );
    for (platform, expected) in [
        (ActivationPlatform::Wayland, "wayland"),
        (ActivationPlatform::X11, "x11"),
    ] {
        let paired =
            ActivationToken::from_platform_data(Some("wayland".into()), Some("x11".into()))
                .unwrap();
        let mut token = Some(paired);
        let mut submitted = Vec::new();
        assert_eq!(
            activate_surface_once(&mut token, Some(platform), |token| {
                submitted.push(token.into_raw());
                Ok::<_, ()>(())
            }),
            Ok(())
        );
        assert_eq!(submitted, [expected]);
        assert!(token.is_none());
    }
    let mut wrong = Some(ActivationToken::for_platform(
        ActivationPlatform::X11,
        "x11".into(),
    ));
    let mut submitted = false;
    assert_eq!(
        activate_surface_once(&mut wrong, Some(ActivationPlatform::Wayland), |_| {
            submitted = true;
            Ok::<_, ()>(())
        }),
        Err(core::window::ActivationError::Unsupported)
    );
    assert!(!submitted);
    assert!(wrong.is_none());
}
