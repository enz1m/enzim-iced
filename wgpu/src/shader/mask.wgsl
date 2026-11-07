// Masks the window's rounded group in place (enzUI-ICED #3442).
//
// Compositing a group with coverage `m` over a backdrop `B` gives
// `m * C + (1 - m * C.a) * B` for content `C`. Drawing the content straight
// over the backdrop gives `D = C + (1 - C.a) * B`, and `m * D + (1 - m) * B`
// is the same pixel. So the frame is first multiplied by `m`, then
// `(1 - m) * B` is added, where `B` is known: the clear colour, under an
// optional solid quad of the group's own rounded shape.

struct Mask {
    clip_rect: vec4<f32>,
    clip_radius: vec4<f32>,
    // Premultiplied.
    clear: vec4<f32>,
    // Premultiplied; zero without a base quad.
    base: vec4<f32>,
    // x: how many times the base quad's coverage applies (its own rounded
    // clip can apply it a second time).
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> mask: Mask;

@vertex
fn mask_vs(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0, -1.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

fn coverage(position: vec2<f32>) -> f32 {
    return rounded_clip_coverage(position, mask.clip_rect, mask.clip_radius);
}

// The frame times `m`; blended as `dst * src`.
@fragment
fn mask_multiply_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(coverage(position.xy));
}

// `(1 - m) * B`; blended as `dst + src`.
@fragment
fn mask_backdrop_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let m = coverage(position.xy);
    let base = mask.base * pow(m, mask.params.x);
    let backdrop = base + mask.clear * (1.0 - base.a);
    return backdrop * (1.0 - m);
}
