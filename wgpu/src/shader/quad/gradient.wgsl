struct GradientVertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) @interpolate(flat) colors_1: vec4<u32>,
    @location(1) @interpolate(flat) colors_2: vec4<u32>,
    @location(2) @interpolate(flat) colors_3: vec4<u32>,
    @location(3) @interpolate(flat) colors_4: vec4<u32>,
    @location(4) @interpolate(flat) offsets: vec4<u32>,
    @location(5) direction: vec4<f32>,
    @location(6) pos: vec2<f32>,
    @location(7) scale: vec2<f32>,
    @location(8) border_color: vec4<f32>,
    @location(9) border_radius: vec4<f32>,
    @location(10) border_width: f32,
    @location(11) shadow_color: vec4<f32>,
    @location(12) shadow_offset: vec2<f32>,
    @location(13) shadow_blur_radius: f32,
    @location(14) snap: u32,
}

struct GradientVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(1) @interpolate(flat) colors_1: vec4<u32>,
    @location(2) @interpolate(flat) colors_2: vec4<u32>,
    @location(3) @interpolate(flat) colors_3: vec4<u32>,
    @location(4) @interpolate(flat) colors_4: vec4<u32>,
    @location(5) @interpolate(flat) offsets: vec4<u32>,
    @location(6) direction: vec4<f32>,
    @location(7) pos: vec2<f32>,
    @location(8) scale: vec2<f32>,
    @location(9) border_color: vec4<f32>,
    @location(10) border_radius: vec4<f32>,
    @location(11) border_width: f32,
    @location(12) shadow_color: vec4<f32>,
    @location(13) shadow_offset: vec2<f32>,
    @location(14) shadow_blur_radius: f32,
}

@vertex
fn gradient_vs_main(input: GradientVertexInput) -> GradientVertexOutput {
    var out: GradientVertexOutput;

    var bounds = shadow_expanded_bounds(input.pos, input.scale, input.shadow_offset, input.shadow_blur_radius) * globals.scale;
    var pos: vec2<f32> = bounds.xy;
    var scale: vec2<f32> = bounds.zw;

    var pos_snap = vec2<f32>(0.0, 0.0);
    var scale_snap = vec2<f32>(0.0, 0.0);

    if bool(input.snap) {
        pos_snap = round(pos + nudge) - pos;
        scale_snap = round(pos + scale + nudge) - pos - pos_snap - scale;
    }

    var min_border_radius = min(input.scale.x, input.scale.y) * 0.5;
    var border_radius: vec4<f32> = vec4<f32>(
        min(input.border_radius.x, min_border_radius),
        min(input.border_radius.y, min_border_radius),
        min(input.border_radius.z, min_border_radius),
        min(input.border_radius.w, min_border_radius)
    );

    var transform: mat4x4<f32> = mat4x4<f32>(
        vec4<f32>(scale.x + scale_snap.x + 1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, scale.y + scale_snap.y + 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(pos + pos_snap - vec2<f32>(0.5, 0.5), 0.0, 1.0)
    );

    out.position = globals.transform * transform * vec4<f32>(vertex_position(input.vertex_index), 0.0, 1.0);
    out.colors_1 = input.colors_1;
    out.colors_2 = input.colors_2;
    out.colors_3 = input.colors_3;
    out.colors_4 = input.colors_4;
    out.offsets = input.offsets;
    out.direction = input.direction * globals.scale;
    out.pos = input.pos * globals.scale + pos_snap;
    out.scale = input.scale * globals.scale + scale_snap;
    out.border_color = premultiply(input.border_color);
    out.border_radius = border_radius * globals.scale;
    out.border_width = input.border_width * globals.scale;
    out.shadow_color = premultiply(input.shadow_color);
    out.shadow_offset = input.shadow_offset * globals.scale;
    out.shadow_blur_radius = input.shadow_blur_radius * globals.scale;

    return out;
}

fn random(coords: vec2<f32>) -> f32 {
    return fract(sin(dot(coords, vec2(12.9898,78.233))) * 43758.5453);
}

/// Returns the current interpolated color with a max 8-stop gradient
fn gradient(
    raw_position: vec2<f32>,
    direction: vec4<f32>,
    colors: array<vec2<u32>, 8>,
    offsets: array<f32, 8>,
    last_index: i32
) -> vec4<f32> {
    let start = direction.xy;
    let end = direction.zw;

    let v1 = end - start;
    let v2 = raw_position - start;
    let coord_offset = dot(v1, v2) / dot(v1, v1);

    let noise_granularity: f32 = 0.3/255.0;
    let noise = mix(-noise_granularity, noise_granularity, random(raw_position));

    return stop_color(coord_offset, colors, offsets, last_index) + noise;
}

// The colour at `t` along stops sorted by offset, of which the first
// `last_index + 1` are used. Each pixel evaluates only the segment `t` falls
// in, unpacking just its two colours, with constant indices throughout:
// indexing a local array at run time puts it in scratch memory on some GPUs
// (Intel among them), and evaluating every segment for every pixel made a
// full-screen gradient cost several times a textured quad. Neighbouring
// pixels fall in the same segment, so the branches stay coherent.
fn stop_color(
    t: f32,
    colors: array<vec2<u32>, 8>,
    offsets: array<f32, 8>,
    last_index: i32
) -> vec4<f32> {
    if (last_index < 1 || t <= offsets[0]) {
        return unpack_color(colors[0]);
    }
    if (last_index < 2 || t <= offsets[1]) {
        return segment(colors[0], colors[1], offsets[0], offsets[1], t);
    }
    if (last_index < 3 || t <= offsets[2]) {
        return segment(colors[1], colors[2], offsets[1], offsets[2], t);
    }
    if (last_index < 4 || t <= offsets[3]) {
        return segment(colors[2], colors[3], offsets[2], offsets[3], t);
    }
    if (last_index < 5 || t <= offsets[4]) {
        return segment(colors[3], colors[4], offsets[3], offsets[4], t);
    }
    if (last_index < 6 || t <= offsets[5]) {
        return segment(colors[4], colors[5], offsets[4], offsets[5], t);
    }
    if (last_index < 7 || t <= offsets[6]) {
        return segment(colors[5], colors[6], offsets[5], offsets[6], t);
    }
    return segment(colors[6], colors[7], offsets[6], offsets[7], t);
}

// One segment, linearly (enzICED #78.1: not `smoothstep`, whose flat ends
// put a visible flat spot at every stop). `saturate` clamps the factor and
// guards a zero-width segment (a hard stop), where the division is 0/0.
fn segment(from_: vec2<u32>, to_: vec2<u32>, start: f32, end: f32, t: f32) -> vec4<f32> {
    return interpolate_color(
        unpack_color(from_),
        unpack_color(to_),
        saturate((t - start) / (end - start)),
    );
}

@fragment
fn gradient_fs_main(input: GradientVertexOutput) -> @location(0) vec4<f32> {
    let colors = array<vec2<u32>, 8>(
        input.colors_1.xy,
        input.colors_1.zw,
        input.colors_2.xy,
        input.colors_2.zw,
        input.colors_3.xy,
        input.colors_3.zw,
        input.colors_4.xy,
        input.colors_4.zw,
    );

    let offsets_1: vec4<f32> = unpack_u32(input.offsets.xy);
    let offsets_2: vec4<f32> = unpack_u32(input.offsets.zw);

    let offsets = array<f32, 8>(
        offsets_1.x,
        offsets_1.y,
        offsets_1.z,
        offsets_1.w,
        offsets_2.x,
        offsets_2.y,
        offsets_2.z,
        offsets_2.w,
    );

    // Unused stops carry an offset past 1; the used ones come first.
    let last_index = i32(offsets[1] <= 1.0) + i32(offsets[2] <= 1.0) + i32(offsets[3] <= 1.0)
        + i32(offsets[4] <= 1.0) + i32(offsets[5] <= 1.0) + i32(offsets[6] <= 1.0)
        + i32(offsets[7] <= 1.0);

    var mixed_color: vec4<f32> = gradient(input.position.xy, input.direction, colors, offsets, last_index);

    var dist: f32 = rounded_box_sdf(
        -(input.position.xy - input.pos - input.scale / 2.0) * 2.0,
        input.scale,
        input.border_radius * 2.0
    ) / 2.0;

    if (input.border_width > 0.0) {
        mixed_color = mix(
            mixed_color,
            input.border_color,
            clamp(0.5 + dist + input.border_width, 0.0, 1.0)
        );
    }

    var quad_alpha: f32 = clamp(0.5 - dist, 0.0, 1.0);
    let quad_color = mixed_color * quad_alpha;

    // enzICED #41 stage 2: apply the layer's rounded clip (no-op when unset).
    let clip = rounded_clip_coverage_of_layer(input.position.xy);

    if input.shadow_color.a > 0.0 {
        return mix_shadow(
            quad_color,
            quad_alpha,
            input.position.xy,
            input.pos,
            input.scale,
            input.border_radius,
            input.shadow_color,
            input.shadow_offset,
            input.shadow_blur_radius
        ) * clip;
    }

    return quad_color * clip;
}
