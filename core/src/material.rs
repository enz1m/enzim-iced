// Modified by enzUI contributors. See FORK.md.
//! The renderer-facing description of a sampled-backdrop material
//! (enzUI-ICED #2950).
//!
//! A material is a region whose colour is derived from the content painted
//! *under* it: blurred, refracted at its bevel, tinted and contrast-protected.
//! No widget can express that, because a widget cannot read the frame it is
//! painting into; only a backend that owns the render target can resolve the
//! content so far into something sampleable.
//!
//! This module carries the part both sides must agree on:
//!
//! - [`Material`], the validated parameters of one region, in **logical**
//!   pixels;
//! - [`Support`], what the active backend can actually do, so a caller can
//!   paint an honest opaque fallback instead of a hole;
//! - the optics ([`Material::displacement_at`], [`Material::sample_padding`])
//!   that decide how far outside a region the backend must sample;
//! - [`Uniform`], the exact block the composite shader reads.
//!
//! The renderer entry points are
//! [`Renderer::material_support`](crate::Renderer::material_support) and
//! [`Renderer::start_layer_material`](crate::Renderer::start_layer_material).
//!
//! # Units
//!
//! Every length here is a logical pixel, like the rest of the widget layer.
//! [`Uniform::new`] applies the scale factor once, where the backend converts
//! to its physical target.

use crate::{Color, Point, Rectangle, Size};

/// What the active backend can do with a [`Material`].
///
/// A caller must ask before it decides what to paint: an optical material
/// paints nothing of its own behind its content, while an unsupported one has
/// to paint an opaque panel. Guessing either way is what produces a hole or a
/// fake blur.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Support {
    /// The backend cannot sample the backdrop: unsupported target format,
    /// unsupported backend, a lost device, or a platform preference for
    /// reduced transparency or increased contrast.
    Unsupported,
    /// The backend filters a sampled backdrop, with a ceiling on the filter
    /// radius it will spend a pass on.
    Optical {
        /// The largest blur sigma, in logical pixels, the backend honours.
        max_blur_sigma: f32,
    },
}

impl Support {
    /// The backend's blur ceiling, if it filters at all.
    pub fn max_blur_sigma(self) -> Option<f32> {
        match self {
            Self::Unsupported => None,
            Self::Optical { max_blur_sigma } => Some(max_blur_sigma),
        }
    }

    /// Whether this backend filters a sampled backdrop.
    pub fn is_optical(self) -> bool {
        matches!(self, Self::Optical { .. })
    }
}

/// One material region's parameters, in logical pixels.
///
/// The caller owns the semantics — which named style produced these numbers,
/// and whether the platform allows a material at all. The backend owns only
/// what they mean optically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Corner radii of the region, clockwise from the top left.
    pub radii: [f32; 4],
    /// Gaussian sigma of the backdrop blur.
    pub blur_sigma: f32,
    /// Gain applied to the refracted displacement; `0.0` removes refraction
    /// without removing the blur.
    pub refraction: f32,
    /// Lateral width of the refracting bevel, measured inward.
    pub bevel_width: f32,
    /// Refractive index of the slab. `1.0` or below does not refract.
    pub index_of_refraction: f32,
    /// Per-channel spread of the refractive index.
    pub dispersion: f32,
    /// Strength of the colour that bleeds inward from behind the rim.
    pub color_diffusion: f32,
    /// How far that colour reaches inward.
    pub color_reach: f32,
    /// Distance a refracted ray travels to the backing plane.
    pub optical_depth: f32,
    /// The material's own colour: the tint mixed into the filtered backdrop,
    /// and the colour an unsupported backend must fill the region with.
    pub tint: Color,
    /// How much of `tint` is mixed in. `1.0` is fully opaque, so the backdrop
    /// is not worth sampling.
    pub tint_opacity: f32,
    /// Saturation applied to the filtered backdrop.
    pub saturation: f32,
    /// Edge highlight strength (the library's `edgeHighlight`).
    pub highlight: f32,
    /// Brightness change of the glass, `-0.5` to `0.5`.
    pub brightness: f32,
    /// Direction of the light, in degrees.
    pub light_angle: f32,
    /// Exponent of the bevel's cross-section profile.
    pub profile_curvature: f32,
    /// How strongly the filtered backdrop is pulled toward a legible
    /// luminance under the region's own content.
    pub contrast_protection: f32,
    /// Inset within which contrast protection reaches full strength. The rim
    /// itself is protected at 70%.
    pub protected_inset: f32,
    /// A key under which the renderer measures the backdrop's average
    /// brightness, for content that adapts its ink to it; `0` measures
    /// nothing. Read the result back with
    /// [`Renderer::material_luminance`](crate::Renderer::material_luminance).
    pub probe: u64,
    /// Measure the backdrop under [`probe`](Self::probe) without drawing
    /// the region: for adaptive content that must not appear before it knows
    /// its ink.
    pub measure_only: bool,
}

impl Material {
    /// Whether the region is opaque, so sampling a backdrop would not change
    /// one pixel of it.
    pub fn is_opaque(&self) -> bool {
        self.tint_opacity >= 1.0
    }

    /// Whether this material displaces the backdrop at all.
    pub fn refracts(&self) -> bool {
        self.refraction > 0.0 && self.index_of_refraction > 1.0 && self.optical_depth > 0.0
    }

    /// The same material with its blur clamped to what `support` will spend.
    ///
    /// An unsupported backend returns `None`: there is nothing to clamp to,
    /// and the caller must paint its opaque fallback.
    pub fn within(self, support: Support) -> Option<Self> {
        let max_blur_sigma = support.max_blur_sigma()?;

        Some(Self {
            blur_sigma: self.blur_sigma.min(max_blur_sigma.max(0.0)),
            ..self
        })
    }

    /// Half of the shorter side of `size`: the cap every optical length obeys.
    fn short_half(size: Size) -> f32 {
        if size.width <= 0.0 || size.height <= 0.0 {
            0.0
        } else {
            size.width.min(size.height) / 2.0
        }
    }

    /// The single radius the composite pass evaluates its signed distance
    /// field with: the largest declared corner, capped to the shape.
    pub fn uniform_radius(&self, size: Size) -> f32 {
        self.radii
            .iter()
            .copied()
            .filter(|radius| radius.is_finite())
            .fold(0.0_f32, f32::max)
            .clamp(0.0, Self::short_half(size))
    }

    /// The bevel width actually used for a region of `size`, in logical
    /// pixels.
    ///
    /// This is the radius of the glass's rounded edge (see [`rim_height`]),
    /// capped at the shape's short half: a pill is rounded all the way across
    /// and flat exactly at its centre line, so the slopes from opposite edges
    /// never meet head on there. A large panel has a flat top beyond it. The
    /// shader's twin must agree.
    pub fn effective_bevel_width(&self, size: Size) -> f32 {
        let short_half = Self::short_half(size);

        if short_half <= 0.0 {
            return 0.0;
        }

        self.bevel_width.min(short_half).max(0.0)
    }

    /// The refraction scale, in logical pixels: `0.75 ×` it is how far the
    /// steepest part of the edge would sample at unit gain and slope. Zero
    /// for an empty shape.
    pub fn effective_optical_depth(&self, size: Size) -> f32 {
        if Self::short_half(size) <= 0.0 {
            return 0.0;
        }

        self.optical_depth.max(0.0)
    }

    /// How far, in logical pixels, the fragment `inside` logical pixels in
    /// from the silhouette samples **inward** of itself.
    ///
    /// The rim's slope ([`rim_slope`]) times the refraction gain, the index's
    /// `1 - 1 / index` and the optical depth. Steepest at the very edge,
    /// falling to zero on the flat top. Zero whenever gain, depth or index
    /// removes refraction. The shader's twin must agree.
    pub fn displacement_at(&self, size: Size, inside: f32) -> f32 {
        let rim = self.effective_bevel_width(size);

        if rim <= 0.0 || !inside.is_finite() || inside < 0.0 || self.index_of_refraction <= 1.0 {
            return 0.0;
        }

        let depth = self.effective_optical_depth(size);
        let thickness = rim_height(inside, rim) / rim;

        rim_slope(inside, rim)
            * (1.0 - 1.0 / self.index_of_refraction)
            * self.refraction
            * depth
            * 0.75
            * (2.0 + 0.5 * thickness)
    }

    /// The supremum of every sample offset a fragment applies, in logical
    /// pixels: the inward bend, the small pull toward the centre and the
    /// chromatic fringe. The sampled source is padded by this.
    pub fn max_displacement(&self, size: Size) -> f32 {
        if Self::short_half(size) <= 0.0 {
            return 0.0;
        }

        let rim = self.effective_bevel_width(size);
        let fringe = CHROMA_PER_DISPERSION * self.dispersion.max(0.0);

        if rim <= 0.0 || self.index_of_refraction <= 1.0 {
            return fringe;
        }

        let bend = (0..=64)
            .map(|step| self.displacement_at(size, rim * step as f32 / 64.0))
            .fold(0.0_f32, f32::max);
        let pull = CENTRE_PULL * self.refraction;

        bend + pull + fringe
    }

    /// Padding, in logical pixels, the sampled source must carry beyond the
    /// region.
    ///
    /// Covers three sigma of whichever filter reaches furthest, the grazing
    /// displacement, and one pixel of bilinear support. A source clipped to
    /// the region's own rectangle would pull its edge colour inward instead.
    pub fn sample_padding(&self, size: Size) -> f32 {
        let diffusion_sigma = if self.color_diffusion > 0.0 {
            self.color_reach
        } else {
            0.0
        };

        3.0 * self.blur_sigma.max(diffusion_sigma) + self.max_displacement(size) + 1.0
    }

    /// The sigma of the second, wider filter this material needs, if any.
    ///
    /// `None` means the composite can read the blurred source for both.
    pub fn diffusion_sigma(&self) -> Option<f32> {
        (self.color_diffusion > 0.0 && self.color_reach > self.blur_sigma)
            .then_some(self.color_reach)
    }
}

/// The height, in the unit of `rim`, of a glass edge rounded with radius
/// `rim`, `inside` units in from the silhouette.
///
/// A quarter circle: zero at the silhouette, rising steeply, and flat at
/// `rim` from there in. The shader's twin must agree.
pub fn rim_height(inside: f32, rim: f32) -> f32 {
    if inside <= 0.0 || rim <= 0.0 {
        0.0
    } else if inside >= rim {
        rim
    } else {
        (inside * (2.0 * rim - inside)).sqrt()
    }
}

/// The rim's slope at `inside`, by the same central difference the shader
/// takes, `SLOPE_STEP` either side. Finite at the silhouette, where the exact
/// slope is infinite, and zero on the flat top.
pub fn rim_slope(inside: f32, rim: f32) -> f32 {
    (rim_height(inside + SLOPE_STEP, rim) - rim_height(inside - SLOPE_STEP, rim))
        / (2.0 * SLOPE_STEP)
}

/// Half the central difference the rim's slope is taken over, in logical
/// pixels.
pub const SLOPE_STEP: f32 = 2.0;

/// The largest pull toward the centre a fully refracting face applies, in
/// logical pixels.
pub const CENTRE_PULL: f32 = 4.0;

/// The chromatic fringe, in logical pixels, per unit of dispersion (the
/// library's `chromAberration × 36`).
pub const CHROMA_PER_DISPERSION: f32 = 36.0;

/// The uniform block the material composite shader reads, in **physical**
/// pixels.
///
/// Sixteen-byte aligned and laid out as eight `vec4<f32>`, mirroring `struct
/// Material` in `iced_wgpu`'s `shader/material.wgsl`. The two are one
/// contract and change together.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Uniform {
    /// Target width, target height, scale factor, reserved.
    pub surface: [f32; 4],
    /// Region centre x and y, then half width and half height.
    pub region: [f32; 4],
    /// Corner radius, bevel width, optical depth, profile curvature.
    pub shape: [f32; 4],
    /// Refraction gain, index of refraction, dispersion, blur sigma.
    pub optics: [f32; 4],
    /// Tint opacity, saturation, colour diffusion, diffusion reach.
    pub color: [f32; 4],
    /// Highlight, brightness, light angle in radians, contrast protection.
    pub finish: [f32; 4],
    /// The tint colour's red, green and blue channels, then its alpha.
    pub tint: [f32; 4],
    /// Protected inset, the sampled source's origin x and y, and whether the
    /// region must paint its opaque fallback instead of filtering.
    pub policy: [f32; 4],
}

impl Uniform {
    /// The number of floats in the block.
    pub const FLOATS: usize = 32;

    /// The block's size in bytes.
    pub const SIZE: u64 = 128;

    /// Builds the block for one region.
    ///
    /// `region` and `target` are in logical pixels and are scaled here;
    /// `source_origin` is already in physical pixels, because the backend owns
    /// where it put the sampled source. `fallback` asks the composite to fill
    /// the region's silhouette with [`Material::tint`]'s colour at full
    /// opacity — independent of that colour's own alpha — which is how a
    /// backend that could not resolve a source still paints a panel instead of
    /// a hole.
    pub fn new(
        material: &Material,
        region: Rectangle,
        target: Size,
        source_origin: Point,
        scale: f32,
        fallback: bool,
    ) -> Self {
        let size = region.size();
        let center = region.center();

        Self {
            surface: [target.width * scale, target.height * scale, scale, 0.0],
            region: [
                center.x * scale,
                center.y * scale,
                region.width * scale / 2.0,
                region.height * scale / 2.0,
            ],
            shape: [
                material.uniform_radius(size) * scale,
                material.effective_bevel_width(size) * scale,
                material.effective_optical_depth(size) * scale,
                material.profile_curvature,
            ],
            optics: [
                material.refraction,
                material.index_of_refraction,
                material.dispersion,
                material.blur_sigma * scale,
            ],
            color: [
                material.tint_opacity,
                material.saturation,
                material.color_diffusion,
                material.color_reach * scale,
            ],
            finish: [
                material.highlight,
                material.brightness,
                material.light_angle.to_radians(),
                material.contrast_protection,
            ],
            tint: [
                material.tint.r,
                material.tint.g,
                material.tint.b,
                material.tint.a,
            ],
            policy: [
                material.protected_inset * scale,
                source_origin.x,
                source_origin.y,
                if fallback { 1.0 } else { 0.0 },
            ],
        }
    }

    /// The block as the floats the backend uploads.
    pub fn floats(&self) -> [f32; Self::FLOATS] {
        let mut floats = [0.0; Self::FLOATS];
        let blocks = [
            self.surface,
            self.region,
            self.shape,
            self.optics,
            self.color,
            self.finish,
            self.tint,
            self.policy,
        ];

        for (slot, value) in floats
            .iter_mut()
            .zip(blocks.iter().flat_map(|block| block.iter()))
        {
            *slot = *value;
        }

        floats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens() -> Material {
        Material {
            radii: [20.0; 4],
            blur_sigma: 12.0,
            refraction: 1.0,
            bevel_width: 18.0,
            index_of_refraction: 1.5,
            dispersion: 0.4,
            color_diffusion: 0.35,
            color_reach: 24.0,
            optical_depth: 18.0,
            tint: Color::from_rgba(0.1, 0.1, 0.12, 0.6),
            tint_opacity: 0.18,
            saturation: 1.3,
            highlight: 0.6,
            brightness: 0.0,
            light_angle: 300.0,
            profile_curvature: 2.4,
            contrast_protection: 0.45,
            protected_inset: 6.0,
            probe: 0,
            measure_only: false,
        }
    }

    #[test]
    fn a_compact_toolbar_displaces_several_physical_pixels() {
        let size = Size::new(320.0, 44.0);
        let peak = (1..=40)
            .map(|step| lens().displacement_at(size, step as f32 * 0.25))
            .fold(0.0_f32, f32::max);

        assert!(
            peak > 3.0,
            "a 44pt toolbar must bend its backdrop measurably, got {peak}"
        );
        assert!(peak <= lens().max_displacement(size) + 1.0e-3);
    }

    #[test]
    fn the_short_half_caps_the_bevel_and_the_corner_radius_does_not() {
        // Square corners still bend light along the straight edges, and the
        // rim is capped at the short half: a 24pt control rounds to 12pt.
        let square = Material {
            radii: [0.0; 4],
            ..lens()
        };

        assert_eq!(square.effective_bevel_width(Size::new(320.0, 44.0)), 18.0);
        assert_eq!(
            square.effective_bevel_width(Size::new(320.0, 24.0)),
            12.0,
            "a 24pt control caps its rim at its short half"
        );
        assert_eq!(
            square.effective_optical_depth(Size::new(320.0, 44.0)),
            square.optical_depth,
            "the ray's travel is not the bevel's lateral width"
        );
    }

    #[test]
    fn removing_gain_removes_displacement_but_not_the_blur() {
        let size = Size::new(320.0, 44.0);
        let flat = Material {
            refraction: 0.0,
            ..lens()
        };

        assert_eq!(flat.displacement_at(size, 2.0), 0.0);
        assert_eq!(
            flat.max_displacement(size),
            CHROMA_PER_DISPERSION * flat.dispersion,
            "only the chromatic fringe still needs room"
        );
        assert!(!flat.refracts());
        assert!(
            flat.sample_padding(size) >= 3.0 * flat.blur_sigma,
            "the blur still needs its own padding"
        );
    }

    #[test]
    fn padding_covers_the_widest_filter_and_the_grazing_ray() {
        let size = Size::new(320.0, 44.0);

        assert!(
            lens().sample_padding(size) >= 3.0 * lens().color_reach + lens().max_displacement(size),
            "the diffusion reach is wider than the blur here"
        );
    }

    #[test]
    fn a_renderer_ceiling_clamps_the_blur_and_refusal_removes_the_material() {
        let clamped = lens()
            .within(Support::Optical {
                max_blur_sigma: 4.0,
            })
            .expect("an optical backend keeps the material");

        assert_eq!(clamped.blur_sigma, 4.0);
        assert_eq!(clamped.refraction, lens().refraction);
        assert_eq!(lens().within(Support::Unsupported), None);
    }

    #[test]
    fn the_uniform_block_is_eight_vectors_of_four_floats() {
        assert_eq!(std::mem::size_of::<Uniform>() as u64, Uniform::SIZE);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);

        let uniform = Uniform::new(
            &lens(),
            Rectangle::new(Point::new(10.0, 20.0), Size::new(320.0, 44.0)),
            Size::new(800.0, 600.0),
            Point::new(4.0, 8.0),
            2.0,
            false,
        );
        let floats = uniform.floats();

        assert_eq!(floats.len(), 32);
        assert_eq!(floats[0], 1600.0, "the target is scaled");
        assert_eq!(floats[4], (10.0 + 160.0) * 2.0, "the centre is scaled");
        assert_eq!(floats[15], lens().blur_sigma * 2.0, "sigma is physical");
        assert_eq!(floats[29], 4.0, "the source origin is already physical");
        assert_eq!(floats[31], 0.0, "this region is not a fallback");
    }

    #[test]
    fn an_empty_region_has_no_optics() {
        let size = Size::new(0.0, 44.0);

        assert_eq!(lens().effective_bevel_width(size), 0.0);
        assert_eq!(lens().max_displacement(size), 0.0);
        assert_eq!(lens().displacement_at(size, 1.0), 0.0);
        assert_eq!(lens().uniform_radius(size), 0.0);
    }
}
