// Modified by enzUI contributors. See FORK.md.
//! Modal dialog windows and the windows they block (enzICED).

use crate::core::window;

use rustc_hash::FxHashMap;

/// The open modal dialog that blocks `target`, if any. `dialogs` maps each
/// modal dialog to its parent; `is_open` says whether a window still exists.
pub(crate) fn blocking_dialog(
    dialogs: &FxHashMap<window::Id, window::Id>,
    target: window::Id,
    mut is_open: impl FnMut(window::Id) -> bool,
) -> Option<window::Id> {
    dialogs
        .iter()
        .filter(|(dialog, parent)| **parent == target && is_open(**dialog))
        .map(|(dialog, _)| *dialog)
        .min()
}

/// Whether a window event is input a blocked window must not receive, and
/// whether it should raise the dialog instead.
pub(crate) fn blocked_input(event: &winit::event::WindowEvent) -> Option<bool> {
    use winit::event::{ElementState, WindowEvent};
    match event {
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        }
        | WindowEvent::KeyboardInput { .. } => Some(true),
        WindowEvent::CursorMoved { .. }
        | WindowEvent::CursorEntered { .. }
        | WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::Touch(_)
        | WindowEvent::ModifiersChanged(_)
        | WindowEvent::Ime(_)
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::DroppedFile(_)
        | WindowEvent::HoveredFile(_) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_modal_dialog_blocks_only_its_parent() {
        let parent = window::Id::unique();
        let other = window::Id::unique();
        let dialog = window::Id::unique();
        let mut dialogs = FxHashMap::default();
        let _ = dialogs.insert(dialog, parent);

        assert_eq!(blocking_dialog(&dialogs, parent, |_| true), Some(dialog));
        assert_eq!(blocking_dialog(&dialogs, other, |_| true), None);
        assert_eq!(blocking_dialog(&dialogs, dialog, |_| true), None);
        assert_eq!(
            blocking_dialog(&dialogs, parent, |id| id != dialog),
            None,
            "a closed dialog blocks nothing"
        );
    }

    #[test]
    fn presses_and_keys_raise_the_dialog_and_other_input_is_dropped() {
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        assert_eq!(blocked_input(&press), Some(true));
        assert_eq!(blocked_input(&release), Some(false));
        assert_eq!(blocked_input(&WindowEvent::Focused(true)), None);
        assert_eq!(blocked_input(&WindowEvent::CloseRequested), None);
    }
}
