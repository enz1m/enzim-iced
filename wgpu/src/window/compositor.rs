// Modified by enzUI contributors. See FORK.md.
//! Connect a window with a renderer.
use crate::core::Color;
use crate::core::backend;
use crate::core::renderer;
use crate::graphics::color;
use crate::graphics::compositor;
use crate::graphics::{self, Antialiasing, Shell, Viewport};
use crate::{Engine, Renderer};

/// A window graphics backend for iced powered by `wgpu`.
pub struct Compositor {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    engine: Engine,
    settings: Settings,
    /// Whether the display is Wayland, where frame callbacks pace UI frames.
    wayland: bool,
    /// Whether `present` measures its backend stages, and the last ones.
    measure_stages: bool,
    present_stages: Option<graphics::lane::PresentStages>,
}

/// A compositor error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The surface creation failed.
    #[error("the surface creation failed: {0}")]
    SurfaceCreationFailed(#[from] wgpu::CreateSurfaceError),
    /// The surface is not compatible.
    #[error("the surface is not compatible")]
    IncompatibleSurface,
    /// No adapter was found for the options requested.
    #[error("no adapter was found for the options requested: {0:?}")]
    NoAdapterFound(String),
    /// No device request succeeded.
    #[error("no device request succeeded: {0:?}")]
    RequestDeviceFailed(Vec<(wgpu::Limits, wgpu::RequestDeviceError)>),
}

impl From<Error> for backend::Error {
    fn from(error: Error) -> Self {
        Self::GraphicsAdapterNotFound {
            backend: "wgpu",
            reason: backend::Reason::RequestFailed(error.to_string()),
        }
    }
}

impl Compositor {
    /// Requests a new [`Compositor`] with the given [`Settings`].
    ///
    /// Returns `None` if no compatible graphics adapter could be found.
    pub async fn request(
        settings: Settings,
        display: impl compositor::Display,
        compatible_window: impl compositor::Window,
        shell: Shell,
    ) -> Result<Self, Error> {
        let wayland = wgpu::rwh::HasDisplayHandle::display_handle(&display)
            .is_ok_and(|handle| matches!(handle.as_raw(), wgpu::rwh::RawDisplayHandle::Wayland(_)));
        let instance = wgpu::util::new_instance_with_webgpu_detection(wgpu::InstanceDescriptor {
            backends: settings.backends,
            flags: if cfg!(feature = "strict-assertions") {
                wgpu::InstanceFlags::debugging()
            } else {
                wgpu::InstanceFlags::empty()
            },
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display))
        })
        .await;

        log::info!("{settings:#?}");

        #[cfg(not(target_arch = "wasm32"))]
        if log::max_level() >= log::LevelFilter::Info {
            let available_adapters: Vec<_> = instance
                .enumerate_adapters(settings.backends)
                .await
                .iter()
                .map(wgpu::Adapter::get_info)
                .collect();
            log::info!("Available adapters: {available_adapters:#?}");
        }

        #[allow(unsafe_code)]
        let compatible_surface = instance
            .create_surface(wgpu::SurfaceTarget::Window(Box::new(compatible_window)))
            .ok();

        let power_preference = match settings.power_preference {
            backend::PowerPreference::None => wgpu::PowerPreference::None,
            backend::PowerPreference::LowPower => wgpu::PowerPreference::LowPower,
            backend::PowerPreference::HighPerformance => wgpu::PowerPreference::HighPerformance,
        };

        let adapter_options = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or(power_preference),
            compatible_surface: compatible_surface.as_ref(),
            force_fallback_adapter: false,
        };

        let adapter = instance
            .request_adapter(&adapter_options)
            .await
            .map_err(|_error| Error::NoAdapterFound(format!("{adapter_options:?}")))?;

        log::info!("Selected: {:#?}", adapter.get_info());

        let (format, alpha_mode) = compatible_surface
            .as_ref()
            .and_then(|surface| {
                let capabilities = surface.get_capabilities(&adapter);

                log::info!("Available formats: {:#?}", capabilities.formats);

                let format = select_surface_format(&capabilities.formats, color::GAMMA_CORRECTION)
                    .map(SurfaceFormat::format);

                let alpha_modes = capabilities.alpha_modes;

                log::info!("Available alpha modes: {alpha_modes:#?}");

                let preferred_alpha =
                    if alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                        wgpu::CompositeAlphaMode::PreMultiplied
                    } else {
                        wgpu::CompositeAlphaMode::Auto
                    };

                format.zip(Some(preferred_alpha))
            })
            .ok_or(Error::IncompatibleSurface)?;

        log::info!("Selected format: {format:?} with alpha mode: {alpha_mode:?}");

        #[cfg(target_arch = "wasm32")]
        let limits = [wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())];

        #[cfg(not(target_arch = "wasm32"))]
        let limits = [
            wgpu::Limits::default().using_resolution(adapter.limits()),
            wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ];

        let limits = limits.into_iter().map(|limits| wgpu::Limits {
            max_non_sampler_bindings: 2048,
            ..limits
        });

        // Request SHADER_F16 only if the adapter supports it (e.g., not available in WebGL2)
        let required_features = if adapter.features().contains(wgpu::Features::SHADER_F16) {
            wgpu::Features::SHADER_F16
        } else {
            wgpu::Features::empty()
        };

        let mut errors = Vec::new();

        for required_limits in limits {
            let result = crate::device::request(
                &adapter,
                &wgpu::DeviceDescriptor {
                    label: Some("iced_wgpu::window::compositor device descriptor"),
                    required_features,
                    required_limits: required_limits.clone(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                },
            )
            .await;

            match result {
                Ok((device, queue)) => {
                    let engine = Engine::new(
                        &adapter,
                        device,
                        queue,
                        format,
                        settings.antialiasing,
                        shell,
                    );

                    lose_device_on_request(&engine);

                    return Ok(Compositor {
                        instance,
                        adapter,
                        format,
                        alpha_mode,
                        engine,
                        settings,
                        wayland,
                        measure_stages: false,
                        present_stages: None,
                    });
                }
                Err(error) => {
                    errors.push((required_limits, error));
                }
            }
        }

        Err(Error::RequestDeviceFailed(errors))
    }
}

/// Loses the device after `ICED_DEBUG_DEVICE_LOSS_AFTER_MS` milliseconds,
/// when set, so recovery from a lost device can be exercised in a running
/// application without starving the GPU.
fn lose_device_on_request(engine: &Engine) {
    let Some(after) = std::env::var("ICED_DEBUG_DEVICE_LOSS_AFTER_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(std::time::Duration::from_millis)
    else {
        return;
    };
    let device = engine.device.clone();
    let _ = std::thread::Builder::new()
        .name("iced-debug-device-loss".into())
        .spawn(move || {
            std::thread::sleep(after);
            log::warn!("ICED_DEBUG_DEVICE_LOSS_AFTER_MS: destroying the graphics device");
            device.destroy();
        });
}

/// The half of a [`Compositor`] a render lane needs (enzICED #2219).
///
/// It is deliberately *not* the compositor: the UI thread keeps that, so
/// creating surfaces, loading fonts, taking screenshots and querying adapter
/// information never wait on the lane. What the lane owns is what only it
/// touches -- the device handle it configures the surface with, and the surface
/// format -- plus a second [`Renderer`] over the same device.
struct LaneBackend {
    device: wgpu::Device,
    /// Asked whether the lane's surface accepts copies (enzUI-ICED #3666).
    adapter: wgpu::Adapter,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    present_mode: wgpu::PresentMode,
    /// Whether the display is Wayland; see [`surface_present_mode`].
    wayland: bool,
    /// The lane's OWN renderer: a second renderer over the same device, never
    /// shared with the UI thread (enzICED #2219).
    renderer: Renderer,
    /// Acquire/prepare/submit/present spans of the last successful present,
    /// taken exactly once by the worker (enzICED #2219/#2250).
    stages: Option<graphics::lane::PresentStages>,
    /// Whether the worker asked for those spans at all (enzICED #2250).
    ///
    /// `false` on a default build, and then `present` takes no timestamp.
    measure: bool,
    /// Checks every partial frame against a full one, when asked to.
    check: Option<DamageCheck>,
}

/// Paints every lane frame again in full with a second renderer and logs
/// the pixels a partial frame got wrong (enzUI-ICED #3666).
///
/// `ICED_LANE_DAMAGE_CHECK=<file>` turns it on; each partial frame appends
/// a line with how many pixels differ and where.
struct DamageCheck {
    renderer: Renderer,
    log: std::fs::File,
    target: Option<wgpu::Texture>,
    frames: u64,
    partial_frames: u64,
}

impl DamageCheck {
    fn from_env(engine: &crate::Engine, settings: renderer::Settings) -> Option<Self> {
        let path = std::env::var_os("ICED_LANE_DAMAGE_CHECK")?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self {
            renderer: Renderer::new_presentation(engine.clone(), settings),
            log,
            target: None,
            frames: 0,
            partial_frames: 0,
        })
    }

    fn replay(
        &mut self,
        viewport: &Viewport,
        scene: &crate::core::renderer::Recording,
        tracks: &[crate::core::renderer::Track],
    ) {
        use crate::core::Renderer as _;

        self.renderer
            .reset(crate::core::Rectangle::with_size(viewport.logical_size()));
        let _ = self.renderer.replay_tracks(scene, tracks);
    }

    /// Compares the lane's canvas, if its last frame was partial, with the
    /// same frame painted in full.
    fn compare(&mut self, lane: &Renderer, viewport: &Viewport, background: Color) {
        use std::io::Write as _;

        self.frames += 1;
        if lane.partial_frames() == self.partial_frames {
            return;
        }
        self.partial_frames = lane.partial_frames();
        let Some(canvas) = lane.canvas_texture() else {
            return;
        };
        let device = &lane.engine.device;
        let queue = &lane.engine.queue;
        let target = self
            .target
            .take()
            .filter(|target| target.size() == canvas.size() && target.format() == canvas.format())
            .unwrap_or_else(|| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("iced_wgpu damage check"),
                    size: canvas.size(),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: canvas.format(),
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let _ = self
            .renderer
            .present(Some(background), canvas.format(), &view, viewport);
        let partial = crate::damage::read_pixels(device, queue, canvas);
        let full = crate::damage::read_pixels(device, queue, &target);
        let (differing, span) = crate::damage::difference(canvas.width(), &partial, &full);
        let _ = writeln!(
            self.log,
            "frame {} partial {} differing {differing} span {span:?}",
            self.frames, self.partial_frames
        );
        self.target = Some(target);
    }
}

impl std::fmt::Debug for LaneBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Renderer` is not `Debug`, and the lane's renderer is precisely the
        // thing no other thread may inspect anyway.
        f.debug_struct("LaneBackend")
            .field("format", &self.format)
            .field("alpha_mode", &self.alpha_mode)
            .field("present_mode", &self.present_mode)
            .field("renderer", &"<lane-owned>")
            .finish()
    }
}

impl graphics::lane::Backend for LaneBackend {
    type Surface = wgpu::Surface<'static>;

    fn configure(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        if width == 0 || height == 0 || self.renderer.device_lost() {
            return;
        }

        surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: surface_usage(surface, &self.adapter),
                format: self.format,
                present_mode: surface_present_mode(
                    self.present_mode,
                    self.wayland,
                    surface,
                    &self.adapter,
                ),
                width,
                height,
                alpha_mode: self.alpha_mode,
                view_formats: vec![],
                // enzICED #2219: the LANE's swapchain, not the UI thread's.
                //
                // A blocked UI thread must not starve the lane's next image.
                // With a latency of 1 the lane can only ever have one frame in
                // flight, so it waits for the compositor to release the single
                // image before it can draw the next value -- measured as a
                // 101.5 ms hole in the lane's own presented-frame trace,
                // exactly spanning the deterministic UI stall.
                desired_maximum_frame_latency: 3,
            },
        );
    }

    fn replay(
        &mut self,
        viewport: &Viewport,
        scene: &crate::core::renderer::Recording,
        tracks: &[crate::core::renderer::Track],
    ) -> bool {
        use crate::core::Renderer as _;

        // Replaying installs the scene's media on the device; a lost one only
        // yields invalid textures, and the lane stops at its next present.
        if self.renderer.device_lost() {
            return false;
        }
        if let Some(check) = &mut self.check {
            check.replay(viewport, scene, tracks);
        }
        self.renderer
            .reset(crate::core::Rectangle::with_size(viewport.logical_size()));
        self.renderer.replay_tracks(scene, tracks)
    }

    fn retire_resources(&mut self) {
        self.renderer.retire_presentation_resources();
        self.stages = None;
    }

    fn layout_sample(
        &mut self,
    ) -> Result<
        Option<std::sync::Arc<crate::core::renderer::presentation::Sample>>,
        crate::core::renderer::presentation::Error,
    > {
        self.renderer.layout_sample()
    }

    fn replay_refusal(&self) -> Option<crate::core::renderer::presentation::Error> {
        use crate::core::Renderer as _;
        self.renderer.presentation_refusal()
    }

    fn present(
        &mut self,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background: Color,
        on_pre_present: &mut dyn FnMut(),
    ) -> Result<(), compositor::SurfaceError> {
        // The lane worker's frames reach the SAME pre-commit slot the UI
        // thread's do, so a compositor-feedback request is issued from the
        // committing thread, before its own commit, on both paths -- and on
        // neither when acquisition failed (enzICED #2219). This used to be
        // `|| {}`, which is why lane frames could never be more than
        // `SurfacePresented`.
        if self
            .renderer
            .validate_owned_media_viewport(viewport)
            .is_err()
        {
            return Err(compositor::SurfaceError::Other);
        }
        self.stages = None;

        let presented = present_timed(
            &mut self.renderer,
            surface,
            viewport,
            background,
            self.measure.then_some(&mut self.stages),
            || {
                on_pre_present();
            },
        );
        if presented.is_ok()
            && let Some(check) = &mut self.check
        {
            check.compare(&self.renderer, viewport, background);
        }
        presented
    }

    fn presentation_quality(&self) -> compositor::PresentationQuality {
        // Exactly the milestone `present` above reaches: the surface present
        // call returned. It is NOT compositor feedback and must never be
        // reported as one.
        compositor::PresentationQuality::SurfacePresented
    }

    fn measure_stages(&mut self, enabled: bool) {
        self.measure = enabled;

        if !enabled {
            self.stages = None;
        }
    }

    fn take_present_stages(&mut self) -> Option<graphics::lane::PresentStages> {
        self.stages.take()
    }
}

/// Creates a [`Compositor`] with the given [`Settings`] and window.
pub async fn new(
    settings: Settings,
    display: impl compositor::Display,
    compatible_window: impl compositor::Window,
    shell: Shell,
) -> Result<Compositor, Error> {
    let Some((primary, rest)) = gl_last(settings.backends) else {
        return Compositor::request(settings, display, compatible_window, shell).await;
    };
    let display = std::sync::Arc::new(display);
    let window = std::sync::Arc::new(compatible_window);
    let first = Settings {
        backends: primary,
        ..settings
    };
    match Compositor::request(first, display.clone(), window.clone(), shell.clone()).await {
        Ok(compositor) => Ok(compositor),
        Err(error) => {
            log::info!("No primary graphics backend ({error}); trying {rest:?}");
            let last = Settings {
                backends: rest,
                ..settings
            };
            Compositor::request(last, display, window, shell).await
        }
    }
}

/// Splits `backends` into the ones to try first and OpenGL, when it holds
/// both.
///
/// Creating an instance with OpenGL enabled loads the system GL driver
/// (Mesa's gallium, tens of MiB per process) even when Vulkan is the adapter
/// chosen, so OpenGL is tried only when no other backend yields a device.
fn gl_last(backends: wgpu::Backends) -> Option<(wgpu::Backends, wgpu::Backends)> {
    let primary = backends - wgpu::Backends::GL;
    (backends.contains(wgpu::Backends::GL) && !primary.is_empty())
        .then_some((primary, wgpu::Backends::GL))
}

/// Presents the given primitives with the given [`Compositor`].
pub fn present(
    renderer: &mut Renderer,
    surface: &mut wgpu::Surface<'static>,
    viewport: &Viewport,
    background_color: Color,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    present_timed(
        renderer,
        surface,
        viewport,
        background_color,
        None,
        on_pre_present,
    )
}

/// [`present`], additionally reporting its four backend-internal spans.
///
/// enzICED #2219: from the render lane, `present` is one opaque call. Only here
/// can *waiting for a swapchain image* be told apart from *preparing and
/// encoding commands*, from *handing them to the queue*, from *requesting
/// presentation*, and the isolation claim needs them separate -- a
/// compositor-paced acquire and a slow submit look identical in a total.
///
/// enzICED #2250: preparation and encoding used to run inside the span labelled
/// `Submit`. `Prepare` now covers exactly the real wgpu preparation/encoding
/// and `Submit` exactly the queue submission; the surface commit is its own
/// `Present` span. The four are adjacent and never overlap.
///
/// `stages` stays untouched on every error path, because a frame that was never
/// committed has no submit span to report; passing `None` takes no timestamp at
/// all, which is what the UI thread's [`present`] does.
pub fn present_timed(
    renderer: &mut Renderer,
    surface: &mut wgpu::Surface<'static>,
    viewport: &Viewport,
    background_color: Color,
    stages: Option<&mut Option<graphics::lane::PresentStages>>,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    // Acquiring from a surface whose configuration failed with the device panics
    // inside wgpu; the poll delivers a pending loss callback first.
    if renderer.confirm_device_lost() {
        return Err(compositor::SurfaceError::Lost);
    }
    let measure = stages.is_some();

    // The device can be lost during the frame (a driver allocation failure
    // loses it). wgpu then panics on the first object created from it. Only a
    // panic the loss explains becomes `Lost`; any other is raised again.
    let frame = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        present_frame(
            renderer,
            surface,
            viewport,
            background_color,
            stages,
            on_pre_present,
            measure,
        )
    }));
    match frame {
        Ok(result) => result,
        Err(panic) if renderer.confirm_device_lost() => {
            drop(panic);
            log::warn!(
                "Dropped a frame the graphics device was lost in; the panic above is that frame"
            );
            Err(compositor::SurfaceError::Lost)
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// The frame [`present_timed`] draws, while the device is healthy.
fn present_frame(
    renderer: &mut Renderer,
    surface: &mut wgpu::Surface<'static>,
    viewport: &Viewport,
    background_color: Color,
    stages: Option<&mut Option<graphics::lane::PresentStages>>,
    on_pre_present: impl FnOnce(),
    measure: bool,
) -> Result<(), compositor::SurfaceError> {
    let acquire_started = measure.then(std::time::Instant::now);

    match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) => {
            let acquire_ended = measure.then(std::time::Instant::now);

            // enzUI-ICED #3666: a surface that accepts copies is presented
            // from the canvas the lane keeps, which a frame repaints only where
            // it changed.
            let mut present_damage = None;
            let (_submission, encode) = if renderer.tracks_damage()
                && frame
                    .texture
                    .usage()
                    .contains(wgpu::TextureUsages::COPY_DST)
            {
                let partial_frames = renderer.partial_frames();
                let snapshot =
                    (!measure && renderer.tracks_frame_damage() && frame_damage_log().is_some())
                        .then(|| renderer.layer_snapshot());
                let presented =
                    renderer.present_damaged(background_color, &frame.texture, viewport, measure);
                present_damage = renderer.take_present_damage();
                if let Some(snapshot) = snapshot
                    && renderer.partial_frames() != partial_frames
                {
                    check_frame_damage(renderer, snapshot, viewport, background_color);
                }
                presented
            } else {
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                renderer.present_measured(Some(background_color), &view, viewport, measure)
            };

            let present_started = measure.then(std::time::Instant::now);

            renderer.dump_presented(&frame.texture, measure);

            // enzUI: the compositor is told only what changed, so it repaints
            // and recomposites (glass behind the window, its blur) no more.
            #[cfg(feature = "vulkan")]
            wgpu::hal::vulkan::set_present_damage(present_damage);
            #[cfg(not(feature = "vulkan"))]
            let _ = present_damage;

            // Present the frame
            on_pre_present();
            frame.present();

            if let (
                Some(stages),
                Some(acquire_started),
                Some(acquire_ended),
                Some(encode),
                Some(present_started),
            ) = (
                stages,
                acquire_started,
                acquire_ended,
                encode,
                present_started,
            ) {
                *stages = Some(graphics::lane::PresentStages {
                    acquire: (acquire_started, acquire_ended),
                    prepare: encode.prepare,
                    submit: encode.submit,
                    present: (present_started, std::time::Instant::now()),
                });
            }

            Ok(())
        }
        failed => {
            // Damage measured ahead of this present describes a frame the
            // canvas never received.
            renderer.discard_frame_damage();
            Err(match failed {
                wgpu::CurrentSurfaceTexture::Suboptimal(_)
                | wgpu::CurrentSurfaceTexture::Outdated => compositor::SurfaceError::Outdated,
                wgpu::CurrentSurfaceTexture::Timeout => compositor::SurfaceError::Timeout,
                wgpu::CurrentSurfaceTexture::Occluded => compositor::SurfaceError::Occluded,
                wgpu::CurrentSurfaceTexture::Lost => compositor::SurfaceError::Lost,
                _ => compositor::SurfaceError::Other,
            })
        }
    }
}

/// Where `ICED_UI_DAMAGE_CHECK=<file>` logs partial frames drawn by widgets,
/// if it is set (enzUI-ICED #3669).
fn frame_damage_log() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static LOG: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("ICED_UI_DAMAGE_CHECK")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(std::sync::Mutex::new)
    })
    .as_ref()
}

/// Paints `snapshot`, the layers of a partial frame drawn by widgets as they
/// were before it was drawn, in full and logs the pixels the partial frame
/// got wrong: how many differ and where.
fn check_frame_damage(
    renderer: &mut Renderer,
    snapshot: crate::layer::Stack,
    viewport: &Viewport,
    background: Color,
) {
    use std::io::Write as _;

    let Some(log) = frame_damage_log() else {
        return;
    };
    let Some(canvas) = renderer.canvas_texture().cloned() else {
        return;
    };
    let device = renderer.engine.device.clone();
    let queue = renderer.engine.queue.clone();
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("iced_wgpu frame damage check"),
        size: canvas.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: canvas.format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    if !renderer.present_snapshot(snapshot, background, canvas.format(), &view, viewport) {
        // The second renderer is still loading an image this frame shows.
        if let Ok(mut log) = log.lock() {
            let _ = writeln!(
                log,
                "ui partial {} unchecked: image loading",
                renderer.partial_frames()
            );
        }
        return;
    }
    let partial = crate::damage::read_pixels(&device, &queue, &canvas);
    let full = crate::damage::read_pixels(&device, &queue, &target);
    let (differing, span) = crate::damage::difference(canvas.width(), &partial, &full);
    if let Ok(mut log) = log.lock() {
        let _ = writeln!(
            log,
            "ui partial {} differing {differing} span {span:?}",
            renderer.partial_frames()
        );
    }
}

impl graphics::Compositor for Compositor {
    type Renderer = Renderer;
    type Surface = wgpu::Surface<'static>;

    async fn new(
        settings: backend::Settings,
        display: impl compositor::Display,
        compatible_window: impl compositor::Window,
        shell: Shell,
    ) -> Result<Self, backend::Error> {
        if settings.backend.hardware().is_none() && !settings.backend.matches("wgpu") {
            return Err(backend::Error::GraphicsAdapterNotFound {
                backend: "wgpu",
                reason: backend::Reason::DidNotMatch {
                    preferred_backend: settings.backend,
                },
            });
        }

        let mut settings = Settings::from(settings);

        if let Some(backends) = wgpu::Backends::from_env() {
            settings.backends = backends;
        }

        if let Some(present_mode) = present_mode_from_env() {
            settings.present_mode = present_mode;
        }

        Ok(new(settings, display, compatible_window, shell).await?)
    }

    fn device_lost(&self) -> bool {
        self.engine.device_lost()
    }

    fn emulates_gpu(&self) -> bool {
        self.adapter.get_info().device_type == wgpu::DeviceType::Cpu
    }

    fn create_renderer(&self, settings: renderer::Settings) -> Self::Renderer {
        let mut renderer = Renderer::new(self.engine.clone(), settings);
        renderer.track_frame_damage(std::env::var_os("ICED_DISABLE_UI_DAMAGE").is_none());
        renderer
    }

    fn animation_lane_backend(
        &self,
        renderer_settings: renderer::Settings,
    ) -> Option<Box<dyn graphics::lane::Backend<Surface = Self::Surface>>> {
        if std::env::var("ICED_DISABLE_ANIMATION_LANE").is_ok() {
            // An explicit escape hatch, so a lane defect can be bisected in the
            // field without rebuilding: the runtime then animates on the UI
            // path exactly as it did before.
            return None;
        }

        let mut renderer = Renderer::new_presentation(self.engine.clone(), renderer_settings);
        renderer.track_damage(std::env::var_os("ICED_DISABLE_LANE_DAMAGE").is_none());
        let check = DamageCheck::from_env(&self.engine, renderer_settings);

        Some(Box::new(LaneBackend {
            device: self.engine.device.clone(),
            adapter: self.adapter.clone(),
            format: self.format,
            alpha_mode: self.alpha_mode,
            present_mode: self.settings.present_mode,
            wayland: self.wayland,
            renderer,
            stages: None,
            // enzICED #2250: off until the worker is handed a stage ledger.
            measure: false,
            check,
        }))
    }

    fn create_surface(
        &mut self,
        window: impl compositor::Window,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let mut surface = self
            .instance
            .create_surface(wgpu::SurfaceTarget::Window(Box::new(window)))
            .expect("Create surface");

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        // Configuring with a lost device fails without a word and leaves the
        // surface unconfigured; the rebuilt compositor configures it again.
        if self.engine.device_lost() {
            return;
        }
        surface.configure(
            &self.engine.device,
            &wgpu::SurfaceConfiguration {
                usage: surface_usage(surface, &self.adapter),
                format: self.format,
                present_mode: surface_present_mode(
                    self.settings.present_mode,
                    self.wayland,
                    surface,
                    &self.adapter,
                ),
                width,
                height,
                alpha_mode: self.alpha_mode,
                view_formats: vec![],
                desired_maximum_frame_latency: 1,
            },
        );
    }

    fn information(&self) -> compositor::Information {
        let information = self.adapter.get_info();

        compositor::Information {
            adapter: information.name,
            backend: format!("{:?}", information.backend),
            device: format!(
                "vendor={:#06x};device={:#06x};type={:?};driver={};driver_info={}",
                information.vendor,
                information.device,
                information.device_type,
                information.driver,
                information.driver_info,
            ),
        }
    }

    fn presentation_quality(&self) -> compositor::PresentationQuality {
        // `present` below returns only after `SurfaceTexture::present`; this is
        // stronger than queue submission but is not compositor feedback.
        compositor::PresentationQuality::SurfacePresented
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        self.present_stages = None;
        present_timed(
            renderer,
            surface,
            viewport,
            background_color,
            self.measure_stages.then_some(&mut self.present_stages),
            on_pre_present,
        )
    }

    fn unchanged(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> bool {
        renderer.unchanged_frame(background_color, viewport)
    }

    fn measure_present_stages(&mut self, enabled: bool) {
        self.measure_stages = enabled;
        if !enabled {
            self.present_stages = None;
        }
    }

    fn take_present_stages(&mut self) -> Option<graphics::lane::PresentStages> {
        self.present_stages.take()
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        renderer.screenshot(viewport, background_color)
    }
}

/// The settings of a [`Compositor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The present mode of the [`Renderer`].
    ///
    /// [`Renderer`]: crate::Renderer
    pub present_mode: wgpu::PresentMode,

    /// The graphics backends to use.
    pub backends: wgpu::Backends,

    /// The power-usage preference for graphics adapters.
    ///
    /// By default, it is [`backend::PowerPreference::None`].
    pub power_preference: backend::PowerPreference,

    /// The antialiasing strategy that will be used for triangle primitives.
    ///
    /// By default, it is `None`.
    pub antialiasing: Option<Antialiasing>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            present_mode: wgpu::PresentMode::AutoVsync,
            backends: wgpu::Backends::all(),
            power_preference: backend::PowerPreference::None,
            antialiasing: None,
        }
    }
}

impl From<backend::Settings> for Settings {
    fn from(settings: backend::Settings) -> Self {
        let backends = settings
            .backend
            .hardware()
            .map(|api| match api {
                backend::Api::Best => wgpu::Backends::all(),
                backend::Api::Vulkan => wgpu::Backends::VULKAN,
                backend::Api::Metal => wgpu::Backends::METAL,
                backend::Api::DirectX12 => wgpu::Backends::DX12,
                backend::Api::OpenGL => wgpu::Backends::GL,
                backend::Api::WebGPU => wgpu::Backends::BROWSER_WEBGPU,
            })
            .unwrap_or_else(wgpu::Backends::all);

        Self {
            present_mode: if settings.vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            antialiasing: settings.antialiasing.then_some(Antialiasing::MSAAx4),
            backends,
            power_preference: settings.power_preference,
        }
    }
}

/// Surface usage: copyable as well when frames are dumped.
/// What a window's frames are used for: drawn to, copied into from the
/// render lane's kept canvas where the surface allows it (enzUI-ICED #3666,
/// `ICED_DISABLE_LANE_DAMAGE` turns it off), and copied out of for frame
/// dumps.
fn surface_usage(surface: &wgpu::Surface<'_>, adapter: &wgpu::Adapter) -> wgpu::TextureUsages {
    let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
    if std::env::var_os("ICED_DISABLE_LANE_DAMAGE").is_none()
        && surface
            .get_capabilities(adapter)
            .usages
            .contains(wgpu::TextureUsages::COPY_DST)
    {
        usage |= wgpu::TextureUsages::COPY_DST;
    }
    if crate::frame_dump::enabled() {
        usage |= wgpu::TextureUsages::COPY_SRC;
    }
    usage
}

/// The present mode a surface is configured with.
///
/// Under vsync on Wayland a surface presents in mailbox mode where it offers
/// it. The lane paces its frames from presentation feedback and the UI from
/// frame callbacks, so FIFO's own throttling adds only a present that blocks
/// until the previous frame's callback, and the presenting thread wakes for
/// every Wayland event the compositor sends meanwhile. The UI and the lane
/// share the surface and configure it alike, so handing it over never
/// rebuilds the swapchain. An explicit `ICED_PRESENT_MODE` is kept as given.
fn surface_present_mode(
    requested: wgpu::PresentMode,
    wayland: bool,
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
) -> wgpu::PresentMode {
    present_mode_for(
        requested,
        wayland && std::env::var_os("ICED_PRESENT_MODE").is_none(),
        || {
            surface
                .get_capabilities(adapter)
                .present_modes
                .contains(&wgpu::PresentMode::Mailbox)
        },
    )
}

/// `requested`, or mailbox in its place when vsync may use it and the
/// surface offers it.
fn present_mode_for(
    requested: wgpu::PresentMode,
    may_use_mailbox: bool,
    offers_mailbox: impl FnOnce() -> bool,
) -> wgpu::PresentMode {
    if may_use_mailbox && requested == wgpu::PresentMode::AutoVsync && offers_mailbox() {
        wgpu::PresentMode::Mailbox
    } else {
        requested
    }
}

/// Obtains a [`wgpu::PresentMode`] from the current environment
/// configuration, if set.
///
/// The value returned by this function can be changed by setting
/// the `ICED_PRESENT_MODE` env variable. The possible values are:
///
/// - `vsync` → [`wgpu::PresentMode::AutoVsync`]
/// - `no_vsync` → [`wgpu::PresentMode::AutoNoVsync`]
/// - `immediate` → [`wgpu::PresentMode::Immediate`]
/// - `fifo` → [`wgpu::PresentMode::Fifo`]
/// - `fifo_relaxed` → [`wgpu::PresentMode::FifoRelaxed`]
/// - `mailbox` → [`wgpu::PresentMode::Mailbox`]
pub fn present_mode_from_env() -> Option<wgpu::PresentMode> {
    let present_mode = std::env::var("ICED_PRESENT_MODE").ok()?;

    match present_mode.to_lowercase().as_str() {
        "vsync" => Some(wgpu::PresentMode::AutoVsync),
        "no_vsync" => Some(wgpu::PresentMode::AutoNoVsync),
        "immediate" => Some(wgpu::PresentMode::Immediate),
        "fifo" => Some(wgpu::PresentMode::Fifo),
        "fifo_relaxed" => Some(wgpu::PresentMode::FifoRelaxed),
        "mailbox" => Some(wgpu::PresentMode::Mailbox),
        _ => None,
    }
}

/// The outcome of [`select_surface_format`]: which format was chosen, and
/// whether it came from our allowlist or from the fallback.
///
/// The provenance is carried in the type rather than inferred from the format
/// alone, because on ordinary hardware the fallback's answer and the
/// allowlist's answer are usually the *same* format. A test that only compared
/// formats could therefore not tell "the allowlist worked" from "the allowlist
/// was skipped entirely and the fallback happened to agree" -- which is
/// precisely the regression this design risks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceFormat {
    /// A format from our SDR allowlist was available and chosen.
    Approved(wgpu::TextureFormat),
    /// No approved format was available; this is the adapter's first reported
    /// format, taken so a window can still open. Colours may be wrong.
    Fallback(wgpu::TextureFormat),
}

impl SurfaceFormat {
    /// The chosen texture format, regardless of provenance.
    fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Approved(format) | Self::Fallback(format) => format,
        }
    }
}

/// Picks a surface format from an adapter's capabilities, preferring SDR.
///
/// enzICED patch (upstream iced #3390): the original logic filtered a
/// hand-written blacklist of exotic formats and then took the **first**
/// remaining non-sRGB (or sRGB, in gamma-correct mode) format the driver
/// reported. `Rgba16Float` has `required_features() == Features::empty()` and
/// is non-sRGB, so an HDR-capable driver that reports it early yielded
/// **extended-linear output for SDR UI**.
///
/// The replacement is an allowlist, order-independent by construction: the
/// *preference order* is fixed in code, not derived from the driver's
/// reporting order. For the web-colors path (SDR premultiplied), `Bgra8Unorm`
/// then `Rgba8Unorm`; for the gamma-correct path, the sRGB variants of the
/// same two.
///
/// # When nothing is approved
///
/// The adapter's **first reported format** is taken and a `warn` is logged.
/// This deliberately matches upstream's fallback shape: slightly-wrong colours
/// are recoverable, a window that refuses to open is not. An earlier revision
/// of this patch returned `None` here and let the call site raise
/// [`Error::IncompatibleSurface`]; that traded a cosmetic fault for a fatal
/// one and was reversed.
///
/// The fallback lives **inside** this function rather than at the call site
/// specifically so it stays testable without a GPU -- the call site only has a
/// real `wgpu::Surface` to work with. [`SurfaceFormat`] reports which branch
/// fired.
///
/// Returns `None` only when the adapter reports **no formats at all**, which
/// leaves nothing to fall back to; the call site maps that to
/// [`Error::IncompatibleSurface`].
fn select_surface_format(
    formats: &[wgpu::TextureFormat],
    gamma_correction: bool,
) -> Option<SurfaceFormat> {
    // Order matters: the *first* candidate that appears in `formats` wins.
    // This is why the loop below is written as `for candidate in preferred`
    // rather than iterating `formats` -- we pick by our preference, not by
    // the driver's.
    let preferred: &[wgpu::TextureFormat] = if gamma_correction {
        &[
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ]
    } else {
        &[
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
    };
    for candidate in preferred {
        if formats.contains(candidate) {
            log::info!(
                "Selected SDR surface format: {candidate:?} \
                 (gamma_correction={gamma_correction})"
            );
            return Some(SurfaceFormat::Approved(*candidate));
        }
    }

    // Nothing approved. Fall back to whatever the adapter listed first so a
    // window can still open, and say so loudly enough to be actionable: this
    // warning is the only diagnostic anyone will have when a screenshot comes
    // back looking washed out.
    let fallback = formats.first().copied()?;

    log::warn!(
        "No SDR surface format supported by this adapter. Falling back to \
         {fallback:?}, the first format reported; colours may be rendered \
         incorrectly (for example washed out on an HDR-capable surface). \
         Wanted one of {preferred:?} (gamma_correction={gamma_correction}); \
         adapter reported {formats:?}. Please report this along with your GPU \
         and driver version."
    );

    Some(SurfaceFormat::Fallback(fallback))
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`select_surface_format`].
    //!
    //! The whole point of the refactor is order-independence, so every
    //! preference-order test **shuffles** the capability list. A single
    //! fixed input order tests nothing.
    //!
    //! Note the two distinct senses of "fall back" here, kept apart in the
    //! test names deliberately:
    //!
    //! * choosing the *second* entry of our own allowlist because the first
    //!   is unavailable -- still an [`SurfaceFormat::Approved`] outcome, no
    //!   warning. These tests say `uses_..._when_..._missing`.
    //! * abandoning the allowlist entirely and taking the adapter's first
    //!   reported format -- [`SurfaceFormat::Fallback`], and it warns. These
    //!   tests say `falls_back_...`.
    use super::{SurfaceFormat, select_surface_format};
    use std::cell::Cell;
    use std::sync::{Mutex, Once};
    use wgpu::TextureFormat as F;

    // --- log capture ------------------------------------------------------
    //
    // "The fallback warned" and "the approved path did *not* warn" are both
    // load-bearing claims, so they are asserted against real captured `log`
    // records rather than assumed from the return value.
    //
    // Records are tagged with the emitting thread and dropped unless that
    // thread is actively capturing, so parallel tests cannot pollute each
    // other's capture and the 1440-permutation tests do not accumulate
    // megabytes of unread `info!` strings.

    thread_local! {
        static CAPTURING: Cell<bool> = const { Cell::new(false) };
    }

    static RECORDS: Mutex<Vec<(std::thread::ThreadId, log::Level, String)>> =
        Mutex::new(Vec::new());

    struct CaptureLogger;

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            CAPTURING.with(Cell::get)
        }

        fn log(&self, record: &log::Record<'_>) {
            if !CAPTURING.with(Cell::get) {
                return;
            }
            RECORDS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((
                    std::thread::current().id(),
                    record.level(),
                    record.args().to_string(),
                ));
        }

        fn flush(&self) {}
    }

    static LOGGER: CaptureLogger = CaptureLogger;
    static INSTALL: Once = Once::new();

    /// Runs `f` with `log` capture enabled, returning the records `f` emitted
    /// **on this thread**.
    fn capture_logs(f: impl FnOnce()) -> Vec<(log::Level, String)> {
        INSTALL.call_once(|| {
            // A failure here means something else already installed a logger;
            // `capture_is_wired_up` is the calibration that catches it rather
            // than letting the log assertions pass vacuously.
            let _ = log::set_logger(&LOGGER);
            log::set_max_level(log::LevelFilter::Trace);
        });

        let me = std::thread::current().id();
        CAPTURING.with(|c| c.set(true));
        f();
        CAPTURING.with(|c| c.set(false));

        let records = RECORDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records
            .iter()
            .filter(|(thread, _, _)| *thread == me)
            .map(|(_, level, message)| (*level, message.clone()))
            .collect()
    }

    fn warnings(records: &[(log::Level, String)]) -> Vec<&str> {
        records
            .iter()
            .filter(|(level, _)| *level == log::Level::Warn)
            .map(|(_, message)| message.as_str())
            .collect()
    }

    /// Positive control for the capture harness itself.
    ///
    /// Without this, a broken logger installation would make every
    /// "did not warn" assertion below pass for the wrong reason.
    #[test]
    fn capture_is_wired_up() {
        let records = capture_logs(|| {
            log::warn!("canary");
        });

        assert!(
            warnings(&records).iter().any(|w| w.contains("canary")),
            "log capture is not working, so the warn/no-warn assertions in \
             this module would be vacuous; got {records:?}"
        );
    }

    /// All permutations of `caps`, deterministically enumerated.
    fn permutations(caps: &[F]) -> Vec<Vec<F>> {
        fn go(head: &mut Vec<F>, rest: &[F], out: &mut Vec<Vec<F>>) {
            if rest.is_empty() {
                out.push(head.clone());
                return;
            }
            for i in 0..rest.len() {
                let mut next: Vec<F> = rest.to_vec();
                let picked = next.remove(i);
                head.push(picked);
                go(head, &next, out);
                let _ = head.pop();
            }
        }
        let mut out = Vec::new();
        go(&mut Vec::new(), caps, &mut out);
        assert!(!out.is_empty());
        out
    }

    /// Every permutation of `caps` yields the same **approved** format.
    fn assert_order_independent(caps: &[F], gamma_correction: bool, expected: F) {
        for perm in permutations(caps) {
            let got = select_surface_format(&perm, gamma_correction);
            assert_eq!(
                got,
                Some(SurfaceFormat::Approved(expected)),
                "selection changed with input order: caps={perm:?} \
                 gamma_correction={gamma_correction} got={got:?}"
            );
        }
    }

    /// Every permutation of `caps` abandons the allowlist and takes whichever
    /// format that permutation reports **first**.
    ///
    /// The chosen format necessarily varies with order here -- that is what a
    /// fallback is. The order-independent property being asserted is the
    /// *rule*: always the first reported, never an allowlist entry, always
    /// flagged as `Fallback`.
    fn assert_falls_back_for_every_order(caps: &[F], gamma_correction: bool) {
        for perm in permutations(caps) {
            let got = select_surface_format(&perm, gamma_correction);
            assert_eq!(
                got,
                Some(SurfaceFormat::Fallback(perm[0])),
                "expected fallback to the first reported format: caps={perm:?} \
                 gamma_correction={gamma_correction} got={got:?}"
            );
        }
    }

    // --- web-colors (GAMMA_CORRECTION == false) ---------------------------

    #[test]
    fn web_colors_prefers_bgra8_unorm_over_rgba8_unorm() {
        // Both approved formats present -- BGRA must win regardless of
        // where each appears in the driver's list.
        assert_order_independent(&[F::Bgra8Unorm, F::Rgba8Unorm], false, F::Bgra8Unorm);
    }

    #[test]
    fn web_colors_uses_rgba8_unorm_when_bgra_missing() {
        // Second entry of *our* allowlist -- an approved outcome, not the
        // adapter fallback.
        assert_order_independent(
            &[F::Rgba8Unorm, F::Rgba16Float, F::Rgb10a2Unorm],
            false,
            F::Rgba8Unorm,
        );
    }

    #[test]
    fn web_colors_rejects_rgba16_float_even_when_it_sorts_first() {
        // This is the exact hazard the fix exists to close: an HDR-capable
        // driver reports `Rgba16Float` early. The pre-fix code would pick
        // it because it is `Features::empty()` and non-sRGB; the new
        // selector must ignore it and take the approved BGRA/RGBA8 Unorm
        // format further down the list.
        assert_order_independent(
            &[
                F::Rgba16Float,
                F::Rgb10a2Unorm,
                F::Bgra8Unorm,
                F::Rgba8Unorm,
            ],
            false,
            F::Bgra8Unorm,
        );
    }

    #[test]
    fn web_colors_falls_back_when_only_exotic_formats_available() {
        // HDR-only capability list: no Bgra8Unorm, no Rgba8Unorm. We take
        // the adapter's first reported format so a window can still open,
        // and flag it as a fallback.
        assert_falls_back_for_every_order(
            &[F::Rgba16Float, F::Rgb10a2Unorm, F::Rgb10a2Uint],
            false,
        );
    }

    #[test]
    fn web_colors_falls_back_over_srgb_variants() {
        // sRGB formats are approved only for the gamma-correct path. In
        // web-colors mode they are not selected *as approved*, but they are
        // still better than no window, so the fallback takes them.
        assert_falls_back_for_every_order(&[F::Bgra8UnormSrgb, F::Rgba8UnormSrgb], false);
    }

    // --- gamma-correct (GAMMA_CORRECTION == true) -------------------------

    #[test]
    fn gamma_correct_prefers_bgra8_unorm_srgb_over_rgba8_unorm_srgb() {
        assert_order_independent(
            &[F::Bgra8UnormSrgb, F::Rgba8UnormSrgb],
            true,
            F::Bgra8UnormSrgb,
        );
    }

    #[test]
    fn gamma_correct_uses_rgba8_unorm_srgb_when_bgra_srgb_missing() {
        assert_order_independent(
            &[F::Rgba8UnormSrgb, F::Rgba16Float, F::Rgb10a2Unorm],
            true,
            F::Rgba8UnormSrgb,
        );
    }

    #[test]
    fn gamma_correct_rejects_rgba16_float_even_when_it_sorts_first() {
        assert_order_independent(
            &[
                F::Rgba16Float,
                F::Rgb10a2Unorm,
                F::Bgra8UnormSrgb,
                F::Rgba8UnormSrgb,
            ],
            true,
            F::Bgra8UnormSrgb,
        );
    }

    #[test]
    fn gamma_correct_falls_back_when_only_exotic_formats_available() {
        assert_falls_back_for_every_order(&[F::Rgba16Float, F::Rgb10a2Unorm, F::Rgb10a2Uint], true);
    }

    #[test]
    fn gamma_correct_falls_back_over_unorm_variants() {
        // The non-sRGB unorm variants are the *web-colors* choice, so gamma-
        // correct mode does not treat them as approved (the swapchain would
        // receive pre-linearised pixels intended for an sRGB target). They
        // are still taken by the fallback rather than failing to open.
        assert_falls_back_for_every_order(&[F::Bgra8Unorm, F::Rgba8Unorm], true);
    }

    // --- the fallback boundary --------------------------------------------

    #[test]
    fn fallback_warns_and_names_the_format_it_chose() {
        // The warning is the only diagnostic a user will have when their
        // screenshot looks wrong, so its *content* is under test, not just
        // its existence.
        let caps = [F::Rgba16Float, F::Rgb10a2Unorm];

        let records = capture_logs(|| {
            assert_eq!(
                select_surface_format(&caps, false),
                Some(SurfaceFormat::Fallback(F::Rgba16Float))
            );
        });

        let warned = warnings(&records);
        assert_eq!(
            warned.len(),
            1,
            "expected exactly one warning from the fallback path, got {warned:?}"
        );

        let warning = warned[0];
        assert!(
            warning.contains("Rgba16Float"),
            "warning must name the format actually selected: {warning}"
        );
        assert!(
            warning.contains("Rgb10a2Unorm"),
            "warning must name what the adapter reported: {warning}"
        );
        assert!(
            warning.contains("Bgra8Unorm"),
            "warning must name what was wanted: {warning}"
        );
        assert!(
            warning.contains("colours may be rendered"),
            "warning must say plainly that colours may be wrong: {warning}"
        );
    }

    #[test]
    fn approved_format_does_not_warn() {
        // The regression this whole change risks: a fallback that fires
        // unconditionally still returns a usable format on ordinary
        // hardware, so nothing looks broken to a human. Assert the silence.
        let caps = [F::Rgba16Float, F::Bgra8Unorm, F::Rgba8Unorm];

        let records = capture_logs(|| {
            assert_eq!(
                select_surface_format(&caps, false),
                Some(SurfaceFormat::Approved(F::Bgra8Unorm))
            );
        });

        let warned = warnings(&records);
        assert!(
            warned.is_empty(),
            "the approved path must not warn; got {warned:?}"
        );
    }

    #[test]
    fn fallback_takes_the_first_reported_format_not_an_allowlist_entry() {
        // Pins the *rule* rather than one outcome: with no approved format
        // present, moving a different exotic format to the front moves the
        // selection with it.
        assert_eq!(
            select_surface_format(&[F::Rgb10a2Uint, F::Rgba16Float], false),
            Some(SurfaceFormat::Fallback(F::Rgb10a2Uint))
        );
        assert_eq!(
            select_surface_format(&[F::Rgba16Float, F::Rgb10a2Uint], false),
            Some(SurfaceFormat::Fallback(F::Rgba16Float))
        );
    }

    // --- empty and single-entry lists -------------------------------------

    #[test]
    fn empty_capability_list_returns_none_in_both_modes() {
        // Nothing approved *and* nothing to fall back to. `None` here is not
        // a policy choice -- there is simply no format to return, and the
        // call site raises `IncompatibleSurface`. Must not panic: the
        // fallback reads `formats.first()`, not `formats[0]`.
        assert_eq!(select_surface_format(&[], false), None);
        assert_eq!(select_surface_format(&[], true), None);
    }

    #[test]
    fn empty_capability_list_does_not_warn_about_a_fallback() {
        // The fallback warning names a format it fell back to; with an empty
        // list there is none, so it must not fire and claim otherwise.
        let records = capture_logs(|| {
            assert_eq!(select_surface_format(&[], false), None);
        });

        let warned = warnings(&records);
        assert!(
            warned.is_empty(),
            "an empty capability list has no fallback to warn about; got {warned:?}"
        );
    }

    #[test]
    fn realistic_permutation_list_selects_consistently() {
        // These are the exact six formats reported in iced#3390, by an
        // NVIDIA RTX 4060 on Vulkan driving an HDR display:
        //
        //     Bgra8UnormSrgb, Rgba8UnormSrgb, Rgba16Float,
        //     Rgb10a2Unorm, Bgra8Unorm, Rgba8Unorm
        //
        // The old selector filtered only the two 10-bit formats and then
        // took the first feature-free non-sRGB entry, which on that
        // driver's ordering is `Rgba16Float` — extended-linear output for
        // SDR UI, reported as "everything looks washed out". The reporter
        // confirmed that blacklisting `Rgba16Float` fixes it, but a
        // blacklist only moves the problem: the next unlisted exotic
        // format a driver enumerates first would be selected instead.
        //
        // So the property under test is not "Rgba16Float is rejected" but
        // that the choice does not depend on driver ordering at all. Every
        // permutation must land on the same format for a given mode —
        // 720 orderings per mode, 1440 across both.
        let caps = &[
            F::Rgba16Float,
            F::Rgb10a2Unorm,
            F::Bgra8UnormSrgb,
            F::Bgra8Unorm,
            F::Rgba8UnormSrgb,
            F::Rgba8Unorm,
        ];
        assert_order_independent(caps, false, F::Bgra8Unorm);
        assert_order_independent(caps, true, F::Bgra8UnormSrgb);
    }

    #[test]
    fn iced_3390_list_is_approved_and_silent() {
        // Companion to the permutation test above: the reported hardware is
        // *ordinary* in the sense that an approved format is present, so the
        // fallback must not fire for it. This is the case where an
        // always-fall-back bug would still have produced a working window
        // and gone unnoticed.
        let caps = [
            F::Bgra8UnormSrgb,
            F::Rgba8UnormSrgb,
            F::Rgba16Float,
            F::Rgb10a2Unorm,
            F::Bgra8Unorm,
            F::Rgba8Unorm,
        ];

        let records = capture_logs(|| {
            assert_eq!(
                select_surface_format(&caps, false),
                Some(SurfaceFormat::Approved(F::Bgra8Unorm))
            );
        });

        let warned = warnings(&records);
        assert!(
            warned.is_empty(),
            "iced#3390's adapter offers an approved format, so selection must \
             be silent; got {warned:?}"
        );
    }
}

#[cfg(test)]
mod backend_order_tests {
    use super::{gl_last, present_mode_for};
    use wgpu::Backends as B;

    #[test]
    fn opengl_waits_for_the_other_backends() {
        assert_eq!(gl_last(B::all()), Some((B::all() - B::GL, B::GL)));
        assert_eq!(gl_last(B::VULKAN | B::GL), Some((B::VULKAN, B::GL)));
    }

    #[test]
    fn a_single_choice_is_tried_as_given() {
        assert_eq!(gl_last(B::GL), None);
        assert_eq!(gl_last(B::VULKAN), None);
        assert_eq!(gl_last(B::PRIMARY), None);
    }

    #[test]
    fn vsync_presents_in_mailbox_only_where_allowed_and_offered() {
        use wgpu::PresentMode::{AutoNoVsync, AutoVsync, Fifo, Mailbox};

        assert_eq!(present_mode_for(AutoVsync, true, || true), Mailbox);
        assert_eq!(present_mode_for(AutoVsync, true, || false), AutoVsync);
        assert_eq!(present_mode_for(AutoVsync, false, || true), AutoVsync);
        assert_eq!(present_mode_for(Fifo, true, || true), Fifo);
        assert_eq!(present_mode_for(AutoNoVsync, true, || true), AutoNoVsync);
    }
}
