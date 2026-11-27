# Porting Vocara to Rust

This directory is the Vocara app, ported from its Flutter/Dart client
(`client/lib`, ~66k lines) to Rust with [iced](https://iced.rs) as the GUI,
plus formal verification in Lean 4 of the core algorithms (`verification/`).
The real-time audio engine was already Rust; it lives in this workspace
(`crates/vocara_engine`, moved from `client/rust` unchanged).

The port is complete: every Dart source in the table below is `done`, the
Dart unit and integration tests are ported with the code they test, and the
CI and release workflows build this app. The Flutter client has been removed
from the tree. It stays in the git history: its last commit is the parent of
`git log -1 --diff-filter=D --format=%h -- client/pubspec.yaml`. The `client/`
paths in this document, and the "port of `…dart`" notes throughout the code,
name files of that client.

## The rule: a faithful port

The Dart code is the specification. It encodes more than a hundred review
rounds of fixes, and most of its comments explain an edge case that once went
wrong. Port the behavior, not the shape:

- Every branch, guard, clamp, rounding rule and error path has a Rust
  counterpart. Nothing is "simplified" away. If a Dart comment explains why a
  line exists, the Rust line keeps an equivalent comment.
- Data compatibility is exact. The Rust app opens a library the Flutter app
  wrote, and the reverse:
  - the SQLite schema is `crates/vocara_store/src/schema_v2.sql`, dumped from
    drift (user_version 2), byte for byte; ids are checked to be 26..=36
    characters on insert, as drift did in code;
  - the asset pool layout, file names, sidecars and staging directories;
  - every JSON format (FX chains, sends, reverb, lyrics, settings, sync rows,
    quarantine payloads, migration markers) and every content hash
    (`SyncWriter.contentHashOf` canonicalization);
  - the HLC packed encoding, the ULID format, the oplog semantics;
  - the on-disk locations (application support directory per bundle id:
    `com.rutatang.vocara`, `.dev`, `.test`; the Flutter release's, like every
    Rust release's up to 0.3.0, inside its App Sandbox container, which
    `vocara_services::sandbox_migration` moves the library out of now that
    the app is unsandboxed), the diagnostics log format and rotation, the
    library lock file.
- The Dart unit tests are ported with the code they test, as Rust tests in
  the same crate. A test that cannot apply (a Flutter widget detail with no
  iced counterpart) is replaced by one that checks the same behavior through
  the Rust API.

## Layout

| Crate | Ported from | Depends on |
|---|---|---|
| `crates/vocara_engine` | the engine itself (`client/rust/vocara_engine`, moved as is) | — |
| `crates/vocara_model` | `lib/domain/models`, `lib/utils` (pure parts), `lib/config` (environment, app config) | — |
| `crates/vocara_store` | `lib/data/db`, `lib/data/sync`, `lib/data/repositories` | model |
| `crates/vocara_tract` | not Dart's: CHIMERA's anatomy, airway and head drawing (MIT, `LICENSE-CHIMERA`; its crate docs list the files), and the formant inversion | — |
| `crates/vocara_services` | `lib/data/services`, `lib/data/ffi` | model, store, engine, tract |
| `crates/vocara_app` | `lib/ui`, `lib/main.dart`, `lib/app.dart`, `lib/routing`, `lib/config/dependencies.dart`, `lib/utils/{command,busy_gate}` | all |
| `verification/` | Lean 4 models and proofs of the core algorithms | — |

## Conventions

- **Errors**: one `Error` enum per crate (`thiserror`), `pub type Result<T> =
  std::result::Result<T, Error>`. Dart's `Result<T>` (`utils/result.dart`)
  becomes `std::result::Result`. A Dart method that "never throws" stays
  infallible in Rust.
- **Concurrency**: the store and services are synchronous and `Send + Sync`
  (one SQLite connection behind a `Mutex`, as drift serialized its queries).
  The app runs them off the UI thread with iced `Task`s
  (`tokio::task::spawn_blocking`). Where Dart relied on single-isolate
  ordering (a save chain, a generation counter), Rust keeps the same ordering
  explicitly (a mutex, a sequence number), never by luck.
- **View models** are plain Rust state with methods; they do not depend on
  iced. Effects that need I/O are returned as values the app turns into
  `Task`s, so every view model is unit-testable without a window.
- **Audio**: `vocara_services::engine` wraps the engine's C ABI
  (`vocara_engine::ffi`, macOS + `device` feature) in a safe type, behind the
  same seams the Dart code had (`PlaybackEngine`, the mic and monitor
  interfaces), each with a null/fake implementation. Linux builds and every
  unit test run on the fakes, as the Dart unit tests did.
- **Style**: `cargo fmt`; `cargo clippy --workspace --all-targets -- -D
  warnings` clean; no `unwrap()` on input that comes from disk, the network
  or the user; `unsafe` only in the engine bridge, each block with a
  `// SAFETY:` comment.
- **Gates** (run from `rust/`): `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **One target dir per checkout**: never point two git worktrees of this
  workspace at the same `CARGO_TARGET_DIR`. Cargo names a workspace crate's
  artifacts by its path relative to the workspace, so the checkouts overwrite
  each other's builds and one can silently link the other's code. To save
  disk, build with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`.
- **macOS check from Linux**: the shipped build is macOS, with the `device`
  feature. `cargo check`/`clippy` never link, so a no-op C toolchain lets the
  C dependencies' build scripts pass and type-checks every macOS-only path:
  `CC_aarch64_apple_darwin=true AR_aarch64_apple_darwin=true
  CXX_aarch64_apple_darwin=true cargo clippy --workspace --all-targets
  --features vocara_app/device --target aarch64-apple-darwin -- -D warnings`,
  and the same for `x86_64-apple-darwin` (the slice macOS 10.15 runs).
- **macOS 10.15**, the Flutter app's minimum, is the app's too
  (`LSMinimumSystemVersion` in `packaging/macos/Info.plist.in`). No runner
  has it, so what keeps it is checked by reading the binary:
  - `packaging/macos/bundle.sh` builds the x86_64 slice with
    `MACOSX_DEPLOYMENT_TARGET=10.15` (arm64 at 11.0, the first macOS on
    Apple silicon);
  - nothing may link a framework 10.15 lacks. The file panels are
    `vocara_services::file_panels` on `NSOpenPanel` / `NSSavePanel` (rfd
    only off macOS), since rfd's macOS backend links
    UniformTypeIdentifiers (macOS 11). What the macOS build links (the
    link line of the `device` build), with the first macOS that had each:
    AppKit, ApplicationServices, AudioToolbox, Carbon, CoreAudio,
    CoreFoundation, CoreServices, Foundation, `libSystem`, `libobjc`
    (10.0); Security, `libiconv` (by 10.3); CoreVideo, QuartzCore (10.4);
    AVFoundation, CoreMedia (10.7); CoreGraphics (10.8 as a framework of
    its own, inside ApplicationServices before); Metal (10.11); ColorSync
    (10.13 as a public framework, inside ApplicationServices before: the
    current SDK's linker records it for winit's
    `CGDisplayCreateUUIDFromDisplayID`, which winit links through
    ApplicationServices, and CoreServices likewise for what it
    re-exports). The crates that link them
    are the `objc2-*` bindings, `winit`, `wgpu-hal` and `metal`,
    `core-graphics`, `core-foundation-sys`, `security-framework-sys`
    (native-tls), `sys-locale`, `block2`, `dispatch2` and `libc`;
  - every symbol it imports must be in 10.15. Checked against the macOS
    10.15 SDK's stubs (x86_64) and 11.0's (arm64), all are but
    `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap`
    (macOS 14.2), which cpal's loopback capture uses: the binary links
    CoreAudio weakly for them (`crates/vocara_app/build.rs`), and nothing
    calls them on macOS;
  - an API newer than 10.15 is called only behind `objc2::available!` or a
    `respondsToSelector:` check. Ours are all 10.14 or older; winit and
    wgpu-hal check the OS version themselves;
  - CI's `macos-build` job builds the bundle universal and runs
    `.github/scripts/macos-minimum-check.sh` on it: each slice's minimum,
    the frameworks and libraries against an allow-list of what 10.15
    ships, the two process-tap imports weak. The release workflow runs it
    on the release binary.
- **macOS GUI test**: `crates/vocara_services/tests/macos_gui.rs` (no
  harness: AppKit wants the main thread) drives AppKit for real: the menu
  bar's edit target, the delegate's secure restorable state and the
  intercepted `terminate:`. `cargo test` runs it on macOS, in both feature
  modes on CI, whose runner has a GUI session; on a Mac it opens two small
  windows for a few seconds, takes the keyboard and overwrites the
  clipboard. Elsewhere it prints a skip line.
- **Verified on CI's Mac**, beyond type-checking, by more tests of that
  kind (each prints every check it makes, and says plainly what it could
  not check there):
  - the open and save panels (`crates/vocara_app/tests/macos_file_panels.rs`):
    each opened through the app's own entry point (the lyrics import, the
    technique examples' browse, the settings' save panel asked from a
    worker) inside `-[NSApplication run]`, as iced runs the app; an open
    panel comes up as a sheet on the main window, the save panel
    application-modal, each with the call site's file types, selection
    mode and suggested name; Escape cancels it (the call site gets
    nothing), and a file chosen from the keyboard comes back to the call
    site exactly (an open panel: ⌘⇧G, the file's full path and Return in
    its Go to Folder sheet, then Return to open it; the save panel: a name
    typed over the suggested one, the folder through ⌘⇧G, Return); the
    main window is key again after. On macOS 26 a panel's UI is a remote
    view another process serves (`NSSavePanelService`), which neither the
    events the app posts to its own queue nor `ok:` (it raises "not
    implemented") reach, so the keys are typed through the window server
    (`CGEventPost` at the HID tap), as a keyboard's. That takes post-event
    access (TCC: Accessibility or PostEvent, asked of the process
    responsible for the test; GitHub's runner image grants Accessibility
    to the agent jobs run under): the run checks it first
    (`CGPreflightPostEventAccess`, never asking for it: the prompt would
    sit over the panels unanswered) and with it checks every step
    strictly. Without it the run says so, types nothing, closes each panel
    with an Escape posted in-app or `cancel:`, and lists every accept as
    NOT verified instead of failing;
  - ⌘Q with a take armed (`crates/vocara_app/tests/macos_quit.rs`): the
    hook `main` installs, the app's own quit guard (`App` with the real
    `ServiceQuitHooks` and `RecordingFinalizer::instance`) and a stand-in
    take registered as a recording registers. A posted ⌘Q goes through
    the menu's Quit to the intercepted `terminate:`: AppKit's own never
    runs (the app does not answer `NSTerminateLater`; it exits by itself),
    the app hears `QuitRequested`, the guard runs the finalizer, a second
    ⌘Q meanwhile is ignored, and the guard's `QuitGuardDone` (then
    `iced::exit`) comes only after the take's file landed; a ⌘Q past
    `REPEAT_WINDOW` (it waits the 65 s out, in the run without `device`)
    reaches AppKit's own `terminate:`, which asks the application
    delegate;
  - the trackpad's touches (`crates/vocara_app/tests/macos_touches.rs`):
    a winit window set up as the run loop sets one up takes indirect and
    resting touches and its view's class has the added
    `touchesBeganWithEvent:`; macOS's momentum (CGEvent scrolls handed to
    the view as AppKit hands them) reaches the run loop's embedder, and a
    touch sent to the view as AppKit sends one (an event answering
    `allTouches`, `timestamp` and `locationInWindow`: AppKit makes no
    touch event a program can) is recorded once, on the scrolls' clock and
    at its place, and is an inertia cancel 20 ms after the momentum's
    change but not again, not 60 ms after one, nor after a pan-zoom start;
  - the example players' AVFoundation backend, with `device`
    (`crates/vocara_services/tests/macos_media_player.rs`), on clips
    written at test time (a WAV tone by `audio_dsp::write_wav_mono`, a
    movie of solid red, green and blue seconds by `AVAssetWriter`): load
    and duration, play, pause, exact seek, the end and a replay, frames
    (size, fresh revisions, each second's colour in the right channel
    order), buffering and the buffered extent, and volume and mute read
    back from the `AVPlayer` (mpv's cube law, clamped; mute keeps the
    volume). An audio item's clock needs an output device: on a Mac with
    none, the tone's transport is skipped, said so; the movie's runs on
    the host clock either way;
  - the system UI font's faces (`ui::core::system_font`, its macOS test
    for every canvas label's style, and `text_field::toolbar_face`'s for
    the text toolbar's labels): asks CoreText and AppKit (linked by the
    tests alone) for the fonts Flutter's Skia made for each label style,
    step for step (the family of
    `CTFontCreateUIFontForLanguage(kCTFontUIFontSystem, 14)`, its matching
    descriptors and Skia's style of each from their traits through AppKit's
    `NSFontWeight` constants, the one Skia matched to the style's weight
    made at size 0, Skia's exact copy of it at the style's size with its
    optical size and no tracking), prints every descriptor, the sizes,
    files and variations, and checks the labels against them: the named
    instance's weight the port took is Skia's of the descriptor matched, no
    fake bold, the same file (`SFNS.ttf`), every axis of the instance they
    are drawn at, each glyph's advance and face (no fallback), what the
    shaper adds to it against HarfBuzz at the typeface's own variation, and
    the line (CoreText's ascent, descent and leading, and SkParagraph's
    line from them against the one cosmic-text lays the labels out on).
- **Commits**: small and frequent, on the working branch, with the session's
  trailer lines. CI is not run until the whole port is done.

## The iced application

The app is one iced `application` (iced 0.14, the Elm architecture). The
structure every screen follows, so screens ported by different hands fit:

- **Root** (`vocara_app::app`): `App` holds the `Services` (the port of
  `config/dependencies.dart`'s providers: `Arc`s of the store, repositories,
  settings, update check, media and analysis services), the router and the
  active screen's state. `Message` is one enum with a variant per screen
  (`Message::Home(home::Message)`, ...) plus app-wide ones (window events,
  toasts, ticks). `update` returns `Task<Message>`; `subscription` carries the
  window close request, keyboard, ticks and file drops.
- **Routing** (`vocara_app::router`): `Route` mirrors `routing/routes.dart`
  (`Home`, `Song { id }`, `Settings`, `Techniques`, `Technique { id }`); the
  router keeps go_router's push/pop/go semantics (the back button, deep links
  via `VOCARA_START`, the `START` dart-define's replacement). Every page sits
  under the window drag bar (`_chrome`). A route a widget pushed on the
  root navigator over everything (media_kit's fullscreen video) is the top
  page's `Screen::overlay`, which the app draws over the drag bar, the
  pages and its snack bar, under the dialogs.
- **Screens**: `vocara_app::ui::<screen>` with `view_model.rs` (or a
  `view_model/` folder) holding the Dart view model's state and logic as
  plain Rust with no iced types, and the widgets/views next to it. A view
  model method that did async work in Dart returns an `Effect` (or takes a
  closure) that the screen's `update` turns into a `Task` running the
  service call on a blocking thread; the result comes back as a `Message`.
  Every Dart `notifyListeners` becomes "state changed; iced redraws".
  Generation counters, `_disposed` guards and save chains keep their exact
  semantics: a late result for a screen that is gone, or a superseded load,
  is dropped exactly where Dart dropped it. A screen draws its snack bars
  while it is on top and hands them to the app when it stops being on top
  (`Screen::take_snack_bars`), which keeps them up over the page on top for
  the time they have left, as Dart's `ScaffoldMessenger` above the pages
  did (`app::messenger`). A snack bar can be dragged down and away with the
  mouse or a trackpad's two-finger swipe, as `SnackBar`'s `Dismissible`
  let it be (`widgets::dismissible`): once it has slid out it reports so,
  and its queue takes it down at once and lets the next one enter
  (`SnackQueue::swiped`).
- **Widgets**: `vocara_app::ui::core` holds the theme (`studio.dart` colors,
  `theme.dart`) and the shared widgets (`studio_button`, `studio_controls`,
  `confirm_dialog`, `fatal_open_screen`, `waveform_painter`,
  `spectrogram_painter`, `window_drag_bar`, ...). Painters are
  `canvas::Program`s; controls that need custom input (faders, knobs, drag
  handles) are custom widgets or canvases with the same hit areas, drag
  rules, wheel/trackpad handling and double-click resets as the Dart ones.
  Every dismissible modal barrier (the dialogs', the note editor's and
  technique picker's, the popup menus') dismisses as `ModalBarrier` did:
  on a tap's release, of any button, within `kTouchSlop` of its press
  (`widgets::gesture::AnyTap`), not on the press.
  Icons are Lucide (forui's `FIcons`) from the `lucide-icons` crate. The
  Material icons a Flutter widget drew on its own — the license page's
  `BackButton` arrow (`Icons.arrow_back_ios_new_rounded`), and media_kit's
  video controls' `volume_up` / `volume_down` / `volume_off` /
  `fullscreen` / `fullscreen_exit` (`ui::core::material_icons`) — are
  drawn from their Material Icons outlines (Apache 2.0), and the controls'
  `AnimatedIcons.play_pause` from Flutter's keyframes (BSD-3-Clause), both
  listed on the license page.
- **Renderer**: the macOS app draws with iced's wgpu renderer, patched in
  `rust/vendor/iced_wgpu` (see `rust/vendor/README.md`) to draw a quad's
  border over its fill, as Flutter's `BoxDecoration` (and iced's
  tiny-skia renderer, which the Linux tests draw with) do: upstream drew it
  in place of the fill, so a translucent border (a hovered card's coral at
  60 %) showed the page behind the card. Both renderers (`rust/vendor`'s
  `iced_wgpu` and `iced_tiny_skia`, reached through `iced_renderer`'s
  `Effects`, which `iced::Renderer` implements) composite what iced has no
  primitive for, as Flutter's layers did: a subtree drawn offscreen and
  composited at an opacity (`widgets::opacity_layer`, Flutter's `Opacity`
  and `FadeTransition`), a subtree cut by a hard-edged clip of any path
  (Flutter's `ClipPath` with `Clip.hardEdge`: drawn as without it, then
  every pixel whose center the path does not hold put back as it was), and
  a backdrop blurred and color-filtered inside a hard-edged clip
  (`BackdropFilter`), in the same arithmetic and with the same pixels held
  by a clip (both take them from `iced_tiny_skia::HardClip`), so both draw
  the same pixels. Text always uses the bundled Inter
  (`theme::font::install`), never a copy installed on the machine; only
  the labels Flutter set in the system UI font (each `TextPainter`'s,
  whose style named no family and which inherits no theme: the text
  toolbar's and the canvas labels) take that face on macOS, each weight
  drawn at the instance of its variation axes Flutter's Skia drew
  (`ui::core::system_font`: the named instance Skia matched to the weight,
  w600 Semibold at `wght` 590, at the 12 pt optical size, untracked;
  cosmic-text is vendored for it, and for SkParagraph's lines,
  `rust/vendor/README.md`), and in Inter at their weight off macOS (see
  **Text**).
- **Wheel and trackpad** (`widgets::pan_zoom`): Flutter's macOS embedder
  delivered a scroll with gesture phases (a trackpad's two fingers, a Magic
  Mouse's) as a pan-zoom gesture, hit-tested at its start, which drag
  recognizers took as a drag and flung on their own simulation when the
  fingers lifted; it dropped the momentum macOS sends after; and it
  delivered a scroll without phases (a mouse wheel, a precise device that
  sends none) as a scroll signal. winit merges the momentum phase into the
  touch phase and iced drops both, so the app's run loop is iced_winit
  patched (`rust/vendor/iced_winit`): it reads each wheel event's phases
  off the event AppKit is dispatching as it converts it, runs the
  embedder's state machine over them, and hands each wheel event to the
  widgets on its own with its gesture (`iced_winit::gesture::current`); it
  also hands precise deltas over in logical pixels (winit's are physical:
  iced scrolled twice the fingers' travel on a Retina screen). winit reads
  no trackpad touches either, from which the embedder made an inertia
  cancel (`PointerScrollInertiaCancelEvent`: a finger touching the
  trackpad within 50 ms of the momentum's last change, once per momentum,
  none after a pan-zoom start): the run loop gives each window's view the
  indirect and resting touches Flutter's view took and its class a
  `touchesBeganWithEvent:` that records them (`gesture::accept_touches`),
  runs the embedder's rule over each, and hands a cancel to the widgets
  as a `WheelScrolled` of no delta hit tested where the touch was. The
  widgets that Flutter's gestures told apart follow them: a snack bar's
  swipe, the license sheet's drag and fling, a single-line field's
  horizontal drag and fling, the timeline's pan, the scrollbar's track,
  the video's volume wheel and the EQ's Q wheel, and every scroll view
  (`widgets::fading_scrollbar`'s `scrollable`, which all of the app's
  are made with): a swipe that starts over one drags it 1:1 along its
  axis (alone in the gesture's arena at once, else past 18 px before the
  other drags its start reached, the innermost first on a tie:
  `pan_zoom::enter`), the fingers lifting fling it on
  `ClampingScrollSimulation`, the momentum reaches nothing, a scroll
  signal scrolls the innermost view it would move by its delta (40 px a
  line, as the embedder sent it) at once, and a new swipe on the view, a
  signal that moves it or an inertia cancel over it stops a fling (a
  cancel stops every view under the pointer, nested ones too, the
  license sheet's own fling and a single-line field's, where their last
  frame left them; one over a snack bar reaches nothing beneath). iced's
  own scrollable inside never scrolls by a wheel event: its content is a
  probe that keeps the event from it once the content has seen it. Off
  macOS no phases are known: every wheel event is a signal, and there are
  no touches.
- **Text**: every text is shaped fully and kerned, as Flutter set it: the
  workspace enables iced's `advanced-shaping` feature, which makes
  `Shaping::Advanced` every text's default (iced's own default shapes ASCII
  glyph by glyph, unkerned). A paragraph is shaped when its text changes,
  not per frame; laying a label out takes about 20 µs shaped fully against
  10 µs glyph by glyph in a release build (`ui::core::theme`'s ignored
  `shaping_cost` measures it). Text with a `letterSpacing`, `fontFeatures:
  [FontFeature.tabularFigures()]` or an `overflow: TextOverflow.ellipsis` is
  `studio_text` (`.letter_spacing(px)`, `.tabular()`,
  `.ellipsis(max_lines)`), which lays its own cosmic-text buffer out as
  SkParagraph did; a canvas label with spacing or tabular figures is drawn
  with `studio_text::fill_spaced_text` / `fill_tabular_text`. Every line
  is laid out on SkParagraph's (`ui::core::text`, the vendored
  cosmic-text: `rust/vendor/README.md`): a run's ascent and descent are
  its font's, scaled together to the style's `height` when it has one
  (`TextLeadingDistribution.proportional`, every text's), a line's are its
  runs' largest, its height is their sum rounded to the nearest pixel, its
  glyphs' baseline their ascent below its top rounded to a pixel, each
  line lies under the rounded lines above it (so a text is as tall as its
  lines), an empty paragraph is measured by its font's metrics scaled to
  their rounded sum, and each glyph's baseline is put on the nearest
  device pixel, half up. So iced's `text`, `studio_text`, canvas text and
  the fields sit where Flutter's did, and a box sized by its text (a row,
  a button, a field: `ui::core::text::line_box`, `RenderEditable`'s
  `preferredLineHeight`) is as tall as Flutter's. A `Text` takes the
  Material theme's height 1 (`crate::ui::core::text`, and every line
  height the port sets for one: the transport bar's counter and the
  faders' readouts had iced's 1.3 or a natural 1.21 before); a
  `TextPainter`'s style has none, so its labels are `text::NATURAL` (a
  line height of zero, as SkParagraph takes a multiplier of zero: the
  font's own ascent and descent, its line gap split above and below:
  `hhea`'s, as CoreText's leading, whatever `OS/2` asks for). Not
  identical: a line whose ascent and descent add up to exactly half a
  pixel rounds the way their last bit says, which cosmic-text computes as
  CoreText did (font units scaled in `double`, made `float`s) but cannot
  check against CoreText off a Mac, and a field's empty text is a line
  tall where `RenderEditable` took an empty paragraph's height or its
  hint's (the same for every field the app has). The `TextPainter` labels
  (no family, no theme) are set as Flutter set them, in the system UI font
  (`ui::core::system_font`: on macOS that face at the instance Skia drew
  each weight at, its glyphs, advances and line checked against CoreText
  on a Mac; off macOS Inter at the weight); the time ruler's tabular
  figures are the font's own `tnum` figures, set in a copy of the face
  whose every run takes the feature (`studio_text::tabular_font`), as a
  canvas's text carries no OpenType features.
- **Text fields** (`widgets::text_field`): iced's `text_input` and
  `text_editor` keep their caret, focus and blink in private state and draw
  a 1 px square caret in the text's color, under the text, so the app forks
  them (iced_widget 0.14.2, MIT; their keys, mouse, clipboard and input
  method unchanged but where noted) for what Flutter's macOS `TextField` did: its
  caret (2 px, round ends, `cursorColor`, a pixel above and below the line,
  2 device pixels left of the glyph edge, never left of the text, on a
  device pixel, over the text, 500 ms on and off, clipped to the field only
  while its text overflows) and its right-click menu (the word under the
  click selected unless it was on the selection, and the desktop Cupertino
  toolbar there with what macOS offered: Cut, Copy, Paste, no Select All).
  A multi-line field wraps 3 px short of its width (the caret's room),
  takes a wheel event only while its text overflows, and then a swipe
  drags its text with the fingers in whole lines (iced's editor scrolls
  by lines; Flutter's also flung it when they lifted); one
  read-only is `SelectableText` (it selects and copies, shows no caret,
  and keeps its own selection). A single-line field's text, when wider
  than the field, scrolls as `EditableText` scrolled it
  (`text_field::input::scroll`): only as far as shows the caret, over
  100 ms (`Curves.fastOutSlowIn`) after an edit, a click or the focus
  coming, at once for a keyboard move or a drag of the selection; it stays
  where it is while the field is unfocused, Home and End only scroll on
  macOS (`scrollToBeginningOfDocument:`), a scroll signal's horizontal
  delta scrolls it, and a trackpad's two-finger swipe across drags and
  flings it (past 18 px, before a scroll view's vertical drag around it
  takes the swipe down; the field's drag is there only while its text
  can scroll). As a field reveals its caret, the scroll
  view around it (a `.fading()` one) shows the caret too
  (`text_field::show_on_screen`, `RenderObject.showOnScreen`): with the
  field's 20 px `scrollPadding` (24 below) over the same 100 ms after an
  edit or the focus coming, bare and at once for a keyboard move. A press
  of any button outside a field
  unfocuses it, as `TextFieldTapRegion` did. Tab and Shift+Tab move the
  focus to the next and the previous field (`widgets::focus_traversal`):
  within the top page, a dialog or an overlay holding the keyboard (each a
  focus scope, going round at its ends), in reading order, a single-line
  field they land on selecting all its text (`selectAllOnFocus` on
  macOS) and the scroll view around it showing it. The fields sit forui's
  `contentPadding` less compact density's inside their wells
  (`theme::FIELD_PADDING`: 10 across, 6 down), and a bordered container's
  padding counts its 1 px border, as a Flutter `Container`'s did. The
  caret's device pixels are the window's scale factor
  (`ui::core::scale_factor`, which the scrollbar's tolerance reads too),
  which the app asks for when its window opens and the widgets that read
  it follow when the window is rescaled.
- **Window** (macOS): hidden title bar with a transparent, full-size content
  view; the drag bar starts a window drag; the close request runs the quit
  guard (kill separation sidecars, finalize recordings) before closing, as
  `main.dart`'s `_QuitGuard` and exit listener did.
- **Animations**: `vocara_app::ui::core::motion` holds Flutter's curves
  (with its cubic solver), the implicit animation every `Animated*` widget
  ran (`Implicit`) and an `AnimationController` (`Controller`), both on
  `iced::Animation`, each reading takes the time. The implicit widgets are
  `widgets::animated` (`AnimatedContainer`, and `Animated`, which builds
  its content from an animating value): they keep their animation in the
  widget tree, take their time from `RedrawRequested` and ask for the next
  frame only while they move; so do the widgets that port Material's own
  motion (`material_tooltip`, `fading_scrollbar`, `material_slider`,
  `material_switch` (Material 3's switch as forui themed it, read off
  Flutter 3.38's `switch.dart`), the `InkWell` highlights of
  `animated::ink_hover` (`ink_well` for a button's own overlay colors),
  a snack bar's swipe (`dismissible`, with Flutter's velocity tracker and
  fling spring), the FX chain's reordering cards, the license page's
  draggable sheet). A model that animates (the snack bars, `ui::snack`;
  a page transition and a dialog coming in or going, the app's and the
  pages' that draw their own; the license page; a lyrics list's scroll)
  subscribes to `motion::frames(animating)`, the window's frames only
  while something moves: an idle app does not redraw. The state never
  waits for an animation; only its drawing does. What Flutter faded as a
  layer (a dialog card, a floating snack bar, a tooltip, a toolbar item, a
  license detail, the video controls, a disabled drop zone, fader or play
  disc, a dragged lane) fades as one (`widgets::opacity_layer`): where its
  parts overlap, the topmost shows at the fade's opacity; what Flutter
  painted with a faded color (a barrier, a scrollbar, an ink highlight)
  fades its color. A sliding page moves its layout
  with its drawing (`widgets::slide`), so a tap lands where it is drawn.
- **Scrollbars**: a vertical scrollable that Material's desktop scroll
  behavior gave a scrollbar takes `.fading()` (`widgets::fading_scrollbar`),
  which hides iced's own scrollbar and draws and drives Material's itself,
  as Flutter 3.38's `ScrollbarPainter` and `RawScrollbarState` did: an
  8 px thumb 2 px in from the edge, its share of the view's height but at
  least 48, in `onSurface` at 30 %, 65 % hovered, 75 % dragged
  (`theme::scrollbar`); a drag of it scrolls in proportion, a press on its
  track pages 0.8 of the view in 100 ms; it fades in on a scroll or under
  the pointer and out 600 ms after, and while it shows its 12 px track is
  its own, not the content's: a mouse wheel there scrolls the view and
  never the content under it, a trackpad's swipe that starts there
  scrolls nothing (Flutter's pan-zoom never reached the view's drag; one
  that starts on the content stays the view's), and the bar notices
  after each frame, not only on a move, that the view left a still
  pointer. A horizontal scrollable (`.horizontal()`: Material gave it
  none) and a `ScrollConfiguration(scrollbars: false)` one
  (`fading_scrollbar::no_scrollbar`) have no scrollbar: nothing is drawn
  and nothing takes the pointer at their edge; they drag, fling and take
  the wheel as any scroll view.
- **Tests**: view models are unit-tested directly (the Dart widget tests
  mostly assert view-model behavior; port those assertions); screens get
  `iced_test` simulator tests where a Dart widget test checked what is on
  screen or what a click does.

## Verification (Lean 4)

`verification/` is a Lake project. Each core algorithm gets a Lean model that
mirrors the Rust function line for line, theorems about it, and a generator
that exports test vectors; a Rust test replays them against the Rust code, so
the proofs are about the code that ships, not a lookalike. Targets:

- HLC: `tick` and `merge` are strictly monotone; the packed string order
  equals the clock order.
- Entity LWW / reconcile: the merge is commutative, associative and
  idempotent, so replicas that saw the same rows converge.
- ULID encoding: order-preserving and injective.
- Version comparison (`isNewer`): a strict order; a prerelease never wins.
- Content hash canonicalization: invariant under map key order.
- The SPSC ring's index arithmetic: FIFO, never over capacity.
- Reconcile's three-way rules: an untouched row is never reverted, a row the
  editor never held is never deleted.
- The byte-budgeted LRU trim: fits or keeps only the inserted key, evicts the
  least recently used first.
- The oplog's asset gate: an ack or a boot compaction never changes what a
  read resolves a surviving row's dependency to.
- The lyrics core: `parse_lrc` of `to_lrc` gives back the timed lines,
  trimmed and rounded down to the centisecond, in stable time order, and a
  second save changes nothing; `align_lines` matches only equal lines, in
  order, and within its cell limit as many as their longest common
  subsequence.

`verification/README.md` lists each module, its theorems and its replay.

## Deliberate deviations from the Dart

Where a proof or a ported test showed the Dart behaviour to be wrong, the Rust
fixes it rather than port the bug, and says so where the fix lives:

- The oplog carry (`OplogDrain::ACK_CARRY_SQL`,
  `Persistence::COMPACT_CARRY_SQL`) looks back no further than a row's latest
  tombstone, as the drain's collapse does. The Dart carry inherited a deleted
  insert's asset dependency across the tombstone onto a revive, which could
  hold the revive, and through the ack watermark the whole ledger, forever
  (`verification/VocaraVerify/Oplog.lean`, `carryOld_breaks_the_gate`). Only
  the local ledger is affected; the data stays compatible both ways.

Where iced or the platform layer has no counterpart, the difference is kept
small and written down in the module that has it:

- Touch-only gestures (pointer cancel, a second finger) do not exist on
  iced's desktop pointer.
- Tab stops on text fields alone (`focus_traversal`). Flutter's also
  stopped on Material's focusable widgets, all on screens without a text
  field (the settings rows, slider and switch, the license page's back
  button and list tiles, the tag and track menus' items), drawing a focus
  highlight that Space or Enter activated; iced's buttons, sliders and
  switches take no keyboard focus, so Tab does nothing there.
- A press on a shown scrollbar's track pages at once, where Flutter's tap
  waited out the gesture arena (to the release, or 100 ms) when a
  recognizer above the view wanted the press too: iced has no arena
  (`fading_scrollbar`).
- A text field's right-click toolbar (`text_field::toolbar`, its
  superellipse corners, its shadow's spread, its blurred and saturated
  backdrop and the hard-edged clip of its box and items Flutter's) draws
  its shadow faded on iced's curve, not a Gaussian, and its fill, border
  and items with iced's renderers' antialiasing, not Skia's, onto the
  window where Skia drew them onto the backdrop filter's layer (each
  renderer's rounding apart, the same values). Its labels are in the
  macOS system UI font at the instance Flutter's Skia drew it at (the
  12 pt optical size, weight 400, no tracking) on SkParagraph's
  whole-pixel line, the same outlines at the same places to within
  0.01 px, but rasterized by swash, not CoreText (`text_field::toolbar_face`).
- The canvas labels (every `TextPainter`'s: the time ruler, the EQ's
  decades, the pitch lane's, the vowel chart's, the formant markers, the
  voiceprint's frequency axis, the voice profile's range bar) are in the
  macOS system UI font at the instance Skia drew each weight at
  (`ui::core::system_font`), on SkParagraph's line, rasterized by swash,
  not CoreText; the time ruler's tabular figures are the font's own
  `tnum` figures (`studio_text::tabular_font`).
- Pixels are rasterized by iced's renderers, not Skia: glyph images by
  swash, where Skia on macOS drew them with CoreText and its own gamma and
  contrast tables, and shape edges with `iced_wgpu`'s shaders (tiny-skia's
  antialiasing in the Linux tests), where Skia's GPU backend computed its
  own coverage. Every shape, size, glyph, advance, line and position is
  Flutter's; an edge's or a glyph's antialiased pixels can differ by a few
  levels. This is the renderer the port is built on, kept by decision:
  rasterizing glyphs with CoreText and porting Skia's edge coverage were
  weighed and not taken.

## Design refresh

The app's visual details now follow `rust/DESIGN.md` (the design system,
drawn from the approved mockups in `rust/design/mockups/`), deliberately
departing from the Flutter app's look: its colours, type sizes, radii,
control shapes, icons and chrome. Behaviour, data and layouts do not change:
every feature, control, keyboard shortcut, motion timing and the data shown
are the Flutter app's, as the rest of this document describes. Where a
"port of …" note or a comment in the code says a colour, size or shape
matches Flutter, `DESIGN.md` wins.

What the refresh puts in place, for every screen to build on:

- **Tokens** (`ui::core::theme`): the `Studio` palette at `DESIGN.md`'s
  values with its new tokens (`TOOLBAR`, `PANEL_SELECTED`, `CONTROL`,
  `CONTROL_ACTIVE`, `BORDER_SUBTLE`, `TEXT_DISABLED`, `WARN`, the track
  colours), and the `type_scale`, `radius` and `height` scales. The chrome
  (dialogs, menus, tooltips, text fields, toasts, buttons) draws with
  them; forui's `Zinc` palette stays only for the looks that copy a third
  party's (the text field's Cupertino menu, the licence page's Material
  list, the Material switch and scrollbar).
- **Controls** (`ui::core::widgets::controls`, `line_icon`): the icon
  button, toolbar group, ghost, pop-up and dialog buttons, segmented
  control, tab strip, thin slider, parameter row, power button and the
  M / S / R buttons, the mixer's fader and meter painters, and line icons
  drawn to one optical size (equal visible extent and stroke across a row;
  the settings gear at 75 %). `cap_label` reads in sentence case.
- **The unified toolbar** (`ui::core::widgets::toolbar`): the Flutter app's
  28 px drag bar over a 56 px page header is now one 52 px toolbar per
  page, which is the window's title bar. On macOS the window takes an
  empty unified `NSToolbar` once it opens
  (`vocara_services::window_toolbar`; the unified style from macOS 11,
  asked with `respondsToSelector:`), so the traffic lights sit centred in
  it; a page's toolbar starts 80 px in to clear them. The toolbar's empty
  area drags the window and a double click zooms it, as the drag bar did;
  the environment badge sits at its right end.
- **The library** (`ui::home`): the cards' waveform band reads the
  waveform cache (`SongPracticeRepository::list_practice_previews`) off
  the UI thread after each list read; until it lands, the band is empty.

## Tracking

Every Dart source and where it lives in Rust. A row is `done` only when its
behavior and its tests are ported and the gates pass.

| Dart source (client/lib/) | Lines | Rust home | Status |
|---|---:|---|---|
| `app.dart` | 85 | `vocara_app::app` (`MaterialApp`'s `ScaffoldMessenger` is `app::messenger`: a page hands its snack bars to the app when it stops being on top, and the app draws them over the page on top for the time they have left) | done |
| `config/app_config.dart` | 16 | `vocara_model::config::app_config` | done |
| `config/dependencies.dart` | 109 | `vocara_app::services` (`Services`, `create_source_separation_service`) | done |
| `config/environment.dart` | 59 | `vocara_model::config::environment` | done |
| `data/db/app_database.dart` | 180 | `vocara_store::db::schema` | done |
| `data/db/asset_store.dart` | 1322 | `vocara_store::db::asset_store` | done |
| `data/db/durable_fs.dart` | 119 | `vocara_store::db::durable_fs` | done |
| `data/db/library_lock.dart` | 318 | `vocara_store::db::library_lock` | done |
| `data/db/persistence.dart` | 1286 | `vocara_services::persistence` (it opens the store and drives services: audio edit, waveform, audio files) | done |
| `data/db/safe_migrator.dart` | 379 | `vocara_store::db::safe_migrator` | done |
| `data/db/tables.dart` | 347 | `vocara_store::db::{schema, rows}` | done |
| `data/db/write_gate.dart` | 92 | `vocara_store::db::write_gate` | done |
| `data/ffi/vocara_engine_bindings.dart` | 2051 | `vocara_services::engine` (with the decode policy, probe and device lists from the top of `graph_audio_playback_service.dart`: `engine::media`, `engine::enumerate_*`) | done (native calls type-checked for macOS; their tests run with `device`) |
| `data/repositories/song_practice_repository.dart` | 64 | `vocara_store::repositories::song_practice_repository` | done |
| `data/repositories/song_practice_repository_drift.dart` | 1678 | `vocara_store::repositories::song_practice_repository_drift` | done |
| `data/repositories/technique_repository.dart` | 52 | `vocara_store::repositories::technique_repository` | done |
| `data/repositories/technique_repository_drift.dart` | 607 | `vocara_store::repositories::technique_repository_drift` | done |
| `data/services/app_settings_service.dart` | 1031 | `vocara_services::app_settings_service` | done |
| `data/services/audio_dsp.dart` | 2663 | `vocara_services::audio_dsp` | done |
| `data/services/audio_edit_service.dart` | 863 | `vocara_services::audio_edit_service` (the `AudioEdits` trait is the view models' seam) | done |
| `data/services/audio_exporter.dart` | 173 | `vocara_services::audio_exporter` | done (the save panel is passed in by the app) |
| `data/services/audio_file_service.dart` | 362 | `vocara_services::audio_file_service` | done |
| `data/services/diagnostics_log.dart` | 440 | `vocara_model::diagnostics` (used by the store too) | done |
| `data/services/graph_audio_playback_service.dart` | 1533 | `vocara_services::graph_audio_playback_service` (with `SyncChain`; the top-level decode helpers are `engine::media`) | done |
| `data/services/local_storage_service.dart` | 110 | `vocara_services::local_storage_service` | done |
| `data/services/media_analysis_service.dart` | 112 | `vocara_services::media_analysis_service` | done (the default decoder is the engine bridge's; its tests run on injected decoders) |
| `data/services/mic_permission.dart` | 36 | `vocara_services::mic_permission` | done (macOS half type-checked; runs on CI) |
| `data/services/mic_recording_controller.dart` | 681 | `vocara_services::mic_recording_controller` (the start is `begin_start` on the UI thread + `PendingStart::run` on a worker; the owner drives the two timers with `tick`; the `MicRecorder` trait is the view models' fake seam) | done |
| `data/services/monitor_service.dart` | 262 | `vocara_services::monitor_service` | done |
| `data/services/playback_engine.dart` | 147 | `vocara_services::playback_engine` | done |
| `data/services/playback_engine_factory.dart` | 8 | `vocara_services::playback_engine_factory` | done |
| `data/services/recording_finalizer.dart` | 331 | `vocara_services::recording_finalizer` | done |
| `data/services/source_separation_service.dart` | 745 | `vocara_services::source_separation_service` | done |
| `data/services/spectrogram_service.dart` | 463 | `vocara_services::spectrogram_service` | done (the RSS measurements are ignored tests, run alone on Linux) |
| `data/services/technique_media_service.dart` | 103 | `vocara_services::technique_media_service` | done |
| `data/services/update_check_service.dart` | 285 | `vocara_services::update_check_service` | done |
| `data/services/waveform_service.dart` | 169 | `vocara_services::waveform_service` | done (decode supplied by the engine bridge) |
| `data/sync/hlc.dart` | 381 | `vocara_store::sync::hlc` | done |
| `data/sync/integrity_checker.dart` | 434 | `vocara_store::sync::integrity_checker` | done |
| `data/sync/oplog_drain.dart` | 191 | `vocara_store::sync::oplog_drain` | done |
| `data/sync/quarantine_store.dart` | 290 | `vocara_store::sync::quarantine_store` | done |
| `data/sync/reconcile.dart` | 227 | `vocara_store::sync::reconcile` | done |
| `data/sync/sync_applier.dart` | 1585 | `vocara_store::sync::sync_applier` | done |
| `data/sync/sync_engine.dart` | 1092 | `vocara_store::sync::sync_engine` | done |
| `data/sync/sync_hashes.dart` | 299 | `vocara_store::sync::sync_hashes` | done |
| `data/sync/sync_row.dart` | 618 | `vocara_store::sync::sync_row` | done |
| `data/sync/sync_state_store.dart` | 97 | `vocara_store::sync::sync_state_store` | done |
| `data/sync/sync_transport.dart` | 182 | `vocara_store::sync::sync_transport` (+ the test fake cloud) | done |
| `data/sync/sync_writer.dart` | 142 | `vocara_store::sync::sync_writer` | done |
| `domain/models/audio_device.dart` | 79 | `vocara_model::models::audio_device` | done |
| `domain/models/audio_fx.dart` | 874 | `vocara_model::models::audio_fx` | done |
| `domain/models/audio_track.dart` | 453 | `vocara_model::models::audio_track` | done |
| `domain/models/fx_bus.dart` | 138 | `vocara_model::models::fx_bus` | done |
| `domain/models/fx_presets.dart` | 505 | `vocara_model::models::fx_presets` | done |
| `domain/models/lyric_line.dart` | 42 | `vocara_model::models::lyric_line` | done |
| `domain/models/practice_note.dart` | 95 | `vocara_model::models::practice_note` | done |
| `domain/models/practice_summary.dart` | 41 | `vocara_model::models::practice_summary` | done |
| `domain/models/song_practice.dart` | 436 | `vocara_model::models::song_practice` | done |
| `domain/models/spectrogram.dart` | 220 | `vocara_model::models::spectrogram` | done |
| `domain/models/take_version.dart` | 49 | `vocara_model::models::take_version` | done |
| `domain/models/technique.dart` | 181 | `vocara_model::models::technique` | done |
| `domain/models/technique_analysis.dart` | 62 | `vocara_model::models::technique_analysis` | done |
| `domain/models/technique_marker.dart` | 115 | `vocara_model::models::technique_marker` | done |
| `domain/models/technique_media.dart` | 156 | `vocara_model::models::technique_media` | done |
| `domain/models/voice_metrics.dart` | 975 | `vocara_model::models::voice_metrics` | done |
| `main.dart` | 206 | `src/main.rs`, `vocara_app::app` (library-open and quit-hook seams until the store and services land) | done |
| `routing/navigation.dart` | 22 | `vocara_app::router` | done |
| `routing/router.dart` | 123 | `vocara_app::router`, `vocara_app::app` | done |
| `routing/routes.dart` | 23 | `vocara_app::router` | done |
| `ui/core/themes/studio.dart` | 45 | `vocara_app::ui::core::theme` (`Studio`) | done |
| `ui/core/themes/theme.dart` | 6 | `vocara_app::ui::core::theme` (`Zinc`, `app_theme`, styles) | done |
| `ui/core/widgets/cap_label.dart` | 21 | `vocara_app::ui::core::widgets::cap_label` | done |
| `ui/core/widgets/confirm_dialog.dart` | 284 | `vocara_app::ui::core::widgets::confirm_dialog` | done |
| `ui/core/widgets/fatal_open_screen.dart` | 276 | `vocara_app::ui::core::widgets::fatal_open_screen` | done |
| `ui/core/widgets/holds_keyboard.dart` | 43 | `vocara_app::ui::core::widgets::holds_keyboard` | done |
| `ui/core/widgets/spectrogram_painter.dart` | 946 | `vocara_app::ui::core::widgets::spectrogram_painter` (reads the models through local traits until they land) | done |
| `ui/core/widgets/stable_subtree.dart` | 66 | `vocara_app::ui::core::widgets::stable_subtree` | done |
| `ui/core/widgets/studio_button.dart` | 118 | `vocara_app::ui::core::widgets::studio_button` | done |
| `ui/core/widgets/studio_controls.dart` | 500 | `vocara_app::ui::core::widgets::studio_controls` | done |
| `ui/core/widgets/studio_hint.dart` | 74 | `vocara_app::ui::core::widgets::studio_hint` | done |
| `ui/core/widgets/tick_boundary.dart` | 25 | `vocara_app::ui::core::widgets::tick_boundary` | done |
| `ui/core/widgets/visible_drop_target.dart` | 80 | `vocara_app::ui::core::widgets::visible_drop_target` | done |
| `ui/core/widgets/waveform_painter.dart` | 457 | `vocara_app::ui::core::widgets::waveform_painter` | done |
| `ui/core/widgets/window_drag_bar.dart` | 60 | `vocara_app::ui::core::widgets::window_drag_bar` | done |
| `ui/home/view_model/home_view_model.dart` | 144 | `vocara_app::ui::home::view_model` | done |
| `ui/home/widgets/home_screen.dart` | 491 | `vocara_app::ui::home::home_screen` (its timers run as subscriptions while the page holds them; the "Loading practice" dialog and the toast draw inside the page, under the drag bar, the toast going to the app's `messenger` when a page covers Home; remote-applied events come from `Library::remote_applied`) | done |
| `ui/home/widgets/new_practice_dialog.dart` | 16 | `vocara_app::ui::home::widgets::new_practice_dialog` | done |
| `ui/home/widgets/practice_project_card.dart` | 126 | `vocara_app::ui::home::widgets::practice_project_card` | done |
| `ui/home/widgets/update_notice.dart` | 219 | `vocara_app::ui::home::widgets::update_notice` | done (the 5 s auto-check timer is the home screen's) |
| `ui/settings/licenses.dart` | 213 | `vocara_app::ui::settings::licenses` (the page too, as `LicensePage` laid it out on macOS under forui's Material theme, its registry gathered off the UI thread under a spinner and its detail on a `DraggableScrollableSheet` a trackpad grows and flings; what still differs is in the module docs; the crate list is the generated `assets/licenses/crates.json`: after a dependency change, run `VOCARA_UPDATE_LICENSES=1 cargo test -p vocara_app the_crate_license_list_is_current` and commit it) | done |
| `ui/settings/settings_screen.dart` | 1135 | `vocara_app::ui::settings::settings_screen` (no view model: the screen holds `_SettingsScreenState`'s state; the settings choices block on their persist, so they run off the UI thread one at a time, in order, and those still waiting when the page leaves are applied after it; the save panel, the bundle and the storage root are `SettingsHooks`; the license page and the toasts draw inside the page, a toast up as the page leaves going to the app's `messenger`) | done |
| `ui/song_practice_detail/view_model/analysis_cache.dart` | 1717 | `vocara_app::ui::song_practice_detail::view_model::analysis_cache` (the Dart awaits are a state machine: worker jobs come back through `job_done`, the joins and `Future.wait`s are continuations drained as microtasks; selection and transport come from an `AnalysisHost`; the capture pump is the screen's 30 ms tick) | done |
| `ui/song_practice_detail/view_model/analysis_source.dart` | 64 | `vocara_app::ui::song_practice_detail::view_model::analysis_source` (the listener is a `revision`; a seek is the view's message) | done |
| `ui/song_practice_detail/view_model/interpretations_controller.dart` | 296 | `vocara_app::ui::song_practice_detail::view_model::interpretations_controller` (resync, save and the guarded file clean-up are effects; the delete holds the busy gate throughout) | done |
| `ui/song_practice_detail/view_model/mixer_controller.dart` | 1271 | `vocara_app::ui::song_practice_detail::view_model::mixer_controller` (the view model's closures are a `MixerHost`; a saving edit returns a `SaveRequest`; the resync queues on the graph engine within the call and returns a `PendingSync`) | done |
| `ui/song_practice_detail/view_model/playback_controller.dart` | 439 | `vocara_app::ui::song_practice_detail::view_model::playback_controller` (the closures are a `PlaybackHost`; the engine's streams are delivered by `drain_engine_events`) | done |
| `ui/song_practice_detail/view_model/recording_controller.dart` | 1169 | `vocara_app::ui::song_practice_detail::view_model::recording_controller` (the closures are a `RecordingHost`; the sibling calls, the saves, the empty take's create and drop and the resyncs are `RecordingEffect`s answered by op; the mic's start runs on a worker and hands its take setup to the UI thread as a `TakeSetupRequest`; the finalize holds the busy gate from its read to its last save; the quit's finalizers post to the UI thread and wait (`set_quit_waker`, `serve_quit_requests`); the level tick, monitor meter and device watch are the screen's ticks) | done |
| `ui/song_practice_detail/view_model/song_practice_detail_view_model.dart` | 3337 | `vocara_app::ui::song_practice_detail::view_model::song_practice_detail_view_model` (each await on a repository, file, edit or engine call is a `Job` the screen runs and answers through `job_done`; the save chain is a queue whose first save starts as the call that queued it ends (Dart's microtask); the guarded edits take the busy gate and list it as landing from their start; awaited calls answer with a `Reply`; the controllers are reached through `*_do` / `with_*`, their effects run in place or passed on as the view model's; the screen keeps a disposed view model until `has_work_in_flight` is false) | done |
| `ui/song_practice_detail/widgets/analysis_lanes.dart` | 988 | `vocara_app::ui::song_practice_detail::widgets::analysis_lanes` (plain data in, `Message`s out named after the cache's `toggleLane` / `healLane` and the screen's add / edit callbacks; the heal-on-build is a `HealLane` after the frame, once per revision the screen passes; the spectrogram lane's image is a `LaneImage` the screen keeps per take and `sync`s; the marker lanes' range drag lives in the lane's canvas state, in times; `Placed` lays the take box and the markers out past the lane's edges, as `Positioned` did) | done |
| `ui/song_practice_detail/widgets/bottom_dock.dart` | 312 | `vocara_app::ui::song_practice_detail::widgets::bottom_dock` (plain data in — `BottomDock`, the open tab's content from `dock_content` — and `Message`s out; the post-frame heal is `heals`, asked by the screen after each update; the grabber reports each move with the content's room, and the screen sets the height from the height on screen, read per move; the handle and the content keep their slots while closed, so the tab strip keeps its scroll) | done |
| `ui/song_practice_detail/widgets/detail_transport_bar.dart` | 456 | `vocara_app::ui::song_practice_detail::widgets::detail_transport_bar` (plain data in — `TransportBar` — and `Message`s out named after the calls the bar made; the gating is `controls`; the counter is `studio_text` with tabular figures and letter spacing) | done |
| `ui/song_practice_detail/widgets/fx_panel.dart` | 1723 | `vocara_app::ui::song_practice_detail::widgets::fx_panel` (plain data in — `FxPanel`: the target's id and chain, the EQ's input column under the playhead, the compressor's gain reduction — and `Message`s out, one per view-model call; the panel keys its state by its target; the analyzer ticks on redraws and stops once settled, any redraw waking it; the GR meter keeps its 50 ms poll over the reading the host passed in; a drag or Q debounce in flight when the screen stops showing the panel owes its `CommitFx` to the screen; the studio menus are `widgets::mixer_shared::PopupMenu`) | done |
| `ui/song_practice_detail/widgets/left_panel.dart` | 1545 | `vocara_app::ui::song_practice_detail::widgets::left_panel` (plain data in — `Inspector`, `VoiceSummary` — and `Message`s out named after the view-model and mixer methods; the FX panel is a slot the screen fills with `fx_panel`'s view; the screen holds which chip menu is open, popped by `track_context_menu`'s host; the body is keyed by its channel; the post-frame heal is `Inspector::analysis_heal`) | done |
| `ui/song_practice_detail/widgets/live_spectrogram.dart` | 150 | `vocara_app::ui::song_practice_detail::widgets::live_spectrogram` (the raster and its image live in the canvas's tree state, remade when it draws and the monitor's revision has moved) | done |
| `ui/song_practice_detail/widgets/lyrics_panel.dart` | 1396 | `vocara_app::ui::song_practice_detail::widgets::lyrics_panel` (plain data in — `RightPanelData` — and `Message`s out; what the Dart widgets kept in `State` (the editor's text, the readers' latched line, the lists' scroll) is a `LyricsPanelState` the screen keeps and `sync`s after every update; `_importDrop` and Save's guards are `begin_import` / `end_import` and `save_ticket` / `save_landed` around the view-model calls; `RightTab` and `LyricsMode` are defined here; `ensureVisible`'s scroll animates on frames the screen feeds the panel while a list scrolls) | done |
| `ui/song_practice_detail/widgets/mixer_strip.dart` | 1185 | `vocara_app::ui::song_practice_detail::widgets::mixer_strip` (plain data in — `MixerStrip`, `MicInput` — and `Message`s out, one per view-model, mixer or settings call; the rename and delete-bus prompts are the host's (`RenameBus`, `DeleteBus`); strips keep their state by key (`widgets::mixer_shared::Keyed`); `defaultTakeColor` and the palette are copied here until `track_context_menu` lands) | done |
| `ui/song_practice_detail/widgets/note_editor.dart` | 195 | `vocara_app::ui::song_practice_detail::widgets::note_editor` (the screen holds the open `NoteEditor` and passes in the view model's `normalizeMarkerRange`; `update` returns Dart's result) | done |
| `ui/song_practice_detail/widgets/pitch_lane.dart` | 648 | `vocara_app::ui::song_practice_detail::widgets::pitch_lane` | done (tests on hand-built metrics until `computeVoiceMetrics` lands) |
| `ui/song_practice_detail/widgets/ruler_bar.dart` | 254 | `vocara_app::ui::song_practice_detail::widgets::ruler_bar` (plain data in — `RulerBar` — and `Seek` / `SetLoopStart` / `SetLoopEnd` out; the body and both handles are one canvas recognizing taps and horizontal drags; its header-column and margin constants mirror `track_row`'s) | done |
| `ui/song_practice_detail/widgets/song_practice_detail_screen.dart` | 2172 | `vocara_app::ui::song_practice_detail::widgets::song_practice_detail_screen` (the `Route::Song` page; every view-model `Effect` runs as a task and lands through the answer method it names, a `Reply` resuming what Dart awaited — a drop's next file, the technique list, the lyrics import and Save; after every message the screen drains the engine, asks the dock's and the inspector's heals, reads the FX panel's input and GR, syncs the spectrogram images and the lyrics panel, and commits a drag whose control left the screen; the ticks, the quit's waker, the engine's events and the snack bar's time are subscriptions; the snack bar up as the page leaves (a take's finalize toast at Back) goes to the app's `messenger`; the dialogs go through `Update::dialog`, the note editor and technique picker are drawn over the page; a page gone from the stack lingers in the app while `has_work_in_flight`; the geometry helpers are `widgets::timeline`) | done |
| `ui/song_practice_detail/widgets/technique_picker.dart` | 309 | `vocara_app::ui::song_practice_detail::widgets::technique_picker` (the screen holds the open picker, draws it over its page — under the drag bar, as Home's loading dialog — and hands it the library when the read lands) | done |
| `ui/song_practice_detail/widgets/track_context_menu.dart` | 640 | `vocara_app::ui::song_practice_detail::widgets::track_context_menu` (the menus are elements reporting actions; the screen holds which is open and wraps what was clicked in `menu_host`, which pops it at the press that opened it, or under its trigger; the rename and delete-bus prompts are `StudioPrompt` / `ConfirmDialog` values) | done |
| `ui/song_practice_detail/widgets/track_row.dart` | 1158 | `vocara_app::ui::song_practice_detail::widgets::track_row` (plain data in, `Message`s out; the screen wraps each row in `menu_host`, mounts the rows keyed by take id, and holds the reorder drag's lane and drop target; a clip's move-drag lives in the lane, keyed by clip id; the widget tests run on `widgets::live_ui`, which rebuilds the view between events) | done |
| `ui/song_practice_detail/widgets/voice_profile_view.dart` | 567 | `vocara_app::ui::song_practice_detail::widgets::voice_profile_view` (each tab a function of the `AnalysisSource`, an `Element` of the host's message: neither reports anything) | done |
| `ui/song_practice_detail/widgets/voiceprint_view.dart` | 2015 | `vocara_app::ui::song_practice_detail::widgets::voiceprint_view` (each tab is a function of the `AnalysisSource`; the Spectrogram tab's take image is a `VoiceprintState` the host keeps and `sync`s; a seek and the Take/Live toggle are `Message`s; the live panel's rounded clip draws its corners in the hosts' surface) | done |
| `ui/technique_library/technique_library_screen.dart` | 914 | `vocara_app::ui::technique_library::technique_library_screen` (the snack bars queue, timed by a subscription, and go to the app's `messenger` when a page covers the library or it leaves) | done |
| `ui/technique_library/view_model/technique_detail_view_model.dart` | 2013 | `vocara_app::ui::technique_library::view_model::technique_detail_view_model` (each awaited I/O is a `Job` the screen runs on a worker and lands, the code after the `await` a continuation; Dart's return values are `Answer`s; `micArmed` / `analysisScrub` are effects; the recorder's timers are the screen's ticks, which also carry a quit's `quitFinalize` from the finalizer thread; the screen API is in the module doc) | done |
| `ui/technique_library/view_model/technique_library_view_model.dart` | 280 | `vocara_app::ui::technique_library::view_model::technique_library_view_model` | done |
| `ui/technique_library/widgets/media/technique_audio_player.dart` | 417 | `vocara_app::ui::technique_library::widgets::media::technique_audio_player` (media_kit's `Player` → `vocara_services::media_player`, polled: AVFoundation on macOS with `device`, the null player elsewhere, a fake in tests; the keyed players a technique page keeps → `media::example_players`) | done (the AVFoundation backend played on CI's Mac with `device`, `vocara_services`' `tests/macos_media_player.rs`; its pure parts tested everywhere) |
| `ui/technique_library/widgets/media/technique_media_section.dart` | 898 | `vocara_app::ui::technique_library::widgets::media::technique_media_section` (plain data in — `TechniqueMediaSection`, the constructor's parameters — and `Message`s out, which `MediaSectionState::update` turns into the callbacks' `Event`s; the page keeps that state (hover, drag) and the `ExamplePlayers`; the picker is `vocara_services::file_panels`' (`browse`), the rename dialog goes through the app's dialog stack (`Dialog::RenameExample`)) | done |
| `ui/technique_library/widgets/media/technique_video_player.dart` | 190 | `vocara_app::ui::technique_library::widgets::media::technique_video_player` (the controls `AdaptiveVideoControls` gave it on macOS, media_kit_video 2.0.1's `MaterialDesktopVideoControls` with `modifyVolumeOnScroll: false` → `media::video_controls`, one widget drawing and handling them all; its fullscreen route → the page's `Screen::overlay` plus the window's native fullscreen; the controls' keyboard → the first video started, through `ExamplePlayers::shortcuts`) | done (what iced cannot do the same — focus scopes, rounded clips — is listed in `video_controls`' and the player's module docs) |
| `ui/technique_library/widgets/studio_field.dart` | 96 | `vocara_app::ui::technique_library::widgets::studio_field` | done |
| `ui/technique_library/widgets/technique_analysis_section.dart` | 690 | `vocara_app::ui::technique_library::widgets::technique_analysis_section` (`view` reads the view model, `update` makes the call the Dart tap made where the control was live; the page keeps the Spectrogram tab's `VoiceprintState` and asks `ensureReferenceAnalysis` while the panel is built; the Dart level meter's fill had no height, here it shows) | done (the Range and Dynamics tabs are placeholders until `voice_profile_view` lands) |
| `ui/technique_library/widgets/technique_detail_screen.dart` | 825 | `vocara_app::ui::technique_library::widgets::technique_detail_screen` (the router's `technique_library::detail`; the screen owns the view model, the `ExamplePlayers`, the `MediaSectionState` and the analysis card's `VoiceprintState`; each `Job` runs off the UI thread and lands as `Message::Landed`, each `Answer` is a Dart `await` resuming; the recorder's ticks — and a quit's `quitFinalize` — and the players' poll are subscriptions; the dialogs go through `Update::dialog`; the header's double-tap settle drops its buttons' messages; the snack bars are the page's while it is on top (queued, timed by a subscription), the app's `messenger`'s once it leaves; exports use `file_panels`' save panel, the players `SystemMediaPlayers` — `with_deps` takes a test's) | done |
| `ui/technique_library/widgets/technique_dialogs.dart` | 117 | `vocara_app::ui::technique_library::widgets::technique_dialogs` | done |
| `utils/analysis_gate.dart` | 54 | `vocara_model::util::analysis_gate` | done |
| `utils/busy_gate.dart` | 83 | `vocara_app::util::busy_gate` | done |
| `utils/command.dart` | 71 | `vocara_app::util::command` | done |
| `utils/export_copy.dart` | 153 | `vocara_model::util::export_copy` | done |
| `utils/format.dart` | 13 | `vocara_model::util::format` | done |
| `utils/id.dart` | 87 | `vocara_model::util::id` | done |
| `utils/lru_bytes_cache.dart` | 242 | `vocara_model::util::lru_bytes_cache` | done |
| `utils/lyrics_format.dart` | 223 | `vocara_model::util::lyrics_format` | done |
| `utils/result.dart` | 32 | `(std Result; no port)` | done |
