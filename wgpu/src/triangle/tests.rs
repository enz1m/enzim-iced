// Modified by enzUI contributors. See FORK.md.
use crate::core::renderer::{self, Renderer as _};
use crate::core::{Color, Rectangle, Size, Transformation};
use crate::graphics::mesh::{self, Mesh, Renderer as _, SolidVertex2D};
use crate::graphics::{Antialiasing, Shell, Viewport, color};
use crate::{Engine, Renderer};

const SIZE: Size<u32> = Size::new(600, 400);
const BACKGROUND: Color = Color::BLACK;

fn renderer() -> Renderer {
    let engine = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("an adapter is required to render meshes");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                ..wgpu::DeviceDescriptor::default()
            })
            .await
            .expect("device");
        Engine::new(
            &adapter,
            device,
            queue,
            wgpu::TextureFormat::Rgba8Unorm,
            Some(Antialiasing::MSAAx4),
            Shell::headless(),
        )
    });
    Renderer::new(engine, renderer::Settings::default())
}

/// A solid rectangle filling `bounds`, clipped to them.
fn rectangle(bounds: Rectangle, fill: Color) -> Mesh {
    let color = color::pack(fill);
    let corners = [
        [bounds.x, bounds.y],
        [bounds.x + bounds.width, bounds.y],
        [bounds.x + bounds.width, bounds.y + bounds.height],
        [bounds.x, bounds.y + bounds.height],
    ];
    Mesh::Solid {
        buffers: mesh::Indexed {
            vertices: corners
                .into_iter()
                .map(|position| SolidVertex2D { position, color })
                .collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        transformation: Transformation::IDENTITY,
        clip_bounds: bounds,
    }
}

fn pixel(frame: &[u8], x: u32, y: u32) -> [u8; 3] {
    let index = ((y * SIZE.width + x) * 4) as usize;
    [frame[index], frame[index + 1], frame[index + 2]]
}

/// A small mesh layer is multisampled in a target the size of its bounds,
/// and composited back exactly where it was drawn.
///
/// Sabotage: size the target to the viewport again, and the size bound
/// fails. Drop the region translation from the projection, and the meshes
/// draw outside their targets, so the rectangles stay black. Drop the blit
/// viewport, and the target stretches over the frame, so the wrong colour
/// lands inside the near rectangle.
#[test]
fn a_small_mesh_layer_is_multisampled_in_a_target_the_size_of_its_bounds() {
    let mut renderer = renderer();
    let near = Rectangle::new([24.0, 16.0].into(), Size::new(30.0, 20.0));
    let far = Rectangle::new([520.0, 340.0].into(), Size::new(40.0, 30.0));

    for (bounds, fill) in [
        (near, Color::from_rgb(0.0, 1.0, 0.0)),
        (far, Color::from_rgb(1.0, 0.0, 0.0)),
    ] {
        renderer.start_layer(bounds);
        renderer.draw_mesh(rectangle(bounds, fill));
        renderer.end_layer();
    }

    let frame = renderer.screenshot(
        &Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        ),
        BACKGROUND,
    );

    assert_eq!(
        pixel(&frame, 39, 26),
        [0, 255, 0],
        "inside the near rectangle"
    );
    assert_eq!(
        pixel(&frame, 540, 355),
        [255, 0, 0],
        "inside the far rectangle"
    );
    for (x, y) in [
        (22, 26),
        (56, 26),
        (39, 14),
        (39, 38),
        (518, 355),
        (562, 355),
        (540, 338),
        (540, 372),
    ] {
        assert_eq!(
            pixel(&frame, x, y),
            [0, 0, 0],
            "outside both rectangles at ({x}, {y})"
        );
    }
    // Shifted copies would land at the target origin.
    assert_eq!(
        pixel(&frame, 15, 10),
        [0, 0, 0],
        "nothing at the frame origin"
    );

    let sizes = renderer
        .engine
        .triangle_pipeline
        .msaa
        .as_ref()
        .expect("multisampling is on")
        .target_sizes();
    assert!(!sizes.is_empty(), "the meshes were multisampled");
    for size in sizes {
        assert!(
            size.width <= 64 && size.height <= 64,
            "a 40x30 layer used a {}x{} target",
            size.width,
            size.height
        );
    }
}

/// A multisampled mesh inside a rounded clip far from the window's origin is
/// masked by that clip where it is drawn: the clip, given in window pixels,
/// is tested in the region target's own coordinates (enzUI-ICED #3660). A
/// chart or a gauge inside a rounded card showed nothing, or a stray band.
///
/// Sabotage: pass the window-space clip to the region target -- the mesh is
/// masked away entirely.
#[test]
fn a_multisampled_mesh_in_a_rounded_clip_is_masked_where_it_is_drawn() {
    let mut renderer = renderer();
    let card = Rectangle::new([300.0, 200.0].into(), Size::new(160.0, 120.0));
    renderer.start_layer_rounded(card, 24.0.into());
    renderer.draw_mesh(rectangle(card, Color::from_rgb(0.0, 1.0, 0.0)));
    renderer.end_layer();

    let frame = renderer.screenshot(
        &Viewport::with_physical_size(
            SIZE,
            renderer::Scale {
                window: 1.0,
                application: 1.0,
            },
        ),
        BACKGROUND,
    );

    assert_eq!(pixel(&frame, 380, 260), [0, 255, 0], "the card's middle");
    assert_eq!(pixel(&frame, 302, 260), [0, 255, 0], "its left edge");
    assert_eq!(
        pixel(&frame, 301, 201),
        [0, 0, 0],
        "its rounded top-left corner is masked"
    );
    assert_eq!(pixel(&frame, 380, 330), [0, 0, 0], "below the card");
}
