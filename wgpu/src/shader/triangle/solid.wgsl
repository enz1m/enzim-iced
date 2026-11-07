struct SolidVertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
}

struct SolidVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn solid_vs_main(input: SolidVertexInput) -> SolidVertexOutput {
    var out: SolidVertexOutput;

    out.color = premultiply(input.color);
    out.position = globals.transform * vec4<f32>(input.position, 0.0, 1.0);

    return out;
}

@fragment
fn solid_fs_main(input: SolidVertexOutput) -> @location(0) vec4<f32> {
    // enzICED #41 stage 3: apply the layer's rounded clip (no-op when unset).
    // `input.color` is already premultiplied, so scaling the whole vec4 by the
    // coverage keeps it premultiplied.
    return input.color * rounded_clip_coverage_of_layer(input.position.xy);
}
