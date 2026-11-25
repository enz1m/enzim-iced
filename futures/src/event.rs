// Modified by enzUI contributors. See FORK.md.
//! Listen to runtime events.
use crate::MaybeSend;
use crate::core::event::{self, Event};
use crate::core::window;
use crate::subscription::{self, Subscription};

pub use crate::subscription::Interest;

/// Returns a [`Subscription`] to all the ignored runtime events.
///
/// This subscription will notify your application of any [`Event`] that was
/// not captured by any widget.
pub fn listen() -> Subscription<Event> {
    listen_with(|event, status, _window| match status {
        event::Status::Ignored => Some(event),
        event::Status::Captured => None,
    })
}

/// Creates a [`Subscription`] that listens and filters all the runtime events
/// with the provided function, producing messages accordingly.
///
/// The function receives each interaction except redraw requests. If the function:
///
/// - Returns `None`, the [`Event`] will be discarded.
/// - Returns `Some` message, the `Message` will be produced.
pub fn listen_with<Message>(
    f: fn(Event, event::Status, window::Id) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    listen_with_interest(Interest::ALL, f)
}

/// Creates a [`Subscription`] like [`listen_with`] that reads only the
/// interactions of the given [`Interest`]. Redraws are excluded.
pub fn listen_with_interest<Message>(
    interest: Interest,
    f: fn(Event, event::Status, window::Id) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    #[derive(Hash)]
    struct EventsWithInterest;

    subscription::filter_map_with_interest(
        (EventsWithInterest, f),
        interest.without(Interest::SYSTEM.union(Interest::REDRAWS)),
        move |event| match event {
            subscription::Event::Interaction {
                window,
                event,
                status,
            } => f(event, status, window),
            _ => None,
        },
    )
}

/// Creates a [`Subscription`] that produces a message for every runtime event,
/// including the redraw request events.
///
/// **Warning:** This [`Subscription`], if unfiltered, may produce messages in
/// an infinite loop.
pub fn listen_raw<Message>(
    f: fn(Event, event::Status, window::Id) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    #[derive(Hash)]
    struct RawEvents;

    subscription::filter_map_with_interest(
        (RawEvents, f),
        Interest::ALL.without(Interest::SYSTEM),
        move |event| match event {
            subscription::Event::Interaction {
                window,
                event,
                status,
            } => f(event, status, window),
            subscription::Event::SystemThemeChanged(_)
            | subscription::Event::SystemReducedMotionChanged(_)
            | subscription::Event::PlatformSpecific(_) => None,
        },
    )
}

/// Creates a [`Subscription`] that notifies of custom application URL
/// received from the system.
///
/// _**Note:** Currently, it only triggers on macOS and the executable needs to be properly [bundled]!_
///
/// [bundled]: https://developer.apple.com/library/archive/documentation/CoreFoundation/Conceptual/CFBundles/BundleTypes/BundleTypes.html#//apple_ref/doc/uid/10000123i-CH101-SW19
pub fn listen_url() -> Subscription<String> {
    #[derive(Hash)]
    struct ListenUrl;

    subscription::filter_map_with_interest(
        ListenUrl,
        subscription::Interest::SYSTEM,
        move |event| match event {
            subscription::Event::PlatformSpecific(subscription::PlatformSpecific::MacOS(
                subscription::MacOS::ReceivedUrl(url),
            )) => Some(url),
            _ => None,
        },
    )
}
