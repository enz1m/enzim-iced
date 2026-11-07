// Modified by enzUI contributors. See FORK.md.
//! A daemon without a visible window keeps processing messages under a tight
//! message budget (enzICED #4).
//!
//! Only `RedrawRequested` used to start a fairness turn. A window that is
//! occluded, or absent as here, never redraws, so once one turn's budget was
//! spent every later `AboutToWait` drained and requeued the bus without
//! processing a message: the UI thread spun and input was never handled. The
//! turn's clock was never restarted either, so even the default budget ran out
//! a few milliseconds after startup.
//!
//! The parent spawns this binary as a child running a windowless daemon with
//! `ICED_MESSAGE_BUDGET=1` and a ticking subscription, and requires it to exit
//! by itself after [`MESSAGES`] messages. It needs a Wayland or X11 display.
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const CHILD: &str = "ICED_BACKGROUND_FAIRNESS_CHILD";
const MESSAGES: usize = 100;
const TICK: Duration = Duration::from_millis(5);
const DEADLINE: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    if std::env::var_os(CHILD).is_some() {
        return child();
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!("background_fairness: no Wayland or X11 display; the check did not run");
        return ExitCode::FAILURE;
    }

    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .env(CHILD, "1")
        .env("ICED_MESSAGE_BUDGET", "1")
        .spawn()
        .expect("spawn the windowless daemon");

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for the daemon") {
            break Some(status);
        }
        if started.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    match status {
        Some(status) if status.success() => {
            println!(
                "background_fairness: {MESSAGES} messages under a one-message budget in {:?}\n\
                 test result: ok. 1 passed; 0 failed",
                started.elapsed()
            );
            ExitCode::SUCCESS
        }
        Some(status) => {
            eprintln!("background_fairness: daemon failed with {status}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!(
                "background_fairness: a windowless daemon did not process {MESSAGES} \
                 messages within {DEADLINE:?}: the message budget never reset\n\
                 test result: FAILED. 0 passed; 1 failed"
            );
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone)]
struct Tick;

fn child() -> ExitCode {
    use iced::{Element, Subscription, Task, widget, window};

    fn update(seen: &mut usize, _tick: Tick) -> Task<Tick> {
        *seen += 1;

        if *seen >= MESSAGES {
            iced::exit()
        } else {
            Task::none()
        }
    }

    fn view(_seen: &usize, _window: window::Id) -> Element<'_, Tick> {
        widget::space().into()
    }

    fn ticks() -> iced::futures::channel::mpsc::UnboundedReceiver<Tick> {
        let (sender, receiver) = iced::futures::channel::mpsc::unbounded();

        let _ = std::thread::spawn(move || {
            while sender.unbounded_send(Tick).is_ok() {
                std::thread::sleep(TICK);
            }
        });

        receiver
    }

    let result = iced::daemon(|| 0_usize, update, view)
        .subscription(|_| Subscription::run(ticks))
        .run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("background_fairness: daemon error: {error}");
            ExitCode::FAILURE
        }
    }
}
