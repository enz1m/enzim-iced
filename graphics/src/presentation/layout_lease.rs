// Modified by enzUI contributors. See FORK.md.
use super::{FrameKey, PresentationSink, TrackGeneration};
use crate::core::{layout, renderer::presentation::Sample};
use std::sync::Arc;
use std::time::Instant;

/// Current window and draw identity required before projected allocations can route input.
#[derive(Debug, Clone, Copy)]
pub struct LayoutLeaseContext<'a> {
    /// Window that owns the native widget tree.
    pub window: u64,
    /// Current configured surface generation.
    pub surface: u64,
    /// Current successful surface-owner transfer generation.
    pub ownership: u64,
    /// Current installed recording generation.
    pub scene: u64,
    /// Current logical viewport, including the configured application scale.
    pub viewport: [f32; 2],
    /// Exact program/draw witness retained by the current scene owner.
    pub source: &'a Sample,
    /// Complete active layout-driver generations, sorted by node and nonempty.
    pub tracks: &'a [TrackGeneration],
}

/// Reasons a stale or incompletely reconciled layout cannot become an input lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutLeaseError {
    /// No matching compositor-presented frame; submission or discard is insufficient.
    NotPresented,
    /// A window, source, viewport or complete driver-set mismatch.
    Identity,
    /// Native editor/scroll state is unreconciled, or paint properties have no supported reconciliation.
    ReconciliationRequired,
}

/// One compositor-confirmed layout candidate tied to an operation batch.
///
/// Geometry refresh is read-only. Native input requires every declared editor and scroll
/// adapter to reconcile; unrelated paint properties refuse. Refresh each batch and
/// invalidate on epoch or scale changes.
#[derive(Debug)]
pub struct LayoutLease {
    key: FrameKey,
    sampled_at: Instant,
    sample: Arc<Sample>,
}
impl LayoutLease {
    /// Exact joined submission; not a locally generated presentation sequence.
    pub fn key(&self) -> FrameKey {
        self.key
    }

    /// When the displayed value was sampled, not the start time of a new transition.
    pub fn sampled_at(&self) -> Instant {
        self.sampled_at
    }

    /// Exact sample selected by the joined frame, not the caller's earlier source witness.
    pub fn sample(&self) -> &Arc<Sample> {
        &self.sample
    }

    /// The displayed layout driver's value; never resamples the UI animation clock.
    pub fn driver(&self, node: u64) -> Option<f32> {
        self.sample.driver(node, 0)
    }

    /// Projects only the recorded hierarchy before an input or accessibility operation.
    ///
    /// # Errors
    /// Refuses a different native hierarchy or a projection exceeding the layout budget.
    pub fn project(
        &self,
        source: &layout::Node,
    ) -> Result<layout::Node, crate::core::renderer::presentation::Error> {
        self.sample.project_layout(source)
    }
}

/// Keeps at most one confirmed projection candidate; a failed refresh revokes it.
#[derive(Debug, Default)]
pub struct LayoutLeaseSlot {
    current: Option<LayoutLease>,
}
impl LayoutLeaseSlot {
    /// Releases the retained sample on replacement, resize, surface loss or shutdown.
    pub fn invalidate(&mut self) {
        self.current = None;
    }

    /// Installs a complete, matching compositor-confirmed layout-only lease.
    ///
    /// This entry point refuses editor and scroll state. Unknown paint tracks are also refused,
    /// even if active-anchor lookup accepts them as a harmless superset for animation timing.
    ///
    /// # Errors
    /// Refuses unconfirmed, stale, mismatched or unreconciled geometry and revokes the prior lease.
    pub fn refresh(
        &mut self,
        sink: &PresentationSink,
        context: LayoutLeaseContext<'_>,
    ) -> Result<&LayoutLease, LayoutLeaseError> {
        self.refresh_checked(sink, context, false, false)
    }

    /// Joins a candidate for native editor and scroll reconciliation, not input permission.
    /// The renderer must validate the payload and every declared native adapter must succeed.
    /// Unrelated paint properties refuse; every failure revokes the previous candidate.
    pub fn refresh_for_native_reconciliation(
        &mut self,
        sink: &PresentationSink,
        context: LayoutLeaseContext<'_>,
    ) -> Result<&LayoutLease, LayoutLeaseError> {
        self.refresh_checked(sink, context, true, false)
    }

    /// Joins geometry for read-only semantic collection, never native input dispatch.
    /// Exact frame/source/viewport/driver checks still apply; unrelated paint properties refuse.
    pub fn refresh_for_geometry(
        &mut self,
        sink: &PresentationSink,
        context: LayoutLeaseContext<'_>,
    ) -> Result<&LayoutLease, LayoutLeaseError> {
        self.refresh_checked(sink, context, false, true)
    }

    fn refresh_checked(
        &mut self,
        sink: &PresentationSink,
        context: LayoutLeaseContext<'_>,
        editors: bool,
        geometry: bool,
    ) -> Result<&LayoutLease, LayoutLeaseError> {
        self.invalidate();
        if context.tracks.is_empty()
            || context
                .tracks
                .windows(2)
                .any(|pair| pair[0].node >= pair[1].node)
            || !context
                .viewport
                .into_iter()
                .all(|value| value.is_finite() && value > 0.0)
        {
            return Err(LayoutLeaseError::Identity);
        }
        let (frame, _) = sink
            .active_anchor(
                context.surface,
                context.ownership,
                context.scene,
                context.tracks,
            )
            .ok_or(LayoutLeaseError::NotPresented)?;
        if frame.key.window_id != context.window || frame.tracks.as_ref() != context.tracks {
            return Err(LayoutLeaseError::Identity);
        }
        let sample = frame.layout.ok_or(LayoutLeaseError::Identity)?;
        if !sample.same_source_as(context.source)
            || sample.viewport() != context.viewport
            || !sample
                .driver_nodes()
                .eq(context.tracks.iter().map(|track| track.node.0))
        {
            return Err(LayoutLeaseError::Identity);
        }
        if !(geometry && sample.geometry_reconcilable())
            && !sample.layout_input_only()
            && !(editors && sample.geometry_reconcilable() && sample.has_native_payload())
        {
            return Err(LayoutLeaseError::ReconciliationRequired);
        }
        self.current = Some(LayoutLease {
            key: frame.key,
            sampled_at: frame.sampled_at,
            sample,
        });
        Ok(self.current.as_ref().expect("lease was installed"))
    }

    /// Whether a successful refresh still owns a sample; not authority for a later event batch.
    pub fn is_current(&self) -> bool {
        self.current.is_some()
    }
}

/// Blocks input after a refused lane projection until a newer native frame is confirmed.
#[derive(Debug, Clone, Copy)]
pub struct NativeInputRecovery {
    window: u64,
    surface: u64,
    ownership: u64,
    after_sequence: u64,
}
impl NativeInputRecovery {
    /// Captures the post-reclaim epoch and the last allocated submission sequence.
    pub fn new(window: u64, surface: u64, ownership: u64, after_sequence: u64) -> Self {
        Self {
            window,
            surface,
            ownership,
            after_sequence,
        }
    }
    /// Whether a resize or owner transfer requires a new native recovery fence.
    pub fn matches_epoch(self, window: u64, surface: u64, ownership: u64) -> bool {
        self.window == window && self.surface == surface && self.ownership == ownership
    }
    /// Requires the latest successful native commit to be compositor-confirmed.
    /// `None` refuses input, including when the latest native frame could not be committed.
    pub fn allows(
        self,
        sink: &PresentationSink,
        current_scene: u64,
        current_commit: Option<FrameKey>,
    ) -> bool {
        sink.last_presented().is_some_and(|(frame, _)| {
            current_commit == Some(frame.key)
                && frame.owner == super::FrameOwner::Ui
                && frame.key.window_id == self.window
                && frame.key.surface_generation == self.surface
                && frame.key.ownership_generation == self.ownership
                && frame.key.scene_generation == current_scene
                && frame.key.submitted_sequence > self.after_sequence
        })
    }
}
