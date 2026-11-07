// Modified by enzUI contributors. See FORK.md.
//! The rounded clip region shared by every in-tree pipeline (enzICED #41).
//!
//! `set_scissor_rect` is a rectangle-only hardware primitive, so a rounded
//! layer clip cannot be expressed by the scissor. Instead the layer's rounded
//! region is uploaded with each pipeline's per-layer uniforms and applied
//! per-fragment by the shared `rounded_clip_coverage` SDF in
//! `shader/clip.wgsl`.
//!
//! Stage 2 (`e13616ed`) put this type in `quad`, because quads were the only
//! pipeline that consumed it. Stage 3 extends it to the image and mesh
//! pipelines, so it lives here rather than in any one of them.

/// A rounded clip region for a layer, in **physical** pixels.
///
/// A `radius` of all-zero (the default) means *no rounded clip*: every shader
/// takes a cheap early-out and behaves exactly as it did before, so the plain
/// rectangular-scissor path is unchanged.
///
/// `PartialEq` matters: the mesh pipeline caches uploads keyed on their
/// uniforms, so a cache entry has to be invalidated when the clip region moves
/// even though the transformation has not (see `triangle::Storage::prepare`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipRegion {
    /// `[x, y, width, height]` of the clip rectangle, physical pixels.
    pub rect: [f32; 4],
    /// Corner radii `[top_left, top_right, bottom_right, bottom_left]`,
    /// physical pixels.
    pub radius: [f32; 4],
}

impl ClipRegion {
    /// A region with no rounded clip.
    pub const NONE: Self = Self {
        rect: [0.0; 4],
        radius: [0.0; 4],
    };
}
