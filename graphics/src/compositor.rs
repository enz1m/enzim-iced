// Modified by enzUI contributors. See FORK.md.
//! A compositor is responsible for initializing a renderer and managing window
//! surfaces.
use crate::core::Color;
use crate::core::backend;
use crate::core::font;
use crate::core::renderer;
use crate::futures::{MaybeSend, MaybeSync};
use crate::{Shell, Viewport};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use thiserror::Error;

use std::borrow::Cow;
use std::fmt::Debug;

/// A graphics compositor that can draw to windows.
pub trait Compositor: Sized {
    /// The iced renderer of the backend.
    type Renderer;

    /// The surface of the backend.
    ///
    /// `Send + 'static` because a surface outlives the frame that draws into
    /// it and, for a backend that has one, is **owned by the animation lane's
    /// worker thread** (enzICED #2219).
    ///
    /// This bound is load-bearing rather than decorative. The runtime is
    /// generic over the compositor, so without it `spawn_animation_lane` is
    /// uncallable from `iced_winit` and the lane silently does not exist for
    /// any application -- which is exactly the shape of #2219 finding B1.
    /// Every real surface already satisfies it: `wgpu::Surface<'static>` is
    /// `Send + Sync`, and the software backend's `softbuffer::Surface` is
    /// `Send` (its `PhantomData<Cell<()>>` removes only `Sync`). A backend
    /// whose surface genuinely cannot move between threads would fail to
    /// compile here, loudly, instead of quietly losing its lane.
    type Surface: Send + 'static;

    /// Creates a new [`Compositor`] with the given [`backend::Settings`].
    fn new(
        settings: backend::Settings,
        display: impl Display + Clone,
        compatible_window: impl Window + Clone,
        shell: Shell,
    ) -> impl Future<Output = Result<Self, backend::Error>>;

    /// Whether the graphics device under this compositor has been lost.
    ///
    /// A lost device draws nothing more and every surface configured with it
    /// stays unconfigured, so the runtime builds a new compositor (a new
    /// device) and replaces each window's surface and renderer. Backends
    /// without a device to lose are never lost.
    fn device_lost(&self) -> bool {
        false
    }

    /// Whether this compositor draws on an adapter that emulates a GPU on the
    /// CPU, such as Mesa's llvmpipe or lavapipe in a virtual machine without
    /// GPU passthrough. Such an adapter rasterises every frame in full on the
    /// CPU, so a software renderer that redraws only what changed costs less.
    fn emulates_gpu(&self) -> bool {
        false
    }

    /// Creates a [`Self::Renderer`] for the [`Compositor`].
    fn create_renderer(&self, settings: renderer::Settings) -> Self::Renderer;

    /// Crates a new [`Surface`] for the given window.
    ///
    /// [`Surface`]: Self::Surface
    fn create_surface(
        &mut self,
        window: impl Window + Clone,
        width: u32,
        height: u32,
    ) -> Self::Surface;

    /// Configures a new [`Surface`] with the given dimensions.
    ///
    /// [`Surface`]: Self::Surface
    fn configure_surface(&mut self, surface: &mut Self::Surface, width: u32, height: u32);

    /// Returns [`Information`] used by this [`Compositor`].
    fn information(&self) -> Information;

    /// Loads a font from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`font::Error::InvalidData`] when the bytes contain no loadable
    /// font face. A rejected load leaves every shaped text valid and the
    /// requested family unavailable.
    fn load_font(&mut self, font: Cow<'static, [u8]>) -> Result<(), font::Error> {
        crate::text::font_system()
            .write()
            .expect("Write to font system")
            .load_font(font)
    }

    /// Lists all the available font families.
    fn list_fonts(&mut self) -> Result<Vec<font::Family>, font::Error> {
        use std::collections::BTreeSet;

        let font_system = crate::text::font_system()
            .read()
            .expect("Read from font system");

        let families = BTreeSet::from_iter(font_system.families());

        Ok(families.into_iter().map(font::Family::name).collect())
    }

    /// The animation-lane backend for this compositor, if it has one (enzICED
    /// #2219).
    ///
    /// This is the hook a backend implements, and it is boxed rather than an
    /// associated type for one concrete reason: the **shipped** compositor is
    /// `iced_renderer::fallback::Compositor`, which owns no device of its own
    /// and can only wire a lane by *adapting* the backend of whichever inner
    /// compositor won. Adapting requires naming the inner backend behind a
    /// pointer.
    ///
    /// The default is `None`: a backend whose surface cannot leave the UI
    /// thread (the `softbuffer`-based software backend) has **no** lane, and
    /// the runtime must keep animating on the UI path and say so. Returning
    /// `None` is the honest software fallback, not a failure.
    fn animation_lane_backend(
        &self,
        renderer_settings: renderer::Settings,
    ) -> Option<Box<dyn crate::lane::Backend<Surface = Self::Surface>>> {
        let _ = renderer_settings;
        None
    }

    /// Spawns a renderer-owned animation lane for this compositor (enzICED
    /// #2219).
    ///
    /// The lane owns a **second** renderer over the same device and, while it
    /// is running, the window surface. That is what lets an already-committed
    /// scene keep being sampled and presented while the UI thread is blocked in
    /// unrelated view/layout work -- a same-thread cache cannot.
    ///
    /// Backends override [`animation_lane_backend`](Self::animation_lane_backend),
    /// not this: spawning is identical for every backend, and a compositor that
    /// forgot to forward *this* method while implementing the backend is
    /// exactly the defect that made the lane unreachable in production
    /// (#2219 review finding B1).
    fn spawn_animation_lane(
        &self,
        renderer_settings: renderer::Settings,
    ) -> Option<crate::lane::Lane<Self::Surface>> {
        crate::lane::try_spawn(&crate::lane::admission::global(), || {
            self.animation_lane_backend(renderer_settings)
        })
    }

    /// Strongest milestone this compositor can establish after `present` succeeds.
    ///
    /// The conservative default is [`PresentationQuality::Submitted`]. A backend
    /// must override it only when it actually completes the stronger milestone.
    fn presentation_quality(&self) -> PresentationQuality {
        PresentationQuality::Submitted
    }

    /// Presents the [`Renderer`] primitives to the next frame of the given [`Surface`].
    ///
    /// [`Renderer`]: Self::Renderer
    /// [`Surface`]: Self::Surface
    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), SurfaceError>;

    /// Whether presenting `renderer`'s frame would leave a surface showing
    /// the last frame presented from `renderer` unchanged, so the caller may
    /// skip [`present`](Self::present). A `false` is followed by `present`
    /// to the same surface. The default never skips.
    fn unchanged(
        &mut self,
        _renderer: &mut Self::Renderer,
        _viewport: &Viewport,
        _background_color: Color,
    ) -> bool {
        false
    }

    /// Whether [`present`](Self::present) measures its backend stages
    /// (enzICED). Off by default; a compositor that cannot measure ignores it.
    fn measure_present_stages(&mut self, _enabled: bool) {}

    /// The backend stages of the last [`present`](Self::present) while
    /// measuring: acquiring the image, preparing and encoding, submitting, and
    /// presenting. `None` when not measured.
    fn take_present_stages(&mut self) -> Option<crate::lane::PresentStages> {
        None
    }

    /// Screenshots the current [`Renderer`] primitives to an offscreen texture, and returns the bytes of
    /// the texture ordered as `RGBA` in the `sRGB` color space.
    ///
    /// [`Renderer`]: Self::Renderer
    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8>;
}

/// A window that can be used in a [`Compositor`].
///
/// This is just a convenient super trait of the `raw-window-handle`
/// traits.
pub trait Window: HasWindowHandle + Debug + MaybeSend + MaybeSync + 'static {}

impl<T> Window for T where T: HasWindowHandle + Debug + MaybeSend + MaybeSync + 'static {}

/// An owned display handle that can be used in a [`Compositor`].
///
/// This is just a convenient super trait of the `raw-window-handle`
/// trait.
pub trait Display: HasDisplayHandle + Debug + Send + Sync + 'static {}

impl<T> Display for T where T: HasDisplayHandle + Debug + Send + Sync + 'static {}

/// Defines the default compositor of a renderer.
pub trait Default {
    /// The compositor of the renderer.
    type Compositor: Compositor<Renderer = Self>;
}

/// Strength of a successful compositor frame milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PresentationQuality {
    /// GPU queue submission completed, with no surface presentation result.
    Submitted,
    /// A surface present call completed; compositor visibility is unknown.
    SurfacePresented,
    /// The platform compositor reported actual presentation.
    CompositorPresented,
}

/// Result of an unsuccessful call to [`Compositor::present`].
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum SurfaceError {
    /// A timeout was encountered while trying to acquire the next frame.
    #[error("A timeout was encountered while trying to acquire the next frame")]
    Timeout,
    /// The underlying surface has changed, and therefore the surface must be updated.
    #[error("The underlying surface has changed, and therefore the surface must be updated.")]
    Outdated,
    /// The swap chain has been lost and needs to be recreated.
    #[error("The surface has been lost and needs to be recreated")]
    Lost,
    /// There is no more memory left to allocate a new frame.
    #[error("There is no more memory left to allocate a new frame")]
    OutOfMemory,
    /// The surface is occluded and must not be drawn to.
    #[error("The surface is occluded and must not be drawn to")]
    Occluded,
    /// Acquiring a texture failed with a generic error.
    #[error("Acquiring a texture failed with a generic error")]
    Other,
}

/// Contains information about the graphics (e.g. graphics adapter, graphics backend).
#[derive(Debug)]
pub struct Information {
    /// Contains the graphics adapter.
    pub adapter: String,
    /// Contains the graphics backend.
    pub backend: String,
    /// Device/vendor/type/driver identity reported in-process by the backend.
    pub device: String,
}

#[cfg(debug_assertions)]
impl Compositor for () {
    type Renderer = ();
    type Surface = ();

    async fn new(
        _settings: backend::Settings,
        _display: impl Display,
        _compatible_window: impl Window + Clone,
        _shell: Shell,
    ) -> Result<Self, backend::Error> {
        Ok(())
    }

    fn create_renderer(&self, _settings: renderer::Settings) -> Self::Renderer {}

    fn create_surface(
        &mut self,
        _window: impl Window + Clone,
        _width: u32,
        _height: u32,
    ) -> Self::Surface {
    }

    fn configure_surface(&mut self, _surface: &mut Self::Surface, _width: u32, _height: u32) {}

    fn load_font(&mut self, _font: Cow<'static, [u8]>) -> Result<(), font::Error> {
        Ok(())
    }

    fn list_fonts(&mut self) -> Result<Vec<font::Family>, font::Error> {
        Ok(Vec::new())
    }

    fn information(&self) -> Information {
        Information {
            adapter: String::from("Null Renderer"),
            backend: String::from("Null"),
            device: String::from("None"),
        }
    }

    fn present(
        &mut self,
        _renderer: &mut Self::Renderer,
        _surface: &mut Self::Surface,
        _viewport: &Viewport,
        _background_color: Color,
        _on_pre_present: impl FnOnce(),
    ) -> Result<(), SurfaceError> {
        Ok(())
    }

    fn screenshot(
        &mut self,
        _renderer: &mut Self::Renderer,
        _viewport: &Viewport,
        _background_color: Color,
    ) -> Vec<u8> {
        vec![]
    }
}

#[cfg(debug_assertions)]
impl Default for () {
    type Compositor = ();
}
