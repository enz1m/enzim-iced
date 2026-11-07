// Modified by enzUI contributors. See FORK.md.
//! Compose existing renderers and create type-safe fallback strategies.
use crate::core::backend;
use crate::core::font;
use crate::core::image;
use crate::core::renderer;
use crate::core::svg;
use crate::core::{self, Background, Color, Image, Point, Rectangle, Size, Svg, Transformation};
use crate::graphics::compositor;
use crate::graphics::mesh;
use crate::graphics::text;
use crate::graphics::{self, Shell};

use std::borrow::Cow;

/// A renderer `A` with a fallback strategy `B`.
///
/// This type can be used to easily compose existing renderers and
/// create custom, type-safe fallback strategies.
#[derive(Debug)]
pub enum Renderer<A, B> {
    /// The primary rendering option.
    Primary(A),
    /// The secondary (or fallback) rendering option.
    Secondary(B),
}

macro_rules! delegate {
    ($renderer:expr, $name:ident, $body:expr) => {
        match $renderer {
            Self::Primary($name) => $body,
            Self::Secondary($name) => $body,
        }
    };
}

impl<A, B> core::Renderer for Renderer<A, B>
where
    A: core::Renderer,
    B: core::Renderer,
{
    fn fill_quad(&mut self, quad: renderer::Quad, background: impl Into<Background>) {
        delegate!(self, renderer, renderer.fill_quad(quad, background.into()));
    }

    fn start_layer(&mut self, bounds: Rectangle) {
        delegate!(self, renderer, renderer.start_layer(bounds));
    }

    fn start_layer_retained(&mut self, visible: Rectangle, retained: Rectangle) {
        delegate!(
            self,
            renderer,
            renderer.start_layer_retained(visible, retained)
        );
    }

    // enzUI-ICED #1568: a fallback delegates capabilities; it must not swallow a
    // backend's real subtree compositing behind the core trait's opaque default.
    fn start_layer_alpha(&mut self, bounds: Rectangle, alpha: f32) {
        delegate!(self, renderer, renderer.start_layer_alpha(bounds, alpha));
    }

    fn start_layer_composite(
        &mut self,
        bounds: Rectangle,
        alpha: f32,
        scale: f32,
        rotation: f32,
        origin: Point,
    ) {
        delegate!(
            self,
            renderer,
            renderer.start_layer_composite(bounds, alpha, scale, rotation, origin)
        );
    }

    fn end_layer_composite(&mut self) {
        delegate!(self, renderer, renderer.end_layer_composite());
    }

    // enzUI-ICED #2950: a fallback must report the *active* backend's material
    // capability. Taking the trait default here would tell every caller that no
    // backdrop can be filtered, which is a silent downgrade to opaque panels on
    // a backend that can filter one.
    fn material_support(&self) -> core::material::Support {
        delegate!(self, renderer, renderer.material_support())
    }

    fn material_luminance(&self, probe: u64) -> Option<f32> {
        delegate!(self, renderer, renderer.material_luminance(probe))
    }

    fn material_luminance_with_frame(&self, probe: u64) -> Option<(f32, u64)> {
        delegate!(
            self,
            renderer,
            renderer.material_luminance_with_frame(probe)
        )
    }

    fn material_frame(&self) -> u64 {
        delegate!(self, renderer, renderer.material_frame())
    }

    fn set_material_waker(&mut self, waker: core::shell::Waker) {
        delegate!(self, renderer, renderer.set_material_waker(waker));
    }

    fn request_material_redraw(&self) {
        delegate!(self, renderer, renderer.request_material_redraw());
    }

    fn start_layer_material(&mut self, bounds: Rectangle, material: core::material::Material) {
        delegate!(
            self,
            renderer,
            renderer.start_layer_material(bounds, material)
        );
    }

    // enzICED #41: forward the rounded clip to the active backend so it can
    // honour the radius (tiny_skia does; wgpu falls back to rectangular). Not
    // forwarding this would make every fallback-wrapped renderer silently take
    // the trait default (rectangular), defeating the tiny_skia implementation.
    fn start_layer_rounded(&mut self, bounds: Rectangle, radius: core::border::Radius) {
        delegate!(self, renderer, renderer.start_layer_rounded(bounds, radius));
    }

    fn start_layout_layer(
        &mut self,
        source: core::renderer::LayoutLayer,
        bounds: Rectangle,
        radius: Option<core::border::Radius>,
    ) -> bool {
        delegate!(
            self,
            renderer,
            renderer.start_layout_layer(source, bounds, radius)
        )
    }

    fn layout_clip_capture(
        &self,
        node: core::renderer::NodeId,
    ) -> Option<core::renderer::ClipCapture> {
        delegate!(self, renderer, renderer.layout_clip_capture(node))
    }

    fn layout_reveal_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        delegate!(
            self,
            renderer,
            renderer.layout_reveal_capture_viewport(node)
        )
    }

    fn start_layout_reveal(&mut self, node: core::renderer::NodeId, progress: f32) -> bool {
        delegate!(self, renderer, renderer.start_layout_reveal(node, progress))
    }

    fn end_layer(&mut self) {
        delegate!(self, renderer, renderer.end_layer());
    }

    fn record_presentation(
        &mut self,
        program: std::sync::Arc<core::renderer::presentation::Program>,
    ) -> Result<(), core::renderer::presentation::Error> {
        delegate!(self, renderer, renderer.record_presentation(program))
    }

    fn request_docked_presentation(
        &mut self,
        capture: core::renderer::docked::Capture,
    ) -> Result<(), core::renderer::presentation::Error> {
        delegate!(
            self,
            renderer,
            renderer.request_docked_presentation(capture)
        )
    }

    fn begin_scroll_view(
        &mut self,
        node: core::renderer::NodeId,
        bounds: Rectangle,
        content: Rectangle,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        paint: core::renderer::presentation::scroll_paint::Paint,
    ) -> Option<Rectangle> {
        delegate!(
            self,
            renderer,
            renderer.begin_scroll_view(node, bounds, content, config, input, paint)
        )
    }
    fn draw_scroll_chrome(
        &mut self,
        node: Option<core::renderer::NodeId>,
        bounds: Rectangle,
        content: Rectangle,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        paint: core::renderer::presentation::scroll_paint::Paint,
    ) {
        delegate!(
            self,
            renderer,
            renderer.draw_scroll_chrome(node, bounds, content, config, input, paint)
        );
    }
    fn end_scroll_view(&mut self, node: core::renderer::NodeId) {
        delegate!(self, renderer, renderer.end_scroll_view(node));
    }

    fn editor_presentation_height(&self, node: core::renderer::NodeId) -> Option<f32> {
        delegate!(self, renderer, renderer.editor_presentation_height(node))
    }

    fn fill_layout_editor_scrollbars(
        &mut self,
        owner: renderer::NodeId,
        editor: renderer::NodeId,
        source: Option<std::sync::Arc<()>>,
        bounds: Rectangle,
        metrics: core::text::editor::ScrollMetrics,
        paint: core::text::editor::scrollbar::Paint,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_layout_editor_scrollbars(owner, editor, source, bounds, metrics, paint)
        );
    }

    fn fill_layout_quad(
        &mut self,
        node: core::renderer::NodeId,
        quad: core::renderer::Quad,
        background: core::Background,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_layout_quad(node, quad, background)
        );
    }

    fn fill_layout_blink(
        &mut self,
        node: core::renderer::NodeId,
        quad: core::renderer::Quad,
        background: core::Background,
        blink: core::layout::input::Blink,
        at: core::time::Instant,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_layout_blink(node, quad, background, blink, at)
        );
    }

    fn fill_layout_rule(
        &mut self,
        node: core::renderer::NodeId,
        allocation: Rectangle,
        quad: core::renderer::Quad,
        background: core::Background,
        paint: core::layout::rule::Paint,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_layout_rule(node, allocation, quad, background, paint)
        );
    }

    fn reject_layout_paint(&mut self, node: core::renderer::NodeId) {
        delegate!(self, renderer, renderer.reject_layout_paint(node));
    }

    fn layout_culling_capture(
        &self,
        scope: core::renderer::NodeId,
        viewport: Rectangle,
    ) -> Option<core::renderer::CullCapture> {
        match self {
            Self::Primary(renderer) => renderer.layout_culling_capture(scope, viewport),
            Self::Secondary(renderer) => renderer.layout_culling_capture(scope, viewport),
        }
    }

    fn layout_culling_active(&self, scope: core::renderer::NodeId) -> bool {
        match self {
            Self::Primary(renderer) => renderer.layout_culling_active(scope),
            Self::Secondary(renderer) => renderer.layout_culling_active(scope),
        }
    }

    fn retains_layout_leaf(&self, node: core::renderer::NodeId) -> bool {
        delegate!(self, renderer, renderer.retains_layout_leaf(node))
    }

    fn layout_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        match self {
            Self::Primary(renderer) => renderer.layout_capture_viewport(node),
            Self::Secondary(renderer) => renderer.layout_capture_viewport(node),
        }
    }

    fn tab_strip_capture_viewport(&self, node: core::renderer::NodeId) -> Option<Rectangle> {
        delegate!(self, renderer, renderer.tab_strip_capture_viewport(node))
    }

    fn presentation_refusal(&self) -> Option<core::renderer::presentation::Error> {
        delegate!(self, renderer, renderer.presentation_refusal())
    }

    fn start_recording(&mut self, bounds: Rectangle) -> bool {
        delegate!(self, renderer, renderer.start_recording(bounds))
    }

    fn start_staged_recording(&mut self, bounds: Rectangle) -> bool {
        delegate!(self, renderer, renderer.start_staged_recording(bounds))
    }

    fn start_layout_recording(&mut self, node: core::renderer::NodeId) -> bool {
        delegate!(self, renderer, renderer.start_layout_recording(node))
    }

    fn mark_recording(&mut self, node: core::renderer::NodeId) {
        delegate!(self, renderer, renderer.mark_recording(node));
    }

    fn mark_recording_at(&mut self, node: core::renderer::NodeId, baseline: core::Vector) {
        delegate!(self, renderer, renderer.mark_recording_at(node, baseline));
    }

    fn replay_tracks_at(
        &mut self,
        recording: &core::renderer::Recording,
        tracks: &[core::renderer::Track],
        at: core::time::Instant,
    ) -> bool {
        delegate!(
            self,
            renderer,
            renderer.replay_tracks_at(recording, tracks, at)
        )
    }

    fn replay_tracks(
        &mut self,
        recording: &core::renderer::Recording,
        tracks: &[core::renderer::Track],
    ) -> bool {
        delegate!(self, renderer, renderer.replay_tracks(recording, tracks))
    }

    fn reuse_frame(&mut self) -> bool {
        delegate!(self, renderer, renderer.reuse_frame())
    }

    fn finish_recording(&mut self) -> Option<core::renderer::Recording> {
        delegate!(self, renderer, renderer.finish_recording())
    }

    fn replay(&mut self, recording: &core::renderer::Recording, translation: core::Vector) -> bool {
        // A recording made by the OTHER backend returns `None` from
        // `payload`, so a renderer that fell back mid-run reports the miss and
        // the caller redraws instead of replaying foreign layers (#2219).
        delegate!(self, renderer, renderer.replay(recording, translation))
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        delegate!(
            self,
            renderer,
            renderer.start_transformation(transformation)
        );
    }

    fn end_transformation(&mut self) {
        delegate!(self, renderer, renderer.end_transformation());
    }

    fn allocate_image(
        &self,
        handle: &image::Handle,
        callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
    ) {
        delegate!(self, renderer, renderer.allocate_image(handle, callback));
    }

    fn hint(&mut self, scale: renderer::Scale) {
        delegate!(self, renderer, renderer.hint(scale));
    }

    fn scale(&self) -> Option<renderer::Scale> {
        delegate!(self, renderer, renderer.scale())
    }

    fn current_transformation(&self) -> Transformation {
        delegate!(self, renderer, renderer.current_transformation())
    }

    fn tick(&mut self) {
        delegate!(self, renderer, renderer.tick());
    }

    fn reconcile_presented_scroll(
        &self,
        node: core::renderer::NodeId,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        viewport: core::Size,
        content: core::Size,
    ) -> Option<bool> {
        delegate!(
            self,
            renderer,
            renderer.reconcile_presented_scroll(node, config, input, viewport, content)
        )
    }

    fn install_presented_geometry(
        &mut self,
        recording: &core::renderer::Recording,
        sample: std::sync::Arc<core::renderer::presentation::Sample>,
    ) -> bool {
        delegate!(
            self,
            renderer,
            renderer.install_presented_geometry(recording, sample)
        )
    }

    fn install_presented_layout(
        &mut self,
        recording: &core::renderer::Recording,
        sample: std::sync::Arc<core::renderer::presentation::Sample>,
    ) -> bool {
        delegate!(
            self,
            renderer,
            renderer.install_presented_layout(recording, sample)
        )
    }
    fn presented_layout(&self) -> Option<&std::sync::Arc<core::renderer::presentation::Sample>> {
        delegate!(self, renderer, renderer.presented_layout())
    }
    fn clear_presented_layout(&mut self) {
        delegate!(self, renderer, renderer.clear_presented_layout());
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        delegate!(self, renderer, renderer.reset(new_bounds));
    }

    fn settings(&self) -> renderer::Settings {
        delegate!(self, renderer, renderer.settings())
    }
}

impl<A, B> core::text::Renderer for Renderer<A, B>
where
    A: core::text::Renderer,
    B: core::text::Renderer<Paragraph = A::Paragraph, Editor = A::Editor>,
{
    type Paragraph = A::Paragraph;
    type Editor = A::Editor;

    const ICON_FONT: core::Font = A::ICON_FONT;
    const CHECKMARK_ICON: char = A::CHECKMARK_ICON;
    const ARROW_DOWN_ICON: char = A::ARROW_DOWN_ICON;
    const SCROLL_UP_ICON: char = A::SCROLL_UP_ICON;
    const SCROLL_DOWN_ICON: char = A::SCROLL_DOWN_ICON;
    const SCROLL_LEFT_ICON: char = A::SCROLL_LEFT_ICON;
    const SCROLL_RIGHT_ICON: char = A::SCROLL_RIGHT_ICON;
    const ICED_LOGO: char = A::ICED_LOGO;

    fn reconcile_presented_editor(
        &self,
        node: core::renderer::NodeId,
        editor: &mut Self::Editor,
        viewport: Size,
    ) -> bool {
        match self {
            Self::Primary(renderer) => renderer.reconcile_presented_editor(node, editor, viewport),
            Self::Secondary(renderer) => {
                renderer.reconcile_presented_editor(node, editor, viewport)
            }
        }
    }

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_paragraph(text, position, color, clip_bounds)
        );
    }

    fn draw_editor_view(
        &mut self,
        node: Option<core::renderer::NodeId>,
        editor: &Self::Editor,
        paint: &core::text::editor::paint::Paint<'_, core::Font>,
    ) {
        delegate!(
            self,
            renderer,
            renderer.draw_editor_view(node, editor, paint)
        );
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_editor(editor, position, color, clip_bounds)
        );
    }

    fn fill_text(
        &mut self,
        text: core::Text<String>,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        delegate!(
            self,
            renderer,
            renderer.fill_text(text, position, color, clip_bounds)
        );
    }
}

impl<A, B> text::Renderer for Renderer<A, B>
where
    A: text::Renderer,
    B: text::Renderer,
{
    fn fill_raw(&mut self, raw: text::Raw) {
        delegate!(self, renderer, renderer.fill_raw(raw));
    }
}

impl<A, B> image::Renderer for Renderer<A, B>
where
    A: image::Renderer,
    B: image::Renderer<Handle = A::Handle>,
{
    type Handle = A::Handle;

    fn load_image(&self, handle: &Self::Handle) -> Result<image::Allocation, image::Error> {
        delegate!(self, renderer, renderer.load_image(handle))
    }

    fn measure_image(&self, handle: &Self::Handle) -> Option<Size<u32>> {
        delegate!(self, renderer, renderer.measure_image(handle))
    }

    fn draw_layout_image(
        &mut self,
        node: Option<core::renderer::NodeId>,
        image: Image<A::Handle>,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        scale: f32,
    ) {
        match self {
            Self::Primary(renderer) => {
                renderer.draw_layout_image(node, image, bounds, clip_bounds, scale);
            }
            Self::Secondary(renderer) => {
                renderer.draw_layout_image(node, image, bounds, clip_bounds, scale);
            }
        }
    }

    fn draw_image(&mut self, image: Image<A::Handle>, bounds: Rectangle, clip_bounds: Rectangle) {
        delegate!(
            self,
            renderer,
            renderer.draw_image(image, bounds, clip_bounds)
        );
    }
}

impl<A, B> svg::Renderer for Renderer<A, B>
where
    A: svg::Renderer,
    B: svg::Renderer,
{
    fn measure_svg(&self, handle: &svg::Handle) -> Size<u32> {
        delegate!(self, renderer, renderer.measure_svg(handle))
    }

    fn draw_layout_svg(
        &mut self,
        node: Option<core::renderer::NodeId>,
        svg: Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        match self {
            Self::Primary(renderer) => renderer.draw_layout_svg(node, svg, bounds, clip_bounds),
            Self::Secondary(renderer) => renderer.draw_layout_svg(node, svg, bounds, clip_bounds),
        }
    }

    fn draw_svg(&mut self, svg: Svg, bounds: Rectangle, clip_bounds: Rectangle) {
        delegate!(self, renderer, renderer.draw_svg(svg, bounds, clip_bounds));
    }
}

impl<A, B> mesh::Renderer for Renderer<A, B>
where
    A: mesh::Renderer,
    B: mesh::Renderer,
{
    fn draw_mesh(&mut self, mesh: graphics::Mesh) {
        delegate!(self, renderer, renderer.draw_mesh(mesh));
    }

    fn draw_mesh_cache(&mut self, cache: mesh::Cache) {
        delegate!(self, renderer, renderer.draw_mesh_cache(cache));
    }
}

/// A compositor `A` with a fallback strategy `B`.
///
/// It works analogously to [`Renderer`].
#[derive(Debug)]
pub enum Compositor<A, B>
where
    A: graphics::Compositor,
    B: graphics::Compositor,
{
    /// The primary compositing option.
    Primary(A),
    /// The secondary (or fallback) compositing option.
    Secondary(B),
}

/// A surface `A` with a fallback strategy `B`.
///
/// It works analogously to [`Renderer`].
#[derive(Debug)]
pub enum Surface<A, B> {
    /// The primary surface option.
    Primary(A),
    /// The secondary (or fallback) surface option.
    Secondary(B),
}

/// The animation lane of whichever inner compositor won, adapted to this
/// compositor's surface enum (enzICED #2219).
///
/// # Why this type has to exist
///
/// `Compositor<A, B>` is the **shipped** `iced` compositor: with the default
/// `wgpu` + `tiny-skia` features, `iced::Compositor` *is*
/// `fallback::Compositor<iced_wgpu::window::Compositor,
/// iced_tiny_skia::window::Compositor>`. Its surface is
/// [`Surface<A::Surface, B::Surface>`], which is a different type from the
/// surface `A`'s own lane backend presents to.
///
/// Before this adapter existed, `iced_wgpu` implemented the lane and this
/// wrapper did not forward it, so `spawn_animation_lane` resolved to the
/// trait's `None` default for every real application: the lane compiled, was
/// unit-tested green off-thread, and could never own a surface in production.
/// That is the same class of defect as fork patch 60's "no caller", one layer
/// further out, and no test caught it because every lane test constructed a
/// backend directly (#2219 review finding B1).
///
/// # Failing closed
///
/// A surface from the *other* variant can only appear if the compositor fell
/// back after the lane was built. The adapter then reports a surface error
/// instead of panicking: a panic here would kill the render worker and poison
/// the lane, while an error stops the lane cleanly and the UI thread takes the
/// surface back and draws.
enum LaneAdapter<A, B> {
    /// The primary compositor's lane, presenting into `Surface::Primary`.
    Primary(Box<dyn graphics::lane::Backend<Surface = A>>),
    /// The secondary compositor's lane, presenting into `Surface::Secondary`.
    Secondary(Box<dyn graphics::lane::Backend<Surface = B>>),
}

impl<A, B> graphics::lane::Backend for LaneAdapter<A, B>
where
    A: Send + 'static,
    B: Send + 'static,
{
    type Surface = Surface<A, B>;

    fn configure(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        match (self, surface) {
            (Self::Primary(backend), Surface::Primary(surface)) => {
                backend.configure(surface, width, height);
            }
            (Self::Secondary(backend), Surface::Secondary(surface)) => {
                backend.configure(surface, width, height);
            }
            // Mismatched halves: do nothing here and let `present` below report
            // the error, so the lane stops on a path that has a return value.
            _ => {}
        }
    }

    fn replay(
        &mut self,
        viewport: &graphics::Viewport,
        scene: &renderer::Recording,
        tracks: &[renderer::Track],
    ) -> bool {
        match self {
            Self::Primary(backend) => backend.replay(viewport, scene, tracks),
            Self::Secondary(backend) => backend.replay(viewport, scene, tracks),
        }
    }

    fn retire_resources(&mut self) {
        match self {
            Self::Primary(backend) => backend.retire_resources(),
            Self::Secondary(backend) => backend.retire_resources(),
        }
    }

    fn layout_sample(
        &mut self,
    ) -> Result<
        Option<std::sync::Arc<core::renderer::presentation::Sample>>,
        core::renderer::presentation::Error,
    > {
        match self {
            Self::Primary(backend) => backend.layout_sample(),
            Self::Secondary(backend) => backend.layout_sample(),
        }
    }

    fn replay_refusal(&self) -> Option<core::renderer::presentation::Error> {
        match self {
            Self::Primary(backend) => backend.replay_refusal(),
            Self::Secondary(backend) => backend.replay_refusal(),
        }
    }

    fn present(
        &mut self,
        surface: &mut Self::Surface,
        viewport: &graphics::Viewport,
        background: Color,
        on_pre_present: &mut dyn FnMut(),
    ) -> Result<(), compositor::SurfaceError> {
        // The pre-commit hook is forwarded UNCHANGED (enzICED #2219). The
        // fallback wrapper reconstructs no identity of its own: whichever half
        // actually commits is the one that must trigger the feedback request,
        // in that half's own request stream, at that half's own seam.
        match (self, surface) {
            (Self::Primary(backend), Surface::Primary(surface)) => {
                backend.present(surface, viewport, background, on_pre_present)
            }
            (Self::Secondary(backend), Surface::Secondary(surface)) => {
                backend.present(surface, viewport, background, on_pre_present)
            }
            // The compositor fell back underneath a running lane. Stop, rather
            // than present a surface this backend does not own. Nothing is
            // committed, so the hook is deliberately NOT called.
            _ => Err(compositor::SurfaceError::Lost),
        }
    }

    fn presentation_quality(&self) -> compositor::PresentationQuality {
        match self {
            Self::Primary(backend) => backend.presentation_quality(),
            Self::Secondary(backend) => backend.presentation_quality(),
        }
    }

    /// Forwarded to whichever half actually presented (enzICED #2219).
    ///
    /// Not forwarding it is exactly the class of defect that left
    /// `spawn_animation_lane` unreachable in production: the shipped compositor
    /// is this adapter, so a stage the adapter swallows is a stage the shipped
    /// build never records.
    fn take_present_stages(&mut self) -> Option<graphics::lane::PresentStages> {
        match self {
            Self::Primary(backend) => backend.take_present_stages(),
            Self::Secondary(backend) => backend.take_present_stages(),
        }
    }

    /// Forwarded for the same reason (enzICED #2250): the shipped compositor is
    /// this adapter, so an opt-in the adapter swallows never reaches the wgpu
    /// backend and every live frame reports no spans at all.
    fn measure_stages(&mut self, enabled: bool) {
        match self {
            Self::Primary(backend) => backend.measure_stages(enabled),
            Self::Secondary(backend) => backend.measure_stages(enabled),
        }
    }
}

impl<A, B> graphics::Compositor for Compositor<A, B>
where
    A: graphics::Compositor,
    B: graphics::Compositor,
{
    type Renderer = Renderer<A::Renderer, B::Renderer>;
    type Surface = Surface<A::Surface, B::Surface>;

    async fn new(
        settings: backend::Settings,
        display: impl compositor::Display + Clone,
        compatible_window: impl compositor::Window + Clone,
        shell: Shell,
    ) -> Result<Self, backend::Error> {
        let mut errors = vec![];

        match A::new(
            settings.clone(),
            display.clone(),
            compatible_window.clone(),
            shell.clone(),
        )
        .await
        {
            // A GPU emulated on the CPU yields to the secondary backend unless
            // a backend was named; it stays when the secondary cannot start.
            Ok(compositor)
                if compositor.emulates_gpu() && settings.backend == backend::Backend::Best =>
            {
                if let Ok(secondary) = B::new(
                    settings.clone(),
                    display.clone(),
                    compatible_window.clone(),
                    shell.clone(),
                )
                .await
                {
                    log::info!("the GPU is emulated on the CPU; using the software renderer");
                    return Ok(Self::Secondary(secondary));
                }
                return Ok(Self::Primary(compositor));
            }
            Ok(compositor) => return Ok(Self::Primary(compositor)),
            Err(error) => {
                errors.push(error);
            }
        }

        match B::new(
            settings,
            display.clone(),
            compatible_window.clone(),
            shell.clone(),
        )
        .await
        {
            Ok(compositor) => return Ok(Self::Secondary(compositor)),
            Err(error) => {
                errors.push(error);
            }
        }

        Err(backend::Error::List(errors))
    }

    fn device_lost(&self) -> bool {
        match self {
            Self::Primary(compositor) => compositor.device_lost(),
            Self::Secondary(compositor) => compositor.device_lost(),
        }
    }

    fn emulates_gpu(&self) -> bool {
        match self {
            Self::Primary(compositor) => compositor.emulates_gpu(),
            Self::Secondary(compositor) => compositor.emulates_gpu(),
        }
    }

    fn create_renderer(&self, settings: renderer::Settings) -> Self::Renderer {
        match self {
            Self::Primary(compositor) => Renderer::Primary(compositor.create_renderer(settings)),
            Self::Secondary(compositor) => {
                Renderer::Secondary(compositor.create_renderer(settings))
            }
        }
    }

    fn create_surface(
        &mut self,
        window: impl compositor::Window + Clone,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        match self {
            Self::Primary(compositor) => {
                Surface::Primary(compositor.create_surface(window, width, height))
            }
            Self::Secondary(compositor) => {
                Surface::Secondary(compositor.create_surface(window, width, height))
            }
        }
    }

    fn configure_surface(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        match (self, surface) {
            (Self::Primary(compositor), Surface::Primary(surface)) => {
                compositor.configure_surface(surface, width, height);
            }
            (Self::Secondary(compositor), Surface::Secondary(surface)) => {
                compositor.configure_surface(surface, width, height);
            }
            _ => unreachable!(),
        }
    }

    fn load_font(&mut self, font: Cow<'static, [u8]>) -> Result<(), font::Error> {
        delegate!(self, compositor, compositor.load_font(font))
    }

    fn list_fonts(&mut self) -> Result<Vec<font::Family>, font::Error> {
        delegate!(self, compositor, compositor.list_fonts())
    }

    fn information(&self) -> compositor::Information {
        delegate!(self, compositor, compositor.information())
    }

    fn presentation_quality(&self) -> compositor::PresentationQuality {
        delegate!(self, compositor, compositor.presentation_quality())
    }

    fn measure_present_stages(&mut self, enabled: bool) {
        delegate!(self, compositor, compositor.measure_present_stages(enabled));
    }

    fn take_present_stages(&mut self) -> Option<graphics::lane::PresentStages> {
        delegate!(self, compositor, compositor.take_present_stages())
    }

    /// Forwards the winning inner compositor's lane backend, adapted to this
    /// compositor's surface enum (enzICED #2219).
    fn animation_lane_backend(
        &self,
        renderer_settings: renderer::Settings,
    ) -> Option<Box<dyn graphics::lane::Backend<Surface = Self::Surface>>> {
        match self {
            Self::Primary(compositor) => Some(Box::new(LaneAdapter::Primary(
                compositor.animation_lane_backend(renderer_settings)?,
            ))),
            Self::Secondary(compositor) => Some(Box::new(LaneAdapter::Secondary(
                compositor.animation_lane_backend(renderer_settings)?,
            ))),
        }
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &graphics::Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        match (self, renderer, surface) {
            (Self::Primary(compositor), Renderer::Primary(renderer), Surface::Primary(surface)) => {
                compositor.present(
                    renderer,
                    surface,
                    viewport,
                    background_color,
                    on_pre_present,
                )
            }
            (
                Self::Secondary(compositor),
                Renderer::Secondary(renderer),
                Surface::Secondary(surface),
            ) => compositor.present(
                renderer,
                surface,
                viewport,
                background_color,
                on_pre_present,
            ),
            _ => unreachable!(),
        }
    }

    fn unchanged(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &graphics::Viewport,
        background_color: Color,
    ) -> bool {
        match (self, renderer) {
            (Self::Primary(compositor), Renderer::Primary(renderer)) => {
                compositor.unchanged(renderer, viewport, background_color)
            }
            (Self::Secondary(compositor), Renderer::Secondary(renderer)) => {
                compositor.unchanged(renderer, viewport, background_color)
            }
            _ => false,
        }
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &graphics::Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        match (self, renderer) {
            (Self::Primary(compositor), Renderer::Primary(renderer)) => {
                compositor.screenshot(renderer, viewport, background_color)
            }
            (Self::Secondary(compositor), Renderer::Secondary(renderer)) => {
                compositor.screenshot(renderer, viewport, background_color)
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(feature = "wgpu-bare")]
impl<A, B> iced_wgpu::primitive::Renderer for Renderer<A, B>
where
    A: iced_wgpu::primitive::Renderer,
    B: core::Renderer,
{
    fn draw_primitive(&mut self, bounds: Rectangle, primitive: impl iced_wgpu::Primitive) {
        match self {
            Self::Primary(renderer) => {
                renderer.draw_primitive(bounds, primitive);
            }
            Self::Secondary(_) => {
                log::warn!("Custom shader primitive is not supported with this renderer.");
            }
        }
    }

    fn supports_primitives(&self) -> bool {
        matches!(self, Self::Primary(_))
    }
}

#[cfg(feature = "geometry")]
mod geometry {
    use super::Renderer;
    use crate::core::{Point, Radians, Rectangle, Size, Svg, Vector};
    use crate::graphics::cache::{self, Cached};
    use crate::graphics::geometry::{self, Fill, Image, Path, Stroke, Text};

    impl<A, B> geometry::Renderer for Renderer<A, B>
    where
        A: geometry::Renderer,
        B: geometry::Renderer,
    {
        type Geometry = Geometry<A::Geometry, B::Geometry>;
        type Frame = Frame<A::Frame, B::Frame>;

        fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
            match self {
                Self::Primary(renderer) => Frame::Primary(renderer.new_frame(bounds)),
                Self::Secondary(renderer) => Frame::Secondary(renderer.new_frame(bounds)),
            }
        }

        fn draw_layout_border(
            &mut self,
            node: crate::core::renderer::NodeId,
            bounds: Rectangle,
            border: geometry::border::Border,
        ) {
            delegate!(
                self,
                renderer,
                renderer.draw_layout_border(node, bounds, border)
            );
        }

        fn draw_geometry(&mut self, geometry: Self::Geometry) {
            match (self, geometry) {
                (Self::Primary(renderer), Geometry::Primary(geometry)) => {
                    renderer.draw_geometry(geometry);
                }
                (Self::Secondary(renderer), Geometry::Secondary(geometry)) => {
                    renderer.draw_geometry(geometry);
                }
                _ => unreachable!(),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub enum Geometry<A, B> {
        Primary(A),
        Secondary(B),
    }

    impl<A, B> Cached for Geometry<A, B>
    where
        A: Cached,
        B: Cached,
    {
        type Cache = Geometry<A::Cache, B::Cache>;

        fn load(cache: &Self::Cache) -> Self {
            match cache {
                Geometry::Primary(cache) => Self::Primary(A::load(cache)),
                Geometry::Secondary(cache) => Self::Secondary(B::load(cache)),
            }
        }

        fn cache(self, group: cache::Group, previous: Option<Self::Cache>) -> Self::Cache {
            match (self, previous) {
                (Self::Primary(geometry), Some(Geometry::Primary(previous))) => {
                    Geometry::Primary(geometry.cache(group, Some(previous)))
                }
                (Self::Primary(geometry), None) => Geometry::Primary(geometry.cache(group, None)),
                (Self::Secondary(geometry), Some(Geometry::Secondary(previous))) => {
                    Geometry::Secondary(geometry.cache(group, Some(previous)))
                }
                (Self::Secondary(geometry), None) => {
                    Geometry::Secondary(geometry.cache(group, None))
                }
                _ => unreachable!(),
            }
        }
    }

    #[derive(Debug)]
    pub enum Frame<A, B> {
        Primary(A),
        Secondary(B),
    }

    impl<A, B> geometry::frame::Backend for Frame<A, B>
    where
        A: geometry::frame::Backend,
        B: geometry::frame::Backend,
    {
        type Geometry = Geometry<A::Geometry, B::Geometry>;

        fn width(&self) -> f32 {
            delegate!(self, frame, frame.width())
        }

        fn height(&self) -> f32 {
            delegate!(self, frame, frame.height())
        }

        fn size(&self) -> Size {
            delegate!(self, frame, frame.size())
        }

        fn center(&self) -> Point {
            delegate!(self, frame, frame.center())
        }

        fn fill(&mut self, path: &Path, fill: impl Into<Fill>) {
            delegate!(self, frame, frame.fill(path, fill));
        }

        fn fill_rectangle(&mut self, top_left: Point, size: Size, fill: impl Into<Fill>) {
            delegate!(self, frame, frame.fill_rectangle(top_left, size, fill));
        }

        fn stroke<'a>(&mut self, path: &Path, stroke: impl Into<Stroke<'a>>) {
            delegate!(self, frame, frame.stroke(path, stroke));
        }

        fn stroke_rectangle<'a>(
            &mut self,
            top_left: Point,
            size: Size,
            stroke: impl Into<Stroke<'a>>,
        ) {
            delegate!(self, frame, frame.stroke_rectangle(top_left, size, stroke));
        }

        fn stroke_text<'a>(&mut self, text: impl Into<Text>, stroke: impl Into<Stroke<'a>>) {
            delegate!(self, frame, frame.stroke_text(text, stroke));
        }

        fn fill_text(&mut self, text: impl Into<Text>) {
            delegate!(self, frame, frame.fill_text(text));
        }

        fn draw_image(&mut self, bounds: Rectangle, image: impl Into<Image>) {
            delegate!(self, frame, frame.draw_image(bounds, image));
        }

        fn draw_svg(&mut self, bounds: Rectangle, svg: impl Into<Svg>) {
            delegate!(self, frame, frame.draw_svg(bounds, svg));
        }

        fn push_transform(&mut self) {
            delegate!(self, frame, frame.push_transform());
        }

        fn pop_transform(&mut self) {
            delegate!(self, frame, frame.pop_transform());
        }

        fn draft(&mut self, bounds: Rectangle) -> Self {
            match self {
                Self::Primary(frame) => Self::Primary(frame.draft(bounds)),
                Self::Secondary(frame) => Self::Secondary(frame.draft(bounds)),
            }
        }

        fn paste(&mut self, frame: Self) {
            match (self, frame) {
                (Self::Primary(target), Self::Primary(source)) => {
                    target.paste(source);
                }
                (Self::Secondary(target), Self::Secondary(source)) => {
                    target.paste(source);
                }
                _ => unreachable!(),
            }
        }

        fn translate(&mut self, translation: Vector) {
            delegate!(self, frame, frame.translate(translation));
        }

        fn rotate(&mut self, angle: impl Into<Radians>) {
            delegate!(self, frame, frame.rotate(angle));
        }

        fn scale(&mut self, scale: f32) {
            delegate!(self, frame, frame.scale(scale));
        }

        fn scale_nonuniform(&mut self, scale: impl Into<Vector>) {
            delegate!(self, frame, frame.scale_nonuniform(scale));
        }

        fn into_geometry(self) -> Self::Geometry {
            match self {
                Frame::Primary(frame) => Geometry::Primary(frame.into_geometry()),
                Frame::Secondary(frame) => Geometry::Secondary(frame.into_geometry()),
            }
        }
    }
}

impl<A, B> renderer::Headless for Renderer<A, B>
where
    A: renderer::Headless,
    B: renderer::Headless,
{
    async fn new(settings: renderer::Settings, backend: Option<&str>) -> Option<Self> {
        if let Some(renderer) = A::new(settings, backend).await {
            return Some(Self::Primary(renderer));
        }

        B::new(settings, backend).await.map(Self::Secondary)
    }

    fn name(&self) -> String {
        delegate!(self, renderer, renderer.name())
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        match self {
            crate::fallback::Renderer::Primary(renderer) => {
                renderer.screenshot(size, scale_factor, background_color)
            }
            crate::fallback::Renderer::Secondary(renderer) => {
                renderer.screenshot(size, scale_factor, background_color)
            }
        }
    }
}

impl<A, B> compositor::Default for Renderer<A, B>
where
    A: compositor::Default,
    B: compositor::Default,
{
    type Compositor = Compositor<A::Compositor, B::Compositor>;
}
