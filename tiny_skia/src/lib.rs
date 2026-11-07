// Modified by enzUI contributors. See FORK.md.
#![allow(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod window;

mod engine;
mod layer;
mod primitive;
mod text;

#[cfg(feature = "image")]
mod raster;

#[cfg(feature = "svg")]
mod vector;

#[cfg(feature = "geometry")]
pub mod geometry;

use iced_debug as debug;
pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use layer::Layer;
pub use primitive::Primitive;

use layer::AlphaGroup;

#[cfg(feature = "geometry")]
pub use geometry::Geometry;

use crate::core::renderer;
use crate::core::{Background, Color, Font, Point, Rectangle, Size, Transformation};
use crate::engine::Engine;
use crate::graphics::Viewport;
use crate::graphics::compositor;
use crate::graphics::text::{Editor, Paragraph};

/// A [`tiny-skia`] graphics renderer for [`iced`].
///
/// [`tiny-skia`]: https://github.com/RazrFalcon/tiny-skia
/// [`iced`]: https://github.com/iced-rs/iced
#[derive(Debug)]
pub struct Renderer {
    settings: renderer::Settings,
    layers: layer::Stack,
    engine: Engine, // TODO: Shared engine
    active_alpha_groups: Vec<AlphaGroup>,
    layer_scopes: Vec<Option<AlphaGroup>>,
    next_alpha_group: usize,
}

struct AlphaSurface {
    group: AlphaGroup,
    pixmap: tiny_skia::Pixmap,
}

fn composite_alpha_surface(surfaces: &mut Vec<AlphaSurface>, root: &mut tiny_skia::PixmapMut<'_>) {
    let surface = surfaces.pop().expect("an alpha surface is active");

    // Avoid touching the destination at all for the transparent endpoint. It
    // is a byte-for-byte no-op, not merely visually equivalent.
    if surface.group.alpha <= 0.0 {
        return;
    }

    let paint = tiny_skia::PixmapPaint {
        opacity: surface.group.alpha,
        ..tiny_skia::PixmapPaint::default()
    };

    if let Some(parent) = surfaces.last_mut() {
        parent.pixmap.as_mut().draw_pixmap(
            0,
            0,
            surface.pixmap.as_ref(),
            &paint,
            tiny_skia::Transform::identity(),
            None,
        );
    } else {
        root.draw_pixmap(
            0,
            0,
            surface.pixmap.as_ref(),
            &paint,
            tiny_skia::Transform::identity(),
            None,
        );
    }
}

impl Renderer {
    pub fn new(settings: renderer::Settings) -> Self {
        Self {
            settings,
            layers: layer::Stack::new(),
            engine: Engine::new(),
            active_alpha_groups: Vec::new(),
            layer_scopes: Vec::new(),
            next_alpha_group: 0,
        }
    }

    fn tag_current_layer(&mut self) {
        let groups = self.active_alpha_groups.clone();
        let (layer, _) = self.layers.current_mut();
        layer.alpha_groups = groups;
    }

    pub fn layers(&mut self) -> &[Layer] {
        self.layers.flush();
        self.layers.as_slice()
    }

    pub fn draw(
        &mut self,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut tiny_skia::Mask,
        viewport: &Viewport,
        damage: &[Rectangle],
        background_color: Color,
    ) {
        let scale_factor = viewport.scale_factor();
        self.layers.flush();

        for &damage_bounds in damage {
            let damage_bounds = damage_bounds * scale_factor;

            let path = tiny_skia::PathBuilder::from_rect(
                tiny_skia::Rect::from_xywh(
                    damage_bounds.x,
                    damage_bounds.y,
                    damage_bounds.width,
                    damage_bounds.height,
                )
                .expect("Create damage rectangle"),
            );

            pixels.fill_path(
                &path,
                &tiny_skia::Paint {
                    shader: tiny_skia::Shader::SolidColor(engine::into_color(background_color)),
                    anti_alias: false,
                    blend_mode: tiny_skia::BlendMode::Source,
                    ..Default::default()
                },
                tiny_skia::FillRule::default(),
                tiny_skia::Transform::identity(),
                None,
            );

            // Each active alpha group owns a transparent framebuffer. Layers
            // render at full strength into the innermost framebuffer, then a
            // completed group is blended once into its parent. This is what
            // makes sibling overlap stable and nested alpha multiplicative.
            let mut alpha_surfaces: Vec<AlphaSurface> = Vec::new();

            for layer in self.layers.iter() {
                let common = alpha_surfaces
                    .iter()
                    .zip(&layer.alpha_groups)
                    .take_while(|(surface, group)| surface.group.id == group.id)
                    .count();

                while alpha_surfaces.len() > common {
                    composite_alpha_surface(&mut alpha_surfaces, pixels);
                }

                for &group in &layer.alpha_groups[common..] {
                    alpha_surfaces.push(AlphaSurface {
                        group,
                        pixmap: tiny_skia::Pixmap::new(pixels.width(), pixels.height())
                            .expect("Create alpha layer framebuffer"),
                    });
                }

                let scaled_layer_bounds = layer.bounds * scale_factor;
                let Some(layer_bounds) = damage_bounds.intersection(&scaled_layer_bounds) else {
                    continue;
                };

                if let Some(surface) = alpha_surfaces.last_mut() {
                    let mut target = surface.pixmap.as_mut();
                    Self::draw_layer(
                        &mut self.engine,
                        layer,
                        &mut target,
                        clip_mask,
                        scale_factor,
                        scaled_layer_bounds,
                        layer_bounds,
                        damage_bounds,
                    );
                } else {
                    Self::draw_layer(
                        &mut self.engine,
                        layer,
                        pixels,
                        clip_mask,
                        scale_factor,
                        scaled_layer_bounds,
                        layer_bounds,
                        damage_bounds,
                    );
                }
            }

            while !alpha_surfaces.is_empty() {
                composite_alpha_surface(&mut alpha_surfaces, pixels);
            }
        }

        self.engine.trim();
    }

    fn draw_layer(
        engine: &mut Engine,
        layer: &Layer,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut tiny_skia::Mask,
        scale_factor: f32,
        scaled_layer_bounds: Rectangle,
        layer_bounds: Rectangle,
        // The region being redrawn, which was cleared to the background.
        damage_bounds: Rectangle,
    ) {
        // enzICED #41: a rounded clip layer masks to the corner arc of its
        // FULL (scaled) bounds, not the damage-cropped rectangle — otherwise
        // partial-damage repaints would move the arc. `force_mask` keeps it on
        // primitives inside the rectangular fast path.
        //
        // enzICED #319: radius and bounds are both stored in logical
        // coordinates, so both must receive the same viewport scale. Passing
        // an unscaled radius alongside scaled bounds visibly squares the arc on
        // HiDPI and disagrees with wgpu's `clip_region_of` conversion.
        let rounded_clip = layer
            .clip_radius
            .filter(|r| {
                r.top_left > 0.0 || r.top_right > 0.0 || r.bottom_right > 0.0 || r.bottom_left > 0.0
            })
            .map(|radius| radius * scale_factor);
        let force_mask = rounded_clip.is_some();

        if let Some(radius) = rounded_clip {
            engine::adjust_clip_mask_rounded(
                clip_mask,
                scaled_layer_bounds,
                radius.into(),
                damage_bounds,
            );
        } else {
            engine::adjust_clip_mask(clip_mask, layer_bounds);
        }

        if !layer.quads.is_empty() {
            let render_span = debug::render(debug::Primitive::Quad);
            for (quad, background) in &layer.quads {
                engine.draw_quad(
                    quad,
                    background,
                    Transformation::scale(scale_factor),
                    pixels,
                    clip_mask,
                    layer_bounds,
                    force_mask,
                );
            }
            render_span.finish();
        }

        if !layer.primitives.is_empty() {
            let render_span = debug::render(debug::Primitive::Triangle);

            for group in &layer.primitives {
                let Some(group_bounds) =
                    (group.clip_bounds() * scale_factor).intersection(&layer_bounds)
                else {
                    continue;
                };

                // A primitive group's rectangular sub-clip would overwrite a
                // rounded layer mask. Re-install the rounded mask instead;
                // `force_mask` then keeps it applied (#41 stage 3).
                if let Some(radius) = rounded_clip {
                    engine::adjust_clip_mask_rounded(
                        clip_mask,
                        scaled_layer_bounds,
                        radius.into(),
                        damage_bounds,
                    );
                } else {
                    engine::adjust_clip_mask(clip_mask, group_bounds);
                }

                for primitive in group.as_slice() {
                    engine.draw_primitive(
                        primitive,
                        Transformation::scale(scale_factor) * group.transformation(),
                        pixels,
                        clip_mask,
                        group_bounds,
                        force_mask,
                    );
                }

                if rounded_clip.is_none() {
                    engine::adjust_clip_mask(clip_mask, layer_bounds);
                }
            }

            render_span.finish();
        }

        if !layer.images.is_empty() {
            let render_span = debug::render(debug::Primitive::Image);

            for image in &layer.images {
                engine.draw_image(
                    image,
                    Transformation::scale(scale_factor),
                    pixels,
                    clip_mask,
                    layer_bounds,
                    force_mask,
                );
            }

            render_span.finish();
        }

        if !layer.text.is_empty() {
            let render_span = debug::render(debug::Primitive::Image);

            for group in &layer.text {
                for text in group.as_slice() {
                    engine.draw_text(
                        text,
                        Transformation::scale(scale_factor) * group.transformation(),
                        pixels,
                        clip_mask,
                        layer_bounds,
                        force_mask,
                    );
                }
            }

            render_span.finish();
        }
    }
}

impl core::Renderer for Renderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push_clip(bounds);
        self.tag_current_layer();
        self.layer_scopes.push(None);
    }

    fn start_layer_alpha(&mut self, bounds: Rectangle, alpha: f32) {
        self.layers.push_clip(bounds);

        // Non-finite alpha is treated as the backwards-compatible opaque
        // endpoint. Values outside the documented range are clamped.
        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            1.0
        };

        // The opaque endpoint intentionally takes the old direct-render path,
        // which guarantees byte identity instead of relying on a round trip
        // through a transparent framebuffer.
        let group = (alpha < 1.0).then(|| {
            let group = AlphaGroup {
                id: self.next_alpha_group,
                alpha,
            };
            self.next_alpha_group += 1;
            self.active_alpha_groups.push(group);
            group
        });

        self.tag_current_layer();
        self.layer_scopes.push(group);
    }

    // enzICED #41: tiny_skia honours rounded clipping for real — it records the
    // radius on the layer, and the present pass masks the corners with
    // `adjust_clip_mask_rounded`.
    fn start_layer_rounded(&mut self, bounds: Rectangle, radius: core::border::Radius) {
        self.layers.push_clip_rounded(bounds, radius);
        self.tag_current_layer();
        self.layer_scopes.push(None);
    }

    fn end_layer(&mut self) {
        self.layers.pop_clip();

        if let Some(group) = self.layer_scopes.pop().expect("a layer scope is active") {
            let popped = self
                .active_alpha_groups
                .pop()
                .expect("an alpha group is active");
            debug_assert_eq!(popped.id, group.id);
        }
        self.layers.split_clip_continuation();
        self.tag_current_layer();
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        self.layers.push_transformation(transformation);
    }

    fn end_transformation(&mut self) {
        self.layers.pop_transformation();
    }

    fn fill_quad(&mut self, quad: renderer::Quad, background: impl Into<Background>) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_quad(quad, background.into(), transformation);
    }

    fn allocate_image(
        &self,
        _handle: &core::image::Handle,
        callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>) + Send + 'static,
    ) {
        #[cfg(feature = "image")]
        #[allow(unsafe_code)]
        // TODO: Concurrency
        callback(self.engine.raster_pipeline.load(_handle));

        #[cfg(not(feature = "image"))]
        callback(Err(core::image::Error::Unsupported));
    }

    fn hint(&mut self, _scale: renderer::Scale) {
        // TODO: No hinting supported
        // We'll replace `tiny-skia` with `vello_cpu` soon
    }

    fn scale(&self) -> Option<renderer::Scale> {
        None
    }

    fn current_transformation(&self) -> Transformation {
        // The layer stack composes every pushed transformation onto its
        // parent, so `transformation()` is already the cumulative transform
        // in effect (enzICED #46).
        self.layers.transformation()
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        self.layers.reset(new_bounds);
        self.active_alpha_groups.clear();
        self.layer_scopes.clear();
        self.next_alpha_group = 0;
    }

    fn settings(&self) -> renderer::Settings {
        self.settings
    }
}

impl core::text::Renderer for Renderer {
    type Paragraph = Paragraph;
    type Editor = Editor;

    const ICON_FONT: Font = Font::new("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();

        layer.draw_paragraph(text, position, color, clip_bounds, transformation);
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_editor(editor, position, color, clip_bounds, transformation);
    }

    fn fill_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text(text, position, color, clip_bounds, transformation);
    }
}

impl graphics::text::Renderer for Renderer {
    fn fill_raw(&mut self, raw: graphics::text::Raw) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text_raw(raw, transformation);
    }
}

#[cfg(feature = "geometry")]
impl graphics::geometry::Renderer for Renderer {
    type Geometry = Geometry;
    type Frame = geometry::Frame;

    fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
        geometry::Frame::new(bounds)
    }

    fn draw_geometry(&mut self, geometry: Self::Geometry) {
        let (layer, transformation) = self.layers.current_mut();

        match geometry {
            Geometry::Live {
                primitives,
                images,
                text,
                clip_bounds,
            } => {
                layer.draw_primitive_group(primitives, clip_bounds, transformation);

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, clip_bounds, transformation);
            }
            Geometry::Cache(cache) => {
                layer.draw_primitive_cache(cache.primitives, cache.clip_bounds, transformation);

                for image in cache.images.iter() {
                    layer.draw_image(image.clone(), transformation);
                }

                layer.draw_text_cache(cache.text, cache.clip_bounds, transformation);
            }
        }
    }
}

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, _mesh: graphics::Mesh) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }

    fn draw_mesh_cache(&mut self, _cache: iced_graphics::mesh::Cache) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }
}

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.engine.raster_pipeline.load(handle)
    }

    fn measure_image(&self, handle: &Self::Handle) -> Option<crate::core::Size<u32>> {
        self.engine.raster_pipeline.dimensions(handle)
    }

    fn draw_image(&mut self, image: core::Image, bounds: Rectangle, clip_bounds: Rectangle) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_raster(image, bounds, clip_bounds, transformation);
    }
}

#[cfg(feature = "svg")]
impl core::svg::Renderer for Renderer {
    fn measure_svg(&self, handle: &core::svg::Handle) -> crate::core::Size<u32> {
        self.engine.vector_pipeline.viewport_dimensions(handle)
    }

    fn draw_svg(&mut self, svg: core::Svg, bounds: Rectangle, clip_bounds: Rectangle) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_svg(svg, bounds, clip_bounds, transformation);
    }
}

impl compositor::Default for Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(settings: renderer::Settings, backend: Option<&str>) -> Option<Self> {
        if backend.is_some_and(|backend| !["tiny-skia", "tiny_skia", "software"].contains(&backend))
        {
            return None;
        }

        Some(Self::new(settings))
    }

    fn name(&self) -> String {
        "tiny-skia".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        let viewport = Viewport::with_physical_size(
            size,
            renderer::Scale {
                window: 1.0,
                application: scale_factor,
            },
        );

        window::compositor::screenshot(self, &viewport, background_color)
    }
}
