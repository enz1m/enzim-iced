struct GradientVertexInput {
    @location(0) v_pos: vec2<f32>,
    @location(1) @interpolate(flat) colors_1: vec4<u32>,
    @location(2) @interpolate(flat) colors_2: vec4<u32>,
    @location(3) @interpolate(flat) colors_3: vec4<u32>,
    @location(4) @interpolate(flat) colors_4: vec4<u32>,
    @location(5) @interpolate(flat) offsets: vec4<u32>,
    @location(6) direction: vec4<f32>,
}

struct GradientVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) raw_position: vec2<f32>,
    @location(1) @interpolate(flat) colors_1: vec4<u32>,
    @location(2) @interpolate(flat) colors_2: vec4<u32>,
    @location(3) @interpolate(flat) colors_3: vec4<u32>,
    @location(4) @interpolate(flat) colors_4: vec4<u32>,
    @location(5) @interpolate(flat) offsets: vec4<u32>,
    @location(6) direction: vec4<f32>,
}

@vertex
fn gradient_vs_main(input: GradientVertexInput) -> GradientVertexOutput {
    var output: GradientVertexOutput;

    output.position = globals.transform * vec4<f32>(input.v_pos, 0.0, 1.0);
    output.raw_position = input.v_pos;
    output.colors_1 = input.colors_1;
    output.colors_2 = input.colors_2;
    output.colors_3 = input.colors_3;
    output.colors_4 = input.colors_4;
    output.offsets = input.offsets;
    output.direction = input.direction;

    return output;
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
    let unit = normalize(v1);
    let coord_offset = dot(unit, v2) / length(v1);

    let noise_granularity: f32 = 0.3/255.0;

    return stop_color(coord_offset, colors, offsets, last_index)
        + mix(-noise_granularity, noise_granularity, random(raw_position));
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

    // The used stops end at the first whose offset reaches 1.
    let last_index = min(
        min(min(select(7, 0, offsets[0] >= 1.0), select(7, 1, offsets[1] >= 1.0)),
            min(select(7, 2, offsets[2] >= 1.0), select(7, 3, offsets[3] >= 1.0))),
        min(min(select(7, 4, offsets[4] >= 1.0), select(7, 5, offsets[5] >= 1.0)),
            select(7, 6, offsets[6] >= 1.0))
    );

    // enzICED #41 stage 3: apply the layer's rounded clip (no-op when unset).
    return gradient(input.raw_position, input.direction, colors, offsets, last_index)
        * rounded_clip_coverage_of_layer(input.position.xy);
}

fn random(coords: vec2<f32>) -> f32 {
    return fract(sin(dot(coords, vec2(12.9898,78.233))) * 43758.5453);
}
