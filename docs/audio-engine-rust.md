# Vocara Real-Time Audio Engine — Rust Implementation Cut

Status: design, greenfield. **No Rust exists in the repo yet.** This doc re-cuts the
*already-settled* DAG audio-engine architecture for a Rust implementation and focuses
strictly on **what Rust changes**. The language-agnostic design — graph runtime, RCU
swap at the block boundary, sample-counter clock (`g_frame`), node topology, the three
DSP nodes' math, the shared vocal-reverb send bus, the phased plan, and the test
strategy — is unchanged and is restated only where Rust touches it.

All crate versions below are pinned to the **actual current crates.io releases verified
2026-06-18** (cpal 0.18.1 / 2026-06-07, symphonia 0.6.0 / 2026-05-15, rubato 3.0.0 /
2026-05-20, rtrb 0.3.4 / 2026-04-26, arc-swap 1.9.1 / 2026-04-04, dasp 0.11.0 /
2020-05-29). If a reviewer's mental model says "those versions don't exist yet," their
crate index is stale — check `cargo search` before downgrading.

---

## 1. Why Rust, in one paragraph

The riskiest layer of this engine is the **lock-free cross-thread seam**: a control
thread and one-or-more feeder threads handing data to an OS-owned real-time callback
that may not allocate, lock, or block. In Dart that seam is enforced only by convention
and a `_driftMs = 150` correction loop bolted onto one `media_kit` `Player` per track
(`client/lib/data/services/audio_playback_service.dart:43,51,52,162,193`); there is no
way to *prove* a callback is alloc-free or that a buffer isn't aliased across threads.
Rust makes that proof a compile error: `Send`/`Sync` gate what may cross into the
callback closure, ownership/borrow rules forbid the control thread from mutating live
RT state, `rtrb`'s `Producer`/`Consumer` split makes the SPSC seam the *only* sharing
and it carries `Copy` POD, and `#[deny]` + `assert_no_alloc` turn "no heap on the hot
path" from a hope into a CI gate. The second win is decode coverage: one crate,
**symphonia**, demuxes and decodes wav/flac/mp3/aac/m4a(mp4)/ogg in pure Rust,
retiring the per-format zoo and the AVFoundation-dependence risk that the Dart path
carries today.

---

## 2. The Rust crate stack mapped to engine layers

Substrate is **thin and RustAudio-led where RustAudio has an answer, hand-rolled where
it doesn't.** cpal owns the OS boundary; dasp is frozen *math only*; the graph runtime
and all three DSP kernels are 100% hand-rolled (there is no RustAudio graph/DAG crate
worth adopting).

| Engine layer | Crate (pinned) | In RustAudio org? | Role — and the hard boundary |
|---|---|---|---|
| **OS audio I/O + master clock** | `cpal` 0.18.1 | **yes** | The *only* CoreAudio boundary. Enumerates default output, builds one output `Stream`, drives the one render callback that owns `g_frame` and every output sample. P4 mic = a second input `Stream`. Gives you device + callback and **nothing above** — everything inside the closure is hand-rolled. |
| **DSP primitives (math only)** | `dasp` 0.11.0 (`dasp_sample`, `dasp_frame`, `dasp_interpolate`, `dasp_ring_buffer`) | **yes** | Header-style, const/stack math: `i16↔f32` `Sample` conversion at the symphonia→engine edge, `Frame` for interleaved stereo, `dasp_interpolate::{Linear,Sinc}` as the feeder's cheap resampler, `dasp_ring_buffer::Fixed` as **single-thread** node-local delay lines. **Never the framework, never the cross-thread ring.** Frozen since 2020 → depend on leaf sub-crates only, thin enough to re-implement in a day. |
| **Decode** | `symphonia` 0.6.0 | no | Feeder-thread probe+demux+decode for wav/flac/mp3/aac/m4a(mp4)/ogg → f32/i16 packets. Replaces hound/claxon/lewton/minimp3. Codecs enabled per-feature (`mp3`,`aac`,`isomp4`,`ogg`,`flac`,`alac`). **MPL-2.0** — see §4 license note. |
| **Resample (feeder only)** | `rubato` 3.0.0 (HQ) + `dasp_interpolate::Linear` (trivial ratio) | no (rubato) / yes (dasp) | `SincFixedIn`/`FftFixedIn` via `process_into_buffer()` into pre-allocated scratch → alloc-free **in steady state**. Constructor and any ratio change **do allocate**, so all rubato calls live on the feeder thread; ratio is fixed per source for the session (device rate is stable). NEON on AS, AVX/SSE on Intel. |
| **Lock-free SPSC seams (×3)** | `rtrb` 0.3.4 | no | The only cross-thread sharing. (A) coalesced param/transport command ring control→RT; (B) one PCM ring per source feeder→RT; (C) retired-graph pointer ring RT→reaper. `Producer`/`Consumer` are `Send + !Sync`; wait-free, alloc-free after construction. This is what `dasp_ring_buffer` is **not** (single-thread). |
| **RCU graph cell** | `arc-swap` 1.9.1 **or** raw `AtomicPtr<Graph>` | no | RCU hot-swap at the block boundary. **The retired graph is NEVER dropped on the RT thread** — see §3.3 for the exact pattern; this is the one place we deviate from a naive `ArcSwap::load`. |
| **False-sharing padding** | `crossbeam-utils` 0.8.21 (`CachePadded`) | no | `CachePadded<AtomicU64>` for `g_frame`/`playhead`, `CachePadded<AtomicPtr<Graph>>` for the RCU cell, so control and RT threads don't bounce a cache line. |
| **Debug RT tripwires** | `no_denormals` 0.3.0 *(optional)*, `assert_no_alloc` 1.1.2 | no | FTZ/DAZ RAII guard (or the in-house asm shim, §3.4) and a debug-only `GlobalAlloc` shim that aborts if the callback allocates. Both compile out of release. |

**Hand-rolled (no crate):**

- **The entire graph runtime.** `Node` trait, topo-sort + cycle-reject (control thread,
  at edit time), the reusable buffer pool (indices into a `Vec<Box<[f32]>>`, in-place
  where N-in == M-out), the fan-in **sum node** (the shared vocal-reverb SEND BUS is a
  real vertex), PDC delay lines (`dasp_ring_buffer::Fixed` storage), and the `g_frame`
  transport.
- **The three DSP kernels.** EQ (RBJ biquad cascade — verbatim port, §3.6), feed-forward
  log-domain soft-knee compressor with stereo-linked detector + GR meter, Freeverb /
  Schroeder–Moorer reverb (width-before-return, denormal-flushed combs/allpasses).
- **The FFI surface** (extern "C" + cbindgen → ffigen, §4).

**Rejected:** `ringbuf` 0.5.0 (broader API than the RT path needs — keep only if a later
*non-RT* async producer is wanted); `flutter_rust_bridge` for the engine surface (§4).

---

## 3. RT-safety in Rust idioms

### 3.0 Thread & ownership model (the `Send`/`Sync` proof starts here)

Three logical threads, mapped to ownership:

- **Control thread** — Dart→FFI. Owns graph *editing* on a **separate** `Box<Graph>`
  (staging), the `Producer` ends of the param ring and PCM rings, and publishes the new
  graph by pointer.
- **Audio (RT) thread** — cpal's CoreAudio render thread (OS-created). Owns the cpal
  `Stream`, the `Consumer` ends of the param + PCM rings, the `Producer` of the
  retired-graph ring, and a **non-owning** view of the live graph.
- **Reaper thread** — a plain `std::thread`. Owns the `Consumer` of the retired-graph
  ring; its only job is `recv → grace-wait → drop(Box<Graph>)` **off** the RT thread.

```text
CONTROL (Dart via FFI)                         RT (one CoreAudio callback)
 params/transport ─► rtrb cmd ring ───────────► drain coalesced cmds (apply)
 graph edit ─► topo-sort+cycle-reject ─► swap() ► AtomicPtr load ONCE/block ─► process
       │                                              │
       └─ retired *mut Graph ─► rtrb ─► REAPER        ▼
FEEDER (one per source)                        buffer pool · fan-in sum · EQ/Comp/Reverb
 file ─► symphonia ─► rubato/dasp ─► rtrb PCM ring ─► callback pop_slice ─► node 0 input
                                                      ▼
                                               cpal output buffer (dasp Sample conv at edge)
```

`cpal`'s `Stream` is **`!Send` and `!Sync`** (Android AAudio constraint, not going away).
It must be *built and dropped on its owning thread*. Architectural consequence enforced
from day one: a dedicated owner thread builds the stream and parks; control happens only
via atomics + rings. Do not try to store the `Stream` in a struct that crosses the FFI
boundary — it won't compile, and that compile error is the design working.

### 3.1 The cpal callback contract

The callback body is a hard RT contract: **no heap alloc, no lock, no syscall, no
logging, no panic.** Why each is structurally impossible, not just discouraged:

```rust
let stream = device.build_output_stream(
    &config,
    move |out: &mut [f32], _info| {
        let _fz = denormals::FlushGuard::new();          // §3.4, set on THIS thread
        assert_no_alloc::permit_alloc(|| false);          // debug tripwire, §3.5
        render(&mut ctx, out);                            // the whole graph render
    },
    move |_err| { ERR_CODE.store(/* code */, Relaxed); }, // NO logging on error path
    None,
)?;
```

- **No alloc** — every buffer is sized in `prepare()` and lives in `ctx.scratch:
  BufferPool` (a `Vec<Box<[f32]>>` of fixed worst-case blocks, indices handed out by
  value). Params arrive as `Copy` POD via `rtrb`; PCM arrives via `read_chunk`/`pop_slice`
  into existing slices. No `Vec::push`/`Box::new`/`format!` appears in the call tree, and
  `assert_no_alloc` **proves** it in CI under the offline harness.
- **No locks** — the only shared state reachable from `render` is atomics (`g_frame`,
  `playhead`, `transport`), the `AtomicPtr<Graph>` cell (wait-free `Acquire` load, no
  refcount), and `rtrb` rings (wait-free SPSC). No `Mutex`/`RwLock` in the call tree.
- **No syscalls / no clock calls** — `g_frame` **is** the clock; `Instant::now()` is
  never called. This is the whole point of the sample-counter transport: per-source
  drift correction **structurally disappears** (the Dart `_driftMs=150` loop is deleted,
  not ported). File I/O + decode + resample all live on the feeder thread.
- **No logging** — diagnostics are atomic counters (`xrun_count`, `last_error`) read
  off-thread at vsync.
- **No panics** — see §3.5.

> **cpal block size is a request, not a guarantee.** CoreAudio can deliver short blocks
> even with `BufferSize::Fixed(256)`. `prepare(max_block, sr)` sizes everything to the
> **worst case**; `process(&[&[f32]], &mut [&mut [f32]], block_len)` must handle any
> `block_len ∈ [1, max_block]`. Hardcoding 256 anywhere is a latent crash.

### 3.2 Send/Sync: what's natural vs. the one `unsafe` boundary

- **Naturally `Send`:** `rtrb::Producer/Consumer` (documented `Send + !Sync` — moved into
  `RtContext`, never shared by `&`), `Arc<CachePadded<AtomicU64>>` and `Arc<Transport>`
  (atomics only → `Send + Sync`), `BufferPool` (owns `Box<[f32]>`), and every DSP node
  whose state is `[f32; N]` / `Box<[f32]>` / scalars (the biquad, compressor, reverb are
  all POD-ish, trivially `Send`).
- **The ONLY `unsafe impl Send`** is the retired raw pointer:

```rust
#[repr(transparent)]
struct RetiredPtr(*mut Graph);
// SAFETY: the pointer is uniquely owned at hand-off; the RT thread relinquishes all
// access before pushing, and only the reaper dereferences/drops it. No aliasing.
unsafe impl Send for RetiredPtr {}
```

The control thread never holds `&mut` to the *published* graph; it edits a separate
owned `Box<Graph>`. The RT thread sees the graph only through a **block-scoped** `&Graph`
that cannot be stored or sent (its lifetime is bound to the callback invocation). They
are different allocations until the atomic publish ⇒ no `&`/`&mut` aliasing, enforced by
the borrow checker.

### 3.3 RCU graph swap WITHOUT dropping the retired graph on the RT thread

This is the single most RT-suspect decision in the stack, and the adversarial review is
right to push on it. Two layers:

**Default path — `arc-swap`, with exactly one bounded load per block.** Hold the live
graph as `ArcSwap<Graph>`. Control thread builds the new (topo-sorted, cycle-rejected,
state-migrated) graph and `swap()`s it; the audio thread does **one** `load_full()` at
the block boundary, holds that `Arc` for the whole block, releases it after. Never per
sample. Writes (the expensive side) stay on the control thread.

**The disqualifier we design around:** `arc-swap`'s `DefaultStrategy` can run the retired
`Arc`'s `Drop`/dealloc on **whichever thread releases the last reference** — which can be
the *audio* thread when it drops the last `Guard`. Freeing an entire retired graph (every
node's `Box<[f32]>` reverb tails) inside the callback = an unbounded `free()` = a missed
deadline. So we **never let the RT thread own the last reference.** The retired graph is
handed to the off-thread reaper:

```rust
struct GraphCell { ptr: CachePadded<AtomicPtr<Graph>> }

impl GraphCell {
    // RT thread. Non-owning borrow, valid for THIS block only. No refcount, nothing to drop.
    #[inline]
    fn current<'a>(&'a self) -> &'a Graph {
        // SAFETY: control thread + reaper guarantee the pointee outlives this block
        // (one-block grace, below). Publish uses Release; this is Acquire.
        unsafe { &*self.ptr.load(Ordering::Acquire) }
    }
}

// Control thread:
fn publish(&self, mut staged: Box<Graph>, reaper: &mut rtrb::Producer<RetiredPtr>) {
    migrate_carryover(&mut staged);                       // §3.7 reverb tail hand-off
    let new = Box::into_raw(staged);
    let old = self.cell.ptr.swap(new, Ordering::AcqRel);  // block-boundary publish
    SWAP_FRAME.store(g_frame.load(Relaxed), Relaxed);     // record grace epoch
    let _ = reaper.push(RetiredPtr(old));                 // ring sized > max in-flight swaps
}

// Reaper thread (off the RT path — the ONLY place a Graph is freed):
while let Ok(RetiredPtr(p)) = retired.pop() {
    while g_frame.load(Relaxed) < SWAP_FRAME.load(Relaxed) + block_len { hint::spin_loop(); }
    drop(unsafe { Box::from_raw(p) });
}
```

**The grace argument:** the RT thread reads the pointer exactly once per block and never
stores `&Graph`, so once `g_frame ≥ swap_frame + block_len` the RT thread has provably
returned from any borrow that could observe the old pointer. One block of grace is
sufficient. The RT thread does **zero refcount atomics and zero `free()`**. This single-
reader/single-retirer epoch scheme is cheaper and more predictable than hazard pointers;
if true multi-version overlap is ever needed, `crossbeam-epoch`'s `defer_destroy` is a
drop-in with the same off-thread-reclaim shape.

**Decision:** ship behind `GraphCell` either way. Start with `arc-swap` + one-load-per-
block for ergonomics during P1; if a TSan/latency trace on the **offline harness** (not
the live callback — see §5) shows `load()` hitting its debt-slot fallback, flip the
internal to the raw `AtomicPtr` + reaper above. The public `GraphCell::current()` borrow
signature does not change, so it is a genuine drop-in, not a rewrite. Gate any change to
the "read once, never cache `&Graph`" invariant with Miri + TSan on the harness.

**Carry-over migration** runs on the control thread *before* publish:
`migrate_carryover` `mem::swap`s the `Box<[f32]>` comb/allpass/PDC delay lines from the
old graph's matching nodes into the staged graph (keyed by stable node id), so the RT
thread sees a fully-initialized graph whose reverb buffers already hold the previous
tail. The old graph — now holding the *staged* graph's silent buffers — is what gets
reaped, harmless to free.

### 3.4 Denormals — exact asm (this was hand-waved before; here it is concrete)

There is **no portable std flush API**, and `_mm_setcsr` / `_MM_SET_FLUSH_ZERO_MODE` are
**deprecated since Rust 1.75**. Critically, **aarch64 FPCR is per-thread and NOT inherited
by child threads**, and the cpal render thread is OS-created — so flushing once at startup
on the wrong thread silently does nothing and denormal stalls reappear *only* under
stacked feedback (exactly the shared reverb send-bus case). **Set FTZ at the top of every
callback**, on the callback's own thread. The `mrs/msr` pair is a couple of cycles.

```rust
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn set_ftz() {
    let mut fpcr: u64;
    core::arch::asm!("mrs {0}, fpcr", out(reg) fpcr, options(nomem, nostack));
    fpcr |= 1 << 24;                       // FZ (flush-to-zero). bit 25 = AH (alt half-prec), optional.
    core::arch::asm!("msr fpcr, {0}", in(reg) fpcr, options(nomem, nostack));
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn set_ftz() {
    let mut mxcsr: u32;
    core::arch::asm!("stmxcsr [{0}]", in(reg) &mut mxcsr, options(nostack));
    mxcsr |= (1 << 15) | (1 << 6);         // FTZ | DAZ
    core::arch::asm!("ldmxcsr [{0}]", in(reg) &mxcsr, options(nostack));
}
```

Wrap as a RAII `FlushGuard` (save prior bits, restore on drop) constructed first thing in
the callback. `no_denormals` 0.3.0 implements exactly this and is a fine dependency; the
in-house shim with `options(nomem, nostack)` is the lower-footgun long-term choice (the
crate's own docs flag the API as "technically UB" when mixed with auto-vectorized code
the compiler assumes runs in default FP mode — whole-callback scope is the accepted
mitigation, but owning the shim lets us reason about it).

**Belt-and-suspenders** for the Freeverb feedback paths (survives even a future SIMD path
where FTZ isn't honored): the classic `undenormalise` sign-flip, or add a tiny bias to the
feedback term, so a comb decaying into silence can't enter subnormal arithmetic:

```rust
const ANTI_DN: f32 = 1.0e-18;
// in the comb feedback accumulator:
y = input + feedback * (filtered + ANTI_DN);   // keeps the tail out of subnormal land
```

### 3.5 Panic discipline + the alloc tripwire

```toml
[profile.release]
panic   = "abort"      # unwinding across the C ABI is UB; abort is deterministic and
lto     = "fat"        # lets the optimizer drop unwind landing pads on the hot path
codegen-units = 1
[profile.dev]
panic   = "abort"
```

- Every `#[no_mangle] extern "C"` entry point wraps its body in `std::panic::catch_unwind`
  and returns an **error code** instead of unwinding (validate Dart-supplied inputs there;
  never reach a panic on bad input). `panic = "abort"` then makes any *reachable* panic
  deterministic. Note the cost: `abort` kills the whole app, UI included, so the discipline
  is "validate at the FFI edge, return codes," not "let it ride."
- The `dsp` and `graph-rt` crates carry
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing,
  clippy::panic, clippy::arithmetic_side_effects)]`. The inner loops do one length check
  per block, then `chunks_exact_mut` / `get_unchecked` — branch-free, panic-free.
- `assert_no_alloc` 1.1.2 wraps `render()` in debug: any heap (de)alloc on the RT thread
  aborts with a stacktrace. Compiled out of release entirely, so it proves nothing about
  release — pair it with the offline TSan harness and a manual audit (§5). It is a
  `GlobalAlloc` wrapper and conflicts with any other custom global allocator; we have none.

### 3.6 The biquad kernel — verbatim port (golden-file bit-comparable)

Confirmed against the live source: `client/lib/data/services/audio_dsp.dart:411-433`
`_biquadLp` is an RBJ low-pass with coeffs pre-divided by `a0` and a DF-I recurrence
`yi = b0*xi + b1*x1 + b2*x2 - a1*y1 - a2*y2`. The Rust port keeps the algebra **identical**
(use `f64` during P2 to match the golden file bit-for-bit; `f32` in production):

```rust
#[derive(Clone, Copy)]
struct Biquad { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32, x1: f32, x2: f32, y1: f32, y2: f32 }

impl Biquad {
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        // EXACT DF-I recurrence ported from audio_dsp.dart:_biquadLp (coeffs pre-divided by a0)
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
              - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1; self.x1 = x;
        self.y2 = self.y1; self.y1 = y;
        y
    }
}
```

`Biquad` is `Copy`, `Send`, zero-alloc. The EQ node is a `Box<[Biquad]>` cascade sized in
`prepare()`. **Param change = two filter instances, OUTPUT cross-fade over a block —
never lerp coefficients** (coeff interpolation is transiently unstable). NaN/inf hygiene:
because feedback can theoretically produce non-finite values, the node clamps its block
output (`out[i] = y.clamp(-CEIL, CEIL)` with a `y.is_finite()` zero-replacement on the
state write-back) so one bad sample can't poison `y1`/`y2` for the rest of the session.

### 3.7 Coalesced param command ring (rtrb, producer-side coalescing)

POD command, `Copy`, so no `Box`/`Drop` ever crosses the ring:

```rust
#[derive(Clone, Copy)]
struct ParamCmd { node: NodeId, param: ParamId, value: f32, epoch: u32 }
```

The control thread **coalesces** duplicate `(node, param)` *before* they reach the RT
thread (a slider drag firing 200 events/frame collapses to one) via a fixed-size
`pending: Box<[Option<f32>]>` indexed by `node * params_per_node + param`, sized at
`prepare()`. `flush()` once per control tick drains `pending` into the ring; on a full
ring it leaves the value pending and retries next tick (never blocks). The RT consumer
drains a **bounded** count per block (`min(slots, MAX_PER_BLOCK)`) and applies each.
Adding/removing nodes at runtime resizes/remaps `pending` on the control thread and
re-sends in-flight values after publish, sequenced by `epoch`.

---

## 4. Build + FFI

**Library type: `cdylib`** (plus `rlib` for the offline-render test binary and benches).
A Flutter macOS app loads native code as frameworks under `Contents/Frameworks/`,
resolved at runtime via `DynamicLibrary.open` — `dart:ffi` wants a dylib, and the entire
existing plugin set here (`record_macos.framework`, `objective_c.framework`,
`media_kit_libs_macos_audio.framework`, the FFmpeg `Av*.framework`s) is dynamic. A
`staticlib` would force editing `project.pbxproj` link phases + `OTHER_LDFLAGS` — fragile
and un-Flutter-idiomatic. `cdylib` also gives a minimal export surface (only `#[no_mangle]
extern "C"` symbols) and is trivially codesignable as a unit.

**Integration: cargokit pattern, vendored and trimmed — a CocoaPods pod whose
`script_phase` runs cargo.** Grounded in the actual repo: `macos/Podfile` is
`platform :osx, '10.15'` + `use_frameworks!` (verified lines 1, 29-30), and the existing
`[CP] Embed Pods Frameworks` phase already copies *and* re-signs every framework it sees.
So the cleanest path is to **make the Rust artifact look like just another plugin
framework** — that path is already load-bearing in this build. We vendor a ~20-line
`macos/vocara_engine.podspec` (`vendored_frameworks` + `script_phase`
`:execution_position => :before_compile` + `prepare_command` for fresh checkouts) and add
one line to the Podfile:

```ruby
pod 'vocara_engine', :path => '.'
```

Because `use_frameworks!` is on, the vendored `vocara_engine.framework` is auto-copied
into `vocara.app/Contents/Frameworks/` and re-signed by the existing embed phase — the
**same mechanism** that already embeds `record_macos.framework`. **No pbxproj editing.**
Rejected alternatives: **corrosion** (introduces a CMake layer macOS doesn't otherwise
have — CMake only appears on Flutter's Linux/Windows embedders); a **raw Xcode Run-Script
phase** (merge-hostile pbxproj surgery, doesn't survive `flutter clean` cleanly).

`build_rust.sh` (the cargo driver), critical details that are the actual P0 footguns:

- **`install_name` MUST be `@rpath/vocara_engine.framework/Versions/A/vocara_engine`** via
  `install_name_tool -id` — the Runner's `@rpath` includes `@executable_path/../Frameworks`;
  get this wrong and `DynamicLibrary.open` fails at runtime even though the file is present.
  Add an `otool -L` smoke check to the P0 gate.
- **Idempotency** (survives repeated `flutter run` / CI shards): `set -euo pipefail`,
  `rm -rf` the framework dir before rebuild, verify both arch dylibs exist before `lipo`.
- **Apple-Silicon-only by default; universal is one env flag.**
  `cargo build --target aarch64-apple-darwin` always (deterministic regardless of host
  default triple). `VOCARA_UNIVERSAL=1` (release CI only) additionally builds
  `x86_64-apple-darwin` and `lipo`s the two into a fat dylib (`lipo -info` verified in
  CI). Both targets are already installed (`rustup target list --installed` shows
  `aarch64-apple-darwin` and `x86_64-apple-darwin`). Gate x86_64 behind CI so the local
  arm64 loop isn't a 2× build.

**The RT surface: hand-rolled `extern "C"` + cbindgen → ffigen. Reject
flutter_rust_bridge for this surface.** FRB marshals calls onto a background isolate and
returns `Future`s; the *entire point* of this architecture is that the control plane is
non-blocking atomic writes + SPSC `push`es and the data plane is a CoreAudio callback FRB
never sees. FRB's `RustOpaque`/`DartFnFuture` layer wraps an allocation- and Future-shaped
indirection around what should be a `store(Relaxed)` or a `ring.push()`. Keep FRB *only*
as an optional later path for non-RT control-plane conveniences ("load this project file,
return a parsed struct"), where it can coexist as a second set of exports.

The flat, opaque-handle C API (POD only across the boundary):

```rust
#[repr(C)] pub struct VeEngine { _p: [u8; 0] }              // opaque

#[no_mangle] pub extern "C" fn ve_create() -> *mut VeEngine;
#[no_mangle] pub extern "C" fn ve_destroy(e: *mut VeEngine);
#[no_mangle] pub extern "C" fn ve_open_file(e: *mut VeEngine, path: *const c_char) -> c_int;
#[no_mangle] pub extern "C" fn ve_play(e: *mut VeEngine) -> c_int;   // control-plane: atomic write
#[no_mangle] pub extern "C" fn ve_stop(e: *mut VeEngine) -> c_int;
#[no_mangle] pub extern "C" fn ve_push_pcm(e: *mut VeEngine, pcm: *const f32, frames: usize) -> usize; // ret = frames accepted (back-pressure)
#[no_mangle] pub extern "C" fn ve_frame_pos(e: *mut VeEngine) -> u64; // atomic read of g_frame
```

`build.rs` runs cbindgen → `include/vocara_engine.h`; `ffigen.yaml` → 
`lib/data/ffi/vocara_engine_bindings.dart`. **Both the header and the Dart bindings are
checked into git** (pin cbindgen + ffigen versions in lockfiles), so CI and teammates
without libclang can still `flutter build`; regeneration is a deliberate
`dart run ffigen` + `cargo build` step, never on the hot build path. `ve_push_pcm` returns
frames-accepted as the **back-pressure signal** — the feeder/isolate observes a reduced
return and slows down; it must **never spin-block** on a full ring (the producer side
checks remaining capacity and writes only what fits).

**`flutter clean` / CI survival:** `rust/target/` lives *outside* `build/`, so incremental
cargo state survives `flutter clean` (fast rebuilds) while the embedded framework is
always regenerated by the `script_phase` (correctness). Add `rust/target/` and
`macos/Frameworks/` to `.gitignore`. CI bootstrap adds the two `rustup target`s (already
present locally) and caches `~/.cargo` + `rust/target`.

**Codesign / Hardened Runtime:** nothing custom for dev — the vendored framework rides the
existing embed phase's `codesign` (ad-hoc `-` in Debug, real identity in notarized
Release). Rust dylibs are JIT-free, so Hardened Runtime needs no extra entitlement; the
existing `com.apple.security.cs.allow-jit` in `DebugProfile.entitlements` is for the Dart
VM, not Rust. cpal *output* needs no entitlement; P4 mic monitoring is **already covered**
by `com.apple.security.device.audio-input` (verified in both `Release.entitlements` and
`DebugProfile.entitlements`).

**License posture:** everything is MIT/Apache-2.0 **except symphonia (MPL-2.0, file-level
weak copyleft)** and rubato (MIT). Linking symphonia *unmodified* is fine for a closed-
source app, but **patching any symphonia file obligates publishing that file.** This is a
legal-process item, not a technical one: **get a one-line legal sign-off before shipping**,
and if a perf patch to e.g. the mp3 decoder is ever needed, plan to publish that file
(e.g. a public fork). No GPL anywhere.

---

## 5. P0 in Rust

**The spike (one workspace crate, `vocara_engine`):**

1. **Feeder thread** (`std::thread`, not a Dart isolate yet): `symphonia` opens one file,
   decodes packets to f32, resamples to the device rate on this thread (rubato
   `process_into_buffer`, or `dasp_interpolate::Linear` for trivial ratios), pushes
   interleaved stereo into an `rtrb` PCM ring. Polls an `AtomicBool device_changed`.
2. **cpal output**: `default_output_device()`, `SampleFormat::F32`, 2ch, device native
   rate, `BufferSize::Fixed(256)` *requested*. Callback does ONLY: `pop_slice` from the
   ring into the output, zero-fill on underrun, `g_frame.fetch_add(frames, Relaxed)`,
   FTZ at entry. No alloc/lock/syscall/panic/log.
3. **Dart `GraphAudioPlaybackService`**: mirrors the current `AudioPlaybackService`
   interface (so the P1 cutover is a swap, not a rewrite). Calls `ve_open_file` /
   `ve_play` / `ve_stop`; exposes `position` via a vsync `Ticker` reading `ve_frame_pos()`
   and converting samples→`Duration` — **replacing** the `Timer.periodic(30ms)` +
   `Stopwatch` + `_driftMs=150` synthetic clock entirely. `ve_push_pcm` proves the
   isolate→FFI feed path: the isolate's `Float32List` is pinned for the call duration,
   Rust copies into the ring and returns, so **no Dart GC ever touches anything the
   callback reads** (the callback touches only Rust-owned ring memory allocated once).
4. **Device-change**: cpal raises it on its own thread → set `AtomicBool` → feeder polls,
   pauses, closes+reopens the stream, clears the ring, resets the bool. The device-change
   callback never re-enters the stream or calls into Dart (avoids re-entrancy).

**Exit gate — P0 passes when ALL hold on Apple Silicon:**

1. `flutter run -d macos` builds the framework via the pod `script_phase`, embeds +
   codesigns it, and the app `DynamicLibrary.open`s it. `otool -L` shows the `@rpath`
   install_name.
2. `ve_open_file` + `ve_play` → glitch-free real-song audio at native rate, ~256-frame
   buffer, ≥ 5 min, **zero feeder-logged xruns**.
3. The Dart playhead (from `ve_frame_pos`) advances monotonically and tracks wall-clock to
   **< 1 frame of drift over the whole song** — proving the sample-counter clock removes
   drift structurally.
4. `ve_push_pcm` from a Dart isolate under **deliberate GC pressure** → no audio-thread
   stall, no hang; back-pressure shows as a reduced return count, never a block.
5. Unplug/switch the output device mid-playback → recovers (feeder reopens) without crash.
6. `flutter clean && flutter run` reproduces 1–2 from scratch; a `VOCARA_UNIVERSAL=1`
   build produces a `lipo -info`-verified fat dylib.
7. **The headless offline-render binary** (`bin/render.rs`) links the engine as `rlib`,
   renders the same graph deterministically with no cpal, and runs **green under TSan +
   `assert_no_alloc`**. This — not the live OS callback — is the **primary** RT-safety and
   golden-file surface (TSan on the OS-owned CoreAudio RT thread is non-trivial; the live
   callback is validated by the xrun counter + the offline harness, not by instrumenting
   the RT thread).

**Riskiest item:** the **build/FFI bootstrap**, specifically the `@rpath` install_name +
the `script_phase`/embed-phase ordering. It is unproven in this repo and gates everything
else; a wrong install_name passes the build and fails only at runtime `dlopen`. Mitigated
by the `otool -L` smoke check in gate item 1 and `prepare_command` for fresh checkouts.
(The RCU-on-RT-thread question is a *close second* but is fully designed-around in §3.3
with the reaper + escape hatch.)

**Honest reality:** this is **greenfield Rust + FFI on macOS — none of it exists in the
repo today.** The repo is Flutter 3.38.5 / Dart 3.10.4, `package:ffi` is already a
transitive dep (`pubspec.lock:140`), Rust 1.91.1 with both Apple targets is installed, but
there is **zero Rust, zero Cargo.toml, zero FFI wiring.** The current production path —
`media_kit` (libmpv) per-track players with the `Stopwatch`/drift clock — **stays the
default and ships** behind a feature flag. The Rust engine is built and validated entirely
behind that flag and **does not become the default until P1 reaches parity** with the
media_kit path (multi-track sync, seek, loop, playhead) and clears the P0 gate above. Any
review that says "the design describes work that isn't done" is correct *and that is the
point of a P0 spike doc* — this is the plan we build from, not a claim of completion.

---

## 6. What is unchanged

Everything above the implementation language carries over **verbatim.** The graph runtime
(node trait, topo-sort + cycle-reject at edit time, reusable buffer pool, fan-in sum
node), the RCU swap at the block boundary, the sample-counter `g_frame` transport that
deletes per-source drift correction, the node topology, the **shared vocal-reverb send
bus as a real vertex**, the three DSP nodes' exact math (RBJ biquad DF-I recurrence,
feed-forward log-domain soft-knee compressor, Freeverb/Schroeder–Moorer reverb), the
phased plan (P0 substrate → P1 parity/cutover → P2 DSP nodes + golden files → P3
multi-track → P4 live mic), and the test strategy (offline headless render as the primary
RT-safety + golden-file surface) are all identical to the settled language-agnostic
design. **Only the implementation language changes** — Rust supplies compile-time proof on
the lock-free seams and one-crate decode coverage, and nothing else about the architecture
moves.

---

## 7. Transport fidelity boundaries (as shipped)

The output-boundary resampler (non-48 kHz devices) reports the playhead as
`rendered − un-played FIFO backlog`, pause folds that backlog into the clock, seek/pause/
play flush the FIFO, and stream rebuilds clear the shared backlog. Three boundaries are
**deliberate** and documented rather than open bugs:

* **Pause fold vs. an in-flight callback.** `pause()` runs on the control thread; a device
  callback already inside `render_block` when `playing` flips can still advance the render
  clock after the fold. The race is bounded by one device buffer and costs position
  *precision only* — transport state can never corrupt. A hard barrier would mean blocking
  the control thread on the RT callback, which the engine's wait-free contract forbids.
* **DSP state does not rewind.** Resume re-renders the frames the listener never heard,
  but compressor envelopes / reverb tails have already consumed them once — standard DAW
  behavior; snapshotting the whole graph's DSP state per pause is out of proportion to a
  ≤ one-block artifact.
* **A device switch drops the old stream's un-played FIFO tail.** The old stream is gone;
  its ≤ one render block of in-flight audio has no device to reach. Transferring FIFO
  contents across stream generations is not worth the coupling.
* **Loop wrap on a non-48 kHz device reports loop_start while the pre-wrap tail plays.**
  `rendered − backlog` is ambiguous across the wrap (the FIFO's remaining frames belong to
  the pre-wrap segment, whose true position is near loop_end); the playhead clamps to
  loop_start for that ≤ one render block instead of jumping backwards. This is a KNOWN
  SEMANTIC BOUNDARY, not a precision bound — only the consumed-frames cursor below removes
  the ambiguity, because it tracks delivery, not rendering.

The clean long-term shape — noted for the next engine iteration — is a **consumed-frames
cursor**: the output path (identity, resampler, VPIO) publishes the absolute engine-frame
count actually delivered to the device, and `frame()` reads that cursor directly instead
of reconstructing it as `rendered − backlog`. Pause, loop wrap, and stream switches then
fall out for free.
