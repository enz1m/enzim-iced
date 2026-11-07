// Modified by enzUI contributors. See FORK.md.
//! AccessKit (AT-SPI / UI Automation) bridge for the winit backend.
//!
//! Enabled by the `a11y` cargo feature. When on, `iced_winit` hosts one
//! [`accesskit_winit::Adapter`] per window and, each frame the tree could have
//! changed, pushes a fresh [`accesskit::TreeUpdate`] built by an
//! **application-supplied collector** (see [`set_tree_builder`]). Platform
//! action requests (click / focus / set-value / increment / decrement / scroll)
//! are surfaced back to the event loop as [`Request`]s.
//!
//! # Why the collector is supplied by the application
//!
//! iced core has no accessibility model of its own — the semantics live in a
//! downstream crate (enzUI-ICED's `enzui-iced-core::collect`) that knows how a
//! widget publishes its role/state via `Operation::custom`. `iced_winit` must
//! not depend on that crate (wrong direction), so instead of collecting the
//! tree itself it exposes a *runner*: the application registers a closure that,
//! given the ability to run an [`Operation`] over the live widget tree, returns
//! an [`accesskit::TreeUpdate`]. iced_winit owns the winit/AccessKit plumbing;
//! the app owns the semantics. Register once before [`crate::run`]:
//!
//! ```ignore
//! iced_winit::accessibility::set_tree_builder(|window, run| {
//!     let mut collector = enzui_iced_core::collect::SemanticsCollector::new();
//!     run(&mut collector);
//!     // `window` is the exact iced window this publish belongs to (#2677),
//!     // so a per-window binding can be maintained here and nowhere else.
//!     let _ = window;
//!     Some(collector.to_tree_update())
//! });
//! ```
//!
//! When no builder is registered (the default), the feature is inert: no
//! adapter is created and there is zero per-frame cost.

use std::sync::{Arc, Mutex, OnceLock};

use accesskit::{ActionRequest, TreeUpdate};
use accesskit_winit::Adapter;

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub use accesskit_winit::virtual_text;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::core::widget::Operation;
use crate::core::window;

/// Application-facing registration of a lazy virtual-text provider (#2677).
///
/// The concrete Unix adapter lives inside [`WindowAccessibility`], which is
/// owned by iced's window manager: application code cannot reach it, and a
/// registry keyed only by AccessKit's *local* node ID would collide between
/// two windows that publish the same node. Applications therefore name the
/// window they published the node in; the per-frame accessibility path binds
/// the request to that window's exact adapter and tree.
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod virtual_text_mailbox {
    use super::virtual_text;
    use crate::core::window;
    use std::any::Any;
    use std::sync::{Arc, Mutex, Weak};

    /// One registration's lifetime state, shared by the application's handle
    /// and the event loop that binds it.
    enum Binding {
        Pending,
        /// A bind call is outside the mailbox lock. This prevents two frames
        /// from concurrently registering the same selector.
        Binding(usize),
        /// The live adapter registration token. Dropping it cancels in-flight
        /// requests and withdraws the provider-only AT-SPI interfaces.
        Bound {
            adapter: usize,
            _token: Box<dyn Any + Send>,
        },
        Withdrawn,
    }

    struct Registration {
        window: window::Id,
        tree: accesskit::TreeId,
        node: accesskit::NodeId,
        /// Weak: a provider that the application dropped must not be kept
        /// alive (and must not be bound) by the mailbox.
        provider: Weak<dyn virtual_text::VirtualTextProvider>,
        binding: Mutex<Binding>,
    }

    impl Registration {
        fn withdraw(&self) {
            if let Ok(mut binding) = self.binding.lock() {
                // Replacing the state drops a live platform token here. This
                // must happen before a replacement reaches the adapter, whose
                // exact-selector API correctly rejects two live owners.
                *binding = Binding::Withdrawn;
            }
        }
    }

    /// Weak entries make the application's RAII handle the sole registration
    /// owner. Bound entries stay discoverable so an adapter recreation can
    /// replace the old platform token with one from the new exact adapter.
    static REGISTRATIONS: Mutex<Vec<Weak<Registration>>> = Mutex::new(Vec::new());

    /// RAII owner of one application registration.
    ///
    /// Dropping it withdraws the request whether or not the window has bound it
    /// yet, so a closed editor cannot leave a stale provider answering AT-SPI.
    #[must_use = "dropping the handle immediately withdraws the provider"]
    pub struct VirtualTextHandle {
        registration: Arc<Registration>,
    }

    impl std::fmt::Debug for VirtualTextHandle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("VirtualTextHandle").finish_non_exhaustive()
        }
    }

    impl VirtualTextHandle {
        /// Whether this registration is currently bound to a live adapter.
        pub fn is_bound(&self) -> bool {
            self.registration
                .binding
                .lock()
                .is_ok_and(|binding| matches!(*binding, Binding::Bound { .. }))
        }
    }

    impl Drop for VirtualTextHandle {
        fn drop(&mut self) {
            self.registration.withdraw();
        }
    }

    /// Register `provider` for the node `node` of tree `tree` as published in
    /// window `window`.
    ///
    /// The request is bound on the window's next accessibility publish, and
    /// rebound after the window's adapter is recreated. Registering again for
    /// the same window/tree/node first withdraws the previous provider. An old
    /// handle is then inert and cannot remove the replacement token.
    pub fn register(
        window: window::Id,
        tree: accesskit::TreeId,
        node: accesskit::NodeId,
        provider: &Arc<dyn virtual_text::VirtualTextProvider>,
    ) -> VirtualTextHandle {
        let registration = Arc::new(Registration {
            window,
            tree,
            node,
            provider: Arc::downgrade(provider),
            binding: Mutex::new(Binding::Pending),
        });

        if let Ok(mut registrations) = REGISTRATIONS.lock() {
            registrations.retain(|weak| {
                let Some(existing) = weak.upgrade() else {
                    return false;
                };
                if existing.window == window && existing.tree == tree && existing.node == node {
                    existing.withdraw();
                    false
                } else {
                    true
                }
            });
            registrations.push(Arc::downgrade(&registration));
        }

        VirtualTextHandle { registration }
    }

    /// Bind registrations for one exact iced window and one tree to the real
    /// platform adapter that is publishing that tree.
    ///
    /// `adapter` is the platform adapter's monotonic identity, not a local node
    /// or allocation address. A failed bind returns to `Pending` and is retried
    /// by the next publish. Generic token ownership keeps the race and retry
    /// consequences testable without constructing a native window.
    pub(super) fn bind<T, E, F>(
        window: window::Id,
        adapter: usize,
        tree: accesskit::TreeId,
        mut register: F,
    ) where
        T: Send + 'static,
        F: FnMut(
            accesskit::TreeId,
            accesskit::NodeId,
            &Arc<dyn virtual_text::VirtualTextProvider>,
        ) -> Result<T, E>,
    {
        let ready = {
            let Ok(mut registrations) = REGISTRATIONS.lock() else {
                return;
            };
            let mut ready = Vec::new();
            registrations.retain(|weak| {
                let Some(registration) = weak.upgrade() else {
                    return false;
                };
                if registration.window == window && registration.tree == tree {
                    ready.push(registration);
                }
                true
            });
            ready
        };

        for registration in ready {
            let Some(provider) = registration.provider.upgrade() else {
                registration.withdraw();
                continue;
            };

            let previous = {
                let Ok(mut binding) = registration.binding.lock() else {
                    continue;
                };
                match &*binding {
                    Binding::Withdrawn | Binding::Binding(_) => continue,
                    Binding::Bound {
                        adapter: bound_adapter,
                        ..
                    } if *bound_adapter == adapter => continue,
                    Binding::Pending | Binding::Bound { .. } => {
                        std::mem::replace(&mut *binding, Binding::Binding(adapter))
                    }
                }
            };
            // On adapter recreation, release the old exact-selector token
            // before registering against the new adapter.
            drop(previous);

            let result = register(registration.tree, registration.node, &provider);
            let Ok(mut binding) = registration.binding.lock() else {
                // A successful token is dropped here if the lock was poisoned.
                continue;
            };
            if !matches!(*binding, Binding::Binding(binding_adapter) if binding_adapter == adapter)
            {
                // The handle was withdrawn while the platform call ran.
                continue;
            }
            match result {
                Ok(token) => {
                    *binding = Binding::Bound {
                        adapter,
                        _token: Box::new(token),
                    };
                }
                Err(_) => {
                    *binding = Binding::Pending;
                }
            }
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub use virtual_text_mailbox::{VirtualTextHandle, register as register_virtual_text_provider};

/// The application-supplied tree builder.
///
/// It is handed a *runner* — a `&mut dyn FnMut(&mut dyn Operation)` that runs
/// an [`Operation`] over the current widget tree — and returns the assembled
/// [`accesskit::TreeUpdate`], or `None` to skip this frame.
///
/// Collection always runs on iced's event-loop thread. `Send + Sync` is still
/// required because the builder lives in a process-wide static registration.
///
/// # Why the publishing `window::Id` is a parameter (#2677)
///
/// The builder is a process-wide static, but a publish is always **per
/// window**. A downstream binding that must name the window it published in
/// (the lazy virtual-text mailbox) cannot obtain that id any other way: the
/// initial window's id is created and dropped as `_id` in `lib.rs`, and
/// `window::latest()` is an asynchronous `Task`, so it is both unavailable to
/// a static closure and wrong for a multi-window publish. Only this call site
/// knows which window is being published, so it passes the id it already has.
/// The optional `&str` is the current synchronized window title, so the
/// builder can stamp it on the AccessKit root without a side-channel.
pub type TreeBuilder = dyn Fn(window::Id, Option<&str>, &mut dyn FnMut(&mut dyn Operation)) -> Option<TreeUpdate>
    + Send
    + Sync
    + 'static;

/// The application-supplied window-closed callback, invoked with the exact
/// iced [`window::Id`] the runtime has just destroyed. Registered as part of
/// the same bridge as the tree builder and request handler, because a
/// per-window routing table published by the builder can only be released by
/// the owner that published it (enzUI-ICED #2680).
pub type WindowClosedHandler = dyn Fn(window::Id) + Send + Sync + 'static;

/// The process-global accessibility bridge: the three registrations an owner
/// installs together.
///
/// Registered atomically by [`try_register_bridge`]. Installing the halves
/// separately is what produced mixed ownership — a tree builder from one owner
/// and a request handler from another, i.e. a published tree whose actions are
/// routed by somebody else's map.
///
/// A `Bridge` is also the *unit of publication*: the registry stores exactly
/// one of these values, so a reader either sees a whole registration or none
/// of it. The legacy one-half setters build a `Bridge` with the other halves
/// absent, which is still one value published once — never a half added to
/// somebody else's registration (enzUI-ICED #2680).
pub struct Bridge {
    tree_builder: Option<Box<TreeBuilder>>,
    request_handler: Option<Box<RequestHandler>>,
    on_window_closed: Option<Box<WindowClosedHandler>>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge").finish_non_exhaustive()
    }
}

impl Bridge {
    /// Assemble the three halves one owner installs together.
    pub fn new<B, H, C>(tree_builder: B, request_handler: H, on_window_closed: C) -> Self
    where
        B: Fn(window::Id, Option<&str>, &mut dyn FnMut(&mut dyn Operation)) -> Option<TreeUpdate>
            + Send
            + Sync
            + 'static,
        H: Fn(Request) + Send + Sync + 'static,
        C: Fn(window::Id) + Send + Sync + 'static,
    {
        Self {
            tree_builder: Some(Box::new(tree_builder)),
            request_handler: Some(Box::new(request_handler)),
            on_window_closed: Some(Box::new(on_window_closed)),
        }
    }

    /// The legacy tree-builder-only registration.
    fn tree_only(tree_builder: Box<TreeBuilder>) -> Self {
        Self {
            tree_builder: Some(tree_builder),
            request_handler: None,
            on_window_closed: None,
        }
    }

    /// The legacy request-handler-only registration.
    fn request_only(request_handler: Box<RequestHandler>) -> Self {
        Self {
            tree_builder: None,
            request_handler: Some(request_handler),
            on_window_closed: None,
        }
    }
}

/// The one process-global registration slot.
///
/// There is exactly **one** slot, and it holds the whole [`Bridge`]. A reader
/// therefore observes a registration as a single transaction: before the
/// publication it sees nothing, after it sees every half that registration
/// carries. Three independent slots filled in sequence had no such boundary —
/// a window closing (or a request arriving) between two `set` calls observed a
/// tree builder with no request handler, which is exactly the mixed ownership
/// the bridge exists to prevent (enzUI-ICED #2680).
///
/// `OnceLock::set` also decides the winner between concurrent registrars
/// without a reservation: the loser's whole `Bridge` is returned to it in the
/// `Err`, so a loser can never replace a half of the winner's registration.
struct Registry {
    published: OnceLock<Bridge>,
}

impl Registry {
    const fn new() -> Self {
        Self {
            published: OnceLock::new(),
        }
    }

    /// The one load every reader performs. All the observers below derive
    /// from it, so none of them can disagree about who is registered.
    fn bridge(&self) -> Option<&Bridge> {
        self.published.get()
    }

    /// Publish one registration, or refuse it whole.
    fn publish(&self, bridge: Bridge) -> Result<(), AlreadyRegistered> {
        self.published.set(bridge).map_err(|_| AlreadyRegistered)
    }

    fn has_tree_builder(&self) -> bool {
        self.bridge()
            .is_some_and(|bridge| bridge.tree_builder.is_some())
    }

    fn has_request_handler(&self) -> bool {
        self.bridge()
            .is_some_and(|bridge| bridge.request_handler.is_some())
    }

    fn has_window_closed_handler(&self) -> bool {
        self.bridge()
            .is_some_and(|bridge| bridge.on_window_closed.is_some())
    }

    fn collect_tree(
        &self,
        window: window::Id,
        title: Option<&str>,
        run: &mut dyn FnMut(&mut dyn Operation),
    ) -> Option<TreeUpdate> {
        self.bridge()?.tree_builder.as_ref()?(window, title, run)
    }

    fn dispatch_request(&self, request: Request) {
        if let Some(handler) = self
            .bridge()
            .and_then(|bridge| bridge.request_handler.as_ref())
        {
            handler(request);
        }
    }

    fn dispatch_window_closed(&self, window: window::Id) {
        if let Some(handler) = self
            .bridge()
            .and_then(|bridge| bridge.on_window_closed.as_ref())
        {
            handler(window);
        }
    }

    /// Which halves the single published registration carries, read with
    /// **one** load of the slot.
    ///
    /// The three public observers are three independent loads, so a tuple
    /// built from them can straddle a publication and observe a mixed triple
    /// even though no partial [`Bridge`] ever existed. That is a property of
    /// the caller's three calls, not of publication, so the atomicity claim is
    /// tested through this single-load snapshot instead (enzUI-ICED #2680).
    #[cfg(test)]
    fn snapshot(&self) -> Registration {
        match self.bridge() {
            Some(bridge) => Registration {
                tree_builder: bridge.tree_builder.is_some(),
                request_handler: bridge.request_handler.is_some(),
                window_closed: bridge.on_window_closed.is_some(),
            },
            None => Registration::EMPTY,
        }
    }
}

/// One single-load observation of the registration slot.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Registration {
    tree_builder: bool,
    request_handler: bool,
    window_closed: bool,
}

#[cfg(test)]
impl Registration {
    const EMPTY: Self = Self {
        tree_builder: false,
        request_handler: false,
        window_closed: false,
    };
    const WHOLE: Self = Self {
        tree_builder: true,
        request_handler: true,
        window_closed: true,
    };
}

static REGISTRY: Registry = Registry::new();

/// The application-supplied action-request handler. Invoked (on the event-loop
/// thread) for each AccessKit [`Request`] so the app can correlate the target
/// node with a widget and enqueue the matching message. `Send + Sync` is
/// required because the handler lives in a process-wide static registration.
pub type RequestHandler = dyn Fn(Request) + Send + Sync + 'static;

/// Register the tree builder, the action-request handler and the
/// window-closed callback as one indivisible publication.
///
/// This is the supported registration: the three halves are the same owner's
/// view of one window's lifetime — publish, route, release. Returns
/// [`AlreadyRegistered`] without installing *anything* if any half already has
/// an owner, so a partial bridge is never left behind.
pub fn try_register_bridge(bridge: Bridge) -> Result<(), AlreadyRegistered> {
    REGISTRY.publish(bridge)
}

/// Whether a window-closed callback has been registered.
pub fn has_window_closed_handler() -> bool {
    REGISTRY.has_window_closed_handler()
}

/// Notify the registered owner that `window` has been destroyed, so it can
/// release everything keyed by that exact id. Called by the event loop from
/// the one site that removes a window from the window manager.
///
/// Crate-internal: the event loop is the only thing that knows a window died,
/// so this is not part of the product API. A hosted test reaches it through
/// the `a11y-test-hooks` feature.
pub(crate) fn dispatch_window_closed(window: window::Id) {
    REGISTRY.dispatch_window_closed(window);
}

/// Read-only entry points into the registered bridge, for a hosted test that
/// stands in for the event loop.
///
/// Compiled **only** with the explicit `a11y-test-hooks` feature; the default
/// `a11y` API surface exposes neither the frame-publish call nor the
/// window-closed edge, because in production both
/// belong to the event loop (enzUI-ICED #2680). Nothing here adds behaviour —
/// each function forwards to the same private call the event loop makes.
#[cfg(feature = "a11y-test-hooks")]
pub mod test_hooks {
    use super::{Operation, Request, TreeUpdate, window};

    /// The frame publish `WindowAccessibility::update` performs.
    pub fn collect_tree_from_registered_bridge(
        window: window::Id,
        title: Option<&str>,
        run: &mut dyn FnMut(&mut dyn Operation),
    ) -> Option<TreeUpdate> {
        super::collect_tree(window, title, run)
    }

    /// The window-closed edge `run_action` dispatches on window removal.
    pub fn dispatch_window_closed_to_registered_bridge(window: window::Id) {
        super::dispatch_window_closed(window);
    }

    /// The action-request delivery the event loop performs. Present here too
    /// so a hosted test names one hook module rather than mixing surfaces.
    pub fn dispatch_request_to_registered_bridge(request: Request) {
        super::dispatch_request(request);
    }
}

/// A process-global accessibility registration already has an owner.
///
/// Returned by [`try_set_tree_builder`] / [`try_set_request_handler`] so a
/// second registration is a reported outcome rather than a silently discarded
/// closure: an application that lost one half of the pair has an inert or
/// unroutable accessibility tree and no other symptom (enzUI-ICED #2680).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyRegistered;

impl std::fmt::Display for AlreadyRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this accessibility registration already has an owner")
    }
}

impl std::error::Error for AlreadyRegistered {}

/// Register the handler that receives AccessKit action requests (click, focus,
/// set-value, increment, decrement, scroll-into-view, …). Optional: without it,
/// requests are dropped (the AT tree is still exposed read-only). Call once
/// before [`crate::run`].
///
/// A second call is ignored. Prefer [`try_set_request_handler`], which reports
/// that case instead.
pub fn set_request_handler<F>(handler: F)
where
    F: Fn(Request) + Send + Sync + 'static,
{
    let _ = try_set_request_handler(handler);
}

/// Register only the action-request half, reporting an existing owner.
///
/// Legacy seam: it publishes a [`Bridge`] carrying only this half through the
/// same single slot, so it cannot interleave with a bridge registration — but
/// a caller that installs one half alone still leaves the other halves
/// unowned, and every later registration is refused. New owners register a
/// whole [`Bridge`].
pub fn try_set_request_handler<F>(handler: F) -> Result<(), AlreadyRegistered>
where
    F: Fn(Request) + Send + Sync + 'static,
{
    REGISTRY.publish(Bridge::request_only(Box::new(handler)))
}

/// Whether an action-request handler has been registered.
///
/// The counterpart of [`is_enabled`] for the request side. Without it a
/// framework bootstrap cannot tell that its own handler registration was
/// dropped in favour of an earlier one.
pub fn has_request_handler() -> bool {
    REGISTRY.has_request_handler()
}

/// Deliver an action request to the registered handler, if any. Called by the
/// event loop as requests are drained from each window's adapter.
pub fn dispatch_request(request: Request) {
    REGISTRY.dispatch_request(request);
}

/// Register the accessibility tree builder. Call once before [`crate::run`].
///
/// A second call is ignored — the first registration wins, matching iced's
/// single-`run` model. Prefer [`try_set_tree_builder`], which reports that case
/// instead of discarding the closure silently.
pub fn set_tree_builder<F>(builder: F)
where
    F: Fn(window::Id, &mut dyn FnMut(&mut dyn Operation)) -> Option<TreeUpdate>
        + Send
        + Sync
        + 'static,
{
    let _ = try_set_tree_builder(builder);
}

/// Register only the tree-builder half, reporting an existing owner.
///
/// Legacy seam with the same single-slot publication and the same caveat as
/// [`try_set_request_handler`]: prefer [`try_register_bridge`].
pub fn try_set_tree_builder<F>(builder: F) -> Result<(), AlreadyRegistered>
where
    F: Fn(window::Id, &mut dyn FnMut(&mut dyn Operation)) -> Option<TreeUpdate>
        + Send
        + Sync
        + 'static,
{
    // Wrap the 2-arg legacy callback into the 3-arg TreeBuilder (the title
    // parameter is simply ignored by legacy callers).
    REGISTRY.publish(Bridge::tree_only(Box::new(
        move |window, _title: Option<&str>, run: &mut dyn FnMut(&mut dyn Operation)| {
            builder(window, run)
        },
    )))
}

/// Whether an application tree builder has been registered. When `false`, the
/// whole subsystem stays inert (no adapters, no per-frame work).
pub fn is_enabled() -> bool {
    REGISTRY.has_tree_builder()
}

/// Collect one window's tree from the registered builder, if any.
///
/// The read side of the registered [`Bridge`], and the exact call the event
/// loop makes every time a window adapter publishes a frame: `run` applies the
/// owner's collecting operation to that window's live user interface.
///
/// Private: publishing a frame is the window adapter's job, so this is not a
/// product API. `test_hooks` re-exports it behind the `a11y-test-hooks`
/// feature for a hosted test that has no native window.
fn collect_tree(
    window: window::Id,
    title: Option<&str>,
    run: &mut dyn FnMut(&mut dyn Operation),
) -> Option<TreeUpdate> {
    REGISTRY.collect_tree(window, title, run)
}

/// A platform action request routed back from AccessKit, tagged with the exact
/// iced window whose tree received it. The application can correlate
/// `request.target_node` (an `accesskit::NodeId`) with the widget `Id` from
/// that window's published tree.
#[derive(Debug)]
pub struct Request {
    /// The iced window the request targets.
    ///
    /// This is deliberately the same identity passed to [`TreeBuilder`]. A
    /// winit `WindowId` here cannot be reconciled with the framework's iced id
    /// without a private adapter-side lookup, and hashing the two unrelated id
    /// types does not make them equal.
    pub window: window::Id,
    /// The AccessKit action request (kind + target node + optional data).
    pub request: ActionRequest,
}

/// The per-window accessibility state owned by each `Window` in the manager.
///
/// Holds the winit adapter plus the shared inbox and lifecycle state written by
/// direct AccessKit handlers. The handlers may run on platform threads, so they
/// only change this small synchronized state and wake the event loop; they never
/// inspect the live widget tree.
pub struct WindowAccessibility {
    geometry_waker: std::task::Waker,
    adapter: Adapter,
    lifecycle: Arc<Mutex<Lifecycle>>,
    /// Queued action requests, drained by the event loop each turn.
    inbox: Arc<Mutex<Vec<ActionRequest>>>,
    window_id: winit::window::WindowId,
}

/// AccessKit's current interest in one window.
///
/// `accesskit_winit::Adapter::update_if_active` does not expose a query and its
/// callback must return a `TreeUpdate`; our collector may instead return
/// `None` to skip a frame. Therefore the callback cannot safely be used as the
/// only collection gate. The direct lifecycle handlers drive this explicit
/// flag, while the event-loop thread owns all resulting collection and redraw.
#[derive(Debug, Default)]
struct Lifecycle {
    active: bool,
    activation_frame_pending: bool,
    geometry_pending: bool,
}

struct GeometryWake {
    lifecycle: std::sync::Weak<Mutex<Lifecycle>>,
    wake_loop: Arc<dyn Fn() + Send + Sync>,
}

impl std::task::Wake for GeometryWake {
    fn wake(self: Arc<Self>) {
        let Some(lifecycle) = self.lifecycle.upgrade() else {
            return;
        };
        let Ok(mut state) = lifecycle.try_lock() else {
            return;
        };
        if !state.active || state.geometry_pending {
            return;
        }
        state.geometry_pending = true;
        drop(state);
        (self.wake_loop)();
    }
}

impl std::fmt::Debug for WindowAccessibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowAccessibility")
            .field("window_id", &self.window_id)
            .finish_non_exhaustive()
    }
}

impl WindowAccessibility {
    /// Create the adapter for `window`. **Must be called while the window is
    /// still invisible** (AccessKit panics otherwise); iced creates windows
    /// with `.with_visible(false)` and shows them later, so the correct call
    /// site is right after `create_window`, before `set_visible(true)`.
    ///
    /// Returns `None` when no tree builder is registered (feature inert).
    /// `waker` is a type-erased closure that wakes the event loop.  The
    /// caller constructs it from the winit `EventLoopProxy` so that
    /// `ActionBridge::do_action` — called on the AT-SPI thread — can kick
    /// the loop out of `ControlFlow::Wait` and trigger a drain.
    pub fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Self> {
        if !is_enabled() {
            return None;
        }

        let window_id = window.id();
        let lifecycle = Arc::new(Mutex::new(Lifecycle::default()));
        let inbox: Arc<Mutex<Vec<ActionRequest>>> = Arc::new(Mutex::new(Vec::new()));

        let activation = ActivationBridge {
            lifecycle: lifecycle.clone(),
            waker: waker.clone(),
        };
        let action = ActionBridge {
            inbox: inbox.clone(),
            waker: waker.clone(),
        };
        let geometry_waker = std::task::Waker::from(Arc::new(GeometryWake {
            lifecycle: Arc::downgrade(&lifecycle),
            wake_loop: waker.clone(),
        }));
        let deactivation = DeactivationBridge {
            lifecycle: lifecycle.clone(),
            waker,
        };

        let adapter =
            Adapter::with_direct_handlers(event_loop, window, activation, action, deactivation);

        Some(Self {
            adapter,
            lifecycle,
            geometry_waker,
            inbox,
            window_id,
        })
    }

    /// Bind a lazy text provider to a node in this exact window adapter.
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    pub fn register_virtual_text_provider(
        &self,
        tree: accesskit::TreeId,
        node: accesskit::NodeId,
        provider: &Arc<dyn virtual_text::VirtualTextProvider>,
    ) -> Result<virtual_text::VirtualTextRegistration, virtual_text::VirtualTextError> {
        self.adapter
            .register_virtual_text_provider(tree, node, provider)
    }

    /// Feed a winit window event to the adapter. Must be called for every
    /// window event before iced handles it (per AccessKit's contract).
    pub fn process_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        self.adapter.process_event(window, event);
    }

    /// Publishes the application collector's tree only while this window has active accessibility.
    /// `run` visits the live widget tree; logical bounds are scaled to physical pixels.
    /// A builder returning `None` skips publication. Collection and scaling hold the
    /// lifecycle lock, so deactivation prevents later visits; AccessKit's second gate
    /// runs after unlocking to avoid lock inversion.
    pub fn update(
        &mut self,
        window: crate::core::window::Id,
        title: &str,
        scale_factor: f64,
        run: &mut dyn FnMut(&mut dyn Operation),
    ) {
        self.update_ready(window, title, scale_factor, &mut |operation| {
            if let Some(operation) = operation {
                run(operation);
            }
            true
        });
    }

    pub(crate) fn geometry_waker(&self) -> std::task::Waker {
        self.geometry_waker.clone()
    }

    pub(crate) fn take_geometry_request(&mut self) -> bool {
        take_geometry_request(&self.lifecycle)
    }

    pub(crate) fn update_ready(
        &mut self,
        window: crate::core::window::Id,
        title: &str,
        scale_factor: f64,
        run: &mut dyn FnMut(Option<&mut dyn Operation>) -> bool,
    ) {
        let update = collect_synchronized_tree_if_active(
            &self.lifecycle,
            window,
            title,
            scale_factor,
            run,
            |window, title, run| {
                crate::accessibility_publication::collect_if_ready(
                    true,
                    run,
                    |run| run(None),
                    |run| {
                        let mut valid = true;
                        let update = collect_tree(window, title, &mut |operation| {
                            if valid {
                                valid = run(Some(operation));
                            }
                        });
                        if valid { update } else { None }
                    },
                )
            },
        );
        if let Some(update) = update {
            // Registrations belong to this publishing window's adapter, never a synthetic one.
            #[cfg(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            {
                let adapter = &self.adapter;
                virtual_text_mailbox::bind(
                    window,
                    adapter.virtual_text_adapter_identity(),
                    update.tree_id,
                    |tree, node, provider| {
                        adapter.register_virtual_text_provider(tree, node, provider)
                    },
                );
            }
            let _ = window;
            self.adapter.update_if_active(|| update);
        }
    }

    /// Consume the one redraw requested by a new AccessKit activation.
    ///
    /// Called on the event-loop thread before its idle guard. Returning `true`
    /// makes the caller request a frame for this window, ensuring the pending
    /// AccessKit adapter receives a complete current tree even when the app is
    /// otherwise idle.
    pub fn take_activation_frame_request(&mut self) -> bool {
        take_activation_frame_request(&self.lifecycle)
    }

    /// Drain queued action requests, tagging each with the exact iced window
    /// whose adapter owns this inbox.
    pub fn take_requests(&mut self, window: window::Id) -> Vec<Request> {
        let Ok(mut inbox) = self.inbox.lock() else {
            return Vec::new();
        };
        inbox
            .drain(..)
            .map(|request| Request { window, request })
            .collect()
    }
}

fn collect_synchronized_tree_if_active<C: ?Sized, B>(
    lifecycle: &Arc<Mutex<Lifecycle>>,
    window: window::Id,
    title: &str,
    scale_factor: f64,
    run: &mut C,
    builder: B,
) -> Option<TreeUpdate>
where
    B: FnOnce(window::Id, Option<&str>, &mut C) -> Option<TreeUpdate>,
{
    collect_if_active(lifecycle, scale_factor, run, |run| {
        builder(window, Some(title), run)
    })
}

fn take_activation_frame_request(lifecycle: &Arc<Mutex<Lifecycle>>) -> bool {
    let Ok(mut lifecycle) = lifecycle.lock() else {
        return false;
    };

    if lifecycle.active && lifecycle.activation_frame_pending {
        lifecycle.activation_frame_pending = false;
        true
    } else {
        false
    }
}

fn collect_if_active<C: ?Sized, B>(
    lifecycle: &Arc<Mutex<Lifecycle>>,
    scale_factor: f64,
    run: &mut C,
    builder: B,
) -> Option<TreeUpdate>
where
    B: FnOnce(&mut C) -> Option<TreeUpdate>,
{
    let lifecycle = lifecycle.lock().ok()?;
    crate::accessibility_publication::collect_if_ready(
        lifecycle.active,
        run,
        |_| true,
        |run| {
            let mut update = builder(run)?;
            scale_bounds(&mut update, scale_factor);
            Some(update)
        },
    )
}

/// Scale every node's logical bounds to physical pixels in place. AccessKit
/// consumers work in physical coordinates; iced lays out in logical ones, so a
/// window at a non-unit scale factor would otherwise report bounds at the wrong
/// size and position (Review #162, Medium). A scale of `1.0` leaves the tree
/// untouched.
fn scale_bounds(update: &mut TreeUpdate, scale_factor: f64) {
    if (scale_factor - 1.0).abs() < f64::EPSILON {
        return;
    }
    for (_, node) in update.nodes.iter_mut() {
        if let Some(bounds) = node.bounds() {
            node.set_bounds(accesskit::Rect {
                x0: bounds.x0 * scale_factor,
                y0: bounds.y0 * scale_factor,
                x1: bounds.x1 * scale_factor,
                y1: bounds.y1 * scale_factor,
            });
        }
    }
}

/// Marks this window active and wakes the event loop for a fresh frame.
///
/// The live widget tree is event-loop-owned, so this potentially foreign-thread
/// callback intentionally returns `None`. AccessKit supplies its own temporary
/// placeholder until the forced frame publishes a complete current tree.
struct ActivationBridge {
    lifecycle: Arc<Mutex<Lifecycle>>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl accesskit::ActivationHandler for ActivationBridge {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        if let Ok(mut lifecycle) = self.lifecycle.lock() {
            lifecycle.active = true;
            lifecycle.activation_frame_pending = true;
        }
        (self.waker)();
        None
    }
}

/// Queues action requests for the event loop to drain.
///
/// Holds a type-erased wake handle so that `do_action` — called on the AT-SPI
/// / platform accessibility thread — can wake the event loop.  Without it the
/// inbox would sit unnoticed until an unrelated window event happened to fire,
/// which for a screen-reader-only user might be *never*.
struct ActionBridge {
    inbox: Arc<Mutex<Vec<ActionRequest>>>,
    /// Type-erased closure that wakes the event loop.  The concrete closure
    /// captures a `winit::event_loop::EventLoopProxy` and calls `send_event`;
    /// type-erasing it here keeps the accessibility module free of the
    /// `Action<Message>` generic.
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl accesskit::ActionHandler for ActionBridge {
    fn do_action(&mut self, request: ActionRequest) {
        if let Ok(mut inbox) = self.inbox.lock() {
            inbox.push(request);
        }
        // Wake the event loop so the request is drained promptly even when
        // no window event is pending.  This is the fix for enzUI-ICED #309:
        // previously the push notified nothing and the inbox could sit
        // indefinitely while `ControlFlow::Wait` kept the loop parked.
        (self.waker)();
    }
}

/// Stops collection for this window and cancels an unconsumed activation frame.
///
/// There is deliberately no cached tree to retain: a later activation must
/// force a frame and publish current widget state rather than reusing a stale
/// pre-deactivation snapshot.
struct DeactivationBridge {
    lifecycle: Arc<Mutex<Lifecycle>>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl accesskit::DeactivationHandler for DeactivationBridge {
    fn deactivate_accessibility(&mut self) {
        if let Ok(mut lifecycle) = self.lifecycle.lock() {
            lifecycle.active = false;
            lifecycle.activation_frame_pending = false;
        }
        // Ensure an event-loop turn observes the lifecycle transition even if
        // the application and all windows are otherwise idle.
        (self.waker)();
    }
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
mod virtual_text_route_tests {
    use super::virtual_text::{Cancellation, VirtualTextError, VirtualTextProvider};
    use super::{register_virtual_text_provider, virtual_text_mailbox};
    use crate::core::window;
    use std::collections::HashSet;
    use std::ops::Range;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A provider that answers nothing: these tests are about *which* provider
    /// reaches *which* window's adapter, not about document contents (those
    /// consequences live in the adapter and in `enzui-iced-core::virtual_text`).
    struct Inert;

    impl VirtualTextProvider for Inert {
        fn summary(&self) -> Result<super::virtual_text::VirtualTextSummary, VirtualTextError> {
            Err(VirtualTextError::Unavailable)
        }

        fn read(
            &self,
            _document: u128,
            _revision: u64,
            _characters: Range<u64>,
            _cancellation: &Cancellation,
        ) -> Result<super::virtual_text::VirtualTextRange, VirtualTextError> {
            Err(VirtualTextError::Unavailable)
        }

        fn set_selection(
            &self,
            _document: u128,
            _revision: u64,
            _anchor: Option<u64>,
            _focus: u64,
            _cancellation: &Cancellation,
        ) -> Result<u64, VirtualTextError> {
            Err(VirtualTextError::Unavailable)
        }

        fn replace(
            &self,
            _document: u128,
            _revision: u64,
            _characters: Range<u64>,
            _value: &str,
            _cancellation: &Cancellation,
        ) -> Result<u64, VirtualTextError> {
            Err(VirtualTextError::Unavailable)
        }
    }

    fn provider() -> Arc<dyn VirtualTextProvider> {
        Arc::new(Inert)
    }

    /// Stands in for the real adapter registration token so the ownership
    /// consequences (bind, replace, withdraw) are observable without a native
    /// window or an AT-SPI bus.
    struct StrictToken {
        id: usize,
        selector: (accesskit::TreeId, accesskit::NodeId),
        live: Arc<Mutex<HashSet<(accesskit::TreeId, accesskit::NodeId)>>>,
        dropped: Arc<Mutex<Vec<usize>>>,
    }

    impl Drop for StrictToken {
        fn drop(&mut self) {
            let _ = self.live.lock().unwrap().remove(&self.selector);
            self.dropped.lock().unwrap().push(self.id);
        }
    }

    /// Enforces the production adapter's exact-selector rule: a second live
    /// registration is refused, rather than silently replacing the first.
    struct StrictAdapter {
        identity: usize,
        bound: Arc<Mutex<Vec<(accesskit::TreeId, accesskit::NodeId, usize)>>>,
        live: Arc<Mutex<HashSet<(accesskit::TreeId, accesskit::NodeId)>>>,
        dropped: Arc<Mutex<Vec<usize>>>,
        next: AtomicUsize,
        fail_next: AtomicUsize,
    }

    impl Default for StrictAdapter {
        fn default() -> Self {
            static NEXT_ADAPTER: AtomicUsize = AtomicUsize::new(1);
            Self {
                identity: NEXT_ADAPTER.fetch_add(1, Ordering::Relaxed),
                bound: Arc::default(),
                live: Arc::default(),
                dropped: Arc::default(),
                next: AtomicUsize::new(0),
                fail_next: AtomicUsize::new(0),
            }
        }
    }

    impl StrictAdapter {
        /// Publish the root tree exactly as `WindowAccessibility::update` does.
        fn publish(&self, window: window::Id) {
            self.publish_tree(window, accesskit::TreeId::ROOT);
        }

        fn publish_tree(&self, window: window::Id, published_tree: accesskit::TreeId) {
            virtual_text_mailbox::bind(
                window,
                self.identity,
                published_tree,
                |tree, node, provider| {
                    if self.fail_next.swap(0, Ordering::Relaxed) > 0 {
                        return Err(VirtualTextError::Unavailable);
                    }
                    let selector = (tree, node);
                    if !self.live.lock().unwrap().insert(selector) {
                        return Err(VirtualTextError::Unavailable);
                    }
                    let id = self.next.fetch_add(1, Ordering::Relaxed);
                    self.bound.lock().unwrap().push((
                        tree,
                        node,
                        Arc::as_ptr(provider) as *const () as usize,
                    ));
                    Ok(StrictToken {
                        id,
                        selector,
                        live: self.live.clone(),
                        dropped: self.dropped.clone(),
                    })
                },
            );
        }

        fn fail_once(&self) {
            self.fail_next.store(1, Ordering::Relaxed);
        }

        fn bound(&self) -> Vec<(accesskit::TreeId, accesskit::NodeId, usize)> {
            self.bound.lock().unwrap().clone()
        }

        fn dropped(&self) -> Vec<usize> {
            self.dropped.lock().unwrap().clone()
        }
    }

    fn address(provider: &Arc<dyn VirtualTextProvider>) -> usize {
        Arc::as_ptr(provider) as *const () as usize
    }

    #[test]
    fn a_registration_binds_on_the_windows_own_default_publish() {
        let window = window::Id::unique();
        let other = window::Id::unique();
        let document = provider();
        let handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(7),
            &document,
        );

        let adapter = StrictAdapter::default();
        adapter.publish(other);
        assert!(
            adapter.bound().is_empty(),
            "another window's publish must not bind this window's provider"
        );
        assert!(!handle.is_bound());

        adapter.publish(window);
        assert_eq!(
            adapter.bound(),
            vec![(
                accesskit::TreeId::ROOT,
                accesskit::NodeId(7),
                address(&document)
            )]
        );
        assert!(handle.is_bound());

        // Bound exactly once: a later frame must not re-register.
        adapter.publish(window);
        assert_eq!(adapter.bound().len(), 1);
    }

    /// #2677 consequence: a provider registered *during collection* — which
    /// is when the tree builder runs, and the only moment the publishing
    /// window id is available — is bound by the SAME
    /// `WindowAccessibility::update`, not the next one.
    ///
    /// This is the ordering `update()` really has: `collect_if_active` (which
    /// calls the app builder, where the registration is made) runs first, then
    /// `virtual_text_mailbox::bind` a few lines later in the same call. An app
    /// whose document exists before any window therefore becomes reachable on
    /// the first publish rather than after a wasted frame.
    #[test]
    fn a_provider_registered_during_collection_binds_in_the_same_update() {
        let window = window::Id::unique();
        let document = provider();
        let adapter = StrictAdapter::default();

        // Step 1 — inside `collect_if_active`, holding the exact window id.
        let handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(2677),
            &document,
        );
        assert!(
            !handle.is_bound(),
            "collection cannot have bound it yet; reporting bound here would be a lie"
        );
        assert!(adapter.bound().is_empty());

        // Step 2 — the remainder of that same `update()`.
        adapter.publish(window);

        assert!(
            handle.is_bound(),
            "the first publish must bind a provider registered during its own collection"
        );
        assert_eq!(
            adapter.bound(),
            vec![(
                accesskit::TreeId::ROOT,
                accesskit::NodeId(2677),
                address(&document)
            )]
        );
    }

    /// #2677 per-window teardown: dropping one window's handle withdraws only
    /// that window. The same widget id published in a second window keeps
    /// answering there, which is what makes a window-agnostic app-side
    /// registration safe.
    #[test]
    fn withdrawing_one_window_leaves_another_windows_provider_bound() {
        let first = window::Id::unique();
        let second = window::Id::unique();
        let document = provider();
        let node = accesskit::NodeId(2677);

        let first_handle =
            register_virtual_text_provider(first, accesskit::TreeId::ROOT, node, &document);
        let second_handle =
            register_virtual_text_provider(second, accesskit::TreeId::ROOT, node, &document);

        let first_adapter = StrictAdapter::default();
        let second_adapter = StrictAdapter::default();
        first_adapter.publish(first);
        second_adapter.publish(second);
        assert!(first_handle.is_bound() && second_handle.is_bound());

        // The node left the first window's tree: that handle is dropped.
        drop(first_handle);
        assert_eq!(
            first_adapter.dropped().len(),
            1,
            "the first window's platform token must be released"
        );
        assert!(
            second_handle.is_bound(),
            "the second window's provider must stay reachable"
        );
        assert!(
            second_adapter.dropped().is_empty(),
            "no token belonging to the second window may be released"
        );
    }

    #[test]
    fn dropping_the_handle_withdraws_before_and_after_binding() {
        let never = window::Id::unique();
        let document = provider();
        drop(register_virtual_text_provider(
            never,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(1),
            &document,
        ));

        let adapter = StrictAdapter::default();
        adapter.publish(never);
        assert!(
            adapter.bound().is_empty(),
            "a withdrawn request must never reach an adapter"
        );

        let live = window::Id::unique();
        let handle = register_virtual_text_provider(
            live,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(1),
            &document,
        );
        adapter.publish(live);
        assert_eq!(adapter.bound().len(), 1);
        assert!(adapter.dropped().is_empty());

        drop(handle);
        assert_eq!(
            adapter.dropped(),
            vec![0],
            "dropping the handle must release the adapter registration token"
        );
    }

    #[test]
    fn re_registering_the_same_node_replaces_the_provider() {
        let window = window::Id::unique();
        let first = provider();
        let second = provider();

        let first_handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(3),
            &first,
        );
        let adapter = StrictAdapter::default();
        adapter.publish(window);

        let second_handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(3),
            &second,
        );
        adapter.publish(window);

        assert_eq!(
            adapter
                .bound()
                .into_iter()
                .map(|(_, _, provider)| provider)
                .collect::<Vec<_>>(),
            vec![address(&first), address(&second)],
            "the replacement must register in order on the same node"
        );

        drop(first_handle);
        assert_eq!(
            adapter.dropped(),
            vec![0],
            "an old handle must not remove the replacement's token"
        );
        assert!(second_handle.is_bound());
    }

    #[test]
    fn a_failed_platform_bind_stays_pending_and_retries_next_publish() {
        let window = window::Id::unique();
        let document = provider();
        let handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(31),
            &document,
        );
        let adapter = StrictAdapter::default();
        adapter.fail_once();

        adapter.publish(window);
        assert!(!handle.is_bound());
        assert!(adapter.bound().is_empty());

        adapter.publish(window);
        assert!(handle.is_bound());
        assert_eq!(adapter.bound().len(), 1);
    }

    #[test]
    fn adapter_recreation_releases_the_old_token_before_rebinding() {
        let window = window::Id::unique();
        let document = provider();
        let handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(32),
            &document,
        );
        let old_adapter = StrictAdapter::default();
        old_adapter.publish(window);
        assert!(handle.is_bound());

        let new_adapter = StrictAdapter::default();
        new_adapter.publish(window);
        assert_eq!(old_adapter.dropped(), vec![0]);
        assert_eq!(new_adapter.bound().len(), 1);
        assert!(handle.is_bound());
    }

    #[test]
    fn dropping_a_handle_during_bind_drops_the_just_created_token() {
        let window = window::Id::unique();
        let document = provider();
        let handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(33),
            &document,
        );
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let dropped = Arc::new(Mutex::new(Vec::new()));

        std::thread::scope(|scope| {
            let worker_entered = entered.clone();
            let worker_release = release.clone();
            let worker_dropped = dropped.clone();
            let worker = scope.spawn(move || {
                virtual_text_mailbox::bind(
                    window,
                    2677,
                    accesskit::TreeId::ROOT,
                    |_tree, _node, _provider| {
                        let _ = worker_entered.wait();
                        let _ = worker_release.wait();
                        Ok::<_, VirtualTextError>(StrictToken {
                            id: 77,
                            selector: (accesskit::TreeId::ROOT, accesskit::NodeId(33)),
                            live: Arc::new(Mutex::new(HashSet::new())),
                            dropped: worker_dropped.clone(),
                        })
                    },
                );
            });
            let _ = entered.wait();
            drop(handle);
            let _ = release.wait();
            worker.join().unwrap();
        });

        assert_eq!(*dropped.lock().unwrap(), vec![77]);
    }

    #[test]
    fn identical_node_ids_in_two_windows_bind_to_their_own_adapters() {
        let left_window = window::Id::unique();
        let right_window = window::Id::unique();
        let left = provider();
        let right = provider();
        let node = accesskit::NodeId(42);

        let _left_handle =
            register_virtual_text_provider(left_window, accesskit::TreeId::ROOT, node, &left);
        let _right_handle =
            register_virtual_text_provider(right_window, accesskit::TreeId::ROOT, node, &right);

        let left_adapter = StrictAdapter::default();
        let right_adapter = StrictAdapter::default();
        left_adapter.publish(left_window);
        right_adapter.publish(right_window);

        assert_eq!(
            left_adapter.bound(),
            vec![(accesskit::TreeId::ROOT, node, address(&left))]
        );
        assert_eq!(
            right_adapter.bound(),
            vec![(accesskit::TreeId::ROOT, node, address(&right))]
        );
    }

    #[test]
    fn a_dropped_provider_is_not_resurrected_by_a_queued_request() {
        let window = window::Id::unique();
        let document = provider();
        let weak = Arc::downgrade(&document);
        let _handle = register_virtual_text_provider(
            window,
            accesskit::TreeId::ROOT,
            accesskit::NodeId(9),
            &document,
        );
        drop(document);

        let adapter = StrictAdapter::default();
        adapter.publish(window);
        assert!(adapter.bound().is_empty());
        assert!(weak.upgrade().is_none(), "the mailbox must hold a Weak");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::{Node, NodeId, Rect, Role, Tree, TreeId, TreeUpdate};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tree_with_bounds(rect: Rect) -> TreeUpdate {
        let mut node = Node::new(Role::Button);
        node.set_bounds(rect);
        TreeUpdate {
            nodes: vec![(NodeId(1), node)],
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        }
    }

    #[test]
    fn scale_bounds_multiplies_all_corners() {
        let mut update = tree_with_bounds(Rect {
            x0: 10.0,
            y0: 20.0,
            x1: 50.0,
            y1: 60.0,
        });
        scale_bounds(&mut update, 2.0);
        let b = update.nodes[0].1.bounds().unwrap();
        assert_eq!(b.x0, 20.0);
        assert_eq!(b.y0, 40.0);
        assert_eq!(b.x1, 100.0);
        assert_eq!(b.y1, 120.0);
    }

    #[test]
    fn scale_bounds_unit_scale_is_noop() {
        let rect = Rect {
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut update = tree_with_bounds(rect);
        scale_bounds(&mut update, 1.0);
        let b = update.nodes[0].1.bounds().unwrap();
        assert_eq!(b.x0, 1.0);
        assert_eq!(b.x1, 3.0);
    }

    #[test]
    fn scale_bounds_fractional_scale() {
        let mut update = tree_with_bounds(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        });
        scale_bounds(&mut update, 1.5);
        let b = update.nodes[0].1.bounds().unwrap();
        assert_eq!(b.x1, 150.0);
        assert_eq!(b.y1, 150.0);
    }

    struct ProbeOperation;

    impl Operation for ProbeOperation {
        fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}
    }

    fn inactive_lifecycle() -> Arc<Mutex<Lifecycle>> {
        Arc::new(Mutex::new(Lifecycle::default()))
    }

    fn activate(
        lifecycle: &Arc<Mutex<Lifecycle>>,
        wake_count: &Arc<AtomicUsize>,
    ) -> ActivationBridge {
        let wake_count = wake_count.clone();
        ActivationBridge {
            lifecycle: lifecycle.clone(),
            waker: Arc::new(move || {
                let _ = wake_count.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    fn deactivate(
        lifecycle: &Arc<Mutex<Lifecycle>>,
        wake_count: &Arc<AtomicUsize>,
    ) -> DeactivationBridge {
        let wake_count = wake_count.clone();
        DeactivationBridge {
            lifecycle: lifecycle.clone(),
            waker: Arc::new(move || {
                let _ = wake_count.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    #[test]
    fn inactive_at_invokes_no_collector_and_visits_no_semantic_nodes() {
        for semantic_node_count in [100, 1_000, 10_000] {
            let lifecycle = inactive_lifecycle();
            let collector_calls = AtomicUsize::new(0);
            let semantic_visits = AtomicUsize::new(0);
            let mut run = |_operation: &mut dyn Operation| {
                let _ = semantic_visits.fetch_add(semantic_node_count, Ordering::SeqCst);
            };

            for _ in 0..1_000 {
                let update = collect_if_active(&lifecycle, 2.0, &mut run, |run| {
                    let _ = collector_calls.fetch_add(1, Ordering::SeqCst);
                    let mut operation = ProbeOperation;
                    run(&mut operation);
                    Some(tree_with_bounds(Rect {
                        x0: 1.0,
                        y0: 1.0,
                        x1: 2.0,
                        y1: 2.0,
                    }))
                });
                assert!(update.is_none());
            }

            assert_eq!(
                collector_calls.load(Ordering::SeqCst),
                0,
                "inactive {semantic_node_count}-node tree must invoke no collectors",
            );
            assert_eq!(
                semantic_visits.load(Ordering::SeqCst),
                0,
                "inactive {semantic_node_count}-node tree must visit no nodes",
            );
        }
    }

    #[test]
    fn active_at_collects_builds_and_scales_each_profile_frame() {
        let lifecycle = Arc::new(Mutex::new(Lifecycle {
            active: true,
            activation_frame_pending: false,
            geometry_pending: false,
        }));

        for semantic_node_count in [100, 1_000, 10_000] {
            let collector_calls = AtomicUsize::new(0);
            let semantic_visits = AtomicUsize::new(0);
            let nodes_built = AtomicUsize::new(0);
            let nodes_scaled = AtomicUsize::new(0);
            let mut run = |_operation: &mut dyn Operation| {
                let _ = semantic_visits.fetch_add(semantic_node_count, Ordering::SeqCst);
            };

            for _ in 0..1_000 {
                let update = collect_if_active(&lifecycle, 2.0, &mut run, |run| {
                    let _ = collector_calls.fetch_add(1, Ordering::SeqCst);
                    let mut operation = ProbeOperation;
                    run(&mut operation);

                    let mut nodes = Vec::with_capacity(semantic_node_count);
                    for index in 0..semantic_node_count {
                        let mut node = Node::new(if index == 0 {
                            Role::Window
                        } else {
                            Role::Button
                        });
                        node.set_bounds(Rect {
                            x0: index as f64,
                            y0: 1.0,
                            x1: index as f64 + 1.0,
                            y1: 2.0,
                        });
                        nodes.push((NodeId(index as u64 + 1), node));
                    }
                    let _ = nodes_built.fetch_add(nodes.len(), Ordering::SeqCst);
                    Some(TreeUpdate {
                        nodes,
                        tree: Some(Tree::new(NodeId(1))),
                        tree_id: TreeId::ROOT,
                        focus: NodeId(1),
                    })
                })
                .expect("active frame must produce an update");

                assert!(update.nodes.iter().all(|(_, node)| {
                    let bounds = node.bounds().expect("every benchmark node has bounds");
                    bounds.y0 == 2.0 && bounds.y1 == 4.0
                }));
                let _ = nodes_scaled.fetch_add(update.nodes.len(), Ordering::SeqCst);
            }

            assert_eq!(collector_calls.load(Ordering::SeqCst), 1_000);
            assert_eq!(
                semantic_visits.load(Ordering::SeqCst),
                semantic_node_count * 1_000,
            );
            assert_eq!(
                nodes_built.load(Ordering::SeqCst),
                semantic_node_count * 1_000,
            );
            assert_eq!(
                nodes_scaled.load(Ordering::SeqCst),
                semantic_node_count * 1_000,
            );

            let node_storage_bytes =
                std::mem::size_of::<(NodeId, Node)>() * semantic_node_count * 1_000;
            eprintln!(
                "active nodes={semantic_node_count} frames=1000 collectors=1000 \
                 visits={} built={} scaled={} node_vec_allocations>=1000 \
                 node_storage_bytes>={node_storage_bytes}",
                semantic_visits.load(Ordering::SeqCst),
                nodes_built.load(Ordering::SeqCst),
                nodes_scaled.load(Ordering::SeqCst),
            );
        }
    }

    #[test]
    fn activation_wakes_and_forces_one_fresh_complete_tree_frame() {
        use accesskit::ActivationHandler;

        let lifecycle = inactive_lifecycle();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let mut bridge = activate(&lifecycle, &wake_count);

        assert!(bridge.request_initial_tree().is_none());
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);

        assert!(take_activation_frame_request(&lifecycle));
        assert!(!take_activation_frame_request(&lifecycle));

        let collector_calls = AtomicUsize::new(0);
        let mut run = |_operation: &mut dyn Operation| {};
        let update = collect_if_active(&lifecycle, 2.0, &mut run, |_run| {
            let _ = collector_calls.fetch_add(1, Ordering::SeqCst);
            let mut root = Node::new(Role::Window);
            root.set_label("Current window");
            root.set_children(vec![NodeId(2)]);
            let mut button = Node::new(Role::Button);
            button.set_label("Current action");
            button.set_bounds(Rect {
                x0: 2.0,
                y0: 3.0,
                x1: 12.0,
                y1: 13.0,
            });
            Some(TreeUpdate {
                nodes: vec![(NodeId(1), root), (NodeId(2), button)],
                tree: Some(Tree::new(NodeId(1))),
                tree_id: TreeId::ROOT,
                focus: NodeId(2),
            })
        })
        .expect("activation must collect a full current tree");

        assert_eq!(collector_calls.load(Ordering::SeqCst), 1);
        assert_eq!(update.nodes.len(), 2);
        assert_eq!(update.nodes[1].1.label(), Some("Current action"));
        assert_eq!(update.focus, NodeId(2));
        assert_eq!(update.nodes[1].1.bounds().unwrap().x1, 24.0);
    }

    #[test]
    fn deactivate_reactivate_collects_current_tree_and_is_window_local() {
        use accesskit::{ActivationHandler, DeactivationHandler};

        let first = inactive_lifecycle();
        let second = inactive_lifecycle();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let mut first_activation = activate(&first, &wake_count);
        let mut first_deactivation = deactivate(&first, &wake_count);
        let mut second_activation = activate(&second, &wake_count);
        let calls = AtomicUsize::new(0);
        let mut run = |_operation: &mut dyn Operation| {};

        let _ = first_activation.request_initial_tree();
        let old = collect_if_active(&first, 1.0, &mut run, |_run| {
            let _ = calls.fetch_add(1, Ordering::SeqCst);
            let mut node = Node::new(Role::Button);
            node.set_label("old");
            Some(TreeUpdate {
                nodes: vec![(NodeId(1), node)],
                tree: Some(Tree::new(NodeId(1))),
                tree_id: TreeId::ROOT,
                focus: NodeId(1),
            })
        })
        .unwrap();
        assert_eq!(old.nodes[0].1.label(), Some("old"));

        first_deactivation.deactivate_accessibility();
        assert!(
            collect_if_active(&first, 1.0, &mut run, |_run| {
                let _ = calls.fetch_add(1, Ordering::SeqCst);
                Some(tree_with_bounds(Rect::ZERO))
            })
            .is_none()
        );

        // Activating another window must not accidentally reopen the first.
        let _ = second_activation.request_initial_tree();
        assert!(
            collect_if_active(&first, 1.0, &mut run, |_run| {
                let _ = calls.fetch_add(1, Ordering::SeqCst);
                Some(tree_with_bounds(Rect::ZERO))
            })
            .is_none()
        );

        let _ = first_activation.request_initial_tree();
        let current = collect_if_active(&first, 1.0, &mut run, |_run| {
            let _ = calls.fetch_add(1, Ordering::SeqCst);
            let mut root = Node::new(Role::Window);
            root.set_label("new");
            root.set_children(vec![NodeId(3)]);
            let mut inserted = Node::new(Role::Button);
            inserted.set_label("inserted");
            Some(TreeUpdate {
                nodes: vec![(NodeId(1), root), (NodeId(3), inserted)],
                tree: Some(Tree::new(NodeId(1))),
                tree_id: TreeId::ROOT,
                focus: NodeId(3),
            })
        })
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(current.nodes.len(), 2);
        assert_eq!(current.nodes[0].1.label(), Some("new"));
        assert_eq!(current.nodes[1].0, NodeId(3));
        assert_eq!(current.focus, NodeId(3));
        assert!(current.nodes.iter().all(|(id, _)| *id != NodeId(2)));
    }

    // -----------------------------------------------------------------------
    // ActionBridge wake tests (enzUI-ICED #309)
    // -----------------------------------------------------------------------

    /// A fake action request for testing.  `ActionRequest` needs only an
    /// action kind and a target node; the defaults are fine for the waker
    /// test since we only care about the push + wake, not the request
    /// contents.
    fn test_request() -> ActionRequest {
        ActionRequest {
            action: accesskit::Action::Click,
            target_node: NodeId(42),
            target_tree: TreeId::ROOT,
            data: None,
        }
    }

    /// Calibration: prove the inbox receives a pushed request at all.
    /// This passes against both the broken and fixed code (the push was
    /// never the defective part), establishing that the test harness
    /// can observe a drain.
    #[test]
    fn action_bridge_inbox_receives_request() {
        let inbox: Arc<Mutex<Vec<ActionRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let waker_calls = Arc::new(AtomicUsize::new(0));
        let waker_calls2 = waker_calls.clone();
        let mut bridge = ActionBridge {
            inbox: inbox.clone(),
            waker: Arc::new(move || {
                let _ = waker_calls2.fetch_add(1, Ordering::SeqCst);
            }),
        };

        use accesskit::ActionHandler;
        bridge.do_action(test_request());

        let queued = inbox.lock().unwrap();
        assert_eq!(queued.len(), 1, "inbox must contain exactly one request");
        assert_eq!(
            queued[0].target_node,
            NodeId(42),
            "the request must carry the correct target node",
        );
    }

    /// **The test that distinguishes the fix from the broken code.**
    ///
    /// `do_action` must call the waker exactly once per request so the
    /// event loop wakes promptly.  A bridge without the waker call —
    /// the pre-fix code — would leave `waker_calls` at zero.
    ///
    /// This tests the *mechanism* ("the waker is called"), not the
    /// end-to-end loop wake, because constructing a real winit event
    /// loop in a unit test is not feasible in this tree.  The brief
    /// explicitly accepts this: "test the piece you can: that
    /// ActionBridge holds a live wake handle and calls it, with a fake
    /// handle recording the call."
    #[test]
    fn action_bridge_do_action_wakes_the_event_loop() {
        let inbox: Arc<Mutex<Vec<ActionRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let waker_calls = Arc::new(AtomicUsize::new(0));
        let waker_calls2 = waker_calls.clone();
        let mut bridge = ActionBridge {
            inbox: inbox.clone(),
            waker: Arc::new(move || {
                let _ = waker_calls2.fetch_add(1, Ordering::SeqCst);
            }),
        };

        use accesskit::ActionHandler;

        // Before any action, the waker must not have been called.
        assert_eq!(
            waker_calls.load(Ordering::SeqCst),
            0,
            "waker must not fire before do_action",
        );

        bridge.do_action(test_request());
        assert_eq!(
            waker_calls.load(Ordering::SeqCst),
            1,
            "do_action must wake the event loop exactly once",
        );

        // Second request: the counter must increment again.
        bridge.do_action(test_request());
        assert_eq!(
            waker_calls.load(Ordering::SeqCst),
            2,
            "each do_action must wake independently",
        );
    }

    /// Multiple requests each wake independently.
    #[test]
    fn action_bridge_multiple_requests_wake_multiple_times() {
        let inbox: Arc<Mutex<Vec<ActionRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let waker_calls = Arc::new(AtomicUsize::new(0));
        let waker_calls2 = waker_calls.clone();
        let mut bridge = ActionBridge {
            inbox: inbox.clone(),
            waker: Arc::new(move || {
                let _ = waker_calls2.fetch_add(1, Ordering::SeqCst);
            }),
        };

        use accesskit::ActionHandler;
        for _ in 0..5 {
            bridge.do_action(test_request());
        }

        assert_eq!(
            inbox.lock().unwrap().len(),
            5,
            "all five requests must be queued",
        );
        assert_eq!(
            waker_calls.load(Ordering::SeqCst),
            5,
            "waker must fire once per request",
        );
    }

    // -----------------------------------------------------------------------
    // Drain-site placement tests (enzUI-ICED #309, #423 class)
    // -----------------------------------------------------------------------
    //
    // The wake handler only helps if the drain is reachable from the woken
    // turn.  Two failure modes would make the fix a one-link-longer chain
    // with no user-visible improvement (#423):
    //
    //   (a) removing the AboutToWait drain and relying only on the
    //       WindowEvent drain — the idle wake would then land on a turn
    //       with no window event and the request would sit until an
    //       incidental event arrived, exactly the defect this issue fixes;
    //
    //   (b) placing the AboutToWait drain **below** the
    //       `if events.is_empty() && messages.is_empty() && window_manager.is_idle() { continue; }`
    //       guard — the idle-wake turn hits that guard *because* nothing
    //       else fired, `continue`s past the drain, and the loop parks
    //       again with a full inbox.
    //
    // The wake-side unit tests above cannot fail on either (a) or (b): they
    // exercise `ActionBridge::do_action` in isolation and never touch the
    // event loop.  These tests scan the runtime source and pin the two
    // structural properties the fix depends on.
    //
    // They are static (source-scanning) rather than behavioural because a
    // real winit event loop needs a display connection and a top-level
    // `run_app` — not constructible in a `#[cfg(test)]` unit test in this
    // tree.  A brittle text scan that catches the exact hazard is more
    // useful than a broad behavioural test that cannot be written honestly.

    // -----------------------------------------------------------------------
    // Atomic bridge registration (enzUI-ICED #2680)
    // -----------------------------------------------------------------------
    //
    // These drive private `Registry` instances: the production one is a
    // process-global that other tests in this binary may already have written.

    fn probe_bridge(log: Arc<Mutex<Vec<String>>>) -> Bridge {
        let closed = Arc::clone(&log);
        Bridge::new(
            move |_window, _title, _run| None,
            {
                let log = Arc::clone(&log);
                move |_request| log.lock().unwrap().push("request".to_owned())
            },
            move |window| closed.lock().unwrap().push(format!("closed {window:?}")),
        )
    }

    #[test]
    fn synchronized_title_reaches_the_registered_builder_on_window_update() {
        const RUNTIME_SRC: &str = include_str!("lib.rs");
        let state_read = RUNTIME_SRC
            .find("let title = window.state.title().to_owned();")
            .expect("redraw must read its own synchronized window title");
        let update_call = RUNTIME_SRC
            .find("a11y.update_ready(id, &title, scale_factor")
            .expect("redraw must pass that title to WindowAccessibility::update_ready");
        assert!(
            state_read < update_call,
            "State::title must be read before publication"
        );
        assert_eq!(
            RUNTIME_SRC
                .matches("a11y.update_ready(id, &title, scale_factor")
                .count(),
            1,
            "there must be one production accessibility update site",
        );

        let registry = Registry::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let builder_observed = Arc::clone(&observed);
        assert_eq!(
            registry.publish(Bridge::tree_only(Box::new(move |_window, title, _run| {
                builder_observed
                    .lock()
                    .unwrap()
                    .push(title.map(str::to_owned));
                Some(tree_with_bounds(Rect::ZERO))
            }))),
            Ok(()),
        );
        let lifecycle = Arc::new(Mutex::new(Lifecycle {
            active: true,
            activation_frame_pending: false,
            geometry_pending: false,
        }));
        let mut run = |_operation: &mut dyn Operation| {};

        let update = collect_synchronized_tree_if_active(
            &lifecycle,
            window::Id::unique(),
            "synchronized title 2711",
            1.0,
            &mut run,
            |window, title, run| registry.collect_tree(window, title, run),
        );

        assert!(
            update.is_some(),
            "the active production update must publish a tree"
        );
        assert_eq!(
            *observed.lock().unwrap(),
            vec![Some("synchronized title 2711".to_owned())],
            "the registered builder must observe State::title from this redraw",
        );
    }

    #[test]
    fn a_bridge_registration_installs_all_three_halves() {
        let registry = Registry::new();
        let log = Arc::new(Mutex::new(Vec::new()));

        assert_eq!(registry.publish(probe_bridge(log)), Ok(()));

        assert!(registry.has_tree_builder(), "tree builder half");
        assert!(
            registry.has_request_handler(),
            "request handler half: a published tree whose actions vanish is \
             not an accessible application",
        );
        assert!(
            registry.has_window_closed_handler(),
            "window-closed half: without it the owner cannot release the \
             per-window routing table it published",
        );
    }

    /// Publication is indivisible: an earlier one-half writer refuses the
    /// whole bridge, and the bridge must not install its other halves on top.
    /// Sabotage: let `publish` overwrite an occupied slot and the
    /// request/closed assertions red.
    #[test]
    fn a_refused_bridge_installs_no_half_at_all() {
        let registry = Registry::new();
        assert_eq!(
            registry.publish(Bridge::tree_only(Box::new(|_window, _title, _run| None))),
            Ok(()),
        );

        let log = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            registry.publish(probe_bridge(Arc::clone(&log))),
            Err(AlreadyRegistered),
            "a legacy half already owns part of the bridge",
        );
        assert!(
            !registry.has_request_handler() && !registry.has_window_closed_handler(),
            "a refused bridge must leave no half behind: mixed ownership \
             routes one window's actions through another owner's map",
        );

        registry.dispatch_window_closed(window::Id::unique());
        assert!(
            log.lock().unwrap().is_empty(),
            "nothing from the refused bridge may be reachable",
        );
    }

    /// The legacy one-half setters go through the same single slot: once a
    /// bridge owns it, a later half registration is refused rather than
    /// silently dropped — and, crucially, cannot land next to the winner.
    #[test]
    fn legacy_half_setters_are_refused_after_a_bridge_registration() {
        let registry = Registry::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(registry.publish(probe_bridge(log)), Ok(()));

        assert_eq!(
            registry.publish(Bridge::tree_only(Box::new(|_window, _title, _run| None))),
            Err(AlreadyRegistered),
        );
        assert_eq!(
            registry.publish(Bridge::request_only(Box::new(|_request| {}))),
            Err(AlreadyRegistered),
        );
    }

    /// One registration changes from wholly empty to wholly full.
    ///
    /// Each observation is **one** load of the registration slot
    /// ([`Registry::snapshot`]): that is the unit publication is atomic over.
    /// Three separate `has_*` calls are three loads and may legitimately
    /// straddle a publication, so nothing here claims atomicity *across*
    /// public calls. The channel handshake makes both sides of the transition
    /// deterministic instead of hoping a reader happens to win a scheduler
    /// race.
    ///
    /// Sabotage: publish a `tree_only` bridge (or restore separate slots) and
    /// the full-snapshot assertion reds.
    #[test]
    fn one_snapshot_load_observes_all_empty_then_all_full() {
        use std::sync::mpsc;

        let registry = Arc::new(Registry::new());
        let (empty_seen, empty_ack) = mpsc::channel();
        let (published, published_ack) = mpsc::channel();

        let reader = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                let before = registry.snapshot();
                empty_seen.send(()).expect("report empty snapshot");
                published_ack.recv().expect("wait for publication");
                let after = registry.snapshot();
                (before, after)
            })
        };

        empty_ack.recv().expect("reader must observe empty first");
        assert_eq!(
            registry.publish(probe_bridge(Arc::new(Mutex::new(Vec::new())))),
            Ok(()),
        );
        published
            .send(())
            .expect("release reader after publication");

        let (before, after) = reader.join().expect("reader thread");
        assert_eq!(before, Registration::EMPTY);
        assert_eq!(after, Registration::WHOLE);
    }

    const SELF_SRC: &str = include_str!("accessibility.rs");

    /// The frame-publish call and the window-closed edge are the event loop's,
    /// not the product API's. A hosted test needs them, and the cheap way to
    /// give it them — making both `pub` — would ship two new process-global
    /// entry points to every application, one of which can release another
    /// owner's per-window state (enzUI-ICED #2680).
    ///
    /// So they are private/crate-internal and re-exported only behind the
    /// explicit `a11y-test-hooks` feature, absent from the default `a11y`
    /// surface. This is a source check because the absence of a `pub` item cannot be observed
    /// from inside the crate that defines it.
    ///
    /// Sabotage: drop the `#[cfg(feature = "a11y-test-hooks")]` from
    /// `mod test_hooks`, or restore `pub fn collect_tree` /
    /// `pub fn dispatch_window_closed`, and this reds.
    #[test]
    fn the_default_api_surface_exposes_no_publish_or_close_hook() {
        assert!(
            SELF_SRC.contains("fn collect_tree("),
            "positive control: the private frame-publish call must exist",
        );
        let public_collect = concat!("pub fn ", "collect_tree(");
        let public_close = concat!("pub fn ", "dispatch_window_closed(");
        let crate_close = concat!("pub(crate) fn ", "dispatch_window_closed(");
        assert!(
            !SELF_SRC.contains(public_collect),
            "the frame publish must not be product API: the window adapter \
             owns it",
        );
        assert!(
            !SELF_SRC.contains(public_close),
            "the window-closed edge must not be product API: an application \
             calling it would release another owner's per-window state",
        );
        assert!(
            SELF_SRC.contains(crate_close),
            "positive control: the event loop's own crate-internal edge must \
             still exist",
        );

        // Built at run time so this test's own text never matches.
        let module = ["pub mod", "test_hooks {"].join(" ");
        let hooks = SELF_SRC
            .find(&module)
            .expect("the hosted-test hook module must exist");
        let gate = SELF_SRC
            .find(&format!("#[cfg(feature = \"a11y-test-hooks\")]\n{module}"))
            .expect("the hook module must be gated by the test-only feature");
        assert!(
            gate < hooks,
            "the hook module's gate must immediately precede it",
        );
        assert_eq!(
            SELF_SRC.matches(module.as_str()).count(),
            1,
            "exactly one hook module: a second, ungated one would be product \
             API again",
        );
    }

    #[test]
    fn a_registered_bridge_reports_the_exact_closed_window() {
        let registry = Registry::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(registry.publish(probe_bridge(Arc::clone(&log))), Ok(()));

        let window = window::Id::unique();
        let other = window::Id::unique();
        registry.dispatch_window_closed(window);

        assert_eq!(
            *log.lock().unwrap(),
            vec![format!("closed {window:?}")],
            "the callback must receive the destroyed window, not another one",
        );
        assert_ne!(format!("closed {other:?}"), format!("closed {window:?}"));
    }

    const LIB_RS: &str = include_str!("lib.rs");

    /// Both drain sites must exist: one inside the `WindowEvent` arm
    /// (ordering-contract: adapter sees the raw event first) and one in
    /// `AboutToWait` (the idle-wake path this issue fixes).  Removing
    /// either regresses to a partial fix.
    #[test]
    fn drain_sites_both_pass_the_exact_iced_window_to_the_event_loop() {
        let count = LIB_RS.matches("a11y.take_requests(id)").count();
        assert_eq!(
            count, 2,
            "expected exactly two `a11y.take_requests(id)` call sites in \
             winit/src/lib.rs — one in the WindowEvent arm (ordering \
             contract) and one in AboutToWait (#309 idle-wake path); \
             found {count}. Removing either loses the request's iced-window \
             identity or regresses the drain.",
        );
    }

    /// The `AboutToWait` drain must precede the
    /// `events.is_empty() && messages.is_empty() && window_manager.is_idle()`
    /// early-continue guard.  If a future refactor moves it below, an
    /// idle-only wake will `continue` past the drain and the loop parks
    /// with a full inbox — the #423 hazard the issue's brief calls out.
    ///
    /// This is deliberately anchored on the `#309: drain a11y` comment
    /// marker so a refactor that keeps the drain conceptually but drops
    /// the tag also fails loudly (comment lost = test lost = future
    /// silent breakage).
    #[test]
    fn abouttowait_drain_precedes_idle_guard() {
        let drain_marker = "// #309: drain a11y action requests";
        let guard = "if events.is_empty() && messages.is_empty() && window_manager.is_idle() {";

        let drain_at = LIB_RS.find(drain_marker).unwrap_or_else(|| {
            panic!(
                "could not find `{drain_marker}` in winit/src/lib.rs; \
                 the AboutToWait a11y drain block appears to be missing \
                 or its comment tag has been dropped",
            )
        });
        let guard_at = LIB_RS.find(guard).unwrap_or_else(|| {
            panic!(
                "could not find the idle-guard expression in \
                 winit/src/lib.rs; if it was refactored, update this test \
                 to match its new form",
            )
        });

        assert!(
            drain_at < guard_at,
            "the #309 a11y drain block must appear BEFORE the idle-guard \
             `{guard}` in the AboutToWait match arm. \
             Drain marker at byte {drain_at}; guard at byte {guard_at}. \
             With the drain after the guard, an idle screen-reader wake \
             hits the `continue` and skips the drain, so the fix becomes \
             a one-link-longer chain with no user-visible effect (#423).",
        );
    }

    /// Activation's per-window frame request must be consumed before the idle
    /// guard. Otherwise the platform-thread waker reaches `AboutToWait`, the
    /// guard observes an otherwise-idle app and continues, and AccessKit keeps
    /// its placeholder until an unrelated event happens.
    #[test]
    fn activation_redraw_precedes_idle_guard() {
        let redraw = "a11y.take_activation_frame_request()";
        let guard = "if events.is_empty() && messages.is_empty() && window_manager.is_idle() {";
        let redraw_at = LIB_RS
            .find(redraw)
            .unwrap_or_else(|| panic!("missing activation redraw call `{redraw}`"));
        let guard_at = LIB_RS
            .find(guard)
            .unwrap_or_else(|| panic!("missing AboutToWait idle guard `{guard}`"));

        assert_eq!(
            LIB_RS.matches(redraw).count(),
            1,
            "activation redraw must have one unambiguous event-loop call site",
        );
        assert!(
            redraw_at < guard_at,
            "activation redraw at byte {redraw_at} must precede idle guard at byte {guard_at}",
        );
    }

    /// Sanity: `AboutToWait` occurs exactly once (as a match arm), so
    /// the position-comparison test above is unambiguous.  If a future
    /// refactor introduces a second `AboutToWait` handler, revisit both
    /// placement tests to check which one hosts the drain.
    #[test]
    fn abouttowait_arm_appears_once() {
        let count = LIB_RS.matches("event::Event::AboutToWait =>").count();
        assert_eq!(
            count, 1,
            "expected exactly one `event::Event::AboutToWait =>` match arm \
             in winit/src/lib.rs; found {count}. The drain-placement test \
             assumes there is one.",
        );
    }

    /// The window-closed callback is only useful if the event loop actually
    /// fires it where a window is removed. Removing that call leaves every
    /// closed window's routing table (and its queued actions) alive for the
    /// life of the daemon process.
    #[test]
    fn the_event_loop_dispatches_window_closed_at_the_window_removal_site() {
        let removal = LIB_RS
            .find("if window_manager.remove(id).is_some() {")
            .expect("the window manager removal site must exist in winit/src/lib.rs");
        let dispatch = LIB_RS
            .find("accessibility::dispatch_window_closed(id);")
            .expect(
                "winit/src/lib.rs must tell the accessibility owner which \
                 window was destroyed",
            );

        assert!(
            dispatch > removal && dispatch - removal < 600,
            "the window-closed dispatch must sit inside the removal arm \
             (removal at {removal}, dispatch at {dispatch})",
        );
        assert_eq!(
            LIB_RS
                .matches("accessibility::dispatch_window_closed(")
                .count(),
            1,
            "exactly one dispatch site: a second one would double-release a \
             window's routing table",
        );
    }

    /// A window-scoped widget action must revive **at most its own window**.
    ///
    /// `revive_user_interfaces` rebuilds every parked interface in the
    /// process; using it here would take an action addressed to one window
    /// and wake up all the others, defeating the scoping this action exists
    /// for. The scoped arm therefore calls the single-window
    /// `revive_user_interface`, guarded by the named window's own lookup.
    ///
    /// The broadcast `Action::Widget` arm is the positive control: it *must*
    /// keep the global revival, because its operation really does walk every
    /// window.
    #[test]
    fn the_window_scoped_widget_action_revives_only_its_own_window() {
        let scoped_at = LIB_RS
            .find("Action::WidgetIn { window, operation } => {")
            .expect("the window-scoped widget action arm must exist in winit/src/lib.rs");
        let broadcast_at = LIB_RS
            .find("Action::Widget(operation) => {")
            .expect("the broadcast widget action arm must exist in winit/src/lib.rs");

        // Bounded by the next match arm, so the slice ends on a real
        // boundary however the arm grows.
        let scoped_len = LIB_RS[scoped_at..]
            .find("Action::Image(action) =>")
            .expect("the arm after the window-scoped one must still exist");
        let scoped_arm = &LIB_RS[scoped_at..scoped_at + scoped_len];
        let broadcast_arm = &LIB_RS[broadcast_at..scoped_at];

        assert!(
            !scoped_arm.contains("revive_user_interfaces("),
            "the window-scoped arm must not revive every parked interface: \
             an action for one window would rebuild all the others",
        );
        assert!(
            scoped_arm
                .contains("revive_user_interface(interfaces, ui_caches, program, target, window)"),
            "the window-scoped arm must revive its own named window through \
             the single-window `revive_user_interface`",
        );
        assert!(
            broadcast_arm.contains("revive_user_interfaces("),
            "positive control: the broadcast arm still revives every window, \
             because its operation walks all of them",
        );
    }
}

fn take_geometry_request(lifecycle: &Arc<Mutex<Lifecycle>>) -> bool {
    let Ok(mut state) = lifecycle.lock() else {
        return false;
    };
    let pending = std::mem::take(&mut state.geometry_pending);
    state.active && pending
}

#[cfg(test)]
mod moving_geometry_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    #[test]
    fn moving_geometry_wake_coalesces_only_active_publications() {
        let state = Arc::new(Mutex::new(Lifecycle::default()));
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let waker = std::task::Waker::from(Arc::new(GeometryWake {
            lifecycle: Arc::downgrade(&state),
            wake_loop: Arc::new(move || {
                let _ = counter.fetch_add(1, SeqCst);
            }),
        }));
        waker.wake_by_ref();
        assert_eq!(count.load(SeqCst), 0);
        assert!(!take_geometry_request(&state));
        state.lock().unwrap().active = true;
        for _ in 0..20 {
            waker.wake_by_ref();
        }
        assert_eq!(count.load(SeqCst), 1);
        assert!(take_geometry_request(&state));
        assert!(!take_geometry_request(&state));
        waker.wake_by_ref();
        assert_eq!(count.load(SeqCst), 2);
        state.lock().unwrap().active = false;
        assert!(!take_geometry_request(&state));
    }

    #[test]
    fn moving_geometry_wake_skips_busy_and_dropped_lifecycle() {
        let state = Arc::new(Mutex::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let waker = std::task::Waker::from(Arc::new(GeometryWake {
            lifecycle: Arc::downgrade(&state),
            wake_loop: Arc::new(move || {
                let _ = counter.fetch_add(1, SeqCst);
            }),
        }));
        let held = state.lock().unwrap();
        waker.wake_by_ref();
        assert_eq!(count.load(SeqCst), 0);
        drop(held);
        waker.wake_by_ref();
        assert_eq!(count.load(SeqCst), 1);
        drop(state);
        waker.wake_by_ref();
        assert_eq!(count.load(SeqCst), 1);
    }
}
