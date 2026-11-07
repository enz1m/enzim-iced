// Modified by enzUI contributors. See FORK.md.
mod state;

use state::State;

pub use crate::core::window::{Event, Id, RedrawRequest, Settings};

use crate::Proxy;
use crate::conversion;
use crate::core;
use crate::core::alignment;
use crate::core::input_method;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::shell;
use crate::core::text;
use crate::core::theme;
use crate::core::time::Instant;
use crate::core::{Color, InputMethod, Padding, Point, Rectangle, Size, Text, Vector};
use crate::graphics::Compositor;
use crate::program::{self, Program};

use winit::dpi::{LogicalPosition, LogicalSize};
use winit::monitor::MonitorHandle;

use std::collections::BTreeMap;
use std::sync::Arc;

/// Removes a registered value before invoking its teardown callback.
///
/// The callback borrows the removed value, proving its lifetime guards remain
/// alive while external registrations are cleared. A missing key invokes
/// nothing. `Manager::remove` uses this for Wayland surface feedback teardown.
#[doc(hidden)]
pub fn remove_registered<K: Ord, V>(
    entries: &mut BTreeMap<K, V>,
    key: &K,
    unregister: impl FnOnce(&mut V),
) -> Option<V> {
    let mut value = entries.remove(key)?;
    unregister(&mut value);
    Some(value)
}

fn retire_scene(scene: &mut Option<(renderer::Recording, Size)>) {
    drop(scene.take());
}

pub struct Manager<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    aliases: BTreeMap<winit::window::WindowId, Id>,
    entries: BTreeMap<Id, Window<P, C>>,
    /// The per-display `wp_presentation` hub, created on the first window
    /// (enzICED #2219).
    ///
    /// One guest queue serves every window on the display; a session with no
    /// protocol answers `None` once and is never asked again, so a
    /// non-Wayland run costs one `Option` and every frame stays honestly
    /// `SurfacePresented`.
    hub: Option<Arc<crate::presentation::PresentationHub>>,
    hub_attempted: bool,
}

impl<P, C> Manager<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    pub fn new() -> Self {
        Self {
            aliases: BTreeMap::new(),
            entries: BTreeMap::new(),
            hub: None,
            hub_attempted: false,
        }
    }

    /// The compositor-feedback hub, if this display has one.
    pub fn presentation_hub(&self) -> Option<&Arc<crate::presentation::PresentationHub>> {
        self.hub.as_ref()
    }

    /// Creates the hub on first use from winit's own display handle.
    fn ensure_hub(
        &mut self,
        display: &winit::event_loop::OwnedDisplayHandle,
    ) -> Option<Arc<crate::presentation::PresentationHub>> {
        if !self.hub_attempted {
            self.hub_attempted = true;
            self.hub = crate::presentation::PresentationHub::new(display.clone());
        }

        self.hub.clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        id: Id,
        window: Arc<winit::window::Window>,
        display: &winit::event_loop::OwnedDisplayHandle,
        program: &program::Instance<P>,
        compositor: &mut C,
        proxy: Proxy<P::Message>,
        renderer_settings: renderer::Settings,
        exit_on_close_request: bool,
        system_theme: theme::Mode,
    ) -> &mut Window<P, C> {
        let state = State::new(program, id, &window, system_theme);
        let surface_size = state.physical_size();
        let surface_version = state.surface_version();
        let surface =
            compositor.create_surface(window.clone(), surface_size.width, surface_size.height);
        let mut renderer = compositor.create_renderer(renderer_settings);
        let material_window = Arc::downgrade(&window);
        let full_redraw = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let material_full_redraw = full_redraw.clone();
        core::Renderer::set_material_waker(
            &mut renderer,
            shell::Waker::new(move || {
                if let Some(window) = material_window.upgrade() {
                    material_full_redraw.store(true, std::sync::atomic::Ordering::Release);
                    window.request_redraw();
                }
            }),
        );

        let waker = shell::Waker::new(move || {
            proxy.send_action(iced_runtime::Action::Event {
                window: id,
                event: core::Event::Waken,
            });
        });

        // enzICED #2219: per-window compositor-feedback state. The requester
        // is `None` -- and every frame therefore stays `SurfacePresented` --
        // whenever the session is not Wayland, the compositor does not
        // advertise `wp_presentation`, or the surface proxy cannot be
        // reconstructed. It is never faked.
        let hub = self.ensure_hub(display);
        #[cfg(feature = "wayland")]
        let requester = hub.as_ref().and_then(|hub| {
            hub.surface_for(&window)
                .map(|surface| hub.requester(surface))
        });
        #[cfg(not(feature = "wayland"))]
        let requester = None;
        let presentation = Arc::new(crate::presentation::WindowPresentation::new(
            id.as_u64(),
            requester,
        ));

        if let Some(hub) = hub.as_ref()
            && !hub.register(id.as_u64(), presentation.clone())
        {
            presentation.shutdown();
        }

        // enzICED #2250: the LIVE installation of per-frame stage recording.
        //
        // #2219 shipped `Window::record_stages` and the runtime span bridge and
        // then never called either from the real winit path, so a live run
        // produced no joined stage rows while a test that drove the bridge by
        // hand stayed green. This is the one call that makes the instrumentation
        // real, and it is on the path every window goes through. It returns
        // `None` -- costing one atomic load -- unless a harness installed a
        // `StageRecorder` or `ICED_STAGE_LEDGER` is set.
        let stages = crate::presentation::attach_stage_ledger(id.as_u64());

        // Create the lane lazily so non-animating windows do not retain a second renderer and parked thread.
        let _ = self.aliases.insert(window.id(), id);

        let _ = self.entries.insert(
            id,
            Window {
                raw: window,
                waker,
                state,
                exit_on_close_request,
                surface: Some(surface),
                lane: None,
                lane_settings: renderer_settings,
                lane_attempted: false,
                lane_report: None,
                lane_disabled: false,
                presentation_quarantined: false,
                shows_ui_present: false,
                input_recovery: None,
                native_input_commit: None,
                input_relayout: false,
                lane_feedback_at: None,
                content_frame_at: None,
                ui_pacer: crate::graphics::presentation::FramePacer::new(),
                ui_sample: None,
                redraw_requested_at: None,
                content_redraw: false,
                pointer_hold: crate::pointer_hold::PointerHold::default(),
                full_redraw,
                ui_wake: None,
                ui_presented_sample: None,
                ui_repainting: false,
                ui_frame_at: None,
                ui_commit_cost: None,
                input_lease: crate::graphics::presentation::LayoutLeaseSlot::default(),
                active_tracks: Vec::new(),
                clock_restart_allowed: true,
                resize_hold_until: None,
                presentation,
                stages,
                scene_generation: 0,
                surface_version,
                surface_error_at: None,
                renderer,
                mouse_interaction: mouse::Interaction::None,
                cursor_grab: crate::core::window::CursorGrab::None,
                redraw_at: None,
                scene: None,
                #[cfg(feature = "a11y")]
                accessibility: None,
                preedit: None,
                ime_state: None,
                ime_surrounding: None,
            },
        );

        self.entries
            .get_mut(&id)
            .expect("Get window that was just inserted")
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_idle(&self) -> bool {
        self.entries
            .values()
            .all(|window| window.redraw_at.is_none())
    }

    pub fn redraw_at(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|window| window.redraw_at)
            .min()
    }

    pub fn first(&self) -> Option<&Window<P, C>> {
        self.entries.first_key_value().map(|(_id, window)| window)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Id, &mut Window<P, C>)> {
        self.entries.iter_mut().map(|(k, v)| (*k, v))
    }

    pub fn get(&self, id: Id) -> Option<&Window<P, C>> {
        self.entries.get(&id)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut Window<P, C>> {
        self.entries.get_mut(&id)
    }

    pub fn get_mut_alias(
        &mut self,
        id: winit::window::WindowId,
    ) -> Option<(Id, &mut Window<P, C>)> {
        let id = self.aliases.get(&id).copied()?;

        Some((id, self.get_mut(id)?))
    }

    pub fn last_monitor(&self) -> Option<MonitorHandle> {
        self.entries.values().last()?.raw.current_monitor()
    }

    pub fn remove(&mut self, id: Id) -> Option<Window<P, C>> {
        // Remove first so a nonexistent id cannot mutate hub state. The local
        // `window` keeps its raw Arc alive while unregister drops the
        // reconstructed WlSurface requester and aborts pending feedback.
        let hub = self.hub.as_ref();
        let window = remove_registered(&mut self.entries, &id, |window| {
            // Close feedback before retirement so even a quarantined presenter cannot register a late frame.
            window.shutdown_for_teardown();
            if let Some(hub) = hub {
                hub.unregister(id.as_u64());
            }
            // enzICED #2250: a closed window stops producing spans; leaving it
            // registered would file a reused id's spans into a dead ledger.
            crate::presentation::detach_stage_ledger(id.as_u64());
        })?;
        let _ = self.aliases.remove(&window.raw.id());

        Some(window)
    }

    pub fn replace_with(&mut self, mut f: impl FnMut(Window<P, C>) -> Window<P, C>) {
        self.entries = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|(id, window)| (id, f(window)))
            .collect();
    }
}

impl<P, C> Drop for Manager<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    fn drop(&mut self) {
        // Release every reconstructed surface proxy first, then stop and join
        // the guest queue while its OwnedDisplayHandle is still stored in the
        // hub. `PresentationHub::Drop` repeats shutdown idempotently as the
        // final backstop.
        if let Some(hub) = self.hub.as_ref() {
            for (id, window) in &mut self.entries {
                window.shutdown_for_teardown();
                hub.unregister(id.as_u64());
            }
        } else {
            for window in self.entries.values_mut() {
                window.shutdown_for_teardown();
            }
        }
        self.entries.clear();
        if let Some(hub) = self.hub.take() {
            hub.shutdown();
        }
    }
}

impl<P, C> Default for Manager<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    fn default() -> Self {
        Self::new()
    }
}

/// How long after the last resize the animation lane may present again.
pub const RESIZE_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether a resize at most [`RESIZE_SETTLE`] ago still keeps the surface with
/// the UI thread at `now`; `until` is the end of that hold.
fn resize_holds(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|until| now < until)
}

pub struct Window<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    /// Declared before `raw` so Rust's field drop order releases the
    /// reconstructed WlSurface requester while the raw winit window is still
    /// alive, even if a Window is dropped outside Manager teardown.
    pub presentation: Arc<crate::presentation::WindowPresentation>,
    pub raw: Arc<winit::window::Window>,
    pub waker: shell::Waker,
    pub state: State<P>,
    pub exit_on_close_request: bool,
    pub mouse_interaction: mouse::Interaction,
    /// How the window holds the pointer (enzICED).
    pub cursor_grab: crate::core::window::CursorGrab,
    // UI and lane never share the surface. Quarantine forbids creating a replacement for an unconfirmed owner.
    surface: Option<C::Surface>,
    /// The renderer-owned animation lane for this window (enzICED #2219).
    ///
    /// `None` when the backend has no lane -- the software/`softbuffer` path --
    /// in which case animation stays on the UI thread and is reported as such.
    pub lane: Option<crate::graphics::lane::Lane<C::Surface>>,
    /// The renderer settings the lane's own renderer is created with, kept so
    /// the lane can be spawned lazily on the first travel (enzICED #2219).
    lane_settings: renderer::Settings,
    /// Whether a lane was ever requested from the compositor for this window.
    ///
    /// A backend with no lane answers `None` once and is never asked again.
    lane_attempted: bool,
    /// What the lane reported the last time the surface came back.
    pub lane_report: Option<crate::graphics::lane::Report>,
    /// Set when the lane must never own this window's surface again
    /// (enzICED #2219).
    ///
    /// A lane that refused a replay or lost the surface is a lane whose next
    /// frame cannot be trusted. Animation stays on the UI thread from then on:
    /// slower, still correct, and never a wrong picture or a retry loop.
    pub lane_disabled: bool,
    presentation_quarantined: bool,
    /// The surface shows the UI thread's last present: nothing else has
    /// presented to it, and it has not been replaced or reconfigured since.
    /// Only then may a frame that changed nothing skip its present.
    shows_ui_present: bool,
    input_recovery: Option<crate::graphics::presentation::NativeInputRecovery>,
    input_relayout: bool,
    native_input_commit: Option<crate::graphics::presentation::FrameKey>,
    /// The last lane-presented instant this window has already handed to its
    /// widgets (enzICED #2219).
    ///
    /// The lane presents frames the UI thread never sees. When the UI thread
    /// next runs -- an input event, a settle -- the widgets must learn the
    /// instant of the frame the user is actually looking at, or a retarget or
    /// reversal anchors at the UI thread's stale sample and jumps backwards.
    lane_feedback_at: Option<Instant>,
    /// The vblank the last content frame drawn for the lane was woken for.
    content_frame_at: Option<Instant>,
    /// Predicts the vblank each UI-presented frame lands on (enzUI-ICED
    /// #3082), so `RedrawRequested` carries the instant the frame will be
    /// shown, not the instant the event loop woke.
    ui_pacer: crate::graphics::presentation::FramePacer,
    /// The instant the redraw in progress sampled, committed to `ui_pacer`
    /// only when its frame is presented.
    ui_sample: Option<Instant>,
    /// The first redraw request since the last UI-presented frame, and the
    /// wake that served it, for the `ui_frame` trace (enzUI-ICED #3396).
    redraw_requested_at: Option<Instant>,
    /// A content redraw ([`core::Shell::request_content_redraw`]) is pending
    /// (enzUI-ICED #3442).
    content_redraw: bool,
    /// The buttons the pointer holds, and a leave held back meanwhile.
    pub(crate) pointer_hold: crate::pointer_hold::PointerHold,
    /// Some other redraw is pending: the next frame must be drawn in full.
    /// Shared with the material waker, which requests frames from elsewhere.
    full_redraw: Arc<std::sync::atomic::AtomicBool>,
    ui_wake: Option<Instant>,
    /// The instant the last frame the UI thread presented was laid out for.
    ui_presented_sample: Option<Instant>,
    /// The latest instant a UI frame was sampled for.
    ui_frame_at: Option<Instant>,
    /// Whether the frame in progress repaints what moves, so the UI thread
    /// commits the frames after it too (enzUI-ICED #3660).
    ui_repainting: bool,
    /// How long the UI thread recently took from waking for a frame to
    /// committing it to the running lane (enzUI-ICED #3660).
    ui_commit_cost: Option<std::time::Duration>,
    input_lease: crate::graphics::presentation::LayoutLeaseSlot,
    /// The `(node, generation)` set of the travel currently published to the
    /// lane (enzICED #2219).
    ///
    /// A retarget replaces the travel and changes **only** these generations:
    /// the surface is the same, the owner is the same, and a retarget that
    /// publishes just a new curve records no new scene. So without this, a
    /// compositor callback for a frame submitted *before* the retarget -- which
    /// may legitimately be presented *after* it -- would anchor the replacement
    /// travel at the previous travel's instant and jump the picture. It is
    /// recorded by the same calls that hand animations to the lane, so it can
    /// never disagree with what the lane is sampling.
    ///
    /// It is **not** cleared when the lane settles or hands the surface back:
    /// after a settle, the last presented frames still describe that travel,
    /// and a reversal triggered by the next click must be able to anchor on
    /// them.
    active_tracks: Vec<crate::graphics::presentation::TrackGeneration>,
    // Active retargets retain chronology; only initial or settled ownership may defer a fresh travel.
    clock_restart_allowed: bool,
    /// Until when a resize keeps the surface with the UI thread; see
    /// [`Self::note_resized`].
    resize_hold_until: Option<Instant>,
    /// Per-frame UI-thread stage ledger, installed only when a harness asked
    /// for stage evidence (enzICED #2219).
    ///
    /// `None` by default: an ordinary build pays one `Option` check per frame
    /// and allocates nothing. The same ledger is handed to this window's lane,
    /// so the six UI stages and the four render-lane stages land in one place
    /// and stay separable by their owner.
    stages: Option<Arc<crate::graphics::presentation::StageLedger>>,
    /// Bumped only when a newly RECORDED scene is installed (enzICED #2219).
    ///
    /// Not on every commit and not on the worker's receipt of a command: a
    /// delayed or reordered commit must carry the producer's own generation,
    /// or a feedback join could be satisfied by a scene the frame never
    /// contained.
    pub scene_generation: u64,
    pub surface_version: u64,
    /// When a surface last failed with `SurfaceError::Other` and was
    /// recreated. Upstream iced #3478 recovers such a surface at most once a
    /// second, so a surface that keeps failing cannot spin the event loop.
    pub surface_error_at: Option<Instant>,
    pub renderer: P::Renderer,
    pub redraw_at: Option<Instant>,
    /// The last scene this window committed, and the viewport it was committed
    /// at (enzICED #2219).
    ///
    /// An animation-only frame is served by replaying this instead of walking
    /// the widget tree. It is dropped whenever it can no longer be trusted --
    /// see `Window::invalidate_scene`.
    pub scene: Option<(core::renderer::Recording, Size)>,
    /// AccessKit adapter + collector plumbing for this window. Set right after
    /// the window is inserted (created in the `Runner` while still invisible).
    /// `None` unless the `a11y` feature is on and a tree builder is registered.
    #[cfg(feature = "a11y")]
    pub accessibility: Option<crate::accessibility::WindowAccessibility>,
    preedit: Option<Preedit<P::Renderer>>,
    ime_state: Option<(Rectangle, input_method::Purpose)>,
    /// The text around the cursor last told to the input method.
    ime_surrounding: Option<input_method::Surrounding>,
}

impl<P, C> Window<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    /// Takes the surface from the animation lane on a resize, and keeps it with
    /// the UI thread until the size has held for [`RESIZE_SETTLE`].
    ///
    /// The lane replays the scene recorded at the old size. Presenting it after
    /// the compositor asked for a new size commits a buffer of the old size, so
    /// during an interactive resize the window alternated between the two
    /// sizes and its content jumped back and forth.
    pub fn note_resized(&mut self, now: Instant) {
        let _ = self.reclaim_surface();
        self.resize_hold_until = Some(now + RESIZE_SETTLE);
    }

    /// Takes the surface back from the animation lane, blocking until the lane
    /// has finished the frame it is presenting (enzICED #2219).
    ///
    /// This is the ownership handshake: after it returns, the UI thread owns
    /// the surface again and the lane presents nothing. It is a no-op when the
    /// lane is idle, which is the common case.
    pub fn reclaim_surface(&mut self) -> Option<crate::graphics::lane::Report> {
        self.revoke_presented_input();
        let lane = self.lane.as_mut()?;

        if !lane.holds_surface() {
            return None;
        }

        match lane.reclaim() {
            Some((surface, report)) => {
                // A successful lane -> UI transfer (enzICED #2219).
                let _ = self.presentation.transfer_ownership();
                self.surface = Some(surface);
                self.shows_ui_present = false;
                self.clock_restart_allowed =
                    matches!(report.stop, crate::graphics::lane::Stop::Settled);
                self.lane_report = Some(report.clone());
                Some(report)
            }
            None => {
                self.presentation_quarantined = true;
                self.lane_disabled = true;
                self.presentation.shutdown();
                self.lane = None;
                self.surface = None;
                retire_scene(&mut self.scene);
                None
            }
        }
    }

    // This window's surface, reclaiming it from the lane if needed.
    //
    // Returns `None` when ownership is unconfirmed. Quarantined windows must not recreate it.
    pub fn surface_mut(&mut self) -> Option<&mut C::Surface> {
        let _ = self.reclaim_surface();
        self.surface.as_mut()
    }

    // Reports unconfirmed native ownership; a replacement surface must not be created.
    pub fn presentation_quarantined(&self) -> bool {
        self.presentation_quarantined
    }

    // Installs a replacement only while no unconfirmed backend owns this window.
    pub fn set_surface(&mut self, surface: C::Surface) {
        let _ = self.reclaim_surface();
        if !self.presentation_quarantined {
            self.surface = Some(surface);
            self.shows_ui_present = false;
        }
    }

    pub fn prepare_widget_input(&mut self, id: Id) -> bool {
        if self.presentation_quarantined {
            return false;
        }
        if let Some(recovery) = self.input_recovery {
            if !recovery.matches_epoch(
                id.as_u64(),
                self.presentation.surface_generation(),
                self.presentation.ownership_generation(),
            ) {
                self.input_recovery =
                    Some(crate::graphics::presentation::NativeInputRecovery::new(
                        id.as_u64(),
                        self.presentation.surface_generation(),
                        self.presentation.ownership_generation(),
                        self.presentation.submitted_sequence(),
                    ));
                self.input_relayout = true;
                self.request_full_redraw();
                return false;
            }
            if !recovery.allows(
                self.presentation.sink(),
                self.scene_generation,
                self.native_input_commit,
            ) {
                return false;
            }
            self.input_recovery = None;
        }
        let needs_projection = self.needs_presented_input();
        if self.prepare_presented_input(id) || !needs_projection {
            return true;
        }
        self.refuse_native_reconciliation(id);
        false
    }

    fn needs_presented_input(&self) -> bool {
        self.lane
            .as_ref()
            .is_some_and(crate::graphics::lane::Lane::holds_surface)
            && self
                .scene
                .as_ref()
                .is_some_and(|(scene, _)| scene.presentation().is_some())
    }

    #[cfg(feature = "a11y")]
    pub(crate) fn prepare_accessibility_input(&mut self, id: Id) -> bool {
        use crate::core::Renderer as _;
        if self.presentation_quarantined {
            return false;
        }
        if self.needs_presented_input() {
            self.revoke_presented_input();
            if self.input_recovery.is_some() {
                return false;
            }
            let viewport = self.state.logical_size();
            let Some((recording, size)) = &self.scene else {
                return false;
            };
            if *size != viewport {
                return false;
            }
            let Some((_, source)) = self
                .presentation
                .presented_layout(self.scene_generation, &self.active_tracks)
            else {
                return false;
            };
            let mut slot = crate::graphics::presentation::LayoutLeaseSlot::default();
            let context = crate::graphics::presentation::LayoutLeaseContext {
                window: id.as_u64(),
                surface: self.presentation.surface_generation(),
                ownership: self.presentation.ownership_generation(),
                scene: self.scene_generation,
                viewport: [viewport.width, viewport.height],
                source: &source,
                tracks: &self.active_tracks,
            };
            let Ok(lease) = slot.refresh_for_geometry(self.presentation.sink(), context) else {
                return false;
            };
            self.renderer
                .install_presented_geometry(recording, lease.sample().clone())
        } else {
            self.prepare_widget_input(id)
        }
    }

    pub fn refuse_native_reconciliation(&mut self, id: Id) {
        self.revoke_presented_input();
        let _ = self.reclaim_surface();
        self.invalidate_scene();
        self.input_recovery = Some(crate::graphics::presentation::NativeInputRecovery::new(
            id.as_u64(),
            self.presentation.surface_generation(),
            self.presentation.ownership_generation(),
            self.presentation.submitted_sequence(),
        ));
        self.input_relayout = true;
        self.request_full_redraw();
    }

    pub fn begin_native_update(&mut self, id: Id) {
        if self.input_recovery.is_some_and(|recovery| {
            recovery.matches_epoch(
                id.as_u64(),
                self.presentation.surface_generation(),
                self.presentation.ownership_generation(),
            ) && recovery.allows(
                self.presentation.sink(),
                self.scene_generation,
                self.native_input_commit,
            )
        }) {
            self.input_recovery = None;
        }
        self.native_input_commit = None;
    }

    pub fn take_input_relayout(&mut self) -> bool {
        std::mem::take(&mut self.input_relayout)
    }

    pub fn prepare_presented_input(&mut self, id: Id) -> bool {
        use crate::core::Renderer as _;
        use crate::graphics::presentation::LayoutLeaseContext;
        self.revoke_presented_input();
        let viewport = self.state.logical_size();
        let Some((recording, size)) = &self.scene else {
            return false;
        };
        if *size != viewport || recording.presentation().is_none() {
            return false;
        }
        let Some((_, sample)) = self
            .presentation
            .presented_layout(self.scene_generation, &self.active_tracks)
        else {
            return false;
        };
        let context = LayoutLeaseContext {
            window: id.as_u64(),
            surface: self.presentation.surface_generation(),
            ownership: self.presentation.ownership_generation(),
            scene: self.scene_generation,
            viewport: [viewport.width, viewport.height],
            source: &sample,
            tracks: &self.active_tracks,
        };
        let sample = match self
            .input_lease
            .refresh_for_native_reconciliation(self.presentation.sink(), context)
        {
            Ok(lease) => lease.sample().clone(),
            Err(_) => return false,
        };
        if !self.renderer.install_presented_layout(recording, sample) {
            self.input_lease.invalidate();
            return false;
        }
        true
    }

    // Revokes previously validated input data before ownership or native geometry changes.
    pub fn revoke_presented_input(&mut self) {
        use crate::core::Renderer as _;
        self.input_lease.invalidate();
        self.renderer.clear_presented_layout();
    }

    /// The instant a redraw woken at `wake` samples: the vblank its frame is
    /// predicted to be shown on, from the compositor's presentation feedback,
    /// or `wake` without it (enzUI-ICED #3082).
    ///
    /// Layout and draw both see this one instant, as `RedrawRequested(at)`.
    pub fn redraw_instant(&mut self, wake: Instant) -> Instant {
        let grid = self.presentation.presentation_grid();
        let lane_cost = self
            .is_lane_running()
            .then_some(self.ui_commit_cost.unwrap_or_default());
        let at = ui_frame_target(&mut self.ui_pacer, wake, grid, lane_cost, self.ui_frame_at);
        self.ui_sample = Some(at);
        self.ui_frame_at = Some(at);
        self.ui_wake = Some(wake);
        at
    }

    /// When the UI thread should next run for content it draws while the lane
    /// presents: the next vblank on the presentation grid, or 60 Hz without
    /// one. The lane's own cadence is no measure of it: the lane
    /// presents only what the UI hands it, so pacing on it slows both.
    ///
    /// A wake still pending is the answer to every request until it fires, and
    /// the one after it is at least half a frame later, so neither repeated
    /// requests nor an early wake run two frames for one vblank.
    pub fn next_content_frame(&mut self, now: Instant) -> Instant {
        let grid = self.presentation.presentation_grid();
        if let Some(pending) = self.content_frame_at
            && pending > now
        {
            return pending;
        }
        let period = grid.map_or(std::time::Duration::from_micros(16_667), |grid| grid.period);
        let after = self
            .content_frame_at
            .map_or(now, |fired| now.max(fired + period / 2));
        let next = grid.map_or(after + period, |grid| grid.next_after(after));
        self.content_frame_at = Some(next);
        next
    }

    /// The instant of a lane-presented frame the widgets have not been told
    /// about yet, if any (enzICED #2219).
    ///
    /// Returns `Some(at)` at most once per lane-presented frame: the runtime
    /// delivers it as a real frame instant before the events of the same batch,
    /// so `Motion`-style effects resume from the value the LANE presented.
    /// Returns `None` when the lane never presented, or when the widgets are
    /// already up to date.
    pub fn take_lane_feedback(&mut self) -> Option<Instant> {
        // enzICED #2219: prefer the instant of a frame the COMPOSITOR
        // confirmed it showed. `Lane::last_presented_at` is only the moment
        // `SurfaceTexture::present()` returned, which is a submission; a frame
        // the compositor discarded never reaches the branch below at all.
        //
        // With no protocol this falls back to the submission milestone, which
        // is exactly what it always was -- weaker, and labelled as such.
        let at = if self.presentation.has_protocol() {
            // Once compositor feedback exists, only a callback-confirmed frame
            // may steer a reversal. Falling back here to the worker's submit
            // timestamp would let an unconfirmed or stale-generation picture
            // become the user's alleged last-visible value.
            //
            // The active track generations are part of the question, not
            // decoration: a retarget changes them and nothing else, so a
            // callback for the frame before the retarget must not anchor the
            // travel after it (enzICED #2219).
            self.presentation
                .last_presented_at(self.scene_generation, &self.active_tracks)?
        } else {
            // Honest weaker fallback for X11 or a Wayland compositor without
            // `wp_presentation`: submission is the strongest available event.
            self.lane.as_ref()?.last_presented_at()?
        };

        if self.lane_feedback_at == Some(at) {
            return None;
        }

        self.lane_feedback_at = Some(at);

        // A lane frame holding a commit the UI thread has already moved past
        // (enzUI-ICED #3660) is no instant to resume from: the widgets would
        // step back in time and lay out an older frame.
        if self.ui_frame_at.is_some_and(|latest| at < latest) {
            return None;
        }

        Some(at)
    }

    /// Records the `(node, generation)` set of the travel being handed to the
    /// lane (enzICED #2219).
    ///
    /// Derived exactly as the lane derives the generations it reports for a
    /// presented frame: a published track whose node has no live animation
    /// behind it is generation `0`, explicitly, so "static" and "missing" stay
    /// distinguishable on both sides of the comparison.
    ///
    /// A publication with no tracks at all leaves the previous set in place: it
    /// describes no travel, and replacing a real travel with nothing would
    /// refuse every anchor for the travel that is still running.
    fn note_active_travel(
        &mut self,
        tracks: &[core::renderer::Track],
        animations: &[core::renderer::TrackAnimation],
    ) {
        if tracks.is_empty() {
            return;
        }

        self.active_tracks = tracks
            .iter()
            .map(|track| crate::graphics::presentation::TrackGeneration {
                node: track.node,
                generation: animations
                    .iter()
                    .find(|animation| animation.node == track.node)
                    .map_or(0, |animation| animation.generation),
            })
            .collect();
        self.active_tracks.sort_unstable_by_key(|track| track.node);
    }

    /// The `(node, generation)` set of the travel currently published to the
    /// lane (enzICED #2219).
    pub fn active_travel(&self) -> &[crate::graphics::presentation::TrackGeneration] {
        &self.active_tracks
    }

    /// Updates the running travel without replacing the committed scene, and
    /// records the generations that travel is made of (enzICED #2219).
    ///
    /// The runtime must go through here rather than reaching for
    /// `Window::lane` directly: the recorded generations are what stops a
    /// pre-retarget callback from steering the post-retarget travel, and a
    /// caller that published to the lane without recording them would
    /// reintroduce exactly that defect.
    pub fn animate_lane(
        &mut self,
        tracks: Vec<core::renderer::Track>,
        animations: Vec<core::renderer::TrackAnimation>,
    ) {
        self.note_active_travel(&tracks, &animations);

        if let Some(lane) = self.lane.as_mut() {
            lane.animate(tracks, animations);
        }
    }

    /// Whether a usable lane exists -- or can still be created -- for this
    /// window.
    ///
    /// A window that has never animated has no lane yet *and* has not been
    /// refused one, and must still be planned as lane-capable, or the lazy lane
    /// would never get its first hand over (enzICED #2219).
    pub fn has_lane(&self) -> bool {
        !self.lane_disabled
            && match self.lane.as_ref() {
                Some(lane) => !lane.is_poisoned(),
                None => !self.lane_attempted,
            }
    }

    /// Creates this window's animation lane if it does not have one yet
    /// (enzICED #2219).
    ///
    /// Returns whether a usable lane now exists. The compositor is asked at
    /// most once per window: a backend without a lane (software/`softbuffer`)
    /// answers `None` and is never asked again, so the lazy path costs one
    /// bool for the life of the window.
    fn ensure_lane(&mut self, compositor: &C) -> bool {
        if self.lane_disabled || self.presentation_quarantined {
            return false;
        }

        if self.lane.is_none() && !self.lane_attempted {
            self.lane_attempted = true;

            let lane = compositor.spawn_animation_lane(self.lane_settings);

            // The lane wakes the event loop when it stops on its own, so a
            // settled or failed travel becomes an ordinary UI frame instead of
            // a parked animation.
            if let Some(lane) = lane.as_ref() {
                // enzICED #2219: the lane commits through the SAME pre-commit
                // seam the UI thread does, so its frames get real compositor
                // feedback instead of a submission timestamp.
                lane.observe_presentation(Arc::new(crate::presentation::LaneObserver::new(
                    self.presentation.clone(),
                )));

                // enzICED #2219: ONE ledger per window. The lane seals its
                // four stages into the same place the UI thread seals its six,
                // so a row's owner -- not the file it was written to -- is what
                // says which thread did the work.
                if let Some(ledger) = self.stages.as_ref() {
                    lane.record_stages(ledger.clone());
                }

                let waker = self.waker.clone();
                lane.on_stop(move || waker.wake());
            }

            self.lane = lane;
        }

        self.lane.as_ref().is_some_and(|lane| !lane.is_poisoned())
    }

    /// Installs the per-frame stage ledger for this window (enzICED #2219).
    ///
    /// Must be called before the first hand over for the lane to share it; a
    /// lane spawned earlier keeps recording nothing rather than acquiring a
    /// second ledger, because two ledgers cannot be joined into one frame row.
    pub fn record_stages(&mut self, ledger: Arc<crate::graphics::presentation::StageLedger>) {
        if let Some(lane) = self.lane.as_ref() {
            lane.record_stages(ledger.clone());
        }

        self.stages = Some(ledger);
    }

    /// The stage ledger installed on this window, if any.
    pub fn stages(&self) -> Option<&Arc<crate::graphics::presentation::StageLedger>> {
        self.stages.as_ref()
    }

    /// Records one UI-thread stage span for the frame being built
    /// (enzICED #2219).
    ///
    /// A no-op without a ledger. Offering a render-lane stage here is recorded
    /// as a misattribution error rather than accepted.
    pub fn record_stage(
        &self,
        stage: crate::graphics::presentation::FrameStage,
        start: Instant,
        end: Instant,
    ) {
        if let Some(ledger) = self.stages.as_ref() {
            ledger.record(
                crate::graphics::presentation::FrameOwner::Ui,
                stage,
                start,
                end,
            );
        }
    }

    /// Whether the animation lane is presenting this window right now.
    pub fn is_lane_running(&self) -> bool {
        !self.lane_disabled
            && self
                .lane
                .as_ref()
                .is_some_and(crate::graphics::lane::Lane::is_running)
    }

    /// Whether the lane stopped on its own and the UI thread has not taken the
    /// surface back yet (enzICED #2219).
    pub fn is_lane_stopped(&self) -> bool {
        self.lane
            .as_ref()
            .is_some_and(crate::graphics::lane::Lane::has_stopped)
    }

    // Reclaims a self-stopped lane; settlement and occlusion preserve future lane availability.
    //
    // Other stops disable the lane for this window and discard its retained scene.
    pub fn recover_from_lane(&mut self) -> Option<crate::graphics::lane::Report> {
        use crate::graphics::lane::Stop;

        let report = self.reclaim_surface();

        match report.as_ref().map(|report| &report.stop) {
            Some(Stop::Settled | Stop::Occluded) => {}
            _ => {
                self.lane_disabled = true;
                retire_scene(&mut self.scene);
                if let Some(lane) = self.lane.as_mut() {
                    lane.retire_scene();
                }
            }
        }

        report
    }

    /// Publishes `scene` to the lane while it keeps presenting
    /// (enzICED #2219).
    ///
    /// This is the content-revision path: the UI thread recorded a replacement
    /// scene, so it hands it over *without* taking the surface back, and the
    /// track it is already presenting continues from where it is.
    /// Records whether the frame in progress repaints what moves
    /// ([`core::Shell::request_redraw`]).
    pub fn note_repainting(&mut self, repainting: bool) {
        self.ui_repainting = repainting;
    }

    pub fn commit_scene_to_lane(
        &mut self,
        tracks: Vec<core::renderer::Track>,
        animations: Vec<core::renderer::TrackAnimation>,
    ) -> bool {
        if !self.is_lane_running() {
            return false;
        }

        let Some(scene) = self.replayable_scene(self.state.logical_size()).cloned() else {
            return false;
        };

        if self.input_recovery.is_some()
            || (scene.presentation().is_some() && !self.presentation.has_protocol())
        {
            return false;
        }
        let viewport = self.state.viewport().clone();
        let background = self.state.background_color();

        // The replacement scene carries the travel that is running now, so the
        // generations a later callback is judged against move with it
        // (enzICED #2219).
        self.note_active_travel(&tracks, &animations);

        if let Some(wake) = self.ui_wake {
            // A slower frame counts at once; a faster one eases the estimate
            // down, so one quick frame does not make the next one late.
            let cost = Instant::now().saturating_duration_since(wake);
            self.ui_commit_cost = Some(match self.ui_commit_cost {
                Some(previous) if cost < previous => (previous * 3 + cost) / 4,
                _ => cost,
            });
        }
        match self.lane.as_mut() {
            Some(lane) => {
                // The UI thread drew this scene for its frame instant and
                // has not presented it.
                lane.commit(
                    scene,
                    self.scene_generation,
                    viewport,
                    background,
                    tracks,
                    animations,
                    self.ui_sample,
                    false,
                    self.ui_repainting,
                );
                true
            }
            None => false,
        }
    }

    // Transfers an admitted recording and its live animations to the lane.
    //
    // Skipping UI submission requires an owned-layout recording with live tenure;
    // owned layouts also require the presentation feedback protocol. Input remains
    // gated on an exact compositor-confirmed frame. Refusal leaves the surface with the UI.
    pub fn hand_over_to_lane(
        &mut self,
        compositor: &C,
        tracks: Vec<core::renderer::Track>,
        mut animations: Vec<core::renderer::TrackAnimation>,
        now: Instant,
        rebase: impl FnOnce(
            &P::Renderer,
            &[core::renderer::TrackAnimation],
            core::time::Duration,
        ) -> bool,
    ) -> bool {
        if self.input_recovery.is_some()
            || (self
                .scene
                .as_ref()
                .is_some_and(|(scene, _)| scene.presentation().is_some())
                && !self.presentation.has_protocol())
        {
            return false;
        }
        if self.lane_disabled || resize_holds(self.resize_hold_until, now) {
            return false;
        }

        let Some(scene) = self.replayable_scene(self.state.logical_size()).cloned() else {
            return false;
        };
        let delay = crate::layout_ownership::preparation_delay(
            &scene,
            &animations,
            &self.active_tracks,
            self.clock_restart_allowed,
            now,
        );
        if delay.is_none()
            && !animations
                .iter()
                .any(|animation| animation.is_usable() && !animation.is_finished(now))
        {
            return false;
        }

        if !self.ensure_lane(compositor) {
            return false;
        }

        match self.lane.as_ref() {
            Some(lane) if !lane.holds_surface() && !lane.is_poisoned() => {}
            _ => return false,
        }

        if self.input_recovery.is_some()
            || (scene.presentation().is_some() && !self.presentation.has_protocol())
        {
            return false;
        }
        let now = Instant::now();
        let delay = crate::layout_ownership::preparation_delay(
            &scene,
            &animations,
            &self.active_tracks,
            self.clock_restart_allowed,
            now,
        );
        if let Some(delay) = delay
            && rebase(&self.renderer, &animations, delay)
        {
            for animation in &mut animations {
                animation.start = now;
            }
        }
        if !animations
            .iter()
            .any(|animation| animation.is_usable() && !animation.is_finished(now))
        {
            return false;
        }
        let viewport = self.state.viewport().clone();
        let background = self.state.background_color();

        // The travel the lane is about to own (enzICED #2219).
        self.note_active_travel(&tracks, &animations);

        // Commit first: the worker is idle, so it applies the scene and then
        // takes the surface, and its very first frame already has something to
        // replay.
        if let Some(lane) = self.lane.as_mut() {
            // The UI thread has presented this scene itself: the lane carries
            // it on from there.
            lane.commit(
                scene,
                self.scene_generation,
                viewport.clone(),
                background,
                tracks,
                animations,
                self.ui_presented_sample,
                true,
                self.ui_repainting,
            );
        }

        self.start_lane(viewport, background)
    }

    /// Hands the surface to the animation lane, which starts presenting the
    /// committed scene on its own clock (enzICED #2219).
    ///
    /// Returns whether the lane took it. Every refusal keeps the surface on the
    /// UI thread, so a lane that cannot start costs correctness nothing.
    pub fn start_lane(&mut self, viewport: crate::graphics::Viewport, background: Color) -> bool {
        let _ = self.reclaim_surface();

        let (Some(lane), Some(surface)) = (self.lane.as_mut(), self.surface.take()) else {
            return false;
        };

        match lane.start(surface, viewport, background) {
            // Refused: take the surface back, unchanged. Nothing transferred,
            // so the ownership generation must NOT move -- a frame committed
            // either side of a refusal is still the same owner's and must stay
            // joinable (enzICED #2219).
            Some(surface) => {
                self.surface = Some(surface);
                false
            }
            None => {
                self.clock_restart_allowed = false;
                let _ = self.presentation.transfer_ownership();
                true
            }
        }
    }

    // Presents the renderer's frame after reclaiming exclusive surface ownership.
    // Unconfirmed ownership reports SurfaceError::Lost without authorizing recreation.
    pub fn present_frame(
        &mut self,
        compositor: &mut C,
    ) -> Result<(), crate::graphics::compositor::SurfaceError> {
        let reclaim_started = Instant::now();
        if !crate::layout_ownership::prepare_ui_frame(
            self,
            |window| {
                let _ = window.reclaim_surface();
                window.surface.is_some()
            },
            |window| {
                window
                    .scene
                    .as_ref()
                    .map(|(recording, _)| recording.clone())
            },
            |window| retire_scene(&mut window.scene),
        ) {
            return Err(crate::graphics::compositor::SurfaceError::Lost);
        }

        let reclaimed = Instant::now();
        let viewport = self.state.viewport().clone();
        let background = self.state.background_color();
        let raw = self.raw.clone();
        let presentation = self.presentation.clone();
        let stages = self.stages.clone();
        // A window with a stage ledger has its backend stages measured too.
        compositor.measure_present_stages(stages.is_some());
        let scene_generation = self.scene_generation;
        let ui_sample = self.ui_sample.take();

        let Some(surface) = self.surface.as_mut() else {
            return Err(crate::graphics::compositor::SurfaceError::Lost);
        };

        let mut native_commit = None;
        // A frame that changed nothing leaves the surface as the last present
        // did: no swapchain image, copy or commit, and the compositor has
        // nothing to composite again.
        let unchanged = self.shows_ui_present
            && compositor.unchanged(&mut self.renderer, &viewport, background);
        let result = if unchanged {
            Ok(())
        } else {
            compositor.present(&mut self.renderer, surface, &viewport, background, || {
                // enzICED #2219: ORDER IS THE CONTRACT. The frame is already
                // acquired and submitted; the feedback request goes into the
                // connection's request stream HERE, and only then is the surface
                // committed. `present` does not call this hook on the error paths,
                // so a failed acquisition requests nothing and leaves no pending
                // identity behind.
                //
                // The UI thread presents a static frame, so it samples no tracks:
                // an empty list, not a fabricated one.
                let key = presentation.pre_commit(crate::presentation::ui_submission(
                    scene_generation,
                    ui_sample.unwrap_or_else(Instant::now),
                    &[],
                ));
                native_commit = key;

                raw.pre_present_notify();
            })
        };
        self.shows_ui_present = result.is_ok();
        // Stage spans require the native identity the hook registered; they
        // are sealed once the present they include has returned. A missing
        // identity discards them instead of assigning them to a later frame.
        if let Some(ledger) = stages.as_ref() {
            use crate::graphics::presentation::{FrameOwner, FrameStage};
            ledger.record(
                FrameOwner::Ui,
                FrameStage::Reclaim,
                reclaim_started,
                reclaimed,
            );
            if let Some(spans) = compositor.take_present_stages() {
                for (stage, span) in [
                    (FrameStage::Acquire, spans.acquire),
                    (FrameStage::Prepare, spans.prepare),
                    (FrameStage::Submit, spans.submit),
                    (FrameStage::Present, spans.present),
                ] {
                    ledger.record(FrameOwner::Ui, stage, span.0, span.1);
                }
            }
            let _ = ledger.seal(FrameOwner::Ui, native_commit);
        }
        self.native_input_commit = if result.is_ok() { native_commit } else { None };
        if let Some(key) = native_commit.filter(|_| result.is_ok()) {
            let submitted = Instant::now();
            let (requested, wake) = (self.redraw_requested_at.take(), self.ui_wake.take());
            let refusal = {
                use crate::core::Renderer as _;
                self.renderer.presentation_refusal()
            };
            crate::presentation::trace_ui_frame(|| {
                crate::presentation::ui_frame_record(&key, requested, wake, submitted, refusal)
            });
        }
        if result.is_ok()
            && let Some(at) = ui_sample
        {
            self.ui_pacer.presented(at);
            self.ui_presented_sample = Some(at);
        }
        result
    }

    /// Reconfigures this window's surface, reclaiming it first.
    pub fn configure_surface(&mut self, compositor: &mut C, width: u32, height: u32) {
        // enzICED #2219: the surface this window commits to is not the one any
        // in-flight feedback was requested against. Those callbacks stay
        // honest telemetry, but they can no longer steer this window.
        let _ = self.presentation.invalidate_surface();

        let _ = self.reclaim_surface();
        if self.presentation_quarantined {
            return;
        }
        self.shows_ui_present = false;
        match self.surface.as_mut() {
            Some(surface) => compositor.configure_surface(surface, width, height),
            None => {
                let surface = compositor.create_surface(self.raw.clone(), width, height);
                self.set_surface(surface);
            }
        }
    }

    /// Recreates this window's surface from scratch.
    pub fn recreate_surface(&mut self, compositor: &mut C, width: u32, height: u32) {
        let _ = self.presentation.invalidate_surface();
        let _ = self.reclaim_surface();
        if self.presentation_quarantined {
            return;
        }
        self.surface = None;
        let surface = compositor.create_surface(self.raw.clone(), width, height);
        self.set_surface(surface);
    }

    // Retires the lane before replacing its compositor or renderer.
    // Unconfirmed destruction quarantines this window; callers must check before replacing resources.
    pub fn shutdown_lane(&mut self) {
        let _ = self.reclaim_surface();
        if let Some(lane) = self.lane.take() {
            let result = lane.shutdown_checked();
            if result.retirement == crate::graphics::lane::Retirement::Quarantined {
                self.presentation_quarantined = true;
                self.presentation.shutdown();
                self.surface = None;
            } else if !self.presentation_quarantined && result.surface.is_some() {
                self.surface = result.surface;
            }
        }
        self.lane_disabled = self.presentation_quarantined;
        self.lane_attempted = self.presentation_quarantined;
        retire_scene(&mut self.scene);
    }

    // Feedback must be revoked and the window's scene dropped before a bounded backend wait.
    fn shutdown_for_teardown(&mut self) {
        self.revoke_presented_input();
        self.presentation.shutdown();
        retire_scene(&mut self.scene);
        self.shutdown_lane();
    }

    /// Drops the committed scene, so the next frame is drawn in full
    /// (enzICED #2219).
    ///
    /// Called for every event that can change what an unchanged widget would
    /// paint but that no track describes: resize, scale factor, theme, fonts,
    /// a device loss or a compositor fallback. Retention must fail *closed* --
    /// a stale scene is a wrong picture, and a full draw is only slow.
    pub fn invalidate_scene(&mut self) {
        self.revoke_presented_input();
        self.native_input_commit = None;
        // A scene the UI thread no longer trusts must not keep being presented
        // by the lane either, so ownership comes back with the invalidation.
        retire_scene(&mut self.scene);
        let _ = self.reclaim_surface();
        if let Some(lane) = self.lane.as_mut() {
            lane.retire_scene();
        }
    }

    /// The committed scene, if it still matches `viewport_size`
    /// (enzICED #2219).
    pub fn replayable_scene(&self, viewport_size: Size) -> Option<&core::renderer::Recording> {
        self.scene
            .as_ref()
            .and_then(|(scene, size)| (*size == viewport_size).then_some(scene))
    }

    pub fn position(&self) -> Option<Point> {
        self.raw
            .outer_position()
            .ok()
            .map(|position| position.to_logical(self.raw.scale_factor()))
            .map(|position| Point {
                x: position.x,
                y: position.y,
            })
    }

    pub fn request_redraw(&mut self, redraw_request: RedrawRequest) {
        if let Some((request, at)) = crate::core::shell::wake_trace::take() {
            let kind = match request {
                RedrawRequest::NextFrame => "next",
                RedrawRequest::At(_) => "at",
                RedrawRequest::Wait => "wait",
            };
            crate::debug::time(format!("wake:{kind}:{}:{}", at.file(), at.line())).finish();
        }
        match redraw_request {
            // With the lane presenting, no frame callback paces the UI
            // thread: a next frame is the next vblank, or it would run as
            // fast as it can draw.
            RedrawRequest::NextFrame if self.is_lane_running() => {
                let at = self.next_content_frame(Instant::now());
                self.request_redraw(RedrawRequest::At(at));
            }
            RedrawRequest::NextFrame => {
                self.request_full_redraw();
                self.redraw_at = None;
                let _ = self.redraw_requested_at.get_or_insert_with(Instant::now);
            }
            // A later request never postpones an earlier pending one.
            RedrawRequest::At(at) => {
                self.redraw_at = Some(self.redraw_at.map_or(at, |pending| pending.min(at)));
            }
            RedrawRequest::Wait => {}
        }
    }

    /// Requests a frame that must be drawn in full (enzUI-ICED #3442).
    pub fn request_full_redraw(&mut self) {
        self.full_redraw
            .store(true, std::sync::atomic::Ordering::Release);
        self.raw.request_redraw();
    }

    /// Requests a frame for content redraws only: unless something else asks
    /// for a frame first, it presents the last frame again with its
    /// primitives prepared (enzUI-ICED #3442).
    pub fn request_content_redraw(&mut self) {
        self.content_redraw = true;
        if self.is_lane_running() {
            let at = self.next_content_frame(Instant::now());
            self.request_redraw(RedrawRequest::At(at));
            return;
        }
        self.raw.request_redraw();
        self.redraw_at = None;
        let _ = self.redraw_requested_at.get_or_insert_with(Instant::now);
    }

    /// Whether the frame about to be drawn was asked for by content redraws
    /// alone. Clears both kinds of pending request.
    pub fn take_content_frame(&mut self) -> bool {
        let full = self
            .full_redraw
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        std::mem::take(&mut self.content_redraw) && !full
    }

    /// Whether an input method preedit is shown over the window.
    pub fn has_preedit(&self) -> bool {
        self.preedit.is_some()
    }

    pub fn request_input_method(&mut self, input_method: InputMethod) {
        match input_method {
            InputMethod::Disabled => {
                self.disable_ime();
            }
            InputMethod::Enabled {
                cursor,
                purpose,
                preedit,
            } => {
                self.enable_ime(cursor, purpose);

                if let Some(preedit) = preedit {
                    if preedit.content.is_empty() {
                        self.preedit = None;
                    } else {
                        let mut overlay = self.preedit.take().unwrap_or_else(Preedit::new);

                        overlay.update(
                            cursor,
                            &preedit,
                            self.state.background_color(),
                            &self.renderer,
                        );

                        self.preedit = Some(overlay);
                    }
                } else {
                    self.preedit = None;
                }
            }
        }
    }

    /// Shows the cursor for `interaction`; a custom cursor is returned for
    /// the event loop to make, which only it can (enzICED).
    #[must_use]
    pub fn update_mouse(&mut self, interaction: mouse::Interaction) -> Option<mouse::CustomCursor> {
        let mut custom = None;
        if interaction != self.mouse_interaction {
            if let mouse::Interaction::Custom(cursor) = interaction {
                custom = Some(cursor);

                if self.mouse_interaction == mouse::Interaction::Hidden {
                    self.raw.set_cursor_visible(true);
                }
            } else if let Some(icon) = conversion::mouse_interaction(interaction) {
                self.raw.set_cursor(icon);

                if self.mouse_interaction == mouse::Interaction::Hidden {
                    self.raw.set_cursor_visible(true);
                }
            } else {
                self.raw.set_cursor_visible(false);
            }

            self.mouse_interaction = interaction;
        }
        custom
    }

    pub fn draw_preedit(&mut self) {
        if let Some(preedit) = &self.preedit {
            preedit.draw(
                &mut self.renderer,
                self.state.text_color(),
                self.state.background_color(),
                &Rectangle::new(Point::ORIGIN, self.state.viewport().logical_size()),
            );
        }
    }

    fn enable_ime(&mut self, cursor: Rectangle, purpose: input_method::Purpose) {
        if self.ime_state.is_none() {
            self.raw.set_ime_allowed(true);
        }

        if self.ime_state != Some((cursor, purpose)) {
            self.raw.set_ime_cursor_area(
                LogicalPosition::new(cursor.x, cursor.y),
                LogicalSize::new(cursor.width, cursor.height),
            );
            self.raw.set_ime_purpose(conversion::ime_purpose(purpose));

            self.ime_state = Some((cursor, purpose));
        }
    }

    /// Tells the input method the text around its cursor, when it changed;
    /// `None` clears it.
    pub fn request_input_method_surrounding(
        &mut self,
        surrounding: Option<input_method::Surrounding>,
    ) {
        if self.ime_surrounding == surrounding {
            return;
        }
        self.raw.set_ime_surrounding_text(
            surrounding
                .clone()
                .map(|surrounding| (surrounding.text, surrounding.cursor, surrounding.anchor)),
        );
        self.ime_surrounding = surrounding;
    }

    fn disable_ime(&mut self) {
        if self.ime_state.is_some() {
            self.raw.set_ime_allowed(false);
            self.ime_state = None;
        }

        self.preedit = None;
    }
}

impl<P, C> Drop for Window<P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    fn drop(&mut self) {
        // `raw` stays alive while feedback is revoked and bounded lane retirement begins.
        self.shutdown_for_teardown();
    }
}

struct Preedit<Renderer>
where
    Renderer: text::Renderer,
{
    position: Point,
    content: Renderer::Paragraph,
    spans: Vec<text::Span<'static, ()>>,
}

impl<Renderer> Preedit<Renderer>
where
    Renderer: text::Renderer,
{
    fn new() -> Self {
        Self {
            position: Point::ORIGIN,
            spans: Vec::new(),
            content: Renderer::Paragraph::default(),
        }
    }

    fn update(
        &mut self,
        cursor: Rectangle,
        preedit: &input_method::Preedit,
        background: Color,
        renderer: &Renderer,
    ) {
        self.position = cursor.position() + Vector::new(0.0, cursor.height);

        let background = Color {
            a: 1.0,
            ..background
        };

        let spans = match &preedit.selection {
            Some(selection) => {
                vec![
                    text::Span::new(&preedit.content[..selection.start]),
                    text::Span::new(if selection.start == selection.end {
                        "\u{200A}"
                    } else {
                        &preedit.content[selection.start..selection.end]
                    })
                    .color(background),
                    text::Span::new(&preedit.content[selection.end..]),
                ]
            }
            _ => vec![text::Span::new(&preedit.content)],
        };

        if spans != self.spans.as_slice() {
            use text::Paragraph as _;

            self.content = Renderer::Paragraph::with_spans(Text {
                content: &spans,
                bounds: Size::INFINITE,
                size: preedit.text_size.unwrap_or_else(|| renderer.text_size()),
                line_height: renderer.line_height(),
                font: renderer.font(),
                align_x: text::Alignment::Default,
                align_y: alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            });

            self.spans.clear();
            self.spans
                .extend(spans.into_iter().map(text::Span::to_static));
        }
    }

    fn draw(&self, renderer: &mut Renderer, color: Color, background: Color, viewport: &Rectangle) {
        use text::Paragraph as _;

        if self.content.min_width() < 1.0 {
            return;
        }

        let mut bounds = Rectangle::new(
            self.position - Vector::new(0.0, self.content.min_height()),
            self.content.min_bounds(),
        );

        bounds.x = bounds
            .x
            .max(viewport.x)
            .min(viewport.x + viewport.width - bounds.width);

        bounds.y = bounds
            .y
            .max(viewport.y)
            .min(viewport.y + viewport.height - bounds.height);

        renderer.with_layer(bounds, |renderer| {
            let background = Color {
                a: 1.0,
                ..background
            };

            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    ..Default::default()
                },
                background,
            );

            renderer.fill_paragraph(&self.content, bounds.position(), color, bounds);

            const UNDERLINE: f32 = 2.0;

            renderer.fill_quad(
                renderer::Quad {
                    bounds: bounds.shrink(Padding {
                        top: bounds.height - UNDERLINE,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                color,
            );

            for span_bounds in self.content.span_bounds(1) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: span_bounds + (bounds.position() - Point::ORIGIN),
                        ..Default::default()
                    },
                    color,
                );
            }
        });
    }
}

/// The instant a UI frame woken at `wake` is laid out for.
///
/// While the lane presents (`lane_cost` is the UI thread's recent cost from
/// wake to commit), the frame is shown by the first lane frame that starts
/// after the commit arrives (half a period before the vblank that frame
/// follows), so it is laid out for the vblank that frame is sampled for: laid
/// out for an earlier one, the lane would show it later, moved rigidly. It is
/// never an earlier instant than `latest`, a frame already laid out, for the
/// same reason (enzUI-ICED #3660).
fn ui_frame_target(
    pacer: &mut crate::graphics::presentation::FramePacer,
    wake: Instant,
    grid: Option<crate::graphics::presentation::PresentationGrid>,
    lane_cost: Option<std::time::Duration>,
    latest: Option<Instant>,
) -> Instant {
    let (ready, lead) = match (grid, lane_cost) {
        (Some(grid), Some(cost)) => (
            wake + grid.period / 2 + cost.min(grid.period * 2),
            crate::graphics::lane::LANE_PRESENT_LEAD,
        ),
        _ => (wake, 0),
    };
    let at = pacer.target(ready, grid, lead);
    latest.map_or(at, |latest| at.max(latest))
}

#[cfg(test)]
mod tests {

    /// While the lane presents, a frame is laid out for the vblank its commit
    /// reaches, and a smaller cost estimate never takes it back before a frame
    /// already laid out (enzUI-ICED #3660).
    ///
    /// Sabotage: drop the `latest` bound -- the second frame is laid out for
    /// an earlier vblank than the first.
    #[test]
    fn a_lane_frame_is_laid_out_for_its_vblank_and_never_earlier() {
        use crate::graphics::presentation::{FramePacer, PresentationGrid};
        let period = std::time::Duration::from_micros(6_944);
        let anchor = Instant::now();
        let grid = PresentationGrid::new(anchor, period);
        let mut pacer = FramePacer::new();
        let wake = anchor + period / 4;
        let cost = period * 2;
        let first = super::ui_frame_target(&mut pacer, wake, grid, Some(cost), None);
        assert!(
            first >= wake + period / 2 + cost,
            "a commit made after its cost lands on a later vblank"
        );
        let second = super::ui_frame_target(
            &mut pacer,
            wake + period / 2,
            grid,
            Some(std::time::Duration::ZERO),
            Some(first),
        );
        assert!(second >= first, "{second:?} is before {first:?}");
        assert_eq!(
            super::ui_frame_target(&mut FramePacer::new(), wake, None, None, None),
            wake,
            "no grid, no lane: the wake"
        );
    }

    #[test]
    fn a_resize_keeps_the_lane_off_until_the_size_settles() {
        let resized = Instant::now();
        let until = Some(resized + super::RESIZE_SETTLE);
        assert!(
            super::resize_holds(until, resized),
            "the lane waits right after a resize"
        );
        assert!(
            super::resize_holds(until, resized + super::RESIZE_SETTLE / 2),
            "and while the size is still settling"
        );
        assert!(
            !super::resize_holds(until, resized + super::RESIZE_SETTLE),
            "it may present again once the size has held"
        );
        assert!(!super::resize_holds(None, resized), "no resize, no hold");
    }

    use super::*;
    use crate::core::renderer::presentation::{
        Bounds, Expression, Layout, Node, Program as PresentationProgram,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn scene_reclamation_window_slot_drops_payload_and_program_exactly_once() {
        let program = Arc::new(
            PresentationProgram::new(
                vec![Expression::Constant(0.0)],
                vec![Node {
                    id: 1,
                    layout: Layout::Fixed {
                        recorded: Bounds {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                        },
                    },
                    children: Vec::new(),
                }],
                Vec::new(),
                [0, 0],
                Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            )
            .expect("test presentation program should be valid"),
        );
        let program_weak = Arc::downgrade(&program);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut scene = Some((
            renderer::Recording::new(DropProbe(Arc::clone(&drops))).with_presentation(program),
            Size::new(1.0, 1.0),
        ));

        retire_scene(&mut scene);
        assert!(scene.is_none());
        assert!(program_weak.upgrade().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        retire_scene(&mut scene);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
