// Modified by enzUI contributors. See FORK.md.
use crate::widget::Tree;
use crate::{Alignment, Padding, Point, Rectangle, Size, Vector};

#[cfg(test)]
mod tests;

/// The bounds of an element and its children.
///
/// Equality compares geometry; it does not attest matching paint or presentation identities.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub(super) drawing_cull: Option<(crate::renderer::NodeId, bool)>,
    pub(crate) presented_reveal_progress: Option<f32>,
    /// Declarations few widgets make, kept out of line so a node stays small:
    /// layout moves and copies nodes for every widget.
    rare: Option<Box<Rare>>,
    bounds: Rectangle,
    children: Vec<Node>,
    text_presentation: Option<TextPresentation>,
    flex_presentation: Option<std::sync::Arc<FlexPresentation>>,
    flex_node: Option<crate::renderer::NodeId>,
    container_presentation: Option<std::sync::Arc<crate::renderer::presentation::Container>>,
    container_node: Option<crate::renderer::NodeId>,
    reversed_children: bool,
    interposed: bool,
    pub(super) placed: Option<std::sync::Arc<[usize]>>,
    /// The sizing of the declared widget a pass-through wrapper lays out
    /// as; set on measured nodes only, never stored as a declaration.
    passed_sizing: Option<Size<crate::Length>>,
    /// A measured node's subtree extent, relative to its origin; set by a
    /// measurer that needs it, never stored as a declaration.
    pub(super) measured_extent: Option<Rectangle>,
}

/// The declarations few nodes carry; see [`Node`]'s `rare` field.
#[derive(Debug, Clone, Default)]
pub(crate) struct Rare {
    pub(crate) flow_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Wrapping>,
    )>,
    pub(crate) input_presentation: Option<(crate::renderer::NodeId, super::input::Declaration)>,
    pub(crate) tab_strip_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::TabStrip>,
    )>,
    pub(crate) band_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Band>,
    )>,
    pub(crate) clip_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Clip>,
    )>,
    pub(crate) responsive_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Responsive>,
    )>,
    pub(crate) media_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Media>,
    )>,
    pub(crate) scroll_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Scroll>,
    )>,
    pub(crate) overlay_presentation: Option<std::sync::Arc<crate::renderer::presentation::Overlay>>,
    pub(crate) overlay_node: Option<crate::renderer::NodeId>,
    pub(crate) pair_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Pair>,
    )>,
    pub(crate) editor_presentation: Option<(
        crate::renderer::NodeId,
        std::sync::Arc<crate::renderer::presentation::Editor>,
    )>,
}

static EMPTY_RARE: Rare = Rare {
    flow_presentation: None,
    input_presentation: None,
    tab_strip_presentation: None,
    band_presentation: None,
    clip_presentation: None,
    responsive_presentation: None,
    media_presentation: None,
    scroll_presentation: None,
    overlay_presentation: None,
    overlay_node: None,
    pair_presentation: None,
    editor_presentation: None,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct TextPresentation {
    pub node: crate::renderer::NodeId,
    pub sizing: Size<crate::Length>,
    pub limits: Size,
}

#[derive(Debug, Clone)]
pub(crate) struct FlexPresentation {
    pub collapse_spacing: bool,
    pub sequence: Option<crate::layout::flex::SequenceFlow>,
    pub bookends: Option<crate::layout::flex::Bookends>,
    pub axis: crate::renderer::presentation::Axis,
    pub limits: super::Limits,
    pub sizing: Size<crate::Length>,
    pub padding: Padding,
    pub spacing: f32,
    pub align: Alignment,
    pub children: Vec<Size<crate::Length>>,
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.bounds == other.bounds && self.children == other.children
    }
}

impl Node {
    /// The rare declarations, empty when the node makes none.
    pub(crate) fn rare(&self) -> &Rare {
        self.rare.as_deref().unwrap_or(&EMPTY_RARE)
    }

    /// The rare declarations, made on first write.
    pub(crate) fn rare_mut(&mut self) -> &mut Rare {
        self.rare.get_or_insert_with(Box::default)
    }

    /// Retains subtree culling against the incoming viewport without changing layout geometry.
    /// The clip option also narrows the logical viewport passed to descendants.
    pub fn with_drawing_cull(mut self, clip: bool) -> Self {
        self.enable_drawing_cull(clip);
        self
    }

    /// Retains the per-child viewport filter used by native rows and columns.
    pub fn with_child_drawing_culls(mut self) -> Self {
        for child in &mut self.children {
            child.enable_drawing_cull(false);
        }
        self
    }

    fn enable_drawing_cull(&mut self, clip: bool) {
        match &mut self.drawing_cull {
            Some((_, prior)) => *prior |= clip,
            slot @ None => *slot = Some((crate::renderer::NodeId::unique_layout(), clip)),
        }
    }

    /// Creates a new [`Node`] with the given [`Size`].
    pub const fn new(size: Size) -> Self {
        Self::with_children(size, Vec::new())
    }

    /// Creates a new [`Node`] with the given [`Size`] and children.
    pub const fn with_children(size: Size, children: Vec<Node>) -> Self {
        Node {
            drawing_cull: None,
            presented_reveal_progress: None,
            rare: None,
            bounds: Rectangle {
                x: 0.0,
                y: 0.0,
                width: size.width,
                height: size.height,
            },
            children,
            text_presentation: None,
            flex_presentation: None,
            flex_node: None,
            container_presentation: None,
            container_node: None,
            reversed_children: false,
            interposed: false,
            placed: None,
            passed_sizing: None,
            measured_extent: None,
        }
    }

    /// Declares the four-slot policy without retaining widgets or probe callbacks.
    pub fn with_priority_band_presentation(
        mut self,
        limits: super::Limits,
        policy: super::priority_band::Specification,
        children: Vec<Size<crate::Length>>,
        probes: Vec<super::priority_band::Probe>,
    ) -> Self {
        self.rare_mut().band_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Band {
                width: None,
                limits,
                policy,
                children,
                probes,
                recorded: crate::renderer::presentation::Bounds::default(),
                recorded_content_clip: crate::renderer::presentation::Bounds::default(),
            }),
        ));
        self
    }

    pub(crate) fn band_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Band,
    )> {
        self.rare()
            .band_presentation
            .as_ref()
            .map(|(id, band)| (*id, band.as_ref()))
    }

    /// Declares an allocation-relative clip around a child whose layout and paint remain independently checked.
    pub fn with_clip_presentation(
        mut self,
        limits: super::Limits,
        child: Size<crate::Length>,
        outset: Padding,
    ) -> Self {
        self.rare_mut().clip_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Clip {
                reveal: None,
                limits,
                child,
                outset,
                recorded: crate::renderer::presentation::Bounds::default(),
                width: None,
            }),
        ));
        self
    }

    /// Declares a retained reveal with independently measured child material and normalized progress.
    pub fn with_reveal_presentation(
        mut self,
        limits: super::Limits,
        sizing: Size<crate::Length>,
        child: Size<crate::Length>,
        rule: super::reveal::Rule,
        captured: f32,
        driver: Option<crate::renderer::NodeId>,
    ) -> Self {
        use crate::renderer::presentation::{Bounds, Clip, Reveal};
        self.rare_mut().clip_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(Clip {
                reveal: Some(Reveal {
                    sizing,
                    rule,
                    captured,
                    driver: driver.map(|id| id.0),
                    progress: None,
                    source: Bounds::default(),
                }),
                limits,
                child,
                outset: Padding::ZERO,
                recorded: Bounds::default(),
                width: None,
            }),
        ));
        self
    }

    pub(crate) fn clip_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Clip,
    )> {
        self.rare()
            .clip_presentation
            .as_ref()
            .map(|(id, clip)| (*id, clip.as_ref()))
    }

    /// Bounds the width domain of an already declared retained composition.
    ///
    /// # Errors
    /// Refuses invalid floors or a node without a container declaration.
    pub fn with_reflow_min_width(
        mut self,
        width: f32,
    ) -> Result<Self, crate::renderer::presentation::Error> {
        if !width.is_finite() || width < 0.0 {
            return Err(crate::renderer::presentation::Error::NonFinite);
        }
        let value = self
            .container_presentation
            .as_mut()
            .ok_or(crate::renderer::presentation::Error::UnsupportedBackend)?;
        std::sync::Arc::make_mut(value).reflow_min_width = Some(width);
        Ok(self)
    }

    /// Declares a transparent native responsive wrapper; its child still needs its own contract.
    pub fn with_responsive_presentation(
        mut self,
        limits: super::Limits,
        rule: super::responsive::Rule,
        child: Size<crate::Length>,
        driver: Option<crate::renderer::NodeId>,
    ) -> Self {
        self.rare_mut().responsive_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Responsive {
                limits,
                rule,
                child,
                driver: driver.map(|id| id.0),
                progress: None,
            }),
        ));
        self
    }
    pub(crate) fn responsive_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Responsive,
    )> {
        self.rare()
            .responsive_presentation
            .as_ref()
            .map(|(id, value)| (*id, value.as_ref()))
    }

    /// Declares native image geometry; drawing must bind a matching owned resource.
    pub fn with_media_presentation(
        mut self,
        limits: super::Limits,
        geometry: super::media::Media,
        kind: crate::renderer::presentation::MediaKind,
    ) -> Self {
        self.rare_mut().media_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Media {
                recorded: crate::renderer::presentation::Bounds::default(),
                width: None,
                limits,
                geometry,
                kind,
            }),
        ));
        self
    }
    pub(crate) fn media_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Media,
    )> {
        self.rare()
            .media_presentation
            .as_ref()
            .map(|(id, value)| (*id, value.as_ref()))
    }

    /// Captures native scroll sizing; live offsets and chrome are bound by the drawing backend.
    pub fn with_scroll_presentation(
        mut self,
        limits: super::Limits,
        sizing: Size<crate::Length>,
        child: Size<crate::Length>,
        config: super::scroll::Config,
    ) -> Self {
        self.rare_mut().scroll_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Scroll {
                recorded: crate::renderer::presentation::Bounds::default(),
                recorded_content: crate::renderer::presentation::Bounds::default(),
                chrome: crate::renderer::NodeId::unique_layout().0,
                width: None,
                limits,
                sizing,
                child,
                config,
            }),
        ));
        self
    }
    pub(crate) fn scroll_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Scroll,
    )> {
        self.rare()
            .scroll_presentation
            .as_ref()
            .map(|(id, value)| (*id, value.as_ref()))
    }

    /// Captures the native editor's allocation contract for owned viewport replay.
    pub fn with_editor_presentation(
        mut self,
        limits: super::Limits,
        sizing: Size<crate::Length>,
        padding: Padding,
    ) -> Self {
        self.rare_mut().editor_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Editor {
                recorded: crate::renderer::presentation::Bounds::default(),
                width: None,
                limits,
                sizing,
                padding,
            }),
        ));
        self
    }

    pub(crate) fn editor_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::Editor,
    )> {
        self.rare()
            .editor_presentation
            .as_ref()
            .map(|(node, value)| (*node, value.as_ref()))
    }

    pub(crate) fn with_container_presentation(
        mut self,
        value: crate::renderer::presentation::Container,
    ) -> Self {
        self.container_presentation = Some(std::sync::Arc::new(value));
        self.container_node = Some(crate::renderer::NodeId::unique_layout());
        self
    }

    /// Declares a text viewport clipped by this container; it does not imply a rounded or GPU layer clip.
    pub fn with_content_viewport_clip(mut self, enabled: bool) -> Self {
        if let Some(container) = &mut self.container_presentation {
            std::sync::Arc::make_mut(container).clip_content = enabled;
            self.container_node = Some(crate::renderer::NodeId::unique_layout());
        }
        self
    }

    pub(crate) fn container_node(&self) -> Option<crate::renderer::NodeId> {
        self.container_node
            .or_else(|| self.rare().input_presentation.as_ref().map(|(id, _)| *id))
    }

    pub(crate) fn with_flow_presentation(
        mut self,
        declaration: crate::renderer::presentation::Wrapping,
    ) -> Self {
        self.rare_mut().flow_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(declaration),
        ));
        self
    }

    pub(crate) fn flow_presentation(&self) -> Option<&crate::renderer::presentation::Wrapping> {
        self.rare()
            .flow_presentation
            .as_ref()
            .map(|(_, value)| value.as_ref())
    }

    pub(crate) fn with_input_presentation(
        mut self,
        declaration: super::input::Declaration,
    ) -> Self {
        self.rare_mut().input_presentation =
            Some((crate::renderer::NodeId::unique_layout(), declaration));
        self
    }

    pub(crate) fn input_presentation(&self) -> Option<&super::input::Declaration> {
        self.rare()
            .input_presentation
            .as_ref()
            .map(|(_, value)| value)
    }

    pub(crate) fn input_capture_node(&self, paint: super::input::Paint) -> Self {
        let mut node = self.clone();
        if self.rare().input_presentation.is_some() {
            let text = node.children[0].bounds;
            let caret = &mut node.children[0].children[0].children[1];
            caret.bounds = if let Some(value) = paint.caret {
                Rectangle {
                    x: value.x + paint.offset(text.width),
                    y: 0.0,
                    width: value.width,
                    height: text.height,
                }
            } else {
                Rectangle::new(Point::ORIGIN, Size::ZERO)
            };
        }
        node
    }

    pub(crate) fn container_presentation(
        &self,
    ) -> Option<&crate::renderer::presentation::Container> {
        self.container_presentation.as_deref()
    }

    /// Declares the native base-sized overlay algorithm; child paint is admitted separately.
    pub fn with_overlay_presentation(
        mut self,
        limits: super::Limits,
        sizing: Size<crate::Length>,
        base: usize,
        children: Vec<Size<crate::Length>>,
    ) -> Self {
        self.rare_mut().overlay_node = Some(crate::renderer::NodeId::unique_layout());
        self.rare_mut().overlay_presentation = Some(std::sync::Arc::new(
            crate::renderer::presentation::Overlay {
                limits,
                sizing,
                base,
                children,
                width: None,
            },
        ));
        self
    }

    pub(crate) fn overlay_presentation(&self) -> Option<&crate::renderer::presentation::Overlay> {
        self.rare().overlay_presentation.as_deref()
    }

    /// Declares both native pair arrangements without admitting unknown child layout or paint.
    pub fn with_pair_presentation(
        mut self,
        limits: super::Limits,
        policy: super::pair::Policy,
        children: [Size<crate::Length>; 2],
    ) -> Self {
        self.rare_mut().pair_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::Pair {
                limits,
                policy,
                children,
                width: None,
            }),
        ));
        self
    }

    pub(crate) fn pair_presentation(&self) -> Option<&crate::renderer::presentation::Pair> {
        self.rare()
            .pair_presentation
            .as_ref()
            .map(|(_, pair)| pair.as_ref())
    }

    pub(crate) fn presentation_sizing(&self) -> Option<Size<crate::Length>> {
        if let Some((_, pair)) = &self.rare().pair_presentation {
            return Some(Size::new(pair.policy.width, crate::Length::Shrink));
        }
        if let Some((_, flow)) = &self.rare().flow_presentation {
            return Some(flow.rule.sizing);
        }
        if let Some((_, input)) = &self.rare().input_presentation {
            return Some(Size::new(input.shape.width, crate::Length::Shrink));
        }
        if self.rare().tab_strip_presentation.is_some() {
            return Some(
                self.rare()
                    .tab_strip_presentation
                    .as_ref()?
                    .1
                    .policy
                    .sizing(),
            );
        }
        if let Some((_, band)) = &self.rare().band_presentation {
            return Some(Size::new(crate::Length::Fill, band.policy.height));
        }
        if let Some((_, clip)) = &self.rare().clip_presentation {
            return Some(clip.reveal.map_or(clip.child, |reveal| reveal.sizing));
        }
        self.rare()
            .scroll_presentation
            .as_ref()
            .map(|(_, scroll)| scroll.sizing)
            .or_else(|| {
                self.rare()
                    .editor_presentation
                    .as_ref()
                    .map(|(_, editor)| editor.sizing)
            })
            .or_else(|| {
                self.rare()
                    .media_presentation
                    .as_ref()
                    .map(|(_, value)| value.geometry.sizing)
            })
            .or_else(|| self.text_presentation.map(|text| text.sizing))
            .or_else(|| self.flex_presentation.as_ref().map(|flex| flex.sizing))
            .or_else(|| {
                self.rare()
                    .overlay_presentation
                    .as_ref()
                    .map(|overlay| overlay.sizing)
            })
            .or_else(|| {
                self.container_presentation
                    .as_ref()
                    .map(|container| container.sizing)
            })
            .or(self.passed_sizing)
    }

    pub(crate) fn with_flex_presentation(mut self, value: FlexPresentation) -> Self {
        self.flex_node = Some(crate::renderer::NodeId::unique_layout());
        self.flex_presentation = Some(std::sync::Arc::new(value));
        self
    }

    /// Declares complete document-strip material and its pure membership policy.
    /// Unknown descendant layout remains unsupported by program lowering.
    pub fn with_tab_strip_presentation(
        self,
        limits: super::Limits,
        policy: super::tab_strip::Specification,
        children: Vec<Size<crate::Length>>,
    ) -> Self {
        self.with_item_strip_presentation(limits, super::item_strip::Policy::Tabs(policy), children)
    }

    /// Declares complete material and its callback-free width-dependent assignment policy.
    pub fn with_item_strip_presentation(
        mut self,
        limits: super::Limits,
        policy: super::item_strip::Policy,
        children: Vec<Size<crate::Length>>,
    ) -> Self {
        self.rare_mut().tab_strip_presentation = Some((
            crate::renderer::NodeId::unique_layout(),
            std::sync::Arc::new(crate::renderer::presentation::TabStrip {
                recorded: crate::renderer::presentation::Bounds::default(),
                source: crate::renderer::presentation::Bounds::default(),
                width: None,
                limits,
                policy,
                children,
            }),
        ));
        self
    }

    pub(crate) fn tab_strip_presentation(
        &self,
    ) -> Option<(
        crate::renderer::NodeId,
        &crate::renderer::presentation::TabStrip,
    )> {
        self.rare()
            .tab_strip_presentation
            .as_ref()
            .map(|(id, value)| (*id, value.as_ref()))
    }

    /// Identity shared by native geometry and its owned layout declaration.
    ///
    /// A clone retains the identity; a new declaration receives a new identity.
    /// Unknown custom layout has no identity and cannot be projected from a sample.
    pub fn presentation_node(&self) -> Option<crate::renderer::NodeId> {
        if let Some((id, _)) = &self.rare().pair_presentation {
            return Some(*id);
        }
        if let Some((id, _)) = &self.rare().flow_presentation {
            return Some(*id);
        }
        if let Some((id, _)) = &self.rare().input_presentation {
            return Some(*id);
        }
        if let Some((id, _)) = &self.rare().tab_strip_presentation {
            return Some(*id);
        }
        if let Some((id, _)) = &self.rare().band_presentation {
            return Some(*id);
        }
        if let Some((id, _)) = &self.rare().clip_presentation {
            return Some(*id);
        }
        self.rare()
            .responsive_presentation
            .as_ref()
            .map(|(id, _)| *id)
            .or_else(|| self.rare().media_presentation.as_ref().map(|(id, _)| *id))
            .or_else(|| self.rare().scroll_presentation.as_ref().map(|(id, _)| *id))
            .or_else(|| self.rare().editor_presentation.as_ref().map(|(id, _)| *id))
            .or_else(|| self.text_presentation.map(|text| text.node))
            .or(self.container_node)
            .or(self.rare().overlay_node)
            .or(self.flex_node)
    }

    pub(crate) fn projected(&self, bounds: Rectangle, children: Vec<Self>) -> Self {
        Self {
            drawing_cull: self.drawing_cull,
            rare: self.rare.clone(),
            bounds,
            children,
            presented_reveal_progress: None,
            text_presentation: self.text_presentation,
            flex_presentation: self.flex_presentation.clone(),
            flex_node: self.flex_node,
            container_presentation: self.container_presentation.clone(),
            container_node: self.container_node,
            reversed_children: self.reversed_children,
            interposed: self.interposed,
            placed: self.placed.clone(),
            passed_sizing: None,
            measured_extent: None,
        }
    }

    pub(crate) fn flex_presentation(&self) -> Option<&FlexPresentation> {
        self.flex_presentation.as_deref()
    }

    pub(crate) fn with_text_presentation(
        mut self,
        node: crate::renderer::NodeId,
        sizing: Size<crate::Length>,
        limits: Size,
    ) -> Self {
        self.text_presentation = Some(TextPresentation {
            node,
            sizing,
            limits,
        });
        self
    }

    pub(crate) fn text_presentation(&self) -> Option<TextPresentation> {
        self.text_presentation
    }

    /// Creates a new [`Node`] that wraps a single child with some [`Padding`].
    pub fn container(child: Self, padding: Padding) -> Self {
        Self::with_children(
            child.bounds.size().expand(padding),
            vec![child.move_to(Point::new(padding.left, padding.top))],
        )
    }

    /// Returns the [`Size`] of the [`Node`].
    pub fn size(&self) -> Size {
        Size::new(self.bounds.width, self.bounds.height)
    }

    /// Returns the position of the [`Node`].
    pub fn position(&self) -> Point {
        self.bounds.position()
    }

    /// Returns the bounds of the [`Node`].
    pub fn bounds(&self) -> Rectangle {
        self.bounds
    }

    /// Returns the children of the [`Node`].
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// Aligns the [`Node`] in the given space.
    pub fn align(mut self, align_x: Alignment, align_y: Alignment, space: Size) -> Self {
        self.align_mut(align_x, align_y, space);
        self
    }

    /// Mutable reference version of [`Self::align`].
    pub fn align_mut(&mut self, align_x: Alignment, align_y: Alignment, space: Size) {
        match align_x {
            Alignment::Start => {}
            Alignment::Center => {
                self.bounds.x += (space.width - self.bounds.width) / 2.0;
            }
            Alignment::End => {
                self.bounds.x += space.width - self.bounds.width;
            }
        }

        match align_y {
            Alignment::Start => {}
            Alignment::Center => {
                self.bounds.y += (space.height - self.bounds.height) / 2.0;
            }
            Alignment::End => {
                self.bounds.y += space.height - self.bounds.height;
            }
        }
    }

    /// Moves the [`Node`] to the given position.
    pub fn move_to(mut self, position: impl Into<Point>) -> Self {
        self.move_to_mut(position);
        self
    }

    /// Mutable reference version of [`Self::move_to`].
    pub fn move_to_mut(&mut self, position: impl Into<Point>) {
        let position = position.into();

        self.bounds.x = position.x;
        self.bounds.y = position.y;
    }

    /// Translates the [`Node`] by the given translation.
    pub fn translate(mut self, translation: impl Into<Vector>) -> Self {
        self.translate_mut(translation);
        self
    }

    /// Translates the [`Node`] by the given translation.
    pub fn translate_mut(&mut self, translation: impl Into<Vector>) {
        self.bounds += translation.into();
    }
}

impl Node {
    /// The geometry and declarations `tree` holds after its last layout,
    /// without its descendants (enzICED).
    ///
    /// A parent measuring a child through [`Measure`] receives this; applying
    /// the parent's node writes back only what the parent placed.
    pub fn of(tree: &Tree) -> Self {
        let mut node = tree.presentation.as_deref().cloned().unwrap_or_default();
        node.bounds = Rectangle::new(
            Point::new(tree.translation.x, tree.translation.y),
            tree.size,
        );
        // A wrapper that lays out as its child answers with that child's
        // sizing, as its node used to be the child's own.
        if !node.declares_layout() {
            let mut inner = tree;
            while let Some(index) = inner.pass_through_child() {
                inner = &inner.children[index];
                if let Some(declared) = inner
                    .presentation()
                    .filter(|declared| declared.declares_layout())
                {
                    node.passed_sizing = declared.presentation_sizing();
                    break;
                }
            }
        }
        node
    }

    /// The geometry and declarations of `tree` and all its descendants,
    /// positioned at `tree`'s translation (enzICED).
    ///
    /// Allocates the whole subtree; meant for lowering, projection and
    /// fingerprints, not per-frame paint.
    pub fn snapshot(tree: &Tree) -> Self {
        let mut node = Self::of(tree);
        if !tree.children.is_empty() {
            node.children = match node.placed.as_deref() {
                Some(placed) => placed
                    .iter()
                    .filter_map(|&index| tree.children.get(index))
                    .map(Self::snapshot_placed)
                    .collect(),
                None if node.reversed_children => tree
                    .children
                    .iter()
                    .rev()
                    .map(Self::snapshot_placed)
                    .collect(),
                None => tree.children.iter().map(Self::snapshot_placed).collect(),
            };
        }
        node
    }

    /// A child's snapshot as its parent placed it, inside the node the
    /// parent interposed, if any.
    fn snapshot_placed(tree: &Tree) -> Self {
        let node = Self::snapshot(tree);
        match tree.interposed.as_deref() {
            Some(slot) => {
                let mut slot = slot.clone();
                let inner = node.bounds.position() - slot.bounds.position();
                slot.children = vec![node.move_to(Point::new(inner.x, inner.y))];
                slot
            }
            None => node,
        }
    }

    /// Declares this node synthetic: it wraps its only child, which pairs
    /// with the widget tree in its place (enzICED).
    ///
    /// Applying the parent keeps this node, without its child, as the
    /// child tree's [interposed](Tree::interposed) node, and gives the child
    /// tree its position in the parent; snapshots rebuild the level.
    pub fn interposed(mut self) -> Self {
        debug_assert_eq!(self.children.len(), 1, "an interposed node wraps one child");
        self.interposed = true;
        self
    }

    /// Declares that this node places only as many of its tree's children
    /// as it has, in order; the rest are laid out but never shown (a
    /// measuring twin) (enzICED).
    pub fn with_placed_prefix(self) -> Self {
        let placed = (0..self.children.len()).collect::<Vec<_>>();
        self.with_placed(placed)
    }

    /// Declares which of its tree's children this node's children place,
    /// in order; the others are laid out but never shown (the inactive
    /// branches of a switch) (enzICED).
    pub fn with_placed(mut self, placed: impl Into<std::sync::Arc<[usize]>>) -> Self {
        let placed = placed.into();
        debug_assert_eq!(
            placed.len(),
            self.children.len(),
            "one index per placed child"
        );
        self.placed = Some(placed);
        self
    }

    /// Attaches the extent of the subtree this node was measured from, for a
    /// parent that reads descendant overhang from a shallow node (enzICED).
    pub fn with_measured_extent(mut self, extent: Rectangle) -> Self {
        self.measured_extent = Some(extent);
        self
    }

    /// Declares that this node lists its tree's children in reverse, the
    /// way a mirrored row lays out in visual order while its trees keep
    /// logical order (enzICED).
    pub fn with_reversed_children(mut self, reversed: bool) -> Self {
        self.reversed_children = reversed;
        self
    }

    /// Writes this node's geometry and declarations into `tree` (enzICED).
    ///
    /// A childless node leaves the tree's descendants as their own layout
    /// left them. Otherwise its children pair with the tree's in order: a
    /// node that places a child it did not lay out through `tree.children`
    /// is a layout defect. The one exception is a leaf widget: its tree has
    /// no children, so the node's children are its own synthetic parts (an
    /// input's text and caret) and stay with its declarations, reachable
    /// through [`Layout::synthetic`](super::Layout::synthetic).
    pub fn apply(mut self, tree: &mut Tree) {
        self.passed_sizing = None;
        self.measured_extent = None;
        tree.translation = Vector::new(self.bounds.x, self.bounds.y);
        tree.size = self.bounds.size();

        if tree.children.is_empty() {
            let keep = self.declares() || !self.children.is_empty();
            store(&mut tree.presentation, keep.then_some(self));
            return;
        }

        let children = std::mem::take(&mut self.children);
        let reversed = self.reversed_children;
        let placed = self.placed.clone();
        let keep = self.declares();
        store(&mut tree.presentation, keep.then_some(self));

        if children.is_empty() {
            return;
        }

        if let Some(placed) = placed {
            for (child, &index) in children.into_iter().zip(placed.iter()) {
                if let Some(tree) = tree.children.get_mut(index) {
                    place(child, tree);
                }
            }
            return;
        }

        debug_assert_eq!(
            children.len(),
            tree.children.len(),
            "a layout node's children must pair with its widget tree's"
        );

        fn place(child: Node, tree: &mut Tree) {
            let (child, slot) = if child.interposed && child.children.len() == 1 {
                let mut slot = child;
                let inner = slot.children.pop().expect("an interposed child");
                let offset = Vector::new(slot.bounds.x, slot.bounds.y);
                (inner.translate(offset), Some(Box::new(slot)))
            } else {
                (child, None)
            };
            // A childless node over a laid-out subtree must be what that
            // subtree last measured; an earlier probe's size would pair
            // with a later layout's descendants. Collapsing a child to
            // nothing (a hidden slot) is deliberate.
            debug_assert!(
                !child.children.is_empty()
                    || tree.children.is_empty()
                    || child.bounds.size() == tree.size
                    || child.bounds.size() == Size::ZERO,
                "a layout node placed a child measurement its tree no longer holds"
            );
            child.apply(tree);
            tree.interposed = slot;
        }

        if reversed {
            for (child, tree) in children.into_iter().zip(tree.children.iter_mut().rev()) {
                place(child, tree);
            }
        } else {
            for (child, tree) in children.into_iter().zip(&mut tree.children) {
                place(child, tree);
            }
        }
    }

    /// The only child of an undeclared wrapper that hands it its whole
    /// allocation (enzICED).
    ///
    /// Before layout lived in the widget tree such a wrapper returned its
    /// child's node as its own, so owned presentation never saw it; lowering
    /// and projection still look through it.
    pub(crate) fn transparent_child(&self) -> Option<&Node> {
        match self.children.as_slice() {
            [child]
                if !self.declares_layout()
                    && child.bounds == Rectangle::new(Point::ORIGIN, self.bounds.size()) =>
            {
                Some(child)
            }
            _ => None,
        }
    }

    /// Whether this node declares anything a wrapper cannot hand through
    /// to the child that covers it: everything but placement and a drawing
    /// cull, which moves onto that child.
    pub(crate) fn declares_layout(&self) -> bool {
        self.presentation_node().is_some()
            || self.presented_reveal_progress.is_some()
            || self.flex_presentation.is_some()
            || self.container_presentation.is_some()
            || self.rare().overlay_presentation.is_some()
            || self.reversed_children
    }

    fn declares_beyond_placement(&self) -> bool {
        self.presentation_node().is_some()
            || self.drawing_cull.is_some()
            || self.presented_reveal_progress.is_some()
            || self.flex_presentation.is_some()
            || self.container_presentation.is_some()
            || self.rare().overlay_presentation.is_some()
            || self.reversed_children
    }

    fn declares(&self) -> bool {
        self.declares_beyond_placement() || self.placed.is_some()
    }
}

/// Lays out a widget into its [`Tree`] and returns what it laid out as a
/// [`Node`] (enzICED).
///
/// The bridge between upstream's tree-owned layout and the fork's node
/// builders: a parent measures its children, arranges the returned nodes
/// and [applies](Node::apply) its own node to its tree.
pub trait Measure<Message, Theme, Renderer> {
    /// Lays out into `tree` and returns [`Node::of`] it, at the origin.
    fn measure(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &super::Limits) -> Node;

    /// Lays out into `tree` and returns its whole [`Node::snapshot`], for
    /// inspecting the laid out subtree (tests, probes); not for layout.
    fn measure_snapshot(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &super::Limits,
    ) -> Node;
}

impl<Message, Theme, Renderer, W> Measure<Message, Theme, Renderer> for W
where
    W: crate::Widget<Message, Theme, Renderer> + ?Sized,
    Renderer: crate::Renderer,
{
    fn measure(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &super::Limits) -> Node {
        self.layout(tree, renderer, limits);
        // At the origin, as a fresh layout result: the tree's translation is
        // where the parent placed it last time, and a parent that moves its
        // child relatively must not accumulate it.
        Node::of(tree).move_to(Point::ORIGIN)
    }

    fn measure_snapshot(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &super::Limits,
    ) -> Node {
        self.layout(tree, renderer, limits);
        Node::snapshot(tree).move_to(Point::ORIGIN)
    }
}

/// A [`Node`] placed at an absolute position: the fork's owned counterpart
/// of a [`Layout`](super::Layout), walked by presentation lowering.
#[derive(Debug, Clone, Copy)]
pub struct Placed<'a> {
    pub(super) position: Point,
    pub(super) node: &'a Node,
    pub(super) parent: Option<Rectangle>,
}

impl<'a> Placed<'a> {
    /// Identity of an explicitly declared allocation-relative clip scope.
    pub fn clip_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.node.clip_presentation().map(|(id, _)| id)
    }

    /// Allocation owner for container-bound decorations; absent on untyped layouts.
    pub fn container_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.node.container_node()
    }

    /// Captured editor allocation identity, if this node has a native editor contract.
    pub fn editor_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.node.editor_presentation().map(|(node, _)| node)
    }

    /// Native scroll identity used to bind draw-time viewport state and chrome.
    pub fn scroll_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.node.scroll_presentation().map(|(id, _)| id)
    }

    /// Draws a quad belonging to this container allocation, preserving ordinary paint on refusal.
    pub fn fill_presentation_quad(
        self,
        renderer: &mut impl crate::Renderer,
        quad: crate::renderer::Quad,
        background: crate::Background,
    ) {
        if let Some(node) = self.node.container_node() {
            renderer.fill_layout_quad(node, quad, background);
        } else {
            renderer.fill_quad(quad, background);
        }
    }

    /// Refuses responsive capture of this container when its paint cannot follow the owned allocation.
    pub fn reject_presentation_paint(self, renderer: &mut impl crate::Renderer) {
        if let Some(node) = self.node.container_node() {
            renderer.reject_layout_paint(node);
        }
    }

    /// Places `node` at the absolute `position`.
    pub fn new(position: Point, node: &'a Node) -> Self {
        Self {
            position,
            node,
            parent: None,
        }
    }

    /// Places `node` at its own position.
    pub fn of(node: &'a Node) -> Self {
        Self::with_offset(Vector::ZERO, node)
    }

    /// Lowers this owned layout like [`Layout::presentation_program`](super::Layout::presentation_program).
    ///
    /// # Errors
    /// Refuses unsupported descendants, as the widget path does.
    pub fn presentation_program(
        self,
        driver: crate::renderer::NodeId,
        channel: u8,
        clip: Rectangle,
    ) -> Result<
        std::sync::Arc<crate::renderer::presentation::Program>,
        crate::renderer::presentation::Error,
    > {
        super::presentation::lower(self, driver, channel, clip)
    }

    /// Lowers this owned layout like [`Layout::driven_presentation`](super::Layout::driven_presentation).
    ///
    /// # Errors
    /// Refuses opaque descendants, missing driven geometry, and invalid or over-budget declarations.
    pub fn driven_presentation(
        self,
        driver: crate::renderer::NodeId,
        clip: Rectangle,
    ) -> Result<
        std::sync::Arc<crate::renderer::presentation::Program>,
        crate::renderer::presentation::Error,
    > {
        super::presentation::lower_docked(self, driver, clip)
    }

    /// Places `node` at its own position, offset by `offset`.
    pub fn with_offset(offset: Vector, node: &'a Node) -> Self {
        Self::new(node.position() + offset, node)
    }

    /// The absolute position.
    pub fn position(&self) -> Point {
        self.position
    }

    /// The absolute bounds.
    pub fn bounds(&self) -> Rectangle {
        Rectangle::new(self.position, self.node.size())
    }

    /// The size.
    pub fn size(&self) -> Size {
        self.node.size()
    }

    /// The bounds of the parent, if any.
    pub fn parent(&self) -> Option<Rectangle> {
        self.parent
    }

    /// The placed node.
    pub fn node(&self) -> &'a Node {
        self.node
    }

    /// The [`Layout`](super::Layout) of the widget this node was measured
    /// from; its children are reached through that widget's tree.
    pub fn to_layout(self) -> super::Layout {
        super::Layout::new(self.node.size()).move_to(self.position)
    }

    /// The placed children.
    pub fn children(self) -> impl DoubleEndedIterator<Item = Placed<'a>> + ExactSizeIterator {
        let parent = self.bounds();
        let offset = Vector::new(self.position.x, self.position.y);

        self.node.children().iter().map(move |node| Placed {
            position: node.position() + offset,
            node,
            parent: Some(parent),
        })
    }

    /// The placed child at `index`.
    ///
    /// # Panics
    /// Panics if index is out of bounds.
    pub fn child(self, index: usize) -> Placed<'a> {
        let node = &self.node.children()[index];

        Placed {
            position: node.position() + Vector::new(self.position.x, self.position.y),
            node,
            parent: Some(self.bounds()),
        }
    }
}

impl Tree {
    /// The child a wrapper lays out as, when it hands that child its whole
    /// allocation (see [`super::pass_through`]) (enzICED).
    pub fn pass_through_child(&self) -> Option<usize> {
        let index = match self.presentation() {
            Some(node) if node.declares_layout() => return None,
            Some(node) => match node.placed.as_deref() {
                Some(&[index]) => index,
                None if self.children.len() == 1 => 0,
                _ => return None,
            },
            None if self.children.len() == 1 => 0,
            None => return None,
        };
        let child = self.children.get(index)?;
        (child.translation == Vector::ZERO && child.size == self.size).then_some(index)
    }

    /// The owned-presentation declarations of the last layout, if any (enzICED).
    pub fn presentation(&self) -> Option<&Node> {
        self.presentation.as_deref()
    }

    /// Identity shared by native geometry and its owned layout declaration,
    /// looking through undeclared wrappers that hand their only child their
    /// whole allocation (see [`Node::of`]).
    pub fn presentation_node(&self) -> Option<crate::renderer::NodeId> {
        let mut tree = self;

        loop {
            let placed = match tree.presentation() {
                Some(node) if node.declares_layout() => {
                    return node.presentation_node();
                }
                Some(node) => node.placed.as_deref(),
                None => None,
            };

            let child = match (placed, tree.children.as_slice()) {
                (Some(&[index]), children) => children.get(index),
                (None, [child]) => Some(child),
                _ => None,
            };

            match child {
                Some(child) if child.translation == Vector::ZERO && child.size == tree.size => {
                    tree = child;
                }
                _ => return None,
            }
        }
    }

    /// The document-strip declaration identity; absent for unrelated layouts.
    pub fn tab_strip_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?
            .tab_strip_presentation()
            .map(|(id, _)| id)
    }

    /// The band's declaration identity; absent for unrelated or opaque layouts.
    pub fn priority_band_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?.band_presentation().map(|(id, _)| id)
    }

    /// Identity of native media paint declared by this allocation.
    pub fn media_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?.media_presentation().map(|(id, _)| id)
    }

    /// Identity of an explicitly declared allocation-relative clip scope.
    pub fn clip_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?.clip_presentation().map(|(id, _)| id)
    }

    /// Progress carried by a source-matching layout projection, not a new clock sample.
    pub fn presented_reveal_progress(&self) -> Option<f32> {
        self.presentation()?.presented_reveal_progress
    }

    /// Native scroll identity used to bind draw-time viewport state and chrome.
    pub fn scroll_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?.scroll_presentation().map(|(id, _)| id)
    }

    /// Captured editor allocation identity, if this layout has a native editor contract.
    pub fn editor_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?
            .editor_presentation()
            .map(|(node, _)| node)
    }

    /// Allocation owner for container-bound decorations; absent on untyped layouts.
    pub fn container_presentation_node(&self) -> Option<crate::renderer::NodeId> {
        self.presentation()?.container_node()
    }
}

/// Lays `tree` out as its child `index`, which holds the whole allocation:
/// a wrapper whose layout is its child's (enzICED).
///
/// Before layout lived in the widget tree such a wrapper returned its
/// child's node as its own. The tree's other children (a popup laid out for
/// an overlay) stay unplaced, as they were absent from that node.
pub fn pass_through(tree: &mut Tree, index: usize) {
    tree.size = tree.children[index].size;
    tree.children[index].translation = Vector::ZERO;

    if tree.children.len() == 1 {
        tree.presentation = None;
        return;
    }

    // Laid out every frame: keep the declaration if it already says this.
    let current = tree.presentation.as_deref().is_some_and(|node| {
        !node.declares_beyond_placement() && node.placed.as_deref() == Some(&[index])
    });
    if !current {
        store(
            &mut tree.presentation,
            Some(Node {
                placed: Some(std::sync::Arc::from([index])),
                ..Node::default()
            }),
        );
    }
}

/// Stores a tree's declarations, reusing its allocation across layouts.
fn store(slot: &mut Option<Box<Node>>, node: Option<Node>) {
    match (slot.as_deref_mut(), node) {
        (Some(stored), Some(node)) => *stored = node,
        (None, Some(node)) => *slot = Some(Box::new(node)),
        (_, None) => *slot = None,
    }
}

/// The node of a widget that declared nothing.
pub(super) static EMPTY: Node = Node::new(Size::ZERO);
