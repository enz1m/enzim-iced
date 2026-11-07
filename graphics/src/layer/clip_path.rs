// Modified by enzUI contributors. See FORK.md.
use super::{cumulative_clip_bounds, inherited_clip_radius, intersected_clip_radius, nonempty};
use crate::core::renderer::{
    LayoutLayer,
    presentation::{Error, MAX_DEPTH},
};
use crate::core::{Rectangle, border};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
enum Origin {
    Scene,
    Unspecified,
    Layout(LayoutLayer),
}

/// An immutable chain of declared clip sources; ordinary unresolved layer pushes fail admission.
#[derive(Debug)]
pub struct ClipPath {
    parent: Option<Arc<Self>>,
    origin: Origin,
    bounds: Rectangle,
    radius: Option<border::Radius>,
    depth: usize,
}
impl ClipPath {
    /// Native batch provenance, ignoring visibility-only scopes while retaining real clip barriers.
    pub fn paint_allocation(path: &Arc<Self>) -> &Arc<Self> {
        let mut at = path;
        while matches!(
            at.origin,
            Origin::Layout(LayoutLayer::Material(_) | LayoutLayer::Cull(_))
        ) {
            let Some(parent) = &at.parent else { break };
            at = parent;
        }
        at
    }

    pub(super) fn next_allocation(&self) -> Arc<Self> {
        Arc::new(Self {
            parent: self.parent.clone(),
            origin: self.origin,
            bounds: self.bounds,
            radius: self.radius,
            depth: self.depth,
        })
    }

    /// Establishes the fixed capture coverage before responsive drawing begins.
    pub fn scene(bounds: Rectangle, radius: Option<border::Radius>) -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            origin: Origin::Scene,
            bounds,
            radius,
            depth: 1,
        })
    }
    pub(super) fn push(
        parent: &Arc<Self>,
        bounds: Rectangle,
        radius: Option<border::Radius>,
    ) -> Arc<Self> {
        if parent.depth > MAX_DEPTH {
            return parent.clone();
        }
        Arc::new(Self {
            parent: Some(parent.clone()),
            origin: Origin::Unspecified,
            bounds,
            radius,
            depth: parent.depth + 1,
        })
    }
    /// Binds only the just-opened scope, retaining the identities of all ancestors.
    pub fn bind(path: &Arc<Self>, source: LayoutLayer) -> Result<Arc<Self>, Error> {
        if path.depth > MAX_DEPTH || !matches!(path.origin, Origin::Unspecified) {
            return Err(Error::RecordingState);
        }
        if matches!(source, LayoutLayer::Inherited) {
            let parent = path.parent.as_ref().ok_or(Error::RecordingState)?;
            let bounds = parent.evaluate(|_, captured| Ok(captured))?.0;
            if path.radius.is_some()
                || (nonempty(bounds) && path.bounds.intersection(&bounds) != Some(bounds))
            {
                return Err(Error::ClipCoverage(0));
            }
            return Ok(parent.clone());
        }
        Ok(Arc::new(Self {
            parent: path.parent.clone(),
            origin: Origin::Layout(source),
            bounds: path.bounds,
            radius: path.radius,
            depth: path.depth,
        }))
    }
    /// Whether all existing scopes were explicitly bound; this is not source-revision validation.
    pub fn is_declared(&self) -> bool {
        let mut at = Some(self);
        while let Some(path) = at {
            if path.depth > MAX_DEPTH || matches!(path.origin, Origin::Unspecified) {
                return false;
            }
            at = path.parent.as_deref();
        }
        true
    }
    /// Evaluates clip sources using the same rectangular and rounded intersection rules as native layers.
    pub fn evaluate(
        &self,
        mut resolve: impl FnMut(LayoutLayer, Rectangle) -> Result<Rectangle, Error>,
    ) -> Result<(Rectangle, Option<border::Radius>), Error> {
        if self.depth > MAX_DEPTH {
            return Err(Error::Budget);
        }
        let mut chain = [None; MAX_DEPTH];
        let mut length = 0;
        let mut at = Some(self);
        while let Some(path) = at {
            if length == chain.len() {
                return Err(Error::Budget);
            }
            chain[length] = Some(path);
            length += 1;
            at = path.parent.as_deref();
        }
        let mut result: Option<(Rectangle, Option<border::Radius>)> = None;
        for path in chain[..length].iter().rev() {
            let path = path.ok_or(Error::RecordingState)?;
            let requested = match path.origin {
                Origin::Scene if result.is_none() => path.bounds,
                Origin::Layout(LayoutLayer::Inherited) => result.ok_or(Error::RecordingState)?.0,
                Origin::Layout(source) => resolve(source, path.bounds)?,
                _ => return Err(Error::OpaquePaint),
            };
            if ![
                requested.x,
                requested.y,
                requested.width,
                requested.height,
                requested.x + requested.width,
                requested.y + requested.height,
            ]
            .into_iter()
            .all(f32::is_finite)
                || requested.width < 0.0
                || requested.height < 0.0
            {
                return Err(Error::NonFinite);
            }
            if let Some(radius) = path.radius {
                let values: [f32; 4] = radius.into();
                if !values.into_iter().all(|v| v.is_finite() && v >= 0.0) {
                    return Err(Error::NonFinite);
                }
            }
            let (bounds, radius) = if let Some((parent, parent_radius)) = result {
                let bounds = cumulative_clip_bounds(parent, requested);
                let own = if nonempty(bounds) {
                    path.radius
                        .and_then(|r| inherited_clip_radius(requested, r, bounds))
                } else {
                    None
                };
                let inherited = if nonempty(bounds) {
                    parent_radius.and_then(|r| inherited_clip_radius(parent, r, bounds))
                } else {
                    None
                };
                (bounds, intersected_clip_radius(own, inherited))
            } else {
                (requested, path.radius)
            };
            result = Some((bounds, radius));
        }
        result.ok_or(Error::RecordingState)
    }
}
