// Modified by enzUI contributors. See FORK.md.
//! Rounded masks over completed image and gradient compositions.

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{Handle, Image, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::{Color, Gradient, Rectangle, Size, border, gradient};

const SIZE: u32 = 120;
const BG: Color = Color::from_rgb(40.0 / 255.0, 38.0 / 255.0, 43.0 / 255.0);
fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
    .expect("real WGPU adapter required for rounded composition verification")
}
fn bounds() -> Rectangle {
    Rectangle {
        x: 8.0,
        y: 8.0,
        width: 100.0,
        height: 100.0,
    }
}
fn picture(renderer: &mut Renderer, handle: Handle) {
    renderer.start_layer(bounds());
    renderer.draw_image(Image::new(handle), bounds(), bounds());
    renderer.end_layer();
}
fn darken(renderer: &mut Renderer) {
    let black = Color {
        a: 0.98,
        ..Color::BLACK
    };
    renderer.start_layer(bounds());
    renderer.fill_quad(
        Quad {
            bounds: bounds(),
            ..Default::default()
        },
        Gradient::Linear(
            gradient::Linear::new(0.0)
                .add_stop(0.0, black)
                .add_stop(1.0, black),
        ),
    );
    renderer.end_layer();
}
fn screenshot(renderer: &mut Renderer) -> Vec<u8> {
    <Renderer as Headless>::screenshot(renderer, Size::new(SIZE, SIZE), 1.0, BG)
}
fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let offset = ((y * SIZE + x) * 4) as usize;
    pixels[offset..offset + 3].try_into().unwrap()
}
#[test]
fn rounded_completed_image_gradient_composition_is_masked_once() {
    let white = Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
    let mut plain = renderer();
    picture(&mut plain, white.clone());
    darken(&mut plain);
    let flat = screenshot(&mut plain);
    let center = pixel(&flat, 58, 58);
    assert!(
        center.iter().all(|c| *c < 64),
        "opaque composition must actually darken: {center:?}"
    );
    assert_ne!(center, pixel(&flat, 0, 0));
    let composed = Handle::from_rgba(1, 1, vec![center[0], center[1], center[2], 255]);
    let mut reference = renderer();
    reference.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut reference, composed);
    reference.end_layer();
    let expected = screenshot(&mut reference);
    let mut actual = renderer();
    actual.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut actual, white);
    darken(&mut actual);
    actual.end_layer();
    let pixels = screenshot(&mut actual);
    assert_eq!(
        pixel(&pixels, 8, 8),
        pixel(&pixels, 0, 0),
        "excluded corner must remain the backdrop"
    );
    let mut excess = 0;
    let mut max_delta = 0;
    for y in 8..23 {
        for x in 8..23 {
            let a = pixel(&pixels, x, y);
            let b = pixel(&expected, x, y);
            let delta = a
                .into_iter()
                .zip(b)
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .unwrap();
            if delta > 6 {
                excess += 1;
            }
            max_delta = max_delta.max(delta);
        }
    }
    assert_eq!(
        excess, 0,
        "{excess} fringe pixels differ from flattened content; largest channel delta {max_delta}"
    );
}

fn glass() -> iced_wgpu::core::material::Material {
    iced_wgpu::core::material::Material {
        radii: [12.0; 4],
        blur_sigma: 6.0,
        refraction: 1.0,
        bevel_width: 10.0,
        index_of_refraction: 1.5,
        dispersion: 0.0,
        color_diffusion: 0.0,
        color_reach: 0.0,
        optical_depth: 10.0,
        tint: Color::WHITE,
        tint_opacity: 0.5,
        saturation: 1.0,
        highlight: 0.0,
        brightness: 0.0,
        light_angle: 225.0,
        profile_curvature: 2.5,
        contrast_protection: 0.0,
        protected_inset: 0.0,
        probe: 0,
        measure_only: false,
    }
}

#[test]
fn rounded_scope_preserves_optical_material_sampling() {
    let mut renderer = renderer();
    renderer.fill_quad(
        Quad {
            bounds: Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32)),
            ..Default::default()
        },
        Color::from_rgb(0.0, 0.0, 0.6),
    );
    renderer.start_layer_rounded(bounds(), border::Radius::new(14.0));
    renderer.start_layer_material(bounds(), glass());
    renderer.end_layer();
    renderer.end_layer();
    let pixels = screenshot(&mut renderer);
    assert!(
        pixel(&pixels, 58, 58)[0] > 40,
        "material must visibly composite"
    );
    let statistics = renderer.material_statistics();
    assert_eq!(statistics.composites, 1, "{statistics:?}");
    assert_eq!(statistics.fallback_composites, 0, "{statistics:?}");
}

#[test]
fn rounded_composition_tracks_retained_translation() {
    use iced_wgpu::core::Vector;
    use iced_wgpu::core::renderer::{NodeId, Track};
    let node = NodeId(17);
    let outer = Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32));
    let mut r = renderer();
    r.reset(outer);
    assert!(r.start_recording(outer));
    r.mark_recording(node);
    r.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut r, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut r);
    r.end_layer();
    r.mark_recording(NodeId::STATIC);
    let recording = r.finish_recording().expect("retained image composition");
    for offset in [0.0, 4.0, -4.0, 4.0] {
        r.reset(outer);
        assert!(r.replay_tracks(
            &recording,
            &[Track {
                translation: Vector::new(offset, 0.0),
                ..Track::identity(node)
            }]
        ));
        let actual = screenshot(&mut r);
        let mut direct = renderer();
        direct.with_translation(Vector::new(offset, 0.0), |r| {
            r.start_layer_rounded(bounds(), border::Radius::new(14.0));
            picture(r, Handle::from_rgba(1, 1, vec![255; 4]));
            darken(r);
            r.end_layer();
        });
        let expected = screenshot(&mut direct);
        let different = actual
            .iter()
            .zip(&expected)
            .filter(|(a, b)| a.abs_diff(**b) > 2)
            .count();
        assert_eq!(
            different, 0,
            "translated mask at {offset}: {different} different channels"
        );
    }
}

/// Paint before any group, so the first group is composited rather than
/// masked in place over the clear (enzUI-ICED #3442).
fn paint_first(renderer: &mut Renderer) {
    renderer.fill_quad(
        Quad {
            bounds: Rectangle::new([0.0, 0.0].into(), Size::new(2.0, 2.0)),
            ..Default::default()
        },
        Color::WHITE,
    );
}

#[test]
fn rounded_identical_nested_masks_and_repeated_frames_match_single_mask() {
    let mut single = renderer();
    paint_first(&mut single);
    single.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut single, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut single);
    single.end_layer();
    let expected = screenshot(&mut single);
    let mut nested = renderer();
    paint_first(&mut nested);
    nested.start_layer_rounded(bounds(), border::Radius::new(14.0));
    nested.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut nested, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut nested);
    nested.end_layer();
    nested.end_layer();
    for _ in 0..3 {
        assert_eq!(screenshot(&mut nested), expected);
        assert_eq!(nested.alpha_group_statistics().groups, 1);
        assert_eq!(nested.alpha_group_statistics().max_depth, 1);
    }
}

#[test]
fn rounded_single_image_and_plain_controls_need_no_offscreen_target() {
    let mut r = renderer();
    r.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut r, Handle::from_rgba(1, 1, vec![255; 4]));
    r.end_layer();
    let actual = screenshot(&mut r);
    assert_ne!(pixel(&actual, 58, 58), pixel(&actual, 0, 0));
    assert_eq!(pixel(&actual, 8, 8), pixel(&actual, 0, 0));
    assert_eq!(r.alpha_group_statistics().target_allocations, 0);
    r.reset(Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32)));
    r.start_layer_rounded(bounds(), border::Radius::new(14.0));
    darken(&mut r);
    r.end_layer();
    let actual = screenshot(&mut r);
    assert_ne!(pixel(&actual, 58, 58), pixel(&actual, 0, 0));
    assert_eq!(r.alpha_group_statistics().target_allocations, 0);
}

#[test]
fn rounded_material_samples_preceding_content_inside_its_scope() {
    fn frame(rounded: bool) -> (Vec<u8>, iced_wgpu::MaterialStatistics) {
        let mut r = renderer();
        if rounded {
            r.start_layer_rounded(bounds(), border::Radius::new(14.0));
        } else {
            r.start_layer(bounds());
        }
        picture(&mut r, Handle::from_rgba(1, 1, vec![255, 0, 0, 255]));
        darken(&mut r);
        r.start_layer_material(bounds(), glass());
        r.end_layer();
        r.end_layer();
        let pixels = screenshot(&mut r);
        (pixels, r.material_statistics())
    }
    let (expected, control) = frame(false);
    let (actual, statistics) = frame(true);
    assert_eq!(control.composites, 1);
    assert_eq!(statistics.composites, 1);
    assert_eq!(statistics.fallback_composites, 0);
    assert_eq!(pixel(&actual, 58, 58), pixel(&expected, 58, 58));
    assert_ne!(pixel(&actual, 58, 58), pixel(&actual, 0, 0));
}

#[test]
fn rounded_child_image_keeps_its_distinct_mask_inside_a_composition() {
    let mut r = renderer();
    r.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut r, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut r);
    let child = Rectangle {
        x: 38.0,
        y: 38.0,
        width: 40.0,
        height: 40.0,
    };
    r.start_layer_rounded(child, border::Radius::new(12.0));
    r.draw_image(
        Image::new(Handle::from_rgba(1, 1, vec![255; 4])),
        child,
        child,
    );
    r.end_layer();
    r.end_layer();
    let actual = screenshot(&mut r);
    assert!(
        pixel(&actual, 38, 38)
            .into_iter()
            .zip(pixel(&actual, 35, 35))
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "the child's excluded corner must retain the dark parent"
    );
    assert_eq!(
        pixel(&actual, 58, 58),
        [255; 3],
        "the child is visibly painted"
    );
}

#[test]
fn rounded_retained_reveal_crops_without_inventing_new_corners() {
    use iced_wgpu::core::renderer::{NodeId, Track};
    let outer = Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32));
    let clip = Rectangle {
        x: 28.0,
        y: 8.0,
        width: 80.0,
        height: 100.0,
    };
    let mut r = renderer();
    r.reset(outer);
    assert!(r.start_recording(outer));
    r.mark_recording(NodeId(17));
    r.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut r, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut r);
    r.end_layer();
    r.mark_recording(NodeId::STATIC);
    let recording = r.finish_recording().expect("retained composition");
    r.reset(outer);
    assert!(r.replay_tracks(
        &recording,
        &[Track {
            clip: Some(clip),
            ..Track::identity(NodeId(17))
        }]
    ));
    let actual = screenshot(&mut r);
    let mut direct = renderer();
    direct.start_layer(clip);
    direct.start_layer_rounded(bounds(), border::Radius::new(14.0));
    picture(&mut direct, Handle::from_rgba(1, 1, vec![255; 4]));
    darken(&mut direct);
    direct.end_layer();
    direct.end_layer();
    let expected = screenshot(&mut direct);
    let different = actual
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a.abs_diff(**b) > 2)
        .count();
    assert_eq!(different, 0, "a reveal must crop the original rounded mask");
}

#[test]
fn rounded_owned_clip_reflows_a_segmented_image_gradient_composition() {
    use iced_wgpu::core::renderer::presentation::{
        Bounds, Clip, Container, Expression, Layout, Media, MediaKind, Node, Overlay, Program,
    };
    use iced_wgpu::core::renderer::{LayoutLayer, NodeId, Track};
    use iced_wgpu::core::{Alignment, ContentFit, Length, Padding, Rotation, Vector, layout};
    use std::sync::Arc;
    for nested in [false, true] {
        let initial = Rectangle::with_size(Size::new(100.0, 100.0));
        let captured = Bounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let outer = Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32));
        let limits = layout::Limits::new(Size::ZERO, initial.size());
        let sizing = Size::new(Length::Fill, Length::Fill);
        let mut nodes = vec![
            Node {
                id: 2,
                children: vec![],
                layout: Layout::Media(Arc::new(Media {
                    recorded: captured,
                    width: None,
                    limits,
                    kind: MediaKind::Raster,
                    geometry: layout::media::Media {
                        source: Size::new(1, 1),
                        sizing,
                        crop: None,
                        fit: ContentFit::Fill,
                        rotation: Rotation::default(),
                        expand: false,
                    },
                })),
            },
            Node {
                id: 3,
                children: vec![],
                layout: Layout::Container(Arc::new(Container {
                    reflow_min_width: None,
                    inset: None,
                    clip_content: false,
                    mode: layout::container::Mode::Atomic,
                    recorded: Some(captured),
                    width: None,
                    limits,
                    sizing,
                    padding: Padding::ZERO,
                    alignment: [Alignment::Start; 2],
                    child: Size::new(Length::Shrink, Length::Shrink),
                })),
            },
            Node {
                id: 4,
                children: vec![0, 1],
                layout: Layout::Overlay(Arc::new(Overlay {
                    limits,
                    sizing,
                    base: 0,
                    children: vec![sizing, sizing],
                    width: None,
                })),
            },
            Node {
                id: 1,
                children: vec![2],
                layout: Layout::Clip(Arc::new(Clip {
                    reveal: None,
                    recorded: captured,
                    limits,
                    child: sizing,
                    outset: Padding::ZERO,
                    width: Some(1),
                })),
            },
        ];
        if nested {
            nodes.push(Node {
                id: 5,
                children: vec![3],
                layout: Layout::Clip(Arc::new(Clip {
                    reveal: None,
                    recorded: captured,
                    limits,
                    child: sizing,
                    outset: Padding::ZERO,
                    width: Some(1),
                })),
            });
        }
        let program = Arc::new(
            Program::new(
                vec![
                    Expression::Constant(0.0),
                    Expression::Driver {
                        node: 999,
                        channel: 0,
                    },
                ],
                nodes,
                vec![],
                [0, 0],
                Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: SIZE as f32,
                    height: SIZE as f32,
                },
            )
            .expect("valid responsive composition"),
        );
        let mut r = renderer();
        r.reset(outer);
        assert!(r.start_recording(outer));
        r.record_presentation(program).expect("owned presentation");
        let paint = |r: &mut Renderer| {
            r.with_layout_layer(
                LayoutLayer::Clip(NodeId(1)),
                initial,
                Some(border::Radius::new(14.0)),
                |r| {
                    r.with_layout_layer(LayoutLayer::Inherited, initial, None, |r| {
                        r.draw_layout_image(
                            Some(NodeId(2)),
                            Image::new(Handle::from_rgba(1, 1, vec![255; 4])),
                            initial,
                            initial,
                            1.0,
                        );
                    });
                    let black = Color {
                        a: 0.98,
                        ..Color::BLACK
                    };
                    r.with_layout_layer(LayoutLayer::Inherited, initial, None, |r| {
                        r.fill_layout_quad(
                            NodeId(3),
                            Quad {
                                bounds: initial,
                                ..Default::default()
                            },
                            Gradient::Linear(
                                gradient::Linear::new(0.0)
                                    .add_stop(0.0, black)
                                    .add_stop(1.0, black),
                            )
                            .into(),
                        );
                    });
                },
            );
        };
        if nested {
            r.with_layout_layer(
                LayoutLayer::Clip(NodeId(5)),
                initial,
                Some(border::Radius::new(14.0)),
                paint,
            );
        } else {
            paint(&mut r);
        }
        let recording = r
            .finish_recording()
            .expect("closed responsive image and gradient resources");
        let mut plain = renderer();
        picture(&mut plain, Handle::from_rgba(1, 1, vec![255; 4]));
        darken(&mut plain);
        let shade = pixel(&screenshot(&mut plain), 58, 58);
        assert!(
            shade.iter().all(|c| *c < 64),
            "the control must darken the white image"
        );
        assert!(
            pixel(&screenshot(&mut r), 50, 50)
                .into_iter()
                .zip(shade)
                .all(|(a, b)| a.abs_diff(b) <= 1),
            "the captured paint order must darken the image"
        );
        for width in [100.0, 80.0, 100.0] {
            r.reset(outer);
            assert!(
                r.replay_tracks(
                    &recording,
                    &[Track {
                        translation: Vector::new(width, 0.0),
                        ..Track::identity(NodeId(999))
                    }]
                ),
                "{:?}",
                r.presentation_refusal()
            );
            let actual = screenshot(&mut r);
            let mut reference = renderer();
            let area = Rectangle::with_size(Size::new(width, 100.0));
            reference.start_layer_rounded(area, border::Radius::new(14.0));
            reference.draw_image(
                Image::new(Handle::from_rgba(
                    1,
                    1,
                    vec![shade[0], shade[1], shade[2], 255],
                )),
                area,
                area,
            );
            reference.end_layer();
            let expected = screenshot(&mut reference);
            let different = actual
                .iter()
                .zip(&expected)
                .filter(|(a, b)| a.abs_diff(**b) > 2)
                .count();
            assert_eq!(
                different, 0,
                "responsive width {width} must match flattened content"
            );
        }
    }
}
