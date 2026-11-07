// Modified by enzUI contributors. See FORK.md.
//! Pixel gates for subtree alpha compositing (enzUI-ICED #1568).
//!
//! These tests exercise the tiny-skia framebuffer, not stored renderer state.
//! Quad, text, raster image, and SVG each have an independent half-alpha gate;
//! endpoint controls protect the old opaque path and the transparent no-op.
//! Nesting and overlap distinguish real layer compositing from per-primitive
//! color scaling.

use iced_tiny_skia::Renderer;
use iced_tiny_skia::core::alignment;
use iced_tiny_skia::core::image::{Image, Renderer as _};
use iced_tiny_skia::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_tiny_skia::core::svg::{Handle as SvgHandle, Renderer as _, Svg};
use iced_tiny_skia::core::text::{
    Alignment, Ellipsis, LineHeight, Renderer as _, Shaping, Wrapping,
};
use iced_tiny_skia::core::{
    Background, Border, Color, Font, Pixels, Point, Rectangle, Shadow, Size, Text,
};

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
const WHITE: Color = Color::WHITE;
const SQUARE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><rect width="32" height="32" fill="#ff0000"/></svg>"##;

#[derive(Debug, Clone, Copy)]
enum Primitive {
    Quad,
    Text,
    Image,
    Svg,
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

fn draw(renderer: &mut Renderer, primitive: Primitive) {
    match primitive {
        Primitive::Quad => renderer.fill_quad(
            Quad {
                bounds: rect(12.0, 12.0, 32.0, 32.0),
                border: Border::default(),
                shadow: Shadow::default(),
                snap: false,
            },
            Background::Color(FG),
        ),
        Primitive::Text => renderer.fill_text(text(), Point::new(8.0, 28.0), FG, full()),
        Primitive::Image => renderer.draw_image(
            Image::new(iced_tiny_skia::core::image::Handle::from_rgba(
                1,
                1,
                vec![255, 0, 0, 255],
            )),
            rect(56.0, 12.0, 32.0, 32.0),
            full(),
        ),
        Primitive::Svg => renderer.draw_svg(
            Svg::new(SvgHandle::from_memory(SQUARE_SVG)),
            rect(100.0, 12.0, 32.0, 32.0),
            full(),
        ),
    }
}

fn frame(primitive: Primitive, alpha: Option<f32>) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());

    if let Some(alpha) = alpha {
        renderer.start_layer_alpha(full(), alpha);
    }
    draw(&mut renderer, primitive);
    if alpha.is_some() {
        renderer.end_layer();
    }

    <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, BG)
}

fn blank() -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, BG)
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

fn assert_half_blend(primitive: Primitive, probe: Option<(u32, u32)>) {
    let opaque = frame(primitive, None);
    let half = frame(primitive, Some(0.5));
    let (x, y) = probe.unwrap_or_else(|| strongest_ink(&opaque));
    let background = px(&blank(), x, y);
    let opaque = px(&opaque, x, y);
    let half = px(&half, x, y);

    assert_ne!(
        opaque, background,
        "{primitive:?} positive control drew no pixel"
    );

    for channel in 0..4 {
        let expected = (u16::from(opaque[channel]) + u16::from(background[channel])).div_ceil(2);
        assert!(
            (i16::from(half[channel]) - expected as i16).abs() <= 3,
            "{primitive:?} channel {channel} at ({x},{y}) was {half:?}; expected halfway between opaque {opaque:?} and backdrop {background:?}"
        );
    }
}

#[test]
fn quad_is_composited_at_layer_alpha() {
    assert_half_blend(Primitive::Quad, Some((24, 24)));
}

#[test]
fn text_is_composited_at_layer_alpha() {
    assert_half_blend(Primitive::Text, None);
}

#[test]
fn image_is_composited_at_layer_alpha() {
    assert_half_blend(Primitive::Image, None);
}

#[test]
fn svg_is_composited_at_layer_alpha() {
    assert_half_blend(Primitive::Svg, Some((112, 24)));
}

#[test]
fn alpha_one_is_byte_identical_to_no_alpha_layer() {
    for primitive in [
        Primitive::Quad,
        Primitive::Text,
        Primitive::Image,
        Primitive::Svg,
    ] {
        assert_eq!(
            frame(primitive, Some(1.0)),
            frame(primitive, None),
            "opaque endpoint changed {primitive:?} pixels"
        );
    }
}

#[test]
fn alpha_zero_leaves_backdrop_byte_identical() {
    let backdrop = blank();
    for primitive in [
        Primitive::Quad,
        Primitive::Text,
        Primitive::Image,
        Primitive::Svg,
    ] {
        assert_eq!(
            frame(primitive, Some(0.0)),
            backdrop,
            "transparent endpoint changed backdrop pixels for {primitive:?}"
        );
    }
}

fn quad_frame(draw_quads: impl FnOnce(&mut Renderer)) -> Vec<u8> {
    let mut renderer = Renderer::new(Settings::default());
    draw_quads(&mut renderer);
    <Renderer as Headless>::screenshot(&mut renderer, Size::new(W, H), 1.0, Color::BLACK)
}

fn white_quad(renderer: &mut Renderer, bounds: Rectangle) {
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(WHITE),
    );
}

fn rgb_distance(a: [u8; 4], b: [u8; 4]) -> u16 {
    (0..3)
        .map(|channel| u16::from(a[channel].abs_diff(b[channel])))
        .sum()
}

#[test]
fn nested_alpha_layers_multiply() {
    let nested = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.5);
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
        renderer.end_layer();
        renderer.end_layer();
    });
    let quarter = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.25);
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
        renderer.end_layer();
    });
    let opaque = quad_frame(|renderer| {
        white_quad(renderer, rect(10.0, 10.0, 80.0, 80.0));
    });

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
        "nested alpha matched the no-alpha opaque reference; both layers were ignored"
    );
    assert!(
        rgb_distance(nested, quarter) < rgb_distance(nested, opaque),
        "nested alpha was not closer to quarter-strength than opaque: nested={nested:?}, quarter={quarter:?}, opaque={opaque:?}"
    );
}

#[test]
fn overlapping_siblings_are_composited_once() {
    let frame = quad_frame(|renderer| {
        renderer.start_layer_alpha(full(), 0.5);
        white_quad(renderer, rect(10.0, 10.0, 70.0, 70.0));
        white_quad(renderer, rect(50.0, 10.0, 70.0, 70.0));
        renderer.end_layer();
    });
    let opaque = quad_frame(|renderer| {
        white_quad(renderer, rect(10.0, 10.0, 70.0, 70.0));
        white_quad(renderer, rect(50.0, 10.0, 70.0, 70.0));
    });

    let single = px(&frame, 30, 30);
    let overlap = px(&frame, 60, 30);
    let opaque_overlap = px(&opaque, 60, 30);
    assert_eq!(
        overlap, single,
        "overlap was alpha-scaled per primitive instead of once as a layer"
    );
    assert_ne!(
        overlap, opaque_overlap,
        "overlap matched the no-alpha opaque reference; the layer was not composited"
    );
}
