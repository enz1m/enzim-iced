struct Globals {
    transform: mat4x4<f32>,
    scale: f32,
    // enzICED #41 stage 2: rounded clip region in physical pixels.
    // clip_rect = [x, y, w, h]; clip_radius = [tl, tr, br, bl].
    // An all-zero clip_radius means "no rounded clip".
    clip_rect: vec4<f32>,
    clip_radius: vec4<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

fn rounded_box_sdf(p: vec2<f32>, size: vec2<f32>, corners: vec4<f32>) -> f32 {
    var box_half = select(corners.yz, corners.xw, p.x > 0.0);
    var corner = select(box_half.y, box_half.x, p.y > 0.0);
    var q = abs(p) - size + corner;
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - corner;
}

// enzICED #41: the layer's rounded clip, for this pipeline's `Globals`.
//
// The SDF itself lives in `shader/clip.wgsl`, shared with the image and mesh
// pipelines (stage 3); this wrapper just supplies the quad `Globals` fields so
// the call sites in `quad/solid.wgsl` and `quad/gradient.wgsl` stay unchanged.
fn rounded_clip_coverage_of_layer(frag_pos: vec2<f32>) -> f32 {
    return rounded_clip_coverage(frag_pos, globals.clip_rect, globals.clip_radius);
}
