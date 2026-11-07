struct Composite {
    // x = opacity, y = uniform scale, zw = scale origin in texture UV space.
    value: vec4<f32>,
    clip_rect: vec4<f32>,
    clip_radius: vec4<f32>,
    // x = rotation about the origin in radians, clockwise on screen.
    turn: vec4<f32>,
};

@group(0) @binding(0) var source_sampler: sampler;
@group(0) @binding(1) var source_texture: texture_2d<f32>;
@group(1) @binding(0) var<uniform> composite: Composite;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0, -1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let scale = max(composite.value.y, 0.000001);
    let origin = composite.value.zw;
    // Back from the composited pixel to the texel it shows: undo the turn
    // and the scale about the origin, in pixels so the turn keeps its shape.
    let size = vec2<f32>(textureDimensions(source_texture));
    let angle = composite.turn.x;
    let offset = (input.uv - origin) * size;
    let unturned = vec2<f32>(
        cos(angle) * offset.x + sin(angle) * offset.y,
        -sin(angle) * offset.x + cos(angle) * offset.y,
    );
    let source_uv = origin + unturned / scale / size;

    if (any(source_uv < vec2<f32>(0.0)) || any(source_uv > vec2<f32>(1.0))) {
        return vec4<f32>(0.0);
    }

    return textureSample(source_texture, source_sampler, source_uv)
        * composite.value.x
        * rounded_clip_coverage(input.position.xy, composite.clip_rect, composite.clip_radius);
}
