// Sampled-backdrop material: shared separable filter, region composite, and
// the scene blit that makes the backdrop sampleable at all (enzUI-ICED #2950).
//
// The backend copies the content painted so far into `source`, filters it once
// per distinct sigma with `fs_filter`, and then composites each region with
// `fs_material`. `fs_blit` returns the offscreen scene to the swapchain.
//
// `struct Material` mirrors `iced_core::material::Uniform` exactly; the two are
// one contract and must change together. Every length in it is physical.
//
// `ENCODED` is prepended by the backend: true when the target stores
// sRGB-encoded values without hardware conversion (the fork's `web-colors`
// policy). Every optical step — blur, tint, contrast protection, highlights —
// is then done in linear light and re-encoded on the way out, so a tuned
// luminance means the same thing on both kinds of target.

struct Material {
    surface: vec4<f32>,
    region: vec4<f32>,
    shape: vec4<f32>,
    optics: vec4<f32>,
    color: vec4<f32>,
    finish: vec4<f32>,
    tint: vec4<f32>,
    policy: vec4<f32>,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var blurred_texture: texture_2d<f32>;
@group(0) @binding(2) var diffused_texture: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
@group(0) @binding(4) var<uniform> u: Material;

// Brightest linear luminance the glass keeps under light ink.
const DARK_CEILING: f32 = 0.16;
// Largest linear darkness the glass keeps under dark ink: a luminance floor
// of 0.25, where black text still reads at about 4.5:1. Higher turns light
// glass into milky frosted glass.
const LIGHT_CEILING: f32 = 0.75;

// A soft ceiling: values well under `limit` pass almost unchanged, values over
// it approach it smoothly. A gentler knee would lift or dim every tone of the
// backdrop, not only the ones that threaten legibility.
fn knee(value: f32, limit: f32) -> f32 {
    let ratio = max(value, 0.0) / max(limit, 0.00001);
    return value / pow(1.0 + pow(ratio, 4.0), 0.25);
}

// `color` with its chroma around `luma` scaled by up to `gain`, stopping
// where the first channel would leave [0, 1], so the hue never shifts.
fn vivid(color: vec3<f32>, luma: f32, gain: f32) -> vec3<f32> {
    let offset = color - vec3<f32>(luma);
    var limit = gain;
    for (var i = 0; i < 3; i++) {
        if offset[i] < -0.00001 {
            limit = min(limit, luma / -offset[i]);
        } else if offset[i] > 0.00001 {
            limit = min(limit, (1.0 - luma) / offset[i]);
        }
    }
    return clamp(vec3<f32>(luma) + offset * max(limit, 1.0), vec3<f32>(0.0), vec3<f32>(1.0));
}

struct Varying {
    @builtin(position) position: vec4<f32>,
};

// Signed distance to the region's rectangle with corner `radius`, negative
// inside.
fn rounded_sdf(p: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p - u.region.xy) - u.region.zw + radius;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

// The silhouette.
fn sdf(p: vec2<f32>) -> f32 {
    return rounded_sdf(p, u.shape.x);
}

// The lens field: the same rectangle with corners at least as round as the
// rim. Inside a rounded rectangle deeper than its corner radius the contours
// turn sharp and the rim's slope creases along the diagonals, so the rim is
// measured against this field and stays smooth through the corners.
fn lens_sdf(p: vec2<f32>, radius: f32) -> f32 {
    return rounded_sdf(p, radius);
}

// A glass edge rounded with radius `rim`: a quarter circle from the
// silhouette, flat from `rim` in. The Rust twin is
// `iced_core::material::rim_height`.
fn rim_height(inside: f32, rim: f32) -> f32 {
    if inside <= 0.0 {
        return 0.0;
    }
    if inside >= rim {
        return rim;
    }
    return sqrt(inside * (2.0 * rim - inside));
}

// `iced_core::material::{SLOPE_STEP, CENTRE_PULL, CHROMA_PER_DISPERSION}`.
const SLOPE_STEP: f32 = 2.0;
const CENTRE_PULL: f32 = 4.0;
const CHROMA_PER_DISPERSION: f32 = 36.0;

// sRGB transfer regardless of the target's encoding, for light that must add
// in perceptual rather than linear terms.
fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3<f32>(0.0031308));
}

fn srgb_decode(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(pow((v + 0.055) / 1.055, vec3<f32>(2.4)), v / 12.92, v <= vec3<f32>(0.04045));
}

fn source_uv(texel: vec2<f32>) -> vec2<f32> {
    return (texel - u.policy.yz) / vec2<f32>(textureDimensions(source_texture));
}

// A nonzero blur reads the filtered source everywhere inside the lens, rim
// included: a crisper rim than face reads as uneven frosting, not as glass.
//
// The sample is **premultiplied**, like everything else this backend renders:
// its colour already carries its own coverage. Callers un-premultiply through
// `straight` before any straight-colour maths and premultiply again on the way
// out. When the blur is below one texel of travel the backend binds the
// unfiltered source into the blurred slot, so both branches return the same
// texels and neither claims a filter that did not run.
fn filtered(texel: vec2<f32>) -> vec4<f32> {
    let uv = source_uv(texel);

    if u.optics.w < 0.001 {
        return textureSampleLevel(source_texture, linear_sampler, uv, 0.0);
    }

    return textureSampleLevel(blurred_texture, linear_sampler, uv, 0.0);
}

// The straight colour of a premultiplied sample, in linear light. A fully
// transparent sample has no colour to recover and answers black, which is what
// it already encoded.
fn straight(sample: vec4<f32>) -> vec3<f32> {
    return decode(sample.rgb / max(sample.a, 0.0001));
}

// sRGB transfer, applied only where the target does not do it in hardware.
fn decode(c: vec3<f32>) -> vec3<f32> {
    if !ENCODED {
        return c;
    }

    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(pow((v + 0.055) / 1.055, vec3<f32>(2.4)), v / 12.92, v <= vec3<f32>(0.04045));
}

fn encode(c: vec3<f32>) -> vec3<f32> {
    if !ENCODED {
        return c;
    }

    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3<f32>(0.0031308));
}

// The region's own rectangle, one pixel proud for the antialiased silhouette.
@vertex
fn vs_region(@builtin(vertex_index) index: u32) -> Varying {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );

    let half_size = u.region.zw + vec2<f32>(1.0, 1.0);
    let texel = u.region.xy + corners[index] * half_size;
    let ndc = vec2<f32>(
        texel.x / max(u.surface.x, 1.0) * 2.0 - 1.0,
        1.0 - texel.y / max(u.surface.y, 1.0) * 2.0,
    );

    return Varying(vec4<f32>(ndc, 0.0, 1.0));
}

@fragment
fn fs_material(input: Varying) -> @location(0) vec4<f32> {
    let texel = input.position.xy;
    let signed = sdf(texel);
    let mask = 1.0 - smoothstep(-0.75, 0.75, signed);

    if mask <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // An explicit opaque fallback: with no source resolved, the region paints
    // its own colour over the whole silhouette, whatever alpha the tint carries.
    if u.policy.w > 0.5 {
        return vec4<f32>(u.tint.rgb * mask, mask);
    }

    let inside = max(-signed, 0.0);
    let scale = max(u.surface.z, 0.0001);
    let short_half = min(u.region.z, u.region.w);
    // Shoutout to github.com/ybouane/liquidglass (MIT): this follows its
    // WebGL glass closely. A slab
    // whose edge is a quarter circle of radius `rim` (its `zRadius`),
    // refraction along that edge's slope, and light added in encoded values.
    // Unlike there, the rim is capped at the shape's short half: a control
    // thinner than twice its rim would otherwise reach its centre line still
    // sloping, with the slopes from opposite edges meeting there head on, and
    // the image would jump along that line. Capped, a pill is rounded all the
    // way across and flat exactly at its centre, at any size.
    // A zero bevel is a flat filter, including its lighting and centre pull.
    let flat = u.shape.y <= 0.0;
    let rim = select(max(min(u.shape.y, short_half), scale), 0.0, flat);
    // Distance from the true silhouette; direction from a field rounded at
    // least as much as the rim, so the edge does not crease at the corners.
    let d = inside;
    let lens_radius = min(max(u.shape.x, rim), short_half);
    let step = SLOPE_STEP * scale;
    let field = vec2<f32>(
        lens_sdf(texel + vec2<f32>(step, 0.0), lens_radius) - lens_sdf(texel - vec2<f32>(step, 0.0), lens_radius),
        lens_sdf(texel + vec2<f32>(0.0, step), lens_radius) - lens_sdf(texel - vec2<f32>(0.0, step), lens_radius),
    );
    let inward = -field / max(length(field), 0.00001);
    // The edge's slope (`rim_slope`'s twin), pointing inward, and the normal.
    let slope = inward * (rim_height(d + step, rim) - rim_height(d - step, rim)) / (2.0 * step);
    let surface = normalize(vec3<f32>(-slope, 1.0));
    let depth_in = select(smoothstep(0.0, max(rim, scale), d), 0.0, flat);

    // Biconvex refraction: entry, exit and the path through the slab, with
    // `optical_depth × 0.75` standing in for the library's 30 px.
    let index = u.optics.y;
    let gain = u.optics.x;
    let depth = u.shape.z;
    let thickness = rim_height(d, rim) / max(rim, scale);
    let bend = slope * max(1.0 - 1.0 / max(index, 1.0), 0.0) * gain * depth * 0.75
        * (2.0 + 0.5 * thickness);
    let to_centre = (u.region.xy - texel) / max(u.region.zw, vec2<f32>(1.0));
    let pull = to_centre * CENTRE_PULL * gain * scale * depth_in;
    let base = texel + bend + pull;

    // Chromatic aberration along the normal, strongest near the edge.
    let edge = smoothstep(short_half * 0.35, 0.0, d);
    let fringe = surface.xy * (u.optics.z * CHROMA_PER_DISPERSION * (edge * 0.7 + 0.3)) * scale;

    // The green channel's sample is the one whose coverage describes this
    // fragment's backdrop; red and blue sit either side of it.
    let center_sample = filtered(base);
    let backdrop_alpha = clamp(center_sample.a, 0.0, 1.0);
    var color = vec3<f32>(
        straight(filtered(base + fringe)).r,
        straight(center_sample).g,
        straight(filtered(base - fringe)).b,
    );
    // The library keeps up to 15% of the unfiltered image at the rim; here
    // the frost stays even from rim to face, which reads better on wide,
    // square-ended surfaces.

    // Local colour diffusion, off in the stock styles.
    if u.color.z > 0.0 {
        let spill = straight(textureSampleLevel(
            diffused_texture,
            linear_sampler,
            source_uv(base),
            0.0,
        ));
        let reach = max(rim, 2.0 * u.color.w);
        color = mix(color, spill, u.color.z * (1.0 - smoothstep(0.0, reach, inside)));
    }

    let tint = decode(u.tint.rgb);
    color = mix(color, tint, u.color.x);

    // Optional contrast protection, a smooth luminance roll-off in linear
    // light: under light ink the glass never gets brighter than
    // `DARK_CEILING`, under dark ink never darker than `1 - LIGHT_CEILING`,
    // and the chroma that darkening drains is restored without shifting hue.
    let bounded = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
    let luminance = dot(bounded, vec3<f32>(0.2126, 0.7152, 0.0722));
    // Reserve contrast for off-white theme text on flat panels.
    let dark_luma = knee(luminance, select(DARK_CEILING, 0.15, flat));
    let dark_protected = bounded * (dark_luma / max(luminance, 0.00001));
    let inverse = 1.0 - luminance;
    let light_luma = knee(inverse, LIGHT_CEILING);
    let light_protected =
        vec3<f32>(1.0) - (vec3<f32>(1.0) - bounded) * (light_luma / max(inverse, 0.00001));
    let dark_moved = 1.0 - dark_luma / max(luminance, 0.00001);
    let dark_vivid = vivid(dark_protected, dark_luma, 1.0 + 0.6 * dark_moved);
    let light_moved = 1.0 - light_luma / max(inverse, 0.00001);
    let light_vivid = vivid(light_protected, 1.0 - light_luma, 1.0 + 0.4 * light_moved);
    let tint_luminance = dot(tint, vec3<f32>(0.2126, 0.7152, 0.0722));
    // How much this is light glass (dark ink), from its tint: continuous, so
    // an ink crossfade moves the protection with it instead of snapping.
    let light_glass = smoothstep(0.35, 0.65, tint_luminance);
    let protected_color = mix(dark_vivid, light_vivid, vec3<f32>(light_glass));
    // Light glass carries dark ink, which the library never has to. Frosted
    // light glass, which hides its backdrop anyway, keeps at least 70% of the
    // floor so dark text stays readable over a dark backdrop. Clear glass
    // shows its backdrop as it is, in light mode as in dark.
    let frost = smoothstep(0.0, 3.0 * scale, u.optics.w);
    let protection = mix(u.finish.w, max(u.finish.w, 0.7 * frost), light_glass);
    color = mix(color, protected_color, protection);

    // From here on, the library's colour steps in encoded values:
    // brightness, saturation, then light.
    // Brightness darkens dark glass for light ink; light glass, which carries
    // dark ink, is left as it is.
    var lit = srgb_encode(color);
    lit = mix(lit * (1.0 + u.finish.y), lit, vec3<f32>(light_glass));
    let lum = dot(lit, vec3<f32>(0.299, 0.587, 0.114));
    lit = max(mix(vec3<f32>(lum), lit, u.color.y), vec3<f32>(0.0));
    if !flat {
        lit = lit * (1.0 + 0.06 * depth_in);

        let light = vec2<f32>(cos(u.finish.z), sin(u.finish.z));
        let fresnel = pow(1.0 - abs(surface.z), 4.0);
        let reflection = (dot(surface.xy, light) * 0.5 + 0.5) * fresnel * 0.08;
        let facing = dot(-inward, light);
        let lobe = pow(max(facing, 0.0), 2.0) + 0.45 * pow(max(-facing, 0.0), 2.0);
        let directional = 0.12 + 0.88 * lobe;
        let border = 1.5 * scale;
        let stroke = smoothstep(-border - scale, -border, signed) * (1.0 - smoothstep(-scale, 0.0, signed))
            * directional;
        let highlight = u.finish.x;
        let rim_light = edge * highlight * 0.22 * directional;
        let glow = smoothstep(5.0 * scale, 0.0, d) * highlight * 0.15 * directional;
        lit = lit + vec3<f32>(rim_light + glow + stroke * highlight * 0.55 + reflection);
        lit = mix(lit, vec3<f32>(1.0), clamp(fresnel * 0.2 * directional, 0.0, 1.0));
    }
    color = srgb_decode(lit);

    // Coverage is the region's silhouette times the coverage its backdrop
    // actually had. Multiplying the mask alone would promote a transparent part
    // of the frame into an opaque panel, and premultiplying by a coverage the
    // sample already carried would darken every translucent backdrop twice.
    //
    // A material over a *fully* transparent frame region therefore paints
    // nothing: there is no backdrop to filter there. A panel that must be
    // visible over one is an opaque style, not a lens.
    let alpha = mask * backdrop_alpha;

    return vec4<f32>(encode(color) * alpha, alpha);
}

// One fullscreen triangle; the probe pass's viewport is a single atlas texel.
@vertex
fn vs_probe(@builtin(vertex_index) index: u32) -> Varying {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: Varying;
    out.position = vec4<f32>(corners[index], 0.0, 1.0);
    return out;
}

// The backdrop's average brightness behind the region, in perceptual
// (encoded) terms: a 6 x 6 grid over the inner 80% of the filtered source,
// clear of the rim, where the refraction pulls in what lies beyond it.
@fragment
fn fs_probe(input: Varying) -> @location(0) vec4<f32> {
    var sum = 0.0;
    for (var y = 0; y < 6; y++) {
        for (var x = 0; x < 6; x++) {
            let at = (vec2<f32>(f32(x), f32(y)) + 0.5) / 6.0 * 2.0 - 1.0;
            let texel = u.region.xy + u.region.zw * at * 0.8;
            let encoded = srgb_encode(straight(filtered(texel)));
            sum += dot(encoded, vec3<f32>(0.299, 0.587, 0.114));
        }
    }
    let average = sum / 36.0;
    return vec4<f32>(average, average, average, 1.0);
}
