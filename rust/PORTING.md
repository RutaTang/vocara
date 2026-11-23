# Porting Vocara to Rust

This directory is the Rust port of the Vocara app: the Flutter/Dart client
(`client/lib`, ~66k lines) rewritten in Rust with [iced](https://iced.rs) as
the GUI, plus formal verification in Lean 4 of the core algorithms
(`verification/`). The real-time audio engine is already Rust
(`client/rust/vocara_engine`) and is used as it is, through a path dependency.

The Flutter app keeps building and shipping until this port reaches parity;
then the engine moves into this workspace, `client/` is retired, and the CI
and release workflows build this app instead.

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
    `com.rutatang.vocara`, `.dev`, `.test`), the diagnostics log format and
    rotation, the library lock file.
- The Dart unit tests are ported with the code they test, as Rust tests in
  the same crate. A test that cannot apply (a Flutter widget detail with no
  iced counterpart) is replaced by one that checks the same behavior through
  the Rust API.

## Layout

| Crate | Ported from | Depends on |
|---|---|---|
| `crates/vocara_model` | `lib/domain/models`, `lib/utils` (pure parts), `lib/config` (environment, app config) | — |
| `crates/vocara_store` | `lib/data/db`, `lib/data/sync`, `lib/data/repositories` | model |
| `crates/vocara_services` | `lib/data/services`, `lib/data/ffi` | model, store, engine |
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
- **macOS check from Linux**: the shipped build is macOS, with the `device`
  feature. `cargo check`/`clippy` never link, so a no-op C toolchain lets the
  C dependencies' build scripts pass and type-checks every macOS-only path:
  `CC_aarch64_apple_darwin=true AR_aarch64_apple_darwin=true
  CXX_aarch64_apple_darwin=true cargo clippy --workspace --all-targets
  --features vocara_app/device --target aarch64-apple-darwin -- -D warnings`.
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
  under the window drag bar (`_chrome`).
- **Screens**: `vocara_app::ui::<screen>` with `view_model.rs` (or a
  `view_model/` folder) holding the Dart view model's state and logic as
  plain Rust with no iced types, and the widgets/views next to it. A view
  model method that did async work in Dart returns an `Effect` (or takes a
  closure) that the screen's `update` turns into a `Task` running the
  service call on a blocking thread; the result comes back as a `Message`.
  Every Dart `notifyListeners` becomes "state changed; iced redraws".
  Generation counters, `_disposed` guards and save chains keep their exact
  semantics: a late result for a screen that is gone, or a superseded load,
  is dropped exactly where Dart dropped it.
- **Widgets**: `vocara_app::ui::core` holds the theme (`studio.dart` colors,
  `theme.dart`) and the shared widgets (`studio_button`, `studio_controls`,
  `confirm_dialog`, `fatal_open_screen`, `waveform_painter`,
  `spectrogram_painter`, `window_drag_bar`, ...). Painters are
  `canvas::Program`s; controls that need custom input (faders, knobs, drag
  handles) are custom widgets or canvases with the same hit areas, drag
  rules, wheel/trackpad handling and double-click resets as the Dart ones.
  Icons are Lucide (forui's `FIcons`) from the `lucide-icons` crate.
- **Window** (macOS): hidden title bar with a transparent, full-size content
  view; the drag bar starts a window drag; the close request runs the quit
  guard (kill separation sidecars, finalize recordings) before closing, as
  `main.dart`'s `_QuitGuard` and exit listener did.
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

## Tracking

Every Dart source and where it lives in Rust. A row is `done` only when its
behavior and its tests are ported and the gates pass.

| Dart source (client/lib/) | Lines | Rust home | Status |
|---|---:|---|---|
| `app.dart` | 85 | `vocara_app::app` | done |
| `config/app_config.dart` | 16 | `vocara_model::config::app_config` | done |
| `config/dependencies.dart` | 109 | `vocara_app::dependencies` | todo |
| `config/environment.dart` | 59 | `vocara_model::config::environment` | done |
| `data/db/app_database.dart` | 180 | `vocara_store::db::schema` | done |
| `data/db/asset_store.dart` | 1322 | `vocara_store::db::asset_store` | done |
| `data/db/durable_fs.dart` | 119 | `vocara_store::db::durable_fs` | done |
| `data/db/library_lock.dart` | 318 | `vocara_store::db::library_lock` | done |
| `data/db/persistence.dart` | 1286 | `vocara_services::persistence` (it opens the store and drives services: audio edit, waveform, audio files) | todo |
| `data/db/safe_migrator.dart` | 379 | `vocara_store::db::safe_migrator` | done |
| `data/db/tables.dart` | 347 | `vocara_store::db::{schema, rows}` | done |
| `data/db/write_gate.dart` | 92 | `vocara_store::db::write_gate` | done |
| `data/ffi/vocara_engine_bindings.dart` | 2051 | `vocara_services::engine` (with the decode policy, probe and device lists from the top of `graph_audio_playback_service.dart`: `engine::media`, `engine::enumerate_*`) | done (native calls type-checked for macOS; their tests run with `device`) |
| `data/repositories/song_practice_repository.dart` | 64 | `vocara_store::repositories::song_practice_repository` | done |
| `data/repositories/song_practice_repository_drift.dart` | 1678 | `vocara_store::repositories::song_practice_repository_drift` | done |
| `data/repositories/technique_repository.dart` | 52 | `vocara_store::repositories::technique_repository` | done |
| `data/repositories/technique_repository_drift.dart` | 607 | `vocara_store::repositories::technique_repository_drift` | done |
| `data/services/app_settings_service.dart` | 1031 | `vocara_services::app_settings_service` | todo |
| `data/services/audio_dsp.dart` | 2663 | `vocara_services::audio_dsp` | done |
| `data/services/audio_edit_service.dart` | 863 | `vocara_services::audio_edit_service` | todo |
| `data/services/audio_exporter.dart` | 173 | `vocara_services::audio_exporter` | done (the save panel is passed in by the app) |
| `data/services/audio_file_service.dart` | 362 | `vocara_services::audio_file_service` | done |
| `data/services/diagnostics_log.dart` | 440 | `vocara_model::diagnostics` (used by the store too) | done |
| `data/services/graph_audio_playback_service.dart` | 1533 | `vocara_services::graph_audio_playback_service` | todo |
| `data/services/local_storage_service.dart` | 110 | `vocara_services::local_storage_service` | done |
| `data/services/media_analysis_service.dart` | 112 | `vocara_services::media_analysis_service` | done (the default decoder is the engine bridge's; its tests run on injected decoders) |
| `data/services/mic_permission.dart` | 36 | `vocara_services::mic_permission` | done (macOS half type-checked; runs on CI) |
| `data/services/mic_recording_controller.dart` | 681 | `vocara_services::mic_recording_controller` | todo |
| `data/services/monitor_service.dart` | 262 | `vocara_services::monitor_service` | done |
| `data/services/playback_engine.dart` | 147 | `vocara_services::playback_engine` | done |
| `data/services/playback_engine_factory.dart` | 8 | `vocara_services::playback_engine_factory` | todo |
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
| `ui/home/view_model/home_view_model.dart` | 144 | `vocara_app::ui::home::view_model::home_view_model` | todo |
| `ui/home/widgets/home_screen.dart` | 491 | `vocara_app::ui::home::widgets::home_screen` | todo |
| `ui/home/widgets/new_practice_dialog.dart` | 16 | `vocara_app::ui::home::widgets::new_practice_dialog` | todo |
| `ui/home/widgets/practice_project_card.dart` | 126 | `vocara_app::ui::home::widgets::practice_project_card` | todo |
| `ui/home/widgets/update_notice.dart` | 219 | `vocara_app::ui::home::widgets::update_notice` | todo |
| `ui/settings/licenses.dart` | 213 | `vocara_app::ui::settings::licenses` | todo |
| `ui/settings/settings_screen.dart` | 1135 | `vocara_app::ui::settings::settings_screen` | todo |
| `ui/song_practice_detail/view_model/analysis_cache.dart` | 1717 | `vocara_app::ui::song_practice_detail::view_model::analysis_cache` | todo |
| `ui/song_practice_detail/view_model/analysis_source.dart` | 64 | `vocara_app::ui::song_practice_detail::view_model::analysis_source` | todo |
| `ui/song_practice_detail/view_model/interpretations_controller.dart` | 296 | `vocara_app::ui::song_practice_detail::view_model::interpretations_controller` | todo |
| `ui/song_practice_detail/view_model/mixer_controller.dart` | 1271 | `vocara_app::ui::song_practice_detail::view_model::mixer_controller` | todo |
| `ui/song_practice_detail/view_model/playback_controller.dart` | 439 | `vocara_app::ui::song_practice_detail::view_model::playback_controller` | todo |
| `ui/song_practice_detail/view_model/recording_controller.dart` | 1169 | `vocara_app::ui::song_practice_detail::view_model::recording_controller` | todo |
| `ui/song_practice_detail/view_model/song_practice_detail_view_model.dart` | 3337 | `vocara_app::ui::song_practice_detail::view_model::song_practice_detail_view_model` | todo |
| `ui/song_practice_detail/widgets/analysis_lanes.dart` | 988 | `vocara_app::ui::song_practice_detail::widgets::analysis_lanes` | todo |
| `ui/song_practice_detail/widgets/bottom_dock.dart` | 312 | `vocara_app::ui::song_practice_detail::widgets::bottom_dock` | todo |
| `ui/song_practice_detail/widgets/detail_transport_bar.dart` | 456 | `vocara_app::ui::song_practice_detail::widgets::detail_transport_bar` | todo |
| `ui/song_practice_detail/widgets/fx_panel.dart` | 1723 | `vocara_app::ui::song_practice_detail::widgets::fx_panel` | todo |
| `ui/song_practice_detail/widgets/left_panel.dart` | 1545 | `vocara_app::ui::song_practice_detail::widgets::left_panel` | todo |
| `ui/song_practice_detail/widgets/live_spectrogram.dart` | 150 | `vocara_app::ui::song_practice_detail::widgets::live_spectrogram` | todo |
| `ui/song_practice_detail/widgets/lyrics_panel.dart` | 1396 | `vocara_app::ui::song_practice_detail::widgets::lyrics_panel` | todo |
| `ui/song_practice_detail/widgets/mixer_strip.dart` | 1185 | `vocara_app::ui::song_practice_detail::widgets::mixer_strip` | todo |
| `ui/song_practice_detail/widgets/note_editor.dart` | 195 | `vocara_app::ui::song_practice_detail::widgets::note_editor` | todo |
| `ui/song_practice_detail/widgets/pitch_lane.dart` | 648 | `vocara_app::ui::song_practice_detail::widgets::pitch_lane` | todo |
| `ui/song_practice_detail/widgets/ruler_bar.dart` | 254 | `vocara_app::ui::song_practice_detail::widgets::ruler_bar` | todo |
| `ui/song_practice_detail/widgets/song_practice_detail_screen.dart` | 2172 | `vocara_app::ui::song_practice_detail::widgets::song_practice_detail_screen` | todo |
| `ui/song_practice_detail/widgets/technique_picker.dart` | 309 | `vocara_app::ui::song_practice_detail::widgets::technique_picker` | todo |
| `ui/song_practice_detail/widgets/track_context_menu.dart` | 640 | `vocara_app::ui::song_practice_detail::widgets::track_context_menu` | todo |
| `ui/song_practice_detail/widgets/track_row.dart` | 1158 | `vocara_app::ui::song_practice_detail::widgets::track_row` | todo |
| `ui/song_practice_detail/widgets/voice_profile_view.dart` | 567 | `vocara_app::ui::song_practice_detail::widgets::voice_profile_view` | todo |
| `ui/song_practice_detail/widgets/voiceprint_view.dart` | 2015 | `vocara_app::ui::song_practice_detail::widgets::voiceprint_view` | todo |
| `ui/technique_library/technique_library_screen.dart` | 914 | `vocara_app::ui::technique_library::technique_library_screen` | todo |
| `ui/technique_library/view_model/technique_detail_view_model.dart` | 2013 | `vocara_app::ui::technique_library::view_model::technique_detail_view_model` | todo |
| `ui/technique_library/view_model/technique_library_view_model.dart` | 280 | `vocara_app::ui::technique_library::view_model::technique_library_view_model` | todo |
| `ui/technique_library/widgets/media/technique_audio_player.dart` | 417 | `vocara_app::ui::technique_library::widgets::media::technique_audio_player` | todo |
| `ui/technique_library/widgets/media/technique_media_section.dart` | 898 | `vocara_app::ui::technique_library::widgets::media::technique_media_section` | todo |
| `ui/technique_library/widgets/media/technique_video_player.dart` | 190 | `vocara_app::ui::technique_library::widgets::media::technique_video_player` | todo |
| `ui/technique_library/widgets/studio_field.dart` | 96 | `vocara_app::ui::technique_library::widgets::studio_field` | todo |
| `ui/technique_library/widgets/technique_analysis_section.dart` | 690 | `vocara_app::ui::technique_library::widgets::technique_analysis_section` | todo |
| `ui/technique_library/widgets/technique_detail_screen.dart` | 825 | `vocara_app::ui::technique_library::widgets::technique_detail_screen` | todo |
| `ui/technique_library/widgets/technique_dialogs.dart` | 117 | `vocara_app::ui::technique_library::widgets::technique_dialogs` | todo |
| `utils/analysis_gate.dart` | 54 | `vocara_model::util::analysis_gate` | done |
| `utils/busy_gate.dart` | 83 | `vocara_app::util::busy_gate` | done |
| `utils/command.dart` | 71 | `vocara_app::util::command` | done |
| `utils/export_copy.dart` | 153 | `vocara_model::util::export_copy` | done |
| `utils/format.dart` | 13 | `vocara_model::util::format` | done |
| `utils/id.dart` | 87 | `vocara_model::util::id` | done |
| `utils/lru_bytes_cache.dart` | 242 | `vocara_model::util::lru_bytes_cache` | done |
| `utils/lyrics_format.dart` | 223 | `vocara_model::util::lyrics_format` | done |
| `utils/result.dart` | 32 | `(std Result; no port)` | done |
