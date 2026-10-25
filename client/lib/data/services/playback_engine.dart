import 'dart:async';

import '../../domain/models/audio_fx.dart';

/// Playback spec for one track, derived from the domain model. [startMs] is the track's
/// offset on the shared timeline; [durationMs] its audio length.
class TrackPlayback {
  const TrackPlayback({
    required this.id,
    required this.path,
    this.startMs = 0,
    this.durationMs = 0,
    this.volume = 1.0,
    this.muted = false,
    this.pan = 0.0,
    this.isVocal = false,
    this.fx,
    this.busId,
    this.sends = const [],
    this.laneId,
  });

  final String id;
  final String path;
  final int startMs;
  final int durationMs;
  final double volume;
  final bool muted;

  /// Stereo pan in [-1, 1] (0 = center).
  final double pan;

  /// Vocal takes get the effect chain (EQ/comp/reverb send); backing tracks stay dry.
  final bool isVocal;

  /// The vocal take's effect settings (re-applied to the engine after a graph rebuild).
  final TrackFx? fx;

  /// Output routing: the FX bus id this track feeds, or null = master.
  final String? busId;

  /// Post-fader aux sends to FX buses.
  final List<TrackSend> sends;

  /// The lane (take) this clip belongs to: every clip of one lane plays through ONE engine
  /// chain, so a compressor or gate runs across its clip boundaries instead of restarting
  /// cold on each clip. Null: the clip gets a chain of its own.
  final String? laneId;
}

/// Playback spec for one FX bus (group/aux), derived from the domain model.
class BusPlayback {
  const BusPlayback({
    required this.id,
    required this.fx,
    this.volume = 1.0,
    this.muted = false,
    this.pan = 0.0,
    this.outputBusId,
    this.sends = const [],
  });

  final String id;

  /// The bus's insert chain (EQ + comp) + reverb send level.
  final TrackFx fx;
  final double volume;
  final bool muted;

  /// Stereo pan in [-1, 1] (0 = center).
  final double pan;

  /// Output routing: another bus's id, or null = master.
  final String? outputBusId;

  /// Post-fader aux sends to other FX buses.
  final List<TrackSend> sends;
}

/// The transport surface the [SongPracticeDetailViewModel] drives, independent of the
/// backing implementation. The native Rust graph engine ([GraphAudioPlaybackService])
/// satisfies it; the ViewModel holds a [PlaybackEngine] built by [createPlaybackEngine].
abstract interface class PlaybackEngine {
  /// Timeline position, for the playhead.
  Stream<Duration> get position;

  /// Whether the transport is running.
  Stream<bool> get playing;

  Duration get currentPosition;
  bool get isPlaying;

  /// Set the play range; while enabled, playback wraps at [endMs] back to [startMs].
  void setLoop(bool enabled, int startMs, int endMs);

  /// Reconcile the engine with the current track set + FX buses (paths / offsets /
  /// volumes / mutes / routing). A structural change (files or routing) rebuilds the
  /// graph; volume/mute-only changes are pushed live.
  Future<void> sync(List<TrackPlayback> tracks, List<BusPlayback> buses);

  Future<void> play();
  Future<void> pause();
  Future<void> seek(Duration position);

  /// Set a track's level; [muted] forces silence regardless of [volume].
  Future<void> setVolume(String id, double volume, {bool muted = false});

  /// Set a track's stereo pan in [-1, 1] (0 = center); no-op without a pan node.
  void setTrackPan(String id, double pan);

  /// Set the master bus output gain (linear, 1.0 = unity) — the overall mix level
  /// (no-op on engines without a master bus).
  void setMasterVolume(double gain);

  /// Whether this engine can apply EQ / compressor / reverb (only the native engine can).
  bool get supportsEffects;

  /// Apply a vocal track's effect chain immediately (no-op on engines without effects).
  void setTrackFx(String trackId, TrackFx fx);

  /// Apply an FX bus's effect chain immediately (no-op on engines without effects).
  void setBusFx(String busId, TrackFx fx);

  /// Set an FX bus's fader level; [muted] forces silence (no-op without buses).
  void setBusVolume(String busId, double volume, {bool muted = false});

  /// Set an FX bus's stereo pan in [-1, 1] (no-op without buses).
  void setBusPan(String busId, double pan);

  /// The bus compressor's current gain reduction (dB, >= 0) — poll for a meter.
  double busGainReductionDb(String busId);

  /// Set a track's aux-send level (dB) for send [slot] (no-op without sends).
  void setTrackSendLevel(String trackId, int slot, double levelDb);

  /// Set a bus's aux-send level (dB) for send [slot].
  void setBusSendLevel(String busId, int slot, double levelDb);

  /// Apply the shared reverb bus settings (no-op on engines without effects).
  void setReverb(ReverbSettings reverb);

  /// The vocal compressor's current gain reduction (dB, >= 0) for a track — poll for a meter.
  /// 0 on engines without effects.
  double trackGainReductionDb(String trackId);

  Future<void> dispose();
}
