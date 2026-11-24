# Vendored crates

Crates patched in through `[patch.crates-io]` in `rust/Cargo.toml`. Each is
the published crate's source, unchanged but for the marked lines (`vocara:`
comments); `LICENSE` is the upstream project's.

- `iced_wgpu` 0.14.0 (MIT, <https://github.com/iced-rs/iced>):
  `src/shader/quad/solid.wgsl` draws a quad's border over its fill, as
  Flutter's `BoxDecoration` and iced's own tiny-skia renderer do. Upstream
  draws the border in place of the fill, so a translucent border on an
  opaque fill (a hovered card's coral at 60 %) showed the page behind the
  card through it instead of the card's own fill.

Moving to a new iced release: re-check whether upstream composites the
border over the fill, and drop the patch if it does.
