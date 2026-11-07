// Modified by enzUI contributors. See FORK.md.
use crate::{
    Layer,
    core::{Rectangle, Transformation},
    geometry, graphics, triangle,
};
use graphics::geometry::{Stroke, border::Border, frame::Backend};

pub(crate) fn paint(
    layer: &mut Layer,
    bounds: Rectangle,
    border: Border,
    transform: Transformation,
) {
    let mut frame = geometry::Frame::new(Rectangle::with_size(bounds.size()));
    if let Some(path) = border.path(bounds.size()) {
        frame.stroke(
            &path,
            Stroke::default().with_width(1.0).with_color(border.color),
        );
    }
    let geometry::Geometry::Live {
        meshes,
        images,
        text,
    } = frame.into_geometry()
    else {
        unreachable!()
    };
    debug_assert!(images.is_empty() && text.is_empty());
    layer.draw_mesh_group(
        meshes,
        transform * Transformation::translate(bounds.x, bounds.y),
    );
}

pub(crate) fn matches(actual: &triangle::Batch, expected: &triangle::Batch) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, b)| match (a, b) {
            (
                triangle::Item::Group {
                    transformation: a,
                    meshes: am,
                },
                triangle::Item::Group {
                    transformation: b,
                    meshes: bm,
                },
            ) => a == b && am == bm,
            _ => false,
        })
}

pub(crate) fn cost(batch: &triangle::Batch) -> Option<usize> {
    let mut total = 0usize;
    for item in batch {
        let triangle::Item::Group { meshes, .. } = item else {
            return None;
        };
        for mesh in meshes {
            let vertices = match mesh {
                graphics::mesh::Mesh::Solid { buffers, .. } => buffers.vertices.len(),
                graphics::mesh::Mesh::Gradient { .. } => return None,
            };
            total = total
                .checked_add(vertices)?
                .checked_add(mesh.indices().len())?;
        }
    }
    Some(total)
}
