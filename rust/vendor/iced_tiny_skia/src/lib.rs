#![allow(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod window;

mod effect; // vocara
mod engine;
mod layer;
mod primitive;
mod settings;
mod text;

#[cfg(feature = "image")]
mod raster;

#[cfg(feature = "svg")]
mod vector;

#[cfg(feature = "geometry")]
pub mod geometry;

use iced_debug as debug;
pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use layer::Layer;
pub use layer::{Backdrop, Effect, Group}; // vocara
pub use primitive::Primitive;
pub use settings::Settings;

#[cfg(feature = "geometry")]
pub use geometry::Geometry;

use crate::core::renderer;
use crate::core::{
    Background, Color, Font, Pixels, Point, Rectangle, Size, Transformation,
};
use crate::engine::Engine;
use crate::graphics::Viewport;
use crate::graphics::compositor;
use crate::graphics::text::{Editor, Paragraph};

/// A [`tiny-skia`] graphics renderer for [`iced`].
///
/// [`tiny-skia`]: https://github.com/RazrFalcon/tiny-skia
/// [`iced`]: https://github.com/iced-rs/iced
#[derive(Debug)]
pub struct Renderer {
    default_font: Font,
    default_text_size: Pixels,
    layers: layer::Stack,
    engine: Engine, // TODO: Shared engine
    // vocara: the opacity layers drawing, innermost last: each holds the
    // stack it interrupted (`layers` is the group's own meanwhile).
    groups: Vec<OpenGroup>,
}

/// vocara: an opacity layer being drawn.
#[derive(Debug)]
struct OpenGroup {
    /// The stack the group's layer goes into when it ends.
    parent: layer::Stack,
    /// The group's clip, transformed and inside the layer it is in.
    bounds: Rectangle,
    /// The opacity it is composited at.
    opacity: f32,
}

impl Renderer {
    pub fn new(default_font: Font, default_text_size: Pixels) -> Self {
        Self {
            default_font,
            default_text_size,
            layers: layer::Stack::new(),
            engine: Engine::new(),
            groups: Vec::new(),
        }
    }

    /// vocara: starts a group that is drawn offscreen and, when
    /// [`end_opacity_layer`](Self::end_opacity_layer) ends it, composited
    /// as one layer at `opacity` inside `bounds` (Flutter's `Opacity`):
    /// what overlaps inside it shows the topmost part, not a blend of
    /// translucent parts. Clips and transformations go on working inside,
    /// and the clip of the layer it is drawn in clips it too.
    pub fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32) {
        let (layer, transformation) = self.layers.current_mut();
        let bounds = (bounds * transformation)
            .intersection(&layer.bounds)
            .unwrap_or_default();
        let mut group = layer::Stack::new();

        group.reset(bounds);
        group.push_transformation(transformation);

        let parent = std::mem::replace(&mut self.layers, group);

        self.groups.push(OpenGroup {
            parent,
            bounds,
            opacity,
        });
    }

    /// vocara: ends the group [`start_opacity_layer`] started; it is
    /// composited above what was drawn before it, like a layer started
    /// there.
    ///
    /// [`start_opacity_layer`]: Self::start_opacity_layer
    pub fn end_opacity_layer(&mut self) {
        let Some(open) = self.groups.pop() else {
            return;
        };

        let mut group = std::mem::replace(&mut self.layers, open.parent);
        group.flush();

        self.layers.push_clip(Rectangle::default());
        self.layers.current_mut().0.bounds = open.bounds;
        self.layers.current_mut().0.effect =
            Some(layer::Effect::Group(std::sync::Arc::new(layer::Group {
                opacity: open.opacity,
                layers: group,
            })));
        self.layers.pop_clip();
    }

    /// vocara: filters what was drawn before under `clip` (Flutter's
    /// `BackdropFilter`, in a `Clip.hardEdge`): a Gaussian blur of standard
    /// deviation `blur_sigma` (logical pixels; beyond the window's edge the
    /// nearest pixel), then `color_matrix` (4 rows of 5, applied to
    /// unpremultiplied colors, the offsets in 0..=1), drawn over it on the
    /// pixels whose centers `clip` holds. It is a layer of its own: what is
    /// drawn over it must be drawn in a later layer.
    #[cfg(feature = "geometry")]
    pub fn draw_backdrop_filter(
        &mut self,
        clip: &graphics::geometry::Path,
        blur_sigma: f32,
        color_matrix: [f32; 20],
    ) {
        let Some(path) = geometry::convert_path(clip) else {
            return;
        };

        let shape = path.bounds();
        let (layer, transformation) = self.layers.current_mut();
        let bounds = (Rectangle {
            x: shape.x(),
            y: shape.y(),
            width: shape.width(),
            height: shape.height(),
        } * transformation)
            .intersection(&layer.bounds)
            .unwrap_or_default();

        self.layers.push_clip(Rectangle::default());
        self.layers.current_mut().0.bounds = bounds;
        self.layers.current_mut().0.effect =
            Some(layer::Effect::Backdrop(std::sync::Arc::new(layer::Backdrop {
                clip: path,
                transformation,
                blur_sigma,
                color_matrix,
            })));
        self.layers.pop_clip();
    }

    pub fn layers(&mut self) -> &[Layer] {
        self.layers.flush();
        self.layers.as_slice()
    }

    pub fn draw(
        &mut self,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut tiny_skia::Mask,
        viewport: &Viewport,
        damage: &[Rectangle],
        background_color: Color,
    ) {
        let scale_factor = viewport.scale_factor();

        self.layers.flush();

        for &damage_bounds in damage {
            let damage_bounds = damage_bounds * scale_factor;

            let path = tiny_skia::PathBuilder::from_rect(
                tiny_skia::Rect::from_xywh(
                    damage_bounds.x,
                    damage_bounds.y,
                    damage_bounds.width,
                    damage_bounds.height,
                )
                .expect("Create damage rectangle"),
            );

            pixels.fill_path(
                &path,
                &tiny_skia::Paint {
                    shader: tiny_skia::Shader::SolidColor(engine::into_color(
                        background_color,
                    )),
                    anti_alias: false,
                    blend_mode: tiny_skia::BlendMode::Source,
                    ..Default::default()
                },
                tiny_skia::FillRule::default(),
                tiny_skia::Transform::identity(),
                None,
            );

            // vocara: the layers are drawn by `draw_layers` (upstream's
            // loop), which composites the effects' layers too.
            draw_layers(
                &mut self.engine,
                self.layers.as_slice(),
                scale_factor,
                pixels,
                clip_mask,
                damage_bounds,
            );
        }

        self.engine.trim();
    }
}

/// vocara: draws `layers` inside `clip_bounds` (physical), compositing the
/// effects' layers: upstream's drawing loop, moved out of `Renderer::draw`
/// so that a group's layers can be drawn offscreen with it.
fn draw_layers(
    engine: &mut Engine,
    layers: &[Layer],
    scale_factor: f32,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    clip_mask: &mut tiny_skia::Mask,
    clip_bounds: Rectangle,
) {
    for layer in layers {
        let Some(layer_bounds) =
            clip_bounds.intersection(&(layer.bounds * scale_factor))
        else {
            continue;
        };

        match &layer.effect {
            Some(layer::Effect::Group(group)) => {
                // Drawn offscreen, as at the top level, then composited.
                let (width, height) = (pixels.width(), pixels.height());

                if let (Some(mut offscreen), Some(mut mask)) = (
                    tiny_skia::Pixmap::new(width, height),
                    tiny_skia::Mask::new(width, height),
                ) {
                    draw_layers(
                        engine,
                        group.layers.as_slice(),
                        scale_factor,
                        &mut offscreen.as_mut(),
                        &mut mask,
                        layer_bounds,
                    );

                    effect::composite(
                        pixels,
                        &offscreen,
                        group.opacity,
                        layer_bounds,
                    );
                }

                continue;
            }
            Some(layer::Effect::Backdrop(backdrop)) => {
                effect::draw_backdrop(
                    backdrop,
                    scale_factor,
                    pixels,
                    layer_bounds,
                );

                continue;
            }
            None => {}
        }

        engine::adjust_clip_mask(clip_mask, layer_bounds);

        if !layer.quads.is_empty() {
            let render_span = debug::render(debug::Primitive::Quad);
            for (quad, background) in &layer.quads {
                engine.draw_quad(
                    quad,
                    background,
                    Transformation::scale(scale_factor),
                    pixels,
                    clip_mask,
                    layer_bounds,
                );
            }
            render_span.finish();
        }

        if !layer.primitives.is_empty() {
            let render_span = debug::render(debug::Primitive::Triangle);

            for group in &layer.primitives {
                let Some(group_bounds) = (group.clip_bounds()
                    * scale_factor)
                    .intersection(&layer_bounds)
                else {
                    continue;
                };

                engine::adjust_clip_mask(clip_mask, group_bounds);

                for primitive in group.as_slice() {
                    engine.draw_primitive(
                        primitive,
                        Transformation::scale(scale_factor)
                            * group.transformation(),
                        pixels,
                        clip_mask,
                        group_bounds,
                    );
                }

                engine::adjust_clip_mask(clip_mask, layer_bounds);
            }

            render_span.finish();
        }

        if !layer.images.is_empty() {
            let render_span = debug::render(debug::Primitive::Image);

            for image in &layer.images {
                engine.draw_image(
                    image,
                    Transformation::scale(scale_factor),
                    pixels,
                    clip_mask,
                    layer_bounds,
                );
            }

            render_span.finish();
        }

        if !layer.text.is_empty() {
            let render_span = debug::render(debug::Primitive::Image);

            for group in &layer.text {
                for text in group.as_slice() {
                    engine.draw_text(
                        text,
                        Transformation::scale(scale_factor)
                            * group.transformation(),
                        pixels,
                        clip_mask,
                        layer_bounds,
                    );
                }
            }

            render_span.finish();
        }
    }
}

impl core::Renderer for Renderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push_clip(bounds);
    }

    fn end_layer(&mut self) {
        self.layers.pop_clip();
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        self.layers.push_transformation(transformation);
    }

    fn end_transformation(&mut self) {
        self.layers.pop_transformation();
    }

    fn fill_quad(
        &mut self,
        quad: renderer::Quad,
        background: impl Into<Background>,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_quad(quad, background.into(), transformation);
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        // vocara: a group left open is dropped with what it drew.
        if let Some(open) = self.groups.drain(..).next() {
            self.layers = open.parent;
        }

        self.layers.reset(new_bounds);
    }

    fn allocate_image(
        &mut self,
        _handle: &core::image::Handle,
        callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>)
        + Send
        + 'static,
    ) {
        #[cfg(feature = "image")]
        #[allow(unsafe_code)]
        // TODO: Concurrency
        callback(self.engine.raster_pipeline.load(_handle));

        #[cfg(not(feature = "image"))]
        callback(Err(core::image::Error::Unsupported));
    }
}

impl core::text::Renderer for Renderer {
    type Font = Font;
    type Paragraph = Paragraph;
    type Editor = Editor;

    const ICON_FONT: Font = Font::with_name("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn default_font(&self) -> Self::Font {
        self.default_font
    }

    fn default_size(&self) -> Pixels {
        self.default_text_size
    }

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();

        layer.draw_paragraph(
            text,
            position,
            color,
            clip_bounds,
            transformation,
        );
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_editor(editor, position, color, clip_bounds, transformation);
    }

    fn fill_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text(text, position, color, clip_bounds, transformation);
    }
}

impl graphics::text::Renderer for Renderer {
    fn fill_raw(&mut self, raw: graphics::text::Raw) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text_raw(raw, transformation);
    }
}

#[cfg(feature = "geometry")]
impl graphics::geometry::Renderer for Renderer {
    type Geometry = Geometry;
    type Frame = geometry::Frame;

    fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
        geometry::Frame::new(bounds)
    }

    fn draw_geometry(&mut self, geometry: Self::Geometry) {
        let (layer, transformation) = self.layers.current_mut();

        match geometry {
            Geometry::Live {
                primitives,
                images,
                text,
                clip_bounds,
            } => {
                layer.draw_primitive_group(
                    primitives,
                    clip_bounds,
                    transformation,
                );

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, clip_bounds, transformation);
            }
            Geometry::Cache(cache) => {
                layer.draw_primitive_cache(
                    cache.primitives,
                    cache.clip_bounds,
                    transformation,
                );

                for image in cache.images.iter() {
                    layer.draw_image(image.clone(), transformation);
                }

                layer.draw_text_cache(
                    cache.text,
                    cache.clip_bounds,
                    transformation,
                );
            }
        }
    }
}

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, _mesh: graphics::Mesh) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }

    fn draw_mesh_cache(&mut self, _cache: iced_graphics::mesh::Cache) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }
}

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.engine.raster_pipeline.load(handle)
    }

    fn measure_image(
        &self,
        handle: &Self::Handle,
    ) -> Option<crate::core::Size<u32>> {
        self.engine.raster_pipeline.dimensions(handle)
    }

    fn draw_image(
        &mut self,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_raster(image, bounds, clip_bounds, transformation);
    }
}

#[cfg(feature = "svg")]
impl core::svg::Renderer for Renderer {
    fn measure_svg(
        &self,
        handle: &core::svg::Handle,
    ) -> crate::core::Size<u32> {
        self.engine.vector_pipeline.viewport_dimensions(handle)
    }

    fn draw_svg(
        &mut self,
        svg: core::Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_svg(svg, bounds, clip_bounds, transformation);
    }
}

impl compositor::Default for Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(
        default_font: Font,
        default_text_size: Pixels,
        backend: Option<&str>,
    ) -> Option<Self> {
        if backend.is_some_and(|backend| {
            !["tiny-skia", "tiny_skia"].contains(&backend)
        }) {
            return None;
        }

        Some(Self::new(default_font, default_text_size))
    }

    fn name(&self) -> String {
        "tiny-skia".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        let viewport = Viewport::with_physical_size(size, scale_factor);

        window::compositor::screenshot(self, &viewport, background_color)
    }
}
