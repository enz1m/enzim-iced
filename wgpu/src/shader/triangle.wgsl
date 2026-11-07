struct Globals {
    transform: mat4x4<f32>,
    // enzICED #41 stage 3: the layer's rounded clip region in physical pixels.
    // clip_rect = [x, y, w, h]; clip_radius = [tl, tr, br, bl].
    // An all-zero clip_radius means "no rounded clip".
    clip_rect: vec4<f32>,
    clip_radius: vec4<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

// enzICED #41 stage 3: the layer's rounded clip, for this pipeline's `Globals`.
// The SDF itself lives in `shader/clip.wgsl`, shared with the quad and image
// pipelines.
fn rounded_clip_coverage_of_layer(frag_pos: vec2<f32>) -> f32 {
    return rounded_clip_coverage(frag_pos, globals.clip_rect, globals.clip_radius);
}
