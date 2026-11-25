//! vocara: compositing iced's own primitives cannot express, as Flutter
//! composited: a subtree drawn as one layer at an opacity (`Opacity`,
//! `FadeTransition`), and a filter of what lies behind a shape
//! (`BackdropFilter`). Both concrete renderers implement it, and so does
//! the fallback renderer that is `iced::Renderer`, through them.
use crate::core::Rectangle;

#[cfg(feature = "geometry")]
use crate::graphics::geometry::Path;

/// A filter of what lies behind a shape (Flutter's `BackdropFilter` with
/// `ImageFilter.compose(outer: ColorFilter.matrix(..), inner:
/// ImageFilter.blur(..))`): a Gaussian blur, then a color matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackdropFilter {
    /// The blur's standard deviation, in logical pixels (`sigmaX` and
    /// `sigmaY`). Beyond the window's edge the blur takes the nearest
    /// pixel (`TileMode.clamp`, what a composed filter's blur gets).
    pub blur_sigma: f32,
    /// The color matrix, 4 rows of 5 as `ColorFilter.matrix` takes it
    /// (red, green, blue and alpha, then an offset), applied to
    /// unpremultiplied colors; the offsets are in 0..=1, not 0..=255.
    pub color_matrix: [f32; 20],
}

impl BackdropFilter {
    /// The identity color matrix.
    pub const IDENTITY: [f32; 20] = [
        1.0, 0.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0, //
    ];

    /// A blur of standard deviation `sigma` alone.
    pub fn blur(sigma: f32) -> Self {
        Self {
            blur_sigma: sigma,
            color_matrix: Self::IDENTITY,
        }
    }

    /// The color matrix that scales saturation by `saturation` around the
    /// luminance weights 0.213, 0.715 and 0.072 (Cupertino's
    /// `_matrixWithSaturation`).
    pub fn saturation_matrix(saturation: f32) -> [f32; 20] {
        let r = 0.213 * (1.0 - saturation);
        let g = 0.715 * (1.0 - saturation);
        let b = 0.072 * (1.0 - saturation);

        [
            r + saturation, g, b, 0.0, 0.0, //
            r, g + saturation, b, 0.0, 0.0, //
            r, g, b + saturation, 0.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, 0.0, //
        ]
    }
}

/// A renderer that composites a subtree at an opacity and filters a
/// backdrop.
pub trait Effects {
    /// Starts a group drawn offscreen and, when
    /// [`end_opacity_layer`](Self::end_opacity_layer) ends it, composited
    /// as one layer at `opacity` inside `bounds`: where parts of it
    /// overlap, the topmost shows, at `opacity`, not a blend of each part
    /// at `opacity`. Text, images, quads, meshes and canvases inside it all
    /// go into the group; clips and transformations go on working, and the
    /// clip of the layer it is started in clips it too. Like a layer, it is
    /// drawn over what was drawn before it.
    fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32);

    /// Ends the group [`start_opacity_layer`](Self::start_opacity_layer)
    /// started.
    fn end_opacity_layer(&mut self);

    /// Draws what `draw` draws as one layer at `opacity` inside `bounds`
    /// (see [`start_opacity_layer`](Self::start_opacity_layer)).
    fn with_opacity_layer(
        &mut self,
        bounds: Rectangle,
        opacity: f32,
        draw: impl FnOnce(&mut Self),
    ) where
        Self: Sized,
    {
        self.start_opacity_layer(bounds, opacity);
        draw(self);
        self.end_opacity_layer();
    }

    /// Filters what was drawn before under `clip`, a hard-edged clip (the
    /// pixels whose centers it holds): the filtered backdrop is drawn over
    /// the backdrop (source over), as Flutter's `BackdropFilter` inside a
    /// `Clip.hardEdge` drew it. It is a layer of its own, so what goes
    /// over it must be drawn in a later layer.
    #[cfg(feature = "geometry")]
    fn draw_backdrop_filter(&mut self, clip: &Path, filter: BackdropFilter);
}

#[cfg(feature = "wgpu-bare")]
impl Effects for iced_wgpu::Renderer {
    fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32) {
        iced_wgpu::Renderer::start_opacity_layer(self, bounds, opacity);
    }

    fn end_opacity_layer(&mut self) {
        iced_wgpu::Renderer::end_opacity_layer(self);
    }

    #[cfg(feature = "geometry")]
    fn draw_backdrop_filter(&mut self, clip: &Path, filter: BackdropFilter) {
        iced_wgpu::Renderer::draw_backdrop_filter(
            self,
            clip,
            filter.blur_sigma,
            filter.color_matrix,
        );
    }
}

#[cfg(feature = "tiny-skia")]
impl Effects for iced_tiny_skia::Renderer {
    fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32) {
        iced_tiny_skia::Renderer::start_opacity_layer(self, bounds, opacity);
    }

    fn end_opacity_layer(&mut self) {
        iced_tiny_skia::Renderer::end_opacity_layer(self);
    }

    #[cfg(feature = "geometry")]
    fn draw_backdrop_filter(&mut self, clip: &Path, filter: BackdropFilter) {
        iced_tiny_skia::Renderer::draw_backdrop_filter(
            self,
            clip,
            filter.blur_sigma,
            filter.color_matrix,
        );
    }
}

impl<A, B> Effects for crate::fallback::Renderer<A, B>
where
    A: Effects,
    B: Effects,
{
    fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32) {
        match self {
            Self::Primary(renderer) => {
                renderer.start_opacity_layer(bounds, opacity);
            }
            Self::Secondary(renderer) => {
                renderer.start_opacity_layer(bounds, opacity);
            }
        }
    }

    fn end_opacity_layer(&mut self) {
        match self {
            Self::Primary(renderer) => renderer.end_opacity_layer(),
            Self::Secondary(renderer) => renderer.end_opacity_layer(),
        }
    }

    #[cfg(feature = "geometry")]
    fn draw_backdrop_filter(&mut self, clip: &Path, filter: BackdropFilter) {
        match self {
            Self::Primary(renderer) => {
                renderer.draw_backdrop_filter(clip, filter);
            }
            Self::Secondary(renderer) => {
                renderer.draw_backdrop_filter(clip, filter);
            }
        }
    }
}
