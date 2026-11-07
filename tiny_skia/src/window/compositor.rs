// Modified by enzUI contributors. See FORK.md.
use crate::core::backend;
use crate::core::renderer;
use crate::core::{Color, Rectangle, Size};
use crate::graphics::compositor::{self, Information};
use crate::graphics::damage;
use crate::graphics::{Shell, Viewport};
use crate::{Layer, Renderer};

use std::collections::VecDeque;
use std::num::NonZeroU32;

pub struct Compositor {
    context: softbuffer::Context<Box<dyn compositor::Display>>,
}

pub struct Surface {
    window: softbuffer::Surface<Box<dyn compositor::Display>, Box<dyn compositor::Window>>,
    clip_mask: tiny_skia::Mask,
    frames: VecDeque<Frame>,
    max_age: u8,
}

#[derive(Clone)]
struct Frame {
    background: Color,
    layers: Vec<Layer>,
}

impl crate::graphics::Compositor for Compositor {
    type Renderer = Renderer;
    type Surface = Surface;

    async fn new(
        settings: backend::Settings,
        display: impl compositor::Display,
        _compatible_window: impl compositor::Window,
        _shell: Shell,
    ) -> Result<Self, backend::Error> {
        if !settings.backend.is_software() && !settings.backend.matches("tiny-skia") {
            return Err(backend::Error::GraphicsAdapterNotFound {
                backend: "tiny-skia",
                reason: backend::Reason::DidNotMatch {
                    preferred_backend: settings.backend,
                },
            });
        }

        Ok(new(display))
    }

    fn create_renderer(&self, settings: renderer::Settings) -> Self::Renderer {
        Renderer::new(settings)
    }

    fn create_surface(
        &mut self,
        window: impl compositor::Window + Clone,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let window = softbuffer::Surface::new(&self.context, Box::new(window.clone()) as _)
            .expect("Create softbuffer surface for window");

        let mut surface = Surface {
            window,
            clip_mask: tiny_skia::Mask::new(1, 1).expect("Create clip mask"),
            frames: VecDeque::new(),
            max_age: 0,
        };

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        surface
            .window
            .resize(
                NonZeroU32::new(width).expect("Non-zero width"),
                NonZeroU32::new(height).expect("Non-zero height"),
            )
            .expect("Resize surface");

        surface.clip_mask = tiny_skia::Mask::new(width, height).expect("Create clip mask");
        surface.frames.clear();
    }

    fn information(&self) -> Information {
        Information {
            adapter: String::from("CPU"),
            backend: String::from("tiny-skia"),
            device: String::from("software rasterizer"),
        }
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        present(
            renderer,
            surface,
            viewport,
            background_color,
            on_pre_present,
        )
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        screenshot(renderer, viewport, background_color)
    }
}

pub fn new(display: impl compositor::Display) -> Compositor {
    #[allow(unsafe_code)]
    let context =
        softbuffer::Context::new(Box::new(display) as _).expect("Create softbuffer context");

    Compositor { context }
}

pub fn present(
    renderer: &mut Renderer,
    surface: &mut Surface,
    viewport: &Viewport,
    background: Color,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    let physical_size = viewport.physical_size();

    let mut buffer = surface
        .window
        .buffer_mut()
        .map_err(|_| compositor::SurfaceError::Lost)?;

    let last_frame = {
        let age = buffer.age();

        surface.max_age = surface.max_age.max(age);
        surface.frames.truncate(surface.max_age as usize);

        if age > 0 {
            surface.frames.get(age as usize - 1)
        } else {
            None
        }
    };

    let damage = last_frame
        .and_then(|last_frame| {
            (last_frame.background == background).then(|| {
                damage::diff(
                    &last_frame.layers,
                    renderer.layers(),
                    |layer| vec![layer.bounds],
                    Layer::damage,
                )
            })
        })
        .unwrap_or_else(|| vec![Rectangle::with_size(viewport.logical_size())]);

    let presented_damage = presentation_damage(
        &damage,
        surface.frames.front(),
        renderer.layers(),
        background,
        viewport,
    );

    if damage.is_empty() {
        if let Some(last_frame) = last_frame {
            surface.frames.push_front(last_frame.clone());
        }
    } else {
        surface.frames.push_front(Frame {
            background,
            layers: renderer.layers().to_vec(),
        });

        let damage = damage::group(damage, Rectangle::with_size(viewport.logical_size()));

        let mut pixels = tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut buffer),
            physical_size.width,
            physical_size.height,
        )
        .expect("Create pixel map");

        renderer.draw(
            &mut pixels,
            &mut surface.clip_mask,
            viewport,
            &damage,
            background,
        );
    }

    on_pre_present();
    buffer
        .present_with_damage(&presented_damage)
        .map_err(|_| compositor::SurfaceError::Lost)
}

fn presentation_damage(
    draw_damage: &[Rectangle],
    previous: Option<&Frame>,
    current: &[Layer],
    background: Color,
    viewport: &Viewport,
) -> Vec<softbuffer::Rect> {
    // Wayland surface damage compares against the last presentation, not the aged draw buffer.
    let surface_damage = previous
        .filter(|frame| frame.background == background)
        .map(|frame| {
            damage::diff(
                &frame.layers,
                current,
                |layer| vec![layer.bounds],
                Layer::damage,
            )
        })
        .unwrap_or_else(|| vec![Rectangle::with_size(viewport.logical_size())]);
    let damage = damage::group(
        draw_damage.iter().copied().chain(surface_damage).collect(),
        Rectangle::with_size(viewport.logical_size()),
    );
    physical_damage(&damage, viewport.scale_factor(), viewport.physical_size())
}

// Damage rounds outward and clamps to the physical buffer so no painted edge is omitted.
fn physical_damage(damage: &[Rectangle], scale: f32, size: Size<u32>) -> Vec<softbuffer::Rect> {
    damage
        .iter()
        .filter_map(|rect| {
            let x0 = (rect.x * scale).floor().max(0.0) as u32;
            let y0 = (rect.y * scale).floor().max(0.0) as u32;
            let x1 = ((rect.x + rect.width) * scale).ceil().max(0.0) as u32;
            let y1 = ((rect.y + rect.height) * scale).ceil().max(0.0) as u32;
            let (x1, y1) = (x1.min(size.width), y1.min(size.height));
            Some(softbuffer::Rect {
                x: x0,
                y: y0,
                width: NonZeroU32::new(x1.checked_sub(x0)?)?,
                height: NonZeroU32::new(y1.checked_sub(y0)?)?,
            })
        })
        .collect()
}

pub fn screenshot(
    renderer: &mut Renderer,
    viewport: &Viewport,
    background_color: Color,
) -> Vec<u8> {
    let size = viewport.physical_size();

    let mut offscreen_buffer: Vec<u32> = vec![0; size.width as usize * size.height as usize];

    let mut clip_mask = tiny_skia::Mask::new(size.width, size.height).expect("Create clip mask");

    renderer.draw(
        &mut tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut offscreen_buffer),
            size.width,
            size.height,
        )
        .expect("Create offscreen pixel map"),
        &mut clip_mask,
        viewport,
        &[Rectangle::with_size(Size::new(
            size.width as f32,
            size.height as f32,
        ))],
        background_color,
    );

    offscreen_buffer.iter().fold(
        Vec::with_capacity(offscreen_buffer.len() * 4),
        |mut acc, pixel| {
            const A_MASK: u32 = 0xFF_00_00_00;
            const R_MASK: u32 = 0x00_FF_00_00;
            const G_MASK: u32 = 0x00_00_FF_00;
            const B_MASK: u32 = 0x00_00_00_FF;

            let a = ((A_MASK & pixel) >> 24) as u8;
            let r = ((R_MASK & pixel) >> 16) as u8;
            let g = ((G_MASK & pixel) >> 8) as u8;
            let b = (B_MASK & pixel) as u8;

            acc.extend([r, g, b, a]);
            acc
        },
    )
}

#[cfg(test)]
mod damage_tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    fn viewport() -> Viewport {
        Viewport::with_physical_size(
            Size::new(800, 500),
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        )
    }

    fn frame(quad: bool) -> Frame {
        use crate::core::Renderer as _;
        let viewport = viewport();
        let mut renderer = Renderer::new(renderer::Settings::default());
        let bounds = Rectangle::with_size(viewport.logical_size());
        renderer.reset(bounds);
        renderer.with_layer(bounds, |renderer| {
            if quad {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: rect(350.0, 150.0, 200.0, 200.0),
                        border: crate::core::Border::default(),
                        shadow: crate::core::Shadow::default(),
                        snap: true,
                    },
                    Color::from_rgb(0.0, 1.0, 0.0),
                );
            }
        });
        Frame {
            background: Color::BLACK,
            layers: renderer.layers().to_vec(),
        }
    }

    fn drawing_damage(previous: &Frame, current: &Frame) -> Vec<Rectangle> {
        damage::diff(
            &previous.layers,
            &current.layers,
            |layer| vec![layer.bounds],
            Layer::damage,
        )
    }

    fn assert_quad_surface_damage(buffer: &Frame, displayed: &Frame, current: &Frame) {
        let draw_damage = drawing_damage(buffer, current);
        assert!(
            draw_damage.is_empty(),
            "aged buffer already holds the current image"
        );
        let output = presentation_damage(
            &draw_damage,
            Some(displayed),
            &current.layers,
            current.background,
            &viewport(),
        );
        for (x, y) in [(350, 150), (549, 150), (350, 349), (549, 349)] {
            assert!(
                output.iter().any(|rect| {
                    rect.x <= x
                        && x < rect.x + rect.width.get()
                        && rect.y <= y
                        && y < rect.y + rect.height.get()
                }),
                "presented quad corner {x},{y} must be damaged: {output:?}"
            );
        }
    }

    #[test]
    fn buffer_age_removed_quad_damages_the_last_presented_surface() {
        assert_quad_surface_damage(&frame(false), &frame(true), &frame(false));
    }

    #[test]
    fn buffer_age_added_quad_damages_the_last_presented_surface() {
        assert_quad_surface_damage(&frame(true), &frame(false), &frame(true));
    }

    #[test]
    fn buffer_age_background_change_damages_the_whole_surface() {
        let displayed = frame(false);
        let mut current = frame(false);
        current.background = Color::WHITE;
        let output = presentation_damage(
            &[],
            Some(&displayed),
            &current.layers,
            current.background,
            &viewport(),
        );
        assert_eq!(output.len(), 1);
        assert_eq!(
            (
                output[0].x,
                output[0].y,
                output[0].width.get(),
                output[0].height.get()
            ),
            (0, 0, 800, 500),
        );
    }

    #[test]
    fn buffer_age_draw_damage_is_retained_when_surface_is_unchanged() {
        let buffer = frame(true);
        let displayed = frame(false);
        let current = frame(false);
        let draw_damage = drawing_damage(&buffer, &current);
        assert!(!draw_damage.is_empty());
        let output = presentation_damage(
            &draw_damage,
            Some(&displayed),
            &current.layers,
            current.background,
            &viewport(),
        );
        assert!(output.iter().any(|rect| rect.x <= 350
            && 550 <= rect.x + rect.width.get()
            && rect.y <= 150
            && 350 <= rect.y + rect.height.get()));
    }

    #[test]
    fn buffer_age_unchanged_buffer_and_surface_have_no_damage() {
        for visible in [false, true] {
            let previous = frame(visible);
            let current = frame(visible);
            let damage = drawing_damage(&previous, &current);
            assert!(damage.is_empty());
            assert!(
                presentation_damage(
                    &damage,
                    Some(&previous),
                    &current.layers,
                    current.background,
                    &viewport(),
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn damage_scales_rounds_outward_and_clamps() {
        let size = Size::new(200, 100);
        let out = physical_damage(&[rect(10.25, 5.5, 20.5, 10.0)], 2.0, size);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].x, out[0].y), (20, 11));
        assert_eq!((out[0].width.get(), out[0].height.get()), (42, 20));

        let clamped = physical_damage(&[rect(90.0, 40.0, 50.0, 50.0)], 2.0, size);
        assert_eq!(
            (
                clamped[0].x + clamped[0].width.get(),
                clamped[0].y + clamped[0].height.get()
            ),
            (200, 100)
        );
        assert!(physical_damage(&[rect(0.0, 0.0, 0.0, 5.0)], 1.0, size).is_empty());
        assert!(physical_damage(&[rect(300.0, 0.0, 5.0, 5.0)], 1.0, size).is_empty());
    }
}
