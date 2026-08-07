# Disk-streaming playback migration

The industry-standard multi-track memory model (Pro Tools / Logic / Reaper /
Live): audio streams from disk with a small per-track look-ahead buffer; RAM
scales with track COUNT, never track LENGTH. Vocara's playback currently
decodes every track in full (`ve_tracks_add_pcm` holds an `Arc` of the whole
PCM; `BufferSource` random-accesses it), so a 60-minute session costs
~1.38 GB per stereo float track — the reason a "project memory budget"
question existed at all. Streaming makes the question disappear structurally.

## P0 — transcode at import (DONE)

`AudioEditService.ensureStreamableWav`: compressed media becomes an
engine-rate float WAV ONCE at import, via the punch engine's streamed,
transactional split-0 path (decode budgets apply; output lands atomically).
The song import flows route through it and delete the compressed copy; the
rollback helper covers transcode failures like any failed step. After P0
every playable track is an uncompressed WAV — a seekable byte range, no
decoder needed on the playback path.

- Disk cost: ~1.4 GB/hour of media — negligible at practice-take lengths,
  and the price every DAW pays for O(1) seek.
- Also removes the decode-every-open cost: the WAV is read, not re-decoded.

## P1 — file-backed sources (OPEN; do with the consumed-cursor round)

Replace the full-RAM `Arc` behind `BufferSource` with per-track file feeders:

- **Feeder thread** (one, multiplexing all tracks): reads each track's WAV in
  chunks ahead of the playhead into a per-track SPSC ring (2–4 s deep,
  ~1.5 MB/track at 48 kHz stereo f32). The RT graph node pops frames; on
  underrun it emits silence and counts (the existing `RingSource` shape, plus
  timeline placement).
- **Timeline placement**: today `BufferSource` maps `io.tl` to a buffer
  offset per block, which makes seek free. A ring-fed source must instead be
  POSITIONED: the feeder derives each track's file offset from the transport
  frame, and the node trusts ring order. Every transport discontinuity —
  seek, loop wrap, punch — must flush the rings and reposition the feeder
  (the exact protocol `out_flush_gen` already implements for the OUTPUT
  boundary; sources need the same generation handshake on the input side).
- **Loop wrap** is a feeder-side re-seek at the loop boundary — which is why
  this belongs in the SAME round as the consumed-frames cursor: both
  redefine how transport position maps to what the callback consumes, and
  the flush/generation machinery is shared.
- **Recording playback (fresh takes)** already stream from disk via the
  recovery reader; the migration unifies them with imported tracks.
- **Cache**: the `ve_tracks_add` path-keyed PCM cache disappears; the file
  IS the cache. `ve_tracks_add_pcm` survives only for ephemeral PCM
  (technique practice takes) or is retired.

### Invariants to pin in tests

1. Sample-accurate equivalence: a streamed render of a project equals the
   Arc-backed render bit-for-bit (offline render harness exists).
2. Seek/loop flush: no pre-seek audio audible after a discontinuity (the
   out_flush_gen tests are the template).
3. Underrun accounting: a starved ring produces counted silence, never
   stale frames.
4. Memory ceiling: rings + chunk buffers only — assert with the RSS
   measurement harness (test/measure_analysis_peak_test.dart pattern).

## Explicitly out of scope until P1 lands

- A project-level RAM budget (P1 makes it moot; an interim LRU of decoded
  Arcs was considered and rejected as a patch that delays the real fix).
