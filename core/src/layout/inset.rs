// Modified by enzUI contributors. See FORK.md.
//! Driver-bound shell insets with the same clamping and allocation on native and owned paths.

use super::{Limits, Node};
use crate::renderer::{NodeId, presentation};
use crate::{Alignment, Length, Padding, Size};

/// Logical-pixel inset policy; progress is clamped to the unit interval before arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Policy {
    /// Keeps the toggle inboard of a seam, stopping at the leading edge.
    ToggleTrailing {
        /// Full sidebar allocation before progress is applied.
        sidebar: f32,
        /// Unscaled control footprint along the seam axis.
        toggle: f32,
    },
    /// Reserves the toggle footprint not already covered by the sidebar.
    OppositeClearance {
        /// Full sidebar allocation before progress is applied.
        sidebar: f32,
        /// Unscaled control footprint along the seam axis.
        toggle: f32,
        /// Logical-pixel clearance added to the control footprint.
        gutter: f32,
    },
    /// Spends the withheld control's footprint without keeping a control mounted.
    WithheldClearance {
        /// Unscaled control footprint even while the control is absent.
        toggle: f32,
        /// Logical-pixel clearance withdrawn with the control footprint.
        gutter: f32,
    },
}

impl Policy {
    /// The native inset in logical pixels, before container padding fitting.
    pub fn amount(self, progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::ToggleTrailing { sidebar, toggle } => (sidebar * progress - toggle).max(0.0),
            Self::OppositeClearance {
                sidebar,
                toggle,
                gutter,
            } => (toggle + gutter - sidebar * progress).max(0.0),
            Self::WithheldClearance { toggle, gutter } => (toggle + gutter) * progress,
        }
    }

    /// Whether parameters and both clamped endpoints are finite and nonnegative.
    pub fn is_valid(self) -> bool {
        let (sidebar, toggle, gutter) = match self {
            Self::ToggleTrailing { sidebar, toggle } => (sidebar, toggle, 0.0),
            Self::OppositeClearance {
                sidebar,
                toggle,
                gutter,
            } => (sidebar, toggle, gutter),
            Self::WithheldClearance { toggle, gutter } => (0.0, toggle, gutter),
        };
        [sidebar, toggle, gutter, self.amount(0.0), self.amount(1.0)]
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.0)
    }

    /// Mirrors the padded edge; callers must mirror child alignment with it.
    pub fn padding(self, progress: f32, trailing: bool) -> Padding {
        let amount = self.amount(progress);
        if trailing {
            Padding::ZERO.right(amount)
        } else {
            Padding::ZERO.left(amount)
        }
    }
}

/// Native aligned allocation with an explicit independent layout driver.
///
/// Untyped children and invalid policies retain native drawing but cannot lower to owned layout.
pub fn resolve(
    limits: &Limits,
    sizing: Size<Length>,
    policy: Policy,
    progress: f32,
    trailing: bool,
    driver: NodeId,
    child: impl FnOnce(&Limits) -> Node,
) -> Node {
    let padding = policy.padding(progress, trailing);
    let alignment = [
        if trailing {
            Alignment::End
        } else {
            Alignment::Start
        },
        Alignment::Start,
    ];
    let node = super::positioned_node(
        limits,
        sizing.width,
        sizing.height,
        padding,
        |limits| child(&limits.loose()),
        |child, size| child.align(alignment[0], alignment[1], size),
    );
    let Some(child) = node.children()[0].presentation_sizing() else {
        return node;
    };
    if !policy.is_valid()
        || !progress.is_finite()
        || driver == NodeId::STATIC
        || driver.0 >= (1 << 63)
    {
        return node;
    }
    node.with_container_presentation(presentation::Container {
        reflow_min_width: None,
        clip_content: false,
        mode: super::container::Mode::Aligned,
        recorded: None,
        width: None,
        limits: *limits,
        sizing,
        padding,
        alignment,
        child,
        inset: Some(presentation::Inset {
            policy,
            driver: driver.0,
            progress: None,
            captured: progress.clamp(0.0, 1.0),
            trailing,
        }),
    })
}
