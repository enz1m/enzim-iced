// Modified by enzUI contributors. See FORK.md.
//! One reader for the guest connections on winit's `wl_display`.
//!
//! Every thread that polls a Wayland socket wakes for every event the
//! compositor sends, whichever queue the event is for. A [`GuestConnection`]
//! is one foreign-display backend on winit's display, shared by every guest
//! in the process, with one thread that reads its socket. A read routes each
//! event to the queue that owns its object and wakes that queue's waker, so a
//! guest either runs a [`Pump`] on the reader thread or sleeps on a waker of
//! its own (`EventQueue::poll_dispatch_pending`) and wakes only for its own
//! events.
//!
//! The backend is a guest view of winit's display, never a second
//! connection: object ids stay valid across winit and its guests. Winit keeps
//! reading the socket for its own queue, so its events are unaffected.

use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use wayland_client::Connection;
use wayland_client::backend::Backend;

/// How long dropping the last share waits for the reader to stop before it
/// detaches the thread.
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(250);

type PumpFn = Box<dyn FnMut() -> bool + Send>;

/// A foreign-display backend on winit's display and the thread that reads
/// it.
pub struct GuestConnection {
    connection: Connection,
    reader: Reader,
    // The reader thread holds its own share, so a detached reader cannot
    // outlive the display either.
    _display: Arc<winit::event_loop::OwnedDisplayHandle>,
}

impl std::fmt::Debug for GuestConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GuestConnection")
            .finish_non_exhaustive()
    }
}

impl GuestConnection {
    /// The guest connection on `display`, shared by every caller on the same
    /// display.
    ///
    /// Returns `None` off Wayland or when the reader thread cannot start.
    pub fn of(display: &winit::event_loop::OwnedDisplayHandle) -> Option<Arc<Self>> {
        use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

        static GUESTS: Mutex<Vec<(usize, Weak<GuestConnection>)>> = Mutex::new(Vec::new());

        let pointer = match display.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(handle) => handle.display.as_ptr(),
            _ => return None,
        };
        let mut guests = GUESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A live guest keeps its display open, so its address cannot be
        // reused by another display while the entry upgrades.
        guests.retain(|(_, guest)| guest.strong_count() > 0);
        if let Some(guest) = guests
            .iter()
            .find(|(known, _)| *known == pointer as usize)
            .and_then(|(_, guest)| guest.upgrade())
        {
            return Some(guest);
        }
        let display = Arc::new(display.clone());

        // SAFETY: `pointer` is winit's live `wl_display`, confirmed Wayland
        // above. The guest and its reader thread each hold a share of the
        // `OwnedDisplayHandle`, and `from_foreign_display` does not take
        // ownership, so winit closes the display after the last share drops.
        #[allow(unsafe_code, reason = "adopts winit's Wayland display")]
        let backend = unsafe { Backend::from_foreign_display(pointer.cast()) };
        let guest = Arc::new(Self::start(Connection::from_backend(backend), display)?);
        guests.push((pointer as usize, Arc::downgrade(&guest)));
        Some(guest)
    }

    fn start(
        connection: Connection,
        display: Arc<winit::event_loop::OwnedDisplayHandle>,
    ) -> Option<Self> {
        let reader_connection = connection.clone();
        let reader_display = Arc::clone(&display);
        let reader = Reader::spawn(move |shared, wake| {
            let _display = reader_display;
            read(&reader_connection, &shared, &wake);
        })?;
        Some(Self {
            connection,
            reader,
            _display: display,
        })
    }

    /// The guest connection. Queues made on it are dispatched by their
    /// owners; the reader only routes events to them.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Runs `pump` on the reader thread after every read, and once soon
    /// after this call, until it returns `false` or the returned [`Pump`] is
    /// removed or dropped. A pump that panics is dropped.
    pub fn pump(&self, pump: impl FnMut() -> bool + Send + 'static) -> Pump {
        let id = {
            let mut pumps = self.reader.shared.lock_pumps();
            pumps.next += 1;
            let id = pumps.next;
            pumps.list.push((id, Box::new(pump)));
            id
        };
        self.reader.shared.nudge();
        Pump {
            id,
            shared: Arc::downgrade(&self.reader.shared),
        }
    }
}

/// A pump registered on a [`GuestConnection`]'s reader; dropping it removes
/// the pump.
#[derive(Debug)]
pub struct Pump {
    id: u64,
    shared: Weak<Shared>,
}

impl Pump {
    /// Stops running the pump. Idempotent. A pump the reader is running
    /// right now finishes that run and is then dropped on the reader thread.
    pub fn remove(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let removed = {
            let mut pumps = shared.lock_pumps();
            match pumps.list.iter().position(|(id, _)| *id == self.id) {
                Some(index) => Some(pumps.list.remove(index)),
                None => {
                    pumps.removed.push(self.id);
                    None
                }
            }
        };
        drop(removed);
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.remove();
    }
}

#[derive(Default)]
struct Pumps {
    next: u64,
    list: Vec<(u64, PumpFn)>,
    /// Pumps removed while the reader was running them.
    removed: Vec<u64>,
}

struct Shared {
    stop: AtomicBool,
    pumps: Mutex<Pumps>,
    nudge: UnixStream,
}

impl Shared {
    fn lock_pumps(&self) -> std::sync::MutexGuard<'_, Pumps> {
        self.pumps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn nudge(&self) {
        use std::io::Write as _;

        let _ = (&self.nudge).write(&[1]);
    }

    /// Runs every pump outside the lock, keeping those that want more.
    fn run_pumps(&self) {
        let mut running = std::mem::take(&mut self.lock_pumps().list);
        running.retain_mut(|(_, pump)| catch_unwind(AssertUnwindSafe(&mut *pump)).unwrap_or(false));
        let mut pumps = self.lock_pumps();
        let removed = std::mem::take(&mut pumps.removed);
        running.retain(|(id, _)| !removed.contains(id));
        running.append(&mut pumps.list);
        pumps.list = running;
    }
}

/// The reader thread, stopped within a bounded wait when the guest drops.
struct Reader {
    shared: Arc<Shared>,
    done: Mutex<mpsc::Receiver<()>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Reader {
    fn spawn(run: impl FnOnce(Arc<Shared>, UnixStream) + Send + 'static) -> Option<Self> {
        let (wake, nudge) = UnixStream::pair().ok()?;
        nudge.set_nonblocking(true).ok()?;
        wake.set_nonblocking(true).ok()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            pumps: Mutex::new(Pumps::default()),
            nudge,
        });
        let worker_shared = Arc::clone(&shared);
        let (done_sender, done) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("iced-wayland-reader".into())
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| run(worker_shared, wake)));
                let _ = done_sender.try_send(());
            })
            .ok()?;
        Some(Self {
            shared,
            done: Mutex::new(done),
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Stops the reader, waiting at most `budget` for it to acknowledge;
    /// returns whether it did. Idempotent.
    fn shutdown(&self, budget: Duration) -> bool {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.nudge();
        let Some(worker) = self.worker.lock().ok().and_then(|mut slot| slot.take()) else {
            return true;
        };
        let acknowledged = self
            .done
            .lock()
            .is_ok_and(|done| done.recv_timeout(budget).is_ok());
        drop(worker);
        acknowledged
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.shutdown(SHUTDOWN_BUDGET);
    }
}

/// What woke the reader.
enum Woken {
    Readable,
    Nudged,
    Lost,
}

fn wait(socket: std::os::fd::BorrowedFd<'_>, nudge: &UnixStream) -> std::io::Result<Woken> {
    use rustix::event::{PollFd, PollFlags, poll};

    loop {
        let mut fds = [
            PollFd::new(&socket, PollFlags::IN),
            PollFd::new(nudge, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        if fds[1]
            .revents()
            .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
        {
            return Ok(Woken::Nudged);
        }
        let wayland = fds[0].revents();
        if wayland.intersects(PollFlags::HUP | PollFlags::ERR) {
            return Ok(Woken::Lost);
        }
        if wayland.contains(PollFlags::IN) {
            return Ok(Woken::Readable);
        }
    }
}

fn drain(nudge: &UnixStream) {
    use std::io::Read as _;

    let mut scratch = [0u8; 64];
    while matches!((&*nudge).read(&mut scratch), Ok(count) if count == scratch.len()) {}
}

/// Reads the socket until the guest stops or the connection is lost.
fn read(connection: &Connection, shared: &Shared, nudge: &UnixStream) {
    while !shared.stop.load(Ordering::Acquire) {
        shared.run_pumps();
        if let Err(error) = connection.flush()
            && !matches!(&error, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
        {
            break;
        }
        // Routes events another reader left in this backend's queue.
        if connection.backend().dispatch_inner_queue().is_err() {
            break;
        }
        let Some(guard) = connection.prepare_read() else {
            continue;
        };
        match wait(guard.connection_fd(), nudge) {
            // `read` routes what it read to the owning queues and wakes
            // their wakers.
            Ok(Woken::Readable) => {
                if guard.read().is_err() {
                    break;
                }
            }
            Ok(Woken::Nudged) => {
                // Cancelling the prepared read keeps libwayland's
                // synchronized-read protocol for every other reader.
                drop(guard);
                drain(nudge);
            }
            Ok(Woken::Lost) | Err(_) => {
                drop(guard);
                break;
            }
        }
    }
    shared.run_pumps();
    // Pumps own queues on this connection; they go before the display.
    let pumps = std::mem::take(&mut shared.lock_pumps().list);
    drop(pumps);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_reader_shutdown_is_bounded_and_the_detached_reader_finishes() {
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel::<()>();
        let drops = Arc::new(AtomicUsize::new(0));
        let worker_drops = Arc::clone(&drops);
        let reader = Reader::spawn(move |_, _| {
            let _probe = DropProbe(worker_drops);
            let _ = entered_tx.send(());
            let _ = release.recv_timeout(Duration::from_secs(2));
        })
        .expect("the reader spawns");
        entered
            .recv_timeout(Duration::from_secs(1))
            .expect("the reader runs");

        let started = Instant::now();
        assert!(!reader.shutdown(Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        drop(release_tx);
        let deadline = Instant::now() + Duration::from_secs(1);
        while drops.load(Ordering::SeqCst) != 1 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pumps_run_until_they_decline_panic_or_are_removed() {
        let shared = Shared {
            stop: AtomicBool::new(false),
            pumps: Mutex::new(Pumps::default()),
            nudge: UnixStream::pair().expect("a socket pair").0,
        };
        let runs = Arc::new([(); 4].map(|()| AtomicUsize::new(0)));
        let add = |index: usize, keep: fn(usize) -> bool| {
            let runs = Arc::clone(&runs);
            let mut pumps = shared.lock_pumps();
            pumps.next += 1;
            let id = pumps.next;
            pumps.list.push((
                id,
                Box::new(move || {
                    let run = runs[index].fetch_add(1, Ordering::SeqCst) + 1;
                    keep(run)
                }),
            ));
            id
        };
        let _ = add(0, |_| true);
        let _ = add(1, |run| run < 2);
        let _ = add(2, |_| panic!("a pump that panics"));
        let removed = add(3, |_| true);
        shared.lock_pumps().removed.push(removed);

        for _ in 0..3 {
            shared.run_pumps();
        }
        let counts = runs.each_ref().map(|run| run.load(Ordering::SeqCst));
        assert_eq!(counts, [3, 2, 1, 1]);
        assert_eq!(shared.lock_pumps().list.len(), 1);
    }
}
