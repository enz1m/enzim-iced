// Modified by enzUI contributors. See FORK.md.
use crate::futures::futures::{
    Future, Sink, StreamExt,
    channel::mpsc,
    task::{Context, Poll, Waker},
};
use crate::graphics::shell;
use crate::runtime::Action;
use crate::runtime::window;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// An event loop proxy with backpressure that implements `Sink`.
#[derive(Debug)]
pub struct Proxy<T: 'static> {
    raw: winit::event_loop::EventLoopProxy<Action<T>>,
    sender: mpsc::Sender<Action<T>>,
    slots: Arc<Slots>,
}

// Actions sent through the `Sink` and not yet freed by the event loop. A
// sender only parks, and `free_slots` only wakes, when the limit is reached.
// Both sides store one flag and then load the other, so every access is
// `SeqCst`: a weaker order could lose the wake of a parked sender.
#[derive(Debug, Default)]
struct Slots {
    in_flight: AtomicUsize,
    has_waiters: AtomicBool,
    waiters: Mutex<Vec<Waker>>,
}

impl<T: 'static> Clone for Proxy<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            sender: self.sender.clone(),
            slots: self.slots.clone(),
        }
    }
}

impl<T: 'static> Proxy<T> {
    const MAX_SIZE: usize = 100;

    /// Creates a new [`Proxy`] from an `EventLoopProxy`.
    ///
    /// The returned future forwards the actions of [`Proxy::input`].
    pub fn new(
        raw: winit::event_loop::EventLoopProxy<Action<T>>,
    ) -> (Self, impl Future<Output = ()>) {
        let (sender, mut receiver) = mpsc::channel(Self::MAX_SIZE);
        let proxy = raw.clone();

        let worker = async move {
            while let Some(action) = receiver.next().await {
                let _ = proxy.send_event(action);
            }
        };

        (
            Self {
                raw,
                sender,
                slots: Arc::new(Slots::default()),
            },
            worker,
        )
    }

    /// Returns a reference to the underlying `winit::event_loop::EventLoopProxy`.
    ///
    /// Used by the accessibility `ActionBridge` waker (#309): the bridge
    /// needs to wake the event loop from a foreign (AT-SPI) thread without
    /// knowing the `Action<T>` generic, so the caller type-erases it into
    /// a closure before handing it over.
    pub fn raw(&self) -> &winit::event_loop::EventLoopProxy<Action<T>> {
        &self.raw
    }

    /// Creates an input channel for the [`Proxy`].
    pub fn input(&self) -> mpsc::Sender<Action<T>> {
        self.sender.clone()
    }

    /// Sends a value to the event loop.
    ///
    /// Note: This skips the backpressure mechanism with an unbounded
    /// channel. Use sparingly!
    pub fn send(&self, value: T) {
        self.send_action(Action::Output(value));
    }

    /// Sends an action to the event loop.
    ///
    /// Note: This skips the backpressure mechanism with an unbounded
    /// channel. Use sparingly!
    pub fn send_action(&self, action: Action<T>) {
        let _ = self.raw.send_event(action);
    }

    /// Frees an amount of slots for additional messages to be queued in
    /// this [`Proxy`].
    pub fn free_slots(&mut self, amount: usize) {
        self.slots.free(amount);
    }
}

impl Slots {
    fn poll_ready(&self, limit: usize, cx: &mut Context<'_>) -> Poll<()> {
        if self.in_flight.load(Ordering::SeqCst) < limit {
            return Poll::Ready(());
        }

        self.waiters
            .lock()
            .expect("Lock waiters")
            .push(cx.waker().clone());
        self.has_waiters.store(true, Ordering::SeqCst);

        // A slot freed between the check and the registration must not be missed.
        if self.in_flight.load(Ordering::SeqCst) < limit {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn acquire(&self) {
        let _ = self.in_flight.fetch_add(1, Ordering::SeqCst);
    }

    fn free(&self, amount: usize) {
        let _ = self
            .in_flight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                Some(count.saturating_sub(amount))
            });

        if self.has_waiters.swap(false, Ordering::SeqCst) {
            let waiters = std::mem::take(&mut *self.waiters.lock().expect("Lock waiters"));
            for waker in waiters {
                waker.wake();
            }
        }
    }
}

impl<T: 'static> Sink<Action<T>> for Proxy<T> {
    type Error = mpsc::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.slots.poll_ready(Self::MAX_SIZE, cx).map(Ok)
    }

    fn start_send(self: Pin<&mut Self>, action: Action<T>) -> Result<(), Self::Error> {
        self.slots.acquire();
        let _ = self.raw.send_event(action);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl<T> shell::Notifier for Proxy<T>
where
    T: Send,
{
    fn tick(&self) {
        self.send_action(Action::Tick);
    }

    fn request_redraw(&self) {
        self.send_action(Action::Window(window::Action::RedrawAll));
    }

    fn invalidate_layout(&self) {
        self.send_action(Action::Window(window::Action::RelayoutAll));
    }
}

#[cfg(test)]
mod tests {
    use super::Slots;
    use crate::futures::futures::task::{ArcWake, Context, Poll, waker};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingWaker(AtomicUsize);

    impl ArcWake for CountingWaker {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            let _ = arc_self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn sender_parks_at_the_limit_until_a_slot_is_freed() {
        let slots = Slots::default();
        let wakes = Arc::new(CountingWaker::default());
        let waker = waker(wakes.clone());
        let mut cx = Context::from_waker(&waker);

        for _ in 0..2 {
            assert_eq!(slots.poll_ready(2, &mut cx), Poll::Ready(()));
            slots.acquire();
        }
        assert_eq!(slots.poll_ready(2, &mut cx), Poll::Pending);
        assert_eq!(wakes.0.load(Ordering::SeqCst), 0);

        slots.free(1);

        assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(slots.poll_ready(2, &mut cx), Poll::Ready(()));
    }

    #[test]
    fn freeing_below_the_limit_wakes_nobody() {
        let slots = Slots::default();
        let wakes = Arc::new(CountingWaker::default());
        let waker = waker(wakes.clone());
        let mut cx = Context::from_waker(&waker);

        slots.acquire();
        assert_eq!(slots.poll_ready(2, &mut cx), Poll::Ready(()));
        slots.free(5);

        assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
        assert_eq!(slots.in_flight.load(Ordering::SeqCst), 0);
        assert!(slots.waiters.lock().unwrap().is_empty());
    }
}
