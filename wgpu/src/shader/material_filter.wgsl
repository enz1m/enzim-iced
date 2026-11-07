// Shared separable filter and scene blit for sampled-backdrop materials
// (enzUI-ICED #2950).
//
// Both passes read exact texels rather than sampling, so a filtered source is
// reproducible and a returned scene is the scene. `iced_wgpu`'s material state
// scissors every pass to the rectangle it actually copied.

struct Filter {
    // Axis of this pass, the Gaussian sigma, and the tap radius the backend
    // capped for it.
    direction: vec2<f32>,
    sigma: f32,
    radius: f32,
    // The rectangle the backend copied into this target *this frame*, as
    // inclusive physical texel bounds (x0, y0, x1, y1). Taps clamp to it, so a
    // pooled target's previous contents can never enter a filtered result even
    // in the padding band the composite does not read.
    bounds: vec4<f32>,
};

@group(0) @binding(0) var filter_input: texture_2d<f32>;
@group(0) @binding(1) var<uniform> filter_uniform: Filter;

struct Varying {
    @builtin(position) position: vec4<f32>,
};

// One oversized triangle; the backend's scissor bounds the work to the rect it
// actually copied, so no pass ever touches the whole frame.
@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> Varying {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );

    return Varying(vec4<f32>(corners[index], 0.0, 1.0));
}

// A tap inside the copied rectangle, clamped to it rather than to the texture:
// the texels outside it belong to no frame in particular.
fn load(texel: vec2<i32>) -> vec4<f32> {
    let limit = vec2<i32>(textureDimensions(filter_input)) - vec2<i32>(1, 1);
    let low = clamp(vec2<i32>(filter_uniform.bounds.xy), vec2<i32>(0, 0), limit);
    let high = clamp(vec2<i32>(filter_uniform.bounds.zw), low, limit);

    return textureLoad(filter_input, clamp(texel, low, high), 0);
}

// A tap anywhere in the texture, clamped only to its edge. The scene blit reads
// the whole attachment and carries no copied rectangle.
fn load_frame(texel: vec2<i32>) -> vec4<f32> {
    let limit = vec2<i32>(textureDimensions(filter_input)) - vec2<i32>(1, 1);
    return textureLoad(filter_input, clamp(texel, vec2<i32>(0, 0), limit), 0);
}

// A premultiplied tap in perceptual (sRGB-encoded) values. The filter
// averages encoded values, as CSS `backdrop-filter` and the WebGL glass this
// material follows do: averaged in linear light, the bright gaps of a busy
// backdrop bloom into a milky haze. `ENCODED` is prepended by the backend:
// true when the target already stores encoded values, which are then taken as
// they are; a linear target is encoded per tap and decoded once at the end.
fn perceptual(sample: vec4<f32>) -> vec4<f32> {
    if ENCODED || sample.a <= 0.0 {
        return sample;
    }

    let v = clamp(sample.rgb / sample.a, vec3<f32>(0.0), vec3<f32>(1.0));
    let encoded = select(1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3<f32>(0.0031308));

    return vec4<f32>(encoded * sample.a, sample.a);
}

fn stored(sample: vec4<f32>) -> vec4<f32> {
    if ENCODED || sample.a <= 0.0 {
        return sample;
    }

    let v = clamp(sample.rgb / sample.a, vec3<f32>(0.0), vec3<f32>(1.0));
    let decoded = select(pow((v + 0.055) / 1.055, vec3<f32>(2.4)), v / 12.92, v <= vec3<f32>(0.04045));

    return vec4<f32>(decoded * sample.a, sample.a);
}

// Separable Gaussian. Weights are evaluated here rather than uploaded so a
// changed sigma costs nothing but a different uniform.
@fragment
fn fs_filter(input: Varying) -> @location(0) vec4<f32> {
    let center = vec2<i32>(input.position.xy);
    let sigma = max(filter_uniform.sigma, 0.0001);
    let radius = i32(filter_uniform.radius);
    let axis = vec2<i32>(filter_uniform.direction);

    var total = perceptual(load(center));
    var weight_sum = 1.0;

    for (var i = 1; i <= radius; i = i + 1) {
        let offset = f32(i) / sigma;
        let weight = exp(-0.5 * offset * offset);
        total = total
            + weight * (perceptual(load(center + axis * i)) + perceptual(load(center - axis * i)));
        weight_sum = weight_sum + 2.0 * weight;
    }

    return stored(total / weight_sum);
}

// Exact 1:1 return of the offscreen scene; the frame's own prior content stays
// underneath because this is blended, not written. The scene is premultiplied
// and is blended premultiplied, so the returned texels need no correction.
//
// This pass shares the filter's bind group layout but not its rectangle: it
// reads the whole attachment, which is why it clamps with `load_frame` and not
// with `load`. Reading a uniform the backend never filled would otherwise pin
// the entire frame to one texel.
@fragment
fn fs_blit(input: Varying) -> @location(0) vec4<f32> {
    return load_frame(vec2<i32>(input.position.xy));
}
