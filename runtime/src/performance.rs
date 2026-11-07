// Modified by enzUI contributors. See FORK.md.
//! Opt-in, per-window input-to-presentation performance observations.
//!
//! Collection is disabled by default. The disabled path is one atomic load and
//! does not allocate. Enabling collection preallocates the bounded trace; the
//! fixed-size histograms never allocate or discard observations. Timestamps are
//! nanoseconds from one monotonic process-local origin and are only comparable
//! within one collection session.
//!
//! A successful GPU submission is deliberately not called "visible". Every
//! latency snapshot carries both the strongest [`PresentationQuality`] observed
//! and exact-quality input-latency histograms, so one stronger milestone cannot
//! relabel weaker samples. `Submitted` means exactly that; only a platform
//! integration with compositor feedback may report `CompositorPresented`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// A stable runtime window identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

/// Nanoseconds from a collector's monotonic origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

/// The timed part of a rendered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Rebuilding the application's declarative view.
    ViewRebuild,
    /// Diffing the new widget tree against retained state.
    TreeDiff,
    /// Computing widget layout.
    Layout,
    /// Drawing and preparing renderer primitives.
    DrawPrepare,
    /// Preparing GPU commands and submitting them.
    GpuSubmit,
}

impl Stage {
    const ALL: [Self; 5] = [
        Self::ViewRebuild,
        Self::TreeDiff,
        Self::Layout,
        Self::DrawPrepare,
        Self::GpuSubmit,
    ];

    fn index(self) -> usize {
        match self {
            Self::ViewRebuild => 0,
            Self::TreeDiff => 1,
            Self::Layout => 2,
            Self::DrawPrepare => 3,
            Self::GpuSubmit => 4,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::ViewRebuild => "view_rebuild",
            Self::TreeDiff => "tree_diff",
            Self::Layout => "layout",
            Self::DrawPrepare => "draw_prepare",
            Self::GpuSubmit => "gpu_submit",
        }
    }
}

/// Strength of the timestamp used as a frame milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PresentationQuality {
    /// Commands were accepted by the GPU queue; no surface result is known.
    Submitted,
    /// `SurfaceTexture::present` returned; compositor visibility is unknown.
    SurfacePresented,
    /// The platform compositor reported presentation.
    CompositorPresented,
}

impl PresentationQuality {
    const ALL: [Self; 3] = [
        Self::Submitted,
        Self::SurfacePresented,
        Self::CompositorPresented,
    ];

    fn index(self) -> usize {
        match self {
            Self::Submitted => 0,
            Self::SurfacePresented => 1,
            Self::CompositorPresented => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Submitted => "Submitted",
            Self::SurfacePresented => "SurfacePresented",
            Self::CompositorPresented => "CompositorPresented",
        }
    }
}

/// Fixed-memory logarithmic high-dynamic-range histogram.
///
/// Four buckets per power of two cover nanoseconds from zero through `u64::MAX`.
/// Percentiles are bucket upper bounds while [`HistogramSnapshot::max`] is the
/// exact observed maximum. The histogram never drops samples.
#[derive(Debug, Clone)]
pub struct HdrHistogram {
    buckets: [u64; 257],
    count: u64,
    max: u64,
}

impl Default for HdrHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; 257],
            count: 0,
            max: 0,
        }
    }
}

impl HdrHistogram {
    fn bucket(value: u64) -> usize {
        if value == 0 {
            return 0;
        }
        let power = 63 - value.leading_zeros() as usize;
        let base = 1_u64 << power;
        let quarter = (base / 4).max(1);
        let sub = ((value - base) / quarter).min(3) as usize;
        (1 + power * 4 + sub).min(256)
    }

    fn upper(bucket: usize) -> u64 {
        if bucket == 0 {
            return 0;
        }
        let nonzero = bucket - 1;
        let power = nonzero / 4;
        let sub = nonzero % 4;
        let base = 1_u64.checked_shl(power as u32).unwrap_or(u64::MAX);
        let quarter = (base / 4).max(1);
        base.saturating_add(quarter.saturating_mul((sub + 1) as u64))
            .saturating_sub(1)
    }

    /// Records one duration in nanoseconds.
    pub fn record(&mut self, nanos: u64) {
        self.buckets[Self::bucket(nanos)] = self.buckets[Self::bucket(nanos)].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.max = self.max.max(nanos);
    }

    fn percentile(&self, numerator: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = self.count.saturating_mul(numerator).div_ceil(100).max(1);
        let mut seen = 0_u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= target {
                return Self::upper(index).min(self.max);
            }
        }
        self.max
    }

    /// Returns a compact percentile snapshot.
    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            count: self.count,
            p50: self.percentile(50),
            p95: self.percentile(95),
            p99: self.percentile(99),
            max: self.max,
        }
    }
}

/// Percentiles from one bounded histogram, in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistogramSnapshot {
    /// Number of recorded values.
    pub count: u64,
    /// 50th percentile.
    pub p50: u64,
    /// 95th percentile.
    pub p95: u64,
    /// 99th percentile.
    pub p99: u64,
    /// Exact maximum.
    pub max: u64,
}

impl HistogramSnapshot {
    fn json(self) -> String {
        format!(
            "{{\"count\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{}}}",
            self.count, self.p50, self.p95, self.p99, self.max
        )
    }
}

/// A bounded diagnostic trace event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    /// Window that produced the event.
    pub window: WindowId,
    /// Monotonic timestamp.
    pub at: Timestamp,
    /// Machine-readable event name.
    pub kind: &'static str,
    /// Optional duration.
    pub duration_ns: Option<u64>,
    /// Number of coalesced events/invalidations represented.
    pub count: u64,
    /// Whether this event invalidated or drew new content.
    pub changed: bool,
    /// Timestamp quality for milestone events.
    pub quality: Option<PresentationQuality>,
}

/// Memory and retention limits for a collector.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Maximum retained trace events. Histograms remain complete after wrap.
    pub trace_capacity: usize,
    /// Maximum windows retained in one session.
    pub max_windows: usize,
}

impl Config {
    /// Upper bound for preallocated collector payload bytes.
    ///
    /// This includes trace entries and fixed histogram/window state. The
    /// allocator may add its own bookkeeping around the two backing buffers.
    pub const fn payload_memory_cap_bytes(self) -> usize {
        self.trace_capacity
            .saturating_mul(std::mem::size_of::<TraceEvent>())
            .saturating_add(
                self.max_windows
                    .saturating_mul(std::mem::size_of::<(WindowId, WindowData)>()),
            )
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trace_capacity: 4096,
            max_windows: 64,
        }
    }
}

/// A per-window stage metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageSnapshot {
    /// Stage represented by this row.
    pub stage: Stage,
    /// Duration distribution.
    pub duration: HistogramSnapshot,
}

/// A point-in-time per-window performance report.
#[derive(Debug, Clone)]
pub struct WindowSnapshot {
    /// Window represented by this report.
    pub window: WindowId,
    /// Requested redraw count.
    pub requested_frames: u64,
    /// Frames whose UI was newly drawn.
    pub drawn_frames: u64,
    /// Successful frame milestones.
    pub milestone_frames: u64,
    /// Redraws skipped before a newly-drawn milestone.
    pub skipped_redraws: u64,
    /// Input events merged into the most recent newly-drawn frame.
    pub last_coalesced_inputs: u64,
    /// Invalidations merged into the most recent newly-drawn frame.
    pub last_coalesced_invalidations: u64,
    /// Time from event-loop arrival to input dispatch start.
    pub event_queue_wait: HistogramSnapshot,
    /// Input dispatch duration.
    pub input_dispatch: HistogramSnapshot,
    /// First invalidating input arrival to first newly-drawn milestone latency.
    /// This aggregate is retained for compatibility; visible-latency consumers
    /// must use [`Self::input_to_milestone_by_quality`] so a stronger sample
    /// cannot relabel weaker samples.
    pub input_to_milestone: HistogramSnapshot,
    /// The same input latency split by exact milestone quality, in
    /// `Submitted`, `SurfacePresented`, `CompositorPresented` order.
    pub input_to_milestone_by_quality: [HistogramSnapshot; 3],
    /// First invalidation to first newly-drawn milestone latency.
    pub invalidation_to_milestone: HistogramSnapshot,
    /// Intervals between newly-drawn frame milestones.
    pub frame_interval: HistogramSnapshot,
    /// Per-stage duration distributions.
    pub stages: Vec<StageSnapshot>,
    /// Strongest milestone quality observed. This is descriptive only; use
    /// `input_to_milestone_by_quality` for latency claims because a window may
    /// contain mixed-quality samples.
    pub presentation_quality: Option<PresentationQuality>,
    /// Retained bounded trace, oldest first.
    pub trace: Vec<TraceEvent>,
    /// Trace events evicted by ring wrap.
    pub dropped_trace_events: u64,
    /// Configured trace capacity.
    pub trace_capacity: usize,
    /// Configured maximum retained windows.
    pub max_windows: usize,
    /// Events refused after the window bound was reached.
    pub dropped_window_events: u64,
    /// Preallocated collector payload memory cap in bytes.
    pub payload_memory_cap_bytes: usize,
}

impl WindowSnapshot {
    /// Serializes this snapshot as one JSON object.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"window_id\":{},\"requested_frames\":{},\"drawn_frames\":{},\"milestone_frames\":{},\"skipped_redraws\":{},\"last_coalesced_inputs\":{},\"last_coalesced_invalidations\":{},\"presentation_quality\":{},\"event_queue_wait\":{},\"input_dispatch\":{},\"input_to_milestone\":{},\"input_to_milestone_by_quality\":{{\"Submitted\":{},\"SurfacePresented\":{},\"CompositorPresented\":{}}},\"invalidation_to_milestone\":{},\"frame_interval\":{},\"stages\":{{",
            self.window.0,
            self.requested_frames,
            self.drawn_frames,
            self.milestone_frames,
            self.skipped_redraws,
            self.last_coalesced_inputs,
            self.last_coalesced_invalidations,
            self.presentation_quality
                .map(|q| format!("\"{}\"", q.name()))
                .unwrap_or_else(|| "null".into()),
            self.event_queue_wait.json(),
            self.input_dispatch.json(),
            self.input_to_milestone.json(),
            self.input_to_milestone_by_quality[PresentationQuality::Submitted.index()].json(),
            self.input_to_milestone_by_quality[PresentationQuality::SurfacePresented.index()]
                .json(),
            self.input_to_milestone_by_quality[PresentationQuality::CompositorPresented.index()]
                .json(),
            self.invalidation_to_milestone.json(),
            self.frame_interval.json(),
        );
        for (index, stage) in self.stages.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{}\":{}", stage.stage.name(), stage.duration.json());
        }
        let _ = write!(
            out,
            "}},\"trace_capacity\":{},\"max_windows\":{},\"payload_memory_cap_bytes\":{},\"dropped_trace_events\":{},\"dropped_window_events\":{},\"trace\":[",
            self.trace_capacity,
            self.max_windows,
            self.payload_memory_cap_bytes,
            self.dropped_trace_events,
            self.dropped_window_events
        );
        for (index, event) in self.trace.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"window_id\":{},\"at_ns\":{},\"kind\":\"{}\",\"duration_ns\":{},\"count\":{},\"changed\":{},\"quality\":{}}}",
                event.window.0,
                event.at.0,
                event.kind,
                event
                    .duration_ns
                    .map_or_else(|| "null".into(), |v| v.to_string()),
                event.count,
                event.changed,
                event
                    .quality
                    .map(|q| format!("\"{}\"", q.name()))
                    .unwrap_or_else(|| "null".into()),
            );
        }
        out.push_str("]}");
        out
    }
}

#[derive(Debug, Default)]
struct Pending {
    first_arrival: Option<Timestamp>,
    arrived_events: u64,
    dispatch_start: Option<Timestamp>,
    dispatch_events: u64,
    first_input: Option<Timestamp>,
    inputs: u64,
    first_invalidation: Option<Timestamp>,
    invalidations: u64,
}

#[derive(Debug, Default)]
struct WindowData {
    requested: u64,
    drawn: u64,
    milestones: u64,
    skipped: u64,
    last_inputs: u64,
    last_invalidations: u64,
    event_queue_wait: HdrHistogram,
    input_dispatch: HdrHistogram,
    input_latency: HdrHistogram,
    input_latency_by_quality: [HdrHistogram; 3],
    invalidation_latency: HdrHistogram,
    frame_interval: HdrHistogram,
    stages: [HdrHistogram; 5],
    last_new_frame: Option<Timestamp>,
    quality: Option<PresentationQuality>,
    pending: Pending,
}

/// Synthetic-clock-friendly collector used by the live observer and tests.
#[derive(Debug)]
pub struct Collector {
    config: Config,
    windows: Vec<(WindowId, WindowData)>,
    trace: VecDeque<TraceEvent>,
    dropped_trace_events: u64,
    dropped_window_events: u64,
}

impl Collector {
    /// Creates an empty collector and preallocates its bounded trace.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            windows: Vec::with_capacity(config.max_windows),
            trace: VecDeque::with_capacity(config.trace_capacity),
            dropped_trace_events: 0,
            dropped_window_events: 0,
        }
    }

    fn window(&mut self, window: WindowId) -> Option<&mut WindowData> {
        if let Some(index) = self.windows.iter().position(|(id, _)| *id == window) {
            return Some(&mut self.windows[index].1);
        }
        if self.windows.len() == self.config.max_windows {
            self.dropped_window_events = self.dropped_window_events.saturating_add(1);
            return None;
        }
        self.windows.push((window, WindowData::default()));
        self.windows.last_mut().map(|(_, data)| data)
    }

    fn push(&mut self, event: TraceEvent) {
        if self.config.trace_capacity == 0 {
            self.dropped_trace_events = self.dropped_trace_events.saturating_add(1);
            return;
        }
        if self.trace.len() == self.config.trace_capacity {
            let _ = self.trace.pop_front();
            self.dropped_trace_events = self.dropped_trace_events.saturating_add(1);
        }
        self.trace.push_back(event);
    }

    /// Records input arrival in the event queue.
    pub fn input_arrived(&mut self, window: WindowId, at: Timestamp, event_count: u64) {
        let Some(data) = self.window(window) else {
            return;
        };
        let _ = data.pending.first_arrival.get_or_insert(at);
        data.pending.arrived_events = data.pending.arrived_events.saturating_add(event_count);
        self.push(TraceEvent {
            window,
            at,
            kind: "input_arrived",
            duration_ns: None,
            count: event_count,
            changed: false,
            quality: None,
        });
    }

    /// Starts dispatch of `event_count` input events.
    pub fn input_start(&mut self, window: WindowId, at: Timestamp, event_count: u64) {
        let Some(data) = self.window(window) else {
            return;
        };
        if let Some(arrived) = data.pending.first_arrival {
            data.event_queue_wait.record(at.0.saturating_sub(arrived.0));
        }
        data.pending.dispatch_start = Some(at);
        data.pending.dispatch_events = event_count;
        self.push(TraceEvent {
            window,
            at,
            kind: "input_start",
            duration_ns: None,
            count: event_count,
            changed: false,
            quality: None,
        });
    }

    /// Ends input dispatch and attributes only invalidating input to latency.
    pub fn input_end(&mut self, window: WindowId, at: Timestamp, invalidated: bool) {
        let Some(data) = self.window(window) else {
            return;
        };
        let start = data.pending.dispatch_start.take().unwrap_or(at);
        let count = std::mem::take(&mut data.pending.dispatch_events);
        let duration = at.0.saturating_sub(start.0);
        data.input_dispatch.record(duration);
        let arrival = data.pending.first_arrival.take().unwrap_or(start);
        let arrived_count = std::mem::take(&mut data.pending.arrived_events);
        if invalidated {
            let _ = data.pending.first_input.get_or_insert(arrival);
            data.pending.inputs = data.pending.inputs.saturating_add(count.max(arrived_count));
        }
        self.push(TraceEvent {
            window,
            at,
            kind: "input_end",
            duration_ns: Some(duration),
            count,
            changed: invalidated,
            quality: None,
        });
    }

    /// Records one or more invalidations/wakes coalesced toward a frame.
    pub fn invalidate(&mut self, window: WindowId, at: Timestamp, count: u64) {
        let Some(data) = self.window(window) else {
            return;
        };
        let _ = data.pending.first_invalidation.get_or_insert(at);
        data.pending.invalidations = data.pending.invalidations.saturating_add(count);
        self.push(TraceEvent {
            window,
            at,
            kind: "invalidate",
            duration_ns: None,
            count,
            changed: true,
            quality: None,
        });
    }

    /// Records a completed frame stage.
    pub fn stage(&mut self, window: WindowId, stage: Stage, start: Timestamp, end: Timestamp) {
        let duration = end.0.saturating_sub(start.0);
        let Some(data) = self.window(window) else {
            return;
        };
        data.stages[stage.index()].record(duration);
        self.push(TraceEvent {
            window,
            at: end,
            kind: stage.name(),
            duration_ns: Some(duration),
            count: 1,
            changed: true,
            quality: None,
        });
    }

    /// Records arrival of a redraw request.
    pub fn frame_requested(&mut self, window: WindowId, at: Timestamp) {
        let Some(data) = self.window(window) else {
            return;
        };
        data.requested = data.requested.saturating_add(1);
        self.push(TraceEvent {
            window,
            at,
            kind: "frame_requested",
            duration_ns: None,
            count: 1,
            changed: false,
            quality: None,
        });
    }

    /// Records production of new draw primitives.
    pub fn frame_drawn(&mut self, window: WindowId, at: Timestamp) {
        let Some(data) = self.window(window) else {
            return;
        };
        data.drawn = data.drawn.saturating_add(1);
        self.push(TraceEvent {
            window,
            at,
            kind: "frame_drawn",
            duration_ns: None,
            count: 1,
            changed: true,
            quality: None,
        });
    }

    /// Records a no-op or failed redraw that produced no new milestone.
    pub fn frame_skipped(&mut self, window: WindowId, at: Timestamp) {
        let Some(data) = self.window(window) else {
            return;
        };
        data.skipped = data.skipped.saturating_add(1);
        self.push(TraceEvent {
            window,
            at,
            kind: "frame_skipped",
            duration_ns: None,
            count: 1,
            changed: false,
            quality: None,
        });
    }

    /// Records the strongest available milestone for a frame.
    ///
    /// `newly_drawn = false` never contributes latency or pacing samples.
    pub fn milestone(
        &mut self,
        window: WindowId,
        at: Timestamp,
        newly_drawn: bool,
        quality: PresentationQuality,
    ) {
        let Some(data) = self.window(window) else {
            return;
        };
        data.quality = Some(data.quality.map_or(quality, |old| old.max(quality)));
        if newly_drawn {
            data.milestones = data.milestones.saturating_add(1);
            if let Some(start) = data.pending.first_input.take() {
                let latency = at.0.saturating_sub(start.0);
                data.input_latency.record(latency);
                data.input_latency_by_quality[quality.index()].record(latency);
            }
            if let Some(start) = data.pending.first_invalidation.take() {
                data.invalidation_latency
                    .record(at.0.saturating_sub(start.0));
            }
            if let Some(previous) = data.last_new_frame.replace(at) {
                data.frame_interval.record(at.0.saturating_sub(previous.0));
            }
            data.last_inputs = std::mem::take(&mut data.pending.inputs);
            data.last_invalidations = std::mem::take(&mut data.pending.invalidations);
        }
        self.push(TraceEvent {
            window,
            at,
            kind: "frame_milestone",
            duration_ns: None,
            count: 1,
            changed: newly_drawn,
            quality: Some(quality),
        });
    }

    /// Returns one window's report without disturbing collection.
    pub fn snapshot(&self, window: WindowId) -> Option<WindowSnapshot> {
        let data = &self.windows.iter().find(|(id, _)| *id == window)?.1;
        Some(WindowSnapshot {
            window,
            requested_frames: data.requested,
            drawn_frames: data.drawn,
            milestone_frames: data.milestones,
            skipped_redraws: data.skipped,
            last_coalesced_inputs: data.last_inputs,
            last_coalesced_invalidations: data.last_invalidations,
            event_queue_wait: data.event_queue_wait.snapshot(),
            input_dispatch: data.input_dispatch.snapshot(),
            input_to_milestone: data.input_latency.snapshot(),
            input_to_milestone_by_quality: PresentationQuality::ALL
                .map(|quality| data.input_latency_by_quality[quality.index()].snapshot()),
            invalidation_to_milestone: data.invalidation_latency.snapshot(),
            frame_interval: data.frame_interval.snapshot(),
            stages: Stage::ALL
                .into_iter()
                .map(|stage| StageSnapshot {
                    stage,
                    duration: data.stages[stage.index()].snapshot(),
                })
                .collect(),
            presentation_quality: data.quality,
            trace: self
                .trace
                .iter()
                .filter(|event| event.window == window)
                .cloned()
                .collect(),
            dropped_trace_events: self.dropped_trace_events,
            trace_capacity: self.config.trace_capacity,
            max_windows: self.config.max_windows,
            dropped_window_events: self.dropped_window_events,
            payload_memory_cap_bytes: self.config.payload_memory_cap_bytes(),
        })
    }

    /// Serializes all windows as newline-delimited JSON.
    pub fn to_ndjson(&self) -> String {
        let mut out = String::new();
        for (window, _) in &self.windows {
            if let Some(snapshot) = self.snapshot(*window) {
                out.push_str(&snapshot.to_json());
                out.push('\n');
            }
        }
        out
    }
}

struct Global {
    origin: Instant,
    collector: Collector,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static GLOBAL: OnceLock<Mutex<Global>> = OnceLock::new();

/// Enables collection and starts a new monotonic session.
pub fn enable(config: Config) {
    let mutex = GLOBAL.get_or_init(|| {
        Mutex::new(Global {
            origin: Instant::now(),
            collector: Collector::new(config),
        })
    });
    let mut global = mutex.lock().expect("performance observer lock");
    *global = Global {
        origin: Instant::now(),
        collector: Collector::new(config),
    };
    ENABLED.store(true, Ordering::Release);
}

/// Disables collection. Existing snapshots remain available.
pub fn disable() {
    ENABLED.store(false, Ordering::Release);
}

/// Returns whether live collection is enabled.
#[inline]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[cold]
fn record(mut f: impl FnMut(&mut Collector, Timestamp)) {
    let Some(mutex) = GLOBAL.get() else {
        return;
    };
    let mut global = mutex.lock().expect("performance observer lock");
    let elapsed = global.origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    f(&mut global.collector, Timestamp(elapsed));
}

/// Receives every completed stage span individually (enzICED #2219).
///
/// The histograms above answer "how long does layout take?"; they cannot answer
/// "which stages did *this* frame run?", which is the whole isolation claim of
/// #2219. This hook is additive: it changes no [`Stage`], no snapshot and no
/// existing consumer, and it is called only while collection is enabled.
pub trait SpanObserver: Send + Sync + 'static {
    /// One completed stage span, with real monotonic endpoints.
    fn span(&self, window: WindowId, stage: Stage, start: Instant, end: Instant);
}

static SPANS: OnceLock<Mutex<Option<std::sync::Arc<dyn SpanObserver>>>> = OnceLock::new();

/// Installs the per-span observer. Replaces any previous one.
pub fn observe_spans(observer: std::sync::Arc<dyn SpanObserver>) {
    let slot = SPANS.get_or_init(|| Mutex::new(None));

    if let Ok(mut slot) = slot.lock() {
        *slot = Some(observer);
    }
}

/// Removes the per-span observer.
pub fn stop_observing_spans() {
    if let Some(slot) = SPANS.get()
        && let Ok(mut slot) = slot.lock()
    {
        *slot = None;
    }
}

fn span_observer() -> Option<std::sync::Arc<dyn SpanObserver>> {
    SPANS.get()?.lock().ok()?.clone()
}

/// A live stage span. Dropping it records the duration.
pub struct StageSpan {
    window: WindowId,
    stage: Stage,
    start: Timestamp,
    /// Real monotonic start, so a per-span observer joins against the same
    /// clock the presentation ledger uses (enzICED #2219).
    start_at: Instant,
}

impl Drop for StageSpan {
    fn drop(&mut self) {
        let window = self.window;
        let stage = self.stage;
        let start = self.start;
        record(|collector, end| collector.stage(window, stage, start, end));

        if let Some(observer) = span_observer() {
            observer.span(window, stage, self.start_at, Instant::now());
        }
    }
}

/// Starts a live stage span, or returns `None` on the cold disabled path.
#[inline]
pub fn begin_stage(window: WindowId, stage: Stage) -> Option<StageSpan> {
    if !is_enabled() {
        return None;
    }
    let mut start = Timestamp(0);
    record(|_, at| start = at);
    Some(StageSpan {
        window,
        stage,
        start,
        start_at: Instant::now(),
    })
}

/// Records live input arrival in the event queue.
#[inline]
pub fn input_arrived(window: WindowId, count: u64) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.input_arrived(window, at, count));
}

/// Records live input dispatch start.
#[inline]
pub fn input_start(window: WindowId, count: u64) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.input_start(window, at, count));
}

/// Records live input dispatch completion.
#[inline]
pub fn input_end(window: WindowId, invalidated: bool) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.input_end(window, at, invalidated));
}

/// Records a live invalidation/wake.
#[inline]
pub fn invalidate(window: WindowId, count: u64) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.invalidate(window, at, count));
}

/// Records a live requested frame.
#[inline]
pub fn frame_requested(window: WindowId) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.frame_requested(window, at));
}

/// Records a live newly-drawn frame.
#[inline]
pub fn frame_drawn(window: WindowId) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.frame_drawn(window, at));
}

/// Records a live skipped/no-op redraw.
#[inline]
pub fn frame_skipped(window: WindowId) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.frame_skipped(window, at));
}

/// Records a live frame milestone.
#[inline]
pub fn milestone(window: WindowId, newly_drawn: bool, quality: PresentationQuality) {
    if !is_enabled() {
        return;
    }
    record(|collector, at| collector.milestone(window, at, newly_drawn, quality));
}

/// Returns a live snapshot, including while collection is disabled.
pub fn snapshot(window: WindowId) -> Option<WindowSnapshot> {
    GLOBAL.get()?.lock().ok()?.collector.snapshot(window)
}

/// Returns all live snapshots as NDJSON.
pub fn to_ndjson() -> String {
    GLOBAL
        .get()
        .and_then(|global| {
            global
                .lock()
                .ok()
                .map(|global| global.collector.to_ndjson())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_clock_attributes_stages_coalescing_and_windows() {
        let mut c = Collector::new(Config {
            trace_capacity: 64,
            max_windows: 4,
        });
        let a = WindowId(1);
        let b = WindowId(2);
        c.input_arrived(a, Timestamp(5), 1);
        c.input_start(a, Timestamp(10), 1);
        c.input_end(a, Timestamp(12), true);
        c.invalidate(a, Timestamp(12), 1);
        c.input_arrived(a, Timestamp(13), 1);
        c.input_start(a, Timestamp(14), 1);
        c.input_end(a, Timestamp(15), true);
        c.invalidate(a, Timestamp(15), 2);
        c.stage(a, Stage::ViewRebuild, Timestamp(16), Timestamp(18));
        c.stage(a, Stage::TreeDiff, Timestamp(18), Timestamp(20));
        c.stage(a, Stage::Layout, Timestamp(20), Timestamp(27));
        c.stage(a, Stage::DrawPrepare, Timestamp(27), Timestamp(31));
        c.stage(a, Stage::GpuSubmit, Timestamp(31), Timestamp(36));
        c.milestone(a, Timestamp(40), true, PresentationQuality::Submitted);
        c.stage(b, Stage::Layout, Timestamp(100), Timestamp(111));
        c.milestone(
            b,
            Timestamp(120),
            true,
            PresentationQuality::SurfacePresented,
        );

        let sa = c.snapshot(a).unwrap();
        assert_eq!(sa.last_coalesced_inputs, 2);
        assert_eq!(sa.last_coalesced_invalidations, 3);
        assert_eq!(sa.event_queue_wait.count, 2);
        assert_eq!(sa.event_queue_wait.max, 5);
        assert_eq!(sa.input_to_milestone.max, 35);
        assert_eq!(sa.invalidation_to_milestone.max, 28);
        assert_eq!(
            sa.stages
                .iter()
                .map(|stage| stage.duration.count)
                .collect::<Vec<_>>(),
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(sa.stages[Stage::Layout.index()].duration.max, 7);
        assert_eq!(sa.stages[Stage::DrawPrepare.index()].duration.max, 4);
        assert_eq!(sa.stages[Stage::GpuSubmit.index()].duration.max, 5);
        assert_eq!(
            sa.presentation_quality,
            Some(PresentationQuality::Submitted)
        );
        let sb = c.snapshot(b).unwrap();
        assert_eq!(sb.input_to_milestone.count, 0);
        assert_eq!(sb.stages[Stage::Layout.index()].duration.max, 11);
        assert_eq!(
            sb.presentation_quality,
            Some(PresentationQuality::SurfacePresented)
        );
    }

    #[test]
    fn event_queue_wait_is_attributed_to_the_queue_not_dispatch() {
        // An event sits in the loop's queue for 100ns before dispatch begins,
        // then dispatch itself takes only 5ns. The wait must land in
        // event_queue_wait; dispatch must see only its own 5ns, never the
        // 105ns arrival-to-end span.
        let mut c = Collector::new(Config::default());
        let w = WindowId(3);
        c.input_arrived(w, Timestamp(0), 1);
        c.input_start(w, Timestamp(100), 1);
        c.input_end(w, Timestamp(105), true);
        c.milestone(w, Timestamp(130), true, PresentationQuality::Submitted);
        let s = c.snapshot(w).unwrap();
        assert_eq!(s.event_queue_wait.count, 1);
        assert_eq!(
            s.event_queue_wait.max, 100,
            "the 100ns queue wait must be recorded as queue wait"
        );
        assert_eq!(s.input_dispatch.count, 1);
        assert_eq!(
            s.input_dispatch.max, 5,
            "dispatch must be timed from input_start, not from arrival"
        );
        // input-to-milestone measures from arrival (0) to the frame (130), so
        // the queue wait is part of user-perceived latency, not dispatch cost.
        assert_eq!(s.input_to_milestone.max, 130);
    }

    #[test]
    fn input_latency_is_separated_by_exact_milestone_quality() {
        let mut c = Collector::new(Config::default());
        let w = WindowId(6);
        for (base, quality) in [
            (0, PresentationQuality::Submitted),
            (20, PresentationQuality::SurfacePresented),
        ] {
            c.input_arrived(w, Timestamp(base), 1);
            c.input_start(w, Timestamp(base + 1), 1);
            c.input_end(w, Timestamp(base + 2), true);
            c.milestone(w, Timestamp(base + 10), true, quality);
        }
        let s = c.snapshot(w).unwrap();
        assert_eq!(s.input_to_milestone.count, 2);
        assert_eq!(
            s.input_to_milestone_by_quality[PresentationQuality::Submitted.index()].count,
            1
        );
        assert_eq!(
            s.input_to_milestone_by_quality[PresentationQuality::SurfacePresented.index()].count,
            1
        );
        assert_eq!(
            s.input_to_milestone_by_quality[PresentationQuality::CompositorPresented.index()].count,
            0
        );
        assert_eq!(s.frame_interval.count, 1);
        assert_eq!(s.frame_interval.max, 20);
    }

    #[test]
    fn non_invalidating_input_and_represent_do_not_fake_samples() {
        let mut c = Collector::new(Config::default());
        let w = WindowId(7);
        c.input_arrived(w, Timestamp(0), 1);
        c.input_start(w, Timestamp(1), 1);
        c.input_end(w, Timestamp(5), false);
        c.milestone(w, Timestamp(10), true, PresentationQuality::Submitted);
        c.milestone(w, Timestamp(15), false, PresentationQuality::Submitted);
        c.milestone(w, Timestamp(30), true, PresentationQuality::Submitted);
        let s = c.snapshot(w).unwrap();
        assert_eq!(s.input_dispatch.count, 1);
        assert_eq!(s.input_to_milestone.count, 0);
        assert_eq!(s.frame_interval.count, 1);
        assert_eq!(
            s.frame_interval.max, 20,
            "the no-op at 15 must not improve pacing"
        );
    }

    #[test]
    fn trace_wrap_is_explicit_and_histograms_remain_complete() {
        let mut c = Collector::new(Config {
            trace_capacity: 3,
            max_windows: 1,
        });
        let w = WindowId(9);
        for n in 0..5 {
            c.stage(
                w,
                Stage::DrawPrepare,
                Timestamp(n * 10),
                Timestamp(n * 10 + 3),
            );
        }
        let s = c.snapshot(w).unwrap();
        assert_eq!(s.trace.len(), 3);
        assert_eq!(s.dropped_trace_events, 2);
        assert_eq!(s.stages[Stage::DrawPrepare.index()].duration.count, 5);
        assert!(s.to_json().contains("\"dropped_trace_events\":2"));
        assert_eq!(
            s.payload_memory_cap_bytes,
            Config {
                trace_capacity: 3,
                max_windows: 1
            }
            .payload_memory_cap_bytes()
        );

        c.frame_requested(WindowId(10), Timestamp(99));
        let s = c.snapshot(w).unwrap();
        assert_eq!(s.dropped_window_events, 1);
        assert!(c.snapshot(WindowId(10)).is_none());
        assert!(c.to_ndjson().ends_with('\n'));
    }
}
