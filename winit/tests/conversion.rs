// Modified by enzUI contributors. See FORK.md.
//! enzUI #2831 integration coverage: `iced_winit::conversion::window_event`
//! normalizes a physical `PixelDelta` to logical coordinates, leaving
//! `LineDelta` unchanged.
//!
//! These live in `tests/` rather than an inline `#[cfg(test)] mod` so that
//! `cargo test -p iced_winit --test conversion` builds `iced_winit` in its
//! normal (non-`cfg(test)`) profile: the locked target runs this gate without
//! compiling any unrelated `#[cfg(test)]` module in the crate.

use iced_winit::conversion::window_event;
use iced_winit::core::{Event, mouse};
use iced_winit::winit;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::ModifiersState;

// A `PixelDelta` arrives from winit in physical pixels (the Wayland backend
// applies `.to_physical`), and iced widgets consume `ScrollDelta::Pixels` in
// logical layout units, so `window_event` must divide by `scale_factor` exactly
// as `CursorMoved` does. `scale_factor` is the caller's authoritative per-window
// viewport scale (`State::scale_factor()`), so these cases stand in for every
// per-window/per-scale value the event loop supplies.

fn wheel(delta: MouseScrollDelta, scale_factor: f32) -> mouse::ScrollDelta {
    match window_event(
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta,
            phase: TouchPhase::Moved,
        },
        scale_factor,
        ModifiersState::empty(),
    ) {
        Some(Event::Mouse(mouse::Event::WheelScrolled { delta })) => delta,
        other => panic!("expected a WheelScrolled mouse event, got {other:?}"),
    }
}

fn pixels(delta: mouse::ScrollDelta) -> (f32, f32) {
    match delta {
        mouse::ScrollDelta::Pixels { x, y } => (x, y),
        mouse::ScrollDelta::Lines { .. } => {
            panic!("expected pixel delta, got line delta")
        }
    }
}

#[test]
fn pixel_delta_normalizes_to_logical_across_hidpi_scales() {
    // A fixed finger intent of 60 logical px, measured at 1x, 1.5x, 2x and the
    // reporter's 2.75x. winit hands us `intent * scale` physical px; the logical
    // result must be 60 on every output, so scroll distance never varies with
    // density (the #2831 defect made 2.75x scroll 2.75x too far).
    let intent = 60.0_f32;

    for scale in [1.0_f32, 1.5, 2.0, 2.75] {
        let physical = f64::from(intent) * f64::from(scale);
        let (x, y) = pixels(wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, physical)),
            scale,
        ));

        assert_eq!(x, 0.0, "no horizontal movement was sent at scale {scale}");
        assert!(
            (y - intent).abs() < 1.0e-3,
            "scale {scale}: {physical} physical px should normalize to \
             {intent} logical px, got {y}"
        );
    }
}

#[test]
fn pixel_delta_normalizes_both_axes() {
    // Both axes are divided by the same scale factor; at 2x, physical (30, 90)
    // is logical (15, 45).
    let (x, y) = pixels(wheel(
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(30.0, 90.0)),
        2.0,
    ));

    assert!((x - 15.0).abs() < 1.0e-3, "horizontal axis: got {x}");
    assert!((y - 45.0).abs() < 1.0e-3, "vertical axis: got {y}");
}

#[test]
fn line_delta_is_never_scaled() {
    // Control: `LineDelta` is a unitless tick count. It must survive any scale
    // factor untouched; only `PixelDelta` is a physical measurement.
    let delta = wheel(MouseScrollDelta::LineDelta(1.0, -3.0), 2.75);

    match delta {
        mouse::ScrollDelta::Lines { x, y } => {
            assert_eq!(x, 1.0);
            assert_eq!(y, -3.0);
        }
        mouse::ScrollDelta::Pixels { .. } => panic!("line delta became a pixel delta"),
    }
}

/// Touchpad gestures reach iced as `Event::Gesture`, a swipe in logical
/// pixels like the scroll above (enzICED).
#[test]
fn touchpad_gestures_become_gesture_events() {
    use iced_winit::core::gesture;
    let device_id = DeviceId::dummy();
    let pinch = window_event(
        WindowEvent::PinchGesture {
            device_id,
            delta: 0.25,
            phase: TouchPhase::Moved,
        },
        2.0,
        ModifiersState::empty(),
    );
    assert_eq!(
        pinch,
        Some(Event::Gesture(gesture::Event::Pinch {
            scale: 0.25,
            phase: gesture::Phase::Moved,
        }))
    );
    let swipe = window_event(
        WindowEvent::PanGesture {
            device_id,
            delta: PhysicalPosition::new(40.0, -20.0),
            phase: TouchPhase::Ended,
        },
        2.0,
        ModifiersState::empty(),
    );
    assert_eq!(
        swipe,
        Some(Event::Gesture(gesture::Event::Swipe {
            delta: iced_winit::core::Vector::new(20.0, -10.0),
            phase: gesture::Phase::Ended,
        }))
    );
}

/// The compositor's decoration report reaches iced (enzICED).
#[test]
fn decoration_reports_become_window_events() {
    use iced_winit::core::window;
    let event = window_event(
        WindowEvent::DecorationsChanged {
            server_side: true,
            tiled: winit::event::TiledEdges {
                top: true,
                bottom: false,
                left: true,
                right: false,
            },
            maximized: false,
            fullscreen: false,
        },
        1.0,
        ModifiersState::empty(),
    );
    assert_eq!(
        event,
        Some(Event::Window(window::Event::DecorationsChanged(
            window::Decorations {
                server_side: true,
                tiled: window::TiledEdges {
                    top: true,
                    bottom: false,
                    left: true,
                    right: false,
                },
                maximized: false,
                fullscreen: false,
            }
        )))
    );
}

#[test]
fn tablet_tools_become_stylus_events() {
    use iced_winit::core::stylus::{self, Button, Phase, ToolKind};
    use winit::event::{
        ElementState, TabletTool, TabletToolEvent, TabletToolKind, TabletToolPhase,
    };

    let tool = |phase| TabletToolEvent {
        device_id: DeviceId::dummy(),
        tool: TabletTool {
            kind: TabletToolKind::Eraser,
            serial: 0xA1,
        },
        phase,
        position: PhysicalPosition::new(300.0, 150.0),
        pressure: 0.5,
        distance: 0.0,
        tilt: (40.0, -10.0),
        rotation: 90.0,
    };
    let convert = |phase| match window_event(
        WindowEvent::TabletTool(tool(phase)),
        2.0,
        ModifiersState::default(),
    ) {
        Some(Event::Stylus(event)) => event,
        other => panic!("expected a stylus event, got {other:?}"),
    };

    let down = convert(TabletToolPhase::Down);
    assert_eq!(
        down,
        stylus::Event {
            tool: stylus::Tool {
                kind: ToolKind::Eraser,
                serial: 0xA1
            },
            phase: Phase::Down,
            position: iced_winit::core::Point::new(150.0, 75.0),
            pressure: 0.5,
            distance: 0.0,
            tilt: iced_winit::core::Vector::new(40.0, -10.0),
            rotation: 90.0,
        },
        "positions are logical"
    );
    let lower = convert(TabletToolPhase::Button {
        button: 0x14b,
        state: ElementState::Pressed,
    });
    assert_eq!(lower.phase, Phase::ButtonPressed(Button::Lower));
    let upper = convert(TabletToolPhase::Button {
        button: 0x14c,
        state: ElementState::Released,
    });
    assert_eq!(upper.phase, Phase::ButtonReleased(Button::Upper));
}
