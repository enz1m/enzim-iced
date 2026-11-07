// Modified by enzUI contributors. See FORK.md.
use crate::{Layer, Scene, Segment, core, graphics, presentation_bounds};
use core::renderer::presentation::{Error, MAX_ITEMS, Media, MediaKind, Program, State};
use core::{Rectangle, Transformation};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const MAX_SCENE_MEDIA_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub(crate) struct Assets {
    #[cfg(feature = "image")]
    rasters: BTreeMap<core::image::Id, Arc<crate::image::raster::source::Source>>,
    #[cfg(feature = "image")]
    raster_allocations: BTreeMap<core::image::Id, core::image::Allocation>,
    #[cfg(feature = "svg")]
    vectors: BTreeMap<u64, Arc<crate::image::vector::source::Source>>,
    // Last so usage stays charged while retained source handles are destroyed.
    permits: Vec<graphics::resource::Permit>,
}
impl Assets {
    /// `scale` is the device scale the recording is drawn at, which sizes the
    /// raster an SVG the lane cannot rasterize is frozen to.
    pub(crate) fn capture(
        cache: &mut crate::image::Cache,
        segments: &mut [Segment],
        scale: f32,
    ) -> Result<Self, Error> {
        let mut assets = Self::default();
        let mut items = 0;
        let mut bytes = 0usize;
        // An image the frame shows nothing for (still loading, or failed to
        // load) is no paint to replay, not a reason to refuse the scene
        // (enzUI-ICED #3660).
        #[cfg(feature = "image")]
        for layer in segments.iter_mut().flat_map(|segment| &mut segment.layers) {
            layer.images.retain(|image| match image {
                graphics::Image::Raster { image, .. } => !cache.raster_shows_nothing(&image.handle),
                #[allow(
                    unreachable_patterns,
                    reason = "vectors exist only with the svg feature"
                )]
                _ => true,
            });
        }
        let mut vector_work = 0usize;
        for image in segments
            .iter_mut()
            .flat_map(|segment| &mut segment.layers)
            .flat_map(|layer| &mut layer.images)
        {
            if !valid_paint(image) {
                return Err(Error::NonFinite);
            }
            items += 1;
            if items > MAX_ITEMS {
                return Err(Error::Budget);
            }
            match image {
                #[cfg(feature = "image")]
                graphics::Image::Raster { image, .. } => {
                    let id = image.handle.id();
                    if !assets.rasters.contains_key(&id) {
                        let source = cache
                            .raster_source(&image.handle)
                            .ok_or(Error::MediaAssets)?;
                        bytes = bytes.checked_add(source.bytes()).ok_or(Error::Budget)?;
                        if bytes > MAX_SCENE_MEDIA_BYTES {
                            return Err(Error::Budget);
                        }
                        let _ = assets.rasters.insert(id, source);
                    }
                    image.handle = assets.rasters[&id].handle.clone();
                }
                #[cfg(feature = "svg")]
                graphics::Image::Vector {
                    svg,
                    bounds,
                    clip_bounds,
                } => {
                    let id = svg.handle.id();
                    if !assets.vectors.contains_key(&id) {
                        let Some(source) = cache.vector_source(&svg.handle) else {
                            // The lane cannot rasterize this SVG (filters,
                            // patterns, text, images): it draws the raster the
                            // UI thread freezes at the drawn size instead.
                            #[cfg(feature = "image")]
                            {
                                let size =
                                    crate::nudge::round(*bounds * Transformation::scale(scale));
                                if size.width < 1.0 || size.height < 1.0 {
                                    continue;
                                }
                                let frozen = cache
                                    .frozen_vector(
                                        &svg.handle,
                                        svg.color,
                                        core::Size::new(size.width as u32, size.height as u32),
                                    )
                                    .ok_or(Error::MediaAssets)?;
                                let raster = graphics::Image::Raster {
                                    image: core::Image {
                                        handle: frozen.handle.clone(),
                                        filter_method: core::image::FilterMethod::Linear,
                                        rotation: svg.rotation,
                                        border_radius: core::border::Radius::default(),
                                        opacity: svg.opacity,
                                    },
                                    bounds: *bounds,
                                    clip_bounds: *clip_bounds,
                                };
                                let frozen_id = frozen.handle.id();
                                if !assets.rasters.contains_key(&frozen_id) {
                                    bytes =
                                        bytes.checked_add(frozen.bytes()).ok_or(Error::Budget)?;
                                    if bytes > MAX_SCENE_MEDIA_BYTES {
                                        return Err(Error::Budget);
                                    }
                                    let _ = assets.rasters.insert(frozen_id, frozen);
                                }
                                *image = raster;
                                continue;
                            }
                            #[cfg(not(feature = "image"))]
                            return Err(Error::MediaAssets);
                        };
                        vector_work = vector_work.checked_add(source.work).ok_or(Error::Budget)?;
                        bytes = bytes
                            .checked_add(
                                usize::try_from(source.encoded_bytes).map_err(|_| Error::Budget)?,
                            )
                            .ok_or(Error::Budget)?;
                        if vector_work > 1_048_576 || bytes > MAX_SCENE_MEDIA_BYTES {
                            return Err(Error::Budget);
                        }
                        let _ = assets.vectors.insert(id, source);
                    }
                }
                #[cfg(not(all(feature = "image", feature = "svg")))]
                _ => return Err(Error::UnsupportedBackend),
            }
        }
        let mut resources = 0usize;
        #[cfg(feature = "image")]
        {
            resources = resources
                .checked_add(assets.rasters.len())
                .ok_or(Error::Budget)?;
        }
        #[cfg(feature = "svg")]
        {
            resources = resources
                .checked_add(assets.vectors.len())
                .ok_or(Error::Budget)?;
        }
        assets.admit(graphics::resource::media_admission(), resources, bytes)?;
        Ok(assets)
    }

    fn admit(
        &mut self,
        admission: &Arc<graphics::resource::Admission>,
        resources: usize,
        bytes: usize,
    ) -> Result<(), Error> {
        if resources > 0 {
            self.permits.push(
                admission
                    .reserve(resources, bytes)
                    .map_err(|_| Error::Budget)?,
            );
        }
        Ok(())
    }
    fn freeze(&self, paint: &graphics::Image) -> Result<graphics::Image, Error> {
        let mut paint = paint.clone();
        match &mut paint {
            #[cfg(feature = "image")]
            graphics::Image::Raster { image, .. } => {
                image.handle = self
                    .rasters
                    .get(&image.handle.id())
                    .ok_or(Error::MediaAssets)?
                    .handle
                    .clone();
            }
            #[cfg(feature = "svg")]
            graphics::Image::Vector { svg, .. } => {
                if !self.vectors.contains_key(&svg.handle.id()) {
                    return Err(Error::MediaAssets);
                }
            }
            #[cfg(not(all(feature = "image", feature = "svg")))]
            _ => return Err(Error::UnsupportedBackend),
        }
        Ok(paint)
    }
    fn size(&self, paint: &graphics::Image) -> Result<core::Size<u32>, Error> {
        match paint {
            #[cfg(feature = "image")]
            graphics::Image::Raster { image, .. } => Ok(self
                .rasters
                .get(&image.handle.id())
                .ok_or(Error::MediaAssets)?
                .size()),
            #[cfg(feature = "svg")]
            graphics::Image::Vector { svg, .. } => Ok(self
                .vectors
                .get(&svg.handle.id())
                .ok_or(Error::MediaAssets)?
                .size()),
            #[cfg(not(all(feature = "image", feature = "svg")))]
            _ => Err(Error::UnsupportedBackend),
        }
    }
    pub(crate) fn validate_viewport<'a>(
        &self,
        layers: impl Iterator<Item = &'a Layer>,
        scale: f32,
        maximum: u32,
    ) -> Result<(), Error> {
        #[cfg(feature = "svg")]
        {
            if self.vectors.is_empty() {
                return Ok(());
            }
            if !scale.is_finite() || scale <= 0.0 {
                return Err(Error::Viewport);
            }
            let mut seen = BTreeSet::new();
            let mut pixels = 0u64;
            for layer in layers {
                for image in &layer.images {
                    if let graphics::Image::Vector { svg, bounds, .. } = image {
                        if !self.vectors.contains_key(&svg.handle.id()) {
                            return Err(Error::MediaAssets);
                        }
                        // The same nudged rounding as the image pipeline
                        // (upstream #3466), so the budgeted raster key is the
                        // size that pipeline rasterizes at.
                        let bounds = crate::nudge::round(*bounds * scale);
                        if !finite(bounds)
                            || bounds.width > maximum as f32
                            || bounds.height > maximum as f32
                        {
                            return Err(Error::Budget);
                        }
                        let key = (
                            svg.handle.id(),
                            bounds.width as u32,
                            bounds.height as u32,
                            svg.color.map(core::Color::into_rgba8),
                        );
                        if seen.insert(key) {
                            pixels = pixels
                                .checked_add(u64::from(key.1) * u64::from(key.2))
                                .ok_or(Error::Budget)?;
                        }
                        if pixels > 32 * 1024 * 1024 {
                            return Err(Error::Budget);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn install(
        &self,
        #[cfg(feature = "image")] device: &wgpu::Device,
        #[cfg(feature = "image")] queue: &wgpu::Queue,
        cache: &mut crate::image::Cache,
        claims: &mut Self,
        native: &BTreeMap<u64, Option<usize>>,
        native_overflow: bool,
    ) -> Result<(), Error> {
        #[cfg(feature = "svg")]
        {
            if native_overflow && !self.vectors.is_empty() {
                return Err(Error::Budget);
            }
            for (id, source) in &self.vectors {
                if native
                    .get(id)
                    .is_some_and(|identity| *identity != Some(source.identity()))
                {
                    return Err(Error::MediaAssets);
                }
                if claims
                    .vectors
                    .get(id)
                    .is_some_and(|previous| !previous.same_revision(source))
                {
                    return Err(Error::MediaAssets);
                }
            }
        }
        #[cfg(feature = "image")]
        {
            let retained_bytes =
                claims
                    .raster_allocations
                    .values()
                    .try_fold(0usize, |bytes, allocation| {
                        let size = allocation.size();
                        let allocation_bytes = (size.width as usize)
                            .checked_mul(size.height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                            .ok_or(Error::Budget)?;
                        bytes.checked_add(allocation_bytes).ok_or(Error::Budget)
                    })?;
            let total_bytes = self
                .rasters
                .values()
                .try_fold(retained_bytes, |bytes, source| {
                    if claims.raster_allocations.contains_key(&source.handle.id()) {
                        Ok(bytes)
                    } else {
                        bytes.checked_add(source.bytes()).ok_or(Error::Budget)
                    }
                })?;
            if total_bytes > MAX_SCENE_MEDIA_BYTES {
                return Err(Error::Budget);
            }
            let mut allocations = BTreeMap::new();
            for source in self.rasters.values() {
                let id = source.handle.id();
                if !claims.raster_allocations.contains_key(&id) {
                    // Frozen handles have fresh IDs, so asynchronous uploads omit their first replay frame.
                    let allocation = cache
                        .load_scene_image(device, queue, &source.handle)
                        .map_err(|error| match error {
                            core::image::Error::OutOfMemory => Error::Budget,
                            _ => Error::MediaAssets,
                        })?;
                    let _ = allocations.insert(id, allocation);
                }
            }
            claims.raster_allocations.extend(allocations);
            claims.rasters.extend(
                self.rasters
                    .iter()
                    .map(|(id, source)| (*id, source.clone())),
            );
        }
        #[cfg(feature = "svg")]
        for (id, source) in &self.vectors {
            cache.install_vector_source(source);
            let _ = claims.vectors.insert(*id, source.clone());
        }
        claims.retain_permits(self);
        Ok(())
    }

    fn retain_permits(&mut self, assets: &Self) {
        for permit in &assets.permits {
            if !self
                .permits
                .iter()
                .any(|retained| retained.same_reservation(permit))
            {
                self.permits.push(permit.clone());
            }
        }
    }

    pub(crate) fn usage(&self) -> (usize, usize) {
        let mut resources = 0usize;
        let mut bytes = 0usize;
        #[cfg(feature = "image")]
        {
            resources = resources.saturating_add(self.rasters.len());
            bytes = self
                .rasters
                .values()
                .fold(bytes, |bytes, source| bytes.saturating_add(source.bytes()));
        }
        #[cfg(feature = "svg")]
        {
            resources = resources.saturating_add(self.vectors.len());
            bytes = self.vectors.values().fold(bytes, |bytes, source| {
                bytes.saturating_add(usize::try_from(source.encoded_bytes).unwrap_or(usize::MAX))
            });
        }
        (resources, bytes)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub(crate) paint: graphics::Image,
    pub(crate) scale: f32,
}
impl Captured {
    pub(crate) fn valid(&self, media: &Media) -> bool {
        let (kind, rotation, opacity, ink, bounds, clip) = match &self.paint {
            graphics::Image::Raster {
                image,
                bounds,
                clip_bounds,
            } => {
                let radii: [f32; 4] = image.border_radius.into();
                (
                    MediaKind::Raster,
                    image.rotation,
                    image.opacity,
                    radii.into_iter().all(|v| v.is_finite() && v >= 0.0),
                    *bounds,
                    *clip_bounds,
                )
            }
            graphics::Image::Vector {
                svg,
                bounds,
                clip_bounds,
            } => {
                let ink = svg
                    .color
                    .is_none_or(|c| [c.r, c.g, c.b, c.a].into_iter().all(f32::is_finite));
                (
                    MediaKind::Vector,
                    svg.rotation,
                    svg.opacity,
                    ink,
                    *bounds,
                    *clip_bounds,
                )
            }
        };
        kind == media.kind
            && rotation == media.geometry.rotation.radians()
            && opacity.is_finite()
            && (0.0..=1.0).contains(&opacity)
            && ink
            && self.scale.is_finite()
            && self.scale > 0.0
            && presentation_bounds(clip) == media.recorded
            && bounds == media.geometry.drawing_bounds(clip, self.scale)
            && finite(bounds)
    }
    fn image_at(
        &self,
        assets: &Assets,
        media: &Media,
        bounds: Rectangle,
    ) -> Result<graphics::Image, Error> {
        let mut image = assets.freeze(&self.paint)?;
        let drawing = media.geometry.drawing_bounds(bounds, self.scale);
        if !finite(drawing) {
            return Err(Error::NonFinite);
        }
        if drawing.width > 16_384.0
            || drawing.height > 16_384.0
            || drawing.width * drawing.height > 32.0 * 1024.0 * 1024.0
        {
            return Err(Error::Budget);
        }
        match &mut image {
            graphics::Image::Raster {
                bounds: destination,
                clip_bounds,
                ..
            }
            | graphics::Image::Vector {
                bounds: destination,
                clip_bounds,
                ..
            } => {
                *destination = drawing;
                *clip_bounds = bounds;
            }
        }
        Ok(image)
    }
}
fn valid_paint(paint: &graphics::Image) -> bool {
    let (bounds, clip, rotation, opacity, ink) = match paint {
        graphics::Image::Raster {
            image,
            bounds,
            clip_bounds,
        } => {
            let radii: [f32; 4] = image.border_radius.into();
            (
                *bounds,
                *clip_bounds,
                image.rotation.0,
                image.opacity,
                radii.into_iter().all(|v| v.is_finite() && v >= 0.0),
            )
        }
        graphics::Image::Vector {
            svg,
            bounds,
            clip_bounds,
        } => (
            *bounds,
            *clip_bounds,
            svg.rotation.0,
            svg.opacity,
            svg.color
                .is_none_or(|c| [c.r, c.g, c.b, c.a].into_iter().all(f32::is_finite)),
        ),
    };
    finite(bounds)
        && finite(clip)
        && rotation.is_finite()
        && opacity.is_finite()
        && (0.0..=1.0).contains(&opacity)
        && ink
}

fn finite(bounds: Rectangle) -> bool {
    presentation_bounds(bounds).contains(presentation_bounds(bounds))
}
fn rectangle(bounds: core::renderer::presentation::Bounds) -> Rectangle {
    Rectangle {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

pub(crate) fn normalize_layer(
    layer: &mut Layer,
    capture: &Captured,
    assets: &Assets,
    transform: Transformation,
) -> Result<Vec<graphics::Image>, Error> {
    let mut expected = Layer::default();
    expected.draw_image(assets.freeze(&capture.paint)?, transform);
    if layer.images != expected.images
        || !layer.quads.is_empty()
        || !layer.text.is_empty()
        || !layer.triangles.is_empty()
        || !layer.primitives.is_empty()
    {
        return Err(Error::OpaquePaint);
    }
    let _ = std::mem::take(&mut layer.images);
    Ok(vec![assets.freeze(&capture.paint)?])
}

pub(crate) fn validate(scene: &Scene, program: &Program) -> Result<(), Error> {
    if scene.media.len() != program.media_nodes().count() {
        return Err(Error::OpaquePaint);
    }
    let mut seen = BTreeSet::new();
    for (id, media) in program.media_nodes() {
        let capture = scene.media.get(&id).ok_or(Error::SceneIdentity(id))?;
        if !capture.valid(media)
            || scene.media_assets.size(&capture.paint)? != media.geometry.source
        {
            return Err(Error::SceneIdentity(id));
        }
        let mut expected = Layer::default();
        expected.draw_image(
            scene.media_assets.freeze(&capture.paint)?,
            Transformation::IDENTITY,
        );
        for segment in &scene.segments {
            if segment.path.last().is_none_or(|node| node.0 != id) {
                continue;
            }
            for layer in &segment.layers {
                if layer.is_empty() {
                    continue;
                }
                if layer.images != expected.images
                    || !layer.quads.is_empty()
                    || !layer.text.is_empty()
                    || !layer.triangles.is_empty()
                    || !layer.primitives.is_empty()
                    || !seen.insert(id)
                {
                    return Err(Error::OpaquePaint);
                }
            }
        }
    }
    if seen.len() != scene.media.len() {
        return Err(Error::OpaquePaint);
    }
    Ok(())
}

pub(crate) fn apply(
    scene: &Scene,
    program: &Program,
    state: &State,
    segments: &mut [Segment],
) -> Result<(), Error> {
    for segment in segments {
        let Some(id) = segment.path.last().map(|node| node.0) else {
            continue;
        };
        let Some(capture) = scene.media.get(&id) else {
            continue;
        };
        let (_, media) = program
            .media_nodes()
            .find(|(node, _)| *node == id)
            .ok_or(Error::SceneIdentity(id))?;
        let allocation = state.bounds(id).ok_or(Error::SceneIdentity(id))?;
        let bounds = Rectangle {
            width: allocation.width,
            height: allocation.height,
            ..rectangle(media.recorded)
        };
        let image = capture.image_at(&scene.media_assets, media, bounds)?;
        for layer in &mut segment.layers {
            if !layer.images.is_empty() {
                layer.images = vec![image.clone()];
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_reclamation_media_admission_refuses_count_and_bytes() {
        let admission = graphics::resource::Admission::new(1, 8);
        let mut first = Assets::default();
        first
            .admit(&admission, 1, 8)
            .expect("first scene should be admitted");
        let mut refused = Assets::default();

        assert!(matches!(
            refused.admit(&admission, 1, 0),
            Err(Error::Budget)
        ));
        drop(first);
        assert!(matches!(
            refused.admit(&admission, 1, 9),
            Err(Error::Budget)
        ));
        assert_eq!(admission.usage(), graphics::resource::Usage::default());
    }

    #[test]
    fn resource_reclamation_media_claim_releases_final_reservation() {
        let admission = graphics::resource::Admission::new(1, 8);
        let mut scene = Assets::default();
        scene
            .admit(&admission, 1, 8)
            .expect("scene should be admitted");
        let mut claims = Assets::default();

        claims.retain_permits(&scene);
        claims.retain_permits(&scene);
        drop(scene);
        assert_eq!(
            admission.usage(),
            graphics::resource::Usage {
                resources: 1,
                bytes: 8,
            }
        );

        drop(claims);
        assert_eq!(admission.usage(), graphics::resource::Usage::default());
    }
}
