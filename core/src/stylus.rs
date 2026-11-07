// Modified by enzUI contributors. See FORK.md.
//! Drawing tablet tools: pens, erasers and pucks (enzICED).
//!
//! A tool also moves the mouse cursor and presses its buttons (contact is
//! the left button, the barrel buttons are middle and right), so widgets that
//! only read the mouse work with a pen. A widget that reads the stylus can
//! skip the mouse events that follow each stylus event while
//! [`Phase::ProximityIn`] has been seen and [`Phase::ProximityOut`] has not.
use crate::{Point, Vector};

/// One change of a tablet tool over the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Which tool this is.
    pub tool: Tool,
    /// What happened.
    pub phase: Phase,
    /// The tip, in logical pixels.
    pub position: Point,
    /// How hard the tip presses, from `0.0` to `1.0`; `0.0` while hovering.
    pub pressure: f32,
    /// How far the tool is above the surface, from `0.0` to `1.0`; `0.0`
    /// when the tablet cannot tell.
    pub distance: f32,
    /// The tilt in degrees from the perpendicular, positive towards the right
    /// (x) and towards the user (y).
    pub tilt: Vector,
    /// The rotation about the tool's own axis in degrees, clockwise.
    pub rotation: f32,
}

/// A physical tablet tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tool {
    /// What kind of tool it is.
    pub kind: ToolKind,
    /// Its hardware serial number, `0` when unknown; one pen keeps it across
    /// tablets and sessions, so a brush can be remembered per pen.
    pub serial: u64,
}

/// The kind of a tablet tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// A pen.
    Pen,
    /// The eraser end of a pen.
    Eraser,
    /// A brush.
    Brush,
    /// A pencil.
    Pencil,
    /// An airbrush.
    Airbrush,
    /// A finger on a tablet that reports it as a tool.
    Finger,
    /// A puck shaped like a mouse.
    Mouse,
    /// A puck with a lens and crosshair.
    Lens,
}

/// What a tool did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The tool came within range over the window.
    ProximityIn,
    /// The tool moved, touching or hovering.
    Moved,
    /// The tip touched the surface.
    Down,
    /// The tip left the surface.
    Up,
    /// A barrel button was pressed.
    ButtonPressed(Button),
    /// A barrel button was released.
    ButtonReleased(Button),
    /// The tool went out of range or left the window.
    ProximityOut,
}

/// A button on a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// The lower barrel button (`BTN_STYLUS`), which also presses the middle
    /// mouse button.
    Lower,
    /// The upper barrel button (`BTN_STYLUS2`), which also presses the right
    /// mouse button.
    Upper,
    /// Another button, by its Linux input code.
    Other(u32),
}
