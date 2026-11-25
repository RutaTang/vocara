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
- `iced_winit` 0.14.1 (MIT, <https://github.com/iced-rs/iced>): the run
  loop hands the app each wheel event's gesture, which iced's
  `WheelScrolled` cannot carry (`rust/crates/vocara_app/src/ui/core/
  widgets/pan_zoom.rs` takes it). `src/gesture.rs` (new) reads, while
  `src/lib.rs` converts winit's `MouseWheel`, the scroll event AppKit is
  dispatching (`NSApp.currentEvent`: winit calls its handler from the
  view's `scrollWheel:` and iced converts the event before returning) for
  its phase and momentum phase, which winit merges, checks it is the
  event winit made the delta of, and runs Flutter's macOS embedder over it
  (`dispatchGestureEvent:`: pan-zoom start, update and end; momentum
  dropped; a scroll without phases a signal); `src/lib.rs` then hands each
  wheel event to the interface in an update of its own with its gesture
  set (`gesture::update_each`, `gesture::current`). `src/conversion.rs`
  hands a precise (pixel) delta over in logical pixels, as every other
  position: winit's are physical, so iced scrolled twice a trackpad's
  travel on a Retina screen. `Cargo.toml` adds the objc2 0.6 AppKit and
  Foundation bindings (macOS only) the reading uses. `src/lib.rs` also
  allows futures' `try_next`, deprecated since the release, whose warning
  a path dependency shows.

Moving to a new iced release: re-check whether upstream composites the
border over the fill, and drop the patch if it does.

Moving iced_winit to a new release: carry `src/gesture.rs` and the marked
lines over; drop the logical pixel delta if upstream scales it, and the
whole patch if winit and iced come to pass the momentum phase on.
