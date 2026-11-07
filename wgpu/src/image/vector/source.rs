// Modified by enzUI contributors. See FORK.md.
use super::*;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Source {
    pub(crate) handle: svg::Handle,
    pub(super) tree: Arc<usvg::Tree>,
    pub(crate) work: usize,
    pub(crate) encoded_bytes: u64,
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorSource")
            .field("handle", &self.handle)
            .field("work", &self.work)
            .field("encoded_bytes", &self.encoded_bytes)
            .finish_non_exhaustive()
    }
}
impl Source {
    pub(crate) fn size(&self) -> Size<u32> {
        let size = self.tree.size();
        Size::new(size.width() as u32, size.height() as u32)
    }
    pub(crate) fn identity(&self) -> usize {
        Arc::as_ptr(&self.tree) as usize
    }
    pub(crate) fn same_revision(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tree, &other.tree)
    }
}

pub(super) fn capture(
    handle: &svg::Handle,
    tree: &Arc<usvg::Tree>,
    encoded_bytes: u64,
) -> Option<Arc<Source>> {
    if encoded_bytes > 64 * 1024 * 1024 {
        return None;
    }
    let size = tree.size();
    if size.width() < 1.0 || size.height() < 1.0 {
        return None;
    }
    let mut work = 0usize;
    let mut pending = vec![(tree.root(), 0usize)];
    while let Some((group, depth)) = pending.pop() {
        if depth >= crate::core::renderer::presentation::MAX_DEPTH || !group.filters().is_empty() {
            return None;
        }
        // Clip paths and masks rasterize like groups, so their contents are
        // charged to the same work bound instead of refusing the source.
        let mut clip = group.clip_path();
        while let Some(path) = clip {
            work = work.checked_add(1)?;
            pending.push((path.root(), depth + 1));
            clip = path.clip_path();
        }
        let mut mask = group.mask();
        while let Some(layer) = mask {
            work = work.checked_add(1)?;
            pending.push((layer.root(), depth + 1));
            mask = layer.mask();
        }
        for node in group.children() {
            work = work.checked_add(1)?;
            match node {
                usvg::Node::Group(child) => pending.push((child.as_ref(), depth + 1)),
                usvg::Node::Path(path) => {
                    for _ in path.data().segments() {
                        work = work.checked_add(1)?;
                        if work > 1_048_576 {
                            return None;
                        }
                    }
                    for paint in path
                        .fill()
                        .map(resvg::usvg::Fill::paint)
                        .into_iter()
                        .chain(path.stroke().map(resvg::usvg::Stroke::paint))
                    {
                        work = work.checked_add(match paint {
                            usvg::Paint::Color(_) => 1,
                            usvg::Paint::LinearGradient(gradient) => gradient.stops().len(),
                            usvg::Paint::RadialGradient(gradient) => gradient.stops().len(),
                            usvg::Paint::Pattern(_) => return None,
                        })?;
                    }
                    if let Some(stroke) = path.stroke() {
                        work = work.checked_add(stroke.dasharray().map_or(0, <[f32]>::len))?;
                    }
                }
                _ => return None,
            }
            if work > 1_048_576 || pending.len() > crate::core::renderer::presentation::MAX_ITEMS {
                return None;
            }
        }
    }
    Some(Arc::new(Source {
        handle: handle.clone(),
        tree: tree.clone(),
        work,
        encoded_bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(markup: &str) -> Option<Arc<Source>> {
        let tree =
            usvg::Tree::from_data(markup.as_bytes(), &usvg::Options::default()).expect("valid svg");
        capture(
            &svg::Handle::from_memory(markup.as_bytes().to_vec()),
            &Arc::new(tree),
            markup.len() as u64,
        )
    }

    /// Application icons commonly clip their artwork. Such an icon is a
    /// replayable source, so a scene that shows it can still be presented by
    /// the render lane.
    ///
    /// Sabotage: refuse groups with a clip path or a mask again -- both icons
    /// are refused.
    #[test]
    fn an_icon_with_a_clip_path_or_a_mask_is_a_replayable_source() {
        let clipped = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
            <clipPath id="c"><rect x="2" y="2" width="12" height="12" rx="3"/></clipPath>
            <g clip-path="url(#c)"><rect width="16" height="16" fill="#3584e4"/></g>
        </svg>"##;
        let masked = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
            <mask id="m"><rect width="8" height="16" fill="#fff"/></mask>
            <g mask="url(#m)"><rect width="16" height="16" fill="#3584e4"/></g>
        </svg>"##;

        let clipped = source(clipped).expect("a clipped icon is replayable");
        let masked = source(masked).expect("a masked icon is replayable");
        assert!(
            clipped.work > 2,
            "the clip path's contents are charged as work"
        );
        assert!(masked.work > 2, "the mask's contents are charged as work");
    }

    /// The control: a filter has no bounded replay cost, so it stays refused.
    #[test]
    fn an_icon_with_a_filter_is_not_a_replayable_source() {
        let filtered = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
            <filter id="f"><feGaussianBlur stdDeviation="2"/></filter>
            <g filter="url(#f)"><rect width="16" height="16" fill="#3584e4"/></g>
        </svg>"##;

        assert!(source(filtered).is_none());
    }
}
