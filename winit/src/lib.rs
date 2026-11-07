// Modified by enzUI contributors. See FORK.md.
//! A windowing shell for Iced, on top of [`winit`].
//!
//! ![The native path of the Iced ecosystem](https://github.com/iced-rs/iced/blob/0525d76ff94e828b7b21634fa94a747022001c83/docs/graphs/native.png?raw=true)
//!
//! `iced_winit` offers some convenient abstractions on top of [`iced_runtime`]
//! to quickstart development when using [`winit`].
//!
//! It exposes a renderer-agnostic [`Program`] trait that can be implemented
//! and then run with a simple call. The use of this trait is optional.
//!
//! Additionally, a [`conversion`] module is available for users that decide to
//! implement a custom event loop.
//!
//! [`iced_runtime`]: https://github.com/iced-rs/iced/tree/master/runtime
//! [`winit`]: https://github.com/rust-windowing/winit
//! [`conversion`]: crate::conversion
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/iced-rs/iced/9ab6923e943f784985e9ef9ca28b10278297225d/docs/logo.svg"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub use iced_debug as debug;
pub use iced_program as program;
pub use iced_runtime as runtime;
pub use program::core;
pub use program::graphics;
pub use runtime::futures;
pub use winit;

#[cfg(feature = "a11y")]
pub mod accessibility;
pub mod clipboard;
pub mod conversion;
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "wayland"
))]
pub mod guest;
pub mod invalidation;
pub mod presentation;

#[cfg(any(feature = "a11y", test))]
mod accessibility_publication;
mod error;
mod kinetic;
mod layout_ownership;
mod modal;
mod overflow;
mod pointer_hold;
mod proxy;
mod replay_verify;
mod window;

pub use clipboard::Clipboard;
pub use error::Error;
pub use invalidation::Plan;
pub use proxy::Proxy;
/// The registration-removal helper the window manager uses for Wayland
/// presentation teardown, exposed so its lifecycle can be gated directly
/// (enzICED #2219). Not part of the supported surface.
#[doc(hidden)]
pub use window::remove_registered;

use crate::core::backend;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::shell;
use crate::core::theme;
use crate::core::time::{Duration, Instant};
use crate::core::widget::operation;
use crate::core::{Point, Renderer, Size};
use crate::futures::futures::channel::mpsc;
use crate::futures::futures::channel::oneshot;
use crate::futures::futures::task;
use crate::futures::futures::{Future, StreamExt};
use crate::futures::subscription;
use crate::futures::{Executor, Runtime};
use crate::graphics::{Compositor, Shell, compositor};
use crate::runtime::fairness;
use crate::runtime::font;
use crate::runtime::image;
use crate::runtime::system;
use crate::runtime::user_interface::{self, UserInterface};
use crate::runtime::{Action, Task};

use program::Program;

use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::slice;
use std::sync::Arc;

#[cfg(feature = "performance-observer")]
fn performance_window(id: core::window::Id) -> runtime::performance::WindowId {
    runtime::performance::WindowId(id.as_u64())
}

#[cfg(feature = "performance-observer")]
fn performance_quality(
    quality: compositor::PresentationQuality,
) -> runtime::performance::PresentationQuality {
    match quality {
        compositor::PresentationQuality::Submitted => {
            runtime::performance::PresentationQuality::Submitted
        }
        compositor::PresentationQuality::SurfacePresented => {
            runtime::performance::PresentationQuality::SurfacePresented
        }
        compositor::PresentationQuality::CompositorPresented => {
            runtime::performance::PresentationQuality::CompositorPresented
        }
    }
}

#[cfg(target_os = "linux")]
fn take_startup_activation_from_env() -> Option<core::window::ActivationToken> {
    const MAX_TOKEN_BYTES: usize = 4096;
    let read = |name| {
        std::env::var(name)
            .ok()
            .filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_BYTES)
    };
    let token = core::window::ActivationToken::from_platform_data(
        read("XDG_ACTIVATION_TOKEN"),
        read("DESKTOP_STARTUP_ID"),
    );
    winit::platform::startup_notify::reset_activation_token_env();
    token
}

/// Runs a [`Program`] with the provided settings.
pub fn run<P>(program: P) -> Result<(), Error>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    use winit::event_loop::EventLoop;

    let boot_span = debug::boot();
    let settings = program.settings();
    let window_settings = program.window();
    #[cfg(target_os = "linux")]
    let startup_activation = take_startup_activation_from_env();

    let event_loop = EventLoop::with_user_event()
        .build()
        .expect("Create event loop");

    let backend_settings = backend::Settings::from(&settings);
    let renderer_settings = renderer::Settings::from(&settings);
    let display_handle = event_loop.owned_display_handle();

    let (proxy, worker) = Proxy::new(event_loop.create_proxy());

    #[cfg(feature = "debug")]
    {
        let proxy = proxy.clone();

        debug::on_hotpatch(move || {
            proxy.send_action(Action::Reload);
        });
    }

    let mut runtime = {
        let executor = P::Executor::new().map_err(Error::ExecutorCreationFailed)?;
        executor.spawn(worker);

        Runtime::new(executor, proxy.clone())
    };

    let (program, task) = runtime.enter(|| program::Instance::new(program));
    let is_daemon = window_settings.is_none();

    let task = if let Some(window_settings) = window_settings {
        let mut task = Some(task);

        let (_id, open) = runtime::window::open(window_settings);

        open.then(move |_| task.take().unwrap_or_else(Task::none))
    } else {
        task
    };

    if let Some(stream) = runtime::task::into_stream(task) {
        runtime.run(stream);
    }

    runtime.track(subscription::into_recipes(
        runtime.enter(|| program.subscription().map(Action::Output)),
    ));

    let (event_sender, event_receiver) = mpsc::unbounded();
    let (control_sender, control_receiver) = mpsc::unbounded();
    let (system_theme_sender, system_theme_receiver) = oneshot::channel();
    let (system_reduced_motion_sender, system_reduced_motion_receiver) = oneshot::channel();

    let instance: std::pin::Pin<Box<dyn Future<Output = ()>>> = Box::pin(run_instance::<P>(
        program,
        runtime,
        proxy.clone(),
        event_receiver,
        control_sender,
        display_handle,
        is_daemon,
        backend_settings,
        renderer_settings,
        settings.fonts,
        system_theme_receiver,
        system_reduced_motion_receiver,
    ));

    let context = task::Context::from_waker(task::noop_waker_ref());

    struct Runner<Message: 'static> {
        instance: std::pin::Pin<Box<dyn Future<Output = ()>>>,
        context: task::Context<'static>,
        id: Option<String>,
        sender: mpsc::UnboundedSender<Event<Action<Message>>>,
        receiver: mpsc::UnboundedReceiver<Control>,
        error: Option<Error>,
        system_theme: Option<oneshot::Sender<theme::Mode>>,
        system_reduced_motion: Option<oneshot::Sender<bool>>,
        /// Image cursors made so far (enzICED).
        custom_cursors: FxHashMap<mouse::CustomCursor, Option<winit::window::CustomCursor>>,
        #[cfg(target_os = "linux")]
        startup_activation: Option<core::window::ActivationToken>,

        /// Type-erased wake handle for the accessibility lifecycle and action
        /// bridges. Constructed once from the raw `EventLoopProxy` and handed
        /// to every `WindowAccessibility` so platform threads can wake the
        /// event loop without knowing `Action<Message>`.
        #[cfg(feature = "a11y")]
        a11y_waker: std::sync::Arc<dyn Fn() + Send + Sync>,

        #[cfg(target_arch = "wasm32")]
        canvas: Option<web_sys::HtmlCanvasElement>,
    }

    // Build the type-erased waker for the accessibility bridges. The concrete
    // closure captures the raw `EventLoopProxy` and sends `Action::Tick` — the
    // lightest existing action — to kick the loop out of `ControlFlow::Wait`.
    // Type-erasing here keeps the accessibility module generic-free.
    #[cfg(feature = "a11y")]
    let a11y_waker: std::sync::Arc<dyn Fn() + Send + Sync> = {
        let raw_proxy = proxy.raw().clone();
        std::sync::Arc::new(move || {
            let _ = raw_proxy.send_event(Action::Tick);
        })
    };

    let runner = Runner {
        instance,
        context,
        id: settings.id,
        sender: event_sender,
        receiver: control_receiver,
        error: None,
        system_theme: Some(system_theme_sender),
        system_reduced_motion: Some(system_reduced_motion_sender),
        custom_cursors: FxHashMap::default(),
        #[cfg(target_os = "linux")]
        startup_activation,

        #[cfg(feature = "a11y")]
        a11y_waker,

        #[cfg(target_arch = "wasm32")]
        canvas: None,
    };

    boot_span.finish();

    impl<Message> winit::application::ApplicationHandler<Action<Message>> for Runner<Message> {
        fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            // enzICED: overlays may leave the window where popups can show them.
            #[cfg(all(target_os = "linux", feature = "wayland"))]
            {
                use winit::platform::wayland::ActiveEventLoopExtWayland as _;

                let disabled =
                    std::env::var_os("ICED_OVERLAY_POPUPS").is_some_and(|value| value == "0");
                if event_loop.is_wayland() && !disabled {
                    let screen = event_loop
                        .available_monitors()
                        .map(|monitor| monitor.size().to_logical::<f32>(monitor.scale_factor()))
                        .fold(Size::ZERO, |largest, size| {
                            Size::new(
                                largest.width.max(size.width),
                                largest.height.max(size.height),
                            )
                        });
                    let reach = if screen.width > 0.0 && screen.height > 0.0 {
                        screen
                    } else {
                        Size::new(1920.0, 1080.0)
                    };
                    core::overlay::set_popup_reach(Some(reach));
                }
            }

            if let Some(sender) = self.system_theme.take() {
                let _ = sender.send(
                    event_loop
                        .system_theme()
                        .map(conversion::theme_mode)
                        .unwrap_or_default(),
                );
            }

            // winit exposes no native reduced-motion query, so off the
            // `linux-theme-detection` path the preference defaults to
            // "no preference" (`false`). On Linux, `run_instance` reads it
            // from mundy and ignores this channel.
            if let Some(sender) = self.system_reduced_motion.take() {
                let _ = sender.send(false);
            }
        }

        fn new_events(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            cause: winit::event::StartCause,
        ) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::NewEvents(cause)),
            );
        }

        fn window_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            window_id: winit::window::WindowId,
            event: winit::event::WindowEvent,
        ) {
            #[cfg(target_os = "windows")]
            let is_move_or_resize = matches!(
                event,
                winit::event::WindowEvent::Resized(_) | winit::event::WindowEvent::Moved(_)
            );

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::WindowEvent { window_id, event }),
            );

            // TODO: Remove when unnecessary
            // On Windows, we emulate an `AboutToWait` event after every `Resized` event
            // since the event loop does not resume during resize interaction.
            // More details: https://github.com/rust-windowing/winit/issues/3272
            #[cfg(target_os = "windows")]
            {
                if is_move_or_resize {
                    self.process_event(
                        event_loop,
                        Event::EventLoopAwakened(winit::event::Event::AboutToWait),
                    );
                }
            }
        }

        fn user_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            action: Action<Message>,
        ) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::UserEvent(action)),
            );
        }

        fn received_url(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, url: String) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::PlatformSpecific(
                    winit::event::PlatformSpecific::MacOS(winit::event::MacOS::ReceivedUrl(url)),
                )),
            );
        }

        fn device_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            device_id: winit::event::DeviceId,
            event: winit::event::DeviceEvent,
        ) {
            // enzICED: only relative motion is used, by pointer grabs.
            if matches!(event, winit::event::DeviceEvent::MouseMotion { .. }) {
                self.process_event(
                    event_loop,
                    Event::EventLoopAwakened(winit::event::Event::DeviceEvent { device_id, event }),
                );
            }
        }

        fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::AboutToWait),
            );
        }
    }

    impl<Message> Runner<Message> {
        fn process_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            event: Event<Action<Message>>,
        ) {
            if event_loop.exiting() {
                return;
            }

            self.sender.start_send(event).expect("Send event");

            loop {
                let poll = self.instance.as_mut().poll(&mut self.context);

                match poll {
                    task::Poll::Pending => match self.receiver.try_recv() {
                        Ok(control) => match control {
                            Control::ChangeFlow(flow) => {
                                use winit::event_loop::ControlFlow;

                                match (event_loop.control_flow(), flow) {
                                    (
                                        ControlFlow::WaitUntil(current),
                                        ControlFlow::WaitUntil(new),
                                    ) if current > Instant::now() && current < new => {}
                                    (ControlFlow::WaitUntil(current), ControlFlow::Wait)
                                        if current > Instant::now() => {}
                                    _ => {
                                        event_loop.set_control_flow(flow);
                                    }
                                }
                            }
                            Control::CreateWindow {
                                id,
                                settings,
                                activation_token,
                                title,
                                scale_factor,
                                monitor,
                                on_open,
                                parent,
                            } => {
                                let exit_on_close_request = settings.exit_on_close_request;
                                // A transparent window whose frame the app draws
                                // (no decorations, or decorations it draws itself).
                                #[cfg(target_os = "linux")]
                                let app_frame = !settings.decorations
                                    || settings.platform_specific.app_drawn_decorations;
                                #[cfg(not(target_os = "linux"))]
                                let app_frame = !settings.decorations;
                                let clears_transparent = settings.transparent && app_frame;
                                let modal = settings.modal;
                                let modal_parent =
                                    parent.filter(|_| modal).map(|(parent, _)| parent);

                                let visible = settings.visible;

                                #[cfg(target_arch = "wasm32")]
                                let target = settings.platform_specific.target.clone();

                                let window_attributes = conversion::window_attributes(
                                    settings,
                                    &title,
                                    scale_factor,
                                    monitor.or(event_loop.primary_monitor()),
                                    self.id.clone(),
                                )
                                .with_visible(false);

                                #[cfg(target_os = "linux")]
                                let window_attributes = {
                                    use winit::platform::startup_notify::WindowAttributesExtStartupNotify;

                                    let startup = self.startup_activation.take();
                                    use core::window::raw_window_handle::HasDisplayHandle as _;
                                    let platform = event_loop
                                        .display_handle()
                                        .ok()
                                        .and_then(|display| activation_platform(display.as_raw()));
                                    match activation_token
                                        .or(startup)
                                        .and_then(|token| {
                                            platform
                                                .and_then(|platform| token.into_raw_for(platform))
                                        })
                                        .map(winit::window::ActivationToken::from_raw)
                                    {
                                        Some(token) => {
                                            window_attributes.with_activation_token(token)
                                        }
                                        None => window_attributes,
                                    }
                                };
                                #[cfg(not(target_os = "linux"))]
                                let _ = activation_token;

                                // enzICED: a dialog stays with its parent.
                                #[cfg(all(target_os = "linux", feature = "wayland"))]
                                let window_attributes = match parent {
                                    Some((_, raw)) => {
                                        use winit::platform::wayland::WindowAttributesExtWayland as _;
                                        window_attributes.with_transient_parent(raw, modal)
                                    }
                                    None => window_attributes,
                                };
                                #[cfg(all(
                                    target_os = "linux",
                                    not(feature = "wayland"),
                                    feature = "x11"
                                ))]
                                let window_attributes = match parent {
                                    Some((_, raw)) => {
                                        use winit::platform::x11::WindowAttributesExtX11 as _;
                                        window_attributes.with_transient_parent(raw, modal)
                                    }
                                    None => window_attributes,
                                };

                                #[cfg(target_arch = "wasm32")]
                                let window_attributes = {
                                    use winit::platform::web::WindowAttributesExtWebSys;
                                    window_attributes.with_canvas(self.canvas.take())
                                };

                                log::info!(
                                    "Window attributes for id `{id:#?}`: {window_attributes:#?}"
                                );

                                // On macOS, the `position` in `WindowAttributes` represents the "inner"
                                // position of the window; while on other platforms it's the "outer" position.
                                // We fix the inconsistency on macOS by positioning the window after creation.
                                #[cfg(target_os = "macos")]
                                let mut window_attributes = window_attributes;

                                #[cfg(target_os = "macos")]
                                let position = window_attributes.position.take();

                                let window = event_loop
                                    .create_window(window_attributes)
                                    .expect("Create window");

                                #[cfg(target_os = "macos")]
                                if let Some(position) = position {
                                    window.set_outer_position(position);
                                }

                                #[cfg(target_arch = "wasm32")]
                                {
                                    use winit::platform::web::WindowExtWebSys;

                                    let canvas = window.canvas().expect("Get window canvas");

                                    let _ = canvas.set_attribute(
                                        "style",
                                        "display: block; width: 100%; height: 100%",
                                    );

                                    let window = web_sys::window().unwrap();
                                    let document = window.document().unwrap();
                                    let body = document.body().unwrap();

                                    let target = target.and_then(|target| {
                                        body.query_selector(&format!("#{target}"))
                                            .ok()
                                            .unwrap_or(None)
                                    });

                                    match target {
                                        Some(node) => {
                                            let _ = node.replace_with_with_node_1(&canvas).expect(
                                                &format!("Could not replace #{}", node.id()),
                                            );
                                        }
                                        None => {
                                            let _ = body
                                                .append_child(&canvas)
                                                .expect("Append canvas to HTML body");
                                        }
                                    };
                                }

                                let window = Arc::new(window);

                                // Create the AccessKit adapter while the window
                                // is still invisible (AccessKit panics if the
                                // window is already shown). No-op unless the
                                // `a11y` feature is on AND a tree builder was
                                // registered.
                                #[cfg(feature = "a11y")]
                                let accessibility = accessibility::WindowAccessibility::new(
                                    event_loop,
                                    &window,
                                    self.a11y_waker.clone(),
                                );

                                self.process_event(
                                    event_loop,
                                    Event::WindowCreated {
                                        id,
                                        window,
                                        exit_on_close_request,
                                        clears_transparent,
                                        make_visible: visible,
                                        on_open,
                                        modal_parent,
                                        #[cfg(feature = "a11y")]
                                        accessibility,
                                    },
                                );
                            }
                            Control::Exit => {
                                self.process_event(event_loop, Event::Exit);
                                event_loop.exit();
                                break;
                            }
                            Control::Crash(error) => {
                                self.error = Some(error);
                                event_loop.exit();
                            }
                            Control::CreatePopup(_request) => {
                                #[cfg(all(target_os = "linux", feature = "wayland"))]
                                {
                                    use winit::platform::wayland::WindowAttributesExtWayland as _;

                                    let request = _request;
                                    let attributes = winit::window::Window::default_attributes()
                                        .with_inner_size(winit::dpi::LogicalSize::new(
                                            f64::from(request.area.width.round().max(1.0)),
                                            f64::from(request.area.height.round().max(1.0)),
                                        ))
                                        .with_decorations(false)
                                        .with_transparent(true)
                                        .with_popup(overflow::popup_attributes(
                                            request.parent_raw,
                                            request.area,
                                            request.anchor,
                                            request.grab,
                                        ));
                                    match event_loop.create_window(attributes) {
                                        Ok(window) => self.process_event(
                                            event_loop,
                                            Event::PopupCreated {
                                                parent: request.parent,
                                                area: request.area,
                                                window: Arc::new(window),
                                            },
                                        ),
                                        Err(error) => {
                                            log::warn!("Overlay popup not created: {error}");
                                        }
                                    }
                                }
                            }
                            Control::SetCustomCursor(window, cursor) => {
                                let made = self.custom_cursors.entry(cursor).or_insert_with(|| {
                                    let image = cursor.image();
                                    match winit::window::CustomCursor::from_rgba(
                                        image.rgba.to_vec(),
                                        image.width,
                                        image.height,
                                        image.hotspot.0,
                                        image.hotspot.1,
                                    ) {
                                        Ok(source) => Some(event_loop.create_custom_cursor(source)),
                                        Err(error) => {
                                            log::warn!("custom cursor refused: {error}");
                                            None
                                        }
                                    }
                                });
                                if let Some(made) = made {
                                    window.set_cursor(made.clone());
                                }
                            }
                            Control::ListMonitors(channel) => {
                                let monitors = event_loop
                                    .available_monitors()
                                    .map(|monitor| conversion::monitor(&monitor))
                                    .collect();
                                let _ = channel.send(monitors);
                            }
                            Control::SetAutomaticWindowTabbing(_enabled) => {
                                #[cfg(target_os = "macos")]
                                {
                                    use winit::platform::macos::ActiveEventLoopExtMacOS;
                                    event_loop.set_allows_automatic_window_tabbing(_enabled);
                                }
                            }
                        },
                        _ => {
                            break;
                        }
                    },
                    task::Poll::Ready(_) => {
                        event_loop.exit();
                        break;
                    }
                };
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut runner = runner;
        let _ = event_loop.run_app(&mut runner);

        runner.error.map(Err).unwrap_or(Ok(()))
    }

    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        let _ = event_loop.spawn_app(runner);

        Ok(())
    }
}

#[derive(Debug)]
enum Event<Message: 'static> {
    WindowCreated {
        id: window::Id,
        window: Arc<winit::window::Window>,
        exit_on_close_request: bool,
        make_visible: bool,
        /// A transparent window whose frame the application draws clears its
        /// frames to transparent (enzICED).
        clears_transparent: bool,
        on_open: oneshot::Sender<window::Id>,
        /// The window this modal dialog blocks (enzICED).
        modal_parent: Option<window::Id>,
        /// AccessKit adapter, created in the `Runner` while the window is still
        /// invisible (AccessKit requires that). `None` when the `a11y` feature
        /// is off or no tree builder is registered.
        #[cfg(feature = "a11y")]
        accessibility: Option<accessibility::WindowAccessibility>,
    },
    EventLoopAwakened(winit::event::Event<Message>),
    /// A popup created for `parent`'s overlays over `area` (enzICED).
    PopupCreated {
        parent: window::Id,
        area: core::Rectangle,
        window: Arc<winit::window::Window>,
    },
    Exit,
}

#[derive(Debug)]
enum Control {
    ChangeFlow(winit::event_loop::ControlFlow),
    Exit,
    Crash(Error),
    CreateWindow {
        id: window::Id,
        settings: window::Settings,
        activation_token: Option<core::window::ActivationToken>,
        title: String,
        monitor: Option<winit::monitor::MonitorHandle>,
        on_open: oneshot::Sender<window::Id>,
        scale_factor: f32,
        /// The open parent window of a dialog, as winit knows it (enzICED).
        parent: Option<(window::Id, winit::window::WindowId)>,
    },
    SetAutomaticWindowTabbing(bool),
    /// Show an image cursor on a window; only the event loop can make one
    /// (enzICED).
    SetCustomCursor(Arc<winit::window::Window>, mouse::CustomCursor),
    /// List the monitors; only the event loop knows them without a window
    /// (enzICED).
    ListMonitors(oneshot::Sender<Vec<core::window::Monitor>>),
    /// Create a popup surface for a window's overflowing overlays (enzICED).
    CreatePopup(overflow::Request),
}

fn route_window_created_theme(
    system_theme: &mut theme::Mode,
    has_observed_window: &mut bool,
    window_theme: Option<winit::window::Theme>,
) -> theme::Mode {
    if !*has_observed_window {
        *has_observed_window = true;

        if *system_theme == theme::Mode::None
            && let Some(mode) = window_theme.map(conversion::theme_mode)
        {
            *system_theme = mode;
        }
    }

    *system_theme
}

fn broadcast_system_theme_change(
    system_theme: &mut theme::Mode,
    mode: theme::Mode,
    mut broadcast: impl FnMut(theme::Mode),
) {
    if mode != *system_theme {
        *system_theme = mode;
        broadcast(mode);
    }
}

trait SystemThemeWindow {
    fn update_theme(&mut self, theme: winit::window::Theme);
    fn invalidate_theme_scene(&mut self);
}

impl<T: SystemThemeWindow + ?Sized> SystemThemeWindow for &mut T {
    fn update_theme(&mut self, theme: winit::window::Theme) {
        (**self).update_theme(theme);
    }

    fn invalidate_theme_scene(&mut self) {
        (**self).invalidate_theme_scene();
    }
}

struct LiveSystemThemeWindow<'a, P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    program: &'a program::Instance<P>,
    window: &'a mut window::Window<P, C>,
}

impl<P, C> SystemThemeWindow for LiveSystemThemeWindow<'_, P, C>
where
    P: Program,
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    fn update_theme(&mut self, theme: winit::window::Theme) {
        self.window.state.update(
            self.program,
            &self.window.raw,
            &winit::event::WindowEvent::ThemeChanged(theme),
        );
    }

    fn invalidate_theme_scene(&mut self) {
        // Every colour in the committed scene was recorded under the previous theme (#2219).
        self.window.invalidate_scene();
    }
}

fn route_native_theme_changed(
    system_theme: &mut theme::Mode,
    theme: winit::window::Theme,
    mut target: impl SystemThemeWindow,
    broadcast: impl FnMut(theme::Mode),
) {
    broadcast_system_theme_change(system_theme, conversion::theme_mode(theme), broadcast);
    target.update_theme(theme);
}

fn route_watcher_theme_changed<T: SystemThemeWindow>(
    system_theme: &mut theme::Mode,
    mode: theme::Mode,
    windows: impl IntoIterator<Item = T>,
    broadcast: impl FnMut(theme::Mode),
) {
    broadcast_system_theme_change(system_theme, mode, broadcast);

    let Some(theme) = conversion::window_theme(mode) else {
        return;
    };

    for mut window in windows {
        window.update_theme(theme);
        window.invalidate_theme_scene();
    }
}

mod activation_policy;

use activation_policy::{
    activate_surface_once, activation_platform, activation_request_expired,
    activation_request_matches,
};

type ActivationReply =
    oneshot::Sender<Result<core::window::ActivationToken, core::window::ActivationError>>;
struct PendingActivation {
    window: core::window::Id,
    serial: winit::event_loop::AsyncRequestSerial,
    platform: core::window::ActivationPlatform,
    reply: ActivationReply,
    expires_at: Instant,
}

const MAX_PENDING_ACTIVATIONS: usize = 32;
const ACTIVATION_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

fn complete_activation_request(
    pending: &mut Vec<PendingActivation>,
    window: core::window::Id,
    serial: winit::event_loop::AsyncRequestSerial,
    token: winit::window::ActivationToken,
    now: Instant,
) -> bool {
    let Some(index) = pending.iter().position(|candidate| {
        activation_request_matches(candidate.window, &candidate.serial, window, &serial)
    }) else {
        return false;
    };

    let pending = pending.swap_remove(index);
    if activation_request_expired(pending.expires_at, now) {
        let _ = pending
            .reply
            .send(Err(core::window::ActivationError::Expired));
    } else {
        let _ = pending
            .reply
            .send(Ok(core::window::ActivationToken::for_platform(
                pending.platform,
                token.into_raw(),
            )));
    }
    true
}

fn fail_window_activation_requests(
    pending: &mut Vec<PendingActivation>,
    window: core::window::Id,
) -> Vec<winit::event_loop::AsyncRequestSerial> {
    let mut canceled = Vec::new();
    let mut index = 0;
    while index < pending.len() {
        if pending[index].window == window {
            let pending = pending.swap_remove(index);
            canceled.push(pending.serial);
            let _ = pending
                .reply
                .send(Err(core::window::ActivationError::WindowClosed));
        } else {
            index += 1;
        }
    }
    canceled
}

fn expire_activation_requests(
    pending: &mut Vec<PendingActivation>,
    now: Instant,
) -> (
    Option<Instant>,
    Vec<(core::window::Id, winit::event_loop::AsyncRequestSerial)>,
) {
    let mut canceled = Vec::new();
    let mut index = 0;
    while index < pending.len() {
        if pending[index].reply.is_canceled() {
            let pending = pending.swap_remove(index);
            canceled.push((pending.window, pending.serial));
        } else if activation_request_expired(pending[index].expires_at, now) {
            let pending = pending.swap_remove(index);
            canceled.push((pending.window, pending.serial));
            let _ = pending
                .reply
                .send(Err(core::window::ActivationError::Expired));
        } else {
            index += 1;
        }
    }
    (
        pending.iter().map(|pending| pending.expires_at).min(),
        canceled,
    )
}

async fn run_instance<P>(
    mut program: program::Instance<P>,
    mut runtime: Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    mut proxy: Proxy<P::Message>,
    mut event_receiver: mpsc::UnboundedReceiver<Event<Action<P::Message>>>,
    mut control_sender: mpsc::UnboundedSender<Control>,
    display_handle: winit::event_loop::OwnedDisplayHandle,
    is_daemon: bool,
    backend_settings: backend::Settings,
    mut renderer_settings: renderer::Settings,
    default_fonts: Vec<Cow<'static, [u8]>>,
    mut _system_theme: oneshot::Receiver<theme::Mode>,
    mut _system_reduced_motion: oneshot::Receiver<bool>,
) where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    use winit::event;
    use winit::event_loop::ControlFlow;

    let mut window_manager = window::Manager::new();
    let mut pending_activations = Vec::new();
    let mut has_observed_window_theme = false;
    let mut is_window_opening = !is_daemon;

    let mut compositor = None;
    // When a replacement for a lost graphics device may next be attempted.
    #[cfg(not(target_arch = "wasm32"))]
    let mut device_retry_at: Option<Instant> = None;
    let mut events = Vec::new();
    // Each window's cursor before its first queued event, so a batch is
    // dispatched with the cursor each event saw (enzICED).
    let mut batch_cursor_origins: FxHashMap<window::Id, mouse::Cursor> = FxHashMap::default();
    let mut messages = shell::Bus::new();
    let mut actions = 0;

    let mut ui_caches = FxHashMap::default();
    let mut user_interfaces = ManuallyDrop::new(FxHashMap::default());
    let mut popups = overflow::Overflow::default();
    // Modal dialog windows and the window each one blocks (enzICED).
    let mut modal_parents: FxHashMap<window::Id, window::Id> = FxHashMap::default();
    // Touchpad scroll momentum per window (enzICED).
    let mut momentum: FxHashMap<window::Id, kinetic::Kinetic> = FxHashMap::default();
    let mut clipboard = Clipboard::connected(&display_handle);
    let mut fair = Fairness::new();

    #[cfg(all(feature = "linux-theme-detection", target_os = "linux"))]
    let (mut system_theme, mut system_reduced_motion) = {
        let to_mode = |color_scheme| match color_scheme {
            mundy::ColorScheme::NoPreference => theme::Mode::None,
            mundy::ColorScheme::Light => theme::Mode::Light,
            mundy::ColorScheme::Dark => theme::Mode::Dark,
        };
        let to_reduced_motion = |reduced_motion: mundy::ReducedMotion| reduced_motion.is_reduce();

        // A single preference stream carries both interests. Each live update
        // fans out into a `NotifyTheme` and a `NotifyReducedMotion` action;
        // the receiving handlers only broadcast when the value actually
        // changes, so the redundant emission is cheap.
        runtime.run(
            mundy::Preferences::stream(
                mundy::Interest::ColorScheme | mundy::Interest::ReducedMotion,
            )
            .flat_map(move |preferences| {
                crate::futures::futures::stream::iter([
                    Action::System(system::Action::NotifyTheme(to_mode(
                        preferences.color_scheme,
                    ))),
                    Action::System(system::Action::NotifyReducedMotion(to_reduced_motion(
                        preferences.reduced_motion,
                    ))),
                ])
            })
            .boxed(),
        );

        let snapshot = runtime.enter(|| {
            mundy::Preferences::once_blocking(
                mundy::Interest::ColorScheme | mundy::Interest::ReducedMotion,
                core::time::Duration::from_millis(200),
            )
        });

        let mode = snapshot
            .map(|preferences| to_mode(preferences.color_scheme))
            .unwrap_or_default();
        let reduced_motion = snapshot
            .map(|preferences| to_reduced_motion(preferences.reduced_motion))
            .unwrap_or_default();

        (mode, reduced_motion)
    };

    #[cfg(not(all(feature = "linux-theme-detection", target_os = "linux")))]
    let mut system_theme = _system_theme.try_recv().ok().flatten().unwrap_or_default();
    #[cfg(not(all(feature = "linux-theme-detection", target_os = "linux")))]
    let mut system_reduced_motion = _system_reduced_motion
        .try_recv()
        .ok()
        .flatten()
        .unwrap_or_default();

    log::info!("System theme: {system_theme:?}");
    log::info!("System reduced motion: {system_reduced_motion:?}");

    'next_event: loop {
        // Empty the queue if possible
        let event = if let Ok(event) = event_receiver.try_recv() {
            Some(event)
        } else {
            event_receiver.next().await
        };

        let Some(event) = event else {
            break;
        };

        match event {
            Event::PopupCreated {
                parent,
                area,
                window,
            } => {
                let Some(current_compositor) = compositor.as_mut() else {
                    continue;
                };
                let to_winit = window_manager
                    .get_mut(parent)
                    .map(|parent| parent.state.viewport().scale().application);
                if let Some(to_winit) = to_winit
                    && popups.adopt(
                        parent,
                        area,
                        to_winit,
                        window,
                        current_compositor,
                        renderer_settings,
                    )
                    && let Some(parent) = window_manager.get_mut(parent)
                {
                    parent.request_full_redraw();
                }
            }
            Event::WindowCreated {
                id,
                window,
                exit_on_close_request,
                clears_transparent,
                make_visible,
                on_open,
                modal_parent,
                #[cfg(feature = "a11y")]
                accessibility,
            } => {
                if compositor.is_none() {
                    let (compositor_sender, compositor_receiver) = oneshot::channel();

                    let create_compositor = {
                        let window = window.clone();
                        let backend_settings = backend_settings.clone();
                        let display_handle = display_handle.clone();
                        let proxy = proxy.clone();
                        let default_fonts = default_fonts.clone();

                        async move {
                            let shell = Shell::new(proxy.clone());

                            let mut compositor =
                                <P::Renderer as compositor::Default>::Compositor::new(
                                    backend_settings,
                                    display_handle,
                                    window,
                                    shell,
                                )
                                .await;

                            if let Ok(compositor) = &mut compositor {
                                for font in default_fonts {
                                    if let Err(error) = compositor.load_font(font.clone()) {
                                        log::warn!("Default font not loaded: {error}");
                                    }
                                }
                            }

                            compositor_sender
                                .send(compositor)
                                .ok()
                                .expect("Send compositor");

                            // HACK! Send a proxy event on completion to trigger
                            // a runtime re-poll
                            // TODO: Send compositor through proxy (?)
                            {
                                let (sender, _receiver) = oneshot::channel();

                                proxy.send_action(Action::Window(
                                    runtime::window::Action::GetLatest(sender),
                                ));
                            }
                        }
                    };

                    #[cfg(target_arch = "wasm32")]
                    wasm_bindgen_futures::spawn_local(create_compositor);

                    #[cfg(not(target_arch = "wasm32"))]
                    runtime.block_on(create_compositor);

                    match compositor_receiver.await.expect("Wait for compositor") {
                        Ok(new_compositor) => {
                            crate::presentation::trace_information(&new_compositor.information());
                            compositor = Some(new_compositor);
                        }
                        Err(error) => {
                            let _ = control_sender.start_send(Control::Crash(error.into()));
                            continue;
                        }
                    }
                }

                if let Some(parent) = modal_parent {
                    let _ = modal_parents.insert(id, parent);
                }

                let is_first = window_manager.is_empty();
                let window_system_theme = route_window_created_theme(
                    &mut system_theme,
                    &mut has_observed_window_theme,
                    window.theme(),
                );

                let window = window_manager.insert(
                    id,
                    window,
                    &display_handle,
                    &program,
                    compositor.as_mut().expect("Compositor must be initialized"),
                    proxy.clone(),
                    renderer_settings,
                    exit_on_close_request,
                    window_system_theme,
                );
                window.state.set_clears_transparent(clears_transparent);

                #[cfg(feature = "a11y")]
                {
                    if let Some(a11y) = &accessibility {
                        window
                            .presentation
                            .sink()
                            .set_geometry_waker(a11y.geometry_waker());
                    }
                    window.accessibility = accessibility;
                }

                window
                    .raw
                    .set_theme(conversion::window_theme(window.state.theme_mode()));

                debug::theme_changed(|| {
                    if is_first {
                        theme::Base::seed(window.state.theme())
                    } else {
                        None
                    }
                });

                let logical_size = window.state.logical_size();

                window.renderer.hint(window.state.scale());

                let _ = user_interfaces.insert(
                    id,
                    build_user_interface(
                        &program,
                        user_interface::Cache::default(),
                        &mut window.renderer,
                        logical_size,
                        id,
                    ),
                );
                // A live interface and a parked cache are exclusive: this
                // window is live, so it must not also sit in `ui_caches`.
                let _ = ui_caches.remove(&id);

                if make_visible {
                    window.raw.set_visible(true);
                }

                events.push((
                    id,
                    core::Event::Window(window::Event::Opened {
                        position: window.position(),
                        size: window.state.logical_size(),
                        scale_factor: window.raw.scale_factor() as f32,
                    }),
                ));

                let _ = on_open.send(id);
                is_window_opening = false;
            }
            Event::EventLoopAwakened(event) => {
                match event {
                    event::Event::NewEvents(event::StartCause::Init) => {
                        for (_id, window) in window_manager.iter_mut() {
                            window.request_full_redraw();
                        }
                    }
                    event::Event::NewEvents(event::StartCause::ResumeTimeReached { .. }) => {
                        let now = Instant::now();

                        for (_id, window) in window_manager.iter_mut() {
                            if let Some(redraw_at) = window.redraw_at
                                && redraw_at <= now
                            {
                                window.request_full_redraw();
                                window.redraw_at = None;
                            }
                        }

                        if let Some(redraw_at) = window_manager.redraw_at() {
                            let _ = control_sender
                                .start_send(Control::ChangeFlow(ControlFlow::WaitUntil(redraw_at)));
                        } else {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(ControlFlow::Wait));
                        }
                    }
                    event::Event::PlatformSpecific(event::PlatformSpecific::MacOS(
                        event::MacOS::ReceivedUrl(url),
                    )) => {
                        runtime.broadcast(subscription::Event::PlatformSpecific(
                            subscription::PlatformSpecific::MacOS(
                                subscription::MacOS::ReceivedUrl(url),
                            ),
                        ));
                    }
                    event::Event::UserEvent(action) => {
                        run_action(
                            action,
                            &program,
                            &proxy,
                            &mut runtime,
                            &mut compositor,
                            &mut events,
                            &mut messages,
                            &mut clipboard,
                            &mut control_sender,
                            &mut user_interfaces,
                            &mut window_manager,
                            &mut ui_caches,
                            &mut is_window_opening,
                            &mut system_theme,
                            &mut system_reduced_motion,
                            &mut renderer_settings,
                            &mut pending_activations,
                        );
                        actions += 1;
                    }
                    event::Event::WindowEvent {
                        window_id: id,
                        event: event::WindowEvent::RedrawRequested,
                        ..
                    } => {
                        // A popup paints with its window's frame.
                        if let Some((parent, _)) = popups.route(id) {
                            if let Some(parent) = window_manager.get_mut(parent) {
                                parent.raw.request_redraw();
                            }
                            continue;
                        }

                        #[cfg(not(target_arch = "wasm32"))]
                        if compositor.as_ref().is_some_and(Compositor::device_lost) {
                            rebuild_lost_device(
                                &mut compositor,
                                &mut window_manager,
                                &backend_settings,
                                renderer_settings,
                                &mut runtime,
                                &proxy,
                                &default_fonts,
                                &mut device_retry_at,
                            );
                        }

                        let Some(mut current_compositor) = compositor.as_mut() else {
                            continue;
                        };

                        let Some((id, mut window)) = window_manager.get_mut_alias(id) else {
                            continue;
                        };

                        // A redraw is one turn of the UI thread: every drain it
                        // performs shares one cooperative budget.
                        fair.begin_turn();

                        let physical_size = window.state.physical_size();
                        let mut logical_size = window.state.logical_size();

                        #[cfg(feature = "performance-observer")]
                        runtime::performance::frame_requested(performance_window(id));

                        if physical_size.width == 0 || physical_size.height == 0 {
                            #[cfg(feature = "performance-observer")]
                            runtime::performance::frame_skipped(performance_window(id));
                            continue;
                        }

                        // The window may have been left parked by a scoped
                        // update; it needs a live interface to be drawn.
                        revive_user_interface(
                            &mut user_interfaces,
                            &mut ui_caches,
                            &program,
                            window,
                            id,
                        );

                        // enzUI-ICED #3442: whether only content redraws asked
                        // for this frame; checked again once the redraw pass ran.
                        let content_requested = window.take_content_frame();

                        // Window was resized between redraws
                        let resized = window.surface_version != window.state.surface_version();
                        if resized {
                            window.renderer.hint(window.state.scale());

                            let ui = user_interfaces.remove(&id).expect("Remove user interface");

                            let layout_span = debug::layout(id);
                            #[cfg(not(feature = "performance-observer"))]
                            let ui = ui.relayout(logical_size, &mut window.renderer);
                            #[cfg(feature = "performance-observer")]
                            let ui = ui.relayout_observed(
                                logical_size,
                                &mut window.renderer,
                                performance_window(id),
                            );
                            let _ = user_interfaces.insert(id, ui);
                            layout_span.finish();
                            invalidation::instrumentation::layout(id);

                            // A resized surface means every recorded coordinate
                            // is at the wrong scale or the wrong place. No track
                            // describes that, so retention fails closed, and
                            // the lane hands the surface back before anyone
                            // reconfigures it (#2219).
                            window.invalidate_scene();

                            if let Some(surface) = window.surface_mut() {
                                current_compositor.configure_surface(
                                    surface,
                                    physical_size.width,
                                    physical_size.height,
                                );
                            } else if !window.presentation_quarantined() {
                                let surface = current_compositor.create_surface(
                                    window.raw.clone(),
                                    physical_size.width,
                                    physical_size.height,
                                );
                                window.set_surface(surface);
                            }

                            window.surface_version = window.state.surface_version();
                        }

                        window.revoke_presented_input();
                        window.begin_native_update(id);
                        // enzUI-ICED #3082: the instant the frame is predicted
                        // to be shown, not the instant the loop woke.
                        let redraw_at = window.redraw_instant(Instant::now());
                        let redraw_event =
                            core::Event::Window(window::Event::RedrawRequested(redraw_at));

                        let cursor = window.state.cursor();

                        let mut interface =
                            user_interfaces.get_mut(&id).expect("Get user interface");

                        let interact_span = debug::interact(id);
                        // enzICED #2219: `Update` is the redraw pass's own
                        // message/update work, timed separately from `Input`
                        // (event dispatch) so the two can never be reported as
                        // one span.
                        let update_started = Instant::now();
                        let mut redraw_count = 0;
                        let mut rebuilt = false;

                        let state = loop {
                            let message_count = messages.len();
                            let mut window_requests = Vec::new();
                            let frame_program = layout_ownership::frame_program(
                                window.is_lane_running(),
                                window.scene.as_ref().map(|(recording, _)| recording),
                            );
                            let (state, _) = interface.update_with_layout_owner(
                                &window.raw,
                                &window.waker,
                                slice::from_ref(&redraw_event),
                                cursor,
                                &mut window.renderer,
                                &mut messages,
                                native_window_state(&window.raw),
                                &mut window_requests,
                                frame_program.as_ref(),
                            );
                            dispatch_widget_window_requests(id, &window.raw, window_requests);

                            if redraw_count >= 2 {
                                log::warn!(
                                    "3 consecutive RedrawRequested events produced invalidation"
                                );

                                break state;
                            }

                            if message_count == messages.len() {
                                match state {
                                    user_interface::State::Outdated => {}
                                    user_interface::State::Updated { change, .. } => match change {
                                        user_interface::Change::None => break state,
                                        user_interface::Change::Overlay => {
                                            redraw_count += 1;
                                            continue;
                                        }
                                        user_interface::Change::Layout => {}
                                    },
                                }
                            }

                            redraw_count += 1;

                            if !messages.is_empty()
                                || matches!(state, user_interface::State::Outdated)
                            {
                                rebuilt = true;
                                let mut caches: FxHashMap<_, _> =
                                    ManuallyDrop::into_inner(user_interfaces)
                                        .into_iter()
                                        .map(|(id, interface)| (id, interface.into_cache()))
                                        .collect();

                                caches.extend(ui_caches.drain());

                                let actions =
                                    update(&mut program, &mut runtime, &mut messages, &mut fair);

                                // We are redrawing this window right now, so it
                                // is dirty no matter what the update declared.
                                let mut plan =
                                    invalidation::Plan::from_actions(&actions).verified();
                                plan.dirty(id);

                                let (interfaces, parked) = build_user_interfaces_scoped(
                                    &program,
                                    &mut window_manager,
                                    caches,
                                    &mut proxy,
                                    &plan,
                                );

                                user_interfaces = ManuallyDrop::new(interfaces);
                                ui_caches = parked;

                                let mut deferring = false;

                                // Invalidation reclaims the lane's surface, so only compositor
                                // replacement may trigger it.
                                let compositor_before = compositor.as_ref().map(std::ptr::from_ref);

                                for action in actions {
                                    // Defer all window actions to avoid compositor
                                    // race conditions while redrawing; and once one
                                    // action of a chain has been deferred, defer the
                                    // rest of the chain too, so that
                                    // `batch(closes).chain(exit())` cannot exit before
                                    // the windows it was asked to close.
                                    if fairness::defer_in_order(
                                        &mut deferring,
                                        matches!(action, Action::Window(_)),
                                    ) {
                                        proxy.send_action(action);
                                        continue;
                                    }

                                    run_action(
                                        action,
                                        &program,
                                        &proxy,
                                        &mut runtime,
                                        &mut compositor,
                                        &mut events,
                                        &mut messages,
                                        &mut clipboard,
                                        &mut control_sender,
                                        &mut user_interfaces,
                                        &mut window_manager,
                                        &mut ui_caches,
                                        &mut is_window_opening,
                                        &mut system_theme,
                                        &mut system_reduced_motion,
                                        &mut renderer_settings,
                                        &mut pending_activations,
                                    );
                                }

                                for (window_id, window) in window_manager.iter_mut() {
                                    // We are already redrawing this window
                                    if window_id == id {
                                        continue;
                                    }

                                    // A sibling the update did not dirty keeps
                                    // its pixels; waking it here is the very
                                    // cost scoped invalidation removes.
                                    if !plan.is_dirty(window_id) {
                                        continue;
                                    }

                                    invalidation::instrumentation::redraw_request(window_id);
                                    window.request_full_redraw();
                                }

                                let Some(next_compositor) = compositor.as_mut() else {
                                    continue 'next_event;
                                };

                                current_compositor = next_compositor;
                                window = window_manager.get_mut(id).unwrap();

                                // Window scale factor changed during a redraw request
                                let mut geometry_changed = false;
                                if logical_size != window.state.logical_size() {
                                    geometry_changed = true;
                                    logical_size = window.state.logical_size();

                                    log::debug!(
                                        "Window scale factor changed during a redraw request"
                                    );

                                    let ui =
                                        user_interfaces.remove(&id).expect("Remove user interface");

                                    let layout_span = debug::layout(id);
                                    #[cfg(not(feature = "performance-observer"))]
                                    let ui = ui.relayout(logical_size, &mut window.renderer);
                                    #[cfg(feature = "performance-observer")]
                                    let ui = ui.relayout_observed(
                                        logical_size,
                                        &mut window.renderer,
                                        performance_window(id),
                                    );
                                    let _ = user_interfaces.insert(id, ui);
                                    layout_span.finish();
                                    invalidation::instrumentation::layout(id);
                                }

                                // A scene recorded by the previous renderer
                                // names resources the new one does not have, and
                                // a scene recorded at the previous size is the
                                // wrong picture -- so both cases must fail
                                // closed (#2219).
                                //
                                // Neither is true for an ordinary message, and
                                // invalidating here unconditionally reclaimed
                                // the surface from the lane on every such frame.
                                let compositor_replaced =
                                    Some(std::ptr::from_ref(&*current_compositor))
                                        != compositor_before;

                                if compositor_replaced || geometry_changed {
                                    window.invalidate_scene();
                                }

                                interface = user_interfaces.get_mut(&id).unwrap();
                            }
                        };
                        interact_span.finish();

                        window.record_stage(
                            crate::graphics::presentation::FrameStage::Update,
                            update_started,
                            Instant::now(),
                        );

                        let draw_span = debug::draw(id);
                        #[cfg(feature = "performance-observer")]
                        let performance_draw = runtime::performance::begin_stage(
                            performance_window(id),
                            runtime::performance::Stage::DrawPrepare,
                        );
                        // enzICED #2219: an animation-only frame is served by
                        // replaying the last committed scene with the tracks
                        // the effects published, so no widget is entered. Every
                        // condition below must hold, and any one of them
                        // failing falls back to a full recorded draw -- the
                        // retained path fails CLOSED.
                        let style = renderer::Style {
                            text_color: window.state.text_color(),
                        };

                        // enzICED: overlays past the window edge go to a popup.
                        let wanted = if core::overlay::popup_reach().is_some() {
                            interface.overlay_placement(&window.renderer).and_then(
                                |(area, anchor, grab)| {
                                    let area = overflow::wanted_area(
                                        Some(area),
                                        window.state.logical_size(),
                                    )?;
                                    Some(overflow::Placement { area, anchor, grab })
                                },
                            )
                        } else {
                            None
                        };
                        let (popup_request, overlay_detached) = popups.reconcile(
                            id,
                            &window.raw,
                            wanted,
                            window.state.viewport().scale().application,
                        );
                        if let Some(request) = popup_request {
                            control_sender
                                .start_send(Control::CreatePopup(request))
                                .expect("Send control action");
                        }
                        interface.set_overlay_detached(overlay_detached);

                        window.note_repainting(matches!(
                            &state,
                            user_interface::State::Updated {
                                is_repainting: true,
                                ..
                            }
                        ));
                        let scene_size = window.state.logical_size();
                        let (animation_only, tracks, animations, requires_layout_ownership) =
                            match &state {
                                user_interface::State::Updated {
                                    is_animation_only,
                                    tracks,
                                    animations,
                                    requires_layout_ownership,
                                    ..
                                } => (
                                    *is_animation_only && !tracks.is_empty(),
                                    tracks.clone(),
                                    animations.clone(),
                                    *requires_layout_ownership,
                                ),
                                user_interface::State::Outdated => {
                                    (false, Vec::new(), Vec::new(), false)
                                }
                            };

                        // enzICED #2219, the render lane: when the lane already
                        // owns the surface and this frame changes nothing but
                        // track values, the UI thread neither draws nor
                        // presents. It only tells the lane what the effect now
                        // wants, and the lane keeps sampling and presenting on
                        // its own clock -- which is why a blocked UI thread
                        // does not stop the animation.
                        let now = Instant::now();
                        let live_animation = animations
                            .iter()
                            .any(|animation| animation.is_usable() && !animation.is_finished(now));
                        let lane_frame = graphics::lane::plan(graphics::lane::Conditions {
                            lane_available: window.has_lane(),
                            lane_running: window.is_lane_running(),
                            animation_only,
                            scene_replayable: window.replayable_scene(scene_size).is_some_and(
                                |scene| {
                                    layout_ownership::eligible(scene, requires_layout_ownership)
                                },
                            ),
                            live_animation,
                        }) == graphics::lane::Plan::LaneAnimates;

                        if lane_frame {
                            // Through the window, not `window.lane`: the call
                            // also records the `(node, generation)` set this
                            // travel is made of, which is what stops a callback
                            // for the frame BEFORE a retarget from anchoring
                            // the travel after it (enzICED #2219).
                            window.animate_lane(tracks.clone(), animations.clone());
                        }

                        // enzUI-ICED #3442: when only primitives' content
                        // changed and the redraw pass changed nothing either,
                        // the last frame is presented again with its
                        // primitives prepared, and no widget draws.
                        let content_frame = content_requested
                            && !resized
                            && !rebuilt
                            && redraw_count == 0
                            && !animation_only
                            && !lane_frame
                            && !window.is_lane_running()
                            && !window.has_preedit()
                            // A redraw pass that asked only for content again (a
                            // frame published while this one was prepared) still
                            // changed nothing a widget draws (enzUI-ICED #3618).
                            && matches!(
                                &state,
                                user_interface::State::Updated {
                                    change: user_interface::Change::None,
                                    redraw_request,
                                    tracks,
                                    is_content_only,
                                    ..
                                } if (*is_content_only
                                    || !matches!(redraw_request, window::RedrawRequest::NextFrame))
                                    && tracks.is_empty()
                            )
                            && core::Renderer::reuse_frame(&mut window.renderer);

                        let replayed = if content_frame || lane_frame {
                            true
                        } else if animation_only {
                            // No lane (software fallback), or the lane is not
                            // running yet: the same-thread retained path still
                            // avoids re-entering every widget, but it cannot
                            // claim isolation and does not.
                            let _ = window.reclaim_surface();

                            match window
                                .replayable_scene(scene_size)
                                .filter(|scene| {
                                    layout_ownership::eligible(scene, requires_layout_ownership)
                                })
                                .cloned()
                            {
                                Some(scene) => {
                                    let replay_span = debug::time("replay");
                                    let replayed = user_interface::replay_retained(
                                        &mut window.renderer,
                                        scene_size,
                                        &scene,
                                        &tracks,
                                    );
                                    replay_span.finish();
                                    if replayed && replay_verify::enabled() {
                                        let replayed = current_compositor.screenshot(
                                            &mut window.renderer,
                                            window.state.viewport(),
                                            window.state.background_color(),
                                        );
                                        interface.draw_for_comparison(
                                            &mut window.renderer,
                                            window.state.theme(),
                                            &style,
                                            cursor,
                                        );
                                        let drawn = current_compositor.screenshot(
                                            &mut window.renderer,
                                            window.state.viewport(),
                                            window.state.background_color(),
                                        );
                                        let size = window.state.physical_size();
                                        replay_verify::compare(
                                            &replayed,
                                            &drawn,
                                            size.width,
                                            size.height,
                                        );
                                    }
                                    replayed
                                }
                                None => false,
                            }
                        } else {
                            false
                        };

                        if !replayed {
                            // Recording is not free: it clones the layer stack.
                            // A window whose effects never publish a track would
                            // pay that on every frame for a scene nobody will
                            // ever replay, so the copy is kept ONLY once this
                            // window has shown it animates through the lane.
                            let tracks_published = !tracks.is_empty();

                            if tracks_published || window.scene.is_some() {
                                // A recorded draw is an ordinary draw that also
                                // keeps a copy: no extra pass, one layer clone.
                                let owned = window
                                    .scene
                                    .as_ref()
                                    .is_some_and(|(scene, _)| scene.docked_tenure().is_some());
                                let running = window.is_lane_running();
                                let recording = layout_ownership::record_replacement(
                                    owned,
                                    running,
                                    &mut (&mut *window, &mut *interface),
                                    |(window, interface)| {
                                        interface
                                            .draw_staged_retained(
                                                &mut window.renderer,
                                                window.state.theme(),
                                                &style,
                                                cursor,
                                            )
                                            .map(Some)
                                    },
                                    |(window, _)| {
                                        let _ = window.reclaim_surface();
                                    },
                                    |(window, interface)| {
                                        interface.draw_retained(
                                            &mut window.renderer,
                                            window.state.theme(),
                                            &style,
                                            cursor,
                                        )
                                    },
                                );
                                window.scene = recording.map(|scene| (scene, scene_size));

                                // enzICED #2219: a newly RECORDED scene is a
                                // new scene generation, and only that. It is
                                // bumped here, by the producer, so the value a
                                // commit carries describes the bytes that
                                // commit actually contained -- a worker that
                                // incremented on receipt could be reordered
                                // and satisfy a join it should have refused.
                                if window.scene.is_some() {
                                    window.scene_generation =
                                        window.scene_generation.saturating_add(1);
                                }
                            } else {
                                interface.draw(
                                    &mut window.renderer,
                                    window.state.theme(),
                                    &style,
                                    cursor,
                                );
                            }
                        }
                        #[cfg(feature = "performance-observer")]
                        drop(performance_draw);
                        #[cfg(feature = "performance-observer")]
                        runtime::performance::frame_drawn(performance_window(id));
                        draw_span.finish();
                        invalidation::instrumentation::draw(id);

                        if overlay_detached {
                            let scale = window.state.viewport().scale();
                            let theme = window.state.theme();
                            let presented = popups.present(
                                id,
                                current_compositor,
                                scale,
                                None,
                                |renderer, area| {
                                    interface.draw_overlay(renderer, theme, &style, cursor, area);
                                },
                            );
                            if presented.is_err() {
                                popups.remove(id);
                                interface.set_overlay_detached(false);
                                window.request_full_redraw();
                            }
                        }

                        // Accessibility: after the tree is laid out and drawn,
                        // collect and publish semantics only when this window's
                        // AccessKit lifecycle gate is active.
                        #[cfg(feature = "a11y")]
                        if let Some(mut a11y) = window.accessibility.take() {
                            let scale_factor = f64::from(window.state.scale_factor());
                            let title = window.state.title().to_owned();
                            a11y.update_ready(id, &title, scale_factor, &mut |operation| {
                                if let Some(operation) = operation {
                                    interface.operate(&window.renderer, operation);
                                    true
                                } else if window.prepare_accessibility_input(id) {
                                    if interface
                                        .reconcile_presented_geometry(&window.renderer)
                                        .is_ok()
                                    {
                                        true
                                    } else {
                                        window.revoke_presented_input();
                                        false
                                    }
                                } else {
                                    false
                                }
                            });
                            window.accessibility = Some(a11y);
                            window.revoke_presented_input();
                        }

                        let redraw_deadline = match &state {
                            user_interface::State::Updated {
                                redraw_deadline, ..
                            } => *redraw_deadline,
                            user_interface::State::Outdated => None,
                        };
                        // A redraw pass that only asked for content redraws (a
                        // frame published while this one was drawn) asks for a
                        // content frame next, not a full one (enzUI-ICED #3618).
                        let content_only_next = matches!(
                            &state,
                            user_interface::State::Updated {
                                is_content_only: true,
                                ..
                            }
                        );
                        let requested_redraw = if let user_interface::State::Updated {
                            redraw_request,
                            input_method,
                            input_method_surrounding,
                            mouse_interaction,
                            clipboard: clipboard_requests,
                            ..
                        } = state
                        {
                            let enabled = input_method.is_enabled();
                            window.request_input_method(input_method);
                            window.request_input_method_surrounding(
                                input_method_surrounding.filter(|_| enabled),
                            );
                            if let Some(cursor) = window.update_mouse(mouse_interaction) {
                                control_sender
                                    .start_send(Control::SetCustomCursor(
                                        window.raw.clone(),
                                        cursor,
                                    ))
                                    .expect("Send control action");
                            }

                            run_clipboard(&mut proxy, &mut clipboard, clipboard_requests, id);
                            Some(redraw_request)
                        } else {
                            None
                        };

                        runtime.broadcast(subscription::Event::Interaction {
                            window: id,
                            event: redraw_event,
                            status: core::event::Status::Ignored,
                        });

                        if !content_frame {
                            window.draw_preedit();
                        }

                        // Owned recordings carry their replay admission before submission;
                        // transform-only recordings retain the UI-presented paint baseline.
                        let plan = graphics::lane::plan(graphics::lane::Conditions {
                            lane_available: window.has_lane(),
                            lane_running: window.is_lane_running(),
                            animation_only,
                            scene_replayable: window.replayable_scene(scene_size).is_some_and(
                                |scene| {
                                    layout_ownership::eligible(scene, requires_layout_ownership)
                                },
                            ),
                            live_animation,
                        });

                        let lane_presents = match plan {
                            graphics::lane::Plan::LaneAnimates => true,
                            graphics::lane::Plan::LaneCommits => {
                                window.commit_scene_to_lane(tracks.clone(), animations.clone())
                            }
                            graphics::lane::Plan::HandOver | graphics::lane::Plan::UiPresents => {
                                false
                            }
                        };

                        let present_span = debug::present(id);
                        #[cfg(feature = "performance-observer")]
                        let performance_submit = runtime::performance::begin_stage(
                            performance_window(id),
                            runtime::performance::Stage::GpuSubmit,
                        );
                        #[cfg(feature = "performance-observer")]
                        let presentation_quality =
                            performance_quality(current_compositor.presentation_quality());
                        if lane_presents {
                            // Motion the lane carries needs no UI frames, but a frame the UI drew for new
                            // content keeps the next frame it asked for, or that content freezes.
                            match (plan, requested_redraw) {
                                (graphics::lane::Plan::LaneCommits, Some(requested))
                                    if content_only_next
                                        && matches!(
                                            requested,
                                            window::RedrawRequest::NextFrame
                                        ) =>
                                {
                                    window.request_content_redraw();
                                }
                                (graphics::lane::Plan::LaneCommits, Some(requested)) => {
                                    window.request_redraw(requested);
                                }
                                _ => window.request_redraw(layout_ownership::owned_frame_request(
                                    redraw_deadline,
                                )),
                            }
                            #[cfg(feature = "performance-observer")]
                            drop(performance_submit);
                            present_span.finish();
                            continue 'next_event;
                        }

                        let handover_scene = window.replayable_scene(scene_size).cloned();
                        let result = layout_ownership::finish_frame(
                            &mut (&mut *window, &mut *current_compositor),
                            requested_redraw,
                            redraw_deadline,
                            |context| {
                                layout_ownership::present_or_handover(
                                    handover_scene.as_ref(),
                                    plan == graphics::lane::Plan::HandOver,
                                    context,
                                    |(window, compositor)| {
                                        window.hand_over_to_lane(
                                            compositor,
                                            tracks.clone(),
                                            animations.clone(),
                                            Instant::now(),
                                            |renderer, animations, delay| {
                                                interface.rebase_docked_clocks(
                                                    renderer, animations, delay,
                                                )
                                            },
                                        )
                                    },
                                    |(window, compositor)| window.present_frame(compositor),
                                )
                            },
                            |(window, _), request| {
                                if content_only_next
                                    && matches!(request, window::RedrawRequest::NextFrame)
                                {
                                    window.request_content_redraw();
                                } else {
                                    window.request_redraw(request);
                                }
                            },
                        );
                        match result {
                            Ok(layout_ownership::PresentationOwner::Lane) => {
                                #[cfg(feature = "performance-observer")]
                                drop(performance_submit);
                                present_span.finish();
                            }
                            Ok(layout_ownership::PresentationOwner::Ui) => {
                                #[cfg(feature = "performance-observer")]
                                drop(performance_submit);
                                #[cfg(feature = "performance-observer")]
                                runtime::performance::milestone(
                                    performance_window(id),
                                    true,
                                    presentation_quality,
                                );
                                present_span.finish();
                            }
                            // The compositor is replaced at the next redraw, no sooner than a failed
                            // replacement allows; a surface recreated on the dead device would fail again.
                            #[cfg(not(target_arch = "wasm32"))]
                            Err(_) if current_compositor.device_lost() => {
                                #[cfg(feature = "performance-observer")]
                                drop(performance_submit);
                                #[cfg(feature = "performance-observer")]
                                runtime::performance::frame_skipped(performance_window(id));
                                present_span.finish();

                                window.invalidate_scene();
                                window.request_redraw(window::RedrawRequest::At(
                                    device_retry_at.unwrap_or_else(Instant::now),
                                ));
                            }
                            Err(error) => match error {
                                compositor::SurfaceError::OutOfMemory => {
                                    // This is an unrecoverable error.
                                    panic!("{error:?}");
                                }
                                compositor::SurfaceError::Outdated
                                | compositor::SurfaceError::Lost
                                | compositor::SurfaceError::Other => {
                                    #[cfg(feature = "performance-observer")]
                                    drop(performance_submit);
                                    #[cfg(feature = "performance-observer")]
                                    runtime::performance::frame_skipped(performance_window(id));
                                    present_span.finish();

                                    // Upstream iced #3478: a generic acquire
                                    // failure is recovered like a lost surface,
                                    // but at most once a second and with no redraw
                                    // in between. It used to request a redraw of
                                    // every window on every frame, which spins
                                    // the loop forever on a surface that keeps
                                    // failing. `Outdated` and `Lost` stay
                                    // immediate: a resize depends on that.
                                    let recover = error != compositor::SurfaceError::Other
                                        || window
                                            .surface_error_at
                                            .is_none_or(|at| at.elapsed() > Duration::from_secs(1));

                                    if recover {
                                        if error == compositor::SurfaceError::Other {
                                            window.surface_error_at = Some(Instant::now());
                                            log::warn!(
                                                "Error {error:?} when presenting surface. Recovering it."
                                            );
                                        }

                                        // Reconfigure surface and try redrawing
                                        let physical_size = window.state.physical_size();

                                        // Device loss or an outdated surface: the
                                        // committed scene references GPU resources
                                        // on the old device or an old surface
                                        // configuration (#2219).
                                        window.invalidate_scene();

                                        if error == compositor::SurfaceError::Outdated {
                                            window.configure_surface(
                                                current_compositor,
                                                physical_size.width,
                                                physical_size.height,
                                            );
                                        } else {
                                            window.recreate_surface(
                                                current_compositor,
                                                physical_size.width,
                                                physical_size.height,
                                            );
                                        }

                                        if !window.presentation_quarantined() {
                                            window.request_full_redraw();
                                        }
                                    }
                                }
                                compositor::SurfaceError::Occluded => {
                                    #[cfg(feature = "performance-observer")]
                                    drop(performance_submit);
                                    #[cfg(feature = "performance-observer")]
                                    runtime::performance::frame_skipped(performance_window(id));
                                    present_span.finish();

                                    // Do nothing and wait for window to become visible again
                                }
                                compositor::SurfaceError::Timeout => {
                                    #[cfg(feature = "performance-observer")]
                                    drop(performance_submit);
                                    #[cfg(feature = "performance-observer")]
                                    runtime::performance::frame_skipped(performance_window(id));
                                    present_span.finish();

                                    // Upstream iced #3478: only the window whose
                                    // acquire timed out tries again.
                                    window.request_full_redraw();
                                }
                            },
                        }
                    }
                    event::Event::WindowEvent {
                        event: window_event,
                        window_id,
                    } => {
                        if !is_daemon
                            && matches!(window_event, winit::event::WindowEvent::Destroyed)
                            && !is_window_opening
                            && window_manager.is_empty()
                        {
                            control_sender
                                .start_send(Control::Exit)
                                .expect("Send control action");

                            continue;
                        }

                        if matches!(window_event, winit::event::WindowEvent::Destroyed) {
                            popups.retain(|id| window_manager.get_mut(id).is_some());
                            modal_parents
                                .retain(|child, _| window_manager.get_mut(*child).is_some());
                            momentum.retain(|id, _| window_manager.get_mut(*id).is_some());
                        }

                        // enzICED: the keyboard moving to the window's own
                        // grabbing popup does not unfocus the window; the
                        // popup's focus stands for the window's below.
                        if matches!(window_event, winit::event::WindowEvent::Focused(false))
                            && popups.route(window_id).is_none()
                            && window_manager
                                .get_mut_alias(window_id)
                                .is_some_and(|(id, _)| popups.has_popup(id))
                        {
                            continue;
                        }

                        // enzICED: a popup's input belongs to its window's overlays.
                        let (window_id, window_event) = match popups.route(window_id) {
                            None => (window_id, window_event),
                            Some((parent, offset)) => match window_event {
                                winit::event::WindowEvent::Resized(size) => {
                                    if let Some(current_compositor) = compositor.as_mut() {
                                        popups.resized(
                                            window_id,
                                            current_compositor,
                                            size.width,
                                            size.height,
                                        );
                                    }
                                    if let Some(parent) = window_manager.get_mut(parent) {
                                        parent.raw.request_redraw();
                                    }
                                    continue;
                                }
                                winit::event::WindowEvent::CloseRequested => {
                                    // Dismissed by the compositor, as for a
                                    // click in another application: the
                                    // overlays get that click, far outside
                                    // them, and close as for one in the window.
                                    popups.dismissed(parent);
                                    if let Some(window) = window_manager.get_mut(parent) {
                                        let far = winit::event::WindowEvent::CursorMoved {
                                            device_id: winit::event::DeviceId::dummy(),
                                            position: winit::dpi::PhysicalPosition::new(
                                                -1.0e6, -1.0e6,
                                            ),
                                        };
                                        window.state.update(&program, &window.raw, &far);
                                        if let Some(moved) = conversion::window_event(
                                            far,
                                            window.state.scale_factor(),
                                            window.state.modifiers(),
                                        ) {
                                            events.push((parent, moved));
                                        }
                                        for click in [
                                            mouse::Event::ButtonPressed(mouse::Button::Left),
                                            mouse::Event::ButtonReleased(mouse::Button::Left),
                                        ] {
                                            events.push((parent, core::Event::Mouse(click)));
                                        }
                                        window.request_full_redraw();
                                    }
                                    continue;
                                }
                                // A grabbing popup holds the keyboard for its
                                // window: its focus is the window's.
                                winit::event::WindowEvent::Focused(focused) => {
                                    let Some(window) = window_manager.get_mut(parent) else {
                                        continue;
                                    };
                                    (window.raw.id(), winit::event::WindowEvent::Focused(focused))
                                }
                                event
                                    if overflow::is_input(&event)
                                        || matches!(
                                            event,
                                            winit::event::WindowEvent::CursorLeft { .. }
                                        ) =>
                                {
                                    let Some(parent) = window_manager.get_mut(parent) else {
                                        continue;
                                    };
                                    let scale = parent.state.scale_factor();
                                    (parent.raw.id(), overflow::translate(event, offset, scale))
                                }
                                _ => continue,
                            },
                        };

                        // enzICED: a window under an open modal dialog takes no
                        // input; a press or key raises the dialog instead.
                        let target = window_manager.get_mut_alias(window_id).map(|(id, _)| id);
                        let dialog = target.and_then(|target| {
                            modal::blocking_dialog(&modal_parents, target, |id| {
                                window_manager.get_mut(id).is_some()
                            })
                        });
                        if let Some(dialog) = dialog
                            && let Some(raise) = modal::blocked_input(&window_event)
                        {
                            if raise && let Some(dialog) = window_manager.get_mut(dialog) {
                                dialog.raw.focus_window();
                            }
                            continue;
                        }

                        let Some((id, window)) = window_manager.get_mut_alias(window_id) else {
                            continue;
                        };

                        // enzICED: a held button keeps the pointer in its
                        // window (see `pointer_hold`).
                        let (window_event, left_after) = match window
                            .pointer_hold
                            .admit(window_event)
                        {
                            pointer_hold::Admit::Hold => continue,
                            pointer_hold::Admit::Pass(event) => (event, None),
                            pointer_hold::Admit::PassThenLeave(event, left) => (event, Some(left)),
                        };

                        if let winit::event::WindowEvent::ActivationTokenDone { serial, token } =
                            window_event
                        {
                            let _ = complete_activation_request(
                                &mut pending_activations,
                                id,
                                serial,
                                token,
                                Instant::now(),
                            );
                            continue;
                        }

                        // Accessibility: let the AccessKit adapter observe the
                        // raw window event first (its contract), then drain any
                        // action requests it queued (handled below the match).
                        #[cfg(feature = "a11y")]
                        if let Some(a11y) = window.accessibility.as_mut() {
                            let raw = window.raw.clone();
                            a11y.process_event(&raw, &window_event);
                            for request in a11y.take_requests(id) {
                                accessibility::dispatch_request(request);
                            }
                        }

                        match window_event {
                            winit::event::WindowEvent::Resized(_) => {
                                window.note_resized(Instant::now());
                                window.request_full_redraw();
                            }
                            winit::event::WindowEvent::Occluded(false) => {
                                window.request_full_redraw();
                            }
                            winit::event::WindowEvent::ThemeChanged(theme) => {
                                route_native_theme_changed(
                                    &mut system_theme,
                                    theme,
                                    LiveSystemThemeWindow {
                                        program: &program,
                                        window,
                                    },
                                    |mode| {
                                        runtime.broadcast(subscription::Event::SystemThemeChanged(
                                            mode,
                                        ));
                                    },
                                );
                            }
                            _ => {}
                        }

                        if matches!(window_event, winit::event::WindowEvent::CloseRequested)
                            && window.exit_on_close_request
                        {
                            run_action(
                                Action::Window(runtime::window::Action::Close(id)),
                                &program,
                                &proxy,
                                &mut runtime,
                                &mut compositor,
                                &mut events,
                                &mut messages,
                                &mut clipboard,
                                &mut control_sender,
                                &mut user_interfaces,
                                &mut window_manager,
                                &mut ui_caches,
                                &mut is_window_opening,
                                &mut system_theme,
                                &mut system_reduced_motion,
                                &mut renderer_settings,
                                &mut pending_activations,
                            );
                        } else {
                            let _ = batch_cursor_origins
                                .entry(id)
                                .or_insert_with(|| window.state.cursor());
                            if !matches!(window_event, winit::event::WindowEvent::ThemeChanged(_)) {
                                window.state.update(&program, &window.raw, &window_event);
                            }

                            // enzICED: touchpad scrolling keeps its momentum.
                            {
                                use winit::event::{
                                    ElementState, MouseScrollDelta, TouchPhase, WindowEvent,
                                };
                                let now = Instant::now();
                                match &window_event {
                                    WindowEvent::MouseWheel {
                                        delta: MouseScrollDelta::PixelDelta(delta),
                                        phase,
                                        ..
                                    } => {
                                        let scale = f64::from(window.state.scale_factor());
                                        let kinetic = momentum.entry(id).or_default();
                                        match phase {
                                            TouchPhase::Started | TouchPhase::Cancelled => {
                                                kinetic.stop();
                                            }
                                            TouchPhase::Moved => kinetic.scroll(
                                                core::Vector::new(
                                                    (delta.x / scale) as f32,
                                                    (delta.y / scale) as f32,
                                                ),
                                                now,
                                            ),
                                            TouchPhase::Ended => {
                                                if !system_reduced_motion
                                                    && !crate::core::preferences::get()
                                                        .reduced_motion
                                                    && kinetic.release(now)
                                                {
                                                    window.raw.request_redraw();
                                                }
                                            }
                                        }
                                    }
                                    WindowEvent::MouseWheel { .. }
                                    | WindowEvent::MouseInput {
                                        state: ElementState::Pressed,
                                        ..
                                    }
                                    | WindowEvent::KeyboardInput { .. }
                                    | WindowEvent::Touch(_)
                                    | WindowEvent::PinchGesture { .. }
                                    | WindowEvent::PanGesture { .. }
                                    | WindowEvent::Focused(false)
                                    | WindowEvent::CursorLeft { .. } => {
                                        if let Some(kinetic) = momentum.get_mut(&id) {
                                            kinetic.stop();
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            if matches!(
                                window_event,
                                winit::event::WindowEvent::DecorationsChanged { .. }
                            ) && let Some(core::Event::Window(
                                core::window::Event::DecorationsChanged(decorations),
                            )) = conversion::window_event(
                                window_event.clone(),
                                window.state.scale_factor(),
                                window.state.modifiers(),
                            ) {
                                core::window::set_decorations(id, decorations);
                            }

                            if let Some(event) = conversion::window_event(
                                window_event,
                                window.state.scale_factor(),
                                window.state.modifiers(),
                            ) {
                                #[cfg(feature = "performance-observer")]
                                runtime::performance::input_arrived(performance_window(id), 1);
                                events.push((id, event));
                            }
                            if let Some(caps_lock) = window.state.take_caps_lock_change() {
                                events.push((
                                    id,
                                    core::Event::Keyboard(core::keyboard::Event::CapsLockChanged(
                                        caps_lock,
                                    )),
                                ));
                            }
                            // The leave held back while a button was.
                            if let Some(left) = left_after {
                                window.state.update(&program, &window.raw, &left);
                                if let Some(event) = conversion::window_event(
                                    left,
                                    window.state.scale_factor(),
                                    window.state.modifiers(),
                                ) {
                                    events.push((id, event));
                                }
                            }
                        }
                    }
                    event::Event::DeviceEvent {
                        event: winit::event::DeviceEvent::MouseMotion { delta },
                        ..
                    } => {
                        // enzICED: relative motion goes to the windows that
                        // hold a pointer grab, and nowhere else.
                        let delta = core::Vector::new(delta.0 as f32, delta.1 as f32);
                        for (id, window) in window_manager.iter_mut() {
                            if window.cursor_grab != core::window::CursorGrab::None {
                                events.push((
                                    id,
                                    core::Event::Mouse(mouse::Event::RelativeMotion { delta }),
                                ));
                            }
                        }
                    }
                    event::Event::AboutToWait => {
                        // enzICED #4: this dispatch is a turn of its own. An
                        // occluded or absent window never redraws, so a
                        // budget spent here would otherwise never be reset
                        // and every later wake would drain nothing.
                        fair.begin_turn();

                        let (activation_deadline, canceled) =
                            expire_activation_requests(&mut pending_activations, Instant::now());
                        #[cfg(target_os = "linux")]
                        {
                            use winit::platform::startup_notify::WindowExtStartupNotify;

                            for (id, serial) in canceled {
                                if let Some(window) = window_manager.get_mut(id) {
                                    let _ = window.raw.cancel_activation_token_request(serial);
                                }
                            }
                        }
                        #[cfg(not(target_os = "linux"))]
                        let _ = canceled;

                        if actions > 0 {
                            proxy.free_slots(actions);
                            actions = 0;
                        }

                        // #309: drain a11y action requests from every window
                        // on *every* AboutToWait, including wakes that carry
                        // no window event.  The per-WindowEvent drain at
                        // line ~1070 is kept (ordering contract: adapter
                        // sees the raw event first), but when the loop wakes
                        // solely from the ActionBridge waker there is no
                        // WindowEvent, so this is the only path that fires.
                        #[cfg(feature = "a11y")]
                        for (id, window) in window_manager.iter_mut() {
                            if let Some(a11y) = window.accessibility.as_mut() {
                                for request in a11y.take_requests(id) {
                                    accessibility::dispatch_request(request);
                                }

                                // Activation handlers may run on a platform
                                // thread. They only set this per-window flag
                                // and wake us; requesting the redraw here keeps
                                // all live-tree work on the event-loop thread.
                                if a11y.take_activation_frame_request() {
                                    window.request_full_redraw();
                                }
                            }
                        }

                        #[cfg(feature = "a11y")]
                        for (id, window) in window_manager.iter_mut() {
                            let requested = window.accessibility.as_mut().is_some_and(
                                accessibility::WindowAccessibility::take_geometry_request,
                            );
                            if !requested || !window.is_lane_running() {
                                continue;
                            }
                            let Some(interface) = user_interfaces.get_mut(&id) else {
                                continue;
                            };
                            let Some(mut a11y) = window.accessibility.take() else {
                                continue;
                            };
                            let title = window.state.title().to_owned();
                            let scale = f64::from(window.state.scale_factor());
                            a11y.update_ready(id, &title, scale, &mut |operation| {
                                if let Some(operation) = operation {
                                    interface.operate(&window.renderer, operation);
                                    true
                                } else {
                                    window.prepare_accessibility_input(id)
                                        && interface
                                            .reconcile_presented_geometry(&window.renderer)
                                            .is_ok()
                                }
                            });
                            window.revoke_presented_input();
                            window.accessibility = Some(a11y);
                        }

                        // enzICED #2219: a lane that stopped on its own -- it
                        // settled, refused a replay, or the surface failed --
                        // wakes this loop through the window waker. Ownership
                        // has to come back here and the window has to be
                        // redrawn from the widget tree, or the animation would
                        // park at whatever the lane last presented. This is one
                        // atomic load per window per wake.
                        for (_id, window) in window_manager.iter_mut() {
                            if !window.is_lane_stopped() {
                                continue;
                            }

                            match window.recover_from_lane() {
                                Some(report) => log::debug!(
                                    "Animation lane returned the surface: {:?} after {} frames",
                                    report.stop,
                                    report.presented
                                ),
                                None => log::warn!(
                                    "Animation lane lost the surface; recreating it on the UI thread"
                                ),
                            }

                            // The UI thread owns the surface again, so it must
                            // present the settled frame and commit final
                            // layout.
                            window.request_full_redraw();
                        }

                        // enzICED: the next step of each window's scroll momentum.
                        {
                            let now = Instant::now();
                            for (id, window) in window_manager.iter_mut() {
                                let Some(kinetic) = momentum.get_mut(&id) else {
                                    continue;
                                };
                                if system_reduced_motion {
                                    kinetic.stop();
                                    continue;
                                }
                                if let Some(step) = kinetic.tick(now) {
                                    events.push((
                                        id,
                                        core::Event::Mouse(mouse::Event::WheelScrolled {
                                            delta: mouse::ScrollDelta::Pixels {
                                                x: step.x,
                                                y: step.y,
                                            },
                                        }),
                                    ));
                                    if kinetic.is_moving() {
                                        window.raw.request_redraw();
                                    }
                                }
                            }
                        }

                        if events.is_empty() && messages.is_empty() && window_manager.is_idle() {
                            // A pending activation produces no event, message or
                            // redraw, so this is the only path that can schedule
                            // its expiry; the flow merge keeps the earlier wake.
                            if let Some(deadline) = activation_deadline {
                                let _ = control_sender.start_send(Control::ChangeFlow(
                                    ControlFlow::WaitUntil(deadline),
                                ));
                            }

                            continue;
                        }

                        let mut stale_uis: Vec<window::Id> = Vec::new();

                        for (id, window) in window_manager.iter_mut() {
                            let interact_span = debug::interact(id);
                            let mut window_events = vec![];

                            events.retain(|(window_id, event)| {
                                if *window_id == id {
                                    window_events.push(event.clone());
                                    false
                                } else {
                                    true
                                }
                            });

                            let cursor_origin = batch_cursor_origins.remove(&id);
                            if window_events.is_empty() {
                                continue;
                            }

                            if window.presentation_quarantined() {
                                for event in window_events {
                                    let status = if matches!(event, core::Event::Window(_)) {
                                        core::event::Status::Ignored
                                    } else {
                                        core::event::Status::Captured
                                    };
                                    runtime.broadcast(subscription::Event::Interaction {
                                        window: id,
                                        event,
                                        status,
                                    });
                                }
                                interact_span.finish();
                                continue;
                            }

                            // An event reached a window a scoped update left
                            // parked: revive it now, once, for this window
                            // only.
                            revive_user_interface(
                                &mut user_interfaces,
                                &mut ui_caches,
                                &program,
                                window,
                                id,
                            );

                            #[cfg(feature = "performance-observer")]
                            runtime::performance::input_start(
                                performance_window(id),
                                window_events.len() as u64,
                            );

                            // enzICED #2219: while the lane owned the cadence,
                            // this thread saw none of the frames the user did.
                            // Hand the widgets the instant the lane last
                            // PRESENTED, ahead of the events of this batch, so
                            // a retarget or reversal starts from the value on
                            // screen instead of from this thread's stale
                            // sample. It is a real frame instant for a frame
                            // that was really presented -- by the render
                            // thread.
                            let focus_directed =
                                user_interface::focus_directed_input(&window_events);
                            let input_ready = if focus_directed {
                                let ui = user_interfaces.get_mut(&id).expect("Get user interface");
                                if !window.prepare_presented_input(id)
                                    || ui.reconcile_presented_layout(&window.renderer).is_err()
                                {
                                    window.revoke_presented_input();
                                    ui.prepare_native_layout(&mut window.renderer);
                                }
                                true
                            } else {
                                prepare_widget_input(
                                    id,
                                    window,
                                    user_interfaces.get_mut(&id).expect("Get user interface"),
                                )
                            };
                            if !input_ready {
                                for event in window_events {
                                    let status = if matches!(event, core::Event::Window(_)) {
                                        core::event::Status::Ignored
                                    } else {
                                        core::event::Status::Captured
                                    };
                                    runtime.broadcast(subscription::Event::Interaction {
                                        window: id,
                                        event,
                                        status,
                                    });
                                }
                                interact_span.finish();
                                continue;
                            }
                            let lane_feedback = if window.renderer.presented_layout().is_some() {
                                None
                            } else {
                                window.take_lane_feedback()
                            };

                            let dispatched = match lane_feedback {
                                Some(at) => {
                                    let mut dispatched =
                                        Vec::with_capacity(window_events.len() + 1);
                                    dispatched.push(core::Event::Window(
                                        window::Event::RedrawRequested(at),
                                    ));
                                    dispatched.extend(window_events.iter().cloned());
                                    dispatched
                                }
                                None => window_events.clone(),
                            };

                            let mut window_requests = Vec::new();
                            // enzICED #2219: `Input` is exactly event dispatch.
                            let input_started = Instant::now();
                            let interface =
                                user_interfaces.get_mut(&id).expect("Get user interface");
                            if let Some(origin) = cursor_origin {
                                interface.set_batch_cursor_origin(origin);
                            }
                            let (ui_state, statuses) = interface.update_with_window_state(
                                &window.raw,
                                &window.waker,
                                &dispatched,
                                window.state.cursor(),
                                &mut window.renderer,
                                &mut messages,
                                native_window_state(&window.raw),
                                &mut window_requests,
                            );
                            dispatch_widget_window_requests(id, &window.raw, window_requests);

                            window.record_stage(
                                crate::graphics::presentation::FrameStage::Input,
                                input_started,
                                Instant::now(),
                            );

                            // The injected frame instant is not an interaction;
                            // only the caller's own events are broadcast.
                            let statuses = match lane_feedback {
                                Some(_) => statuses.into_iter().skip(1).collect::<Vec<_>>(),
                                None => statuses,
                            };

                            #[cfg(feature = "performance-observer")]
                            {
                                let invalidated = ui_state.invalidated();
                                runtime::performance::input_end(
                                    performance_window(id),
                                    invalidated,
                                );
                                if invalidated {
                                    runtime::performance::invalidate(performance_window(id), 1);
                                }
                            }

                            #[cfg(feature = "unconditional-rendering")]
                            window.request_redraw(window::RedrawRequest::NextFrame);

                            match ui_state {
                                user_interface::State::Updated {
                                    redraw_request: _redraw_request,
                                    mouse_interaction,
                                    clipboard: clipboard_requests,
                                    is_content_only: _is_content_only,
                                    ..
                                } => {
                                    if let Some(cursor) = window.update_mouse(mouse_interaction) {
                                        control_sender
                                            .start_send(Control::SetCustomCursor(
                                                window.raw.clone(),
                                                cursor,
                                            ))
                                            .expect("Send control action");
                                    }

                                    // enzUI-ICED #3442: a batch that only
                                    // changed primitives' content asks for a
                                    // frame that need not be drawn in full.
                                    #[cfg(not(feature = "unconditional-rendering"))]
                                    if _is_content_only {
                                        window.request_content_redraw();
                                    } else {
                                        window.request_redraw(_redraw_request);
                                    }

                                    run_clipboard(
                                        &mut proxy,
                                        &mut clipboard,
                                        clipboard_requests,
                                        id,
                                    );
                                }
                                user_interface::State::Outdated => {
                                    // Only *this* window's interface is out of
                                    // date; its siblings were not touched.
                                    stale_uis.push(id);
                                }
                            }

                            for (event, status) in window_events.into_iter().zip(statuses) {
                                runtime.broadcast(subscription::Event::Interaction {
                                    window: id,
                                    event,
                                    status,
                                });
                            }

                            interact_span.finish();
                        }

                        for (id, event) in events.drain(..) {
                            runtime.broadcast(subscription::Event::Interaction {
                                window: id,
                                event,
                                status: core::event::Status::Ignored,
                            });
                        }
                        batch_cursor_origins.clear();

                        if !messages.is_empty() || !stale_uis.is_empty() {
                            let mut cached_interfaces: FxHashMap<_, _> =
                                ManuallyDrop::into_inner(user_interfaces)
                                    .into_iter()
                                    .map(|(id, ui)| (id, ui.into_cache()))
                                    .collect();

                            cached_interfaces.extend(ui_caches.drain());

                            let actions =
                                update(&mut program, &mut runtime, &mut messages, &mut fair);

                            // Nothing is inferred from the events we just
                            // processed: only the program's own declaration
                            // narrows this, and its absence means everything.
                            let mut plan = invalidation::Plan::from_actions(&actions).verified();

                            for id in stale_uis.drain(..) {
                                plan.dirty(id);
                            }

                            let (interfaces, parked) = build_user_interfaces_scoped(
                                &program,
                                &mut window_manager,
                                cached_interfaces,
                                &mut proxy,
                                &plan,
                            );

                            user_interfaces = ManuallyDrop::new(interfaces);
                            ui_caches = parked;

                            for action in actions {
                                run_action(
                                    action,
                                    &program,
                                    &proxy,
                                    &mut runtime,
                                    &mut compositor,
                                    &mut events,
                                    &mut messages,
                                    &mut clipboard,
                                    &mut control_sender,
                                    &mut user_interfaces,
                                    &mut window_manager,
                                    &mut ui_caches,
                                    &mut is_window_opening,
                                    &mut system_theme,
                                    &mut system_reduced_motion,
                                    &mut renderer_settings,
                                    &mut pending_activations,
                                );
                            }

                            for (id, window) in window_manager.iter_mut() {
                                if !plan.is_dirty(id) {
                                    continue;
                                }

                                invalidation::instrumentation::redraw_request(id);
                                window.request_full_redraw();
                            }
                        }

                        if !messages.is_empty() {
                            // The budget ran out with work left over. Every window was
                            // asked to redraw above, which is what resumes the drain;
                            // poll as well, so a shell whose windows are all gone still
                            // wakes up to finish it.
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(ControlFlow::Poll));
                        } else if let Some(wake_at) = window_manager
                            .redraw_at()
                            .into_iter()
                            .chain(activation_deadline)
                            .min()
                        {
                            let _ = control_sender
                                .start_send(Control::ChangeFlow(ControlFlow::WaitUntil(wake_at)));
                        } else {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(ControlFlow::Wait));
                        }
                    }
                    _ => {}
                }
            }
            Event::Exit => break,
        }
    }

    let _ = ManuallyDrop::into_inner(user_interfaces);
}

/// Builds a window's [`UserInterface`] for the [`Program`].
fn build_user_interface<'a, P: Program>(
    program: &'a program::Instance<P>,
    cache: user_interface::Cache,
    renderer: &mut P::Renderer,
    size: Size,
    id: window::Id,
) -> UserInterface<'a, P::Message, P::Theme, P::Renderer>
where
    P::Theme: theme::Base,
{
    let view_span = debug::view(id);
    #[cfg(feature = "performance-observer")]
    let performance_view = runtime::performance::begin_stage(
        performance_window(id),
        runtime::performance::Stage::ViewRebuild,
    );
    // enzICED: widgets read their window's decorations while it is built.
    let view = core::window::viewing(id, || program.view(id));
    #[cfg(feature = "performance-observer")]
    drop(performance_view);
    view_span.finish();
    invalidation::instrumentation::view(id);

    let layout_span = debug::layout(id);
    #[cfg(not(feature = "performance-observer"))]
    let user_interface = UserInterface::build(view, size, cache, renderer);
    #[cfg(feature = "performance-observer")]
    let user_interface =
        UserInterface::build_observed(view, size, cache, renderer, performance_window(id));
    layout_span.finish();
    invalidation::instrumentation::build(id);
    invalidation::instrumentation::layout(id);

    user_interface
}

/// The cooperative budget of the UI thread, and what it has spent so far.
///
/// One "turn" is one dispatch of `RedrawRequested` or `AboutToWait`: the shell
/// may need to drain messages several times before it can hand control back to
/// `winit` (a redraw that invalidates layout does), and all of those drains
/// share one budget. `AboutToWait` starts its own turn because a window that is
/// occluded or absent never redraws (enzICED #4).
struct Fairness {
    budget: fairness::Budget,
    turn: fairness::Turn,
    metrics: fairness::Metrics,
    clock: Clock,
}

impl Fairness {
    fn new() -> Self {
        let budget = fairness::Budget::from_env();

        log::debug!("Message budget per turn: {budget:?}");

        Self {
            budget,
            turn: fairness::Turn::default(),
            metrics: fairness::Metrics::default(),
            clock: Clock::new(),
        }
    }

    /// Starts a new turn, publishing what the previous one spent.
    fn begin_turn(&mut self) {
        if self.turn.messages > 0 || self.turn.yielded {
            self.metrics.record(&self.turn);
            fairness::publish(&self.metrics);

            if self.turn.yielded {
                log::trace!("Message budget spent: {}", self.turn);
            }
        }

        self.turn = fairness::Turn::default();
        self.clock.restart();
    }
}

/// A coarse monotonic clock.
///
/// Reading the real clock once per message would show up in the very profile
/// this budget exists to protect, so it is read once every
/// [`Clock::INTERVAL`] queries. A turn can therefore overshoot
/// [`fairness::Budget::max_duration`] by up to that many messages—the count
/// limits are what make the bound exact.
struct Clock {
    started_at: Instant,
    elapsed: Duration,
    countdown: u16,
}

impl Clock {
    const INTERVAL: u16 = 64;

    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            elapsed: Duration::ZERO,
            countdown: Self::INTERVAL,
        }
    }

    fn restart(&mut self) {
        self.started_at = Instant::now();
        self.elapsed = Duration::ZERO;
        self.countdown = Self::INTERVAL;
    }

    fn elapsed(&mut self) -> Duration {
        if self.countdown == 0 {
            self.elapsed = self.started_at.elapsed();
            self.countdown = Self::INTERVAL;
        } else {
            self.countdown -= 1;
        }

        self.elapsed
    }
}

struct ShellHost<'a, P: Program, E> {
    program: &'a mut program::Instance<P>,
    runtime: &'a mut Runtime<E, Proxy<P::Message>, Action<P::Message>>,
    clock: &'a mut Clock,
}

impl<P: Program, E: Executor> fairness::Host<P::Message> for ShellHost<'_, P, E>
where
    P::Theme: theme::Base,
{
    fn update(&mut self, message: P::Message) -> Option<futures::BoxStream<Action<P::Message>>> {
        let program = &mut *self.program;

        let span = debug::time("update");
        let task = self.runtime.enter(|| program.update(message));
        span.finish();
        runtime::task::into_stream(task)
    }

    fn poll(
        &mut self,
        stream: &mut futures::BoxStream<Action<P::Message>>,
    ) -> task::Poll<Option<Action<P::Message>>> {
        let waker = task::noop_waker_ref();
        let mut context = task::Context::from_waker(waker);

        self.runtime.enter(|| stream.poll_next_unpin(&mut context))
    }

    fn defer(&mut self, stream: futures::BoxStream<Action<P::Message>>) {
        self.runtime.run(stream);
    }

    fn elapsed(&mut self) -> Duration {
        self.clock.elapsed()
    }
}

fn update<P: Program, E: Executor>(
    program: &mut program::Instance<P>,
    runtime: &mut Runtime<E, Proxy<P::Message>, Action<P::Message>>,
    messages: &mut shell::Bus<P::Message>,
    fair: &mut Fairness,
) -> Vec<Action<P::Message>>
where
    P::Theme: theme::Base,
{
    let mut actions = Vec::new();

    let Fairness {
        budget,
        turn,
        clock,
        ..
    } = fair;

    // Upstream's `Bus` pairs each message with a `Receipt` that is dropped
    // once `update` has processed it (`shell::Tracking::is_processed`). The
    // fairness drain yields part-way through a burst, so the receipts are
    // split off and only the ones for processed messages are dropped: the
    // drain processes the queued messages first-in, first-out, before any
    // output they produce, so they are exactly the first `processed`.
    let (mut pending, mut receipts): (Vec<_>, std::collections::VecDeque<_>) =
        messages.drain().unzip();
    let queued = pending.len();
    let processed_before = turn.messages;

    let _ = fairness::drain(
        budget,
        turn,
        &mut pending,
        &mut actions,
        &mut ShellHost {
            program,
            runtime,
            clock,
        },
    );

    let processed = turn.messages.saturating_sub(processed_before).min(queued);
    drop(receipts.drain(..processed));

    for message in pending {
        match receipts.pop_front() {
            Some(receipt) => messages.forward(message, receipt),
            None => {
                let _ = messages.push(message);
            }
        }
    }

    let subscription = runtime.enter(|| program.subscription());
    let recipes = subscription::into_recipes(subscription.map(Action::Output));

    runtime.track(recipes);

    actions
}

fn native_window_state(window: &winit::window::Window) -> core::window::RuntimeState {
    core::window::RuntimeState {
        resizable: window.is_resizable(),
        maximized: window.is_maximized(),
        fullscreen: window.fullscreen().is_some(),
    }
}

/// Bind an id-less widget request to the exact window whose interface produced
/// it. Native state is checked here, at the platform boundary, so stale widget
/// state can never start a forbidden resize.
fn bind_widget_window_request(
    id: core::window::Id,
    state: core::window::RuntimeState,
    request: core::window::Request,
) -> Option<runtime::window::Action> {
    match request {
        core::window::Request::DragResize(direction) if state.can_resize() => {
            Some(runtime::window::Action::DragResize(id, direction))
        }
        core::window::Request::DragResize(_) => None,
    }
}

trait WidgetWindowDispatcher {
    fn runtime_state(&self) -> core::window::RuntimeState;

    fn drag_resize(&self, id: core::window::Id, direction: core::window::Direction);
}

impl WidgetWindowDispatcher for Arc<winit::window::Window> {
    fn runtime_state(&self) -> core::window::RuntimeState {
        native_window_state(self)
    }

    fn drag_resize(&self, _id: core::window::Id, direction: core::window::Direction) {
        let _ = self.drag_resize_window(conversion::resize_direction(direction));
    }
}

fn dispatch_widget_window_requests(
    id: core::window::Id,
    window: &impl WidgetWindowDispatcher,
    requests: impl IntoIterator<Item = core::window::Request>,
) {
    for request in requests {
        if let Some(runtime::window::Action::DragResize(bound, direction)) =
            bind_widget_window_request(id, window.runtime_state(), request)
        {
            window.drag_resize(bound, direction);
        }
    }
}

/// Puts `new_compositor` under every window: each window's lane stops and its
/// surface and renderer are rebuilt on the new device.
fn install_compositor<P, C>(
    window_manager: &mut window::Manager<P, C>,
    new_compositor: &mut C,
    renderer_settings: renderer::Settings,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer> + 'static,
    P::Theme: theme::Base,
{
    graphics::cache::invalidate_all();

    window_manager.replace_with(|mut window| {
        let size = window.state.physical_size();

        // The lane owns a renderer built on the OLD device and may
        // own the surface. Both must be gone before either is
        // recreated (#2219).
        window.shutdown_lane();
        if window.presentation_quarantined() {
            return window;
        }

        // The old surface goes first, while the device that created it is alive; the
        // assignment then drops the old renderer and the last handle to that device.
        window.recreate_surface(new_compositor, size.width, size.height);
        window.renderer = new_compositor.create_renderer(renderer_settings);

        // No lane is started here: the first travel asks the new compositor through
        // `ensure_lane`, which installs the stop waker and presentation observer.

        window
    });
}

/// Replaces a compositor whose graphics device was lost with one on a new
/// device, so the application keeps running instead of panicking on the dead
/// device. A failed attempt (the memory pressure that lost the device can
/// still be there) is retried at most once a second; until then windows keep
/// their last frame.
#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::too_many_arguments,
    reason = "the runtime state a compositor replacement touches"
)]
fn rebuild_lost_device<P, C>(
    compositor: &mut Option<C>,
    window_manager: &mut window::Manager<P, C>,
    backend_settings: &backend::Settings,
    renderer_settings: renderer::Settings,
    runtime: &mut Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    proxy: &Proxy<P::Message>,
    default_fonts: &[Cow<'static, [u8]>],
    retry_at: &mut Option<Instant>,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer> + 'static,
    P::Theme: theme::Base,
{
    if retry_at.is_some_and(|at| Instant::now() < at) {
        return;
    }
    let Some(window) = window_manager.first() else {
        return;
    };
    let created = runtime.block_on(C::new(
        backend_settings.clone(),
        window.raw.clone(),
        window.raw.clone(),
        Shell::new(proxy.clone()),
    ));
    let mut new_compositor = match created {
        Ok(new_compositor) => new_compositor,
        Err(error) => {
            log::error!("Graphics device lost; a new one could not be created yet: {error}");
            *retry_at = Some(Instant::now() + Duration::from_secs(1));
            return;
        }
    };
    *retry_at = None;
    for font in default_fonts {
        if let Err(error) = new_compositor.load_font(font.clone()) {
            log::warn!("Default font not loaded: {error}");
        }
    }
    log::warn!("Graphics device lost; rendering continues on a new device");
    crate::presentation::trace_information(&new_compositor.information());

    install_compositor(window_manager, &mut new_compositor, renderer_settings);
    *compositor = Some(new_compositor);

    for (_id, window) in window_manager.iter_mut() {
        window.invalidate_scene();
        window.request_full_redraw();
    }
}

fn run_action<'a, P, C>(
    action: Action<P::Message>,
    program: &'a program::Instance<P>,
    _proxy: &Proxy<P::Message>,
    runtime: &mut Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    compositor: &mut Option<C>,
    events: &mut Vec<(window::Id, core::Event)>,
    messages: &mut shell::Bus<P::Message>,
    clipboard: &mut Clipboard,
    control_sender: &mut mpsc::UnboundedSender<Control>,
    interfaces: &mut FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>,
    window_manager: &mut window::Manager<P, C>,
    ui_caches: &mut FxHashMap<window::Id, user_interface::Cache>,
    is_window_opening: &mut bool,
    system_theme: &mut theme::Mode,
    system_reduced_motion: &mut bool,
    renderer_settings: &mut renderer::Settings,
    pending_activations: &mut Vec<PendingActivation>,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer> + 'static,
    P::Theme: theme::Base,
{
    use crate::core::Renderer as _;
    use crate::runtime::backend;
    use crate::runtime::clipboard;
    use crate::runtime::window;

    match action {
        Action::Output(message) => {
            let _ = messages.push(message);
        }
        Action::Clipboard(action) => match action {
            clipboard::Action::Read { kind, channel } => {
                clipboard.read(kind, move |result| {
                    let _ = channel.send(result);
                });
            }
            clipboard::Action::Write { content, channel } => {
                clipboard.write(content, move |result| {
                    let _ = channel.send(result);
                });
            }
            clipboard::Action::ReadPrimary { kind, channel } => {
                clipboard.read_primary(kind, move |result| {
                    let _ = channel.send(result);
                });
            }
            clipboard::Action::WritePrimary { content, channel } => {
                clipboard.write_primary(content, move |result| {
                    let _ = channel.send(result);
                });
            }
        },
        Action::Window(action) => match action {
            window::Action::Open(id, settings, channel) => {
                let monitor = window_manager.last_monitor();
                let settings_parent = settings.parent.and_then(|parent| {
                    window_manager
                        .get_mut(parent)
                        .map(|window| (parent, window.raw.id()))
                });

                control_sender
                    .start_send(Control::CreateWindow {
                        id,
                        settings,
                        activation_token: None,
                        title: program.title(id),
                        scale_factor: program.scale_factor(id),
                        monitor,
                        on_open: channel,
                        parent: settings_parent,
                    })
                    .expect("Send control action");

                *is_window_opening = true;
            }
            window::Action::OpenWithActivation(id, settings, token, channel) => {
                let monitor = window_manager.last_monitor();
                let settings_parent = settings.parent.and_then(|parent| {
                    window_manager
                        .get_mut(parent)
                        .map(|window| (parent, window.raw.id()))
                });

                control_sender
                    .start_send(Control::CreateWindow {
                        id,
                        settings,
                        activation_token: Some(token),
                        title: program.title(id),
                        scale_factor: program.scale_factor(id),
                        monitor,
                        on_open: channel,
                        parent: settings_parent,
                    })
                    .expect("Send control action");

                *is_window_opening = true;
            }
            window::Action::Close(id) => {
                let canceled = fail_window_activation_requests(pending_activations, id);
                #[cfg(target_os = "linux")]
                if let Some(window) = window_manager.get_mut(id) {
                    use winit::platform::startup_notify::WindowExtStartupNotify;

                    for serial in canceled {
                        let _ = window.raw.cancel_activation_token_request(serial);
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = canceled;

                let _ = ui_caches.remove(&id);
                let _ = interfaces.remove(&id);

                if window_manager.remove(id).is_some() {
                    // Tell the accessibility owner which exact window died, so
                    // the routing table (and any queued actions) published for
                    // it are released instead of accumulating per close.
                    #[cfg(feature = "a11y")]
                    accessibility::dispatch_window_closed(id);

                    events.push((id, core::Event::Window(core::window::Event::Closed)));
                }

                if window_manager.is_empty() {
                    *compositor = None;
                }
            }
            window::Action::GetOldest(channel) => {
                let id = window_manager.iter_mut().next().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::GetLatest(channel) => {
                let id = window_manager.iter_mut().last().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::Drag(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.drag_window();
                }
            }
            window::Action::DragResize(id, direction) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window
                        .raw
                        .drag_resize_window(conversion::resize_direction(direction));
                }
            }
            window::Action::Resize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let applied = window.raw.request_inner_size(
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor())),
                    );
                    // A size the platform applies at once (a floating Wayland
                    // window) comes back here and sends no `Resized`, so it
                    // is handled as one: without this the surface keeps its
                    // old layout and the compositor stretches it (enzICED).
                    if let Some(applied) = applied {
                        let resized = winit::event::WindowEvent::Resized(applied);
                        window.state.update(program, &window.raw, &resized);
                        window.request_full_redraw();
                        if let Some(event) = conversion::window_event(
                            resized,
                            window.state.scale_factor(),
                            window.state.modifiers(),
                        ) {
                            events.push((id, event));
                        }
                    }
                }
            }
            window::Action::SetMinSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_min_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetMaxSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_max_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetResizeIncrements(id, increments) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resize_increments(increments.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetResizable(id, resizable) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resizable(resizable);
                }
            }
            window::Action::GetSize(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let size = window.state.logical_size();
                    let _ = channel.send(Size::new(size.width, size.height));
                }
            }
            window::Action::GetMaximized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_maximized());
                }
            }
            window::Action::Maximize(id, maximized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(maximized);
                }
            }
            window::Action::GetMinimized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_minimized());
                }
            }
            window::Action::Minimize(id, minimized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_minimized(minimized);
                }
            }
            window::Action::GetPosition(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let position = window
                        .raw
                        .outer_position()
                        .map(|position| {
                            let position = position.to_logical::<f32>(window.raw.scale_factor());

                            Point::new(position.x, position.y)
                        })
                        .ok();

                    let _ = channel.send(position);
                }
            }
            window::Action::GetScaleFactor(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let scale_factor = window.raw.scale_factor();

                    let _ = channel.send(scale_factor as f32);
                }
            }
            window::Action::Move(id, position) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_outer_position(winit::dpi::LogicalPosition {
                        x: position.x,
                        y: position.y,
                    });
                }
            }
            window::Action::SetMode(id, mode) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_visible(conversion::visible(mode));
                    window
                        .raw
                        .set_fullscreen(conversion::fullscreen(window.raw.current_monitor(), mode));
                }
            }
            window::Action::SetIcon(id, icon) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_window_icon(conversion::icon(icon));
                }
            }
            window::Action::SetShadow(id, shadow) => {
                #[cfg(all(feature = "wayland", target_os = "linux"))]
                if let Some(window) = window_manager.get_mut(id) {
                    use winit::platform::wayland::WindowExtWayland;
                    window
                        .raw
                        .set_window_shadow(shadow.as_ref().map(conversion::window_shadow));
                }
                #[cfg(not(all(feature = "wayland", target_os = "linux")))]
                let _ = (id, shadow);
            }
            window::Action::GetMode(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let mode = if window.raw.is_visible().unwrap_or(true) {
                        conversion::mode(window.raw.fullscreen())
                    } else {
                        core::window::Mode::Hidden
                    };

                    let _ = channel.send(mode);
                }
            }
            window::Action::ToggleMaximize(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(!window.raw.is_maximized());
                }
            }
            window::Action::ToggleDecorations(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_decorations(!window.raw.is_decorated());
                }
            }
            window::Action::RequestUserAttention(id, attention_type) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window
                        .raw
                        .request_user_attention(attention_type.map(conversion::user_attention));
                }
            }
            window::Action::GainFocus(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.focus_window();
                }
            }
            window::Action::RequestActivationToken(id, channel) => {
                let (_, canceled) = expire_activation_requests(pending_activations, Instant::now());
                #[cfg(target_os = "linux")]
                {
                    use winit::platform::startup_notify::WindowExtStartupNotify;

                    for (window_id, serial) in canceled {
                        if let Some(window) = window_manager.get_mut(window_id) {
                            let _ = window.raw.cancel_activation_token_request(serial);
                        }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = canceled;

                if channel.is_canceled() {
                    return;
                }

                let Some(window) = window_manager.get_mut(id) else {
                    let _ = channel.send(Err(core::window::ActivationError::WindowClosed));
                    return;
                };

                if pending_activations.len() >= MAX_PENDING_ACTIVATIONS {
                    let _ = channel.send(Err(core::window::ActivationError::Busy));
                    return;
                }

                #[cfg(target_os = "linux")]
                {
                    use winit::platform::startup_notify::WindowExtStartupNotify;

                    use core::window::raw_window_handle::HasDisplayHandle as _;
                    let Some(platform) = window
                        .raw
                        .display_handle()
                        .ok()
                        .and_then(|display| activation_platform(display.as_raw()))
                    else {
                        let _ = channel.send(Err(core::window::ActivationError::Unsupported));
                        return;
                    };
                    match window.raw.request_activation_token() {
                        Ok(serial) if channel.is_canceled() => {
                            let _ = window.raw.cancel_activation_token_request(serial);
                        }
                        Ok(serial) => pending_activations.push(PendingActivation {
                            window: id,
                            serial,
                            platform,
                            reply: channel,
                            expires_at: Instant::now() + ACTIVATION_REQUEST_TIMEOUT,
                        }),
                        Err(_) => {
                            let _ = channel.send(Err(core::window::ActivationError::Unsupported));
                        }
                    }
                }

                #[cfg(not(target_os = "linux"))]
                {
                    let _ = channel.send(Err(core::window::ActivationError::Unsupported));
                }
            }
            window::Action::Activate(id, token, channel) => {
                let Some(window) = window_manager.get_mut(id) else {
                    let _ = channel.send(Err(core::window::ActivationError::WindowClosed));
                    return;
                };

                #[cfg(target_os = "linux")]
                {
                    use winit::platform::startup_notify::WindowExtStartupNotify;

                    let mut token = Some(token);
                    use core::window::raw_window_handle::HasDisplayHandle as _;
                    let platform = window
                        .raw
                        .display_handle()
                        .ok()
                        .and_then(|display| activation_platform(display.as_raw()));
                    let result = activate_surface_once(&mut token, platform, |native| {
                        window.raw.activate_with_token(native)
                    });
                    let _ = channel.send(result);
                }

                #[cfg(not(target_os = "linux"))]
                {
                    let _ = token;
                    let _ = channel.send(Err(core::window::ActivationError::Unsupported));
                }
            }
            window::Action::SetLevel(id, level) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_window_level(conversion::window_level(level));
                }
            }
            window::Action::ShowSystemMenu(id) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let mouse::Cursor::Available(point) = window.state.cursor()
                {
                    window.raw.show_window_menu(winit::dpi::LogicalPosition {
                        x: point.x,
                        y: point.y,
                    });
                }
            }
            window::Action::GetRawId(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.id().into());
                }
            }
            #[cfg(all(target_os = "linux", feature = "wayland"))]
            window::Action::WaylandParentExport(id, channel) => {
                use winit::platform::wayland::WindowExtWayland as _;

                // A window the manager no longer holds answers `None`: the
                // surface is gone with it, so no lease may be taken or renewed
                // against that identity.
                let payload = window_manager
                    .get_mut(id)
                    .and_then(|window| window.raw.wayland_parent_objects())
                    .map(|objects| Box::new(objects) as Box<dyn std::any::Any + Send>);
                let _ = channel.send(payload);
            }
            #[cfg(not(all(target_os = "linux", feature = "wayland")))]
            window::Action::WaylandParentExport(_id, _channel) => {}
            window::Action::GetWaylandSessionId(id, channel) => {
                #[cfg(all(target_os = "linux", feature = "wayland"))]
                let session = {
                    use winit::platform::wayland::WindowExtWayland as _;
                    window_manager
                        .get_mut(id)
                        .and_then(|window| window.raw.wayland_session_id())
                };
                #[cfg(not(all(target_os = "linux", feature = "wayland")))]
                let session = {
                    let _ = id;
                    None
                };
                let _ = channel.send(session);
            }
            #[cfg(all(target_os = "linux", feature = "wayland"))]
            window::Action::WaylandToplevel(id, channel) => {
                use winit::platform::wayland::WindowExtWayland as _;

                let toplevel = window_manager
                    .get_mut(id)
                    .and_then(|window| window.raw.wayland_toplevel())
                    .map(|toplevel| Box::new(toplevel) as Box<dyn std::any::Any + Send>);
                let _ = channel.send(toplevel);
            }
            #[cfg(not(all(target_os = "linux", feature = "wayland")))]
            window::Action::WaylandToplevel(_id, channel) => {
                let _ = channel.send(None);
            }
            window::Action::SetToplevelTag(id, tag, channel) => {
                #[cfg(all(target_os = "linux", feature = "wayland"))]
                let queued = {
                    use winit::platform::wayland::WindowExtWayland as _;
                    window_manager
                        .get_mut(id)
                        .is_some_and(|window| window.raw.set_toplevel_tag(&tag))
                };
                #[cfg(not(all(target_os = "linux", feature = "wayland")))]
                let queued = {
                    let _ = (id, tag);
                    false
                };
                let _ = channel.send(queued);
            }
            window::Action::Run(id, f) => {
                if let Some(window) = window_manager.get_mut(id) {
                    f(&window.raw);
                }
            }
            window::Action::Screenshot(id, channel) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let Some(compositor) = compositor
                {
                    let bytes = compositor.screenshot(
                        &mut window.renderer,
                        window.state.viewport(),
                        window.state.background_color(),
                    );

                    let _ = channel.send(core::window::Screenshot::new(
                        bytes,
                        window.state.physical_size(),
                        window.state.scale_factor(),
                    ));
                }
            }
            window::Action::SetCursorGrab(id, grab, channel) => {
                let mut granted = false;
                if let Some(window) = window_manager.get_mut(id) {
                    use core::window::CursorGrab;
                    use winit::window::CursorGrabMode;
                    let mode = match grab {
                        CursorGrab::None => CursorGrabMode::None,
                        CursorGrab::Confined => CursorGrabMode::Confined,
                        CursorGrab::Locked => CursorGrabMode::Locked,
                    };
                    granted = window.raw.set_cursor_grab(mode).is_ok();
                    window.cursor_grab = if granted { grab } else { CursorGrab::None };
                }
                let _ = channel.send(granted);
            }
            window::Action::EnableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(false);
                }
            }
            window::Action::DisableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(true);
                }
            }
            window::Action::GetMonitors(channel) => {
                control_sender
                    .start_send(Control::ListMonitors(channel))
                    .expect("Send control action");
            }
            window::Action::GetMonitorSize(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let size = window.raw.current_monitor().map(|monitor| {
                        let scale = window.state.scale_factor();
                        let size = monitor.size().to_logical(f64::from(scale));

                        Size::new(size.width, size.height)
                    });

                    let _ = channel.send(size);
                }
            }
            window::Action::SetAllowAutomaticTabbing(enabled) => {
                control_sender
                    .start_send(Control::SetAutomaticWindowTabbing(enabled))
                    .expect("Send control action");
            }
            window::Action::RedrawAll => {
                for (_id, window) in window_manager.iter_mut() {
                    window.request_full_redraw();
                }
            }
            window::Action::RelayoutAll => {
                for (id, window) in window_manager.iter_mut() {
                    if let Some(ui) = interfaces.remove(&id) {
                        let _ = interfaces.insert(
                            id,
                            ui.relayout(window.state.logical_size(), &mut window.renderer),
                        );
                    }

                    window.request_full_redraw();
                }
            }
        },
        Action::System(action) => match action {
            system::Action::GetInformation(_channel) => {
                #[cfg(feature = "sysinfo")]
                {
                    if let Some(compositor) = compositor {
                        let graphics_info = compositor.information();

                        let _ = std::thread::spawn(move || {
                            let information = system_information(graphics_info);

                            let _ = _channel.send(information);
                        });
                    }
                }
            }
            system::Action::GetTheme(channel) => {
                let _ = channel.send(*system_theme);
            }
            system::Action::GetReducedMotion(channel) => {
                let _ = channel.send(*system_reduced_motion);
            }
            system::Action::NotifyReducedMotion(reduced_motion) => {
                if reduced_motion != *system_reduced_motion {
                    *system_reduced_motion = reduced_motion;

                    runtime.broadcast(subscription::Event::SystemReducedMotionChanged(
                        reduced_motion,
                    ));
                }
            }
            system::Action::NotifyTheme(mode) => {
                route_watcher_theme_changed(
                    system_theme,
                    mode,
                    window_manager
                        .iter_mut()
                        .map(|(_id, window)| LiveSystemThemeWindow { program, window }),
                    |mode| {
                        runtime.broadcast(subscription::Event::SystemThemeChanged(mode));
                    },
                );
            }
        },
        Action::Font(action) => match action {
            font::Action::Load { bytes, channel } => {
                if let Some(compositor) = compositor {
                    // The font generation only advances on a real database
                    // mutation, so it is the exact signal a retained scene is
                    // stale. Each guard is a temporary dropped at the `;`, so
                    // `load_font`'s own write lock never contends with it.
                    let generation_before = crate::graphics::text::font_system()
                        .read()
                        .expect("Read font system")
                        .version();

                    let result = compositor.load_font(bytes.clone());

                    let generation_advanced = result.is_ok()
                        && crate::graphics::text::font_system()
                            .read()
                            .expect("Read font system")
                            .version()
                            != generation_before;

                    let _ = channel.send(result);

                    // A newly loaded font can re-shape text that no widget
                    // considers changed, so every committed scene is stale
                    // (#2219). A rejected or duplicate load mutates no glyph,
                    // so the retained scenes stay valid.
                    if generation_advanced {
                        for (_id, window) in window_manager.iter_mut() {
                            window.invalidate_scene();
                        }
                    }
                }
            }
            font::Action::List { channel } => {
                if let Some(compositor) = compositor {
                    let fonts = compositor.list_fonts();
                    let _ = channel.send(fonts);
                }
            }
            font::Action::SetDefaults { font, text_size } => {
                renderer_settings.font = font;
                renderer_settings.text_size = text_size;

                let Some(compositor) = compositor else {
                    return;
                };

                // Recreate renderers and relayout all windows
                for (id, window) in window_manager.iter_mut() {
                    // A new default font or text size re-shapes text the
                    // committed scene recorded under the old one (#2219).
                    window.invalidate_scene();

                    window.renderer = compositor.create_renderer(*renderer_settings);

                    let Some(ui) = interfaces.remove(&id) else {
                        continue;
                    };

                    let size = window.state.logical_size();
                    let ui = ui.relayout(size, &mut window.renderer);
                    let _ = interfaces.insert(id, ui);

                    window.request_full_redraw();
                }
            }
        },
        Action::Widget(operation) => {
            // An operation walks every window, so a parked one has to be live
            // before it runs.
            revive_user_interfaces(interfaces, ui_caches, program, window_manager);

            let mut current_operation = Some(operation);

            while let Some(mut operation) = current_operation.take() {
                for (id, ui) in interfaces.iter_mut() {
                    if let Some(window) = window_manager.get_mut(*id) {
                        if window.presentation_quarantined() {
                            continue;
                        }
                        window.revoke_presented_input();
                        ui.operate_native(&mut window.renderer, operation.as_mut());
                    }
                }

                match operation.finish() {
                    operation::Outcome::None => {}
                    operation::Outcome::Some(()) => {}
                    operation::Outcome::Chain(next) => {
                        current_operation = Some(next);
                    }
                }
            }

            // Redraw all windows
            for (_, window) in window_manager.iter_mut() {
                window.request_full_redraw();
            }
        }
        Action::WidgetIn { window, operation } => {
            // Only this window's interface may be operated, so only this
            // window is revived and redrawn. A widget id published by two
            // windows must not be operated in both, and a *parked* window
            // must stay parked: reviving every interface here would rebuild
            // (and redraw-cost) windows this operation can never touch
            // (#2680). If the named window is gone, nothing is revived at
            // all and the operation finishes with no target.
            if let Some(target) = window_manager.get_mut(window) {
                revive_user_interface(interfaces, ui_caches, program, target, window);
            }

            let _ = runtime::operate_in_window(window, operation, |target, operation| {
                match (interfaces.get_mut(&target), window_manager.get_mut(target)) {
                    (Some(ui), Some(window)) if !window.presentation_quarantined() => {
                        window.revoke_presented_input();
                        ui.operate_native(&mut window.renderer, operation);
                        true
                    }
                    _ => false,
                }
            });

            if let Some(window) = window_manager.get_mut(window) {
                window.request_full_redraw();
            }
        }
        Action::Image(action) => match action {
            image::Action::Allocate(handle, sender) => {
                // TODO: Shared image cache in compositor
                if let Some((_id, window)) = window_manager.iter_mut().next() {
                    window.renderer.allocate_image(&handle, move |allocation| {
                        let _ = sender.send(allocation);
                    });
                }
            }
        },
        Action::Backend(action) => match action {
            #[cfg(not(target_arch = "wasm32"))]
            backend::Action::Configure(settings, sender) => {
                let shell = Shell::new(_proxy.clone());

                let mut new_compositor = if let Some(window) = window_manager.first() {
                    match runtime.block_on(C::new(
                        settings,
                        window.raw.clone(),
                        window.raw.clone(),
                        shell,
                    )) {
                        Ok(compositor) => compositor,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            return;
                        }
                    }
                } else {
                    return;
                };

                install_compositor(window_manager, &mut new_compositor, *renderer_settings);

                *compositor = Some(new_compositor);

                let _ = sender.send(Ok(()));
            }
            #[cfg(target_arch = "wasm32")]
            backend::Action::Configure(_, _) => {}
        },
        Action::Event { window, event } => {
            events.push((window, event));
        }
        Action::Tick => {
            for (_id, window) in window_manager.iter_mut() {
                window.renderer.tick();
            }
        }
        Action::Reload => {
            revive_user_interfaces(interfaces, ui_caches, program, window_manager);

            for (id, window) in window_manager.iter_mut() {
                let Some(ui) = interfaces.remove(&id) else {
                    window.request_full_redraw();
                    continue;
                };

                let cache = ui.into_cache();
                let size = window.state.logical_size();

                let _ = interfaces.insert(
                    id,
                    build_user_interface(program, cache, &mut window.renderer, size, id),
                );

                window.request_full_redraw();
            }
        }
        Action::Invalidate(_) => {
            // Already consumed by `invalidation::Plan::from_actions` before
            // the interfaces of this turn were rebuilt.
        }
        Action::Exit => {
            control_sender
                .start_send(Control::Exit)
                .expect("Send control action");
        }
    }
}

/// Build the user interface for every window.
pub fn build_user_interfaces<'a, P: Program, C>(
    program: &'a program::Instance<P>,
    window_manager: &mut window::Manager<P, C>,
    cached_user_interfaces: FxHashMap<window::Id, user_interface::Cache>,
    proxy: &mut Proxy<P::Message>,
) -> FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>
where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    let (interfaces, _parked) = build_user_interfaces_scoped(
        program,
        window_manager,
        cached_user_interfaces,
        proxy,
        &invalidation::Plan::global(),
    );

    interfaces
}

/// Builds the user interface of every window a [`Plan`] dirtied, and parks
/// the cache of every window it left clean.
///
/// A parked window pays no `view`, no `UserInterface::build` and no layout;
/// the runtime revives it from its cache the next time an event or a redraw
/// reaches it. Windows are still synchronized (title, scale factor, theme,
/// size) because that is how a program's own window state reaches the shell,
/// and a window whose size moved is dirtied regardless of the [`Plan`].
pub fn build_user_interfaces_scoped<'a, P: Program, C>(
    program: &'a program::Instance<P>,
    window_manager: &mut window::Manager<P, C>,
    mut cached_user_interfaces: FxHashMap<window::Id, user_interface::Cache>,
    proxy: &mut Proxy<P::Message>,
    plan: &invalidation::Plan,
) -> (
    FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>,
    FxHashMap<window::Id, user_interface::Cache>,
)
where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    let mut plan = plan.clone();

    for (id, window) in window_manager.iter_mut() {
        let old_size = window.state.logical_size();
        // enzICED #2219: what `synchronize` below is actually allowed to move.
        // Sampled BEFORE, so retention is invalidated when one of them really
        // changed rather than on every rebuild.
        let old_scale = window.state.scale();
        let old_background = window.state.background_color();
        let old_text = window.state.text_color();

        window.state.synchronize(program, id, &window.raw);

        let new_size = window.state.logical_size();

        if old_size != new_size {
            // The window resized underneath the update; it must be rebuilt
            // whatever the program declared.
            plan.dirty(id);

            proxy.send_action(Action::Event {
                window: id,
                event: core::Event::Window(window::Event::Resized(new_size)),
            });
        }

        // The program was synchronized: theme, scale and size may all have
        // moved underneath the committed scene, and none of them is a track.
        // Retention fails closed when one of them did (#2219).
        //
        // It must NOT fail closed when none of them did. `invalidate_scene`
        // reclaims the surface from the animation lane, and this function runs
        // on every rebuild -- so any message during a travel stopped the lane
        // and put the animation back behind the UI thread. That was measured,
        // not deduced: a backtrace from `reclaim_surface` on a real GPU run
        // named this call site, and the lane's own trace showed
        // `Stop::Reclaimed` after 4 frames with 145.6 ms of travel in hand,
        // followed by a 102 ms hole while the UI thread finished its
        // deterministic stall. A rebuild is application work, and the issue
        // requires application work to stay off the cadence-critical path.
        let retention_invalid = old_size != new_size
            || old_scale != window.state.scale()
            || old_background != window.state.background_color()
            || old_text != window.state.text_color();

        if retention_invalid {
            window.invalidate_scene();
        }

        window.renderer.hint(window.state.scale());
    }

    debug::theme_changed(|| {
        window_manager
            .first()
            .and_then(|window| theme::Base::seed(window.state.theme()))
    });

    let live: rustc_hash::FxHashSet<window::Id> =
        window_manager.iter_mut().map(|(id, _window)| id).collect();

    let slots = invalidation::rebuild(
        &plan,
        cached_user_interfaces.drain(),
        |id| live.contains(&id),
        |id, cache| {
            let window = window_manager.get_mut(id)?;
            window.begin_native_update(id);

            Some(build_user_interface(
                program,
                cache,
                &mut window.renderer,
                window.state.logical_size(),
                id,
            ))
        },
    );

    let mut interfaces = FxHashMap::default();
    let mut parked = FxHashMap::default();

    for (id, slot) in slots {
        match slot {
            invalidation::Slot::Built(interface) => {
                let _ = interfaces.insert(id, interface);
            }
            invalidation::Slot::Parked(cache) => {
                let _ = parked.insert(id, cache);
            }
        }
    }

    (interfaces, parked)
}

/// Revives every parked user interface.
///
/// Used by the actions that must reach *every* window — a widget operation, a
/// reload — even when an earlier scoped update left some of them parked.
fn revive_user_interfaces<'a, P: Program, C>(
    interfaces: &mut FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>,
    parked: &mut FxHashMap<window::Id, user_interface::Cache>,
    program: &'a program::Instance<P>,
    window_manager: &mut window::Manager<P, C>,
) where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    for id in parked.keys().copied().collect::<Vec<_>>() {
        let Some(window) = window_manager.get_mut(id) else {
            let _ = parked.remove(&id);
            continue;
        };

        revive_user_interface(interfaces, parked, program, window, id);
    }
}

/// Revives the user interface of a window that a [`Plan`] left parked.
///
/// Does nothing when the interface is already live, so the cost of a scoped
/// update is paid by the window that actually needs it, at the moment it needs
/// it, and never by its siblings.
fn revive_user_interface<'a, P: Program, C>(
    interfaces: &mut FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>,
    parked: &mut FxHashMap<window::Id, user_interface::Cache>,
    program: &'a program::Instance<P>,
    window: &mut window::Window<P, C>,
    id: window::Id,
) where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    if interfaces.contains_key(&id) {
        return;
    }

    let cache = parked.remove(&id).unwrap_or_default();
    let size = window.state.logical_size();
    window.begin_native_update(id);

    let _ = interfaces.insert(
        id,
        build_user_interface(program, cache, &mut window.renderer, size, id),
    );
}

/// Returns true if the provided event should cause a [`Program`] to
/// exit.
pub fn user_force_quit(
    event: &winit::event::WindowEvent,
    _modifiers: winit::keyboard::ModifiersState,
) -> bool {
    match event {
        #[cfg(target_os = "macos")]
        winit::event::WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(c),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } if c == "q" && _modifiers.super_key() => true,
        _ => false,
    }
}

#[cfg(feature = "sysinfo")]
fn system_information(graphics: compositor::Information) -> system::Information {
    use sysinfo::{CpuRefreshKind, System};

    // Only what is reported is read (enzICED). `System::new_all` scanned
    // every process on the machine on a rayon pool, which then kept one idle
    // thread per CPU for the life of the app.
    let mut system = System::new();
    system.refresh_memory();
    system.refresh_cpu_list(CpuRefreshKind::nothing());

    let cpu_brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().to_string())
        .unwrap_or_default();

    let memory_used = own_resident_memory();

    system::Information {
        system_name: System::name(),
        system_kernel: System::kernel_version(),
        system_version: System::long_os_version(),
        system_short_version: System::os_version(),
        cpu_brand,
        cpu_cores: system.physical_core_count(),
        memory_total: system.total_memory(),
        memory_used,
        graphics_adapter: graphics.adapter,
        graphics_backend: graphics.backend,
    }
}

/// This process's resident memory in bytes.
#[cfg(feature = "sysinfo")]
#[cfg(target_os = "linux")]
fn own_resident_memory() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kilobytes: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    kilobytes.checked_mul(1024)
}

/// This process's resident memory in bytes.
#[cfg(feature = "sysinfo")]
#[cfg(not(target_os = "linux"))]
fn own_resident_memory() -> Option<u64> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let pid = sysinfo::get_current_pid().ok()?;
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map(sysinfo::Process::memory)
}

#[cfg(all(test, feature = "sysinfo", target_os = "linux"))]
mod system_information_tests {
    use super::*;

    fn threads() -> usize {
        std::fs::read_dir("/proc/self/task").map_or(0, Iterator::count)
    }

    /// Reading the system information starts no thread pool and still
    /// reports this process's memory.
    ///
    /// Sabotage: go back to `System::new_all` and `refresh_all`, and the
    /// process gains a rayon thread per CPU.
    #[test]
    fn system_information_reads_only_what_it_reports() {
        let before = threads();
        let info = system_information(compositor::Information {
            adapter: "adapter".to_owned(),
            backend: "backend".to_owned(),
            device: "device".to_owned(),
        });
        let after = threads();
        assert!(after <= before + 1, "{before} -> {after} threads");
        assert!(info.memory_used.is_some_and(|bytes| bytes > 1024 * 1024));
        assert!(info.memory_total > 0);
    }
}

fn run_clipboard<Message: Send>(
    proxy: &mut Proxy<Message>,
    clipboard: &mut Clipboard,
    requests: core::Clipboard,
    window: window::Id,
) {
    for kind in requests.reads {
        let proxy = proxy.clone();

        clipboard.read(kind, move |result| {
            proxy.send_action(Action::Event {
                window,
                event: core::Event::Clipboard(core::clipboard::Event::Read(result.map(Arc::new))),
            });
        });
    }

    if let Some(content) = requests.write {
        let proxy = proxy.clone();

        clipboard.write(content, move |result| {
            proxy.send_action(Action::Event {
                window,
                event: core::Event::Clipboard(core::clipboard::Event::Written(result)),
            });
        });
    }

    for kind in requests.primary_reads {
        let proxy = proxy.clone();

        clipboard.read_primary(kind, move |result| {
            proxy.send_action(Action::Event {
                window,
                event: core::Event::Clipboard(core::clipboard::Event::ReadPrimary(
                    result.map(Arc::new),
                )),
            });
        });
    }

    if let Some(content) = requests.primary_write {
        let proxy = proxy.clone();

        clipboard.write_primary(content, move |result| {
            proxy.send_action(Action::Event {
                window,
                event: core::Event::Clipboard(core::clipboard::Event::WrittenPrimary(result)),
            });
        });
    }
}

fn prepare_widget_input<P: Program, C>(
    id: window::Id,
    window: &mut window::Window<P, C>,
    ui: &mut UserInterface<'_, P::Message, P::Theme, P::Renderer>,
) -> bool
where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    if window.prepare_widget_input(id) {
        if ui.reconcile_presented_layout(&window.renderer).is_ok() {
            return true;
        }
        window.refuse_native_reconciliation(id);
    }
    ui.cancel_interactions();
    if window.take_input_relayout() && !window.presentation_quarantined() {
        ui.reconcile_native_layout(&window.renderer);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        Clock, Duration, SystemThemeWindow, WidgetWindowDispatcher, bind_widget_window_request,
        dispatch_widget_window_requests, route_native_theme_changed, route_watcher_theme_changed,
        route_window_created_theme,
    };
    use crate::{conversion, core, runtime};
    use std::sync::{Arc, Mutex};

    fn resizable() -> core::window::RuntimeState {
        core::window::RuntimeState::default()
    }

    /// The id-less widget request is bound by the dispatcher invocation, never
    /// by focus, opening order, or process-global identity.
    #[test]
    fn current_window_resize_request_binds_to_each_dispatching_id() {
        let first = core::window::Id::unique();
        let second = core::window::Id::unique();

        let directions = [
            core::window::Direction::North,
            core::window::Direction::South,
            core::window::Direction::East,
            core::window::Direction::West,
            core::window::Direction::NorthEast,
            core::window::Direction::NorthWest,
            core::window::Direction::SouthEast,
            core::window::Direction::SouthWest,
        ];
        for id in [second, first] {
            for direction in directions {
                let expected = format!("{direction:?}");
                let action = bind_widget_window_request(
                    id,
                    resizable(),
                    core::window::Request::DragResize(direction),
                );
                match action {
                    Some(runtime::window::Action::DragResize(bound, direction)) => {
                        assert_eq!(bound, id);
                        assert_eq!(format!("{direction:?}"), expected);
                    }
                    _ => panic!("expected DragResize for current id {id:?}"),
                }
            }
        }
    }

    /// Two native windows exercise the same dispatcher used by the event loop,
    /// not only the request-binding helper. The fixture dispatches in reverse
    /// opening order and changes its notional focus between calls; neither may
    /// retarget a shell's request away from the id passed with that UI.
    #[test]
    fn two_window_real_dispatch_resizes_each_shells_exact_id() {
        #[derive(Clone)]
        struct FixtureWindow {
            name: &'static str,
            focused: bool,
            calls: Arc<Mutex<Vec<(&'static str, core::window::Id, core::window::Direction)>>>,
        }

        impl WidgetWindowDispatcher for FixtureWindow {
            fn runtime_state(&self) -> core::window::RuntimeState {
                core::window::RuntimeState::default()
            }

            fn drag_resize(&self, id: core::window::Id, direction: core::window::Direction) {
                self.calls
                    .lock()
                    .expect("fixture dispatch log")
                    .push((self.name, id, direction));
            }
        }

        let first_id = core::window::Id::unique();
        let second_id = core::window::Id::unique();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let first = FixtureWindow {
            name: "first-opened",
            focused: false,
            calls: Arc::clone(&calls),
        };
        let second = FixtureWindow {
            name: "second-opened",
            focused: true,
            calls: Arc::clone(&calls),
        };

        let directions = [
            core::window::Direction::North,
            core::window::Direction::South,
            core::window::Direction::East,
            core::window::Direction::West,
            core::window::Direction::NorthEast,
            core::window::Direction::NorthWest,
            core::window::Direction::SouthEast,
            core::window::Direction::SouthWest,
        ];

        // The focused/latest window dispatches first. Then focus notionally
        // returns to the first-opened window. The dispatcher has no focus or
        // opening-order input: the UI's exact id is the only possible binding.
        assert!(second.focused);
        dispatch_widget_window_requests(
            second_id,
            &second,
            directions
                .into_iter()
                .map(core::window::Request::DragResize),
        );
        assert!(!first.focused);
        dispatch_widget_window_requests(
            first_id,
            &first,
            directions
                .into_iter()
                .rev()
                .map(core::window::Request::DragResize),
        );

        let calls = calls.lock().expect("fixture dispatch log");
        assert_eq!(calls.len(), directions.len() * 2);
        for (name, id, _) in &calls[..directions.len()] {
            assert_eq!(*name, "second-opened");
            assert_eq!(*id, second_id, "second shell was retargeted");
        }
        for (name, id, _) in &calls[directions.len()..] {
            assert_eq!(*name, "first-opened");
            assert_eq!(*id, first_id, "first shell was retargeted");
        }
        assert_eq!(
            calls[..directions.len()]
                .iter()
                .map(|(_, _, direction)| format!("{direction:?}"))
                .collect::<Vec<_>>(),
            directions
                .iter()
                .map(|direction| format!("{direction:?}"))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            calls[directions.len()..]
                .iter()
                .map(|(_, _, direction)| format!("{direction:?}"))
                .collect::<Vec<_>>(),
            directions
                .into_iter()
                .rev()
                .map(|direction| format!("{direction:?}"))
                .collect::<Vec<_>>(),
        );
    }

    /// Native state is the final authority even if a stale widget emits a
    /// request. Returning to ordinary resizable state re-arms every octant.
    #[test]
    fn native_window_state_suppresses_and_rearms_every_resize_octant() {
        let id = core::window::Id::unique();
        let directions = [
            core::window::Direction::North,
            core::window::Direction::South,
            core::window::Direction::East,
            core::window::Direction::West,
            core::window::Direction::NorthEast,
            core::window::Direction::NorthWest,
            core::window::Direction::SouthEast,
            core::window::Direction::SouthWest,
        ];
        let blocked = [
            core::window::RuntimeState {
                resizable: false,
                ..resizable()
            },
            core::window::RuntimeState {
                maximized: true,
                ..resizable()
            },
            core::window::RuntimeState {
                fullscreen: true,
                ..resizable()
            },
        ];

        for direction in directions {
            for state in blocked {
                assert!(
                    bind_widget_window_request(
                        id,
                        state,
                        core::window::Request::DragResize(direction),
                    )
                    .is_none(),
                    "{state:?} must suppress {direction:?}",
                );
            }
            assert!(matches!(
                bind_widget_window_request(
                    id,
                    resizable(),
                    core::window::Request::DragResize(direction),
                ),
                Some(runtime::window::Action::DragResize(bound, _)) if bound == id
            ));
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ThemeWindow {
        mode: core::theme::Mode,
        updates: usize,
        invalidations: usize,
    }

    impl ThemeWindow {
        fn new(mode: core::theme::Mode) -> Self {
            Self {
                mode,
                updates: 0,
                invalidations: 0,
            }
        }
    }

    impl SystemThemeWindow for ThemeWindow {
        fn update_theme(&mut self, theme: winit::window::Theme) {
            self.mode = conversion::theme_mode(theme);
            self.updates += 1;
        }

        fn invalidate_theme_scene(&mut self) {
            self.invalidations += 1;
        }
    }

    #[test]
    fn explicit_light_and_dark_system_theme_startup_modes_are_preserved_without_a_change_event() {
        for (native, expected) in [
            (winit::window::Theme::Light, core::theme::Mode::Light),
            (winit::window::Theme::Dark, core::theme::Mode::Dark),
        ] {
            let mut system_theme = core::theme::Mode::None;
            let mut has_observed_window = false;

            let resolved = route_window_created_theme(
                &mut system_theme,
                &mut has_observed_window,
                Some(native),
            );

            assert_eq!(resolved, expected);
            assert_eq!(system_theme, expected);
            assert!(has_observed_window);
        }
    }

    #[test]
    fn second_window_without_system_theme_preserves_dark_process_state() {
        let mut system_theme = core::theme::Mode::Dark;
        let mut has_observed_window = false;

        assert_eq!(
            route_window_created_theme(
                &mut system_theme,
                &mut has_observed_window,
                Some(winit::window::Theme::Dark),
            ),
            core::theme::Mode::Dark
        );
        assert_eq!(
            route_window_created_theme(&mut system_theme, &mut has_observed_window, None),
            core::theme::Mode::Dark
        );
        assert_eq!(system_theme, core::theme::Mode::Dark);
    }

    #[test]
    fn close_all_and_reopen_does_not_repeat_system_theme_startup_refinement() {
        let mut system_theme = core::theme::Mode::None;
        let mut has_observed_window = false;

        assert_eq!(
            route_window_created_theme(&mut system_theme, &mut has_observed_window, None),
            core::theme::Mode::None
        );

        assert_eq!(
            route_window_created_theme(
                &mut system_theme,
                &mut has_observed_window,
                Some(winit::window::Theme::Dark),
            ),
            core::theme::Mode::None
        );
        assert_eq!(system_theme, core::theme::Mode::None);
    }

    #[test]
    fn native_system_theme_route_broadcasts_and_updates_only_its_target() {
        let mut system_theme = core::theme::Mode::Dark;
        let mut broadcasts = Vec::new();
        let mut windows = [
            ThemeWindow::new(core::theme::Mode::Dark),
            ThemeWindow::new(core::theme::Mode::Dark),
        ];

        route_native_theme_changed(
            &mut system_theme,
            winit::window::Theme::Light,
            &mut windows[1],
            |mode| broadcasts.push(mode),
        );

        assert_eq!(broadcasts, [core::theme::Mode::Light]);
        assert_eq!(system_theme, core::theme::Mode::Light);
        assert_eq!(
            windows,
            [
                ThemeWindow::new(core::theme::Mode::Dark),
                ThemeWindow {
                    mode: core::theme::Mode::Light,
                    updates: 1,
                    invalidations: 0,
                },
            ]
        );
    }

    #[test]
    fn watcher_system_theme_route_updates_and_invalidates_both_windows_in_order() {
        let mut system_theme = core::theme::Mode::Dark;
        let mut broadcasts = Vec::new();
        let mut windows = [
            ThemeWindow::new(core::theme::Mode::Dark),
            ThemeWindow::new(core::theme::Mode::Dark),
        ];

        for mode in [
            core::theme::Mode::Light,
            core::theme::Mode::Light,
            core::theme::Mode::Dark,
        ] {
            route_watcher_theme_changed(&mut system_theme, mode, windows.iter_mut(), |mode| {
                broadcasts.push(mode);
            });
        }

        assert_eq!(
            broadcasts,
            [core::theme::Mode::Light, core::theme::Mode::Dark]
        );
        assert_eq!(system_theme, core::theme::Mode::Dark);
        assert_eq!(
            windows,
            [
                ThemeWindow {
                    mode: core::theme::Mode::Dark,
                    updates: 3,
                    invalidations: 3,
                },
                ThemeWindow {
                    mode: core::theme::Mode::Dark,
                    updates: 3,
                    invalidations: 3,
                },
            ]
        );
    }

    /// The budget clock is read once every [`Clock::INTERVAL`] queries, so the
    /// bound it enforces is "the last interval's elapsed time", never a fresh
    /// `Instant::now()` per message.
    #[test]
    fn the_budget_clock_is_read_once_per_interval() {
        let mut clock = Clock::new();

        std::thread::sleep(Duration::from_millis(5));

        for read in 0..Clock::INTERVAL {
            assert_eq!(
                clock.elapsed(),
                Duration::ZERO,
                "read {read} hit the real clock"
            );
        }

        let elapsed = clock.elapsed();

        assert!(
            elapsed >= Duration::from_millis(5),
            "the clock was never read: {elapsed:?}"
        );

        // And it keeps that value until the next interval.
        assert_eq!(clock.elapsed(), elapsed);

        clock.restart();

        assert_eq!(clock.elapsed(), Duration::ZERO);
    }
}
