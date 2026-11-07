// Modified by enzUI contributors. See FORK.md.
//! Pixel and render-pass checks for pooled alpha-target clearing.

use iced_wgpu::Renderer;
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Color, Rectangle, Size};

const SIZE: u32 = 100;

fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
    .expect("real WGPU adapter required for alpha clear verification")
}

fn full() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

fn paint(renderer: &mut Renderer, x: f32) {
    renderer.fill_quad(
        Quad {
            bounds: Rectangle {
                x,
                y: 20.0,
                width: 20.0,
                height: 20.0,
            },
            ..Default::default()
        },
        Color::WHITE,
    );
}

fn screenshot(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, Size::new(SIZE, SIZE), 1.0, Color::BLACK)
}

fn pixel(frame: &[u8], x: u32) -> [u8; 4] {
    let offset = ((30 * SIZE + x) * 4) as usize;
    frame[offset..offset + 4].try_into().unwrap()
}

#[test]
fn alpha_clear_folds_into_sibling_paint_and_reused_targets_stay_empty() {
    let mut control = renderer();
    control.start_layer_alpha(full(), 0.5);
    paint(&mut control, 10.0);
    control.end_layer();
    let half = pixel(&screenshot(&mut control), 20);
    assert!(half[0] > 100 && half[0] < 240);

    let mut renderer = renderer();
    for positions in [[10.0, 60.0], [60.0, 10.0]] {
        renderer.reset(full());
        for x in positions {
            renderer.start_layer_alpha(full(), 0.5);
            paint(&mut renderer, x);
            renderer.end_layer();
        }
        let frame = screenshot(&mut renderer);
        assert_eq!(
            frame,
            screenshot(&mut renderer),
            "identical scene must repeat exactly"
        );
        assert_eq!(pixel(&frame, 20), half);
        assert_eq!(pixel(&frame, 70), half);
        assert_eq!(pixel(&frame, 45), [0, 0, 0, 255]);
        let statistics = renderer.alpha_group_statistics();
        assert_eq!(statistics.groups, 2);
        assert_eq!(statistics.composites, 2);
        assert_eq!(statistics.clear_passes, 0);
        assert_eq!(statistics.target_allocations, 1);
    }
}

#[test]
fn alpha_clear_preserves_an_intermediate_target_before_child_composition() {
    let mut control = renderer();
    control.start_layer_alpha(full(), 0.5);
    control.fill_quad(
        Quad {
            bounds: full(),
            ..Default::default()
        },
        Color::BLACK,
    );
    control.start_layer_alpha(full(), 0.5);
    paint(&mut control, 60.0);
    control.end_layer();
    control.end_layer();
    let quarter = pixel(&screenshot(&mut control), 70);
    assert!(quarter[0] > 100 && quarter[0] < 180);

    let mut renderer = renderer();
    renderer.start_layer_alpha(full(), 0.5);
    paint(&mut renderer, 40.0);
    renderer.end_layer();
    assert!(pixel(&screenshot(&mut renderer), 45)[0] > 100);
    renderer.reset(full());
    renderer.start_layer_alpha(full(), 0.5);
    renderer.start_layer_alpha(full(), 0.5);
    paint(&mut renderer, 60.0);
    renderer.end_layer();
    paint(&mut renderer, 10.0);
    renderer.end_layer();
    let frame = screenshot(&mut renderer);
    assert_eq!(
        frame,
        screenshot(&mut renderer),
        "identical scene must repeat exactly"
    );
    assert_eq!(pixel(&frame, 70), quarter);
    assert!(pixel(&frame, 20)[0] > quarter[0]);
    assert_eq!(pixel(&frame, 45), [0, 0, 0, 255]);
    let statistics = renderer.alpha_group_statistics();
    assert_eq!(statistics.groups, 2);
    assert_eq!(statistics.composites, 2);
    assert_eq!(statistics.clear_passes, 1);
}
