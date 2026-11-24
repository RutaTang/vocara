# Vendored crates

Crates patched in through `[patch.crates-io]` in `rust/Cargo.toml`. Each is
the published crate's source, unchanged but for the marked lines (`vocara:`
comments) and the files named below; `LICENSE` is the upstream project's
(iced 0.14.0's, <https://raw.githubusercontent.com/iced-rs/iced/0.14.0/LICENSE>).

- `iced_wgpu` 0.14.0 (MIT, <https://github.com/iced-rs/iced>):
  - `src/shader/quad/solid.wgsl` draws a quad's border over its fill, as
    Flutter's `BoxDecoration` and iced's own tiny-skia renderer do.
    Upstream draws the border in place of the fill, so a translucent border
    on an opaque fill (a hovered card's coral at 60 %) showed the page
    behind the card through it instead of the card's own fill.
  - The effects' layers (see below): `src/effect.rs` and
    `src/shader/effect.wgsl` (new), `src/layer.rs` (`Layer::effect`, an
    effect's layer merging with nothing), `src/lib.rs` (the renderer's
    `start_opacity_layer`, `end_opacity_layer` and `draw_backdrop_filter`;
    the preparing and drawing loops moved into `prepare_layers` and
    `render_layers`, which go into a group's layers, draw them into an
    offscreen texture and composite it, and filter backdrops),
    `src/engine.rs` (the effects' pipelines), and
    `src/window/compositor.rs` (a window's surface allows copies from it,
    where the platform's does, for the backdrop filter to read it).
- `iced_tiny_skia` 0.14.1 (MIT, <https://github.com/iced-rs/iced>): the
  effects' layers: `src/effect.rs` (new), `src/layer.rs` (`Layer::effect`,
  redrawn whole in the damage), `src/lib.rs` (the same three methods; the
  drawing loop moved into `draw_layers`, which draws a group's layers into
  an offscreen pixmap and composites it, and filters backdrops),
  `src/engine.rs` and `src/geometry.rs` (two helpers made visible to the
  crate).
- `iced_renderer` 0.14.0 (MIT, <https://github.com/iced-rs/iced>):
  `src/effects.rs` (new) is the `Effects` trait the app's widgets call,
  implemented by both renderers and by the fallback renderer that is
  `iced::Renderer`, and `BackdropFilter`.

The effects' layers are compositing iced has no primitive for, which
Flutter did:

- An **opacity layer** (Flutter's `Opacity`, `FadeTransition`): a subtree
  drawn into a layer of its own and composited at an opacity, so where its
  parts overlap the topmost shows, not a blend of each part at the
  opacity. The renderer draws the subtree into a stack of layers of its
  own (iced's text, image, quad, mesh and canvas batches, its clips and
  transformations, all as at the top level), which goes into the
  enclosing stack as one layer: drawn offscreen (a texture, a pixmap)
  cleared to transparent, then over what lies behind it at the opacity
  (premultiplied source over).
- A **backdrop filter** (Flutter's `BackdropFilter` in a `Clip.hardEdge`):
  a layer that blurs what was drawn before it under a shape (a Gaussian,
  each pixel beyond the target's edge the nearest one inside, as a composed
  filter's `TileMode.clamp`), applies a color matrix to the unpremultiplied
  result, and draws it over what was there on the pixels whose centers the
  shape holds.

Both renderers compute them in the same float arithmetic on the same
sRGB-encoded, premultiplied 8-bit values, each result rounded once, so they
draw the same pixels: `iced_wgpu`'s shaders step for step as
`iced_tiny_skia`'s loops (the Gaussian's weights computed alike on the
CPU), the blur's horizontal pass kept in floats in both.

Moving to a new iced release: re-check whether upstream composites the
border over the fill, and drop that patch if it does; carry the effects'
layers over (upstream has neither), keeping the three renderers' drawing
loops as upstream's with the effects' branches added.
