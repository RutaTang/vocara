//! vocara: the effects' layers (`layer::Effect`): a subtree composited at
//! an opacity, and a filter of what lies under a shape.
//!
//! Both are computed here pixel by pixel, in the same float arithmetic as
//! the wgpu renderer's shaders (`iced_wgpu`'s `effect` module), so that the
//! two renderers draw the same 8-bit values: colors are premultiplied,
//! sRGB-encoded (blended as iced blends them, `web-colors`), and each
//! result is rounded once, to the nearest 8-bit value.
use crate::core::{Rectangle, Transformation};
use crate::layer::Backdrop;

/// The longest blur radius, in physical pixels (the wgpu renderer's
/// uniform holds this many weights).
pub const MAX_BLUR_RADIUS: usize = 255;

/// The weights of a Gaussian blur of standard deviation `sigma` (physical
/// pixels): the center's, then each further pixel's on one side, out to
/// three standard deviations (at most [`MAX_BLUR_RADIUS`]), summing to one
/// over both sides. The wgpu renderer computes the same.
pub fn blur_kernel(sigma: f32) -> Vec<f32> {
    if sigma.is_nan() || sigma <= 0.0 {
        return vec![1.0];
    }

    let radius = ((3.0 * sigma).ceil() as usize).min(MAX_BLUR_RADIUS);
    let mut weights: Vec<f32> = (0..=radius)
        .map(|i| {
            let i = i as f32;
            (-(i * i) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum = weights[0] + 2.0 * weights[1..].iter().sum::<f32>();

    for weight in &mut weights {
        *weight /= sum;
    }

    weights
}

/// The pixels a physical rectangle covers in a `width` × `height` pixmap,
/// as `(x0, y0, x1, y1)` (end exclusive): those its edges, rounded to the
/// nearest pixel boundary, enclose (as wgpu's scissor snaps it).
pub fn pixel_range(
    bounds: Rectangle,
    width: u32,
    height: u32,
) -> (usize, usize, usize, usize) {
    let snap = |v: f32, max: u32| v.round().clamp(0.0, max as f32) as usize;

    (
        snap(bounds.x, width),
        snap(bounds.y, height),
        snap(bounds.x + bounds.width, width),
        snap(bounds.y + bounds.height, height),
    )
}

/// Draws `layer` over `pixels` at `opacity` (premultiplied source over)
/// inside `bounds` (physical).
pub fn composite(
    pixels: &mut tiny_skia::PixmapMut<'_>,
    layer: &tiny_skia::Pixmap,
    opacity: f32,
    bounds: Rectangle,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    let width = pixels.width() as usize;
    let (x0, y0, x1, y1) = pixel_range(bounds, pixels.width(), pixels.height());
    let source = layer.data();
    let target = pixels.data_mut();

    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * width + x) * 4;

            if source[i + 3] == 0 {
                continue;
            }

            let alpha = f32::from(source[i + 3]) / 255.0 * opacity;

            for c in 0..4 {
                let s = f32::from(source[i + c]) / 255.0 * opacity;
                let d = f32::from(target[i + c]) / 255.0;

                target[i + c] = to_u8(s + d * (1.0 - alpha));
            }
        }
    }
}

/// Filters what `pixels` hold under `backdrop`'s clip, inside `bounds`
/// (physical): blurs it (each pixel beyond the pixmap's edge taken as the
/// nearest one inside, Flutter's `TileMode.clamp`), applies the color
/// matrix, and draws the result over what was there (source over), on
/// the pixels whose centers the clip holds (a hard edge).
pub fn draw_backdrop(
    backdrop: &Backdrop,
    scale_factor: f32,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    bounds: Rectangle,
) {
    let transformation =
        Transformation::scale(scale_factor) * backdrop.transformation;
    let Some(clip) = backdrop
        .clip
        .clone()
        .transform(crate::engine::into_transform(transformation))
    else {
        return;
    };

    let (width, height) = (pixels.width(), pixels.height());
    let (cx0, cy0, cx1, cy1) = pixel_range(bounds, width, height);
    let shape = clip.bounds();
    let x0 = (shape.left().floor().max(0.0) as usize).max(cx0);
    let y0 = (shape.top().floor().max(0.0) as usize).max(cy0);
    let x1 = (shape.right().ceil().max(0.0) as usize).min(cx1);
    let y1 = (shape.bottom().ceil().max(0.0) as usize).min(cy1);

    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let (w, h) = (x1 - x0, y1 - y0);
    let Some(mut mask) = tiny_skia::Mask::new(w as u32, h as u32) else {
        return;
    };

    mask.fill_path(
        &clip,
        tiny_skia::FillRule::Winding,
        false,
        tiny_skia::Transform::from_translate(-(x0 as f32), -(y0 as f32)),
    );

    let kernel =
        blur_kernel(backdrop.blur_sigma * transformation.scale_factor());
    let radius = kernel.len() - 1;
    let stride = width as usize;
    let last_x = stride - 1;
    let last_y = height as usize - 1;

    // The rows the vertical pass reads.
    let ry0 = y0.saturating_sub(radius);
    let ry1 = (y1 + radius).min(height as usize);

    // The horizontal pass: each pixel of those rows in the clip's columns.
    let source = pixels.as_ref();
    let data = source.data();
    let mut horizontal = vec![[0.0_f32; 4]; w * (ry1 - ry0)];
    let mut row = vec![[0.0_f32; 4]; w + 2 * radius];

    for y in ry0..ry1 {
        for (j, texel) in row.iter_mut().enumerate() {
            let x = (x0 + j).saturating_sub(radius).min(last_x);
            let i = (y * stride + x) * 4;

            for c in 0..4 {
                texel[c] = f32::from(data[i + c]) / 255.0;
            }
        }

        for x in 0..w {
            let mut sum = [0.0_f32; 4];

            for (j, texel) in row[x..=x + 2 * radius].iter().enumerate() {
                let weight = kernel[j.abs_diff(radius)];

                for c in 0..4 {
                    sum[c] += weight * texel[c];
                }
            }

            horizontal[(y - ry0) * w + x] = sum;
        }
    }

    // The vertical pass, the color matrix and the draw.
    let target = pixels.data_mut();

    for y in y0..y1 {
        for x in 0..w {
            if mask.data()[(y - y0) * w + x] == 0 {
                continue;
            }

            let mut sum = [0.0_f32; 4];

            for k in 0..=2 * radius {
                let weight = kernel[k.abs_diff(radius)];
                let sy = (y + k).saturating_sub(radius).min(last_y);
                let texel = horizontal[(sy - ry0) * w + x];

                for c in 0..4 {
                    sum[c] += weight * texel[c];
                }
            }

            // iced stores a pixel's blue first and its red third.
            let [b, g, r, a] = sum;
            let [r, g, b, a] = filter([r, g, b, a], &backdrop.color_matrix);
            let filtered = [b, g, r, a];

            let i = (y * stride + x0 + x) * 4;

            for c in 0..4 {
                let d = f32::from(target[i + c]) / 255.0;

                target[i + c] = to_u8(filtered[c] + d * (1.0 - a));
            }
        }
    }
}

/// `color` (premultiplied red, green, blue, alpha) through `matrix`, which
/// takes it unpremultiplied; premultiplied again.
fn filter(color: [f32; 4], matrix: &[f32; 20]) -> [f32; 4] {
    let [r, g, b, a] = color;
    let (r, g, b) = if a > 0.0 {
        (r / a, g / a, b / a)
    } else {
        (0.0, 0.0, 0.0)
    };
    let row = |n: usize| {
        let m = &matrix[n * 5..n * 5 + 5];

        (m[0] * r + m[1] * g + m[2] * b + m[3] * a + m[4]).clamp(0.0, 1.0)
    };
    let a = row(3);

    [row(0) * a, row(1) * a, row(2) * a, a]
}

/// A channel in 0..=1 as the nearest 8-bit value.
fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
