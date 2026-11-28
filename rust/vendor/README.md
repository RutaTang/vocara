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
    `start_opacity_layer`, `end_opacity_layer`, `start_clip_layer`,
    `end_clip_layer` and `draw_backdrop_filter`; the preparing and drawing
    loops moved into `prepare_layers` and `render_layers`, which go into a
    group's layers, draw them into an offscreen texture and composite it,
    or in place and put back what lies outside its clip, and filter
    backdrops), `src/engine.rs` (the effects' pipelines),
    `src/window/compositor.rs` (a window's surface allows copies from it,
    where the platform's does, for the backdrop filter and the clip layer
    to read it), and `Cargo.toml` (`iced_tiny_skia`, whose `HardClip` gives
    both renderers the pixels a clip holds; the patched crate iced builds
    anyway, no new crate).
- `iced_tiny_skia` 0.14.1 (MIT, <https://github.com/iced-rs/iced>): the
  effects' layers: `src/effect.rs` (new; with `HardClip` and `PixelBox`,
  public for `iced_wgpu`), `src/layer.rs` (`Layer::effect`, redrawn whole
  in the damage), `src/lib.rs` (the same five methods; the drawing loop
  moved into `draw_layers`, which draws a group's layers into an offscreen
  pixmap and composites it, or in place and puts back what lies outside
  its clip, and filters backdrops), `src/engine.rs` and `src/geometry.rs`
  (two helpers made visible to the crate). And `src/engine.rs` draws a raw
  text (the app's `studio_text`) through a mask set to its own clip, as
  it draws every other text: upstream drew it through the mask the text
  before it in the layer had left, so a label after a text editor taller
  than its box (a prose field in a scroll view) was not drawn.
- `iced_renderer` 0.14.0 (MIT, <https://github.com/iced-rs/iced>):
  `src/effects.rs` (new) is the `Effects` trait the app's widgets call,
  implemented by both renderers and by the fallback renderer that is
  `iced::Renderer`, and `BackdropFilter`.
- `cosmic-text` 0.15.0 (MIT OR Apache-2.0,
  <https://github.com/pop-os/cosmic-text>; `LICENSE-MIT` and
  `LICENSE-APACHE` its own; only `src`, the manifests, the licenses and
  the README vendored, the `[[test]]` and `[[bench]]` targets dropped from
  `Cargo.toml`, whose files are not): a face drawn at a fixed instance of
  its variation axes, which the labels Flutter set in the system UI font
  need (the text toolbar's and the canvas labels': that face at the
  instance Flutter's Skia drew each weight at, untracked;
  `rust/crates/vocara_app/src/ui/core/system_font.rs`).
  Upstream varies a face along its weight axis alone, the weight asked
  for, every other axis at its default, and its shaper (harfrust) applies
  a face's `trak` table at 12 pt when the face has a `STAT` table.
  `FontSystem::set_face_instance` (`src/font/system.rs`) gives one face a
  `FaceInstance` (`src/font/mod.rs`): user coordinates on its axes, set
  after the weight's, whether it is untracked, and OpenType features every
  run shaped in it takes before its attributes' own (`src/shape.rs`,
  `shape_fallback`: for a canvas's text, which carries none, set in a copy
  of a face with `tnum`: `studio_text::tabular_font`); every `Font` of that
  face then takes them, in `Font::new_with_instance`: its metrics, the
  shaper's instance (advances, positioning, substitutions), the outlines
  swash draws (`src/swash.rs`: the same normalized coordinates, for the
  images and the outline commands), and, untracked, the shaper reads the
  face through a copy of its table directory without its `trak` record
  (`untracked_directory`, `shaped_font`). Setting one drops that face's
  cached fonts and shape plans (`ShapeBuffer::forget_font`, `src/shape.rs`).
  A face without an instance is drawn as upstream draws it.

  Its lines are laid out as Flutter's SkParagraph laid them out (Skia
  `ea7cdbc6b986bbefcbac92fa429782e59518510f`, which Flutter 3.38.4 pins;
  the rules, with their sources, are in
  `rust/crates/vocara_app/src/ui/core/text.rs`), where upstream centres a
  line's glyphs' ascent and descent in the line height, unrounded:
  `src/layout.rs` adds `RunMetrics` (a run's font size, ascent, descent,
  line gap and own line height: `Run::calculateMetrics`, the font's own
  ascent and descent with half the line gap above and half below without
  a height, a height multiplier within `SK_ScalarNearlyZero` of zero
  being none, else those scaled by the height over their sum), `LineBox`
  (a line's height rounded to the nearest pixel, its baseline its ascent
  rounded half up: `InternalLineMetrics::height`, `TextLine`'s
  `correctedBaseline`), `LayoutLine::runs` and `LayoutLine::line_box`
  (the largest ascent and descent of its runs; an empty paragraph's by
  `ParagraphImpl::computeEmptyMetrics`, its leading the font's line gap),
  and `FontVerticals` (a font's ascent, descent, line gap and units per
  em, scaled to a size in `f64` and made `f32`s, as CoreText gave Skia
  its metrics); `src/shape.rs` records a glyph's font's verticals
  (`ShapeGlyph::vertical`) and a line's runs as it lays the line out;
  `src/buffer_line.rs` gives a line without glyphs
  its default font's run (`BufferLine::default_run`); `src/buffer.rs`
  makes `LayoutRunIter` (every line's `line_top`, `line_y`,
  `line_height`, so every renderer and measure) and the scroll
  (`shape_until_cursor`, `shape_until_scroll`) take each line's box, so
  lines lie on whole pixels one under the other, takes a line height of
  zero as a line without a height (upstream panics; `Buffer::new`,
  `Buffer::new_empty`, `Motion::Vertical`), adds
  `Buffer::is_empty_paragraph`, and keeps the scroll from moving past a
  line no pixel tall, which would never stop. `LayoutGlyph::physical`
  (`src/layout.rs`) rounds a glyph's vertical origin to the nearest device
  pixel, half up, as Skia places a glyph (`SkGlyphPositionRoundingSpec`),
  where upstream truncates; `src/math.rs` drops the `truncf` no longer
  used. A face's ascent and descent are the ones it is shaped with
  (skrifa's: `hhea`'s with their `MVAR` deltas at its instance, or
  `OS/2`'s typographic ones for a face that asks for them, the same for
  every face the app draws with), its line gap `hhea`'s with its `MVAR`
  delta whatever `OS/2` asks for (`Font::line_gap`, `src/font/mod.rs`:
  Lucide asks for its typographic line gap of 90 units, `hhea`'s is 0):
  what CoreText gave Skia. The `syntect` and `vi` editors (features the
  workspace does not enable) still step by the line height.

Moving to a new cosmic-text release: carry the marked lines over (or drop
the patch if upstream comes to take a face's variation coordinates and to
leave a face untracked, and to lay its lines out on whole pixels as
SkParagraph does), and keep the version iced's renderers use.

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
  set (`gesture::update_each`, `gesture::current`). winit reads no
  trackpad touches, which Flutter's embedder turned into an inertia
  cancel (`touchesBeganWithEvent:` within 50 ms of the momentum's last
  `Changed`, once per momentum, none after a pan-zoom start): `src/lib.rs`
  has each window it makes set up by `gesture::accept_touches`, which
  gives the window's view (winit's `WinitView`) indirect and resting
  touches, as Flutter's view had them, and adds to its class at runtime
  (`class_addMethod`, once per class; a method the class had of its own
  would be replaced) a `touchesBeganWithEvent:` that records the touch
  (its view, time and place) and calls through to the implementation the
  class had (`NSResponder`'s). Before each event it handles, the run loop
  takes the touches recorded (`gesture::take_touches`), runs the
  embedder's rule over each (`Embedder::touched`) and hands each cancel to
  its window's interface as a `WheelScrolled` of no delta, with its
  gesture, hit tested where the touch was (`gesture::cursor`).
  `src/conversion.rs` hands a precise (pixel) delta over in logical
  pixels, as every other position: winit's are physical, so iced scrolled
  twice a trackpad's travel on a Retina screen. `Cargo.toml` adds the
  objc2 0.6 AppKit and Foundation bindings (macOS only) the reading and
  the touches use. `src/lib.rs` also allows futures' `try_next`,
  deprecated since the release, whose warning a path dependency shows.
  winit reports no pointer during a drag in from another app (Finder) and
  has no `draggingUpdated:`: `src/drop_pointer.rs` (new) reads the pointer
  from AppKit (`mouseLocationOutsideOfEventStream`), and `src/lib.rs` moves
  the cursor there (a `CursorMoved` of its own, `synthesize`) before a
  `HoveredFile` or `DroppedFile` where it is not already, again about 30
  times a second while a file hovers (`drop_pointer::Drag`, the loop woken
  for each read), and lets it leave (`CursorLeft`) after
  `HoveredFileCancelled`. `src/window/state.rs` adds `cursor_position`.
  On macOS a control-click is a secondary click, as AppKit takes it:
  `src/secondary_click.rs` (new) and `src/lib.rs` report a left press with
  Control held, and its release, as the right button's.
  winit 0.30.13's view commits an `insertText:replacementRange:` only
  while it has marked text (typed text goes out as the key press's), so
  text macOS inserts with no key press and nothing marked (Edit ▸ Emoji &
  Symbols, dictation) reached no field: `src/insert_text.rs` (new) gives
  the view's class, by the touches' runtime means, a `keyDown:` that notes
  a key press is handled while it calls through, and an
  `insertText:replacementRange:` that calls through first (winit does
  everything it did, a composition's commit included) and then records
  the text when no key press is handled, nothing is marked after the call,
  and nothing is replaced (`insert_text::dropped`); `src/lib.rs` accepts
  each window (`insert_text::accept`) and, before each event, hands each
  record to its window's interface as an input method commit
  (`insert_text::take`). `Cargo.toml` adds the Foundation features it reads.
  winit's window delegate answers `draggingEntered:` with "copy" and has
  no `draggingUpdated:`, so the copy badge showed over the whole window
  and a drop beside every target landed on nothing: `src/drag_operation.rs`
  (new) gives the delegate's class a `draggingUpdated:` (added, never
  replaced) that answers "none" only where the app, in the update for the
  cursor's last move under the hovering file, found no drop target taking
  it at that very spot (`drag_operation::reached`, called by the app's
  targets); anywhere it has not looked yet it answers "copy", as before.
  `src/lib.rs` brackets those updates (`open`, `close`) and clears the
  verdict as a drag starts, drops or leaves; `src/drop_pointer.rs`'s read
  is split out for it (`position_in`).
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
  (premultiplied source over), inside its bounds and the clip of the
  layer it was started in (iced's layers do not clip each other, but a
  group stands for what that layer would have drawn).
- A **clip layer** (Flutter's `ClipPath` with `Clip.hardEdge`, which
  Skia applies without antialiasing): a subtree, in a stack of layers of
  its own as an opacity layer's, drawn in place exactly as it would be
  without the clip (its antialiased edges and all), after which every
  pixel of its bounds whose center the clip's path does not hold (nonzero
  winding) is put back as it was before it. Skia's hard clip kept each
  drawing off those pixels instead; with a clip that is all or nothing on
  each pixel, the two leave the same pixels. tiny-skia saves the pixels and
  puts them back on the CPU; wgpu copies them out of the target before the
  subtree and draws them back after it (a pass without blending,
  `restore`), discarding the pixels the clip holds.
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
CPU), the blur's horizontal pass kept in floats in both. Which pixels a
clip (a clip layer's, a backdrop filter's) holds is found once, for both,
by `iced_tiny_skia::HardClip::mask`: tiny-skia's fill without
antialiasing (Skia's, ported) of the path under the layer's transformation
and the scale factor, which wgpu uploads as an 8-bit texture and its
shaders discard by. A clip layer's subtree is drawn by each renderer's own
pipelines; tiny-skia draws a shape that crosses its layer's box (the clip's
bounding box, inside a clip layer) through a mask, which rounds within one
8-bit step of drawing it unmasked.

Moving to a new iced release: re-check whether upstream composites the
border over the fill, and drop that patch if it does; carry the effects'
layers over (upstream has neither), keeping the three renderers' drawing
loops as upstream's with the effects' branches added.

Moving iced_winit to a new release: carry `src/gesture.rs` and the marked
lines over; drop the logical pixel delta if upstream scales it, and the
whole patch if winit and iced come to pass the momentum phase and the
trackpad's touches on, and `drop_pointer` if winit comes to report the
pointer during a drag in (`draggingUpdated:`), and `drag_operation` if
its delegate comes to answer `draggingUpdated:` itself (the method is then
not added, and the badge is winit's again). Moving winit to a new
release: check that its view still lets touches through to the method
added to its class (CI's `macos_touches` test does), and whether its
`insertText:replacementRange:` still drops unmarked text outside a key
press (drop `insert_text` if it commits it).
