//! The calls on one engine instance (the per-engine half of the Dart
//! `VocaraEngineBindings`).

use super::EngineError;

/// One engine instance's C ABI calls, as safe methods: the seam the playback,
/// recording and monitor services drive. [`super::create`] makes the native
/// one (`NativeEngine`, `device` builds); a test fakes it.
///
/// The methods take the C ABI's own types: committed track and bus indices are
/// `i32` (`-1` names the master where a call routes), gains and FX parameters
/// `f32`, frames `i64`/`u64` at [`super::ENGINE_RATE`]. A status the engine
/// returns becomes a `Result` ([`EngineError::Refused`] carrying its code), an
/// index it returns `Ok(index)`; the getters return what the engine returns,
/// which for an unavailable engine is its null-engine answer (0, 0.0, "").
///
/// Every method has a default body that panics naming the call, as Dart's
/// `Fake` threw: a test fake overrides only the calls its scenario expects,
/// and an unexpected one fails the test instead of hiding. The native engine
/// overrides them all (clippy's `missing_trait_methods` is denied on it).
#[allow(clippy::too_many_arguments)]
pub trait EngineBindings: Send + Sync {
    /// Destroys the engine (`ve_destroy`): stops audio, joins its threads and
    /// frees it. Idempotent; every call after it answers as an unavailable
    /// engine. Dropping the native engine does the same.
    fn destroy(&self) {
        unexpected("ve_destroy")
    }

    // --- Output device -------------------------------------------------------

    /// Switches the output device by uid (`ve_set_output_device`). Ok when the
    /// pick was acted on: playback is on it, or on the default output when it
    /// could not be opened, or (while the mic unit owns the output) the pick is
    /// recorded for when the mic closes; [`EngineBindings::actual_output_device`]
    /// says what renders. Err when nothing could be opened.
    fn set_output_device(&self, uid: &str) -> Result<(), EngineError> {
        let _ = uid;
        unexpected("ve_set_output_device")
    }

    /// The REQUESTED output uid (`ve_current_output_device`); "" = follow the
    /// system default. Resubmitting it re-asserts the intent, never a device.
    fn current_output_device(&self) -> String {
        unexpected("ve_current_output_device")
    }

    /// The uid the engine is ACTUALLY rendering to (`ve_actual_output_device`;
    /// "" = nothing open). It diverges from the requested one when that could
    /// not be opened, and for as long as the duplex VPIO unit owns the output,
    /// where playback follows the mic's endpoint or the system default. Judge
    /// monitor-feedback safety from this one.
    fn actual_output_device(&self) -> String {
        unexpected("ve_actual_output_device")
    }

    /// Polls and clears the pending device events selected by `mask`
    /// (`ve_poll_device_event`, bits in [`super::events`]). Returns the claimed
    /// bits; bits outside the mask stay pending for their own consumer, so the
    /// song page's watch and the technique page's tick no longer steal each
    /// other's events. Also ticks the clock-drift estimator and re-derives the
    /// monitor's safety mute. 0 = none.
    fn poll_device_event(&self, mask: u32) -> u32 {
        let _ = mask;
        unexpected("ve_poll_device_event")
    }

    /// Total streaming-source underruns (`ve_stream_xruns`): frames a
    /// disk-streamed track rendered silent because its ring was empty. 0 is
    /// healthy; it never decreases, across commits and dropped feeders.
    fn stream_xruns(&self) -> u64 {
        unexpected("ve_stream_xruns")
    }

    /// Staged-track indices the last SUCCESSFUL commit could not open as
    /// streaming sources, one bit each (`ve_commit_degraded_mask`). Such a
    /// track is in the graph but renders silence forever while the commit
    /// reports success, so this is the only way to tell a good commit from a
    /// degraded one. Indices past bit 63 fold into the top bit. 0 = the graph
    /// is exactly what was staged.
    fn commit_degraded_mask(&self) -> u64 {
        unexpected("ve_commit_degraded_mask")
    }

    /// The clock-drift estimator's input correction in ppm (`ve_drift_ppm`;
    /// 0.0 = identity, deadband or frozen). Diagnostics only.
    fn drift_ppm(&self) -> f64 {
        unexpected("ve_drift_ppm")
    }

    // --- Input device, monitoring ---------------------------------------------

    /// Selects the input device by uid (`ve_set_input_device`; reopens live if
    /// the input is open). Fire-and-forget inside the engine: compare
    /// [`EngineBindings::actual_input_device`] to detect a fallback.
    fn set_input_device(&self, uid: &str) -> Result<(), EngineError> {
        let _ = uid;
        unexpected("ve_set_input_device")
    }

    /// The selected input uid (`ve_current_input_device`; "" = system default).
    fn current_input_device(&self) -> String {
        unexpected("ve_current_input_device")
    }

    /// The uid the OPEN input stream is ACTUALLY bound to
    /// (`ve_actual_input_device`; "" = closed, or the system default): what a
    /// take really records from.
    fn actual_input_device(&self) -> String {
        unexpected("ve_actual_input_device")
    }

    /// Arms the live input (`ve_start_monitor`): opens the mic stream so the
    /// meter and recording work. The audible level is the monitor gain alone.
    fn start_monitor(&self) -> Result<(), EngineError> {
        unexpected("ve_start_monitor")
    }

    /// Disarms the live input (`ve_stop_monitor`): closes the mic stream if
    /// nothing else needs it.
    fn stop_monitor(&self) -> Result<(), EngineError> {
        unexpected("ve_stop_monitor")
    }

    /// The hear-yourself monitor gain, linear, 0 = muted (`ve_set_monitor_gain`).
    fn set_monitor_gain(&self, gain: f32) -> Result<(), EngineError> {
        let _ = gain;
        unexpected("ve_set_monitor_gain")
    }

    /// The record/input gain, linear (`ve_set_input_gain`): the mic fader's
    /// makeup applied to the captured voice before recording and analysis.
    fn set_input_gain(&self, gain: f32) -> Result<(), EngineError> {
        let _ = gain;
        unexpected("ve_set_input_gain")
    }

    /// Names the output judged echo-free (headphones), or clears it with ""
    /// (`ve_set_echo_free`): while that device is the one actually rendering,
    /// the echo canceller passes the mic through untouched.
    fn set_echo_free(&self, uid: &str) -> Result<(), EngineError> {
        let _ = uid;
        unexpected("ve_set_echo_free")
    }

    /// Which input(s) of a multi-channel mic make the mono capture
    /// (`ve_set_input_channel`, modes [`super::INPUT_CHANNEL_AUTO`] and on).
    /// Refused for an unknown mode.
    fn set_input_channel(&self, mode: u32) -> Result<(), EngineError> {
        let _ = mode;
        unexpected("ve_set_input_channel")
    }

    /// The monitor reverb: room, damping, wet, each 0..1 (`ve_set_monitor_reverb`).
    fn set_monitor_reverb(&self, room: f32, damping: f32, wet: f32) -> Result<(), EngineError> {
        let _ = (room, damping, wet);
        unexpected("ve_set_monitor_reverb")
    }

    /// The live input's peak level, 0..1, for a meter: the loudest sample
    /// since the previous call, which clears it, so a 50 ms poll misses no
    /// buffer's peak; 0 when not capturing (`ve_input_level`).
    fn input_level(&self) -> f32 {
        unexpected("ve_input_level")
    }

    /// Total ENGINE-RATE frames the mic callback has emitted
    /// (`ve_input_frames_received`; 0 = the mic is delivering nothing): the
    /// diagnostic and pre-roll clock. Engine-rate so callers divide by 48 000.
    fn input_frames_received(&self) -> u64 {
        unexpected("ve_input_frames_received")
    }

    /// The input-stream state word, `(generation << 1) | running`
    /// (`ve_input_stream_state`). Running is set only after a successful start
    /// and cleared BEFORE any teardown; the generation bumps on every start.
    /// The ready handshake reads it, checks the frames advanced, and reads it
    /// again: an unchanged word with bit 0 set proves the SAME stream
    /// delivered them.
    fn input_stream_state(&self) -> u64 {
        unexpected("ve_input_stream_state")
    }

    /// The record round-trip latency in frames at 48 kHz
    /// (`ve_record_latency_frames`; output IO + acoustic + input IO + AEC
    /// block). Recorded takes shift earlier by it to land on the beat the
    /// singer heard. 0 until the mic has opened once.
    fn record_latency_frames(&self) -> u32 {
        unexpected("ve_record_latency_frames")
    }

    // --- Capture, recording ---------------------------------------------------

    /// Starts capturing input frames for live analysis (`ve_start_capture`;
    /// opens the input stream). A new session starts from an empty ring.
    fn start_capture(&self) -> Result<(), EngineError> {
        unexpected("ve_start_capture")
    }

    /// Stops capturing (`ve_stop_capture`; closes the input if nothing else needs it).
    fn stop_capture(&self) -> Result<(), EngineError> {
        unexpected("ve_stop_capture")
    }

    /// Pops captured interleaved-stereo frames at 48 kHz into `out`, at most
    /// `out.len() / 2` of them (`ve_input_frames`). Returns the frames
    /// written; non-blocking.
    fn input_frames(&self, out: &mut [f32]) -> usize {
        let _ = out;
        unexpected("ve_input_frames")
    }

    /// Records the live input to a mono 48 kHz float WAV at `path` on the
    /// writer thread (`ve_start_input_recording`). Blocks briefly on the writer
    /// creating the file; refused when it could not be (disk full,
    /// permissions), and then nothing will be recorded.
    fn start_input_recording(&self, path: &str) -> Result<(), EngineError> {
        let _ = path;
        unexpected("ve_start_input_recording")
    }

    /// Stops the recording and finalizes the WAV (`ve_stop_input_recording`).
    /// [`EngineError::RecordingIo`] when an I/O error occurred during it. Waits
    /// (up to 250 ms) for the callback in flight to land its tail first.
    fn stop_input_recording(&self) -> Result<(), EngineError> {
        unexpected("ve_stop_input_recording")
    }

    /// Frames the recording ring dropped in the current or latest take
    /// (`ve_record_dropped_frames`; reset at each start). Nonzero: the take on
    /// disk has holes and must not be presented as gapless.
    fn record_dropped_frames(&self) -> u64 {
        unexpected("ve_record_dropped_frames")
    }

    /// Frames written into the recording ring for the CURRENT take
    /// (`ve_record_written_frames`; engine-rate, reset at each start): the
    /// per-take progress ack, and at play time the take's file offset.
    fn record_written_frames(&self) -> u64 {
        unexpected("ve_record_written_frames")
    }

    /// The newest published input-intent sequence (`ve_input_intent_seq`):
    /// read after arming, the arm's watermark.
    fn input_intent_seq(&self) -> u64 {
        unexpected("ve_input_intent_seq")
    }

    /// The host's last SUCCESSFUL convergence as
    /// `(intent_seq << 24) | input_generation` (`ve_input_applied_state`).
    /// Ready contract: `(applied >> 24) >= arm_seq` and the state word shows
    /// running with the SAME generation.
    fn input_applied_state(&self) -> u64 {
        unexpected("ve_input_applied_state")
    }

    // --- Transport ------------------------------------------------------------

    /// Starts the transport (`ve_play`).
    fn play(&self) -> Result<(), EngineError> {
        unexpected("ve_play")
    }

    /// Pauses the transport (`ve_pause`).
    fn pause(&self) -> Result<(), EngineError> {
        unexpected("ve_pause")
    }

    /// Moves the playhead to `frame` (`ve_seek`).
    fn seek(&self, frame: i64) -> Result<(), EngineError> {
        let _ = frame;
        unexpected("ve_seek")
    }

    /// Sets the loop range in frames (`ve_set_loop`); while enabled, playback
    /// wraps at `end` back to `start`.
    fn set_loop(&self, enabled: bool, start: i64, end: i64) -> Result<(), EngineError> {
        let _ = (enabled, start, end);
        unexpected("ve_set_loop")
    }

    /// Queues a coalesced parameter change (`ve_set_param`); [`EngineBindings::flush`]
    /// publishes the queue to the audio thread.
    fn set_param(&self, node: u32, param: u16, value: f32) -> Result<(), EngineError> {
        let _ = (node, param, value);
        unexpected("ve_set_param")
    }

    /// Publishes queued parameter changes (`ve_flush`).
    fn flush(&self) -> Result<(), EngineError> {
        unexpected("ve_flush")
    }

    /// The playhead in frames (`ve_frame_pos`, an atomic read; it also reaps
    /// retired graphs, so polling it bounds their memory).
    fn frame_pos(&self) -> i64 {
        unexpected("ve_frame_pos")
    }

    /// The engine's sample rate (`ve_sample_rate`).
    fn sample_rate(&self) -> f64 {
        unexpected("ve_sample_rate")
    }

    // --- Graph staging --------------------------------------------------------

    /// Begins staging a track set (`ve_tracks_begin`). One source per track,
    /// timeline-aligned; a commit builds and hot-swaps the graph, and volume,
    /// mute and FX change live without a rebuild.
    fn tracks_begin(&self) -> Result<(), EngineError> {
        unexpected("ve_tracks_begin")
    }

    /// Stages a track from the engine's decoded-PCM cache (`ve_tracks_add`) at
    /// `start_ms`, routed to staged bus `bus` (-1 = master). A canonical
    /// engine-rate WAV stages as a disk stream with no decode. `Ok(index)`;
    /// refused on a cache miss, or a file changed on disk since it was cached:
    /// it never decodes on the calling thread, so the caller decodes off it
    /// ([`super::decode`]) and stages the samples with
    /// [`EngineBindings::tracks_add_pcm`].
    fn tracks_add(
        &self,
        path: &str,
        start_ms: i64,
        gain: f32,
        muted: bool,
        is_vocal: bool,
        bus: i32,
    ) -> Result<i32, EngineError> {
        let _ = (path, start_ms, gain, muted, is_vocal, bus);
        unexpected("ve_tracks_add")
    }

    /// Stages a track from decoded interleaved-stereo PCM at the engine rate
    /// (`ve_tracks_add_pcm`, `pcm.len() / 2` frames), caching it under `path`
    /// (replacing what was cached there). The engine copies the samples.
    fn tracks_add_pcm(
        &self,
        path: &str,
        pcm: &[f32],
        start_ms: i64,
        gain: f32,
        muted: bool,
        is_vocal: bool,
        bus: i32,
    ) -> Result<i32, EngineError> {
        let _ = (path, pcm, start_ms, gain, muted, is_vocal, bus);
        unexpected("ve_tracks_add_pcm")
    }

    /// Builds the staged graph and puts it live (`ve_tracks_commit`). Refused:
    /// the engine rejected the topology and kept the old graph. A successful
    /// commit evicts cached PCM no longer in the graph (keeping a bounded
    /// recently-staged tail).
    fn tracks_commit(&self) -> Result<(), EngineError> {
        unexpected("ve_tracks_commit")
    }

    /// [`EngineBindings::tracks_commit`], but the new graph is held back from
    /// the audio thread until [`EngineBindings::tracks_publish`]
    /// (`ve_tracks_commit_staged`): every setter in between applies to it, so
    /// its first block already carries the re-pushed live values. The old
    /// graph keeps playing meanwhile.
    fn tracks_commit_staged(&self) -> Result<(), EngineError> {
        unexpected("ve_tracks_commit_staged")
    }

    /// Puts a staged commit live (`ve_tracks_publish`); a no-op when nothing
    /// is staged.
    fn tracks_publish(&self) -> Result<(), EngineError> {
        unexpected("ve_tracks_publish")
    }

    /// Live volume and mute for a committed track (`ve_set_track_volume`);
    /// `muted` forces silence whatever the gain.
    fn set_track_volume(&self, index: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        let _ = (index, gain, muted);
        unexpected("ve_set_track_volume")
    }

    /// Live stereo pan for a committed track, [-1, 1], 0 = center (`ve_set_pan`).
    fn set_pan(&self, index: i32, pan: f32) -> Result<(), EngineError> {
        let _ = (index, pan);
        unexpected("ve_set_pan")
    }

    /// Begins staging FX buses (`ve_buses_begin`): BEFORE the tracks, so a
    /// track can route into a bus by index.
    fn buses_begin(&self) -> Result<(), EngineError> {
        unexpected("ve_buses_begin")
    }

    /// Stages one FX bus (`ve_buses_add`): its output is another staged bus
    /// index or -1 for the master. `Ok(index)`.
    fn buses_add(&self, gain: f32, muted: bool, output: i32) -> Result<i32, EngineError> {
        let _ = (gain, muted, output);
        unexpected("ve_buses_add")
    }

    /// A staged track's insert-chain order as `FxKind` tags, e.g. `[0, 1]` =
    /// EQ then comp (`ve_track_chain`); empty = the default. After add, before
    /// commit.
    fn track_chain(&self, track: i32, tags: &[u8]) -> Result<(), EngineError> {
        let _ = (track, tags);
        unexpected("ve_track_chain")
    }

    /// Plays staged `track` through earlier staged `leader`'s chain
    /// (`ve_track_lane`): the clips of one lane share one insert chain, fader,
    /// pan and sends, so a compressor runs across clip boundaries. Per-track
    /// setters on `track` then address the leader's chain. Before commit.
    fn track_lane(&self, track: i32, leader: i32) -> Result<(), EngineError> {
        let _ = (track, leader);
        unexpected("ve_track_lane")
    }

    /// Names the lane staged `track` plays by a key kept from sync to sync
    /// (`ve_track_key`): the lane's chain keeps its node ids, so its effects
    /// carry their tails and histories across the rebuild. Before commit. It
    /// shapes what the commit carries over, not whether it succeeds, so a fake
    /// that does not model it takes it as a no-op.
    fn track_key(&self, track: i32, key: u64) -> Result<(), EngineError> {
        let _ = (track, key);
        Ok(())
    }

    /// Names staged FX bus `bus` by a key kept from sync to sync
    /// (`ve_bus_key`; see [`EngineBindings::track_key`]). A no-op in a fake
    /// that does not model it.
    fn bus_key(&self, bus: i32, key: u64) -> Result<(), EngineError> {
        let _ = (bus, key);
        Ok(())
    }

    /// A staged bus's insert-chain order (`ve_bus_chain`).
    fn bus_chain(&self, bus: i32, tags: &[u8]) -> Result<(), EngineError> {
        let _ = (bus, tags);
        unexpected("ve_bus_chain")
    }

    /// A staged track's post-fader aux sends as (target bus index, level dB)
    /// pairs (`ve_track_sends`; the engine keeps the first `MAX_SENDS`). An
    /// unresolvable target keeps its slot. Before commit; a level changes live
    /// with [`EngineBindings::set_send_level`].
    fn track_sends(&self, track: i32, sends: &[(i32, f32)]) -> Result<(), EngineError> {
        let _ = (track, sends);
        unexpected("ve_track_sends")
    }

    /// A staged bus's aux sends (`ve_bus_sends`).
    fn bus_sends(&self, bus: i32, sends: &[(i32, f32)]) -> Result<(), EngineError> {
        let _ = (bus, sends);
        unexpected("ve_bus_sends")
    }

    /// Live level of a committed track's send `slot` (`ve_set_send_level`).
    fn set_send_level(&self, track: i32, slot: i32, level_db: f32) -> Result<(), EngineError> {
        let _ = (track, slot, level_db);
        unexpected("ve_set_send_level")
    }

    /// Live level of a committed bus's send `slot` (`ve_set_bus_send_level`).
    fn set_bus_send_level(&self, bus: i32, slot: i32, level_db: f32) -> Result<(), EngineError> {
        let _ = (bus, slot, level_db);
        unexpected("ve_set_bus_send_level")
    }

    /// The master bus gain, linear, 1.0 = unity (`ve_set_master`), feeding the
    /// master soft-clip. A non-finite gain is dropped.
    fn set_master(&self, gain: f32) -> Result<(), EngineError> {
        let _ = gain;
        unexpected("ve_set_master")
    }

    // --- Track effects (committed index; a node a backing track lacks is a no-op) ---

    /// One EQ band: `freq` Hz, `q`, `gain_db`, on/off (`ve_set_eq_band`).
    fn set_eq_band(
        &self,
        track: i32,
        band: i32,
        freq: f32,
        q: f32,
        gain_db: f32,
        enabled: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, band, freq, q, gain_db, enabled);
        unexpected("ve_set_eq_band")
    }

    /// The compressor (`ve_set_comp`).
    fn set_comp(
        &self,
        track: i32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (
            track,
            threshold_db,
            ratio,
            knee_db,
            attack_ms,
            release_ms,
            makeup_db,
            bypass,
        );
        unexpected("ve_set_comp")
    }

    /// The reverb send in dB, -120 = off (`ve_set_send`).
    fn set_send(&self, track: i32, send_db: f32) -> Result<(), EngineError> {
        let _ = (track, send_db);
        unexpected("ve_set_send")
    }

    /// The shared reverb bus: room, damping, wet, dry, width, each 0..1
    /// (`ve_set_reverb`).
    fn set_reverb(
        &self,
        room: f32,
        damping: f32,
        wet: f32,
        dry: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        let _ = (room, damping, wet, dry, width);
        unexpected("ve_set_reverb")
    }

    /// A track compressor's current gain reduction in dB, >= 0, for a meter
    /// (`ve_track_gr`); 0 for backing.
    fn track_gr(&self, track: i32) -> f32 {
        let _ = track;
        unexpected("ve_track_gr")
    }

    /// The de-esser (`ve_set_deesser`).
    fn set_deesser(
        &self,
        track: i32,
        freq: f32,
        threshold_db: f32,
        range_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, freq, threshold_db, range_db, bypass);
        unexpected("ve_set_deesser")
    }

    /// The gate (`ve_set_gate`).
    fn set_gate(
        &self,
        track: i32,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, threshold_db, range_db, attack_ms, release_ms, bypass);
        unexpected("ve_set_gate")
    }

    /// The delay (`ve_set_delay`).
    fn set_delay(
        &self,
        track: i32,
        time_ms: f32,
        feedback: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, time_ms, feedback, mix, bypass);
        unexpected("ve_set_delay")
    }

    /// The saturation (`ve_set_saturation`).
    fn set_saturation(
        &self,
        track: i32,
        drive_db: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, drive_db, mix, bypass);
        unexpected("ve_set_saturation")
    }

    /// The doubler (`ve_set_doubler`).
    fn set_doubler(
        &self,
        track: i32,
        delay_ms: f32,
        depth_ms: f32,
        rate_hz: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, delay_ms, depth_ms, rate_hz, mix, bypass);
        unexpected("ve_set_doubler")
    }

    /// The limiter (`ve_set_limiter`).
    fn set_limiter(
        &self,
        track: i32,
        ceiling_db: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (track, ceiling_db, release_ms, bypass);
        unexpected("ve_set_limiter")
    }

    /// The reverb as a series insert, wet 0 = off, dry at unity
    /// (`ve_set_reverb_insert`).
    fn set_reverb_insert(
        &self,
        track: i32,
        room: f32,
        damping: f32,
        wet: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        let _ = (track, room, damping, wet, width);
        unexpected("ve_set_reverb_insert")
    }

    // --- Bus effects (0-based bus index; mirror the track setters) -------------

    /// A bus EQ band (`ve_set_bus_eq_band`).
    fn set_bus_eq_band(
        &self,
        bus: i32,
        band: i32,
        freq: f32,
        q: f32,
        gain_db: f32,
        enabled: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, band, freq, q, gain_db, enabled);
        unexpected("ve_set_bus_eq_band")
    }

    /// A bus compressor (`ve_set_bus_comp`).
    fn set_bus_comp(
        &self,
        bus: i32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (
            bus,
            threshold_db,
            ratio,
            knee_db,
            attack_ms,
            release_ms,
            makeup_db,
            bypass,
        );
        unexpected("ve_set_bus_comp")
    }

    /// A bus's reverb send in dB (`ve_set_bus_send`).
    fn set_bus_send(&self, bus: i32, send_db: f32) -> Result<(), EngineError> {
        let _ = (bus, send_db);
        unexpected("ve_set_bus_send")
    }

    /// Live volume and mute for a committed bus, its fader (`ve_set_bus_volume`).
    fn set_bus_volume(&self, bus: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        let _ = (bus, gain, muted);
        unexpected("ve_set_bus_volume")
    }

    /// Live stereo pan for a committed bus (`ve_set_bus_pan`).
    fn set_bus_pan(&self, bus: i32, pan: f32) -> Result<(), EngineError> {
        let _ = (bus, pan);
        unexpected("ve_set_bus_pan")
    }

    /// A bus compressor's current gain reduction in dB (`ve_bus_gr`).
    fn bus_gr(&self, bus: i32) -> f32 {
        let _ = bus;
        unexpected("ve_bus_gr")
    }

    /// A bus de-esser (`ve_set_bus_deesser`).
    fn set_bus_deesser(
        &self,
        bus: i32,
        freq: f32,
        threshold_db: f32,
        range_db: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, freq, threshold_db, range_db, bypass);
        unexpected("ve_set_bus_deesser")
    }

    /// A bus gate (`ve_set_bus_gate`).
    fn set_bus_gate(
        &self,
        bus: i32,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, threshold_db, range_db, attack_ms, release_ms, bypass);
        unexpected("ve_set_bus_gate")
    }

    /// A bus delay (`ve_set_bus_delay`).
    fn set_bus_delay(
        &self,
        bus: i32,
        time_ms: f32,
        feedback: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, time_ms, feedback, mix, bypass);
        unexpected("ve_set_bus_delay")
    }

    /// A bus saturation (`ve_set_bus_saturation`).
    fn set_bus_saturation(
        &self,
        bus: i32,
        drive_db: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, drive_db, mix, bypass);
        unexpected("ve_set_bus_saturation")
    }

    /// A bus doubler (`ve_set_bus_doubler`).
    fn set_bus_doubler(
        &self,
        bus: i32,
        delay_ms: f32,
        depth_ms: f32,
        rate_hz: f32,
        mix: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, delay_ms, depth_ms, rate_hz, mix, bypass);
        unexpected("ve_set_bus_doubler")
    }

    /// A bus limiter (`ve_set_bus_limiter`).
    fn set_bus_limiter(
        &self,
        bus: i32,
        ceiling_db: f32,
        release_ms: f32,
        bypass: bool,
    ) -> Result<(), EngineError> {
        let _ = (bus, ceiling_db, release_ms, bypass);
        unexpected("ve_set_bus_limiter")
    }

    /// A bus reverb insert (`ve_set_bus_reverb_insert`).
    fn set_bus_reverb_insert(
        &self,
        bus: i32,
        room: f32,
        damping: f32,
        wet: f32,
        width: f32,
    ) -> Result<(), EngineError> {
        let _ = (bus, room, damping, wet, width);
        unexpected("ve_set_bus_reverb_insert")
    }
}

/// What a fake's unimplemented call does: fail loudly, naming the call.
#[cold]
#[track_caller]
fn unexpected(call: &str) -> ! {
    panic!("unexpected engine call {call}: this EngineBindings does not implement it")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// A fake the way the Dart tests faked the bindings: only what the
    /// scenario expects.
    #[derive(Default)]
    struct Fake {
        plays: AtomicUsize,
    }

    impl EngineBindings for Fake {
        fn sample_rate(&self) -> f64 {
            48_000.0
        }

        fn play(&self) -> Result<(), EngineError> {
            self.plays.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn a_fake_answers_what_it_overrides_through_the_trait_object() {
        let fake = Arc::new(Fake::default());
        let engine: Arc<dyn EngineBindings> = fake.clone();
        assert_eq!(engine.sample_rate(), 48_000.0);
        engine.play().unwrap();
        engine.play().unwrap();
        assert_eq!(fake.plays.load(Ordering::Relaxed), 2);
    }

    #[test]
    #[should_panic(expected = "unexpected engine call ve_tracks_commit_staged")]
    fn an_unexpected_call_fails_loudly() {
        let engine: Arc<dyn EngineBindings> = Arc::new(Fake::default());
        let _ = engine.tracks_commit_staged();
    }

    #[test]
    #[should_panic(expected = "unexpected engine call ve_set_comp")]
    fn an_unexpected_call_with_arguments_fails_loudly_too() {
        let _ = Fake::default().set_comp(0, -18.0, 3.0, 6.0, 10.0, 100.0, 0.0, false);
    }
}
