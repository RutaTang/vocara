# vocara_engine

Real-time DAG audio engine for vocara (macOS singing practice/analysis app). Pure-Rust core
with a `cpal` + `symphonia` device layer behind a feature flag, exposed to Flutter/Dart over
a C ABI.

## Design

One node graph processed by one owner. A control thread edits a staged graph and publishes
it; the audio thread renders a topologically-ordered schedule over a buffer pool. The DSP
core, graph runtime, and transport are **dependency-free** and validated through a headless
offline render — the primary RT-safety + golden-file surface. The live OS audio path lives
behind the `device` feature.

```
per vocal track:  Source → EQ → Comp → Gain → Pan → Master (dry)
                                       └─ Send → [shared Reverb bus] → Master
backing/reference: Source → Gain → Pan → Master            (clean A/B, no insert chain)
```

### Modules

| Module | Role |
|--------|------|
| `node` | the `Node` trait — the engine-agnostic seam (`process(block)`, state owned, no alloc on RT) |
| `dsp::biquad` | RBJ-cookbook parametric EQ cascade; coefficient changes cross-fade outputs (never lerp coeffs) |
| `dsp::compressor` | feed-forward log-domain soft-knee compressor (C¹ knee), stereo-linked detector, GR meter |
| `dsp::reverb` | Freeverb (Schroeder–Moorer); width before the return; denormal-flushed feedback |
| `dsp::gain_pan` | equal-power (−3 dB) gain/pan + standalone pan; `dsp::master` final sum + soft-clip |
| `dsp::denormals` | per-callback FTZ (`FPCR.FZ` on aarch64 / MXCSR on x86_64) + anti-denormal bias |
| `graph` | compile (topo-sort + cycle rejection), buffer pool, fan-in summation, render loop |
| `engine` | sample-counter transport: seek, gapless loop wrap (offline driver) |
| `live` | `EngineHandle` (control) + `RtProcessor` (audio callback) sharing transport atomics |
| `ring` | wait-free SPSC ring (commands, PCM, retired graphs) |
| `command` | coalesced parameter channel (control → audio) |
| `graph_cell` | RCU graph hot-swap; retired graphs freed off the audio thread after a grace epoch |
| `build` | assembles the vocara topology + shared reverb send bus from a spec |
| `source` | `BufferSource` (offline/timeline) and `RingSource` (live streaming) |
| `device` *(feature)* | cpal CoreAudio output + symphonia decode + resample |
| `ffi` *(feature)* | `ve_*` C ABI, `catch_unwind` at every edge, exported from the cdylib |

## Build & test

```sh
cargo test                      # 31 tests: DSP, graph, transport, live components (dep-free)
cargo run --bin render -- out.wav 3   # render the full graph to a WAV (golden-file harness)
cargo build --release --features device   # the cdylib with the live cpal/symphonia path
```

The `device` feature pulls `cpal` 0.18 and `symphonia` 0.5; without it the engine builds and
tests with zero external crates.

## RT-safety

The cpal callback body (`RtProcessor::process`) does no heap alloc, lock, syscall, or panic:
buffers are sized in `prepare()`, parameters arrive as `Copy` PODs over a wait-free ring, the
graph is loaded once per block via an `AtomicPtr` (no refcount, nothing to drop), and a
retired graph is freed on the control thread after a one-block grace epoch. `panic = "abort"`
in release; every FFI entry point is wrapped in `catch_unwind`.
