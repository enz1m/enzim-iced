//! Event delivery through built-in and custom subscription recipes.

use iced_futures::futures::{channel::mpsc, task::noop_waker_ref};
use iced_futures::subscription::{self, Event, EventStream, Hasher, Recipe, Tracker};
use iced_futures::{BoxStream, Subscription, event, keyboard};
use iced_runtime::core::{self, Point, Size, time::Instant, window};
use iced_runtime::{system, window as runtime_window};

use std::task::Context;

fn receive<T: Send + 'static>(subscription: Subscription<T>, events: &[Event]) -> Vec<T> {
    let (sender, mut receiver) = mpsc::channel(100);
    let mut tracker = Tracker::new();
    let mut tasks = tracker.update(subscription::into_recipes(subscription).into_iter(), sender);

    for event in events {
        tracker.broadcast(event.clone());
    }

    let mut context = Context::from_waker(noop_waker_ref());
    for task in &mut tasks {
        let _ = task.as_mut().poll(&mut context);
    }

    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

#[test]
fn interaction_subscriptions_keep_their_event_kinds_and_status() {
    use core::event::{Event as Interaction, Status};
    use window::Event as Window;

    let window = window::Id::unique();
    let now = Instant::now();
    let dropped = Window::FileDropped("document.txt".into());
    let resized = Window::Resized(Size::new(800.0, 600.0));
    let key = core::keyboard::Event::CapsLockChanged(true);
    let ime = Interaction::InputMethod(core::input_method::Event::Commit("日本語".into()));
    let touch = Interaction::Touch(core::touch::Event::FingerPressed {
        id: core::touch::Finger(1),
        position: Point::ORIGIN,
    });
    let interactions = [
        (
            Interaction::Window(Window::RedrawRequested(now)),
            Status::Ignored,
        ),
        (Interaction::Window(dropped.clone()), Status::Ignored),
        (Interaction::Window(resized.clone()), Status::Captured),
        (Interaction::Keyboard(key.clone()), Status::Ignored),
        (Interaction::Keyboard(key.clone()), Status::Captured),
        (ime.clone(), Status::Captured),
        (touch.clone(), Status::Ignored),
    ];
    let events = interactions.map(|(event, status)| Event::Interaction {
        window,
        event,
        status,
    });

    let status_listener = event::listen_with(|event, status, id| Some((event, status, id)));
    assert_eq!(
        receive(status_listener, &events),
        events[1..]
            .iter()
            .map(|event| match event {
                Event::Interaction {
                    event,
                    status,
                    window,
                } => (event.clone(), *status, *window),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>(),
    );
    assert_eq!(receive(runtime_window::frames(), &events), [now]);
    assert_eq!(
        receive(runtime_window::events(), &events),
        [(window, dropped), (window, resized)]
    );
    assert_eq!(receive(keyboard::listen(), &events), [key]);
    assert_eq!(
        receive(event::listen_raw(|event, _, _| Some(event)), &events)[0],
        Interaction::Window(Window::RedrawRequested(now)),
    );

    let listener = event::listen_with_interest(
        event::Interest::POINTER.union(event::Interest::OTHER),
        |event, _, _| Some(event),
    )
    .with(17)
    .map(|(tag, event)| (event, tag))
    .filter_map(|(event, tag)| (tag == 17).then_some(event));
    assert_eq!(receive(listener, &events), [ime, touch]);
}

#[test]
fn system_events_and_custom_streams_keep_their_delivery() {
    struct AllEvents;

    impl Recipe for AllEvents {
        type Output = Event;

        fn hash(&self, state: &mut Hasher) {
            std::hash::Hash::hash("subscription_events::AllEvents", state);
        }

        fn stream(self: Box<Self>, input: EventStream) -> BoxStream<Event> {
            input
        }
    }

    let url = "test-scheme://document";
    let window = window::Id::unique();
    let events = [
        Event::SystemThemeChanged(core::theme::Mode::Dark),
        Event::SystemReducedMotionChanged(true),
        Event::PlatformSpecific(subscription::PlatformSpecific::MacOS(
            subscription::MacOS::ReceivedUrl(url.into()),
        )),
        Event::Interaction {
            window,
            event: core::Event::Window(window::Event::CloseRequested),
            status: core::event::Status::Ignored,
        },
    ];

    assert_eq!(receive(event::listen_url(), &events), [url]);
    assert_eq!(receive(runtime_window::close_requests(), &events), [window]);
    assert_eq!(
        receive(system::theme_changes(), &events),
        [core::theme::Mode::Dark]
    );
    assert_eq!(receive(system::reduced_motion_changes(), &events), [true]);
    assert_eq!(
        receive(subscription::from_recipe(AllEvents), &events),
        events
    );
    assert_eq!(
        receive(
            Subscription::run_with(17, |tag| {
                iced_futures::futures::stream::iter([*tag, tag + 1])
            }),
            &events
        ),
        [17, 18],
    );
}
