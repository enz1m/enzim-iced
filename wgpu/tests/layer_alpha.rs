// Modified by enzUI contributors. See FORK.md.
//! Real-adapter pixel gates for wgpu subtree-alpha compositing.

use iced_wgpu::Renderer;
use iced_wgpu::core::alignment;
use iced_wgpu::core::image::{Image, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::svg::{Handle as SvgHandle, Renderer as _, Svg};
use iced_wgpu::core::text::{Alignment, Ellipsis, LineHeight, Renderer as _, Shaping, Wrapping};
use iced_wgpu::core::{
    Background, Border, Color, Font, Pixels, Point, Rectangle, Shadow, Size, Text, Transformation,
};
use iced_wgpu::graphics::Viewport;
use iced_wgpu::graphics::color;
use iced_wgpu::graphics::mesh::{self, Mesh, Renderer as _, SolidVertex2D};
use iced_wgpu::primitive::Renderer as _;
use iced_wgpu::{Primitive as WgpuPrimitive, wgpu};

const W: u32 = 160;
const H: u32 = 120;
const BG: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};
const FG: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const SQUARE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><rect width="32" height="32" fill="#ff0000"/></svg>"##;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Quad,
    Text,
    Image,
    Svg,
    Mesh,
    CustomDraw,
    CustomRender,
}

fn try_renderer() -> Option<Renderer> {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
}

fn full() -> Rectangle {
    Rectangle::with_size(Size::new(W as f32, H as f32))
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
    Rectangle {
        x,
        y,
        width,
        height,
    }
}

fn text() -> Text {
    Text {
        content: String::from("MMMM"),
        bounds: Size::new(145.0, 48.0),
        size: Pixels(40.0),
        line_height: LineHeight::Relative(1.0),
        font: Font::DEFAULT,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: Ellipsis::None,
        hint_factor: None,
    }
}

fn solid_mesh(bounds: Rectangle, fill: Color) -> Mesh {
    let fill = color::pack(fill);
    let x2 = bounds.x + bounds.width;
    let y2 = bounds.y + bounds.height;
    Mesh::Solid {
        buffers: mesh::Indexed {
            vertices: vec![
                SolidVertex2D {
                    position: [bounds.x, bounds.y],
                    color: fill,
                },
                SolidVertex2D {
                    position: [x2, bounds.y],
                    color: fill,
                },
                SolidVertex2D {
                    position: [x2, y2],
                    color: fill,
                },
                SolidVertex2D {
                    position: [bounds.x, y2],
                    color: fill,
                },
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        transformation: Transformation::IDENTITY,
        clip_bounds: full(),
    }
}

fn red_mesh() -> Mesh {
    solid_mesh(rect(12.0, 56.0, 32.0, 32.0), FG)
}

fn draw(renderer: &mut Renderer, kind: Kind) {
    match kind {
        Kind::Quad => renderer.fill_quad(
            Quad {
                bounds: rect(12.0, 12.0, 32.0, 32.0),
                border: Border::default(),
                shadow: Shadow::default(),
                snap: false,
            },
            Background::Color(FG),
        ),
        Kind::Text => renderer.fill_text(text(), Point::new(8.0, 28.0), FG, full()),
        Kind::Image => renderer.draw_image(
            Image::new(iced_wgpu::core::image::Handle::from_rgba(
                1,
                1,
                vec![255, 0, 0, 255],
            )),
            rect(56.0, 12.0, 32.0, 32.0),
            full(),
        ),
        Kind::Svg => renderer.draw_svg(
            Svg::new(SvgHandle::from_memory(SQUARE_SVG)),
            rect(100.0, 12.0, 32.0, 32.0),
            full(),
        ),
        Kind::Mesh => renderer.draw_mesh(red_mesh()),
        Kind::CustomDraw => renderer.draw_primitive(
            rect(56.0, 56.0, 32.0, 32.0),
            RedPrimitive {
                separate_pass: false,
            },
        ),
        Kind::CustomRender => renderer.draw_primitive(
            rect(100.0, 56.0, 32.0, 32.0),
            RedPrimitive {
                separate_pass: true,
            },
        ),
    }
}

fn frame(kind: Kind, alpha: Option<f32>) -> Option<(Vec<u8>, iced_wgpu::AlphaGroupStatistics)> {
    let mut renderer = try_renderer()?;
    if let Some(alpha) = alpha {
        renderer.start_layer_alpha(full(), alpha);
    }
    draw(&mut renderer, kind);
    if alpha.is_some() {
        renderer.end_layer();
    }
    let pixels = <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, BG);
    Some((pixels, renderer.alpha_group_statistics()))
}

fn blank() -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        BG,
    ))
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn strongest_ink(buf: &[u8]) -> (u32, u32) {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .max_by_key(|&(x, y)| {
            let p = px(buf, x, y);
            i16::from(p[0]) - i16::from(p[2])
        })
        .expect("the framebuffer is non-empty")
}

fn assert_half_blend(kind: Kind, probe: Option<(u32, u32)>) {
    let Some((opaque, _)) = frame(kind, None) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    let (half, statistics) = frame(kind, Some(0.5)).expect("adapter remained available");
    let background = blank().expect("adapter remained available");
    let (x, y) = probe.unwrap_or_else(|| strongest_ink(&opaque));
    let background = px(&background, x, y);
    let opaque = px(&opaque, x, y);
    let half = px(&half, x, y);

    assert_ne!(
        opaque, background,
        "{kind:?} positive control drew no pixel"
    );
    for channel in 0..4 {
        let expected = if channel == 3 {
            (u16::from(opaque[channel]) + u16::from(background[channel]) + 1) / 2
        } else {
            let linear = |byte: u8| {
                let value = f32::from(byte) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            let srgb = |value: f32| {
                let value = if value <= 0.003_130_8 {
                    value * 12.92
                } else {
                    1.055 * value.powf(1.0 / 2.4) - 0.055
                };
                (value * 255.0).round() as u16
            };
            srgb((linear(opaque[channel]) + linear(background[channel])) / 2.0)
        };
        // One 8-bit quantization step may occur in the offscreen target and a
        // second in the screenshot conversion; ±2 is therefore the format's
        // worst-case precision allowance, not an empirical threshold.
        assert!(
            (i16::from(half[channel]) - expected as i16).abs() <= 2,
            "{kind:?} channel {channel} at ({x},{y}) was {half:?}; expected sRGB-correct halfway between opaque {opaque:?} and backdrop {background:?}"
        );
    }
    assert_eq!(statistics.groups, 1);
    assert_eq!(statistics.composites, 1);
    assert_eq!(statistics.retained_targets, 1);
}

#[test]
fn every_render_branch_is_composited_at_layer_alpha() {
    for (kind, probe) in [
        (Kind::Quad, Some((24, 24))),
        (Kind::Text, None),
        (Kind::Image, Some((68, 24))),
        (Kind::Svg, Some((112, 24))),
        (Kind::Mesh, Some((24, 68))),
        (Kind::CustomDraw, Some((68, 68))),
        (Kind::CustomRender, Some((112, 68))),
    ] {
        assert_half_blend(kind, probe);
    }
}

#[test]
fn alpha_one_is_byte_identical_and_allocates_no_target() {
    for kind in [
        Kind::Quad,
        Kind::Text,
        Kind::Image,
        Kind::Svg,
        Kind::Mesh,
        Kind::CustomDraw,
        Kind::CustomRender,
    ] {
        let Some((opaque, _)) = frame(kind, None) else {
            eprintln!("SKIP: no wgpu adapter available");
            return;
        };
        let (alpha_one, statistics) = frame(kind, Some(1.0)).expect("adapter remained available");
        assert_eq!(alpha_one, opaque, "opaque endpoint changed {kind:?} pixels");
        assert_eq!(statistics.groups, 0);
        assert_eq!(statistics.target_allocations, 0);
        assert_eq!(statistics.retained_targets, 0);
    }
}

#[test]
fn alpha_zero_leaves_backdrop_byte_identical_without_destination_composite() {
    let Some(backdrop) = blank() else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    for kind in [Kind::Quad, Kind::Text, Kind::Image, Kind::Svg, Kind::Mesh] {
        let (transparent, statistics) = frame(kind, Some(0.0)).expect("adapter remained available");
        assert_eq!(
            transparent, backdrop,
            "transparent endpoint changed {kind:?}"
        );
        assert_eq!(statistics.composites, 0);
        assert_eq!(statistics.zero_alpha_skips, 1);
    }
}

fn quad_frame(draw_quads: impl FnOnce(&mut Renderer)) -> Option<Vec<u8>> {
    let mut renderer = try_renderer()?;
    draw_quads(&mut renderer);
    Some(<Renderer as Headless>::screenshot(
        &mut renderer,
        Size::new(W, H),
        1.0,
        Color::BLACK,
    ))
}

fn white_quad(renderer: &mut Renderer, bounds: Rectangle) {
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(Color::WHITE),
    );
}

#[test]
fn full_frame_target_is_reused_across_frames() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    renderer.start_layer_alpha(full(), 0.5);
    white_quad(&mut renderer, rect(10.0, 10.0, 80.0, 80.0));
    renderer.end_layer();

    let _ = <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);
    let first = renderer.alpha_group_statistics();
    let _ = <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);
    let second = renderer.alpha_group_statistics();

    assert_eq!(first.target_allocations, 1);
    assert_eq!(
        second.target_allocations, 1,
        "the second frame allocated a fresh target"
    );
    assert_eq!(second.retained_targets, 1);
    assert_eq!(second.retained_bytes, u64::from(W) * u64::from(H) * 4);
}

#[test]
fn nested_alpha_layers_multiply() {
    let Some(nested) = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.5);
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
        renderer.end_layer();
        renderer.end_layer();
    }) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    let quarter = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.25);
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
        renderer.end_layer();
    })
    .expect("adapter remained available");
    let opaque = quad_frame(|renderer| {
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
    })
    .expect("adapter remained available");
    let nested = px(&nested, 30, 30);
    let quarter = px(&quarter, 30, 30);
    let opaque = px(&opaque, 30, 30);
    for channel in 0..4 {
        assert!(
            (i16::from(nested[channel]) - i16::from(quarter[channel])).abs() <= 2,
            "0.5 inside 0.5 did not match 0.25: nested={nested:?}, quarter={quarter:?}"
        );
    }
    assert_ne!(
        nested, opaque,
        "nested alpha matched the opaque control; both groups were ignored"
    );
}

#[test]
fn alpha_group_stays_between_surrounding_content() {
    let Some(frame) = quad_frame(|renderer| {
        white_quad(renderer, rect(10.0, 10.0, 50.0, 50.0));
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(30.0, 10.0, 50.0, 50.0));
        renderer.end_layer();
        renderer.fill_quad(
            Quad {
                bounds: rect(50.0, 10.0, 50.0, 50.0),
                border: Border::default(),
                shadow: Shadow::default(),
                snap: false,
            },
            Background::Color(FG),
        );
    }) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };

    let before_plus_group = px(&frame, 40, 30);
    let after_over_group = px(&frame, 60, 30);
    assert!(
        before_plus_group[0] > 240 && before_plus_group[1] > 240,
        "group did not paint after preceding content: {before_plus_group:?}"
    );
    assert!(
        after_over_group[0] > 240 && after_over_group[1] < 10 && after_over_group[2] < 10,
        "content recorded after the group did not remain on top: {after_over_group:?}"
    );
}

#[test]
fn overlapping_primitives_are_composited_once() {
    let Some(grouped) = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(10.0, 10.0, 70.0, 70.0));
        white_quad(renderer, rect(50.0, 10.0, 70.0, 70.0));
        renderer.end_layer();
    }) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };

    let opaque = quad_frame(|renderer| {
        white_quad(renderer, rect(10.0, 10.0, 70.0, 70.0));
        white_quad(renderer, rect(50.0, 10.0, 70.0, 70.0));
    })
    .expect("adapter remained available");
    let single = px(&grouped, 30, 30);
    let overlap = px(&grouped, 60, 30);
    let opaque_overlap = px(&opaque, 60, 30);
    assert_eq!(overlap, single, "overlap was alpha-scaled per primitive");
    assert_ne!(
        overlap, opaque_overlap,
        "group overlap matched the opaque control; alpha was ignored"
    );
}

#[test]
fn equal_alpha_sibling_groups_remain_distinct() {
    let Some(grouped) = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(10.0, 10.0, 70.0, 70.0));
        renderer.end_layer();

        // Deliberately use a later primitive level for the second group. Quad
        // then mesh are mergeable in the flattened stack unless group identity
        // and the post-boundary merge segment both reject the merge.
        renderer.start_layer_alpha(full(), 0.5);
        renderer.draw_mesh(solid_mesh(rect(50.0, 10.0, 70.0, 70.0), Color::WHITE));
        renderer.end_layer();
    }) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };

    let first_only = px(&grouped, 30, 30);
    let second_only = px(&grouped, 100, 30);
    let overlap = px(&grouped, 60, 30);
    assert_eq!(first_only, second_only);
    assert!(
        overlap[0] > first_only[0] + 20,
        "equal-alpha siblings were merged into one group: first={first_only:?}, overlap={overlap:?}"
    );
}

#[test]
#[ignore = "release-only real-adapter measurement; run explicitly"]
fn release_two_group_frame_cost() {
    assert!(
        !cfg!(debug_assertions),
        "performance measurement must use --release"
    );
    const PERF_W: u32 = 1440;
    const PERF_H: u32 = 900;
    const SAMPLES: usize = 40;

    fn renderer(grouped: bool) -> Option<Renderer> {
        let mut renderer = try_renderer()?;
        let full = Rectangle::with_size(Size::new(PERF_W as f32, PERF_H as f32));
        let left = Rectangle {
            x: 0.0,
            y: 0.0,
            width: PERF_W as f32 / 2.0,
            height: PERF_H as f32,
        };
        let right = Rectangle {
            x: PERF_W as f32 / 2.0,
            ..left
        };
        for bounds in [left, right] {
            if grouped {
                renderer.start_layer_alpha(full, 0.5);
            }
            white_quad(&mut renderer, bounds);
            if grouped {
                renderer.end_layer();
            }
        }
        Some(renderer)
    }

    let Some(mut direct) = renderer(false) else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    let mut grouped = renderer(true).expect("adapter remained available");
    let size = Size::new(PERF_W, PERF_H);
    for _ in 0..4 {
        let _ = <Renderer as Headless>::screenshot(&mut direct, size, 1.0, Color::BLACK);
        let _ = <Renderer as Headless>::screenshot(&mut grouped, size, 1.0, Color::BLACK);
    }

    let mut deltas = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let (direct_elapsed, grouped_elapsed) = if sample.is_multiple_of(2) {
            let start = std::time::Instant::now();
            let _ = <Renderer as Headless>::screenshot(&mut direct, size, 1.0, Color::BLACK);
            let direct_elapsed = start.elapsed();
            let start = std::time::Instant::now();
            let _ = <Renderer as Headless>::screenshot(&mut grouped, size, 1.0, Color::BLACK);
            (direct_elapsed, start.elapsed())
        } else {
            let start = std::time::Instant::now();
            let _ = <Renderer as Headless>::screenshot(&mut grouped, size, 1.0, Color::BLACK);
            let grouped_elapsed = start.elapsed();
            let start = std::time::Instant::now();
            let _ = <Renderer as Headless>::screenshot(&mut direct, size, 1.0, Color::BLACK);
            (start.elapsed(), grouped_elapsed)
        };
        deltas.push(grouped_elapsed.as_secs_f64() - direct_elapsed.as_secs_f64());
    }
    deltas.sort_by(f64::total_cmp);
    let median_ms = deltas[SAMPLES / 2] * 1000.0;
    let per_group_ms = median_ms / 2.0;
    let frame_fraction = per_group_ms / 16.67;
    let statistics = grouped.alpha_group_statistics();
    println!(
        "release alpha groups: median two-group delta={median_ms:.4} ms, per-group={per_group_ms:.4} ms, per-group fraction of 16.67 ms={frame_fraction:.4}; statistics={statistics:?}"
    );
    assert_eq!(statistics.groups, 2);
    assert_eq!(statistics.max_depth, 1);
    assert_eq!(statistics.retained_targets, 1);
    assert_eq!(
        statistics.retained_bytes,
        u64::from(PERF_W) * u64::from(PERF_H) * 4
    );
}

#[derive(Debug)]
struct RedPrimitive {
    separate_pass: bool,
}

impl WgpuPrimitive for RedPrimitive {
    type Pipeline = RedPipeline;

    fn prepare(
        &self,
        _pipeline: &mut Self::Pipeline,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
    }

    fn draw(&self, pipeline: &Self::Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if self.separate_pass {
            return false;
        }
        pipeline.draw(pass);
        true
    }

    fn render(
        &self,
        pipeline: &Self::Pipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("layer-alpha custom primitive pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_viewport(
            clip_bounds.x as f32,
            clip_bounds.y as f32,
            clip_bounds.width as f32,
            clip_bounds.height as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );
        pipeline.draw(&mut pass);
    }
}

struct RedPipeline(&'static wgpu::RenderPipeline);

impl std::fmt::Debug for RedPipeline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RedPipeline")
    }
}

impl iced_wgpu::primitive::Pipeline for RedPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layer-alpha custom primitive shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
                "@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{var p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}\n@fragment fn fs()->@location(0) vec4<f32>{return vec4(1.,0.,0.,1.);}",
            )),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer-alpha custom primitive layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        Self(Box::leak(Box::new(device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("layer-alpha custom primitive pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        ))))
    }
}

impl RedPipeline {
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(self.0);
        pass.draw(0..3, 0..1);
    }
}

/// A scaled composite keeps every thin line, as text in a dialog scaling in
/// or out needs (enzUI-ICED #3263). Nearest sampling skips whole source rows
/// when the group shrinks, so some 1 px lines vanished mid-animation.
///
/// Sabotage: bind `target.bind_group` for scaled composites in `alpha.rs`
/// and lines that land on a skipped row disappear.
#[test]
fn a_scaled_composite_keeps_every_thin_line() {
    let Some(mut renderer) = try_renderer() else {
        eprintln!("SKIP: no wgpu adapter available");
        return;
    };
    const SCALE: f32 = 0.9;
    let origin = Point::new(W as f32 / 2.0, H as f32 / 2.0);
    let rows: Vec<u32> = (10..110).step_by(5).collect();

    renderer.with_layer_composite(full(), 1.0, SCALE, 0.0, origin, |renderer| {
        for &row in &rows {
            white_quad(renderer, rect(20.0, row as f32, 120.0, 1.0));
        }
    });
    let pixels =
        <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK);

    let x = W / 2;
    for &row in &rows {
        let centre = origin.y + (row as f32 + 0.5 - origin.y) * SCALE;
        let near =
            (centre.floor() as u32).saturating_sub(1)..=(centre.ceil() as u32 + 1).min(H - 1);
        let ink = near.map(|y| px(&pixels, x, y)[0]).max().unwrap_or(0);
        assert!(
            ink >= 60,
            "the 1 px line at source row {row} vanished in the 0.9 composite \
             (strongest ink near row {centre:.1} is {ink})"
        );
    }
}
