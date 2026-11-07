// Modified by enzUI contributors. See FORK.md.
use super::Placed;
use super::*;
use crate::renderer::presentation::{
    Bounds, Error, Expression, Flex, Layout as Recipe, MAX_DEPTH, MAX_ITEMS, Node as OwnedNode,
    Program, TextOverflow,
};
use std::sync::Arc;

pub(super) fn lower(
    layout: Placed<'_>,
    driver: crate::renderer::NodeId,
    channel: u8,
    clip: Rectangle,
) -> Result<Arc<Program>, Error> {
    let bounds = layout.bounds();
    let mut builder = Builder {
        expressions: vec![
            Expression::Constant(bounds.x),
            Expression::Constant(bounds.y),
            Expression::Driver {
                node: driver.0,
                channel,
            },
        ],
        nodes: Vec::new(),
        culling: Vec::new(),
        docked: None,
        retain_material: false,
    };
    let _ = builder.node(layout, false, true, 0)?;
    Program::new_with_culling(
        builder.expressions,
        builder.nodes,
        vec![],
        [0, 1],
        to_bounds(clip),
        builder.culling,
    )
    .map(Arc::new)
}

pub(super) fn lower_docked(
    layout: Placed<'_>,
    driver: crate::renderer::NodeId,
    clip: Rectangle,
) -> Result<Arc<Program>, Error> {
    let bounds = layout.bounds();
    let mut builder = Builder {
        expressions: vec![
            Expression::Constant(bounds.x),
            Expression::Constant(bounds.y),
            Expression::Constant(bounds.width),
        ],
        nodes: Vec::new(),
        culling: Vec::new(),
        docked: Some(driver.0),
        retain_material: false,
    };
    let _ = builder.node(layout, true, true, 0)?;
    if !builder
        .nodes
        .iter()
        .any(|node| matches!(&node.layout, Recipe::Responsive(value) if value.progress.is_some())
            || matches!(&node.layout, Recipe::Container(value) if value.inset.is_some_and(|inset| inset.driver == driver.0 && inset.progress.is_some())))
    {
        return Err(Error::DockedOwnershipRequired);
    }
    Program::new_with_culling(
        builder.expressions,
        builder.nodes,
        vec![],
        [0, 1],
        to_bounds(clip),
        builder.culling,
    )
    .map(Arc::new)
}

fn to_bounds(bounds: Rectangle) -> Bounds {
    Bounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

struct Builder {
    retain_material: bool,
    docked: Option<u64>,
    expressions: Vec<Expression>,
    nodes: Vec<OwnedNode>,
    culling: Vec<crate::renderer::presentation::Cull>,
}

impl Builder {
    fn constant(&mut self, value: f32) -> usize {
        let index = self.expressions.len();
        self.expressions.push(Expression::Constant(value));
        index
    }

    fn node(
        &mut self,
        layout: Placed<'_>,
        native: bool,
        root: bool,
        depth: usize,
    ) -> Result<usize, Error> {
        if depth >= MAX_DEPTH || self.nodes.len() >= MAX_ITEMS {
            return Err(Error::Budget);
        }
        if layout.node.transparent_child().is_some() {
            let index = self.node(layout.child(0), native, root, depth + 1)?;
            // A wrapper's drawing cull scopes the child it covers, as it did
            // when the wrapper's node was that child's.
            // Culls on one widget merge into one scope, as enabling a second
            // cull on a node always did.
            if let Some((id, clip)) = layout.node.drawing_cull {
                let owner = self.nodes[index].id;
                match self.culling.iter_mut().find(|cull| cull.owner == owner) {
                    Some(cull) => cull.clip |= clip,
                    None => self.culling.push(crate::renderer::presentation::Cull {
                        id: id.0,
                        owner,
                        recorded: to_bounds(layout.bounds()),
                        clip,
                    }),
                }
            }
            return Ok(index);
        }
        fn extent(layout: Placed<'_>) -> crate::Rectangle {
            layout.children().fold(layout.bounds(), |bounds, child| {
                let union = to_bounds(bounds).union(to_bounds(extent(child)));
                crate::Rectangle {
                    x: union.x,
                    y: union.y,
                    width: union.width,
                    height: union.height,
                }
            })
        }
        let node = if let Some(flow) = layout.node.flow_presentation() {
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let mut declaration = flow.clone();
            declaration.width = root.then_some(2);
            OwnedNode {
                id: layout
                    .node
                    .presentation_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Wrapping(Arc::new(declaration)),
                children,
            }
        } else if let Some(input) = layout.node.input_presentation() {
            let mut paint = input.paint.get();
            if let Some((caret, _)) = paint.blink {
                paint.caret = Some(caret);
            }
            let source = layout.node.input_capture_node(paint);
            let captured =
                Placed::with_offset(layout.position() - source.bounds().position(), &source);
            let children = captured
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            OwnedNode {
                id: layout
                    .node
                    .presentation_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Input(Arc::new(crate::renderer::presentation::InputGeometry {
                    recorded: to_bounds(layout.bounds()),
                    limits: input.limits,
                    shape: input.shape,
                    paint,
                    width: root.then_some(2),
                })),
                children,
            }
        } else if let Some((id, tabs)) = layout.node.tab_strip_presentation() {
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let mut declaration = tabs.clone();
            declaration.recorded = to_bounds(layout.bounds());
            let source = extent(layout);
            declaration.source = to_bounds(source);
            declaration.width = root.then_some(2);
            OwnedNode {
                id: id.0,
                layout: Recipe::TabStrip(Arc::new(declaration)),
                children,
            }
        } else if let Some((id, clip)) = layout.node.clip_presentation() {
            if layout.node.children().len() != 1 {
                return Err(Error::UnsupportedBackend);
            }
            let child = self.node(layout.child(0), true, false, depth + 1)?;
            let mut declaration = clip.clone();
            if let Some(reveal) = &mut declaration.reveal {
                reveal.source = to_bounds(extent(layout));
                if let Some(driver) = reveal.driver {
                    reveal.progress = Some(self.expressions.len());
                    self.expressions.push(Expression::Driver {
                        node: driver,
                        channel: 0,
                    });
                }
            }
            declaration.recorded = to_bounds(layout.bounds());
            declaration.width = root.then_some(2);
            OwnedNode {
                id: id.0,
                layout: Recipe::Clip(Arc::new(declaration)),
                children: vec![child],
            }
        } else if let Some((id, band)) = layout.node.band_presentation() {
            if layout.node.children().len() != 4 {
                return Err(Error::UnsupportedBackend);
            }
            let mut children = Vec::with_capacity(4);
            let retain_material = self.retain_material;
            for (slot, child) in layout.children().enumerate() {
                self.retain_material = retain_material || slot == super::priority_band::CONTENT;
                children.push(self.node(child, true, false, depth + 1)?);
            }
            self.retain_material = retain_material;
            let mut declaration = band.clone();
            declaration.recorded = to_bounds(layout.bounds());
            declaration.recorded_content_clip = to_bounds(super::priority_band::content_clip(
                layout.child(0),
                band.policy.spill_epsilon,
            ));
            declaration.width = root.then_some(2);
            OwnedNode {
                id: id.0,
                layout: Recipe::Band(Arc::new(declaration)),
                children,
            }
        } else if let Some((id, value)) = layout.node.responsive_presentation() {
            if layout.node.children().len() != 1 {
                return Err(Error::UnsupportedBackend);
            }
            let child = self.node(layout.child(0), true, false, depth + 1)?;
            let mut value = value.clone();
            if let Some(driver) = value.driver {
                if self.docked.is_none() {
                    return Err(Error::MissingDriver(driver));
                }
                value.progress = Some(self.expressions.len());
                self.expressions.push(Expression::Driver {
                    node: driver,
                    channel: 0,
                });
            }
            OwnedNode {
                id: id.0,
                layout: Recipe::Responsive(Arc::new(value)),
                children: vec![child],
            }
        } else if let Some((id, media)) = layout.node.media_presentation() {
            if !layout.node.children().is_empty() {
                return Err(Error::UnsupportedBackend);
            }
            let bounds = layout.bounds();
            let maximum = media
                .limits
                .width(media.geometry.sizing.width)
                .height(media.geometry.sizing.height)
                .max();
            let recipe = if !self.retain_material
                && ((bounds.width == 0.0 && maximum.width == 0.0)
                    || (bounds.height == 0.0 && maximum.height == 0.0))
            {
                Recipe::UnpaintedLeaf {
                    recorded: to_bounds(bounds),
                    maximum,
                }
            } else {
                let mut declaration = media.clone();
                declaration.recorded = to_bounds(bounds);
                declaration.width = root.then_some(2);
                Recipe::Media(Arc::new(declaration))
            };
            OwnedNode {
                id: id.0,
                layout: recipe,
                children: vec![],
            }
        } else if let Some((id, scroll)) = layout.node.scroll_presentation() {
            if layout.node.children().len() != 1 {
                return Err(Error::UnsupportedBackend);
            }
            let child_layout = layout.children().next().ok_or(Error::UnsupportedBackend)?;
            let child = self.node(child_layout, true, false, depth + 1)?;
            let mut declaration = scroll.clone();
            declaration.recorded = to_bounds(layout.bounds());
            declaration.recorded_content = to_bounds(child_layout.bounds());
            declaration.width = root.then_some(2);
            OwnedNode {
                id: id.0,
                layout: Recipe::Scroll(Arc::new(declaration)),
                children: vec![child],
            }
        } else if let Some((id, editor)) = layout.node.editor_presentation() {
            if !layout.node.children().is_empty() {
                return Err(Error::UnsupportedBackend);
            }
            let mut declaration = editor.clone();
            declaration.recorded = to_bounds(layout.bounds());
            declaration.width = root.then_some(2);
            OwnedNode {
                id: id.0,
                layout: Recipe::Editor(Arc::new(declaration)),
                children: vec![],
            }
        } else if let Some(text) = layout.node.text_presentation() {
            if !layout.node.children().is_empty() {
                return Err(Error::UnsupportedBackend);
            }
            let bounds = layout.bounds();
            if !self.retain_material
                && ((bounds.width == 0.0 && text.limits.width == 0.0)
                    || (bounds.height == 0.0 && text.limits.height == 0.0))
            {
                OwnedNode {
                    id: text.node.0,
                    layout: Recipe::UnpaintedLeaf {
                        recorded: to_bounds(bounds),
                        maximum: text.limits,
                    },
                    children: vec![],
                }
            } else {
                let (width, shrink_width) = if native {
                    (
                        None,
                        matches!(text.sizing.width, Length::Shrink | Length::Fit),
                    )
                } else {
                    match text.sizing.width {
                        // A leaf resolves `Fit` exactly like `Shrink`:
                        // intrinsic, clamped to the maximum.
                        Length::Shrink | Length::Fit => (Some(2), true),
                        Length::Fill | Length::FillPortion(_) => (Some(2), false),
                        Length::Fixed(width) => {
                            let constant = self.constant(width);
                            let index = self.expressions.len();
                            self.expressions.push(Expression::Min(2, constant));
                            (Some(index), false)
                        }
                        _ => return Err(Error::UnsupportedBackend),
                    }
                };
                let height = if native {
                    None
                } else {
                    match text.sizing.height {
                        Length::Shrink | Length::Fit => None,
                        Length::Fill | Length::FillPortion(_) | Length::Fixed(_) => {
                            Some(self.constant(bounds.height))
                        }
                        _ => return Err(Error::UnsupportedBackend),
                    }
                };
                let max_height = if !native && text.limits.height.is_finite() {
                    Some(self.constant(text.limits.height))
                } else {
                    None
                };
                OwnedNode {
                    id: text.node.0,
                    layout: Recipe::Text {
                        recorded: to_bounds(bounds),
                        width,
                        height,
                        shrink_width,
                        max_height,
                        overflow: TextOverflow::SceneClip,
                    },
                    children: vec![],
                }
            }
        } else if let Some(container) = layout.node.container_presentation() {
            let expected = usize::from(!matches!(
                container.mode,
                super::container::Mode::Atomic
                    | super::container::Mode::Unrealized
                    | super::container::Mode::Void
            ));
            if layout.node.children().len() != expected {
                return Err(Error::UnsupportedBackend);
            }
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let mut declaration = container.clone();
            if let Some(inset) = &mut declaration.inset {
                if self.docked.is_none() {
                    return Err(Error::MissingDriver(inset.driver));
                }
                inset.progress = Some(self.expressions.len());
                self.expressions.push(Expression::Driver {
                    node: inset.driver,
                    channel: 0,
                });
            }
            declaration.recorded = Some(to_bounds(layout.bounds()));
            declaration.width = root.then_some(2);
            OwnedNode {
                id: layout
                    .node
                    .container_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Container(Arc::new(declaration)),
                children,
            }
        } else if let Some(pair) = layout.node.pair_presentation() {
            if pair.children.len() != layout.node.children().len() {
                return Err(Error::UnsupportedBackend);
            }
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let mut declaration = pair.clone();
            declaration.width = root.then_some(2);
            OwnedNode {
                id: layout
                    .node
                    .presentation_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Pair(Arc::new(declaration)),
                children,
            }
        } else if let Some(overlay) = layout.node.overlay_presentation() {
            if overlay.children.len() != layout.node.children().len() {
                return Err(Error::UnsupportedBackend);
            }
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let mut declaration = overlay.clone();
            declaration.width = root.then_some(2);
            OwnedNode {
                id: layout
                    .node
                    .presentation_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Overlay(Arc::new(declaration)),
                children,
            }
        } else if let Some(flex) = layout.node.flex_presentation() {
            if flex.children.len() != layout.node.children().len() {
                return Err(Error::UnsupportedBackend);
            }
            let children = layout
                .children()
                .map(|child| self.node(child, true, false, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            let gap = self.constant(flex.spacing);
            OwnedNode {
                id: layout
                    .node
                    .presentation_node()
                    .ok_or(Error::UnsupportedBackend)?
                    .0,
                layout: Recipe::Flex(Arc::new(Flex {
                    bookends: flex.bookends,
                    sequence: flex.sequence,
                    collapse_spacing: flex.collapse_spacing,
                    axis: flex.axis,
                    width: root.then_some(2),
                    limits: flex.limits,
                    sizing: flex.sizing,
                    padding: flex.padding,
                    gap,
                    align: flex.align,
                    children: flex.children.clone(),
                })),
                children,
            }
        } else {
            return Err(Error::UnsupportedBackend);
        };
        let index = self.nodes.len();
        if let Some((id, clip)) = layout.node.drawing_cull {
            self.culling.push(crate::renderer::presentation::Cull {
                id: id.0,
                owner: node.id,
                recorded: to_bounds(layout.bounds()),
                clip,
            });
        }
        self.nodes.push(node);
        Ok(index)
    }
}
