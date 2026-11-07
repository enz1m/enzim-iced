// Modified by enzUI contributors. See FORK.md.
//! Keep one burst of messages from monopolizing the UI thread.
//!
//! A shell processes a message by calling `update` and then draining every
//! [`Action`] its [`Task`] can produce *without waiting*. That drain is
//! unbounded: `Task::batch` of a hundred thousand `Task::done`, a synchronous
//! stream, or a message that keeps returning another immediately ready message
//! all keep the UI thread inside `update` for as long as they like, delaying
//! native input, redraws, accessibility and presentation.
//!
//! [`drain`] is that same loop with a **cooperative budget**. When the budget
//! is spent it stops, hands the messages it has not processed back to the
//! caller, and hands any partially consumed stream to the runtime — which
//! delivers the rest asynchronously, through the same backpressured channel
//! every other task uses. Nothing is dropped, nothing inside a single task
//! chain is reordered, and the shell gets a chance to run window, input,
//! accessibility and redraw work before the next chunk.
//!
//! # Order
//!
//! - messages are processed first-in, first-out, and the outputs a generation
//!   of messages produces are processed after every message of that
//!   generation—exactly the order an unbounded drain produced;
//! - a [`Turn`] that yields leaves the unprocessed messages at the front of the
//!   caller's queue, followed by the outputs collected before the yield, so
//!   resuming is indistinguishable from never having stopped;
//! - a chain that is deferred mid-stream keeps its own order, but its remaining
//!   items arrive after the messages already queued. `batch(a).chain(b)` still
//!   runs `b` after every item of `a`;
//! - non-output actions are collected for the shell and run after the drain
//!   returns, as before. Every processed update contributes an invalidation
//!   declaration: if none is immediately available before its task completes,
//!   waits, or is deferred, [`Invalidation::All`] preserves the compatibility
//!   default for that update. A shell that postpones one of the actions—window
//!   actions during a redraw, say—must postpone the rest of that batch too, or
//!   it reorders the chain; [`defer_in_order`] is that rule;
//! - `Action::Exit` ends the event loop when the shell runs it. Work deferred
//!   before it is dropped with the loop, which is what makes shutdown prompt;
//!   it is never resurrected afterwards.
//!
//! [`Task`]: crate::Task
use crate::Action;
use crate::core::time::Duration;
use crate::futures::BoxStream;
use crate::window::Invalidation;

use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::Poll;

/// The cooperative budget of a single UI-thread turn.
///
/// A turn ends when any of the limits is reached. The limits are deliberately
/// redundant: the count limits make the bound deterministic (and testable
/// without a clock), while [`max_duration`] is what actually keeps a slow
/// `update` inside a frame.
///
/// [`max_duration`]: Self::max_duration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The maximum amount of messages that may be processed in a single turn.
    ///
    /// `None` means "as many as there are", which is the unbounded behavior
    /// this module exists to avoid.
    pub max_messages: Option<usize>,

    /// The maximum amount of immediately ready stream items—[`Action::Output`]
    /// and every other [`Action`]—that may be consumed in a single turn.
    pub max_stream_items: Option<usize>,

    /// The maximum amount of time a single turn may spend on the UI thread
    /// before yielding.
    pub max_duration: Option<Duration>,
}

impl Budget {
    /// The default maximum amount of messages per turn.
    pub const DEFAULT_MAX_MESSAGES: usize = 4_096;

    /// The default maximum amount of immediately ready stream items per turn.
    pub const DEFAULT_MAX_STREAM_ITEMS: usize = 4_096;

    /// The default maximum duration of a turn.
    ///
    /// A quarter of a 60Hz frame, so a burst still leaves three quarters of the
    /// frame for input, layout, drawing and presentation.
    pub const DEFAULT_MAX_DURATION: Duration = Duration::from_micros(4_000);

    /// A [`Budget`] that never yields.
    ///
    /// This is the historical, unbounded behavior. It is kept so it can be
    /// selected on purpose—and so a test can use it as a positive control.
    pub const UNLIMITED: Self = Self {
        max_messages: None,
        max_stream_items: None,
        max_duration: None,
    };

    /// Reads the [`Budget`] from the environment, falling back to
    /// [`Budget::default`].
    ///
    /// - `ICED_MESSAGE_BUDGET`: messages *and* stream items per turn.
    /// - `ICED_MESSAGE_BUDGET_MICROS`: turn duration, in microseconds.
    ///
    /// `0`, `none` and `unlimited` disable the corresponding limit; anything
    /// unparseable is ignored with a warning, so a typo cannot silently
    /// unbound the loop.
    pub fn from_env() -> Self {
        Self::parse(
            env::var("ICED_MESSAGE_BUDGET").ok().as_deref(),
            env::var("ICED_MESSAGE_BUDGET_MICROS").ok().as_deref(),
        )
    }

    fn parse(messages: Option<&str>, micros: Option<&str>) -> Self {
        let default = Self::default();

        Self {
            max_messages: parse_limit("ICED_MESSAGE_BUDGET", messages, default.max_messages),
            max_stream_items: parse_limit(
                "ICED_MESSAGE_BUDGET",
                messages,
                default.max_stream_items,
            ),
            max_duration: match micros {
                Some(micros) => parse_limit(
                    "ICED_MESSAGE_BUDGET_MICROS",
                    Some(micros),
                    default
                        .max_duration
                        .map(|duration| duration.as_micros() as usize),
                )
                .map(|micros| Duration::from_micros(micros as u64)),
                None => default.max_duration,
            },
        }
    }

    fn is_spent(&self, turn: &Turn) -> bool {
        self.max_messages.is_some_and(|max| turn.messages >= max)
            || self
                .max_stream_items
                .is_some_and(|max| turn.stream_items >= max)
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_messages: Some(Self::DEFAULT_MAX_MESSAGES),
            max_stream_items: Some(Self::DEFAULT_MAX_STREAM_ITEMS),
            max_duration: Some(Self::DEFAULT_MAX_DURATION),
        }
    }
}

fn parse_limit(key: &str, value: Option<&str>, fallback: Option<usize>) -> Option<usize> {
    let Some(value) = value else {
        return fallback;
    };

    let value = value.trim();

    if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("unlimited") {
        return None;
    }

    match value.parse::<usize>() {
        Ok(0) => None,
        Ok(limit) => Some(limit),
        Err(_) => {
            log::warn!("Ignoring invalid {key}: {value:?}");
            fallback
        }
    }
}

/// What a single turn of [`drain`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Turn {
    /// The amount of messages processed during the turn.
    pub messages: usize,

    /// The amount of immediately ready stream items consumed during the turn.
    pub stream_items: usize,

    /// The amount of immediately ready [`Action::Output`] consumed during the
    /// turn.
    pub outputs: usize,

    /// The longest chain of message generations observed during the turn.
    ///
    /// A message whose task immediately produces another message, whose task
    /// immediately produces another one, is a cascade of depth 3.
    pub cascade: usize,

    /// The amount of messages left to process when the turn ended.
    pub queued: usize,

    /// The amount of partially consumed streams handed back to the runtime for
    /// asynchronous, backpressured delivery *because the budget ran out*.
    ///
    /// A stream that simply became [`Poll::Pending`] is not counted: handing
    /// that one over is not a fairness event, it is how tasks have always
    /// worked.
    pub deferred: usize,

    /// Whether the turn ran out of [`Budget`] and must be resumed.
    pub yielded: bool,
}

impl Turn {
    /// Returns `true` if the caller must schedule another turn.
    ///
    /// Every message left behind is still owned by the caller's queue, so
    /// "must" is literal: nothing else will process them.
    pub fn must_resume(&self) -> bool {
        self.yielded || self.queued > 0
    }
}

impl fmt::Display for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} message(s), {} stream item(s) ({} output(s)), cascade {}, \
             {} queued, {} deferred{}",
            self.messages,
            self.stream_items,
            self.outputs,
            self.cascade,
            self.queued,
            self.deferred,
            if self.yielded { ", yielded" } else { "" }
        )
    }
}

/// The UI-thread environment [`drain`] runs in.
///
/// Implemented by the shell; the trait exists so the fairness logic itself has
/// no clock, no executor and no windowing system, and can therefore be tested
/// exactly.
pub trait Host<T> {
    /// Processes a message, returning the stream of its [`Task`], if any.
    ///
    /// [`drain`] records [`Invalidation::All`] when the immediately ready part
    /// of this stream contains no [`Action::Invalidate`]. A declaration after
    /// the stream first waits or crosses a budget boundary cannot narrow the
    /// update that already ran.
    ///
    /// [`Task`]: crate::Task
    fn update(&mut self, message: T) -> Option<BoxStream<Action<T>>>;

    /// Polls a task stream once, without waiting.
    fn poll(&mut self, stream: &mut BoxStream<Action<T>>) -> Poll<Option<Action<T>>>;

    /// Hands a stream that is not finished back to the runtime, which will
    /// deliver the rest of it asynchronously.
    fn defer(&mut self, stream: BoxStream<Action<T>>);

    /// Returns the time elapsed since the turn started.
    ///
    /// Implementations may coarsen this; [`drain`] only compares it against
    /// [`Budget::max_duration`].
    fn elapsed(&mut self) -> Duration;
}

/// Processes `messages`—and the messages they immediately produce—until they
/// run out or `budget` is spent.
///
/// Messages that were not processed are left in `messages`, in order, followed
/// by any output that was produced before the yield: a resumed turn sees the
/// exact same order an unbounded drain would have. Actions that are not
/// outputs are pushed to `actions` for the caller to run. Each processed
/// message contributes an invalidation declaration, including messages whose
/// task is empty, pending, or deferred before it declares a scope. Repeated
/// global defaults are coalesced because the first one already absorbs the
/// batch.
///
/// `turn` accumulates what the turn has spent so far. A shell that needs to
/// drain more than once before it can return to the event loop passes the same
/// [`Turn`] every time, so the budget covers the whole turn instead of every
/// call; it resets it—and its clock—when the event loop gets its chance.
///
/// The returned [`Turn`] tells the caller whether it must schedule another one.
pub fn drain<T>(
    budget: &Budget,
    turn: &mut Turn,
    messages: &mut Vec<T>,
    actions: &mut Vec<Action<T>>,
    host: &mut impl Host<T>,
) -> Turn {
    // `VecDeque::from` and `Vec::from` hand the same allocation back and forth,
    // so a turn allocates nothing the previous one did not.
    let mut queue = VecDeque::from(std::mem::take(messages));
    let mut generation = 0;
    let mut global_default_recorded = actions
        .iter()
        .any(|action| matches!(action, Action::Invalidate(invalidation) if invalidation.is_all()));

    'turn: loop {
        if generation == 0 && queue.is_empty() {
            break 'turn;
        }

        if budget.is_spent(turn) || is_out_of_time(budget, host) {
            turn.yielded = true;
            break 'turn;
        }

        if generation == 0 {
            generation = queue.len();
            turn.cascade += 1;
        }

        let Some(message) = queue.pop_front() else {
            break 'turn;
        };

        generation -= 1;
        turn.messages += 1;

        let Some(mut stream) = host.update(message) else {
            if !global_default_recorded {
                actions.push(Action::Invalidate(Invalidation::All));
                global_default_recorded = true;
            }
            continue;
        };

        let mut declared_invalidation = false;

        // Run immediately available actions synchronously (e.g. widget
        // operations), while the budget lasts.
        loop {
            if budget
                .max_stream_items
                .is_some_and(|max| turn.stream_items >= max)
                || is_out_of_time(budget, host)
            {
                // The rest of this chain becomes an ordinary asynchronous
                // task: still in order, now backpressured.
                if !declared_invalidation && !global_default_recorded {
                    actions.push(Action::Invalidate(Invalidation::All));
                }
                host.defer(stream);
                turn.deferred += 1;
                turn.yielded = true;
                break 'turn;
            }

            match host.poll(&mut stream) {
                Poll::Ready(Some(Action::Output(output))) => {
                    turn.stream_items += 1;
                    turn.outputs += 1;
                    queue.push_back(output);
                }
                Poll::Ready(Some(action)) => {
                    turn.stream_items += 1;
                    declared_invalidation |= matches!(action, Action::Invalidate(_));
                    actions.push(action);
                }
                Poll::Ready(None) => {
                    if !declared_invalidation && !global_default_recorded {
                        actions.push(Action::Invalidate(Invalidation::All));
                        global_default_recorded = true;
                    }
                    break;
                }
                Poll::Pending => {
                    if !declared_invalidation && !global_default_recorded {
                        actions.push(Action::Invalidate(Invalidation::All));
                        global_default_recorded = true;
                    }
                    host.defer(stream);
                    break;
                }
            }
        }
    }

    *messages = Vec::from(queue);
    turn.queued = messages.len();

    *turn
}

fn is_out_of_time<T>(budget: &Budget, host: &mut impl Host<T>) -> bool {
    budget.max_duration.is_some_and(|max| host.elapsed() >= max)
}

/// Returns `true` if `action` must be deferred to preserve the order of a task
/// chain whose earlier actions were already deferred.
///
/// A shell may need to postpone some kinds of action—window actions during a
/// redraw, for instance—to a later turn. Running the *following* actions of the
/// same chain immediately would reorder them: `Task::batch(closes).chain(exit())`
/// would exit before the windows it was asked to close. Once one action of a
/// batch is postponed, every later action of that batch is too.
pub fn defer_in_order(deferring: &mut bool, action_needs_deferral: bool) -> bool {
    *deferring = *deferring || action_needs_deferral;
    *deferring
}

/// A cumulative summary of every [`drain`] turn on this thread's shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metrics {
    /// The amount of turns that ran.
    pub turns: u64,

    /// The amount of turns that ran out of [`Budget`].
    pub yields: u64,

    /// The total amount of messages processed.
    pub messages: u64,

    /// The total amount of immediately ready stream items consumed.
    pub stream_items: u64,

    /// The total amount of immediately ready [`Action::Output`] consumed.
    pub outputs: u64,

    /// The total amount of partially consumed streams handed back to the
    /// runtime.
    pub deferred: u64,

    /// The amount of messages left queued at the end of the last turn.
    pub queued: usize,

    /// The most messages ever processed in a single turn.
    pub max_messages_per_turn: usize,

    /// The longest message cascade ever observed in a single turn.
    pub longest_cascade: usize,
}

/// Returns the last [`Metrics`] published by the shell.
///
/// This is the observability hook: a shell calls [`Metrics::record`] once per
/// turn and [`publish`] whenever it wants the numbers to become visible.
pub fn metrics() -> Metrics {
    METRICS.snapshot()
}

/// Publishes `metrics` so [`metrics`] can return them.
pub fn publish(metrics: &Metrics) {
    METRICS.publish(metrics);
}

impl Metrics {
    /// Accumulates a finished [`Turn`].
    pub fn record(&mut self, turn: &Turn) {
        self.turns += 1;
        self.yields += u64::from(turn.yielded);
        self.messages += turn.messages as u64;
        self.stream_items += turn.stream_items as u64;
        self.outputs += turn.outputs as u64;
        self.deferred += turn.deferred as u64;
        self.queued = turn.queued;
        self.max_messages_per_turn = self.max_messages_per_turn.max(turn.messages);
        self.longest_cascade = self.longest_cascade.max(turn.cascade);
    }
}

struct Counters {
    turns: AtomicU64,
    yields: AtomicU64,
    messages: AtomicU64,
    stream_items: AtomicU64,
    outputs: AtomicU64,
    deferred: AtomicU64,
    queued: AtomicUsize,
    max_messages_per_turn: AtomicUsize,
    longest_cascade: AtomicUsize,
}

static METRICS: Counters = Counters {
    turns: AtomicU64::new(0),
    yields: AtomicU64::new(0),
    messages: AtomicU64::new(0),
    stream_items: AtomicU64::new(0),
    outputs: AtomicU64::new(0),
    deferred: AtomicU64::new(0),
    queued: AtomicUsize::new(0),
    max_messages_per_turn: AtomicUsize::new(0),
    longest_cascade: AtomicUsize::new(0),
};

impl Counters {
    fn publish(&self, metrics: &Metrics) {
        self.turns.store(metrics.turns, Ordering::Relaxed);
        self.yields.store(metrics.yields, Ordering::Relaxed);
        self.messages.store(metrics.messages, Ordering::Relaxed);
        self.stream_items
            .store(metrics.stream_items, Ordering::Relaxed);
        self.outputs.store(metrics.outputs, Ordering::Relaxed);
        self.deferred.store(metrics.deferred, Ordering::Relaxed);
        self.queued.store(metrics.queued, Ordering::Relaxed);
        self.max_messages_per_turn
            .store(metrics.max_messages_per_turn, Ordering::Relaxed);
        self.longest_cascade
            .store(metrics.longest_cascade, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Metrics {
        Metrics {
            turns: self.turns.load(Ordering::Relaxed),
            yields: self.yields.load(Ordering::Relaxed),
            messages: self.messages.load(Ordering::Relaxed),
            stream_items: self.stream_items.load(Ordering::Relaxed),
            outputs: self.outputs.load(Ordering::Relaxed),
            deferred: self.deferred.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            max_messages_per_turn: self.max_messages_per_turn.load(Ordering::Relaxed),
            longest_cascade: self.longest_cascade.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::task;

    use crate::futures::futures::stream::{self, StreamExt};

    /// What the fake application does when it processes a message.
    #[derive(Debug, Clone)]
    enum Plan {
        /// Produce nothing.
        Nothing,
        /// Produce these messages, all immediately ready.
        Ready(Vec<u32>),
        /// Produce this message, then a stream that never finishes.
        ReadyThenPending(u32),
        /// Produce these messages, then chain an `Action::Exit`.
        ReadyThenExit(Vec<u32>),
        Declare(Invalidation),
        ReadyThenDeclare(Vec<u32>, Invalidation),
        PendingOnceThenDeclare(Invalidation),
        DeclareThenPending(Invalidation),
    }

    /// A deterministic [`Host`]: no clock, no executor, no windows.
    struct Fake {
        /// Every message processed, in order.
        seen: Vec<u32>,
        /// What each message expands into.
        plan: Box<dyn Fn(u32) -> Plan>,
        /// Streams handed back for asynchronous delivery.
        deferred: Vec<BoxStream<Action<u32>>>,
        /// Scripted elapsed time: one entry consumed per query.
        clock: VecDeque<Duration>,
        /// The elapsed time returned once `clock` runs out.
        clock_tail: Duration,
        /// How many times the clock was read.
        clock_reads: usize,
    }

    impl Fake {
        fn new(plan: impl Fn(u32) -> Plan + 'static) -> Self {
            Self {
                seen: Vec::new(),
                plan: Box::new(plan),
                deferred: Vec::new(),
                clock: VecDeque::new(),
                clock_tail: Duration::ZERO,
                clock_reads: 0,
            }
        }

        fn with_clock(mut self, ticks: impl IntoIterator<Item = u64>, tail: u64) -> Self {
            self.clock = ticks.into_iter().map(Duration::from_micros).collect();
            self.clock_tail = Duration::from_micros(tail);
            self
        }

        /// Delivers every deferred stream, synchronously and in order, the way
        /// the runtime eventually would.
        fn flush_deferred(&mut self, messages: &mut Vec<u32>, actions: &mut Vec<Action<u32>>) {
            let deferred = std::mem::take(&mut self.deferred);

            for mut stream in deferred {
                loop {
                    match self.poll(&mut stream) {
                        Poll::Ready(Some(Action::Output(output))) => messages.push(output),
                        Poll::Ready(Some(action)) => actions.push(action),
                        Poll::Ready(None) => break,
                        Poll::Pending => break,
                    }
                }
            }
        }
    }

    impl Host<u32> for Fake {
        fn update(&mut self, message: u32) -> Option<BoxStream<Action<u32>>> {
            self.seen.push(message);

            let task: task::Task<u32> = match (self.plan)(message) {
                Plan::Nothing => task::Task::none(),
                Plan::Ready(messages) => {
                    task::Task::batch(messages.into_iter().map(task::Task::done))
                }
                Plan::ReadyThenPending(message) => {
                    task::Task::done(message).chain(task::Task::stream(stream::pending::<u32>()))
                }
                Plan::ReadyThenExit(messages) => {
                    task::Task::batch(messages.into_iter().map(task::Task::done))
                        .chain(task::effect(Action::<std::convert::Infallible>::Exit))
                }
                Plan::Declare(invalidation) => crate::window::invalidate(invalidation),
                Plan::ReadyThenDeclare(messages, invalidation) => {
                    task::Task::batch(messages.into_iter().map(task::Task::done))
                        .chain(crate::window::invalidate(invalidation))
                }
                Plan::PendingOnceThenDeclare(invalidation) => {
                    let mut polls = 0;

                    task::Task::stream(stream::poll_fn(move |_context| {
                        polls += 1;

                        match polls {
                            1 => Poll::Pending,
                            2 => Poll::Ready(Some(99)),
                            _ => Poll::Ready(None),
                        }
                    }))
                    .chain(crate::window::invalidate(invalidation))
                }
                Plan::DeclareThenPending(invalidation) => crate::window::invalidate(invalidation)
                    .chain(task::Task::stream(stream::pending::<u32>())),
            };

            task::into_stream(task)
        }

        fn poll(&mut self, stream: &mut BoxStream<Action<u32>>) -> Poll<Option<Action<u32>>> {
            use crate::futures::futures::task;

            let waker = task::noop_waker_ref();
            let mut context = task::Context::from_waker(waker);

            stream.poll_next_unpin(&mut context)
        }

        fn defer(&mut self, stream: BoxStream<Action<u32>>) {
            self.deferred.push(stream);
        }

        fn elapsed(&mut self) -> Duration {
            self.clock_reads += 1;
            self.clock.pop_front().unwrap_or(self.clock_tail)
        }
    }

    fn counted(budget: Option<usize>) -> Budget {
        Budget {
            max_messages: budget,
            max_stream_items: budget,
            max_duration: None,
        }
    }

    fn invalidations(actions: &[Action<u32>]) -> Vec<&Invalidation> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Invalidate(invalidation) => Some(invalidation),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_update_keeps_its_own_invalidation_default() {
        let local = crate::core::window::Id::unique();

        for messages in [vec![0, 1], vec![1, 0]] {
            let mut host = Fake::new(move |message| match message {
                0 => Plan::Nothing,
                1 => Plan::Declare(Invalidation::only(local)),
                _ => unreachable!(),
            });
            let mut messages = messages;
            let mut actions = Vec::new();

            let turn = drain(
                &Budget::UNLIMITED,
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );
            let declarations = invalidations(&actions);

            assert_eq!(turn.messages, 2);
            assert_eq!(declarations.len(), 2);
            assert!(declarations.iter().any(|scope| scope.is_all()));
            assert!(declarations.iter().any(|scope| scope.contains(local)));
        }
    }

    #[test]
    fn undeclared_noop_burst_records_one_absorbing_default() {
        let mut host = Fake::new(|_| Plan::Nothing);
        let mut messages: Vec<u32> = (0..1_000).collect();
        let mut actions = Vec::new();

        let turn = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(turn.messages, 1_000);
        assert_eq!(invalidations(&actions), vec![&Invalidation::All]);
    }

    #[test]
    fn explicit_none_and_local_remain_independent_declarations() {
        let local = crate::core::window::Id::unique();
        let mut host = Fake::new(move |message| match message {
            0 => Plan::Declare(Invalidation::none()),
            1 => Plan::Declare(Invalidation::only(local)),
            _ => unreachable!(),
        });
        let mut messages = vec![0, 1];
        let mut actions = Vec::new();

        let _ = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );
        let declarations = invalidations(&actions);

        assert_eq!(declarations.len(), 2);
        assert!(declarations.iter().all(|scope| !scope.is_all()));
        assert_eq!(
            declarations
                .iter()
                .filter(|scope| scope.contains(local))
                .count(),
            1
        );
    }

    #[test]
    fn undeclared_ready_cascade_cannot_be_narrowed_by_its_output() {
        let local = crate::core::window::Id::unique();
        let mut host = Fake::new(move |message| match message {
            0 => Plan::Ready(vec![1]),
            1 => Plan::Declare(Invalidation::only(local)),
            _ => unreachable!(),
        });
        let mut messages = vec![0];
        let mut actions = Vec::new();

        let turn = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );
        let declarations = invalidations(&actions);

        assert_eq!(turn.messages, 2);
        assert!(declarations.iter().any(|scope| scope.is_all()));
        assert!(declarations.iter().any(|scope| scope.contains(local)));
    }

    #[test]
    fn pending_before_a_declaration_keeps_the_global_default() {
        let local = crate::core::window::Id::unique();
        let mut host = Fake::new(move |_| Plan::PendingOnceThenDeclare(Invalidation::only(local)));
        let mut messages = vec![0];
        let mut actions = Vec::new();

        let turn = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(invalidations(&actions), vec![&Invalidation::All]);
        assert_eq!(host.deferred.len(), 1);
        assert!(!turn.yielded);
    }

    #[test]
    fn declaration_before_pending_preserves_its_scope() {
        let local = crate::core::window::Id::unique();
        let mut host = Fake::new(move |_| Plan::DeclareThenPending(Invalidation::only(local)));
        let mut messages = vec![0];
        let mut actions = Vec::new();

        let _ = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );
        let declarations = invalidations(&actions);

        assert_eq!(declarations.len(), 1);
        assert!(declarations[0].contains(local));
        assert!(!declarations[0].is_all());
    }

    #[test]
    fn fairness_deferral_before_a_declaration_keeps_the_global_default() {
        let local = crate::core::window::Id::unique();
        let mut host =
            Fake::new(move |_| Plan::ReadyThenDeclare(vec![7, 8], Invalidation::only(local)));
        let mut messages = vec![0];
        let mut actions = Vec::new();

        let turn = drain(
            &counted(Some(1)),
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert!(turn.yielded);
        assert_eq!(turn.deferred, 1);
        assert_eq!(invalidations(&actions), vec![&Invalidation::All]);

        host.flush_deferred(&mut messages, &mut actions);
        let declarations = invalidations(&actions);
        assert_eq!(declarations.len(), 2);
        assert!(declarations[0].is_all());
        assert!(declarations[1].contains(local));
    }

    /// The unbounded behavior this module replaces: one burst, one turn.
    #[test]
    fn unlimited_budget_drains_everything_in_one_turn() {
        let mut host = Fake::new(|_| Plan::Nothing);
        let mut messages: Vec<u32> = (0..10_000).collect();
        let mut actions = Vec::new();

        let turn = drain(
            &Budget::UNLIMITED,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(turn.messages, 10_000);
        assert_eq!(turn.queued, 0);
        assert!(!turn.yielded);
        assert!(!turn.must_resume());
    }

    #[test]
    fn message_burst_is_bounded_per_turn_and_completes_over_turns() {
        let mut host = Fake::new(|_| Plan::Nothing);
        let mut messages: Vec<u32> = (0..1_000).collect();
        let mut actions = Vec::new();

        let mut turns = 0;
        let mut max_per_turn = 0;

        loop {
            let turn = drain(
                &counted(Some(64)),
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );
            turns += 1;
            max_per_turn = max_per_turn.max(turn.messages);

            if !turn.must_resume() {
                break;
            }

            assert!(turns < 1_000, "too many turns: {turns}");
        }

        assert_eq!(max_per_turn, 64, "a turn exceeded the budget");
        assert_eq!(turns, 1_000 / 64 + 1);
        assert_eq!(host.seen, (0..1_000).collect::<Vec<_>>());
    }

    /// One message whose task is immediately ready 5_000 times.
    #[test]
    fn one_task_with_many_ready_outputs_is_bounded_and_ordered() {
        let mut host = Fake::new(|message| {
            if message == 0 {
                Plan::Ready((1..=5_000).collect())
            } else {
                Plan::Nothing
            }
        });

        let mut messages = vec![0];
        let mut actions = Vec::new();

        let mut turns = 0;
        let mut max_items = 0;

        loop {
            let turn = drain(
                &counted(Some(100)),
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );
            turns += 1;
            max_items = max_items.max(turn.stream_items.max(turn.messages));

            host.flush_deferred(&mut messages, &mut actions);

            if messages.is_empty() {
                break;
            }

            assert!(turns < 5_000, "too many turns");
        }

        assert!(turns > 1, "the burst was not spread over turns");
        assert_eq!(max_items, 100, "a turn exceeded the budget");
        assert_eq!(
            host.seen,
            (0..=5_000).collect::<Vec<_>>(),
            "messages were dropped, duplicated or reordered"
        );
    }

    #[test]
    fn a_cascade_of_immediate_messages_is_bounded_and_measured() {
        // Every message immediately produces the next one, 500 deep.
        let mut host = Fake::new(|message| {
            if message < 500 {
                Plan::Ready(vec![message + 1])
            } else {
                Plan::Nothing
            }
        });

        let mut messages = vec![0];
        let mut actions = Vec::new();

        let mut turns = 0;
        let mut longest_cascade = 0;

        loop {
            let turn = drain(
                &counted(Some(32)),
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );
            turns += 1;
            longest_cascade = longest_cascade.max(turn.cascade);

            assert!(turn.messages <= 32);

            if !turn.must_resume() {
                break;
            }

            assert!(turns < 500, "too many turns");
        }

        assert_eq!(host.seen, (0..=500).collect::<Vec<_>>());
        assert_eq!(longest_cascade, 32, "the cascade depth was not reported");
        assert!(turns > 1);
    }

    #[test]
    fn deferred_stream_keeps_chain_order_and_defers_the_chained_action() {
        // A batch of outputs chained to an action: the action must not run
        // before the outputs of the batch it is chained to.
        let mut host = Fake::new(|message| {
            if message == 0 {
                Plan::ReadyThenExit((1..=10).collect())
            } else {
                Plan::Nothing
            }
        });

        let mut messages = vec![0];
        let mut actions = Vec::new();

        // A budget of 4 stops in the middle of the batch.
        let turn = drain(
            &counted(Some(4)),
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert!(turn.yielded);
        assert_eq!(turn.deferred, 1, "the rest of the chain was dropped");
        assert_eq!(
            invalidations(&actions),
            vec![&Invalidation::All],
            "the deferred update lost its compatibility default"
        );
        assert!(
            !actions.iter().any(|action| matches!(action, Action::Exit)),
            "the chained action ran before the batch finished"
        );

        // The runtime delivers the rest, in order.
        host.flush_deferred(&mut messages, &mut actions);

        assert_eq!(
            messages,
            (1..=10).collect::<Vec<_>>(),
            "the deferred outputs lost their order"
        );
        assert!(
            actions.iter().any(|action| matches!(action, Action::Exit)),
            "the chained action never ran"
        );
    }

    #[test]
    fn a_pending_stream_is_handed_to_the_runtime_as_before() {
        let mut host = Fake::new(|message| {
            if message == 0 {
                Plan::ReadyThenPending(7)
            } else {
                Plan::Nothing
            }
        });

        let mut messages = vec![0];
        let mut actions = Vec::new();

        let turn = drain(
            &Budget::default(),
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(host.seen, vec![0, 7]);
        assert_eq!(host.deferred.len(), 1, "the stream was dropped");
        assert_eq!(turn.deferred, 0, "a pending stream is not a budget yield");
        assert!(!turn.yielded, "a pending stream is not a budget yield");
        assert_eq!(turn.queued, 0);
    }

    #[test]
    fn the_clock_can_end_a_turn_without_a_count_limit() {
        let budget = Budget {
            max_messages: None,
            max_stream_items: None,
            max_duration: Some(Duration::from_micros(4_000)),
        };

        // Reads: 0µs, 1_000µs, 2_000µs, then 9_000µs -> yield on the fourth.
        let mut host = Fake::new(|_| Plan::Nothing).with_clock([0, 1_000, 2_000, 9_000], 9_000);
        let mut messages: Vec<u32> = (0..10).collect();
        let mut actions = Vec::new();

        let turn = drain(
            &budget,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert!(turn.yielded);
        assert_eq!(turn.messages, 3, "the clock budget was not honored");
        assert_eq!(turn.queued, 7);
        assert_eq!(host.clock_reads, 4);

        // Time stands still on the next turn: it completes.
        let mut host = Fake::new(|_| Plan::Nothing);
        let turn = drain(
            &budget,
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert!(!turn.must_resume());
        assert_eq!(host.seen, (3..10).collect::<Vec<_>>());
    }

    #[test]
    fn no_clock_is_read_when_there_is_no_duration_budget() {
        let mut host = Fake::new(|_| Plan::Nothing);
        let mut messages: Vec<u32> = (0..10).collect();
        let mut actions = Vec::new();

        let _ = drain(
            &counted(Some(4)),
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(host.clock_reads, 0);
    }

    #[test]
    fn input_and_producer_take_turns() {
        // A producer that never stops: every message produces another one.
        let mut host = Fake::new(|message| Plan::Ready(vec![message + 1]));

        let mut messages = vec![0];
        let mut actions = Vec::new();

        // Ten turns of a continuous producer: each one ends, so the shell gets
        // to handle input between them.
        for _ in 0..10 {
            let turn = drain(
                &counted(Some(8)),
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );

            assert!(turn.yielded, "the producer never gave the turn back");
            assert_eq!(turn.messages, 8);
            assert!(turn.queued > 0, "the producer's work was dropped");
        }

        // Input arrives and is processed in the very next turn, ahead of the
        // producer's queue growing further.
        assert_eq!(host.seen.len(), 80);
    }

    /// The shell drains more than once before it returns to the event loop.
    /// One shared [`Turn`] must mean one shared budget.
    #[test]
    fn several_drains_share_one_turn_budget() {
        let mut host = Fake::new(|_| Plan::Nothing);
        let mut messages: Vec<u32> = (0..100).collect();
        let mut actions = Vec::new();
        let mut turn = Turn::default();

        for _ in 0..5 {
            let _ = drain(
                &counted(Some(10)),
                &mut turn,
                &mut messages,
                &mut actions,
                &mut host,
            );
        }

        assert_eq!(
            turn.messages, 10,
            "the budget was spent once per call instead of once per turn"
        );
        assert_eq!(host.seen.len(), 10);
        assert!(turn.yielded);
        assert_eq!(turn.queued, 90);

        // A new turn gets a new budget.
        let next = drain(
            &counted(Some(10)),
            &mut Turn::default(),
            &mut messages,
            &mut actions,
            &mut host,
        );

        assert_eq!(next.messages, 10);
        assert_eq!(host.seen, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn the_budget_can_be_configured_and_a_typo_cannot_unbound_it() {
        let default = Budget::default();

        assert_eq!(Budget::parse(None, None), default);
        assert_eq!(
            Budget::parse(Some(" 32 "), Some("1500")),
            Budget {
                max_messages: Some(32),
                max_stream_items: Some(32),
                max_duration: Some(Duration::from_micros(1_500)),
            }
        );
        assert_eq!(
            Budget::parse(Some("0"), Some("unlimited")),
            Budget::UNLIMITED
        );
        assert_eq!(
            Budget::parse(Some("banana"), Some("banana")),
            default,
            "an unparseable value must fall back to the default, not to unlimited"
        );
    }

    /// The benchmark the issue asks for, in the only unit a debug build can
    /// report honestly: messages per uninterrupted turn. Run with
    /// `--nocapture` to see the table.
    #[test]
    fn burst_profile_before_and_after() {
        for burst in [100, 1_000, 10_000, 100_000] {
            let mut before = Fake::new(|_| Plan::Nothing);
            let mut messages: Vec<u32> = (0..burst).collect();
            let mut actions = Vec::new();

            let unbounded = drain(
                &Budget::UNLIMITED,
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut before,
            );

            let mut after = Fake::new(|_| Plan::Nothing);
            let mut messages: Vec<u32> = (0..burst).collect();
            let mut turns = 0;
            let mut max_per_turn = 0;

            loop {
                let turn = drain(
                    &Budget::default(),
                    &mut Turn::default(),
                    &mut messages,
                    &mut actions,
                    &mut after,
                );

                turns += 1;
                max_per_turn = max_per_turn.max(turn.messages);

                if !turn.must_resume() {
                    break;
                }
            }

            println!(
                "burst {burst:>6}: before = 1 turn of {} message(s); \
                 after = {turns} turn(s), at most {max_per_turn} per turn",
                unbounded.messages
            );

            assert_eq!(unbounded.messages, burst as usize);
            assert!(
                max_per_turn <= Budget::DEFAULT_MAX_MESSAGES,
                "a turn ran {max_per_turn} messages"
            );
            assert_eq!(
                after.seen,
                (0..burst).collect::<Vec<_>>(),
                "the burst was not completed exactly once, in order"
            );
            assert_eq!(
                turns,
                burst as usize / Budget::DEFAULT_MAX_MESSAGES + 1,
                "turn count is not the burst divided by the budget"
            );
        }
    }

    #[test]
    fn the_default_budget_is_bounded() {
        let budget = Budget::default();

        assert_eq!(budget.max_messages, Some(Budget::DEFAULT_MAX_MESSAGES));
        assert_eq!(
            budget.max_stream_items,
            Some(Budget::DEFAULT_MAX_STREAM_ITEMS)
        );
        assert_eq!(budget.max_duration, Some(Budget::DEFAULT_MAX_DURATION));
        assert!(
            Budget::DEFAULT_MAX_DURATION <= Duration::from_micros(16_667 / 4),
            "the default turn may span more than a quarter of a 60Hz frame"
        );
    }

    #[test]
    fn defer_in_order_is_sticky() {
        let mut deferring = false;

        assert!(!defer_in_order(&mut deferring, false));
        assert!(defer_in_order(&mut deferring, true));
        // Everything after the first deferral is deferred too, in order.
        assert!(defer_in_order(&mut deferring, false));
    }

    #[test]
    fn metrics_accumulate_and_publish() {
        let mut host = Fake::new(|message| {
            if message == 0 {
                Plan::Ready(vec![100, 101])
            } else {
                Plan::Nothing
            }
        });

        let mut messages: Vec<u32> = (0..10).collect();
        let mut actions = Vec::new();
        let mut collected = Metrics::default();

        for _ in 0..2 {
            let turn = drain(
                &counted(Some(4)),
                &mut Turn::default(),
                &mut messages,
                &mut actions,
                &mut host,
            );
            collected.record(&turn);
        }

        assert_eq!(collected.turns, 2);
        assert_eq!(collected.yields, 2);
        assert_eq!(collected.messages, 8);
        assert_eq!(collected.outputs, 2);
        assert_eq!(collected.max_messages_per_turn, 4);
        assert_eq!(collected.longest_cascade, 1);
        assert_eq!(collected.queued, messages.len());
        assert_eq!(messages.len(), 4, "10 messages + 2 outputs - 8 processed");

        publish(&collected);
        assert_eq!(metrics(), collected);
    }
}
