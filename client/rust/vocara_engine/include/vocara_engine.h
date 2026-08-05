/* vocara_engine C ABI — the surface the Flutter/Dart side binds via ffigen.
 *
 * Hand-authored to match src/ffi.rs exactly; regenerate with `cbindgen` (cbindgen.toml).
 * All control calls are non-blocking; the audio data plane is the cpal callback, never
 * crossing this boundary. POD only.
 */
#ifndef VOCARA_ENGINE_H
#define VOCARA_ENGINE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque engine handle. */
typedef struct VeEngine VeEngine;

/* Decoded PCM handed back from ve_decode_file (interleaved stereo f32, `frames * 2`
 * floats). Free with ve_free_pcm. */
typedef struct VePcm {
  float *ptr;
  size_t frames;
  uint32_t rate;
} VePcm;

/* Audio device descriptor (ve_enum_output_devices). Fixed-size NUL-terminated strings keep
 * it POD across the ABI. */
typedef struct VeDeviceInfo {
  char uid[128];        /* stable CoreAudio device UID — the persist key */
  char name[128];       /* human-readable name */
  uint32_t rate;        /* native sample rate (0 if unknown) */
  uint8_t is_default;   /* 1 if the system default */
  uint8_t is_input;     /* 1 = input device, 0 = output */
  uint8_t is_bluetooth; /* 1 if on a Bluetooth transport (same-device in+out degrades output) */
} VeDeviceInfo;

/* Create the engine: build the graph at the fixed engine rate (48 kHz) and spawn the audio-
 * host thread, which opens the default output and resamples to its native rate at the I/O
 * boundary. Returns NULL on failure. */
VeEngine *ve_create(void);
void ve_destroy(VeEngine *e);

/* Device selection. Enumeration is a static host query (no engine needed); switching routes
 * through the audio-host thread that owns the stream. */
/* Fill up to `cap` entries into `out`; returns the total device count (may exceed `cap`). */
size_t ve_enum_output_devices(VeDeviceInfo *out, size_t cap);
/* Switch the active output device by UID. 0 = ok, negative = failure (falls back to default). */
int ve_set_output_device(VeEngine *e, const char *uid);
/* Write the active output UID into `buf` (<= cap, NUL-terminated); returns its byte length. */
size_t ve_current_output_device(VeEngine *e, char *buf, size_t cap);
/* Poll + clear pending device events (bit 1 = output lost, bit 2 = input lost). 0 = none. */
uint32_t ve_poll_device_event(VeEngine *e);

/* Live input + monitoring. The input stream is opened only while monitoring/recording. */
/* Fill up to `cap` input devices into `out`; returns the total input device count. */
size_t ve_enum_input_devices(VeDeviceInfo *out, size_t cap);
/* Select the input device by UID (reopens live if monitoring). 0 = ok, negative = failure. */
int ve_set_input_device(VeEngine *e, const char *uid);
/* Write the selected input UID into `buf` (<= cap, NUL-terminated); returns its byte length. */
size_t ve_current_input_device(VeEngine *e, char *buf, size_t cap);
/* Start monitoring: open the input stream + ramp the monitor in. 0 = ok. */
int ve_start_monitor(VeEngine *e);
/* Stop monitoring: ramp the monitor out + close the input stream. 0 = ok. */
int ve_stop_monitor(VeEngine *e);
/* Set the live-input monitor gain (linear, 0 = muted). 0 = ok. */
int ve_set_monitor_gain(VeEngine *e, float gain);
/* Set the record/input gain (linear): makeup applied to the captured voice before record/analysis. */
int ve_set_input_gain(VeEngine *e, float gain);
/* Set the monitor reverb (room / damping / wet, each 0..1). 0 = ok. */
int ve_set_monitor_reverb(VeEngine *e, float room, float damping, float wet);
/* Current live-input peak level (0..1) for a meter; 0 when not capturing. */
float ve_input_level(VeEngine *e);
/* Diagnostic + pre-roll clock: total ENGINE-RATE (48 kHz, post-resample) frames the mic
 * callback has emitted (0 = mic delivering nothing). Engine-rate on purpose: callers divide
 * by 48000 to get time. */
uint64_t ve_input_frames_received(VeEngine *e);
/* Host-thread-authoritative input-stream state word: (generation << 1) | running. Running
 * set only after a successful start, cleared BEFORE any teardown joins the old callback;
 * the generation bumps on every start. Handshake: read, verify frames advanced, read again
 * -- an unchanged word with bit 0 set proves the SAME stream delivered those frames. */
uint64_t ve_input_stream_state(VeEngine *e);
/* Estimated record round-trip latency in frames @ 48 kHz (output IO + acoustic + input IO + AEC
   block). Shift recorded takes earlier by this so they land on the beat the singer heard. */
uint32_t ve_record_latency_frames(VeEngine *e);

/* Live capture (analysis) + recording — both use the engine's selected input device (cpal). */
/* Begin/stop capturing input frames for analysis (drained by ve_input_frames). 0 = ok. */
int ve_start_capture(VeEngine *e);
int ve_stop_capture(VeEngine *e);
/* Pop up to `max_frames` captured interleaved-stereo frames (48 kHz) into `out`
 * (max_frames*2 floats). Returns frames written. Non-blocking. */
size_t ve_input_frames(VeEngine *e, float *out, size_t max_frames);
/* Record the live input to a mono 48 kHz WAV at `path` (writer thread). Blocks briefly on
 * the writer creating the file. 0 = ok; -1 = the file could not be created (disk full /
 * permissions) and nothing will be recorded. */
int ve_start_input_recording(VeEngine *e, const char *path);
/* Stop the input recording (finalizes the WAV). 0 = ok; -2 = an IO error occurred during
 * the recording (the WAV at the recorded path is missing, damaged, or incomplete). */
int ve_stop_input_recording(VeEngine *e);
/* Frames the recording ring dropped during the current/latest take (reset at each start).
 * Nonzero = the take on disk is missing that many frames (writer thread stalled past the
 * ring's headroom) — the UI must not present the take as gapless. */
uint64_t ve_record_dropped_frames(VeEngine *e);
/* Frames WRITTEN into the recording ring for the current take (engine-rate; reset at each
 * start) — the per-take recorded-progress ack. Nonzero proves audio is in this take's
 * ring; at play time it IS the take's file offset (the pre-roll). */
uint64_t ve_record_written_frames(VeEngine *e);
/* The newest published input-intent sequence (read after arming = the arm's watermark). */
uint64_t ve_input_intent_seq(VeEngine *e);
/* The intent sequence the audio host last CONVERGED to. applied >= S proves the host has
 * acted on an intent at least as new as S — the ordering half of the ready handshake. */
uint64_t ve_input_applied_seq(VeEngine *e);

int ve_play(VeEngine *e);
int ve_pause(VeEngine *e);
int ve_seek(VeEngine *e, int64_t frame);
int ve_set_loop(VeEngine *e, int enabled, int64_t start, int64_t end);

/* Queue a coalesced parameter change; ve_flush publishes queued changes to the audio thread. */
int ve_set_param(VeEngine *e, uint32_t node, uint16_t param, float value);
int ve_flush(VeEngine *e);

/* Current playhead in samples (atomic read). */
int64_t ve_frame_pos(VeEngine *e);
double ve_sample_rate(VeEngine *e);

/* Multi-track playback: one source per track, timeline-aligned. Stage with begin/add, then
 * commit to build + hot-swap the graph; change volume/mute live without a rebuild.
 * ve_tracks_add stages from the decoded-PCM cache ONLY and returns -1 on a miss — it never
 * decodes on the calling thread; decode off-thread (ve_decode) and stage the samples with
 * ve_tracks_add_pcm. A successful commit evicts cached PCM whose path is not in the new
 * graph (keeping a bounded recently-staged tail), so the cache can't grow unbounded. */
int ve_tracks_begin(VeEngine *e);
int ve_tracks_add(VeEngine *e, const char *path, int64_t start_ms, float gain, int muted,
                  int is_vocal, int bus);
int ve_tracks_commit(VeEngine *e);
int ve_set_track_volume(VeEngine *e, int index, float gain, int muted);
/* Live stereo pan for a committed track ([-1, 1]; 0 = center). */
int ve_set_pan(VeEngine *e, int index, float pan);

/* FX buses (DAW group/aux). Stage with buses_begin/add BEFORE staging tracks (so a track can
 * route into a bus by index), then ve_tracks_commit builds both. `output` is another staged
 * bus index, or -1 for the master. */
int ve_buses_begin(VeEngine *e);
int ve_buses_add(VeEngine *e, float gain, int muted, int output);
/* Insert-chain order (FxKind tags, e.g. {0,1} = EQ→Comp). Stage after add, before commit. */
int ve_track_chain(VeEngine *e, int track, const uint8_t *tags, size_t n);
int ve_bus_chain(VeEngine *e, int bus, const uint8_t *tags, size_t n);
/* Post-fader aux sends: parallel target-bus-index + level-dB arrays. Stage before commit;
 * change a level live with ve_set_send_level. */
int ve_track_sends(VeEngine *e, int track, const int32_t *targets, const float *levels_db, size_t n);
int ve_bus_sends(VeEngine *e, int bus, const int32_t *targets, const float *levels_db, size_t n);
int ve_set_send_level(VeEngine *e, int track, int slot, float level_db);
int ve_set_bus_send_level(VeEngine *e, int bus, int slot, float level_db);

/* ve_decode status codes (written through its `status` out-pointer). */
#define VE_DECODE_OK 0
#define VE_DECODE_ERR -1
#define VE_DECODE_TOO_LONG -2
/* Stateless decode for a background isolate (no engine state touched). `max_frames` (at the
 * output `rate`; 0 = unbounded) caps the decoded duration BEFORE the full track allocates —
 * a too-long file sets VE_DECODE_TOO_LONG in `status` (nullable) and returns empty PCM. */
VePcm ve_decode(const char *path, uint32_t rate, uint64_t max_frames, int *status);
void ve_free_decoded(float *ptr, size_t frames);
/* Declared duration of a media file in ms WITHOUT decoding (container probe only) — the
 * import gate's cheap pre-check. -1 = container declares no length; -2 = unreadable. */
int64_t ve_probe_duration_ms(const char *path);
/* Punch-in splice: write old[0..split_ms] ++ new to out_path as a 16-bit WAV.
   Stateless; returns the output frame count or -1. */
int64_t ve_punch_wav(const char *old_path, const char *new_path, int64_t split_ms,
                     uint32_t rate, const char *out_path);
/* Master bus output gain (linear, 1.0 = unity), feeding the master soft-clip. */
int ve_set_master(VeEngine *e, float gain);
/* Stage a track from already-decoded PCM (from ve_decode). */
int ve_tracks_add_pcm(VeEngine *e, const char *path, const float *pcm, size_t frames,
                      int64_t start_ms, float gain, int muted, int is_vocal, int bus);

/* Per-track effects (vocal tracks) + the shared reverb bus. `track` is the committed index;
 * a param on a node a backing track lacks is a harmless no-op. */
int ve_set_eq_band(VeEngine *e, int track, int band, float freq, float q, float gain_db,
                   int enabled);
int ve_set_comp(VeEngine *e, int track, float threshold_db, float ratio, float knee_db,
                float attack_ms, float release_ms, float makeup_db, int bypass);
int ve_set_send(VeEngine *e, int track, float send_db);
int ve_set_reverb(VeEngine *e, float room, float damping, float wet, float dry, float width);
/* The vocal compressor's current gain reduction (dB, >= 0) for a track — poll for a meter. */
float ve_track_gr(VeEngine *e, int track);

/* FX bus effects (0-based bus index) — mirror the per-track setters. */
int ve_set_bus_eq_band(VeEngine *e, int bus, int band, float freq, float q, float gain_db,
                       int enabled);
int ve_set_bus_comp(VeEngine *e, int bus, float threshold_db, float ratio, float knee_db,
                    float attack_ms, float release_ms, float makeup_db, int bypass);
int ve_set_bus_send(VeEngine *e, int bus, float send_db);
int ve_set_bus_volume(VeEngine *e, int bus, float gain, int muted);
int ve_set_bus_pan(VeEngine *e, int bus, float pan);
float ve_bus_gr(VeEngine *e, int bus);

/* Extra insert plugins (track index is 1-based-less, bus index is 0-based). bypass != 0 off. */
int ve_set_deesser(VeEngine *e, int track, float freq, float threshold_db, float range_db, int bypass);
int ve_set_bus_deesser(VeEngine *e, int bus, float freq, float threshold_db, float range_db, int bypass);
int ve_set_gate(VeEngine *e, int track, float threshold_db, float range_db, float attack_ms, float release_ms, int bypass);
int ve_set_bus_gate(VeEngine *e, int bus, float threshold_db, float range_db, float attack_ms, float release_ms, int bypass);
int ve_set_delay(VeEngine *e, int track, float time_ms, float feedback, float mix, int bypass);
int ve_set_bus_delay(VeEngine *e, int bus, float time_ms, float feedback, float mix, int bypass);
int ve_set_saturation(VeEngine *e, int track, float drive_db, float mix, int bypass);
int ve_set_bus_saturation(VeEngine *e, int bus, float drive_db, float mix, int bypass);
int ve_set_doubler(VeEngine *e, int track, float delay_ms, float depth_ms, float rate_hz, float mix, int bypass);
int ve_set_bus_doubler(VeEngine *e, int bus, float delay_ms, float depth_ms, float rate_hz, float mix, int bypass);
int ve_set_limiter(VeEngine *e, int track, float ceiling_db, float release_ms, int bypass);
int ve_set_bus_limiter(VeEngine *e, int bus, float ceiling_db, float release_ms, int bypass);
/* Reverb as a series insert (wet 0 = off; dry stays unity). */
int ve_set_reverb_insert(VeEngine *e, int track, float room, float damping, float wet, float width);
int ve_set_bus_reverb_insert(VeEngine *e, int bus, float room, float damping, float wet, float width);

/* Decode a file to interleaved-stereo f32 at the engine's device rate. */
VePcm ve_decode_file(VeEngine *e, const char *path);
void ve_free_pcm(VePcm pcm);

#ifdef __cplusplus
}
#endif

#endif /* VOCARA_ENGINE_H */
