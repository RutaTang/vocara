// vocara: the effects' layers (`effect.rs`): a group composited at an
// opacity, a clip layer's backdrop put back outside its clip, and a
// backdrop blurred (two passes) and color-filtered inside its clip. The
// arithmetic is iced_tiny_skia's `effect` module's, step for step, so the
// two renderers draw the same 8-bit values; a clip's pixels are
// iced_tiny_skia's own (`HardClip::mask`), uploaded as `mask`.

struct Effect {
    // The target pixel of this pass's output (0, 0), or of the saved
    // backdrop's.
    origin: vec2<i32>,
    // The target's size: what the blur clamps to.
    size: vec2<i32>,
    // The target pixel of the source texture's (0, 0).
    source: vec2<i32>,
    // The target pixel of the clip's mask's (0, 0).
    mask: vec2<i32>,
    // How far the blur reaches on each side.
    radius: i32,
    // A group's opacity.
    opacity: f32,
    // The color matrix's rows (red, green, blue, alpha).
    rows: array<vec4<f32>, 4>,
    // The color matrix's offsets.
    offset: vec4<f32>,
    // The blur's weights, the center's first, four to an element.
    weights: array<vec4<f32>, 64>,
}

@group(0) @binding(0) var<uniform> effect: Effect;
@group(0) @binding(1) var source: texture_2d<f32>;
// A clip's mask: 1 where it holds the pixel, 0 elsewhere.
@group(0) @binding(2) var mask: texture_2d<f32>;

// A triangle over the whole target.
@vertex
fn cover(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));

    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

// Whether the clip holds the target pixel `pixel`.
fn clipped(pixel: vec2<i32>) -> bool {
    return textureLoad(mask, pixel - effect.mask, 0).r > 0.0;
}

// A group's pixel at its opacity (premultiplied).
@fragment
fn composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(source, vec2<i32>(floor(position.xy)), 0);

    return texel * effect.opacity;
}

// What a clip layer's pixel held before it, where the clip does not hold
// it (drawn with no blending); nothing where it does.
@fragment
fn restore(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));

    if clipped(pixel) {
        discard;
    }

    return textureLoad(source, pixel - effect.origin, 0);
}

fn weight(offset: i32) -> f32 {
    let i = abs(offset);

    return effect.weights[i / 4][i % 4];
}

// The blur along each row of what the target held.
@fragment
fn horizontal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy)) + effect.origin;
    var sum = vec4<f32>(0.0);

    for (var i = -effect.radius; i <= effect.radius; i++) {
        let x = clamp(pixel.x + i, 0, effect.size.x - 1);

        sum += weight(i) * textureLoad(source, vec2<i32>(x, pixel.y) - effect.source, 0);
    }

    return sum;
}

fn row(n: i32, color: vec4<f32>) -> f32 {
    let m = effect.rows[n];

    return clamp(
        m.x * color.r + m.y * color.g + m.z * color.b + m.w * color.a + effect.offset[n],
        0.0,
        1.0,
    );
}

// The blur down each column of the horizontal pass, through the color
// matrix (on unpremultiplied colors), premultiplied again, on the pixels
// the clip holds.
@fragment
fn vertical(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));

    if !clipped(pixel) {
        discard;
    }

    var sum = vec4<f32>(0.0);

    for (var i = -effect.radius; i <= effect.radius; i++) {
        let y = clamp(pixel.y + i, 0, effect.size.y - 1);

        sum += weight(i) * textureLoad(source, vec2<i32>(pixel.x, y) - effect.origin, 0);
    }

    var color = vec4<f32>(0.0, 0.0, 0.0, sum.a);

    if sum.a > 0.0 {
        color = vec4<f32>(sum.rgb / sum.a, sum.a);
    }

    let a = row(3, color);

    return vec4<f32>(row(0, color) * a, row(1, color) * a, row(2, color) * a, a);
}
