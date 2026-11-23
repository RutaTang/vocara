# Vocara

Vocara — a macOS app for singing practice notes and analysis: multi-track
practice sessions over a backing track, takes recorded through a real-time
audio engine, pitch and voice analysis, lyrics, and a technique library.

It is written in Rust, with [iced](https://iced.rs) as the GUI, and the core
algorithms are formally verified in Lean 4.

## Layout

| Path | What |
|---|---|
| `rust/crates/vocara_engine` | the real-time audio engine (graph runtime, CoreAudio device, decoders, DSP) |
| `rust/crates/vocara_model` | the domain model and pure utilities |
| `rust/crates/vocara_store` | the library store: SQLite database, asset pool, sync, repositories |
| `rust/crates/vocara_services` | services: engine bridge, playback, recording, analysis, separation, settings, persistence |
| `rust/crates/vocara_app` | the iced application (`vocara` binary) |
| `rust/verification` | Lean 4 proofs of the core algorithms, and the vectors the Rust tests replay ([README](rust/verification/README.md)) |
| `rust/packaging/macos` | the app bundle script, Info.plist, entitlements and icon |
| `docs/` | design notes, the sync protocol, the release runbook |

`rust/PORTING.md` records how the app was ported from its earlier Flutter
client, file by file, and the conventions the code follows.

## Environments (build flavors)

The macOS app ships in three environments, each a **distinct bundle id** so they
coexist on one machine with separate stores (database + assets):

| Flavor | Bundle id                  | Purpose                          |
| ------ | -------------------------- | -------------------------------- |
| `dev`  | `com.rutatang.vocara.dev`  | Local development (default)      |
| `test` | `com.rutatang.vocara.test` | Automated test runs              |
| `prod` | `com.rutatang.vocara`      | Shipping build                   |

The flavor is the `VOCARA_FLAVOR` environment variable: read at run time by
`cargo run`, and baked into the binary by the bundle script (an app launched
from Finder has no environment of its own).

```sh
just run dev                         # run on macOS with the live audio engine
just bundle prod                     # build vocara.app (rust/packaging/macos/bundle.sh)
```

At runtime, dev/test show a coral `DEV` / `TEST` pill in the window's top bar and
suffix the window title (`Vocara — DEV`); prod shows neither.

## Development

Prerequisites: a Rust toolchain ([rustup](https://rustup.rs)) and
[just](https://github.com/casey/just); Lean 4 (the version in
`rust/verification/lean-toolchain`) for the proofs.

```sh
just check     # format check, clippy (both feature modes), tests
just test      # the tests in both feature modes
just proofs    # build the Lean proofs and regenerate the vectors
```

The `device` feature (`--features vocara_app/device`) compiles the live
CoreAudio engine, VoiceProcessingIO and the AVFoundation players; without it
the app and its tests build and run headless on any platform.
