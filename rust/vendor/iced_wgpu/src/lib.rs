//! A [`wgpu`] renderer for [Iced].
//!
//! ![The native path of the Iced ecosystem](https://github.com/iced-rs/iced/blob/0525d76ff94e828b7b21634fa94a747022001c83/docs/graphs/native.png?raw=true)
//!
//! [`wgpu`] supports most modern graphics backends: Vulkan, Metal, DX11, and
//! DX12 (OpenGL and WebGL are still WIP). Additionally, it will support the
//! incoming [WebGPU API].
//!
//! Currently, `iced_wgpu` supports the following primitives:
//! - Text, which is rendered using [`glyphon`].
//! - Quads or rectangles, with rounded borders and a solid background color.
//! - Clip areas, useful to implement scrollables or hide overflowing content.
//! - Images and SVG, loaded from memory or the file system.
//! - Meshes of triangles, useful to draw geometry freely.
//!
//! [Iced]: https://github.com/iced-rs/iced
//! [`wgpu`]: https://github.com/gfx-rs/wgpu-rs
//! [WebGPU API]: https://gpuweb.github.io/gpuweb/
//! [`glyphon`]: https://github.com/grovesNL/glyphon
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/iced-rs/iced/9ab6923e943f784985e9ef9ca28b10278297225d/docs/logo.svg"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(missing_docs)]
pub mod layer;
pub mod primitive;
pub mod settings;
pub mod window;

#[cfg(feature = "geometry")]
pub mod geometry;

mod buffer;
mod color;
mod effect; // vocara
mod engine;
mod quad;
mod text;
mod triangle;

#[cfg(any(feature = "image", feature = "svg"))]
#[path = "image/mod.rs"]
mod image;

#[cfg(not(any(feature = "image", feature = "svg")))]
#[path = "image/null.rs"]
mod image;

use buffer::Buffer;

use iced_debug as debug;
pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use wgpu;

pub use engine::Engine;
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
use crate::graphics::mesh;
use crate::graphics::text::{Editor, Paragraph};
use crate::graphics::{Shell, Viewport};

/// A [`wgpu`] graphics renderer for [`iced`].
///
/// [`wgpu`]: https://github.com/gfx-rs/wgpu-rs
/// [`iced`]: https://github.com/iced-rs/iced
pub struct Renderer {
    engine: Engine,

    default_font: Font,
    default_text_size: Pixels,
    layers: layer::Stack,

    quad: quad::State,
    triangle: triangle::State,
    text: text::State,
    text_viewport: text::Viewport,

    #[cfg(any(feature = "svg", feature = "image"))]
    image: image::State,

    // TODO: Centralize all the image feature handling
    #[cfg(any(feature = "svg", feature = "image"))]
    image_cache: std::cell::RefCell<image::Cache>,

    staging_belt: wgpu::util::StagingBelt,

    // vocara: the opacity and clip layers drawing, innermost last: each
    // holds the stack it interrupted (`layers` is the group's own
    // meanwhile).
    groups: Vec<OpenGroup>,
    // vocara: the groups' offscreen targets.
    group_targets: effect::Targets,
}

/// vocara: an opacity or clip layer being drawn.
#[derive(Debug)]
struct OpenGroup {
    /// The stack the group's layer goes into when it ends.
    parent: layer::Stack,
    /// The group's clip, transformed and inside the layer it is in.
    bounds: Rectangle,
    /// What it is.
    kind: GroupKind,
}

/// vocara: what a group does with what it draws.
#[derive(Debug)]
enum GroupKind {
    /// Composites it at an opacity.
    Opacity(f32),
    /// Cuts it by a hard-edged clip (one that holds nothing: drops it).
    #[cfg_attr(not(feature = "geometry"), allow(dead_code))]
    Clip(Option<iced_tiny_skia::HardClip>),
}

/// vocara: how many of each kind of prepared batch the layers drawn so far
/// used (the batches are prepared, and drawn, in the same order).
#[derive(Debug, Default)]
struct Counts {
    quad: usize,
    mesh: usize,
    text: usize,
    #[cfg(any(feature = "svg", feature = "image"))]
    image: usize,
}

impl Renderer {
    pub fn new(
        engine: Engine,
        default_font: Font,
        default_text_size: Pixels,
    ) -> Self {
        Self {
            default_font,
            default_text_size,
            layers: layer::Stack::new(),

            quad: quad::State::new(),
            triangle: triangle::State::new(
                &engine.device,
                &engine.triangle_pipeline,
            ),
            text: text::State::new(),
            text_viewport: engine.text_pipeline.create_viewport(&engine.device),

            #[cfg(any(feature = "svg", feature = "image"))]
            image: image::State::new(),

            #[cfg(any(feature = "svg", feature = "image"))]
            image_cache: std::cell::RefCell::new(engine.create_image_cache()),

            // TODO: Resize belt smartly (?)
            // It would be great if the `StagingBelt` API exposed methods
            // for introspection to detect when a resize may be worth it.
            staging_belt: wgpu::util::StagingBelt::new(
                buffer::MAX_WRITE_SIZE as u64,
            ),

            groups: Vec::new(),
            group_targets: effect::Targets::default(),

            engine,
        }
    }

    /// vocara: starts a group that is drawn offscreen and, when
    /// [`end_opacity_layer`](Self::end_opacity_layer) ends it, composited
    /// as one layer at `opacity` inside `bounds` (Flutter's `Opacity`):
    /// what overlaps inside it shows the topmost part, not a blend of
    /// translucent parts. Clips and transformations go on working inside,
    /// and the clip of the layer it is drawn in clips it too.
    pub fn start_opacity_layer(&mut self, bounds: Rectangle, opacity: f32) {
        let transformation = self.layers.current_mut().1;

        self.start_group(bounds * transformation, GroupKind::Opacity(opacity));
    }

    /// vocara: ends the group [`start_opacity_layer`] started; it is
    /// composited above what was drawn before it, like a layer started
    /// there.
    ///
    /// [`start_opacity_layer`]: Self::start_opacity_layer
    pub fn end_opacity_layer(&mut self) {
        self.end_group();
    }

    /// vocara: starts a group that is drawn as it would be without it and,
    /// when [`end_clip_layer`](Self::end_clip_layer) ends it, cut by `clip`
    /// with a hard edge (Flutter's `ClipPath` with `Clip.hardEdge`): every
    /// pixel whose center `clip` does not hold keeps what was there before
    /// the group. Clips and transformations go on working inside, and the
    /// clip of the layer it is drawn in clips it too. The target must allow
    /// copies from it, as for a backdrop filter; where it does not, the
    /// group is drawn unclipped.
    #[cfg(feature = "geometry")]
    pub fn start_clip_layer(&mut self, clip: &graphics::geometry::Path) {
        let transformation = self.layers.current_mut().1;
        let clip = iced_tiny_skia::HardClip::new(clip, transformation);
        let bounds = clip
            .as_ref()
            .map(iced_tiny_skia::HardClip::bounds)
            .unwrap_or_default();

        self.start_group(bounds, GroupKind::Clip(clip));
    }

    /// vocara: ends the group [`start_clip_layer`] started; it is drawn
    /// above what was drawn before it, like a layer started there.
    ///
    /// [`start_clip_layer`]: Self::start_clip_layer
    pub fn end_clip_layer(&mut self) {
        self.end_group();
    }

    /// vocara: starts a group inside `bounds` (transformed).
    fn start_group(&mut self, bounds: Rectangle, kind: GroupKind) {
        let (layer, transformation) = self.layers.current_mut();
        let bounds = bounds.intersection(&layer.bounds).unwrap_or_default();
        let mut group = layer::Stack::new();

        group.reset(bounds);
        group.push_transformation(transformation);

        let parent = std::mem::replace(&mut self.layers, group);

        self.groups.push(OpenGroup {
            parent,
            bounds,
            kind,
        });
    }

    /// vocara: ends the innermost group, whatever it is.
    fn end_group(&mut self) {
        let Some(open) = self.groups.pop() else {
            return;
        };

        let mut group = std::mem::replace(&mut self.layers, open.parent);
        group.merge();

        let effect = match open.kind {
            GroupKind::Opacity(opacity) => {
                layer::Effect::Group(Box::new(layer::Group {
                    opacity,
                    layers: group,
                }))
            }
            GroupKind::Clip(Some(clip)) => {
                layer::Effect::Clip(Box::new(layer::Clip {
                    clip,
                    layers: group,
                }))
            }
            GroupKind::Clip(None) => return,
        };

        self.layers.push_clip(Rectangle::default());
        self.layers.current_mut().0.bounds = open.bounds;
        self.layers.current_mut().0.effect = Some(effect);
        self.layers.pop_clip();
    }

    /// vocara: filters what was drawn before under `clip` (Flutter's
    /// `BackdropFilter`, in a `Clip.hardEdge`): a Gaussian blur of standard
    /// deviation `blur_sigma` (logical pixels; beyond the target's edge the
    /// nearest pixel), then `color_matrix` (4 rows of 5, applied to
    /// unpremultiplied colors, the offsets in 0..=1), drawn over it on the
    /// pixels whose centers `clip` holds. It is a layer of its own: what is
    /// drawn over it must be drawn in a later layer. The target must allow
    /// copies from it (a window's surface does where the platform allows
    /// it, as Metal's does).
    #[cfg(feature = "geometry")]
    pub fn draw_backdrop_filter(
        &mut self,
        clip: &graphics::geometry::Path,
        blur_sigma: f32,
        color_matrix: [f32; 20],
    ) {
        let (layer, transformation) = self.layers.current_mut();
        let Some(clip) = iced_tiny_skia::HardClip::new(clip, transformation)
        else {
            return;
        };
        let bounds = clip
            .bounds()
            .intersection(&layer.bounds)
            .unwrap_or_default();

        self.layers.push_clip(Rectangle::default());
        self.layers.current_mut().0.bounds = bounds;
        self.layers.current_mut().0.effect =
            Some(layer::Effect::Backdrop(Box::new(layer::Backdrop {
                clip,
                blur_sigma,
                color_matrix,
            })));
        self.layers.pop_clip();
    }

    fn draw(
        &mut self,
        clear_color: Option<Color>,
        target: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> wgpu::CommandEncoder {
        let mut encoder = self.engine.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("iced_wgpu encoder"),
            },
        );

        self.prepare(&mut encoder, viewport);
        self.render(&mut encoder, target, clear_color, viewport);

        self.quad.trim();
        self.triangle.trim();
        self.text.trim();

        // TODO: Provide window id (?)
        self.engine.trim();

        #[cfg(any(feature = "svg", feature = "image"))]
        {
            self.image.trim();
            self.image_cache.borrow_mut().trim();
        }

        encoder
    }

    pub fn present(
        &mut self,
        clear_color: Option<Color>,
        _format: wgpu::TextureFormat,
        frame: &wgpu::TextureView,
        viewport: &Viewport,
    ) -> wgpu::SubmissionIndex {
        let encoder = self.draw(clear_color, frame, viewport);

        self.staging_belt.finish();
        let submission = self.engine.queue.submit([encoder.finish()]);
        self.staging_belt.recall();
        submission
    }

    /// Renders the current surface to an offscreen buffer.
    ///
    /// Returns RGBA bytes of the texture data.
    pub fn screenshot(
        &mut self,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        #[derive(Clone, Copy, Debug)]
        struct BufferDimensions {
            width: u32,
            height: u32,
            unpadded_bytes_per_row: usize,
            padded_bytes_per_row: usize,
        }

        impl BufferDimensions {
            fn new(size: Size<u32>) -> Self {
                let unpadded_bytes_per_row = size.width as usize * 4; //slice of buffer per row; always RGBA
                let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize; //256
                let padded_bytes_per_row_padding = (alignment
                    - unpadded_bytes_per_row % alignment)
                    % alignment;
                let padded_bytes_per_row =
                    unpadded_bytes_per_row + padded_bytes_per_row_padding;

                Self {
                    width: size.width,
                    height: size.height,
                    unpadded_bytes_per_row,
                    padded_bytes_per_row,
                }
            }
        }

        let dimensions = BufferDimensions::new(viewport.physical_size());

        let texture_extent = wgpu::Extent3d {
            width: dimensions.width,
            height: dimensions.height,
            depth_or_array_layers: 1,
        };

        let texture =
            self.engine.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("iced_wgpu.offscreen.source_texture"),
                size: texture_extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.engine.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self.draw(Some(background_color), &view, viewport);

        let texture = crate::color::convert(
            &self.engine.device,
            &mut encoder,
            texture,
            if graphics::color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
        );

        let output_buffer =
            self.engine.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iced_wgpu.offscreen.output_texture_buffer"),
                size: (dimensions.padded_bytes_per_row
                    * dimensions.height as usize) as u64,
                usage: wgpu::BufferUsages::MAP_READ
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(dimensions.padded_bytes_per_row as u32),
                    rows_per_image: None,
                },
            },
            texture_extent,
        );

        self.staging_belt.finish();
        let index = self.engine.queue.submit([encoder.finish()]);
        self.staging_belt.recall();

        let slice = output_buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});

        let _ = self.engine.device.poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: None,
        });

        let mapped_buffer = slice.get_mapped_range();

        mapped_buffer.chunks(dimensions.padded_bytes_per_row).fold(
            vec![],
            |mut acc, row| {
                acc.extend(&row[..dimensions.unpadded_bytes_per_row]);
                acc
            },
        )
    }

    fn prepare(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        viewport: &Viewport,
    ) {
        self.text_viewport
            .update(&self.engine.queue, viewport.physical_size());

        self.layers.merge();

        // vocara: the layers are prepared by `prepare_layers` (upstream's
        // loop), which prepares a group's layers too, in the order
        // `render_layers` draws them.
        let layers = std::mem::take(&mut self.layers);
        self.prepare_layers(encoder, viewport, layers.as_slice());
        self.layers = layers;
    }

    /// vocara: upstream's preparing loop, moved out of `prepare` so that a
    /// group's layers are prepared with it.
    fn prepare_layers(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        viewport: &Viewport,
        layers: &[Layer],
    ) {
        let scale_factor = viewport.scale_factor();

        let physical_bounds = Rectangle::<f32>::from(Rectangle::with_size(
            viewport.physical_size(),
        ));

        for layer in layers {
            let clip_bounds = layer.bounds * scale_factor;

            if physical_bounds
                .intersection(&clip_bounds)
                .and_then(Rectangle::snap)
                .is_none()
            {
                continue;
            }

            if let Some(layer::Effect::Group(group)) = &layer.effect {
                self.prepare_layers(encoder, viewport, group.layers.as_slice());
                continue;
            }

            if let Some(layer::Effect::Clip(group)) = &layer.effect {
                self.prepare_layers(encoder, viewport, group.layers.as_slice());
                continue;
            }

            if !layer.quads.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Quad);

                self.quad.prepare(
                    &self.engine.quad_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &layer.quads,
                    viewport.projection(),
                    scale_factor,
                );

                prepare_span.finish();
            }

            if !layer.triangles.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Triangle);

                self.triangle.prepare(
                    &self.engine.triangle_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &layer.triangles,
                    Transformation::scale(scale_factor),
                    viewport.physical_size(),
                );

                prepare_span.finish();
            }

            if !layer.primitives.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Shader);

                let mut primitive_storage = self
                    .engine
                    .primitive_storage
                    .write()
                    .expect("Write primitive storage");

                for instance in &layer.primitives {
                    instance.primitive.prepare(
                        &mut primitive_storage,
                        &self.engine.device,
                        &self.engine.queue,
                        self.engine.format,
                        &instance.bounds,
                        viewport,
                    );
                }

                prepare_span.finish();
            }

            #[cfg(any(feature = "svg", feature = "image"))]
            if !layer.images.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Image);

                self.image.prepare(
                    &self.engine.image_pipeline,
                    &self.engine.device,
                    &mut self.staging_belt,
                    encoder,
                    &mut self.image_cache.borrow_mut(),
                    &layer.images,
                    viewport.projection(),
                    scale_factor,
                );

                prepare_span.finish();
            }

            if !layer.text.is_empty() {
                let prepare_span = debug::prepare(debug::Primitive::Text);

                self.text.prepare(
                    &self.engine.text_pipeline,
                    &self.engine.device,
                    &self.engine.queue,
                    &self.text_viewport,
                    encoder,
                    &layer.text,
                    layer.bounds,
                    Transformation::scale(scale_factor),
                );

                prepare_span.finish();
            }
        }
    }

    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        clear_color: Option<Color>,
        viewport: &Viewport,
    ) {
        // vocara: the layers are drawn by `render_layers` (upstream's loop),
        // which draws a group's layers into an offscreen target and
        // composites them, and filters backdrops.
        let layers = std::mem::take(&mut self.layers);
        let mut counts = Counts::default();

        self.render_layers(
            encoder,
            frame,
            clear_color,
            viewport,
            layers.as_slice(),
            &mut counts,
            0,
        );
        self.layers = layers;
        self.group_targets.trim();

        let scale_factor = viewport.scale_factor();
        let physical_bounds = Rectangle::<f32>::from(Rectangle::with_size(
            viewport.physical_size(),
        ));

        debug::layers_rendered(|| {
            self.layers
                .iter()
                .filter(|layer| {
                    !layer.is_empty()
                        && physical_bounds
                            .intersection(&(layer.bounds * scale_factor))
                            .is_some_and(|viewport| viewport.snap().is_some())
                })
                .count()
        });
    }

    /// vocara: upstream's drawing loop, moved out of `render` so that a
    /// group's layers can be drawn into an offscreen target with it;
    /// `depth` is how deep in groups `layers` are.
    #[allow(clippy::too_many_arguments)]
    fn render_layers(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        clear_color: Option<Color>,
        viewport: &Viewport,
        layers: &[Layer],
        counts: &mut Counts,
        depth: usize,
    ) {
        use std::mem::ManuallyDrop;

        let mut render_pass = ManuallyDrop::new(encoder.begin_render_pass(
            &wgpu::RenderPassDescriptor {
                label: Some("iced_wgpu render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: match clear_color {
                            Some(background_color) => wgpu::LoadOp::Clear({
                                let [r, g, b, a] =
                                    graphics::color::pack(background_color)
                                        .components();

                                wgpu::Color {
                                    r: f64::from(r),
                                    g: f64::from(g),
                                    b: f64::from(b),
                                    a: f64::from(a),
                                }
                            }),
                            None => wgpu::LoadOp::Load,
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            },
        ));

        let scale_factor = viewport.scale_factor();
        let physical_bounds = Rectangle::<f32>::from(Rectangle::with_size(
            viewport.physical_size(),
        ));

        let scale = Transformation::scale(scale_factor);

        for layer in layers {
            let Some(physical_bounds) =
                physical_bounds.intersection(&(layer.bounds * scale_factor))
            else {
                continue;
            };

            let Some(scissor_rect) = physical_bounds.snap() else {
                continue;
            };

            if let Some(effect) = &layer.effect {
                let _ = ManuallyDrop::into_inner(render_pass);

                match effect {
                    layer::Effect::Group(group) => {
                        let target = self.group_targets.get(
                            &self.engine.device,
                            depth,
                            viewport.physical_size(),
                            self.engine.format,
                        );

                        self.render_layers(
                            encoder,
                            &target,
                            Some(Color::TRANSPARENT),
                            viewport,
                            group.layers.as_slice(),
                            counts,
                            depth + 1,
                        );

                        self.engine.effect_pipeline.composite(
                            &self.engine.device,
                            encoder,
                            frame,
                            viewport.physical_size(),
                            &target,
                            group.opacity,
                            scissor_rect,
                        );
                    }
                    layer::Effect::Backdrop(backdrop) => {
                        self.engine.effect_pipeline.backdrop(
                            &self.engine.device,
                            &self.engine.queue,
                            encoder,
                            frame,
                            viewport.physical_size(),
                            backdrop,
                            scale_factor,
                            physical_bounds,
                        );
                    }
                    layer::Effect::Clip(group) => {
                        // Drawn in place, as at the top level, then cut.
                        let saved = self.engine.effect_pipeline.save(
                            &self.engine.device,
                            &self.engine.queue,
                            encoder,
                            frame,
                            viewport.physical_size(),
                            &group.clip,
                            scale_factor,
                            physical_bounds,
                        );

                        self.render_layers(
                            encoder,
                            frame,
                            None,
                            viewport,
                            group.layers.as_slice(),
                            counts,
                            depth,
                        );

                        if let Some(saved) = saved {
                            self.engine.effect_pipeline.restore(
                                &self.engine.device,
                                encoder,
                                frame,
                                viewport.physical_size(),
                                &saved,
                            );
                        }
                    }
                }

                render_pass = ManuallyDrop::new(encoder.begin_render_pass(
                    &wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: frame,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    },
                ));

                continue;
            }

            if !layer.quads.is_empty() {
                let render_span = debug::render(debug::Primitive::Quad);
                self.quad.render(
                    &self.engine.quad_pipeline,
                    counts.quad,
                    scissor_rect,
                    &layer.quads,
                    &mut render_pass,
                );
                render_span.finish();

                counts.quad += 1;
            }

            if !layer.triangles.is_empty() {
                let _ = ManuallyDrop::into_inner(render_pass);

                let render_span = debug::render(debug::Primitive::Triangle);
                counts.mesh += self.triangle.render(
                    &self.engine.triangle_pipeline,
                    encoder,
                    frame,
                    counts.mesh,
                    &layer.triangles,
                    physical_bounds,
                    scale,
                );
                render_span.finish();

                render_pass = ManuallyDrop::new(encoder.begin_render_pass(
                    &wgpu::RenderPassDescriptor {
                        label: Some("iced_wgpu render pass"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: frame,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    },
                ));
            }

            if !layer.primitives.is_empty() {
                let render_span = debug::render(debug::Primitive::Shader);

                let primitive_storage = self
                    .engine
                    .primitive_storage
                    .read()
                    .expect("Read primitive storage");

                let mut need_render = Vec::new();

                for instance in &layer.primitives {
                    let bounds = instance.bounds * scale;

                    if let Some(clip_bounds) = (instance.bounds * scale)
                        .intersection(&physical_bounds)
                        .and_then(Rectangle::snap)
                    {
                        render_pass.set_viewport(
                            bounds.x,
                            bounds.y,
                            bounds.width,
                            bounds.height,
                            0.0,
                            1.0,
                        );

                        render_pass.set_scissor_rect(
                            clip_bounds.x,
                            clip_bounds.y,
                            clip_bounds.width,
                            clip_bounds.height,
                        );

                        let drawn = instance
                            .primitive
                            .draw(&primitive_storage, &mut render_pass);

                        if !drawn {
                            need_render.push((instance, clip_bounds));
                        }
                    }
                }

                render_pass.set_viewport(
                    0.0,
                    0.0,
                    viewport.physical_width() as f32,
                    viewport.physical_height() as f32,
                    0.0,
                    1.0,
                );

                render_pass.set_scissor_rect(
                    0,
                    0,
                    viewport.physical_width(),
                    viewport.physical_height(),
                );

                if !need_render.is_empty() {
                    let _ = ManuallyDrop::into_inner(render_pass);

                    for (instance, clip_bounds) in need_render {
                        instance.primitive.render(
                            &primitive_storage,
                            encoder,
                            frame,
                            &clip_bounds,
                        );
                    }

                    render_pass = ManuallyDrop::new(encoder.begin_render_pass(
                        &wgpu::RenderPassDescriptor {
                            label: Some("iced_wgpu render pass"),
                            color_attachments: &[Some(
                                wgpu::RenderPassColorAttachment {
                                    view: frame,
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Load,
                                        store: wgpu::StoreOp::Store,
                                    },
                                },
                            )],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        },
                    ));
                }

                render_span.finish();
            }

            #[cfg(any(feature = "svg", feature = "image"))]
            if !layer.images.is_empty() {
                let render_span = debug::render(debug::Primitive::Image);
                self.image.render(
                    &self.engine.image_pipeline,
                    counts.image,
                    scissor_rect,
                    &mut render_pass,
                );
                render_span.finish();

                counts.image += 1;
            }

            if !layer.text.is_empty() {
                let render_span = debug::render(debug::Primitive::Text);
                counts.text += self.text.render(
                    &self.engine.text_pipeline,
                    &self.text_viewport,
                    counts.text,
                    &layer.text,
                    scissor_rect,
                    &mut render_pass,
                );
                render_span.finish();
            }
        }

        let _ = ManuallyDrop::into_inner(render_pass);
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
        quad: core::renderer::Quad,
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
        _callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>)
        + Send
        + 'static,
    ) {
        #[cfg(feature = "image")]
        self.image_cache
            .get_mut()
            .allocate_image(_handle, _callback);
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

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.image_cache.borrow_mut().load_image(
            &self.engine.device,
            &self.engine.queue,
            handle,
        )
    }

    fn measure_image(&self, handle: &Self::Handle) -> Option<core::Size<u32>> {
        self.image_cache.borrow_mut().measure_image(handle)
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
    fn measure_svg(&self, handle: &core::svg::Handle) -> core::Size<u32> {
        self.image_cache.borrow_mut().measure_svg(handle)
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

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, mesh: graphics::Mesh) {
        debug_assert!(
            !mesh.indices().is_empty(),
            "Mesh must not have empty indices"
        );

        debug_assert!(
            mesh.indices().len().is_multiple_of(3),
            "Mesh indices length must be a multiple of 3"
        );

        let (layer, transformation) = self.layers.current_mut();
        layer.draw_mesh(mesh, transformation);
    }

    fn draw_mesh_cache(&mut self, cache: mesh::Cache) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_mesh_cache(cache, transformation);
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
                meshes,
                images,
                text,
            } => {
                layer.draw_mesh_group(meshes, transformation);

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, transformation);
            }
            Geometry::Cached(cache) => {
                if let Some(meshes) = cache.meshes {
                    layer.draw_mesh_cache(meshes, transformation);
                }

                if let Some(images) = cache.images {
                    for image in images.iter().cloned() {
                        layer.draw_image(image, transformation);
                    }
                }

                if let Some(text) = cache.text {
                    layer.draw_text_cache(text, transformation);
                }
            }
        }
    }
}

impl primitive::Renderer for Renderer {
    fn draw_primitive(&mut self, bounds: Rectangle, primitive: impl Primitive) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_primitive(bounds, primitive, transformation);
    }
}

impl graphics::compositor::Default for crate::Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(
        default_font: Font,
        default_text_size: Pixels,
        backend: Option<&str>,
    ) -> Option<Self> {
        if backend.is_some_and(|backend| backend != "wgpu") {
            return None;
        }

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env()
                .unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok()?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("iced_wgpu [headless]"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .ok()?;

        let engine = Engine::new(
            &adapter,
            device,
            queue,
            if graphics::color::GAMMA_CORRECTION {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            Some(graphics::Antialiasing::MSAAx4),
            Shell::headless(),
        );

        Some(Self::new(engine, default_font, default_text_size))
    }

    fn name(&self) -> String {
        "wgpu".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        self.screenshot(
            &Viewport::with_physical_size(size, scale_factor),
            background_color,
        )
    }
}
