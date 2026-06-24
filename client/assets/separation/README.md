# Stem separation (prototype engine)

Auto-separates an imported full song into a **backing track** (to sing over) and
an **isolated vocal** (a reference), filling both backing slots of a practice.

## How it's wired

The UI/ViewModel depend only on `SourceSeparationService`
(`lib/data/services/source_separation_service.dart`) — they never know *how*
separation happens. Today the production binding is
`ProcessSourceSeparationService`, which runs `separate.py` (this folder) as a
subprocess and caches stems by a SHA-256 of the source song.

Because everything sits behind that interface, the engine can be swapped later
(e.g. a bundled native ONNX/Core ML engine) **without touching the UI or VM**.

## One-time developer setup

This prototype shells out to Python. On your **dev machine only**:

```sh
pip install "audio-separator[cpu]"      # or [gpu] on CUDA; Apple Silicon: [cpu]
```

That's it — the app **auto-discovers** a Python that has audio-separator (it
probes `python3`, then common conda/homebrew locations like
`/opt/miniconda3/bin/python3`), copies `separate.py` out of its asset bundle and
runs it. The first separation downloads the model (~tens of MB) automatically.

> A macOS app gets a minimal PATH (no conda), so bare `python3` resolves to the
> system Python that lacks the engine — the auto-discovery handles that. You can
> still force a specific interpreter with
> `--dart-define=VOCARA_PYTHON=/path/to/python3` (it's tried first).
>
> The app must also be **non-sandboxed** to spawn the sidecar — DebugProfile has
> the sandbox disabled for this reason (Release stays sandboxed; the shipping
> engine is in-process).

> Shipping note: **end users never run pip.** For distribution the same
> stdout/exit contract is fulfilled by a bundled, self-contained engine (a
> frozen build, or a native ONNX/Core ML runtime) selected behind
> `SourceSeparationService`. The Python path is the prototype only.

## Model

`separate.py` defaults to a Mel-Band Roformer vocal model, overridable with
`--model` or the `VOCARA_SEP_MODEL` env var. List what's available with:

```sh
audio-separator --list_models | grep -i roformer
```

For a commercial release, point this at an explicitly **MIT-licensed** vocal
checkpoint (e.g. ZFTurbo's Mel-Band Roformer) — see the licensing notes captured
in the project memory.

## Contract (`separate.py` ⇄ Dart)

| | |
|---|---|
| args | `--input <song> --instrumental <out.wav> --vocal <out.wav> [--model <name>]` |
| stdout | lines `PROGRESS <0..1>` for the progress bar |
| output | two **16-bit PCM WAV** files (the analysis pipeline only reads 16-bit PCM WAV) |
| exit | `0` success; non-zero on failure (message on stderr) |
