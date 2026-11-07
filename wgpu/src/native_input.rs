// Modified by enzUI contributors. See FORK.md.
use crate::{Scene, core, graphics};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Frame {
    pub editors: BTreeMap<u64, Arc<graphics::text::editor::snapshot::Geometry>>,
    pub scrolls: BTreeMap<u64, Scroll>,
}

pub(crate) struct Scroll {
    source: core::layout::scroll::Input,
    current: core::layout::scroll::Input,
    config: core::layout::scroll::Config,
    viewport: core::Size,
    content: core::Size,
}

impl Scroll {
    pub fn reconcile(
        &self,
        config: core::layout::scroll::Config,
        input: core::layout::scroll::Input,
        viewport: core::Size,
        content: core::Size,
    ) -> Option<bool> {
        (config == self.config
            && input.valid()
            && input.offsets == self.source.offsets
            && input.scale == self.source.scale
            && viewport == self.viewport
            && content == self.content)
            .then_some(self.current.visible)
    }
}

impl Frame {
    pub fn capture_scrolls(
        &mut self,
        scene: &Scene,
        state: &core::renderer::presentation::State,
        inputs: &BTreeMap<u64, core::layout::scroll::Input>,
    ) -> Result<(), core::renderer::presentation::Error> {
        use core::renderer::presentation::{Error, MAX_ITEMS};
        let items = self
            .editors
            .values()
            .try_fold(scene.scrolls.len(), |sum, geometry| {
                sum.checked_add(geometry.retained_items())
                    .ok_or(Error::Budget)
            })?;
        if items > MAX_ITEMS {
            return Err(Error::Budget);
        }
        for (&node, source) in &scene.scrolls {
            let current = *inputs.get(&node).ok_or(Error::SceneIdentity(node))?;
            let viewport = state.bounds(node).ok_or(Error::SceneIdentity(node))?;
            let content = state
                .scroll_content(node)
                .ok_or(Error::SceneIdentity(node))?;
            if !current.valid()
                || current.offsets != source.input.offsets
                || current.scale != source.input.scale
            {
                return Err(Error::SceneIdentity(node));
            }
            let _ = self.scrolls.insert(
                node,
                Scroll {
                    source: source.input,
                    current,
                    config: source.config,
                    viewport: core::Size::new(viewport.width, viewport.height),
                    content: core::Size::new(content.width, content.height),
                },
            );
        }
        Ok(())
    }

    pub fn matches(&self, scene: &Scene, program: &core::renderer::presentation::Program) -> bool {
        let expected: BTreeSet<_> = program.editor_nodes().map(|(node, _)| node).collect();
        let scrolls: BTreeSet<_> = program.scroll_nodes().map(|(node, _)| node).collect();
        self.editors.keys().copied().eq(expected.iter().copied())
            && self.scrolls.keys().copied().eq(scrolls.iter().copied())
            && self.scrolls.iter().all(|(node, geometry)| {
                scene.scrolls.get(node).is_some_and(|source| {
                    geometry.source == source.input && geometry.config == source.config
                })
            })
            && self.editors.iter().all(|(node, geometry)| {
                scene
                    .editors
                    .get(node)
                    .is_some_and(|source| geometry.matches_source(&source.snapshot))
            })
    }
}
