// Modified by enzUI contributors. See FORK.md.
use super::Layer;
use crate::core::{self, Background, Color, Rectangle, Transformation, Vector};
use crate::graphics::{Image, Layer as _};

const IMAGES_ENABLED: bool = cfg!(any(feature = "image", feature = "svg"));

fn commands() -> [Image; 2] {
    [
        Image::Raster {
            image: core::Image::new(core::image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255])),
            bounds: Rectangle::new(core::Point::new(10.0, 20.0), core::Size::new(30.0, 40.0)),
            clip_bounds: Rectangle::new(core::Point::new(15.0, 25.0), core::Size::new(20.0, 30.0)),
        },
        Image::Vector {
            svg: core::Svg::new(core::svg::Handle::from_memory(
                b"<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'/>".as_slice(),
            )),
            bounds: Rectangle::new(core::Point::new(60.0, 70.0), core::Size::new(80.0, 90.0)),
            clip_bounds: Rectangle::new(core::Point::new(65.0, 75.0), core::Size::new(70.0, 80.0)),
        },
    ]
}

fn layer() -> Layer {
    Layer::with_bounds(Rectangle::with_size(core::Size::new(200.0, 200.0)))
}

fn images(layer: &mut Layer) -> Vec<Image> {
    layer.images.iter_mut().map(|image| image.clone()).collect()
}

#[test]
fn image_batch_lifecycle_obeys_the_selected_feature_set() {
    let commands = commands();
    let mut first = layer();
    let mut second = layer();
    first.draw_image(commands[0].clone(), Transformation::IDENTITY);
    second.draw_image(commands[1].clone(), Transformation::IDENTITY);
    assert_eq!(first.images.len(), usize::from(IMAGES_ENABLED));
    assert_eq!(second.images.len(), usize::from(IMAGES_ENABLED));
    assert_eq!(first.is_empty(), !IMAGES_ENABLED);
    assert_eq!(second.is_empty(), !IMAGES_ENABLED);

    first.merge(&mut second);
    assert_eq!(first.images.len(), 2 * usize::from(IMAGES_ENABLED));
    assert_eq!(second.images.len(), 0);
    assert!(second.is_empty());
    let expected = if IMAGES_ENABLED {
        commands.to_vec()
    } else {
        Vec::new()
    };
    assert_eq!(images(&mut first), expected);
    assert_eq!(first.images.is_empty(), !IMAGES_ENABLED);
    if IMAGES_ENABLED {
        assert!(std::mem::size_of::<crate::image::Batch>() > 0);
    } else {
        assert_eq!(std::mem::size_of::<crate::image::Batch>(), 0);
    }

    first.clear_paint();
    assert_eq!(first.images.len(), 0);
    assert!(images(&mut first).is_empty());
    assert!(first.is_empty());
    first.draw_quad(
        core::renderer::Quad {
            bounds: Rectangle::with_size(core::Size::new(10.0, 10.0)),
            ..Default::default()
        },
        Background::Color(Color::WHITE),
        Transformation::IDENTITY,
    );
    assert!(!first.is_empty(), "the layer still retains non-image paint");
    first.reset();
    assert!(first.is_empty());
    assert_eq!(first.images.len(), 0);
}

#[test]
fn image_batch_retained_translation_preserves_the_source_commands() {
    let commands = commands();
    let mut source = layer();
    for command in &commands {
        source.draw_image(command.clone(), Transformation::IDENTITY);
    }
    let expected = if IMAGES_ENABLED {
        commands.to_vec()
    } else {
        Vec::new()
    };
    let mut retained = source.retain().expect("image commands own their handles");
    assert_eq!(images(&mut retained), expected);
    assert_eq!(retained.images.len(), expected.len());
    let travel = Vector::new(7.0, 9.0);
    assert!(retained.translate(travel, Rectangle::INFINITE));
    let translated = images(&mut retained);
    assert_eq!(translated.len(), expected.len());
    for (original, translated) in expected.iter().zip(&translated) {
        let moved = match original {
            Image::Raster {
                image,
                bounds,
                clip_bounds,
            } => Image::Raster {
                image: image.clone(),
                bounds: *bounds + travel,
                clip_bounds: *clip_bounds + travel,
            },
            Image::Vector {
                svg,
                bounds,
                clip_bounds,
            } => Image::Vector {
                svg: svg.clone(),
                bounds: *bounds + travel,
                clip_bounds: *clip_bounds + travel,
            },
        };
        assert_eq!(*translated, moved);
    }
    assert_eq!(
        images(&mut source),
        expected,
        "retained replay cannot move the live recording"
    );
    retained.clear_paint();
    assert_eq!(retained.images.len(), 0);
    assert!(images(&mut retained).is_empty());
    assert!(retained.is_empty());
}
