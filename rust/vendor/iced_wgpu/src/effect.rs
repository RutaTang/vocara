//! vocara: the effects' layers (`layer::Effect`): a subtree drawn into an
//! offscreen texture and composited at an opacity, a subtree inside a
//! hard-edged clip, and what lies under a shape blurred and color-filtered
//! (`shader/effect.wgsl`).
//!
//! The arithmetic is iced_tiny_skia's `effect` module's, so the two
//! renderers draw the same 8-bit values: colors premultiplied and
//! sRGB-encoded (blended as iced blends them, `web-colors`), each result
//! rounded once by the target's conversion. Which pixels a hard-edged clip
//! holds is iced_tiny_skia's own answer ([`HardClip::mask`], uploaded as a
//! texture), so both renderers clip the same pixels.
use crate::core::{Rectangle, Size};

use iced_tiny_skia::{HardClip, PixelBox};

use std::borrow::Cow;
use wgpu::util::DeviceExt;

/// The longest blur radius, in physical pixels (the uniform holds this
/// many weights past the center's).
pub const MAX_BLUR_RADIUS: usize = 255;

/// The weights of a Gaussian blur of standard deviation `sigma` (physical
/// pixels): the center's, then each further pixel's on one side, out to
/// three standard deviations (at most [`MAX_BLUR_RADIUS`]), summing to one
/// over both sides. iced_tiny_skia computes the same.
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

/// The uniform of `shader/effect.wgsl`.
#[derive(Debug, Clone, Copy, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct Uniforms {
    origin: [i32; 2],
    size: [i32; 2],
    source: [i32; 2],
    mask: [i32; 2],
    radius: i32,
    opacity: f32,
    _padding: [f32; 2],
    rows: [[f32; 4]; 4],
    offset: [f32; 4],
    weights: [[f32; 4]; 64],
}

impl Uniforms {
    fn new(size: Size<u32>) -> Self {
        Self {
            origin: [0, 0],
            size: [size.width as i32, size.height as i32],
            source: [0, 0],
            mask: [0, 0],
            radius: 0,
            opacity: 1.0,
            _padding: [0.0; 2],
            rows: [[0.0; 4]; 4],
            offset: [0.0; 4],
            weights: [[0.0; 4]; 64],
        }
    }
}

/// The pipelines of the effects' layers.
#[derive(Debug, Clone)]
pub struct Pipeline {
    layout: wgpu::BindGroupLayout,
    composite: wgpu::RenderPipeline,
    horizontal: wgpu::RenderPipeline,
    vertical: wgpu::RenderPipeline,
    restore: wgpu::RenderPipeline,
    /// The mask bound where a pass reads none.
    no_mask: wgpu::TextureView,
}

/// The format of the blur's horizontal pass: floats, so the vertical pass
/// reads it unrounded, as iced_tiny_skia keeps it.
const HORIZONTAL_FORMAT: wgpu::TextureFormat =
    wgpu::TextureFormat::Rgba16Float;

/// The format of a hard-edged clip's mask: 255 where it holds a pixel, 0
/// elsewhere.
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

impl Pipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float {
                    filterable: false,
                },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        let layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("iced_wgpu.effect.layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX
                            | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<Uniforms>() as u64,
                            ),
                        },
                        count: None,
                    },
                    texture(1),
                    texture(2),
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("iced_wgpu.effect.pipeline_layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });

        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("iced_wgpu.effect.shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                    "shader/effect.wgsl"
                ))),
            });

        let pipeline = |label: &'static str,
                        fragment: &'static str,
                        format: wgpu::TextureFormat,
                        blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("cover"),
                    buffers: &[],
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
                cache: None,
            })
        };

        let composite = pipeline(
            "iced_wgpu.effect.composite",
            "composite",
            format,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );

        let horizontal = pipeline(
            "iced_wgpu.effect.horizontal",
            "horizontal",
            HORIZONTAL_FORMAT,
            None,
        );

        let vertical = pipeline(
            "iced_wgpu.effect.vertical",
            "vertical",
            format,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );

        // What was there, put back as it was.
        let restore =
            pipeline("iced_wgpu.effect.restore", "restore", format, None);

        let no_mask = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("iced_wgpu.effect.no_mask"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: MASK_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            layout,
            composite,
            horizontal,
            vertical,
            restore,
            no_mask,
        }
    }

    fn bind_group(
        &self,
        device: &wgpu::Device,
        uniforms: &Uniforms,
        source: &wgpu::TextureView,
        mask: Option<&wgpu::TextureView>,
    ) -> wgpu::BindGroup {
        let buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iced_wgpu.effect.uniforms"),
                contents: bytemuck::bytes_of(uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iced_wgpu.effect.bind_group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        mask.unwrap_or(&self.no_mask),
                    ),
                },
            ],
        })
    }

    /// Draws `layer` over `target` at `opacity` (premultiplied source
    /// over) inside `scissor`.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: Size<u32>,
        layer: &wgpu::TextureView,
        opacity: f32,
        scissor: Rectangle<u32>,
    ) {
        let uniforms = Uniforms {
            opacity: opacity.clamp(0.0, 1.0),
            ..Uniforms::new(size)
        };
        let bind_group = self.bind_group(device, &uniforms, layer, None);
        let mut pass = begin_pass(encoder, target, "iced_wgpu.effect.composite");

        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_scissor_rect(
            scissor.x,
            scissor.y,
            scissor.width,
            scissor.height,
        );
        pass.draw(0..3, 0..1);
    }

    /// Saves what `target` holds inside `bounds` (physical), and which of
    /// those pixels `clip` holds, before a subtree inside `clip` is drawn
    /// over it in place; [`Pipeline::restore`] then puts back every pixel
    /// `clip` does not hold.
    ///
    /// The target must allow copies from it; when it does not (a surface
    /// that cannot), nothing is saved and the subtree goes unclipped.
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: Size<u32>,
        clip: &HardClip,
        scale_factor: f32,
        bounds: Rectangle,
    ) -> Option<Saved> {
        let texture = target.texture();

        static WARNED: std::sync::Once = std::sync::Once::new();

        if !copyable(
            texture,
            &WARNED,
            "A clip layer needs a target it can copy from; it is drawn \
             unclipped",
        ) {
            return None;
        }

        // Every pixel the subtree can draw on (its layers' scissors lie
        // inside its bounds).
        let region = PixelBox::around(bounds, size.width, size.height);

        if region.width() == 0 || region.height() == 0 {
            return None;
        }

        let mask = clip
            .mask(scale_factor, region)
            .unwrap_or_else(|| vec![0; region.width() * region.height()]);
        let extent = wgpu::Extent3d {
            width: region.width() as u32,
            height: region.height() as u32,
            depth_or_array_layers: 1,
        };

        let copy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu.effect.saved"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture.format(),
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x0 as u32,
                    y: region.y0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            copy.as_image_copy(),
            extent,
        );

        Some(Saved {
            region,
            copy: copy.create_view(&wgpu::TextureViewDescriptor::default()),
            mask: upload_mask(device, queue, region, &mask),
        })
    }

    /// Puts back what [`Pipeline::save`] saved of `target` on every pixel
    /// the clip does not hold.
    pub fn restore(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: Size<u32>,
        saved: &Saved,
    ) {
        let region = saved.region;
        let uniforms = Uniforms {
            origin: [region.x0 as i32, region.y0 as i32],
            mask: [region.x0 as i32, region.y0 as i32],
            ..Uniforms::new(size)
        };
        let bind_group =
            self.bind_group(device, &uniforms, &saved.copy, Some(&saved.mask));
        let mut pass = begin_pass(encoder, target, "iced_wgpu.effect.restore");

        pass.set_pipeline(&self.restore);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_scissor_rect(
            region.x0 as u32,
            region.y0 as u32,
            region.width() as u32,
            region.height() as u32,
        );
        pass.draw(0..3, 0..1);
    }

    /// Filters what `target` holds under `backdrop`'s clip, inside
    /// `bounds` (physical): blurs it (each pixel beyond the target's edge
    /// taken as the nearest one inside, Flutter's `TileMode.clamp`),
    /// applies the color matrix, and draws the result over what was there
    /// (source over), on the pixels whose centers the clip holds.
    ///
    /// The target must allow copies from it; when it does not (a surface
    /// that cannot), nothing is drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn backdrop(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: Size<u32>,
        backdrop: &crate::layer::Backdrop,
        scale_factor: f32,
        bounds: Rectangle,
    ) {
        let texture = target.texture();

        static WARNED: std::sync::Once = std::sync::Once::new();

        if !copyable(
            texture,
            &WARNED,
            "A backdrop filter needs a target it can copy from; none is drawn",
        ) {
            return;
        }

        let (width, height) = (size.width as usize, size.height as usize);
        let snap = |v: f32, max: usize| v.round().clamp(0.0, max as f32) as usize;
        let within = PixelBox {
            x0: snap(bounds.x, width),
            y0: snap(bounds.y, height),
            x1: snap(bounds.x + bounds.width, width),
            y1: snap(bounds.y + bounds.height, height),
        };

        let Some(region) =
            backdrop
                .clip
                .pixels(scale_factor, size.width, size.height, within)
        else {
            return;
        };
        let Some(mask) = backdrop.clip.mask(scale_factor, region) else {
            return;
        };
        let PixelBox { x0, y0, x1, y1 } = region;

        let kernel =
            blur_kernel(backdrop.blur_sigma * backdrop.clip.scale(scale_factor));
        let radius = kernel.len() - 1;

        // What the blur reads: the clip's box and the blur's reach,
        // inside the target.
        let rx0 = x0.saturating_sub(radius);
        let rx1 = (x1 + radius).min(width);
        let ry0 = y0.saturating_sub(radius);
        let ry1 = (y1 + radius).min(height);

        let copy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu.effect.backdrop"),
            size: wgpu::Extent3d {
                width: (rx1 - rx0) as u32,
                height: (ry1 - ry0) as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture.format(),
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rx0 as u32,
                    y: ry0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            copy.as_image_copy(),
            copy.size(),
        );

        let horizontal = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iced_wgpu.effect.horizontal"),
            size: wgpu::Extent3d {
                width: (x1 - x0) as u32,
                height: (ry1 - ry0) as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HORIZONTAL_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let horizontal_view =
            horizontal.create_view(&wgpu::TextureViewDescriptor::default());
        let copy_view = copy.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = upload_mask(device, queue, region, &mask);

        let mut weights = [[0.0; 4]; 64];

        for (i, weight) in kernel.iter().enumerate() {
            weights[i / 4][i % 4] = *weight;
        }

        let m = &backdrop.color_matrix;
        let uniforms = Uniforms {
            origin: [x0 as i32, ry0 as i32],
            source: [rx0 as i32, ry0 as i32],
            mask: [x0 as i32, y0 as i32],
            radius: radius as i32,
            rows: [
                [m[0], m[1], m[2], m[3]],
                [m[5], m[6], m[7], m[8]],
                [m[10], m[11], m[12], m[13]],
                [m[15], m[16], m[17], m[18]],
            ],
            offset: [m[4], m[9], m[14], m[19]],
            weights,
            ..Uniforms::new(size)
        };

        {
            let bind_group =
                self.bind_group(device, &uniforms, &copy_view, None);
            let mut pass = begin_pass(
                encoder,
                &horizontal_view,
                "iced_wgpu.effect.horizontal",
            );

            pass.set_pipeline(&self.horizontal);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let bind_group = self.bind_group(
            device,
            &uniforms,
            &horizontal_view,
            Some(&mask_view),
        );
        let mut pass = begin_pass(encoder, target, "iced_wgpu.effect.vertical");

        pass.set_pipeline(&self.vertical);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_scissor_rect(
            x0 as u32,
            y0 as u32,
            (x1 - x0) as u32,
            (y1 - y0) as u32,
        );
        pass.draw(0..3, 0..1);
    }
}

/// What a clip layer saved of its target ([`Pipeline::save`]).
#[derive(Debug)]
pub struct Saved {
    /// The pixels saved.
    region: PixelBox,
    /// What they held.
    copy: wgpu::TextureView,
    /// Which of them the clip holds.
    mask: wgpu::TextureView,
}

/// Whether `texture` can be copied from; when it cannot, logs `warning`
/// the first time `warned` is passed.
fn copyable(
    texture: &wgpu::Texture,
    warned: &std::sync::Once,
    warning: &str,
) -> bool {
    if texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return true;
    }

    warned.call_once(|| log::warn!("{warning}"));

    false
}

/// A hard-edged clip's `mask` of `region` as a texture.
fn upload_mask(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    region: PixelBox,
    mask: &[u8],
) -> wgpu::TextureView {
    device
        .create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("iced_wgpu.effect.mask"),
                size: wgpu::Extent3d {
                    width: region.width() as u32,
                    height: region.height() as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: MASK_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            mask,
        )
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// A pass drawing over what `target` holds.
fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    label: &'static str,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

/// The offscreen targets of the groups, one for each depth of nesting,
/// the viewport's size, kept from frame to frame.
#[derive(Debug, Default)]
pub struct Targets {
    targets: Vec<(wgpu::Texture, wgpu::TextureView)>,
    used: bool,
}

impl Targets {
    /// The target of a group nested `depth` deep (0 for one at the top),
    /// `size` and `format`.
    pub fn get(
        &mut self,
        device: &wgpu::Device,
        depth: usize,
        size: Size<u32>,
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        self.used = true;

        if let Some((texture, _)) = self.targets.get(depth) {
            if texture.width() != size.width
                || texture.height() != size.height
                || texture.format() != format
            {
                self.targets.truncate(depth);
            }
        }

        while self.targets.len() <= depth {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("iced_wgpu.effect.group"),
                size: wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view =
                texture.create_view(&wgpu::TextureViewDescriptor::default());

            self.targets.push((texture, view));
        }

        self.targets[depth].1.clone()
    }

    /// Drops the targets when no group was drawn since the last trim.
    pub fn trim(&mut self) {
        if !self.used {
            self.targets.clear();
        }

        self.used = false;
    }
}
