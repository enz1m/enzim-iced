// Modified by enzUI contributors. See FORK.md.
//! enzICED: a held mouse button keeps the pointer in its window.
//!
//! A press starts an implicit grab: until the release, the window keeps
//! receiving motion wherever the pointer goes. Mutter still reports the
//! pointer leaving and re-entering as it crosses the window's edge (onto a
//! shadow or resize band the window draws around itself), and a `CursorLeft`
//! there ends every drag that treats a lost pointer as a cancel: a scrollbar
//! thumb, a slider, a reordered row. While a button is held, the leave is
//! held back: re-entering forgets it, and a release outside delivers it
//! right after the release, so the window still learns the pointer is gone.

use winit::event::{ElementState, MouseButton, WindowEvent};

/// What a window does with one event.
#[derive(Debug)]
pub(crate) enum Admit {
    /// Hold it back: a leave while a button is held.
    Hold,
    /// Handle it.
    Pass(WindowEvent),
    /// Handle the release, then the leave held back during the press.
    PassThenLeave(WindowEvent, WindowEvent),
}

/// The buttons a window's pointer holds, and a leave held back meanwhile.
#[derive(Debug, Default)]
pub(crate) struct PointerHold {
    held: u32,
    left: Option<WindowEvent>,
}

fn bit(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 1 << 1,
        MouseButton::Middle => 1 << 2,
        MouseButton::Back => 1 << 3,
        MouseButton::Forward => 1 << 4,
        MouseButton::Other(n) => 1 << (5 + u32::from(n % 27)),
    }
}

impl PointerHold {
    pub(crate) fn admit(&mut self, event: WindowEvent) -> Admit {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                self.held |= bit(button);
                Admit::Pass(event)
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                self.held &= !bit(button);
                match self.left.take() {
                    Some(left) if self.held == 0 => Admit::PassThenLeave(event, left),
                    left => {
                        self.left = left;
                        Admit::Pass(event)
                    }
                }
            }
            WindowEvent::CursorLeft { .. } if self.held != 0 => {
                self.left = Some(event);
                Admit::Hold
            }
            WindowEvent::CursorEntered { .. } => {
                self.left = None;
                Admit::Pass(event)
            }
            // Losing focus can lose the release with it: let go, and deliver
            // a leave held back.
            WindowEvent::Focused(false) => {
                self.held = 0;
                match self.left.take() {
                    Some(left) => Admit::PassThenLeave(event, left),
                    None => Admit::Pass(event),
                }
            }
            event => Admit::Pass(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::DeviceId;

    fn device() -> DeviceId {
        DeviceId::dummy()
    }

    fn input(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device(),
            state,
            button: MouseButton::Left,
        }
    }

    fn kind(admit: &Admit) -> &'static str {
        match admit {
            Admit::Hold => "hold",
            Admit::Pass(_) => "pass",
            Admit::PassThenLeave(..) => "pass, then leave",
        }
    }

    /// A drag across the window's edge and back keeps the pointer; a release
    /// outside delivers the leave after it; without a button, a leave passes.
    ///
    /// Sabotage: pass `CursorLeft` whatever is held. The drag's leave passes.
    #[test]
    fn a_held_button_keeps_the_pointer_until_its_release() {
        let mut hold = PointerHold::default();
        let leave = || WindowEvent::CursorLeft {
            device_id: device(),
        };
        let enter = || WindowEvent::CursorEntered {
            device_id: device(),
        };
        let steps = [
            (input(ElementState::Pressed), "pass"),
            (leave(), "hold"),
            (enter(), "pass"),
            (leave(), "hold"),
            (input(ElementState::Released), "pass, then leave"),
            (enter(), "pass"),
            (leave(), "pass"),
            (input(ElementState::Pressed), "pass"),
            (leave(), "hold"),
            (WindowEvent::Focused(false), "pass, then leave"),
            (leave(), "pass"),
        ];
        for (index, (event, expected)) in steps.into_iter().enumerate() {
            assert_eq!(kind(&hold.admit(event)), expected, "step {index}");
        }
    }
}
