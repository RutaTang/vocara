#!/usr/bin/env python3
"""Vocara source-separation sidecar (prototype engine).

Splits a full song into an instrumental stem (to sing over) and an isolated
vocal stem (a reference), using a Mel-Band Roformer model via `audio-separator`.

Contract with the Dart side (ProcessSourceSeparationService):
    args:   --input <song>  --instrumental <out.wav>  --vocal <out.wav>
            [--model <model_filename>]
    stdout: lines "PROGRESS <0..1>" drive the progress bar
    output: two 16-bit PCM WAV files at the requested paths (the analysis
            pipeline only accepts 16-bit PCM WAV)
    exit:   0 on success, non-zero on failure (human message on stderr)

This is the PROTOTYPE engine and the only piece that needs a one-time setup on
the developer machine:

    pip install "audio-separator[cpu]"      # or [gpu]

End users never install anything: for distribution the same stdout/exit
contract is fulfilled by a bundled, self-contained binary (a frozen build, or a
native ONNX/Core ML engine), selected behind SourceSeparationService.

Model note: the default below is overridable with --model or the VOCARA_SEP_MODEL
env var. For a commercial release point it at an explicitly MIT-licensed vocal
checkpoint (e.g. ZFTurbo's Mel-Band Roformer). Run `audio-separator --list_models`
to see what is available locally.
"""

import argparse
import os
import shutil
import sys
import tempfile

# An INSTRUMENTAL-specialist Mel-Band Roformer: it predicts the backing directly
# (rather than mixture - vocals), which removes far more vocal/breath residue and
# handles reverb tails better. Overridable via --model / $VOCARA_SEP_MODEL.
# For a commercial release, verify the checkpoint's license — this default is a
# community (unlicensed) checkpoint, fine for the prototype only.
DEFAULT_MODEL = "mel_band_roformer_instrumental_becruily.ckpt"


def emit(progress):
    """Report progress to the Dart side."""
    print(f"PROGRESS {progress:.3f}", flush=True)


def reencode_pcm16(src, dst):
    """Rewrite a model output WAV as 16-bit PCM (what the analysis path reads)."""
    import soundfile as sf

    data, sr = sf.read(src, always_2d=True)
    # The Dart side passes atomic-staging paths ending in ".wav.tmp", so soundfile
    # cannot infer the container from the extension — name the format explicitly.
    sf.write(dst, data, sr, subtype="PCM_16", format="WAV")


def pick(paths, *needles):
    """First path whose basename contains any of the case-insensitive needles."""
    for path in paths:
        name = os.path.basename(path).lower()
        if any(n in name for n in needles):
            return path
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--instrumental", required=True)
    parser.add_argument("--vocal", required=True)
    parser.add_argument(
        "--model",
        default=os.environ.get("VOCARA_SEP_MODEL", DEFAULT_MODEL),
    )
    args = parser.parse_args()

    emit(0.02)
    try:
        from audio_separator.separator import Separator
    except ImportError:
        print(
            'audio-separator is not installed. Run: pip install "audio-separator[cpu]"',
            file=sys.stderr,
        )
        return 3

    workdir = tempfile.mkdtemp(prefix="vocara_sep_")
    # The workdir holds full-length raw stem WAVs (tens of MB); always remove it,
    # on success/failure/crash alike, so it doesn't leak into the temp directory.
    try:
        separator = Separator(output_dir=workdir, output_format="WAV")
        emit(0.05)

        try:
            separator.load_model(model_filename=args.model)
        except Exception as exc:  # noqa: BLE001 - surface any load failure verbatim
            # Advice first, verbatim cause LAST: the Dart side surfaces the last
            # non-empty stderr line to the user, so the actual error must be it.
            print(
                "Run `audio-separator --list_models` and pass a valid --model.\n"
                f"Failed to load model '{args.model}': {exc}",
                file=sys.stderr,
            )
            return 4
        emit(0.15)

        # The long step: no fine-grained callback, so progress jumps here.
        outputs = separator.separate(args.input)
        emit(0.85)

        paths = [o if os.path.isabs(o) else os.path.join(workdir, o) for o in outputs]
        # audio-separator labels stems in PARENTHESES, e.g. "(Instrumental)" /
        # "(Vocals)". Match the parenthesized label — NOT a bare "instrument" — because
        # the model name itself can contain "instrumental" (e.g. an instrumental
        # specialist like ..._instrumental_becruily), which would otherwise make the
        # "(Vocals)" file false-match the instrumental needle.
        inst_src = pick(paths, "(instrumental)", "(no_vocals)", "(no vocals)",
                        "(other)", "(accompaniment)", "(inst)")
        vocal_src = pick(paths, "(vocals)", "(vocal)")
        # With exactly two stems, the non-vocal one is the instrumental (covers any
        # odd labelling not matched above).
        if len(paths) == 2:
            if inst_src is None and vocal_src is not None:
                inst_src = next(p for p in paths if p != vocal_src)
            elif vocal_src is None and inst_src is not None:
                vocal_src = next(p for p in paths if p != inst_src)

        if inst_src is None or vocal_src is None or inst_src == vocal_src:
            print(
                "Could not identify the instrumental + vocal stems in: "
                f"{[os.path.basename(p) for p in paths]}.",
                file=sys.stderr,
            )
            return 5

        reencode_pcm16(vocal_src, args.vocal)
        emit(0.92)
        reencode_pcm16(inst_src, args.instrumental)
        emit(1.0)
        return 0
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
