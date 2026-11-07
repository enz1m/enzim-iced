// Modified by enzUI contributors. See FORK.md.
//! Rounded clips whose mask changes nothing take no offscreen group
//! (enzUI-ICED #3406).
//!
//! A tile wrapped in a rounded clip used to open a group per tile, even when
//! its picture was already rounded with the same radius. These tests draw such
//! tiles, count the groups the frame allocated, and compare the pixels with a
//! reference that keeps the group: an extra invisible quad in a corner makes a
//! second primitive there, which is the case a group exists for (#3388).

use iced_wgpu::Renderer;
use iced_wgpu::core::image::{Handle, Image, Renderer as _};
use iced_wgpu::core::renderer::{Headless, Quad, Renderer as _, Settings};
use iced_wgpu::core::text::{Alignment, Ellipsis, LineHeight, Renderer as _, Shaping, Wrapping};
use iced_wgpu::core::{
    Background, Color, Font, Pixels, Point, Rectangle, Size, alignment, border, text::Text,
};

const SIZE: u32 = 120;
const BG: Color = Color::from_rgb(40.0 / 255.0, 38.0 / 255.0, 43.0 / 255.0);
const RADIUS: f32 = 14.0;

fn renderer() -> Renderer {
    pollster::block_on(<Renderer as Headless>::new(
        Settings::default(),
        Some("wgpu"),
    ))
    .expect("real WGPU adapter required for rounded clip verification")
}

fn tile() -> Rectangle {
    Rectangle {
        x: 8.0,
        y: 8.0,
        width: 100.0,
        height: 100.0,
    }
}

fn checker() -> Handle {
    let checker: Vec<u8> = (0..16 * 16)
        .flat_map(|i| {
            if (i / 16 + i % 16) % 2 == 0 {
                [230, 120, 40, 255]
            } else {
                [40, 140, 220, 255]
            }
        })
        .collect();
    Handle::from_rgba(16, 16, checker)
}

fn picture(renderer: &mut Renderer, radius: f32) {
    renderer.draw_image(
        Image {
            border_radius: border::Radius::new(radius),
            ..Image::new(checker())
        },
        tile(),
        tile(),
    );
}

/// A progress bar inside the tile's rounded shape, clear of every arc.
fn progress(renderer: &mut Renderer) {
    renderer.fill_quad(
        Quad {
            bounds: Rectangle {
                x: 18.0,
                y: 92.0,
                width: 60.0,
                height: 4.0,
            },
            border: border::rounded(2.0),
            ..Quad::default()
        },
        Background::Color(Color::WHITE),
    );
}

/// A barely visible quad in the top-left corner: a second primitive there.
fn keep_group(renderer: &mut Renderer) {
    // Barely visible: a fully transparent quad paints nothing and is not
    // drawn at all, so it would keep nothing.
    renderer.fill_quad(
        Quad {
            bounds: Rectangle {
                x: 8.0,
                y: 8.0,
                width: 4.0,
                height: 4.0,
            },
            ..Quad::default()
        },
        Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 1.0 / 255.0)),
    );
}

/// Draws one frame and returns its pixels, the groups it allocated and the
/// elisions it made.
fn frame(draw: impl Fn(&mut Renderer)) -> (Vec<u8>, usize, u64) {
    let mut r = renderer();
    let before = r.rounded_clip_elisions();
    // Paint before the group, so a kept group is composited rather than
    // masked in place over the clear (enzUI-ICED #3442).
    r.fill_quad(
        Quad {
            bounds: Rectangle {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            ..Quad::default()
        },
        Background::Color(Color::WHITE),
    );
    r.start_layer_rounded(tile(), border::Radius::new(RADIUS));
    draw(&mut r);
    r.end_layer();
    let pixels = <Renderer as Headless>::screenshot(&mut r, Size::new(SIZE, SIZE), 1.0, BG);
    (
        pixels,
        r.alpha_group_statistics().groups,
        r.rounded_clip_elisions() - before,
    )
}

fn max_difference(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let offset = ((y * SIZE + x) * 4) as usize;
    pixels[offset..offset + 3].try_into().unwrap()
}

/// The Plex tile at rest: one picture rounded like the clip, and a progress
/// bar inside the shape. It takes no group and paints what the group painted.
///
/// Sabotage: return `false` from `rounded_elision::is_redundant`; the frame
/// allocates a group again.
#[test]
fn a_tile_rounded_like_its_clip_takes_no_group() {
    let (pixels, groups, elisions) = frame(|r| {
        picture(r, RADIUS);
        progress(r);
    });
    let (reference, reference_groups, _) = frame(|r| {
        picture(r, RADIUS);
        progress(r);
        keep_group(r);
    });

    assert_eq!(reference_groups, 1, "the reference keeps its group");
    assert_eq!(groups, 0, "a redundant mask must not open a group");
    assert_eq!(elisions, 1);
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );
}

/// A square picture under a rounded clip still loses its corners: the image
/// clips itself by the layer's radius once no group owns it.
///
/// Sabotage: clear the elided layers' `clip_radius` instead of re-pointing
/// their `clip_group`; the corners come out square.
#[test]
fn a_square_picture_is_still_rounded_without_its_group() {
    let (pixels, groups, _) = frame(|r| picture(r, 0.0));
    let (reference, _, _) = frame(|r| {
        picture(r, 0.0);
        keep_group(r);
    });

    assert_eq!(groups, 0);
    assert_eq!(
        pixel(&pixels, 9, 9),
        pixel(&reference, 9, 9),
        "the corner must stay masked"
    );
    assert_ne!(pixel(&pixels, 9, 9), pixel(&pixels, 30, 30));
    assert!(max_difference(&pixels, &reference) <= 2);
}

/// A full-bleed picture in a nested rectangular layer (a page view inside a
/// rounded panel) keeps the panel's corners, with or without the panel's
/// group (enzUI-ICED #3670).
///
/// Sabotage: clear the elided layers' `clip_radius` where an elided group's
/// layers are re-pointed; the corners come out square.
#[test]
fn a_picture_in_a_nested_rectangular_layer_keeps_the_rounded_corners() {
    // The page view starts a pixel below the panel's top edge: a layer of
    // its own, rectangular, still reaching into every arc.
    let page = Rectangle {
        y: tile().y + 1.0,
        height: tile().height - 1.0,
        ..tile()
    };
    let (pixels, _, _) = frame(|r| {
        r.start_layer(page);
        r.draw_image(Image::new(checker()), page, page);
        r.end_layer();
    });
    let (reference, _, _) = frame(|r| {
        picture(r, 0.0);
        keep_group(r);
    });

    assert_eq!(
        pixel(&pixels, 9, 10),
        pixel(&reference, 9, 10),
        "the corner must stay masked"
    );
    assert_ne!(pixel(&pixels, 9, 10), pixel(&pixels, 30, 30));
}

/// Two primitives meeting in a corner keep the group, so the composition is
/// masked once (#3388).
#[test]
fn two_primitives_in_a_corner_keep_the_group() {
    let (_, groups, elisions) = frame(|r| {
        picture(r, RADIUS);
        r.fill_quad(
            Quad {
                bounds: tile(),
                border: border::rounded(RADIUS),
                ..Quad::default()
            },
            Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.3)),
        );
    });

    assert_eq!(groups, 1);
    assert_eq!(elisions, 0);
}

/// One composition tile, a picture under a scrim, at column `column`.
fn composed_tile(renderer: &mut Renderer, column: u32) {
    let x = 4.0 + column as f32 * 19.0;
    let bounds = Rectangle {
        x,
        y: 20.0,
        width: 16.0,
        height: 60.0,
    };
    renderer.start_layer_rounded(bounds, border::Radius::new(6.0));
    renderer.draw_image(
        Image::new(Handle::from_rgba(1, 1, vec![230, 120, 40, 255])),
        bounds,
        bounds,
    );
    renderer.fill_quad(
        Quad {
            bounds,
            border: border::rounded(6.0),
            ..Quad::default()
        },
        Background::Color(Color::from_rgba(0.0, 0.0, 0.3, 0.4)),
    );
    renderer.end_layer();
}

/// A parent-level paint over the tile at `column`, which must see that tile.
fn between(renderer: &mut Renderer, column: u32) {
    let spot = Rectangle::new(
        iced_wgpu::core::Point::new(6.0 + column as f32 * 19.0, 40.0),
        Size::new(1.0, 1.0),
    );
    renderer.start_layer(spot);
    // Barely visible: a fully transparent quad is not drawn at all.
    renderer.fill_quad(
        Quad {
            bounds: spot,
            ..Quad::default()
        },
        Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 1.0 / 255.0)),
    );
    renderer.end_layer();
}

/// Sibling tiles that each need a group share one target and one composite
/// pass, and paint what separately composited groups paint.
///
/// Sabotage: never take the `sibling` path in `iced_wgpu`'s render loop; no
/// batched pass is issued.
#[test]
fn sibling_tile_groups_share_one_target_and_one_composite_pass() {
    let draw = |separated: bool| {
        let mut r = renderer();
        for column in 0..6 {
            composed_tile(&mut r, column);
            if separated {
                between(&mut r, column);
            }
        }
        let pixels = <Renderer as Headless>::screenshot(&mut r, Size::new(SIZE, SIZE), 1.0, BG);
        (pixels, r.alpha_group_statistics())
    };
    let (batched, statistics) = draw(false);
    let (reference, reference_statistics) = draw(true);

    assert_eq!(reference_statistics.batched_composite_passes, 0);
    assert_eq!(reference_statistics.composites, 6);
    assert_eq!(statistics.groups, 6, "every tile still needs its mask");
    assert_eq!(statistics.composites, 6);
    assert_eq!(statistics.batched_composite_passes, 1, "{statistics:?}");
    assert!(
        max_difference(&batched, &reference) <= 1,
        "batching changed the frame by up to {} levels",
        max_difference(&batched, &reference)
    );
    assert_ne!(
        pixel(&batched, 6, 50),
        pixel(&batched, 6, 10),
        "the tiles paint"
    );
}

/// Siblings that overlap never share a target: the later one's paint would
/// land inside the earlier one's mask.
///
/// Sabotage: make `apart` return true; the overlapping pair is batched.
#[test]
fn overlapping_sibling_groups_composite_separately() {
    let mut r = renderer();
    composed_tile(&mut r, 0);
    // Half a column to the right: overlapping the first tile.
    let bounds = Rectangle {
        x: 12.0,
        y: 20.0,
        width: 16.0,
        height: 60.0,
    };
    r.start_layer_rounded(bounds, border::Radius::new(6.0));
    r.draw_image(
        Image::new(Handle::from_rgba(1, 1, vec![40, 140, 220, 255])),
        bounds,
        bounds,
    );
    r.fill_quad(
        Quad {
            bounds,
            border: border::rounded(6.0),
            ..Quad::default()
        },
        Background::Color(Color::from_rgba(0.3, 0.0, 0.0, 0.4)),
    );
    r.end_layer();
    let _ = <Renderer as Headless>::screenshot(&mut r, Size::new(SIZE, SIZE), 1.0, BG);

    let statistics = r.alpha_group_statistics();
    assert_eq!(statistics.groups, 2);
    assert_eq!(statistics.batched_composite_passes, 0, "{statistics:?}");
}

/// A button's own background, drawn at the parent level just before its tile
/// group, as `enz_clickable` draws it.
fn backing(renderer: &mut Renderer, column: u32) {
    let x = 4.0 + column as f32 * 19.0;
    renderer.fill_quad(
        Quad {
            bounds: Rectangle {
                x,
                y: 20.0,
                width: 16.0,
                height: 60.0,
            },
            border: border::rounded(6.0),
            ..Quad::default()
        },
        Background::Color(Color::from_rgba(0.9, 0.9, 0.2, 0.5)),
    );
}

/// Parent paint between tile groups that stays clear of the pending tiles
/// does not break the batch, and paints as separately composited tiles do.
///
/// Sabotage: never defer a group closing into parent paint (drop the
/// `deferred.extend` branch); each tile composites on its own.
#[test]
fn button_backings_between_tiles_keep_one_composite_pass() {
    let draw = |separated: bool| {
        let mut r = renderer();
        for column in 0..6 {
            backing(&mut r, column);
            composed_tile(&mut r, column);
            if separated {
                between(&mut r, column);
            }
        }
        let pixels = <Renderer as Headless>::screenshot(&mut r, Size::new(SIZE, SIZE), 1.0, BG);
        (
            pixels,
            r.alpha_group_statistics(),
            r.primary_render_passes(),
        )
    };
    let (batched, statistics, passes) = draw(false);
    let (reference, reference_statistics, reference_passes) = draw(true);

    assert_eq!(reference_statistics.batched_composite_passes, 0);
    assert_eq!(statistics.batched_composite_passes, 1, "{statistics:?}");
    assert_eq!(statistics.composites, 6);
    // Each separately composited tile costs a composite pass and a pass to
    // resume the frame; batched, the six composites are one pass.
    let total = passes + statistics.batched_composite_passes;
    let reference_total = reference_passes + reference_statistics.composites;
    println!("passes: batched {total}, separately composited {reference_total}");
    assert!(
        total + 6 <= reference_total,
        "batching must save at least one pass per tile: {total} against {reference_total}"
    );
    assert!(
        max_difference(&batched, &reference) <= 1,
        "batching changed the frame by up to {} levels",
        max_difference(&batched, &reference)
    );
}

/// A label clipped to its own box at `at`.
fn label(renderer: &mut Renderer, at: Point) {
    let bounds = Rectangle::new(at, Size::new(60.0, 20.0));
    renderer.fill_text(
        Text {
            content: String::from("Inbox"),
            bounds: bounds.size(),
            size: Pixels(14.0),
            line_height: LineHeight::Relative(1.2),
            font: Font::DEFAULT,
            align_x: Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::None,
            ellipsis: Ellipsis::None,
            hint_factor: None,
        },
        at,
        Color::WHITE,
        bounds,
    );
}

/// A sidebar row: a backing rounded like its clip and a label clear of the
/// arcs. Text is clipped by rectangle, but nowhere near a corner the mask
/// would change, so the row takes no group and paints what the group did.
///
/// Sabotage: keep every group that holds text -- the row opens one again.
#[test]
fn a_label_clear_of_the_arcs_takes_no_group() {
    let row = |r: &mut Renderer| {
        picture(r, RADIUS);
        label(r, Point::new(30.0, 50.0));
    };
    let (pixels, groups, elisions) = frame(row);
    let (reference, reference_groups, _) = frame(|r| {
        row(r);
        keep_group(r);
    });

    assert_eq!(reference_groups, 1, "the reference keeps its group");
    assert_eq!(groups, 0, "a label clear of the arcs must not open a group");
    assert_eq!(elisions, 1);
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );
}

/// Text reaching into a corner keeps the group: only the mask rounds it.
///
/// Sabotage: let text anywhere go without the mask -- the corner text takes
/// no group.
#[test]
fn a_label_in_a_corner_keeps_the_group() {
    let (_, groups, elisions) = frame(|r| {
        picture(r, RADIUS);
        label(r, Point::new(8.0, 8.0));
    });

    assert_eq!(groups, 1, "text in a corner needs the mask");
    assert_eq!(elisions, 0);
}

/// A count right-aligned across the whole row (the sidebar's unread count)
/// paints only its glyphs, at the row's end and clear of the arcs, so the row
/// takes no group even though the paragraph's box spans the row.
///
/// Sabotage: take a paragraph aligned off its start to paint its whole box --
/// the row opens a group again.
#[test]
fn a_right_aligned_count_takes_no_group() {
    use iced_wgpu::core::text::Paragraph as _;

    // A widget keeps its paragraph past the frame, as the row's text does.
    let bounds = tile();
    let paragraph = <<Renderer as iced_wgpu::core::text::Renderer>::Paragraph>::with_text(Text {
        content: "7",
        bounds: Size::new(bounds.width - 12.0, 20.0),
        size: Pixels(14.0),
        line_height: LineHeight::Relative(1.2),
        font: Font::DEFAULT,
        align_x: Alignment::Right,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: Ellipsis::None,
        hint_factor: None,
    });
    let row = |r: &mut Renderer| {
        picture(r, RADIUS);
        r.fill_paragraph(
            &paragraph,
            Point::new(bounds.x + 6.0, bounds.y + 40.0),
            Color::WHITE,
            bounds,
        );
    };
    let (pixels, groups, elisions) = frame(row);
    let (reference, reference_groups, _) = frame(|r| {
        row(r);
        keep_group(r);
    });

    assert_eq!(reference_groups, 1, "the reference keeps its group");
    assert_eq!(groups, 0, "a count clear of the arcs must not open a group");
    assert_eq!(elisions, 1);
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );
}

/// A photo tile in a pressable: inside the pressable's rounded clip, a card
/// rounded more than the clip and a picture under a clip of the card's
/// radius. Both stay clear of the outer arcs, so neither group is needed;
/// a card rounded barely more than the clip meets its arc and keeps it.
///
/// Sabotage: count a rounded quad in every corner its rectangle reaches --
/// the card and the picture share each corner and the outer group returns.
#[test]
fn a_card_rounded_more_than_its_clip_takes_no_group() {
    let card = |radius: f32| {
        move |r: &mut Renderer| {
            r.fill_quad(
                Quad {
                    bounds: tile(),
                    border: border::rounded(radius),
                    ..Quad::default()
                },
                Background::Color(Color::from_rgb(0.3, 0.3, 0.35)),
            );
            r.start_layer_rounded(tile(), border::Radius::new(radius));
            picture(r, 0.0);
            r.end_layer();
        }
    };
    let (pixels, groups, elisions) = frame(card(RADIUS + 8.0));
    let (reference, reference_groups, _) = frame(|r| {
        card(RADIUS + 8.0)(r);
        keep_group(r);
    });

    assert_eq!(reference_groups, 1, "the reference keeps the outer group");
    assert_eq!(groups, 0, "a card clear of the arcs must not open a group");
    assert_eq!(elisions, 2);
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );

    let (_, groups, _) = frame(card(RADIUS + 1.0));
    assert_eq!(
        groups, 1,
        "a card almost tangent to the arcs keeps the group"
    );
}

/// A count just under the top-right arc and close to the row's end, as a
/// sidebar row's count sits: its line box is clear of the arc, and ink
/// overshoots an advance sideways far less than it does a line box, so the
/// row takes no group.
///
/// Sabotage: pad a line sideways as much as above and below -- the padded
/// count reaches the arc and the row keeps its group.
#[test]
fn a_count_beside_a_corner_takes_no_group() {
    use iced_wgpu::core::text::Paragraph as _;

    let paragraph = <<Renderer as iced_wgpu::core::text::Renderer>::Paragraph>::with_text(Text {
        content: "7",
        bounds: Size::new(40.0, 20.0),
        size: Pixels(14.0),
        line_height: LineHeight::Relative(1.2),
        font: Font::DEFAULT,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: Ellipsis::None,
        hint_factor: None,
    });
    // The advance ends 6.2 px left of the tile's edge, the line box starts a
    // pixel below the top arc.
    let width = paragraph.min_bounds().width;
    let at = Point::new(
        tile().x + tile().width - 6.2 - width,
        tile().y + RADIUS + 1.0,
    );
    let row = |r: &mut Renderer| {
        picture(r, RADIUS);
        r.fill_paragraph(&paragraph, at, Color::WHITE, tile());
    };
    let (pixels, groups, _) = frame(row);
    let (reference, _, _) = frame(|r| {
        row(r);
        keep_group(r);
    });

    assert_eq!(groups, 0, "a count clear of the arcs must not open a group");
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );
}

/// A solid triangle with its clip at `clip`, as a chart's curve paints.
fn triangle_mesh(renderer: &mut Renderer, clip: Rectangle) {
    use iced_wgpu::graphics::mesh::{self, Renderer as _};

    let color = iced_wgpu::graphics::color::pack(Color::from_rgb(0.2, 0.6, 0.9));
    renderer.draw_mesh(iced_wgpu::graphics::Mesh::Solid {
        buffers: mesh::Indexed {
            vertices: [
                [clip.x, clip.y + clip.height],
                [clip.x + clip.width / 2.0, clip.y],
                [clip.x + clip.width, clip.y + clip.height],
            ]
            .into_iter()
            .map(|position| mesh::SolidVertex2D { position, color })
            .collect(),
            indices: vec![0, 1, 2],
        },
        transformation: iced_wgpu::core::Transformation::IDENTITY,
        clip_bounds: clip,
    });
}

/// A chart's curve inside a rounded panel, clear of the panel's arcs,
/// takes no group: meshes are clipped by rectangle, which nowhere near a
/// corner is what the mask would do. One reaching a corner keeps the group.
///
/// Sabotage: keep every group holding a mesh -- the chart's panel opens
/// one again; or let any mesh go without the mask -- the corner mesh does
/// too.
#[test]
fn a_mesh_clear_of_the_arcs_takes_no_group() {
    let chart = Rectangle::new(Point::new(30.0, 30.0), Size::new(56.0, 50.0));
    let (pixels, groups, elisions) = frame(|r| {
        picture(r, RADIUS);
        triangle_mesh(r, chart);
    });
    let (reference, reference_groups, _) = frame(|r| {
        picture(r, RADIUS);
        triangle_mesh(r, chart);
        keep_group(r);
    });

    assert_eq!(reference_groups, 1, "the reference keeps its group");
    assert_eq!(groups, 0, "a mesh clear of the arcs must not open a group");
    assert_eq!(elisions, 1);
    assert!(
        max_difference(&pixels, &reference) <= 2,
        "eliding the group changed the frame by up to {} levels",
        max_difference(&pixels, &reference)
    );

    let (_, groups, _) = frame(|r| {
        picture(r, RADIUS);
        triangle_mesh(r, tile());
    });
    assert_eq!(groups, 1, "a mesh in a corner keeps the group");
}
