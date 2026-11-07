// Modified by enzUI contributors. See FORK.md.
//! Decide which windows a batch of program updates dirtied.
//!
//! The runtime used to treat every message as globally visible: after any
//! update it rebuilt (`view` + `UserInterface::build`), relayouted and
//! requested a redraw of **every** open window. Typing in a preferences window
//! therefore rebuilt an untouched main window.
//!
//! A program narrows that with [`window::invalidate`], which produces an
//! [`Action::Invalidate`]. The runtime also records [`Invalidation::All`] for
//! each update that supplied no immediately ready declaration. This module
//! turns those per-update records and the other actions of one runtime turn
//! into a [`Plan`]: the set of windows that must be rebuilt and redrawn.
//!
//! Nothing is inferred from the event source — a click in window A may
//! intentionally change window B — so the *only* narrowing input is the
//! program's own declaration. Any other action in the same turn (a window
//! action, a `Reload`, a widget operation, a font or backend change) restores
//! global invalidation, and so does a window whose size or scale factor moved
//! underneath us.
//!
//! [`window::invalidate`]: crate::runtime::window::invalidate
use crate::core::window::Id;
use crate::runtime::Action;
use crate::runtime::window::Invalidation;

/// The windows that must be rebuilt and redrawn after a batch of updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    invalidation: Invalidation,
}

impl Plan {
    /// A [`Plan`] that dirties every window. The compatibility default.
    pub fn global() -> Self {
        Self {
            invalidation: Invalidation::All,
        }
    }

    /// A [`Plan`] that dirties no window yet.
    pub fn empty() -> Self {
        Self {
            invalidation: Invalidation::none(),
        }
    }

    /// Builds the [`Plan`] for the actions produced by one runtime turn.
    ///
    /// An empty batch, or a batch without a single [`Action::Invalidate`],
    /// yields [`Plan::global`]. The runtime supplies one declaration per
    /// processed update, so the declarations here can be merged without one
    /// message narrowing another message's compatibility default.
    pub fn from_actions<'a, T: 'a>(actions: impl IntoIterator<Item = &'a Action<T>>) -> Self {
        let mut declared = false;
        let mut plan = Self::empty();

        for action in actions {
            match action {
                Action::Invalidate(invalidation) => {
                    declared = true;
                    plan.declare(invalidation.clone());
                }
                // An injected event is delivered to exactly one window, and
                // that window will be interacted with in this same turn.
                Action::Event { window, .. } => plan.dirty(*window),
                // An allocation only uploads pixels: no `view` can read it
                // until its completion arrives as a message of its own, which
                // invalidates for itself. A video frame or a decoded picture
                // handed over this way then redraws nothing early.
                Action::Image(crate::runtime::image::Action::Allocate(..)) => {}
                // Everything else may move state that any `view` reads:
                // window actions, renderer/backend changes, fonts, images,
                // widget operations, reloads. Those keep global invalidation
                // even when a narrower scope was declared beside them.
                _ => return Self::global(),
            }
        }

        if declared { plan } else { Self::global() }
    }

    /// Merges another declaration into this [`Plan`].
    pub fn declare(&mut self, invalidation: Invalidation) {
        let current = std::mem::replace(&mut self.invalidation, Invalidation::All);
        self.invalidation = current.merge(invalidation);
    }

    /// Marks a single window dirty.
    pub fn dirty(&mut self, id: Id) {
        self.declare(Invalidation::only(id));
    }

    /// Returns `true` if the window with the given [`Id`] must be rebuilt and
    /// redrawn.
    pub fn is_dirty(&self, id: Id) -> bool {
        self.invalidation.contains(id)
    }

    /// Returns this [`Plan`], widened to every window while the verification
    /// mode of [`verify_all_windows`] is on.
    pub fn verified(self) -> Self {
        if is_verifying() { Self::global() } else { self }
    }

    /// Returns `true` if every window is dirty.
    pub fn is_global(&self) -> bool {
        self.invalidation.is_all()
    }
}

/// Widens every [`Plan`] back to all-window invalidation.
///
/// A local declaration is a promise the runtime cannot check: if an update
/// declares one window but changes another, the undeclared window keeps stale
/// pixels. This mode makes the runtime ignore every narrowing, so a program
/// renders exactly as it did before scoped invalidation existed — run a
/// workflow with it on and off and compare: any difference is a wrong
/// declaration, which is caller misuse.
///
/// Enabled by [`verify_all_windows`] or by setting the environment variable
/// `ICED_INVALIDATION_VERIFY` to `1` before the runtime starts.
pub fn verify_all_windows(enabled: bool) {
    VERIFY.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Returns `true` while every [`Plan`] is widened to all windows.
pub fn is_verifying() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    let from_env = *ENV
        .get_or_init(|| std::env::var("ICED_INVALIDATION_VERIFY").is_ok_and(|value| value == "1"));

    from_env || VERIFY.load(std::sync::atomic::Ordering::Relaxed)
}

static VERIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Default for Plan {
    fn default() -> Self {
        Self::global()
    }
}

/// A user interface that is either live or parked as a cache.
///
/// A window that a [`Plan`] left clean keeps its cache instead of paying for a
/// `view` and a `UserInterface::build`; the runtime revives it lazily if an
/// event or a redraw ever reaches that window again.
#[derive(Debug)]
pub enum Slot<U> {
    /// The interface was rebuilt.
    Built(U),
    /// The interface stayed parked as this cache.
    Parked(crate::runtime::user_interface::Cache),
}

/// Rebuilds only the interfaces a [`Plan`] dirtied.
///
/// `is_live` filters out windows that were closed or never opened: their
/// caches are dropped, and every surviving window is still processed. `build`
/// is called for dirty, live windows only, and may itself decline (returning
/// `None`) when the window disappeared underneath it.
pub fn rebuild<U>(
    plan: &Plan,
    caches: impl IntoIterator<Item = (Id, crate::runtime::user_interface::Cache)>,
    is_live: impl Fn(Id) -> bool,
    mut build: impl FnMut(Id, crate::runtime::user_interface::Cache) -> Option<U>,
) -> Vec<(Id, Slot<U>)> {
    caches
        .into_iter()
        .filter_map(|(id, cache)| {
            if !is_live(id) {
                return None;
            }

            if !plan.is_dirty(id) {
                return Some((id, Slot::Parked(cache)));
            }

            Some((id, Slot::Built(build(id, cache)?)))
        })
        .collect()
}

/// Per-window counters for the work scoped invalidation is meant to remove.
///
/// Off by default; a debug build or a test enables it with
/// [`instrumentation::enable`].
pub mod instrumentation {
    use crate::core::window::Id;

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    /// How much work a window did since the last [`reset`].
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Counts {
        /// Calls to `Program::view` for this window.
        pub view: u64,
        /// Calls to `UserInterface::build` for this window.
        pub build: u64,
        /// Layout passes (a build or a relayout) for this window.
        pub layout: u64,
        /// Interface draws for this window.
        pub draw: u64,
        /// Redraws requested from the window by the runtime.
        pub redraw_requests: u64,
    }

    impl Counts {
        /// Returns `true` when the window did no work at all.
        pub fn is_idle(&self) -> bool {
            *self == Counts::default()
        }
    }

    static ENABLED: AtomicBool = AtomicBool::new(false);

    fn counters() -> &'static Mutex<HashMap<Id, Counts>> {
        static COUNTERS: OnceLock<Mutex<HashMap<Id, Counts>>> = OnceLock::new();
        COUNTERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Starts counting per-window work.
    pub fn enable() {
        ENABLED.store(true, Ordering::Relaxed);
    }

    /// Stops counting per-window work.
    pub fn disable() {
        ENABLED.store(false, Ordering::Relaxed);
    }

    /// Returns `true` while counting.
    pub fn is_enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// Clears every counter.
    pub fn reset() {
        if let Ok(mut counters) = counters().lock() {
            counters.clear();
        }
    }

    /// Returns the counters of a window.
    pub fn counts(id: Id) -> Counts {
        counters()
            .lock()
            .ok()
            .and_then(|counters| counters.get(&id).copied())
            .unwrap_or_default()
    }

    /// Returns the counters of every window that did work.
    pub fn snapshot() -> Vec<(Id, Counts)> {
        counters()
            .lock()
            .ok()
            .map(|counters| counters.iter().map(|(id, counts)| (*id, *counts)).collect())
            .unwrap_or_default()
    }

    fn record(id: Id, field: impl Fn(&mut Counts)) {
        if !is_enabled() {
            return;
        }

        if let Ok(mut counters) = counters().lock() {
            field(counters.entry(id).or_default());
        }
    }

    pub(crate) fn view(id: Id) {
        record(id, |counts| counts.view += 1);
    }

    pub(crate) fn build(id: Id) {
        record(id, |counts| counts.build += 1);
    }

    pub(crate) fn layout(id: Id) {
        record(id, |counts| counts.layout += 1);
    }

    pub(crate) fn draw(id: Id) {
        record(id, |counts| counts.draw += 1);
    }

    pub(crate) fn redraw_request(id: Id) {
        record(id, |counts| counts.redraw_requests += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::window::Id;
    use crate::runtime::fairness::{self, Budget, Host, Turn};
    use crate::runtime::futures::BoxStream;
    use crate::runtime::futures::futures::StreamExt;
    use crate::runtime::task::{self, Task};
    use crate::runtime::user_interface::Cache;
    use crate::runtime::window;

    use std::collections::HashMap;
    use std::task::Poll;

    type TestAction = Action<()>;

    fn ids(count: usize) -> Vec<Id> {
        (0..count).map(|_| Id::unique()).collect()
    }

    /// Rebuilds `windows` under `plan`, returning how many times each window
    /// was actually built.
    fn build_counts(plan: &Plan, windows: &[Id]) -> HashMap<Id, usize> {
        let mut counts: HashMap<Id, usize> = windows.iter().map(|id| (*id, 0)).collect();

        let slots = rebuild(
            plan,
            windows.iter().map(|id| (*id, Cache::default())),
            |id| windows.contains(&id),
            |id, _cache| {
                *counts.entry(id).or_default() += 1;
                Some(id)
            },
        );

        assert_eq!(slots.len(), windows.len(), "a window was lost");

        for (id, slot) in slots {
            match slot {
                Slot::Built(built) => assert_eq!(built, id),
                Slot::Parked(_) => assert_eq!(counts[&id], 0, "a parked window was built"),
            }
        }

        counts
    }

    #[derive(Debug, Clone)]
    enum Message {
        Undeclared(Id),
        Only(Id),
        None,
        All(Id),
        CascadeUndeclared { first: Id, second: Id },
        ReadyThenOnly { first: Id },
        PendingThenOnly { first: Id, second: Id },
        YieldThenOnly { first: Id, second: Id },
    }

    struct Reducer {
        changed: Vec<Id>,
        deferred: Vec<BoxStream<Action<Message>>>,
    }

    impl Reducer {
        fn new() -> Self {
            Self {
                changed: Vec::new(),
                deferred: Vec::new(),
            }
        }
    }

    impl Host<Message> for Reducer {
        fn update(&mut self, message: Message) -> Option<BoxStream<Action<Message>>> {
            let task = match message {
                Message::Undeclared(id) => {
                    self.changed.push(id);
                    Task::none()
                }
                Message::Only(id) => {
                    self.changed.push(id);
                    window::invalidate_only(id)
                }
                Message::None => window::invalidate(Invalidation::none()),
                Message::All(id) => {
                    self.changed.push(id);
                    window::invalidate_all()
                }
                Message::CascadeUndeclared { first, second } => {
                    self.changed.push(first);
                    Task::done(Message::Only(second))
                }
                Message::ReadyThenOnly { first } => {
                    self.changed.push(first);
                    Task::done(Message::None).chain(window::invalidate_only(first))
                }
                Message::PendingThenOnly { first, second } => {
                    self.changed.push(first);
                    let mut pending = true;

                    Task::stream(crate::runtime::futures::futures::stream::poll_fn(
                        move |_context| {
                            if pending {
                                pending = false;
                                Poll::Pending
                            } else {
                                Poll::Ready(None)
                            }
                        },
                    ))
                    .chain(window::invalidate_only(second))
                }
                Message::YieldThenOnly { first, second } => {
                    self.changed.push(first);
                    Task::done(Message::Only(second)).chain(window::invalidate_only(second))
                }
            };

            task::into_stream(task)
        }

        fn poll(
            &mut self,
            stream: &mut BoxStream<Action<Message>>,
        ) -> Poll<Option<Action<Message>>> {
            let waker = crate::runtime::futures::futures::task::noop_waker_ref();
            let mut context = crate::runtime::futures::futures::task::Context::from_waker(waker);

            stream.poll_next_unpin(&mut context)
        }

        fn defer(&mut self, stream: BoxStream<Action<Message>>) {
            self.deferred.push(stream);
        }

        fn elapsed(&mut self) -> crate::core::time::Duration {
            crate::core::time::Duration::ZERO
        }
    }

    fn drain_reducer(
        messages: impl IntoIterator<Item = Message>,
        budget: Budget,
    ) -> (Plan, Reducer, Turn) {
        let mut reducer = Reducer::new();
        let mut messages = messages.into_iter().collect();
        let mut actions = Vec::new();
        let mut turn = Turn::default();

        let _ = fairness::drain(
            &budget,
            &mut turn,
            &mut messages,
            &mut actions,
            &mut reducer,
        );

        (Plan::from_actions(&actions), reducer, turn)
    }

    /// An update that hands a frame to the renderer and declares that nothing
    /// on screen changed rebuilds no window.
    ///
    /// Sabotage: let an image allocation restore global invalidation -- the
    /// plan dirties every window.
    #[test]
    fn an_image_allocation_keeps_a_narrow_declaration() {
        let window = Id::unique();
        let (sender, _receiver) = crate::runtime::futures::futures::channel::oneshot::channel();
        let actions: Vec<Action<()>> = vec![
            Action::Image(crate::runtime::image::Action::Allocate(
                crate::core::image::Handle::from_rgba(1, 1, vec![0; 4]),
                sender,
            )),
            Action::Invalidate(crate::runtime::window::Invalidation::none()),
        ];
        let plan = Plan::from_actions(&actions);

        assert!(!plan.is_global());
        assert!(!plan.is_dirty(window));
    }

    #[test]
    fn mixed_reducer_updates_rebuild_both_windows_in_both_orders() {
        let windows = ids(2);

        for messages in [
            vec![Message::Undeclared(windows[0]), Message::Only(windows[1])],
            vec![Message::Only(windows[1]), Message::Undeclared(windows[0])],
        ] {
            let (plan, reducer, turn) = drain_reducer(messages, Budget::UNLIMITED);
            let counts = build_counts(&plan, &windows);

            assert_eq!(turn.messages, 2);
            assert_eq!(reducer.changed.len(), 2);
            assert_eq!(counts[&windows[0]], 1);
            assert_eq!(counts[&windows[1]], 1);
        }
    }

    #[test]
    fn reducer_declarations_union_none_and_all_without_cross_message_narrowing() {
        let windows = ids(3);

        let (local, _, _) = drain_reducer(
            [Message::Only(windows[0]), Message::Only(windows[1])],
            Budget::UNLIMITED,
        );
        let counts = build_counts(&local, &windows);
        assert_eq!(counts[&windows[0]], 1);
        assert_eq!(counts[&windows[1]], 1);
        assert_eq!(counts[&windows[2]], 0);

        let (none_and_local, _, _) = drain_reducer(
            [Message::None, Message::Only(windows[1])],
            Budget::UNLIMITED,
        );
        let counts = build_counts(&none_and_local, &windows);
        assert_eq!(counts[&windows[0]], 0);
        assert_eq!(counts[&windows[1]], 1);
        assert_eq!(counts[&windows[2]], 0);

        let (all, _, _) = drain_reducer(
            [Message::All(windows[0]), Message::Only(windows[1])],
            Budget::UNLIMITED,
        );
        assert!(all.is_global());
        assert_eq!(build_counts(&all, &windows).values().sum::<usize>(), 3);
    }

    #[test]
    fn ready_cascade_retains_the_parent_updates_global_default() {
        let windows = ids(2);
        let (plan, reducer, turn) = drain_reducer(
            [Message::CascadeUndeclared {
                first: windows[0],
                second: windows[1],
            }],
            Budget::UNLIMITED,
        );

        assert_eq!(turn.messages, 2);
        assert_eq!(reducer.changed, windows);
        assert!(plan.is_global());
        assert_eq!(build_counts(&plan, &windows).values().sum::<usize>(), 2);
    }

    #[test]
    fn declaration_after_a_ready_output_still_scopes_its_update() {
        let windows = ids(2);
        let (plan, reducer, turn) = drain_reducer(
            [Message::ReadyThenOnly { first: windows[0] }],
            Budget::UNLIMITED,
        );
        let counts = build_counts(&plan, &windows);

        assert_eq!(turn.messages, 2);
        assert_eq!(reducer.changed, vec![windows[0]]);
        assert_eq!(counts[&windows[0]], 1);
        assert_eq!(counts[&windows[1]], 0);
    }

    #[test]
    fn pending_declaration_cannot_narrow_a_sibling_updates_default() {
        let windows = ids(2);
        let (plan, reducer, turn) = drain_reducer(
            [
                Message::PendingThenOnly {
                    first: windows[0],
                    second: windows[1],
                },
                Message::Only(windows[1]),
            ],
            Budget::UNLIMITED,
        );
        let counts = build_counts(&plan, &windows);

        assert_eq!(reducer.changed, windows);
        assert_eq!(reducer.deferred.len(), 1);
        assert!(!turn.yielded);
        assert_eq!(counts[&windows[0]], 1);
        assert_eq!(counts[&windows[1]], 1);
    }

    #[test]
    fn fairness_deferred_declaration_cannot_narrow_an_updated_sibling() {
        let windows = ids(2);
        let budget = Budget {
            max_messages: None,
            max_stream_items: Some(2),
            max_duration: None,
        };
        let (plan, reducer, turn) = drain_reducer(
            [
                Message::Only(windows[1]),
                Message::YieldThenOnly {
                    first: windows[0],
                    second: windows[1],
                },
            ],
            budget,
        );
        let counts = build_counts(&plan, &windows);

        assert_eq!(reducer.changed, vec![windows[1], windows[0]]);
        assert_eq!(reducer.deferred.len(), 1);
        assert!(turn.yielded);
        assert_eq!(counts[&windows[0]], 1);
        assert_eq!(counts[&windows[1]], 1);
    }

    #[test]
    fn dead_local_scope_and_explicit_none_leave_live_windows_parked() {
        let windows = ids(2);
        let dead = Id::unique();
        let (plan, reducer, turn) = drain_reducer(
            [Message::Only(dead), Message::None, Message::None],
            Budget::UNLIMITED,
        );
        let counts = build_counts(&plan, &windows);

        assert_eq!(turn.messages, 3);
        assert_eq!(reducer.changed, vec![dead]);
        assert_eq!(counts.values().sum::<usize>(), 0);
    }

    #[test]
    fn an_update_that_declares_nothing_invalidates_every_window() {
        let windows = ids(3);

        let plan = Plan::from_actions(&[] as &[TestAction]);
        assert!(plan.is_global());

        let counts = build_counts(&plan, &windows);
        assert!(
            windows.iter().all(|id| counts[id] == 1),
            "the compatibility default must rebuild every window: {counts:?}"
        );
    }

    #[test]
    fn a_local_declaration_builds_only_that_window() {
        let windows = ids(8);
        let dirty = windows[3];

        let plan = Plan::from_actions(&[TestAction::Invalidate(window::Invalidation::only(dirty))]);

        assert!(!plan.is_global());
        assert!(plan.is_dirty(dirty));

        let counts = build_counts(&plan, &windows);

        assert_eq!(counts[&dirty], 1, "the dirty window must be rebuilt");

        for id in windows.iter().filter(|id| **id != dirty) {
            assert_eq!(
                counts[id], 0,
                "a sibling was rebuilt by a window-local update"
            );
        }
    }

    #[test]
    fn a_two_window_declaration_builds_exactly_two_windows() {
        let windows = ids(8);
        let scope = window::Invalidation::only(windows[1]).and(windows[6]);

        let plan = Plan::from_actions(&[TestAction::Invalidate(scope)]);
        let counts = build_counts(&plan, &windows);

        assert_eq!(counts[&windows[1]], 1);
        assert_eq!(counts[&windows[6]], 1);
        assert_eq!(
            counts.values().sum::<usize>(),
            2,
            "exactly the declared windows are rebuilt: {counts:?}"
        );
    }

    #[test]
    fn declaring_all_absorbs_a_local_declaration() {
        let windows = ids(4);

        let plan = Plan::from_actions(&[
            TestAction::Invalidate(window::Invalidation::only(windows[0])),
            TestAction::Invalidate(window::Invalidation::All),
        ]);

        assert!(plan.is_global());
        assert_eq!(build_counts(&plan, &windows).values().sum::<usize>(), 4);
    }

    #[test]
    fn a_shared_state_action_restores_global_invalidation() {
        let windows = ids(4);

        for action in [
            TestAction::Window(window::Action::Close(windows[0])),
            TestAction::Reload,
            TestAction::Tick,
        ] {
            let plan = Plan::from_actions(&[
                TestAction::Invalidate(window::Invalidation::only(windows[0])),
                action,
            ]);

            assert!(
                plan.is_global(),
                "an action that can move shared state must not be narrowed"
            );
        }
    }

    #[test]
    fn an_injected_event_dirties_its_own_window_only() {
        let windows = ids(4);

        let plan = Plan::from_actions(&[
            TestAction::Invalidate(window::Invalidation::only(windows[0])),
            TestAction::Event {
                window: windows[2],
                event: crate::core::Event::Window(crate::core::window::Event::Unfocused),
            },
        ]);

        assert!(!plan.is_global());

        let counts = build_counts(&plan, &windows);
        assert_eq!(counts[&windows[0]], 1);
        assert_eq!(counts[&windows[2]], 1);
        assert_eq!(counts.values().sum::<usize>(), 2, "{counts:?}");
    }

    #[test]
    fn a_stale_window_id_does_not_drop_the_surviving_ones() {
        let windows = ids(3);
        let closed = Id::unique();

        let plan = Plan::from_actions(&[TestAction::Invalidate(
            window::Invalidation::only(closed).and(windows[1]),
        )]);

        let mut built = Vec::new();

        let slots = rebuild(
            &plan,
            windows
                .iter()
                .map(|id| (*id, Cache::default()))
                .chain(std::iter::once((closed, Cache::default()))),
            |id| windows.contains(&id),
            |id, _cache| {
                built.push(id);
                Some(id)
            },
        );

        assert_eq!(built, vec![windows[1]], "only the live dirty window builds");
        assert_eq!(slots.len(), 3, "the closed window keeps no slot");
        assert!(slots.iter().all(|(id, _slot)| *id != closed));
    }

    #[test]
    fn a_window_that_vanishes_mid_build_keeps_no_slot() {
        let windows = ids(2);

        let slots = rebuild(
            &Plan::global(),
            windows.iter().map(|id| (*id, Cache::default())),
            |_id| true,
            |id, _cache| (id == windows[0]).then_some(id),
        );

        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].0, windows[0]);
    }

    #[test]
    fn the_verification_mode_widens_a_local_declaration() {
        let windows = ids(4);

        let local = || {
            Plan::from_actions(&[TestAction::Invalidate(window::Invalidation::only(
                windows[0],
            ))])
        };

        assert!(
            !local().verified().is_global(),
            "verification must be off by default"
        );

        verify_all_windows(true);
        let verified = local().verified();
        verify_all_windows(false);

        assert!(
            verified.is_global(),
            "verification mode must render every window, so a wrong declaration cannot hide"
        );
        assert!(
            !local().verified().is_global(),
            "the mode must be revertible"
        );
    }

    #[test]
    fn counters_are_off_until_enabled() {
        let id = Id::unique();

        instrumentation::disable();
        instrumentation::view(id);
        assert!(instrumentation::counts(id).is_idle());

        instrumentation::enable();
        instrumentation::view(id);
        instrumentation::draw(id);
        instrumentation::disable();

        let counts = instrumentation::counts(id);
        assert_eq!(counts.view, 1);
        assert_eq!(counts.draw, 1);
        assert_eq!(counts.build, 0);
    }
}
