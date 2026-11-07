// The rounded layer clip, shared by every in-tree pipeline (enzICED #41).
//
// `set_scissor_rect` is rectangle-only in hardware, so a rounded layer clip is
// applied per-fragment here instead. The region travels in each pipeline's own
// per-layer uniform block, so this function takes it as PARAMETERS rather than
// reading a `Globals` binding -- the quad, image and mesh pipelines have three
// different uniform layouts and only the quad one could have been assumed.
//
// `clip_rect` is [x, y, w, h] and `clip_radius` is [tl, tr, br, bl], both in
// PHYSICAL pixels, matching the `@builtin(position)` the caller passes in.

// A private copy of the rounded-rect SDF. `quad.wgsl` and `image.wgsl` each
// already define a `rounded_box_sdf` and the shader sources are concatenated,
// so this one is deliberately named apart rather than colliding with (or
// depending on the include order of) either.
fn clip_rounded_box_sdf(p: vec2<f32>, size: vec2<f32>, corners: vec4<f32>) -> f32 {
    let box_half = select(corners.yz, corners.xw, p.x > 0.0);
    let corner = select(box_half.y, box_half.x, p.y > 0.0);
    let q = abs(p) - size + corner;
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - corner;
}

// Anti-aliased coverage of `frag_pos` (physical pixels) inside the layer's
// rounded clip region. Returns 1.0 when there is no rounded clip (all-zero
// radius) so the plain rectangular-scissor path is byte-for-byte unchanged.
fn rounded_clip_coverage(
    frag_pos: vec2<f32>,
    clip_rect: vec4<f32>,
    clip_radius: vec4<f32>,
) -> f32 {
    if (clip_radius.x == 0.0 && clip_radius.y == 0.0
        && clip_radius.z == 0.0 && clip_radius.w == 0.0) {
        return 1.0;
    }

    let center = clip_rect.xy + clip_rect.zw * 0.5;
    let half_size = clip_rect.zw * 0.5;
    // Clamp radii to half the box so a large radius degrades to a circle
    // rather than inverting, matching the CPU/tiny_skia clamp.
    let max_r = min(half_size.x, half_size.y);
    let radius = min(clip_radius, vec4<f32>(max_r));

    // `clip_rounded_box_sdf` selects its corner from the SIGN of `p`, and its
    // `corners` vector is [tl, tr, br, bl] indexed so that a **negated** offset
    // (`center - frag`) lands on the right one -- that is the convention every
    // quad caller uses (`solid_fs_main`, `gradient_fs_main`). Passing
    // `frag - center` flips both sign tests and point-mirrors the lookup
    // (tl<->br, tr<->bl), which is invisible under a uniform radius but wrong
    // for a per-corner one (#306).
    let dist = clip_rounded_box_sdf(center - frag_pos, half_size, radius);
    // dist < 0 inside, > 0 outside; 1px anti-aliased edge.
    return 1.0 - clamp(dist + 0.5, 0.0, 1.0);
}
