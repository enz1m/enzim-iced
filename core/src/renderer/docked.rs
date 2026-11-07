// Modified by enzUI contributors. See FORK.md.
//! Full-shell capture requests that require both responsive layout and presented-input ownership.

use super::{NodeId, TrackAnimation};
use crate::Rectangle;

/// Published layout identities shared by a backend and its admitted captures.
///
/// Ordinary recording publishes immediately; staged recording waits for activation.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    identities: std::sync::Arc<std::sync::atomic::AtomicU64>,
    staged: Option<std::sync::Arc<Pending>>,
}

#[derive(Debug)]
struct Pending {
    identities: std::sync::atomic::AtomicU64,
    active: std::sync::atomic::AtomicBool,
}

impl Recorder {
    /// Marks the start of a recorded draw over whatever layout exists now.
    pub fn observe_layout(&self) {
        self.staged
            .as_ref()
            .map(|pending| &pending.identities)
            .unwrap_or(&self.identities)
            .store(
                NodeId::layout_watermark(),
                std::sync::atomic::Ordering::Relaxed,
            );
    }

    /// Creates a recording scope that leaves the published capture current.
    ///
    /// Its captures require activation by the surface owner between frames.
    /// Dropping an unactivated scope or capture does not expire the published scene.
    #[must_use]
    pub fn stage(&self) -> Self {
        Self {
            identities: self.identities.clone(),
            staged: Some(std::sync::Arc::new(Pending {
                identities: std::sync::atomic::AtomicU64::new(NodeId::layout_watermark()),
                active: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }

    /// Grants a capture the identities of the scene being recorded.
    pub fn tenure(&self) -> Tenure {
        Tenure {
            recorder: self.identities.clone(),
            granted: self
                .staged
                .as_ref()
                .map(|pending| &pending.identities)
                .unwrap_or(&self.identities)
                .load(std::sync::atomic::Ordering::Relaxed),
            staged: self.staged.clone(),
        }
    }
}

/// How long an admitted [`Capture`] may keep painting after handover.
///
/// Publishing newer layout identities expires earlier captures. Ordinary draws
/// publish during recording; staged replacements publish only at activation.
#[derive(Debug, Clone)]
pub struct Tenure {
    recorder: std::sync::Arc<std::sync::atomic::AtomicU64>,
    granted: u64,
    staged: Option<std::sync::Arc<Pending>>,
}

impl Tenure {
    /// Whether these identities are the granting backend's published identities.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.staged
            .as_ref()
            .is_none_or(|pending| pending.active.load(std::sync::atomic::Ordering::Acquire))
            && self.recorder.load(std::sync::atomic::Ordering::Acquire) == self.granted
    }

    /// Publishes a staged capture, or checks an ordinary capture without renewing it.
    ///
    /// Only the surface owner may activate, between frames and before replacement replay.
    /// Newer published identities or a concurrent publication cause refusal; an expired
    /// ordinary capture can never be renewed through this method.
    #[must_use]
    pub fn activate(&self) -> bool {
        use std::sync::atomic::Ordering;
        let Some(pending) = &self.staged else {
            return self.is_current();
        };
        let published = self.recorder.load(Ordering::Acquire);
        if published > self.granted
            || self
                .recorder
                .compare_exchange(published, self.granted, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        pending.active.store(true, Ordering::Release);
        true
    }

    /// The layout-identity watermark this tenure was granted at.
    #[must_use]
    pub fn granted(&self) -> u64 {
        self.granted
    }
}

/// A full-shell responsive capture request, not permission to transfer a surface.
///
/// Progress is carried by translation-x solely as a layout driver; it must never
/// translate or scale a retained endpoint bitmap. Proposed curves do not authorize
/// handover: the renderer must first bind complete layout, paint and presented-input ownership.
#[derive(Debug, Clone)]
pub struct Capture {
    /// Whole supplied subtree lowering; a failure cannot be replaced by endpoint paint.
    pub layout: Result<std::sync::Arc<super::presentation::Program>, super::presentation::Error>,
    /// Stable driver identity and the theme-sampled remaining travel.
    pub animation: TrackAnimation,
    /// Other independently moving layout drivers; total driver population is capped at eight.
    pub additional: Vec<TrackAnimation>,
    /// Stationary layout-driver values required by this recording, not endpoint paint.
    pub stationary: Vec<super::Track>,
    /// Full shell allocation in logical viewport coordinates.
    pub bounds: Rectangle,
}
impl Capture {
    /// Checks bounded finite progress keyframes, including reversed progress.
    pub fn is_valid(&self) -> bool {
        if self.additional.len() > 7
            || self.stationary.len() > 7
            || 1 + self.additional.len() + self.stationary.len() > 8
            || ![
                self.bounds.x,
                self.bounds.y,
                self.bounds.width,
                self.bounds.height,
                self.bounds.x + self.bounds.width,
                self.bounds.y + self.bounds.height,
            ]
            .into_iter()
            .all(f32::is_finite)
            || self.bounds.width <= 0.0
            || self.bounds.height <= 0.0
        {
            return false;
        }
        let mut nodes = std::collections::BTreeSet::new();
        for animation in std::iter::once(&self.animation).chain(&self.additional) {
            if !nodes.insert(animation.node) || !valid_animation(animation) {
                return false;
            }
        }
        self.stationary
            .iter()
            .all(|track| nodes.insert(track.node) && valid_progress(track))
    }

    /// Binds layout-only proposals to the lowered driver set, discarding unused proposals.
    /// Lowering failures are preserved; malformed or incomplete driver sets remain refused.
    #[must_use]
    pub fn bind_layout(
        mut self,
        layout: Result<std::sync::Arc<super::presentation::Program>, super::presentation::Error>,
    ) -> Self {
        self.layout = layout;
        let Ok(program) = &self.layout else {
            return self;
        };
        if !self.is_valid() {
            self.layout = Err(super::presentation::Error::DockedOwnershipRequired);
            return self;
        }
        self.additional.retain(|animation| {
            program
                .layout_drivers()
                .any(|node| node == animation.node.0)
        });
        self.stationary
            .retain(|track| program.layout_drivers().any(|node| node == track.node.0));
        if !program
            .layout_drivers()
            .any(|node| node == self.animation.node.0)
        {
            if self.additional.is_empty() {
                self.layout = Err(super::presentation::Error::DockedOwnershipRequired);
                return self;
            }
            self.animation = self.additional.remove(0);
        }
        if !self.covers_program(program) {
            self.layout = Err(super::presentation::Error::DockedOwnershipRequired);
        }
        self
    }

    /// Publishes the complete bound driver group.
    /// Returns false without publication for unresolved or incomplete layout; no surface authority is granted.
    pub fn publish<Message>(&self, shell: &mut crate::Shell<'_, Message>) -> bool {
        let Ok(program) = &self.layout else {
            return false;
        };
        if !self.covers_program(program) {
            return false;
        }
        shell.publish_docked_tracks(
            std::iter::once(&self.animation)
                .chain(&self.additional)
                .map(|animation| animation.keyframes[0].1)
                .chain(self.stationary.iter().copied()),
        );
        for animation in std::iter::once(&self.animation).chain(&self.additional) {
            shell.publish_track_animation(animation.clone());
        }
        true
    }

    /// Requires exactly the program's driver set; omitted or unrelated tracks cannot complete ownership.
    pub fn covers_program(&self, program: &super::presentation::Program) -> bool {
        if !self.is_valid() {
            return false;
        }
        let mut nodes = vec![self.animation.node.0];
        nodes.extend(self.additional.iter().map(|animation| animation.node.0));
        nodes.extend(self.stationary.iter().map(|track| track.node.0));
        nodes.sort_unstable();
        program.layout_drivers().eq(nodes)
    }
}

fn valid_progress(track: &super::Track) -> bool {
    track.node != NodeId::STATIC
        && track.node.0 < (1 << 63)
        && track.translation.x.is_finite()
        && (0.0..=1.0).contains(&track.translation.x)
        && track.translation.y == 0.0
        && track.opacity == 1.0
        && track.scale == 1.0
        && track.scale_origin == crate::Point::ORIGIN
        && track.clip.is_none()
}

fn valid_animation(animation: &TrackAnimation) -> bool {
    animation.is_usable()
        && animation.keyframes.len() <= 256
        && animation
            .keyframes
            .first()
            .is_some_and(|(at, _)| *at == 0.0)
        && animation.keyframes.last().is_some_and(|(at, _)| *at == 1.0)
        && animation
            .keyframes
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0)
        && animation.keyframes.iter().all(|(at, track)| {
            at.is_finite()
                && (0.0..=1.0).contains(at)
                && track.node == animation.node
                && valid_progress(track)
        })
}
