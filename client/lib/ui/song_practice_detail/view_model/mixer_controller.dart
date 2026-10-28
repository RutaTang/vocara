import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../../data/services/playback_engine.dart';
import '../../../domain/models/audio_fx.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/song_practice.dart';
import '../../../utils/id.dart';

/// Whether a track is heard given its mute/solo and whether any track is
/// soloed. Mute *always* silences a track; solo only isolates which tracks can
/// be heard — so muting a soloed track still silences it (matches user intent
/// and avoids "M does nothing while a solo is active").
@visibleForTesting
bool computeAudible({
  required bool soloed,
  required bool muted,
  required bool anySolo,
}) => (anySolo ? soloed : true) && !muted;

/// The resolved solo state of the whole mix: which tracks and buses are part of
/// the current solo. When [anySolo] is false (the `.all()` case) every source is
/// "in solo" and only its own mute matters. Built by [resolveSolo].
@visibleForTesting
class SoloResolution {
  const SoloResolution(this._tracks, this._buses) : anySolo = true;
  const SoloResolution.all()
    : _tracks = const {},
      _buses = const {},
      anySolo = false;

  final Set<String> _tracks;
  final Set<String> _buses;
  final bool anySolo;

  bool trackInSolo(String id) => !anySolo || _tracks.contains(id);
  bool busInSolo(String id) => !anySolo || _buses.contains(id);
}

/// Resolves the whole mix's solo state in one pass. Solo is a ViewModel concept
/// the engine only ever sees as per-source mutes: with nothing soloed everything
/// (un-muted) plays; once any track *or* bus is soloed, only the soloed sources
/// and the buses that route them to the master stay audible. Crucially, soloing
/// a bus keeps the takes feeding it audible — otherwise the bus would have no
/// input and the solo would be silent.
///
/// Routing follows BOTH the main output (busId → outputBusId) and post-fader aux
/// sends (track/bus `sends`). A reverb / return bus reached only by a send is
/// part of the path, so soloing a take keeps its reverb tail and soloing a
/// send-fed return keeps its feeders audible (a muted source zeroes its sends
/// too, so the return would otherwise go silent). Pure — so it's unit-tested.
@visibleForTesting
SoloResolution resolveSolo(List<AudioTrack> tracks, List<FxBus> buses) {
  final soloedBusIds = {
    for (final b in buses)
      if (b.soloed) b.id,
  };
  final anySolo = tracks.any((t) => t.soloed) || soloedBusIds.isNotEmpty;
  if (!anySolo) return const SoloResolution.all();

  final busById = {for (final b in buses) b.id: b};
  // Every bus a signal reaches from its main output [startBusId] and [sends],
  // following bus outputs and further aux sends onward to the master. A visited
  // set guards routing cycles.
  Set<String> reachableBuses(String? startBusId, List<TrackSend> sends) {
    final result = <String>{};
    final queue = <String>[];
    void visit(String? id) {
      if (id != null && busById.containsKey(id) && result.add(id)) {
        queue.add(id);
      }
    }

    // A send turned all the way down routes nothing (see _routeGainToBus):
    // counted, soloing its bus kept the source's dry path playing too.
    void send(TrackSend s) {
      if (s.levelDb > kSendOffDb) visit(s.busId);
    }

    visit(startBusId);
    sends.forEach(send);
    while (queue.isNotEmpty) {
      final b = busById[queue.removeLast()]!;
      visit(b.outputBusId);
      b.sends.forEach(send);
    }
    return result;
  }

  final audibleTracks = <String>{};
  final audibleBuses = <String>{};
  for (final t in tracks) {
    final reach = reachableBuses(t.busId, t.sends);
    final feedsSoloedBus = reach.any(soloedBusIds.contains);
    // Reuse the pure track rule, treating "feeds a soloed bus" as soloed.
    if (computeAudible(
      soloed: t.soloed || feedsSoloedBus,
      muted: false,
      anySolo: true,
    )) {
      audibleTracks.add(t.id);
      audibleBuses.addAll(reach); // carry this source (dry + sends) onward
    }
  }
  // A soloed bus is heard itself plus everything it routes into, even with no
  // source feeding it yet.
  for (final id in soloedBusIds) {
    final b = busById[id];
    audibleBuses.add(id);
    if (b != null) audibleBuses.addAll(reachableBuses(b.outputBusId, b.sends));
  }
  return SoloResolution(audibleTracks, audibleBuses);
}

/// Everything the engine hears: audibility (mute/solo resolution incl. bus
/// routing), track/bus volume-pan-FX live edits + commits, aux sends, the
/// master bus, FX-bus CRUD/routing with cycle refusal, reverb, gain-reduction
/// meters, composite bus/master display waveforms, backing A/B, and
/// [resyncPlayback] — the full engine graph sync.
///
/// Owns no persistent state beyond the transient backing A/B flag; all mix
/// state lives in the practice model, edited through the host closures (the
/// view model stays the single owner/persister of the model and of selection).
class MixerController extends ChangeNotifier {
  MixerController({
    required PlaybackEngine engine,
    required SongPractice? Function() practice,
    required void Function(SongPractice) setPractice,
    required AudioTrack? Function(String) trackById,
    required List<AudioTrack> Function() allTracks,
    required void Function(String, AudioTrack Function(AudioTrack)) updateTrack,
    required Future<bool> Function() save,
    required void Function(String) toast,
    required void Function(String busId) selectBusAfterMutation,
    required void Function(String busId) clearBusSelectionIf,
    required bool Function() isRecording,
    required String? Function() recordingTargetId,
    required List<double> Function() recordingPeaks,
    required int Function() recordingStartMs,
    required int Function() recordingDurationMs,
    required ValueListenable<Duration> position,
  }) : _playback = engine,
       _practiceOf = practice,
       _setPractice = setPractice,
       _trackById = trackById,
       _allTracksOf = allTracks,
       _updateTrack = updateTrack,
       _save = save,
       _toast = toast,
       _selectBusAfterMutation = selectBusAfterMutation,
       _clearBusSelectionIf = clearBusSelectionIf,
       _isRecording = isRecording,
       _recordingTargetId = recordingTargetId,
       _recordingPeaks = recordingPeaks,
       _recordingStartMs = recordingStartMs,
       _recordingDurationMs = recordingDurationMs,
       _position = position;

  final PlaybackEngine _playback;
  final SongPractice? Function() _practiceOf;
  final void Function(SongPractice) _setPractice;
  final AudioTrack? Function(String) _trackById;
  final List<AudioTrack> Function() _allTracksOf;
  final void Function(String, AudioTrack Function(AudioTrack)) _updateTrack;

  /// Persists the model. The host's save also resyncs the engine with what
  /// it saved (its tail runs [resyncPlayback], failed or not), so a routing
  /// edit here needs no second sync of its own.
  final Future<bool> Function() _save;
  final void Function(String) _toast;

  /// Selects a just-created/duplicated bus in the host (bus and track selection
  /// are mutually exclusive — the host clears the track/clip selection).
  final void Function(String busId) _selectBusAfterMutation;

  /// Clears the host's bus selection when the deleted bus was selected.
  final void Function(String busId) _clearBusSelectionIf;

  // Recording state closures: the take being punched into must be silent
  // (you can't monitor what you're laying down) and the live take folds into
  // the master display waveform while recording.
  final bool Function() _isRecording;
  final String? Function() _recordingTargetId;
  final List<double> Function() _recordingPeaks;
  final int Function() _recordingStartMs;
  final int Function() _recordingDurationMs;

  /// The shared playhead, for the clip-under-playhead gain-reduction meter.
  final ValueListenable<Duration> _position;

  /// Set in [dispose]. Guards async engine pushes racing the tear-down.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  // --- Backing source (accompaniment ⇄ reference) -------------------------

  bool _playReference = false;

  /// When true the reference (original song) is the audible backing instead of
  /// the accompaniment — a seamless A/B; the playhead is unaffected.
  bool get playReference => _playReference;

  /// The backing track currently selected for playback and display.
  AudioTrack? get activeBacking =>
      _playReference ? _practiceOf()?.reference : _practiceOf()?.accompaniment;

  void setPlayReference(bool value) {
    if (_playReference == value) return;
    _playReference = value;
    _applyAudibility();
    notifyListeners();
  }

  /// Sets the backing A/B flag WITHOUT touching the engine or notifying — for
  /// the separation flow, whose save→[resyncPlayback] applies the mix. (Its
  /// failure path restores the previous flag this way too, then resyncs: the
  /// failed save resynced the engine under the flipped flag, and the undo
  /// changed the lanes after it.)
  void setPlayReferenceSilently(bool value) => _playReference = value;

  /// Notes on the lanes the workspace is actually showing: the active backing
  /// (accompaniment or reference, per [playReference]) plus the active
  /// interpretation's takes. The hidden backing's notes stay out, so the
  /// count shown next to the take count can never exceed what the user can
  /// see. Lives here rather than on the model because the A/B choice is
  /// transient view-state owned by this controller —
  /// SongPractice.activeNoteCount deliberately counts both backings (see its
  /// doc).
  int get visibleNoteCount {
    final practice = _practiceOf();
    if (practice == null) return 0;
    var count = activeBacking?.notes.length ?? 0;
    for (final t in practice.activeVocalTracks) {
      count += t.notes.length;
    }
    return count;
  }

  // --- Audibility (mute / solo resolution) ---------------------------------

  /// True if a track should be heard, given the resolved [solo] state. Only the
  /// *active* backing (accompaniment or reference, per [playReference]) is ever
  /// audible; the other backing is always silent. While recording, the take
  /// being punched into is silenced (you can't monitor what you're laying down)
  /// but every other lane still obeys mute/solo. Otherwise mute always silences
  /// and solo isolates.
  bool _audibleTrack(AudioTrack t, SoloResolution solo) {
    if (t.isAccompaniment && _playReference) return false;
    if (t.isReference && !_playReference) return false;
    if (_isRecording() && t.id == _recordingTargetId()) return false;
    if (t.muted) return false;
    return solo.trackInSolo(t.id);
  }

  /// True if a bus passes audio given the resolved [solo] state — muted always
  /// silences; otherwise the bus is heard only when it's part of the solo
  /// (soloed itself, fed by a soloed source, or carrying a soloed bus onward).
  bool _audibleBus(FxBus b, SoloResolution solo) =>
      !b.muted && solo.busInSolo(b.id);

  /// The mix's current solo state — see [resolveSolo]. The INACTIVE backing
  /// is left out: [_audibleTrack] silences it regardless, and it is not shown
  /// anywhere, so a solo left lit on it (solo the original, switch back to the
  /// accompaniment) must not count toward "anything soloed" — that silenced
  /// every visible lane with no lit S in sight.
  SoloResolution _resolveSolo() => resolveSolo([
    for (final t in _allTracksOf())
      if (!(t.isAccompaniment && _playReference) &&
          !(t.isReference && !_playReference))
        t,
  ], fxBuses);

  /// Pushes the current mute/solo state of every track *and* bus to the audio
  /// engine immediately, so M/S respond on click without waiting for the save
  /// round-trip.
  void _applyAudibility() {
    final solo = _resolveSolo();
    for (final t in _allTracksOf()) {
      final muted = !_audibleTrack(t, solo);
      // The engine plays one source per clip — push the lane's level to each.
      for (final c in t.clips) {
        unawaited(_playback.setVolume(c.id, t.volume, muted: muted));
      }
    }
    for (final b in fxBuses) {
      _playback.setBusVolume(b.id, b.volume, muted: !_audibleBus(b, solo));
    }
  }

  Future<void> toggleMute(String trackId) async {
    _updateTrack(trackId, (t) => t.copyWith(muted: !t.muted));
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  Future<void> toggleSolo(String trackId) async {
    _updateTrack(trackId, (t) => t.copyWith(soloed: !t.soloed));
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  // --- Track level / pan ----------------------------------------------------

  /// Applies a volume change live (model + audio) without persisting — call on
  /// every slider tick. Persist once with [commitTrackVolume] on release.
  void setTrackVolumeLive(String trackId, double volume) {
    _updateTrack(trackId, (t) => t.copyWith(volume: volume));
    final track = _trackById(trackId);
    if (track != null) {
      final muted = !_audibleTrack(track, _resolveSolo());
      for (final c in track.clips) {
        unawaited(_playback.setVolume(c.id, track.volume, muted: muted));
      }
    }
    notifyListeners();
  }

  Future<void> commitTrackVolume() => _save();

  /// Applies a pan change live (model + audio) without persisting — call on every
  /// knob tick. Persist once with [commitTrackVolume] on release.
  void setTrackPanLive(String trackId, double pan) {
    final p = pan.clamp(-1.0, 1.0);
    _updateTrack(trackId, (t) => t.copyWith(pan: p));
    final track = _trackById(trackId);
    if (track != null) {
      for (final c in track.clips) {
        _playback.setTrackPan(c.id, p);
      }
    }
    notifyListeners();
  }

  // --- Effects (EQ / compressor / shared reverb) ---------------------------

  /// Whether the active playback engine can apply effects (only the native engine can).
  bool get supportsEffects => _playback.supportsEffects;

  /// The shared reverb settings for this practice (the default until edited).
  ReverbSettings get reverbSettings =>
      _practiceOf()?.reverb ?? const ReverbSettings();

  /// Live shared-reverb edit (model + engine) without persisting.
  void setReverbLive(ReverbSettings reverb) {
    final practice = _practiceOf();
    if (practice == null) return;
    _setPractice(practice.copyWith(reverb: reverb));
    _playback.setReverb(reverb);
    notifyListeners();
  }

  /// Live FX edit for a track (model + engine) without persisting — call on every drag
  /// tick; persist once with [commitFx] on release.
  ///
  /// In the chain order the model holds now: only [reorderTrackInserts]
  /// changes it, and an edit built from an FX panel's earlier build (an
  /// on/off clicked while a reorder's save was landing) wrote the old order
  /// back — the cards snapped back and the reorder was lost.
  void setTrackFxLive(String trackId, TrackFx fx) {
    final live = _trackById(trackId)?.fx;
    if (live != null) fx = fx.copyWith(order: live.order);
    _updateTrack(trackId, (t) => t.copyWith(fx: fx));
    // The lane's FX applies to each of its clip sources in the engine.
    final track = _trackById(trackId);
    if (track != null) {
      for (final c in track.clips) {
        _playback.setTrackFx(c.id, fx);
      }
    }
    notifyListeners();
  }

  Future<void> commitFx() => _save();

  /// Reorders a vocal take's insert chain (the audible plugin order). Persists +
  /// rebuilds the engine graph (the chain order is graph wiring).
  Future<void> reorderTrackInserts(String trackId, List<FxKind> order) async {
    final track = _trackById(trackId);
    if (track == null) return;
    final fx = (track.fx ?? TrackFx.flat()).copyWith(order: order);
    _updateTrack(trackId, (t) => t.copyWith(fx: fx));
    notifyListeners();
    await _save();
  }

  /// Reorders an FX bus's insert chain. Persists + rebuilds the graph.
  Future<void> reorderBusInserts(String busId, List<FxKind> order) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId)
              b.copyWith(fx: b.fx.copyWith(order: order))
            else
              b,
        ],
      ),
    );
    notifyListeners();
    await _save();
  }

  // --- Aux sends (post-fader, to FX buses) --------------------------------

  /// Adds a post-fader send from a take to [busId] (max [kMaxSends], no dupes).
  Future<void> addTrackSend(String trackId, String busId) async {
    final track = _trackById(trackId);
    if (track == null ||
        track.sends.length >= kMaxSends ||
        track.sends.any((s) => s.busId == busId)) {
      return;
    }
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        sends: [
          ...t.sends,
          TrackSend(busId: busId),
        ],
      ),
    );
    notifyListeners();
    await _save();
  }

  Future<void> removeTrackSend(String trackId, int slot) async {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    final next = [...track.sends]..removeAt(slot);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    // Shown now, as every routing edit here is: the save queues, then
    // rebuilds the engine graph before it notifies, and a second click on
    // the same row meanwhile removed the send that had moved into its slot.
    notifyListeners();
    await _save();
  }

  Future<void> setTrackSendTarget(
    String trackId,
    int slot,
    String busId,
  ) async {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    // Two slots pointing at one bus feed it twice, so the bus hears the sum of
    // both faders and neither one reads what you hear. [addTrackSend] refuses
    // it; the menu hides an already-used bus. This is the backstop for the
    // window where the menu's options were computed from a `sends` the save
    // behind a previous retarget has not rebuilt yet.
    if (track.sends[slot].busId == busId) return;
    if (track.sends.any((s) => s.busId == busId)) return;
    final next = [...track.sends];
    next[slot] = next[slot].copyWith(busId: busId);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    notifyListeners();
    await _save();
  }

  /// Live send-level edit (model + engine) without persisting.
  void setTrackSendLevelLive(String trackId, int slot, double levelDb) {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    final next = [...track.sends];
    final wasOff = next[slot].levelDb <= kSendOffDb;
    next[slot] = next[slot].copyWith(levelDb: levelDb);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    for (final c in track.clips) {
      _playback.setTrackSendLevel(c.id, slot, levelDb);
    }
    // A send turned off or back on is a route gone or come back, which a
    // soloed bus's feeders are judged by (see resolveSolo): left to the
    // release, the take stayed muted, or its dry path played, while dragged.
    if (wasOff != levelDb <= kSendOffDb) _applyAudibility();
    notifyListeners();
  }

  List<FxBus> _withBus(String busId, FxBus Function(FxBus) update) => [
    for (final b in fxBuses)
      if (b.id == busId) update(b) else b,
  ];

  Future<void> addBusSend(String busId, String targetBusId) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        busId == targetBusId ||
        bus.sends.length >= kMaxSends ||
        bus.sends.any((s) => s.busId == targetBusId)) {
      return;
    }
    if (_busRouteWouldCycle(busId, targetBusId)) {
      _toast("Can't send there — it would create a feedback loop.");
      return;
    }
    _setPractice(
      practice.copyWith(
        fxBuses: _withBus(
          busId,
          (b) => b.copyWith(
            sends: [
              ...b.sends,
              TrackSend(busId: targetBusId),
            ],
          ),
        ),
      ),
    );
    notifyListeners();
    await _save();
  }

  Future<void> removeBusSend(String busId, int slot) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    _setPractice(
      practice.copyWith(
        fxBuses: _withBus(
          busId,
          (b) => b.copyWith(sends: [...b.sends]..removeAt(slot)),
        ),
      ),
    );
    notifyListeners();
    await _save();
  }

  Future<void> setBusSendTarget(
    String busId,
    int slot,
    String targetBusId,
  ) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        busId == targetBusId ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    // Same duplicate-target backstop as [setTrackSendTarget].
    if (bus.sends[slot].busId == targetBusId) return;
    if (bus.sends.any((s) => s.busId == targetBusId)) return;
    if (_busRouteWouldCycle(busId, targetBusId)) {
      _toast("Can't send there — it would create a feedback loop.");
      return;
    }
    _setPractice(
      practice.copyWith(
        fxBuses: _withBus(busId, (b) {
          final next = [...b.sends];
          next[slot] = next[slot].copyWith(busId: targetBusId);
          return b.copyWith(sends: next);
        }),
      ),
    );
    notifyListeners();
    await _save();
  }

  void setBusSendLevelLive(String busId, int slot, double levelDb) {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    final wasOff = bus.sends[slot].levelDb <= kSendOffDb;
    _setPractice(
      practice.copyWith(
        fxBuses: _withBus(busId, (b) {
          final next = [...b.sends];
          next[slot] = next[slot].copyWith(levelDb: levelDb);
          return b.copyWith(sends: next);
        }),
      ),
    );
    _playback.setBusSendLevel(busId, slot, levelDb);
    if (wasOff != levelDb <= kSendOffDb) _applyAudibility(); // as above
    notifyListeners();
  }

  // --- Master bus ----------------------------------------------------------

  /// The master bus output gain (linear, 1.0 = unity) for the whole mix.
  double get masterVolume => _practiceOf()?.masterVolume ?? 1.0;

  /// Live master-volume edit (model + engine) without persisting — call on every
  /// fader tick; persist once with [commitMasterVolume] on release.
  void setMasterVolumeLive(double gain) {
    final practice = _practiceOf();
    if (practice == null) return;
    _setPractice(practice.copyWith(masterVolume: gain));
    _playback.setMasterVolume(gain);
    notifyListeners();
  }

  Future<void> commitMasterVolume() => _save();

  // --- FX buses (DAW group / aux tracks) ----------------------------------

  List<FxBus> get fxBuses => _practiceOf()?.fxBuses ?? const [];

  /// [nameFor] of the first n from [first] on that no bus is called. The
  /// output and send menus list buses by name alone, so two buses named alike
  /// could not be told apart there: a default 'Bus N' (N the count + 1) after a
  /// delete, or a second copy of one bus (the interpretations' default names
  /// had the same fix).
  static String _unusedBusName(
    List<FxBus> buses,
    String Function(int n) nameFor,
    int first,
  ) {
    final taken = {for (final b in buses) b.name};
    var n = first;
    while (taken.contains(nameFor(n))) {
      n++;
    }
    return nameFor(n);
  }

  /// Creates a new (transparent) FX bus and selects it. Returns its id.
  Future<String> createBus([String name = '']) async {
    final practice = _practiceOf();
    if (practice == null) return '';
    final trimmed = name.trim();
    final bus = FxBus(
      id: newId(),
      name: trimmed.isEmpty
          ? _unusedBusName(
              practice.fxBuses,
              (n) => 'Bus $n',
              practice.fxBuses.length + 1,
            )
          : trimmed,
      fx: TrackFx.flat(),
    );
    _setPractice(practice.copyWith(fxBuses: [...practice.fxBuses, bus]));
    _selectBusAfterMutation(bus.id);
    notifyListeners();
    await _save();
    return bus.id;
  }

  Future<void> renameBus(String busId, String name) async {
    final practice = _practiceOf();
    final trimmed = name.trim();
    if (practice == null || trimmed.isEmpty) return;
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(name: trimmed) else b,
        ],
      ),
    );
    notifyListeners();
    await _save();
  }

  /// Deletes a bus: un-routes any take feeding it and any bus whose output points
  /// at it (both fall back to the master), then removes it. Triggers a rebuild.
  Future<void> deleteBus(String busId) async {
    final practice = _practiceOf();
    if (practice == null) return;
    final tracks = [
      for (final t in practice.vocalTracks)
        t.copyWith(
          clearBus: t.busId == busId,
          sends: t.sends.where((s) => s.busId != busId).toList(),
        ),
    ];
    final buses = [
      for (final b in practice.fxBuses)
        if (b.id != busId)
          b.copyWith(
            clearOutput: b.outputBusId == busId,
            sends: b.sends.where((s) => s.busId != busId).toList(),
          ),
    ];
    _clearBusSelectionIf(busId);
    _setPractice(practice.copyWith(vocalTracks: tracks, fxBuses: buses));
    notifyListeners();
    await _save();
  }

  /// Routes a vocal take's output to [busId] (null = master). Triggers a rebuild.
  Future<void> setTrackBus(String trackId, String? busId) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(busId: busId, clearBus: busId == null),
    );
    notifyListeners();
    await _save();
  }

  /// Whether routing [fromBusId] into [toBusId] (as its output or an aux send)
  /// would close a feedback loop — i.e. [toBusId] can already reach [fromBusId]
  /// through bus outputs and aux sends. The engine REJECTS a cyclic graph and
  /// silently keeps the old one, so the routing UI must refuse the edit instead.
  bool _busRouteWouldCycle(String fromBusId, String? toBusId) {
    final practice = _practiceOf();
    if (toBusId == null || practice == null) return false;
    if (toBusId == fromBusId) return true;
    final byId = {for (final b in practice.fxBuses) b.id: b};
    final seen = <String>{};
    final queue = <String>[toBusId];
    while (queue.isNotEmpty) {
      final id = queue.removeLast();
      if (id == fromBusId) return true;
      if (!seen.add(id)) continue;
      final b = byId[id];
      if (b == null) continue;
      if (b.outputBusId != null) queue.add(b.outputBusId!);
      for (final s in b.sends) {
        queue.add(s.busId);
      }
    }
    return false;
  }

  /// Routes a bus's output to another bus, or to the master ([outputBusId] null).
  /// Self-routing and feedback loops are refused. Rebuilds.
  Future<void> setBusOutput(String busId, String? outputBusId) async {
    final practice = _practiceOf();
    if (practice == null || busId == outputBusId) return;
    if (_busRouteWouldCycle(busId, outputBusId)) {
      _toast("Can't route there — it would create a feedback loop.");
      return;
    }
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId)
              b.copyWith(
                outputBusId: outputBusId,
                clearOutput: outputBusId == null,
              )
            else
              b,
        ],
      ),
    );
    notifyListeners();
    await _save();
  }

  /// Live bus FX edit (model + engine) without persisting — persist with [commitFx].
  /// Kept in the chain order the model holds (see [setTrackFxLive]).
  void setBusFxLive(String busId, TrackFx fx) {
    final practice = _practiceOf();
    if (practice == null) return;
    final live = practice.busById(busId)?.fx;
    if (live != null) fx = fx.copyWith(order: live.order);
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(fx: fx) else b,
        ],
      ),
    );
    _playback.setBusFx(busId, fx);
    notifyListeners();
  }

  /// Live bus fader edit (model + engine) without persisting.
  void setBusVolumeLive(String busId, double volume) {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    final updated = bus.copyWith(volume: volume);
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) updated else b,
        ],
      ),
    );
    _playback.setBusVolume(
      busId,
      volume,
      muted: !_audibleBus(updated, _resolveSolo()),
    );
    notifyListeners();
  }

  /// Live bus pan edit (model + engine) without persisting.
  void setBusPanLive(String busId, double pan) {
    final practice = _practiceOf();
    if (practice == null) return;
    final p = pan.clamp(-1.0, 1.0);
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(pan: p) else b,
        ],
      ),
    );
    _playback.setBusPan(busId, p);
    notifyListeners();
  }

  Future<void> toggleBusMute(String busId) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(muted: !b.muted) else b,
        ],
      ),
    );
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  /// Toggles a bus's solo. Soloing a bus isolates it (and the takes feeding it)
  /// across the whole mix; see [_resolveSolo].
  Future<void> toggleBusSolo(String busId) async {
    final practice = _practiceOf();
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(soloed: !b.soloed) else b,
        ],
      ),
    );
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  /// Sets a bus's lane tint (ARGB). Purely visual; persisting it goes through
  /// the shared save path (which resyncs the graph, harmlessly) like the
  /// track-color edit.
  Future<void> setBusColor(String busId, int value) async {
    final practice = _practiceOf();
    if (practice == null) return;
    _setPractice(
      practice.copyWith(
        fxBuses: [
          for (final b in practice.fxBuses)
            if (b.id == busId) b.copyWith(colorValue: value) else b,
        ],
      ),
    );
    notifyListeners();
    await _save();
  }

  /// Duplicates a bus (its chain, color, level, routing and solo/mute state)
  /// and inserts the copy right after it, then selects it.
  Future<void> duplicateBus(String busId) async {
    final practice = _practiceOf();
    final src = practice?.busById(busId);
    if (practice == null || src == null) return;
    // Every field but the id and the name: a duplicate the user cannot hear is
    // not a duplicate. `soloed` was the one left out, so duplicating a bus
    // WHILE it was soloed produced a copy muted by the solo it did not join —
    // and duplicateTrack (which goes through copyWith) has always carried it.
    final copy = FxBus(
      id: newId(),
      name: _unusedBusName(
        practice.fxBuses,
        (n) => n == 1 ? '${src.name} copy' : '${src.name} copy $n',
        1,
      ),
      fx: src.fx,
      colorValue: src.colorValue,
      volume: src.volume,
      muted: src.muted,
      soloed: src.soloed,
      pan: src.pan,
      outputBusId: src.outputBusId,
      sends: src.sends,
    );
    final list = [...practice.fxBuses];
    final idx = list.indexWhere((b) => b.id == busId);
    list.insert(idx < 0 ? list.length : idx + 1, copy);
    _setPractice(practice.copyWith(fxBuses: list));
    _selectBusAfterMutation(copy.id);
    notifyListeners();
    await _save();
  }

  /// The bus compressor's current gain reduction (dB, >= 0) — for a meter.
  double busGainReductionDb(String busId) =>
      _playback.busGainReductionDb(busId);

  // --- Composite display waveforms -----------------------------------------

  /// The strongest gain with which a source routed to [startBusId] and carrying
  /// [sends] reaches [targetBusId] — 0 when it never does.
  ///
  /// Follows BOTH edge kinds, like [resolveSolo]'s `reachableBuses`: a main
  /// output contributes unity, an aux send contributes its level. Following only
  /// the main output left a bus fed purely by sends (the ordinary reverb return)
  /// drawing a flat line while it was plainly audible, because the real graph
  /// does pass sends to the engine.
  ///
  /// Relaxation, not plain reachability: a bus keeps the best gain seen so far
  /// and is re-visited only when that improves. Every edge gain is <= 1, so the
  /// values decrease monotonically and this terminates even on a routing cycle
  /// (which `addBusSend`/`setBusSendTarget` already reject — this is belt and
  /// braces).
  double _routeGainToBus(
    String targetBusId,
    String? startBusId,
    List<TrackSend> sends,
  ) {
    final busById = {for (final b in fxBuses) b.id: b};
    final best = <String, double>{};
    final queue = <String>[];

    void relax(String? id, double gain) {
      if (id == null || gain <= 0 || !busById.containsKey(id)) return;
      final prev = best[id];
      if (prev != null && prev >= gain) return;
      best[id] = gain;
      queue.add(id);
    }

    double sendGain(TrackSend s) =>
        s.levelDb <= kSendOffDb ? 0.0 : math.pow(10, s.levelDb / 20).toDouble();

    relax(startBusId, 1.0);
    for (final s in sends) {
      relax(s.busId, sendGain(s));
    }
    while (queue.isNotEmpty) {
      final id = queue.removeLast();
      final g = best[id]!;
      final b = busById[id]!;
      relax(b.outputBusId, g);
      for (final s in b.sends) {
        relax(s.busId, g * sendGain(s));
      }
    }
    return best[targetBusId] ?? 0.0;
  }

  /// The strongest gain with which a take routed to [startBusId] and carrying
  /// [sends] reaches the master through buses [solo] lets pass — 0 when every
  /// path is muted. A take routed into a muted bus still drew in the master
  /// lane, though nothing of it was heard. Bus levels are not modelled, as in
  /// the bus lanes; a bus id that names no bus counts as the master, as it
  /// did before.
  double _routeGainToMaster(
    String? startBusId,
    List<TrackSend> sends,
    SoloResolution solo,
  ) {
    final busById = {for (final b in fxBuses) b.id: b};
    var master = startBusId == null || !busById.containsKey(startBusId)
        ? 1.0
        : 0.0;
    final best = <String, double>{};
    final queue = <String>[];

    void relax(String? id, double gain) {
      if (gain <= 0) return;
      if (id == null) {
        if (gain > master) master = gain;
        return;
      }
      final b = busById[id];
      if (b == null || !_audibleBus(b, solo)) return;
      final prev = best[id];
      if (prev != null && prev >= gain) return;
      best[id] = gain;
      queue.add(id);
    }

    double sendGain(TrackSend s) =>
        s.levelDb <= kSendOffDb ? 0.0 : math.pow(10, s.levelDb / 20).toDouble();

    if (startBusId != null) relax(startBusId, 1.0);
    for (final s in sends) {
      relax(s.busId, sendGain(s));
    }
    while (queue.isNotEmpty) {
      final id = queue.removeLast();
      final g = best[id]!;
      final b = busById[id]!;
      relax(b.outputBusId, g);
      for (final s in b.sends) {
        relax(s.busId, g * sendGain(s));
      }
    }
    return master;
  }

  /// The lane envelopes last folded, by bus id (null: the master), with what
  /// they were folded from. The master and bus lanes rebuild on every
  /// notification of the view model — a fader, knob or clip-drag tick, a pan,
  /// a selection — and each rebuild re-folded every clip's peaks: 36k samples
  /// over 12 three-clip lanes, ~0.5 ms in a debug build.
  final Map<String?, ({List<Object?> key, List<double> peaks})> _envelopes = {};

  /// Turns the envelope cache off (every read folds afresh): the oracle for
  /// tests that check the cache returns what a fresh fold would.
  @visibleForTesting
  static bool debugNoEnvelopeCache = false;

  /// [fold]'s envelope for [busId], refolded only when what it reads changed:
  /// the timeline length, the master gain, the A/B backing, each bus's mute,
  /// solo and routing, each track's mute, solo, level, routing and clips
  /// (lists compared by identity, as any edit replaces them), and the take
  /// being recorded (its live envelope for the master only). Not a pan or FX tick, nor a bus's level, pan or FX,
  /// which no envelope shows: keyed on whole tracks and the bus list, each
  /// of those refolded the master and every bus lane at pointer rate.
  List<double> _envelope(String? busId, List<double> Function() fold) {
    if (debugNoEnvelopeCache) return List<double>.unmodifiable(fold());
    final recording = _isRecording();
    final key = <Object?>[
      _practiceOf()?.timelineDurationMs,
      masterVolume,
      _playReference,
      recording,
      if (recording) _recordingTargetId(), // silenced while laid down
      // Only the master lane folds the live take in. Keyed for a bus too,
      // every record tick (20 a second) refolded and repainted each bus lane
      // into the same envelope.
      if (recording && busId == null) ...[
        _recordingPeaks(),
        _recordingStartMs(),
        _recordingDurationMs(),
      ],
      for (final b in fxBuses) ...[
        b.id,
        b.muted,
        b.soloed,
        b.outputBusId,
        b.sends,
      ],
      for (final t in _allTracksOf()) ...[
        t.id,
        t.muted,
        t.soloed,
        t.volume,
        t.busId,
        t.sends,
        t.clips,
      ],
    ];
    final memo = _envelopes[busId];
    if (memo != null && _sameKey(memo.key, key)) return memo.peaks;
    final peaks = List<double>.unmodifiable(fold());
    _envelopes[busId] = (key: key, peaks: peaks);
    return peaks;
  }

  static bool _sameKey(List<Object?> a, List<Object?> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      final x = a[i], y = b[i];
      // Values by value, objects (tracks, lists) by identity.
      if (x is num || x is bool || x is String || x == null) {
        if (x != y) return false;
      } else if (!identical(x, y)) {
        return false;
      }
    }
    return true;
  }

  /// A composite waveform for an FX bus lane: the max-envelope of every audible
  /// take that reaches [busId] — by main output or by aux send, directly or
  /// through another bus — scaled by the take's volume and the gain of the route
  /// it took. Mirrors [masterWaveformPeaks] but filtered to the bus.
  ///
  /// Like the master lane this is the DRY source envelope, not the bus's wet
  /// output: bus effects, volume and pan are not modelled here either.
  List<double> busWaveformPeaks(String busId) =>
      _envelope(busId, () => _busWaveformPeaks(busId));

  List<double> _busWaveformPeaks(String busId) {
    final tl = _practiceOf()?.timelineDurationMs ?? 0;
    if (tl <= 0) return const [];
    const n = 1000;
    final out = List<double>.filled(n, 0.0);
    final solo = _resolveSolo();

    void fold(List<double> peaks, int startMs, int durationMs, double vol) {
      if (peaks.isEmpty || durationMs <= 0 || vol <= 0) return;
      for (var i = 0; i < peaks.length; i++) {
        final tms = startMs + (i + 0.5) / peaks.length * durationMs;
        final b = (tms / tl * n).floor().clamp(0, n - 1);
        final v = peaks[i] * vol;
        if (v > out[b]) out[b] = v;
      }
    }

    for (final t in _allTracksOf()) {
      // Backing lanes send nothing to the engine either (see resyncPlayback).
      if (t.isBacking || !t.hasClip) continue;
      final gain = _routeGainToBus(busId, t.busId, t.sends);
      if (gain <= 0) continue;
      if (!_audibleTrack(t, solo)) continue;
      for (final c in t.clips) {
        fold(c.waveformPeaks, c.startMs, c.durationMs, t.volume * gain);
      }
    }
    return out;
  }

  /// A composite "master" waveform for the master lane: the loudest audible
  /// track envelope (scaled by each track's volume and the master gain) at every
  /// point on the timeline, plus the live take while recording. It approximates
  /// the downmix for display and recomputes reactively as tracks/levels change.
  List<double> get masterWaveformPeaks =>
      _envelope(null, () => _masterWaveformPeaks());

  List<double> _masterWaveformPeaks() {
    final tl = _practiceOf()?.timelineDurationMs ?? 0;
    if (tl <= 0) return const [];
    const n = 1000;
    final out = List<double>.filled(n, 0.0);
    final solo = _resolveSolo();

    void fold(List<double> peaks, int startMs, int durationMs, double vol) {
      if (peaks.isEmpty || durationMs <= 0 || vol <= 0) return;
      for (var i = 0; i < peaks.length; i++) {
        final tms = startMs + (i + 0.5) / peaks.length * durationMs;
        // Off the timeline: the live take's pre-roll, which its stop cuts,
        // or a take sung on past the end (clamped, either piled into the
        // first or the last bin, as a spike of the loudest of it).
        if (tms < 0 || tms >= tl) continue;
        final b = (tms / tl * n).floor().clamp(0, n - 1);
        final v = peaks[i] * vol;
        if (v > out[b]) out[b] = v;
      }
    }

    for (final t in _allTracksOf()) {
      if (!t.hasClip || !_audibleTrack(t, solo)) continue;
      // Backing lanes go straight to the master (see resyncPlayback).
      final gain = t.isBacking
          ? 1.0
          : _routeGainToMaster(t.busId, t.sends, solo);
      if (gain <= 0) continue;
      // Fold every clip of the take (not just the primary), so trailing punched-in
      // clips of a multi-clip take show in the master envelope — matching busWaveformPeaks.
      for (final c in t.clips) {
        fold(c.waveformPeaks, c.startMs, c.durationMs, t.volume * gain);
      }
    }
    // The take being laid down isn't a track yet — fold its live envelope in.
    if (_isRecording()) {
      fold(_recordingPeaks(), _recordingStartMs(), _recordingDurationMs(), 1.0);
    }

    final m = masterVolume;
    if (m != 1.0) {
      for (var b = 0; b < n; b++) {
        out[b] *= m;
      }
    }
    return out;
  }

  /// The vocal compressor's current gain reduction (dB) for a track — for a live meter.
  double gainReductionDb(String trackId) {
    // Engine sources are per-clip, so the FX meter reads the take's clip under
    // the playhead (the one sounding); in the gaps between clips nothing of the
    // take plays, so there is no gain reduction to show.
    final track = _trackById(trackId);
    if (track == null) return 0;
    final posMs = _position.value.inMilliseconds;
    for (final c in track.clips) {
      if (posMs >= c.startMs && posMs < c.endMs) {
        return _playback.trackGainReductionDb(c.id);
      }
    }
    return 0;
  }

  // --- Engine graph sync ----------------------------------------------------

  /// Reconciles the engine with the whole current mix (tracks, buses, reverb,
  /// master). Called by the view model's load/save tail (which is how the
  /// routing edits here reach the engine) and by recording start (mutes the
  /// punched take).
  Future<void> resyncPlayback() async {
    final practice = _practiceOf();
    if (practice == null) return;
    final allTracks = _allTracksOf();

    final solo = _resolveSolo();
    // Flatten lanes to one engine source per clip (id = clip id), all of a
    // lane's clips through the lane's one chain (laneId): its volume / mute /
    // FX apply to every clip, and its dynamics run across their boundaries.
    // Empty takes contribute none.
    final tracks = <TrackPlayback>[];
    for (final t in allTracks) {
      final muted = !_audibleTrack(t, solo);
      for (final c in t.clips) {
        if (c.audioFilePath.isEmpty) continue;
        tracks.add(
          TrackPlayback(
            id: c.id,
            path: c.audioFilePath,
            startMs: c.startMs,
            durationMs: c.durationMs,
            volume: t.volume,
            muted: muted,
            pan: t.pan,
            isVocal: !t.isBacking,
            fx: t.fx,
            busId: t.isBacking ? null : t.busId,
            sends: t.isBacking ? const [] : t.sends,
            laneId: t.id,
          ),
        );
      }
    }
    final buses = [
      for (final b in practice.fxBuses)
        BusPlayback(
          id: b.id,
          fx: b.fx,
          volume: b.volume,
          muted: !_audibleBus(b, solo),
          pan: b.pan,
          outputBusId: b.outputBusId,
          sends: b.sends,
        ),
    ];
    _playback.setReverb(reverbSettings);
    _playback.setMasterVolume(masterVolume);
    await _playback.sync(tracks, buses);
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
