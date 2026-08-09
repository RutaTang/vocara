import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';

/// Engine fake that records what the mixer pushes: per-source levels/mutes,
/// per-bus levels/mutes, the master gain, and the last full graph sync.
class _FakeEngine extends Fake implements PlaybackEngine {
  final Map<String, ({double volume, bool muted})> volumes = {};
  final Map<String, ({double volume, bool muted})> busVolumes = {};
  double? masterGain;
  ReverbSettings? reverb;
  List<TrackPlayback>? lastTracks;
  List<BusPlayback>? lastBuses;

  @override
  bool get supportsEffects => true;

  @override
  Future<void> setVolume(String id, double volume, {bool muted = false}) async {
    volumes[id] = (volume: volume, muted: muted);
  }

  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {
    busVolumes[busId] = (volume: volume, muted: muted);
  }

  @override
  void setMasterVolume(double gain) => masterGain = gain;

  @override
  void setReverb(ReverbSettings r) => reverb = r;

  @override
  Future<void> sync(List<TrackPlayback> tracks, List<BusPlayback> buses) async {
    lastTracks = tracks;
    lastBuses = buses;
  }

  @override
  void setTrackPan(String id, double pan) {}

  @override
  void setTrackFx(String trackId, TrackFx fx) {}

  @override
  void setBusFx(String busId, TrackFx fx) {}

  @override
  void setBusPan(String busId, double pan) {}
}

/// A live host harness mirroring the view model's wiring: the practice model
/// plus the model-edit / selection-hook / recording closures.
class _Host {
  _Host(this.practice);

  SongPractice? practice;
  final engine = _FakeEngine();
  final toasts = <String>[];
  final selectedBuses = <String>[];
  final clearedBuses = <String>[];
  int saves = 0;
  bool recording = false;
  String? recordingTarget;
  List<double> recPeaks = const [];
  int recStartMs = 0;
  int recDurationMs = 0;
  final position = ValueNotifier<Duration>(Duration.zero);

  AudioTrack? trackById(String id) {
    final p = practice;
    if (p == null) return null;
    if (p.accompaniment?.id == id) return p.accompaniment;
    if (p.reference?.id == id) return p.reference;
    for (final t in p.vocalTracks) {
      if (t.id == id) return t;
    }
    return null;
  }

  List<AudioTrack> allTracks() {
    final p = practice;
    if (p == null) return const [];
    return [?p.accompaniment, ?p.reference, ...p.activeVocalTracks];
  }

  void updateTrack(String trackId, AudioTrack Function(AudioTrack) update) {
    final p = practice;
    if (p == null) return;
    if (p.accompaniment?.id == trackId) {
      practice = p.copyWith(accompaniment: update(p.accompaniment!));
    } else if (p.reference?.id == trackId) {
      practice = p.copyWith(reference: update(p.reference!));
    } else {
      practice = p.copyWith(
        vocalTracks: [
          for (final t in p.vocalTracks)
            if (t.id == trackId) update(t) else t,
        ],
      );
    }
  }

  late final MixerController mixer = MixerController(
    engine: engine,
    practice: () => practice,
    setPractice: (p) => practice = p,
    trackById: trackById,
    allTracks: allTracks,
    updateTrack: updateTrack,
    save: () async {
      saves++;
      return true;
    },
    toast: toasts.add,
    selectBusAfterMutation: selectedBuses.add,
    clearBusSelectionIf: clearedBuses.add,
    isRecording: () => recording,
    recordingTargetId: () => recordingTarget,
    recordingPeaks: () => recPeaks,
    recordingStartMs: () => recStartMs,
    recordingDurationMs: () => recDurationMs,
    position: position,
  );
}

AudioTrack _backing(String id, AudioTrackKind kind) => AudioTrack(
  id: id,
  kind: kind,
  displayName: id,
  createdAt: DateTime(2026),
  audioFilePath: '$id.wav',
  durationMs: 10000,
);

AudioTrack _vocal(
  String id, {
  List<double> peaks = const [],
  String? busId,
  List<TrackSend> sends = const [],
  bool muted = false,
}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: id,
  createdAt: DateTime(2026),
  audioFilePath: '$id.wav',
  durationMs: 10000,
  waveformPeaks: peaks,
  busId: busId,
  sends: sends,
  muted: muted,
);

FxBus _bus(
  String id, {
  String? outputBusId,
  List<TrackSend> sends = const [],
}) => FxBus(
  id: id,
  name: id,
  fx: TrackFx.flat(),
  outputBusId: outputBusId,
  sends: sends,
);

SongPractice _practice({
  List<AudioTrack> vocals = const [],
  List<FxBus> buses = const [],
  double masterVolume = 1.0,
}) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  accompaniment: _backing('acc', AudioTrackKind.accompaniment),
  reference: _backing('ref', AudioTrackKind.reference),
  vocalTracks: vocals,
  fxBuses: buses,
  masterVolume: masterVolume,
  timelineDurationMs: 10000,
);

void main() {
  group('backing A/B (playReference)', () {
    test('flips the audible backing and pushes the swap to the engine', () {
      final host = _Host(_practice());
      expect(host.mixer.activeBacking?.id, 'acc');

      host.mixer.setPlayReference(true);

      expect(host.mixer.activeBacking?.id, 'ref');
      expect(host.engine.volumes['acc-c0']?.muted, isTrue);
      expect(host.engine.volumes['ref-c0']?.muted, isFalse);

      host.mixer.setPlayReference(false);
      expect(host.engine.volumes['acc-c0']?.muted, isFalse);
      expect(host.engine.volumes['ref-c0']?.muted, isTrue);
    });

    test('setPlayReferenceSilently changes the flag without engine pushes', () {
      final host = _Host(_practice());
      host.mixer.setPlayReferenceSilently(true);
      expect(host.mixer.playReference, isTrue);
      expect(host.engine.volumes, isEmpty);
    });

    test('visibleNoteCount counts only the shown backing plus the active '
        'interpretation\'s takes', () {
      // The workspace shows ONE backing at a time, so the transport bar's
      // note count must never include the hidden backing's notes (the model's
      // activeNoteCount deliberately counts both; see song_practice_test).
      PracticeNote note(String id) => PracticeNote(
        id: id,
        positionMs: 100,
        content: 'note $id',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      final base = _practice(vocals: [_vocal('v1'), _vocal('v2')]);
      final host = _Host(
        base.copyWith(
          accompaniment: base.accompaniment!.copyWith(notes: [note('a1')]),
          reference: base.reference!.copyWith(notes: [note('r1'), note('r2')]),
          vocalTracks: [
            base.vocalTracks[0].copyWith(notes: [note('t1')]),
            base.vocalTracks[1],
          ],
        ),
      );

      // Accompaniment shown: its 1 note + the take's 1; the reference's 2
      // stay hidden with the lane.
      expect(host.mixer.visibleNoteCount, 2);
      // Original shown: the reference's 2 + the take's 1; the accompaniment's
      // 1 drops out.
      host.mixer.setPlayReference(true);
      expect(host.mixer.visibleNoteCount, 3);
      // The model keeps counting both backings regardless of the toggle.
      expect(host.practice!.activeNoteCount, 4);
    });
  });

  group('resyncPlayback', () {
    test('mutes exactly the take being punched into while recording', () async {
      final host = _Host(_practice(vocals: [_vocal('v1'), _vocal('v2')]));
      host.recording = true;
      host.recordingTarget = 'v1';

      await host.mixer.resyncPlayback();

      final byId = {for (final t in host.engine.lastTracks!) t.id: t};
      expect(byId['v1-c0']!.muted, isTrue);
      expect(byId['v2-c0']!.muted, isFalse);
    });

    test('pushes reverb + master gain alongside the graph', () async {
      final host = _Host(_practice(masterVolume: 0.5));
      await host.mixer.resyncPlayback();
      expect(host.engine.masterGain, 0.5);
      expect(host.engine.reverb, isNotNull);
      expect(host.engine.lastBuses, isEmpty);
    });
  });

  group('bus routing cycle refusal', () {
    test(
      'setBusOutput refuses a route that would close a feedback loop',
      () async {
        // B already outputs into A; routing A into B would close the loop.
        final host = _Host(
          _practice(
            buses: [
              _bus('busA'),
              _bus('busB', outputBusId: 'busA'),
            ],
          ),
        );
        await host.mixer.setBusOutput('busA', 'busB');

        expect(host.toasts.single, contains('feedback loop'));
        expect(host.practice!.busById('busA')!.outputBusId, isNull);
        expect(host.saves, 0);
      },
    );

    test(
      'addBusSend refuses a send that would close a feedback loop',
      () async {
        final host = _Host(
          _practice(
            buses: [
              _bus('busA'),
              _bus('busB', outputBusId: 'busA'),
            ],
          ),
        );
        await host.mixer.addBusSend('busA', 'busB');

        expect(host.toasts.single, contains('feedback loop'));
        expect(host.practice!.busById('busA')!.sends, isEmpty);
        expect(host.saves, 0);
      },
    );

    test('a legal route still applies and rebuilds', () async {
      final host = _Host(_practice(buses: [_bus('busA'), _bus('busB')]));
      await host.mixer.setBusOutput('busA', 'busB');

      expect(host.practice!.busById('busA')!.outputBusId, 'busB');
      expect(host.saves, 1);
      expect(host.engine.lastBuses, isNotNull);
      expect(host.toasts, isEmpty);
    });
  });

  group('deleteBus', () {
    test(
      're-routes every feeder to the master and clears the selection',
      () async {
        final host = _Host(
          _practice(
            vocals: [
              _vocal(
                'v1',
                busId: 'busA',
                sends: const [TrackSend(busId: 'busA')],
              ),
            ],
            buses: [
              _bus('busA'),
              _bus('busB', outputBusId: 'busA'),
            ],
          ),
        );
        await host.mixer.deleteBus('busA');

        final v1 = host.practice!.vocalTracks.single;
        expect(v1.busId, isNull);
        expect(v1.sends, isEmpty);
        expect(host.practice!.fxBuses.map((b) => b.id), ['busB']);
        expect(host.practice!.busById('busB')!.outputBusId, isNull);
        expect(host.clearedBuses, ['busA']);
        expect(host.saves, 1);
        expect(host.engine.lastTracks, isNotNull); // graph rebuilt
      },
    );
  });

  group('createBus', () {
    test(
      'appends a default-named bus and selects it through the host hook',
      () async {
        final host = _Host(_practice(buses: [_bus('busA'), _bus('busB')]));
        final id = await host.mixer.createBus('   ');

        expect(id, isNotEmpty);
        expect(host.practice!.fxBuses, hasLength(3));
        expect(host.practice!.fxBuses.last.name, 'Bus 3');
        expect(host.selectedBuses, [id]);
      },
    );
  });

  group('bus solo', () {
    test('soloing a bus keeps its (transitive) feeders audible and mutes the '
        'rest', () async {
      // v1 feeds busB which outputs into busA; v2 goes straight to master.
      final host = _Host(
        _practice(
          vocals: [
            _vocal('v1', busId: 'busB'),
            _vocal('v2'),
          ],
          buses: [
            _bus('busA'),
            _bus('busB', outputBusId: 'busA'),
          ],
        ),
      );
      await host.mixer.toggleBusSolo('busA');

      expect(
        host.engine.volumes['v1-c0']?.muted,
        isFalse,
        reason: 'the take feeding the soloed bus must stay audible',
      );
      expect(host.engine.volumes['v2-c0']?.muted, isTrue);
      expect(
        host.engine.volumes['acc-c0']?.muted,
        isTrue,
        reason: 'a bus solo isolates: the backing drops out too',
      );
      expect(host.engine.busVolumes['busA']?.muted, isFalse);
      expect(
        host.engine.busVolumes['busB']?.muted,
        isFalse,
        reason: 'the carrying bus is part of the soloed path',
      );
    });
  });

  group('composite display waveforms', () {
    test('masterWaveformPeaks folds audible takes, the live recording, and '
        'the master gain', () async {
      // v1's single peak (0.8) lands mid-clip → bin 500 of 1000.
      final host = _Host(
        _practice(
          vocals: [
            _vocal('v1', peaks: [0.8]),
          ],
          masterVolume: 0.5,
        ),
      );
      var peaks = host.mixer.masterWaveformPeaks;
      expect(peaks[500], closeTo(0.4, 1e-9)); // 0.8 × master 0.5
      expect(peaks[100], 0.0);

      // The in-flight take isn't a track yet — its live envelope folds in.
      host.recording = true;
      host.recPeaks = const [0.5];
      host.recDurationMs = 2000; // single live peak at ~1000 ms → bin 100
      peaks = host.mixer.masterWaveformPeaks;
      expect(peaks[100], closeTo(0.25, 1e-9)); // 0.5 × master 0.5
      expect(peaks[500], closeTo(0.4, 1e-9));

      // Muting the take removes its envelope but keeps the live one.
      host.recording = false;
      await host.mixer.toggleMute('v1');
      expect(host.mixer.masterWaveformPeaks[500], 0.0);
    });

    test('busWaveformPeaks folds only takes routed into the bus, including '
        'through an upstream bus', () {
      final host = _Host(
        _practice(
          vocals: [
            _vocal('v1', peaks: [0.8], busId: 'busB'),
            _vocal('v2', peaks: [0.6]), // straight to master
          ],
          buses: [
            _bus('busA'),
            _bus('busB', outputBusId: 'busA'),
          ],
        ),
      );
      // v1 reaches busA via busB; v2 feeds neither bus.
      expect(host.mixer.busWaveformPeaks('busA')[500], closeTo(0.8, 1e-9));
      expect(host.mixer.busWaveformPeaks('busB')[500], closeTo(0.8, 1e-9));
      expect(host.mixer.busWaveformPeaks('busA').any((v) => v == 0.6), isFalse);
    });

    test('busWaveformPeaks follows aux sends, scaled by the send level', () {
      // The ordinary reverb return: the take stays on the master and only
      // FEEDS the bus. Following the main output alone drew a flat line for a
      // bus that was plainly audible, because resyncPlayback does pass sends
      // to the engine.
      final host = _Host(
        _practice(
          vocals: [
            _vocal(
              'v1',
              peaks: [0.8],
              sends: const [TrackSend(busId: 'rev')],
            ),
          ],
          buses: [_bus('rev')],
        ),
      );
      expect(host.mixer.busWaveformPeaks('rev')[500], closeTo(0.8, 1e-9));
    });

    test(
      'a send level scales the envelope; an off send contributes nothing',
      () {
        SongPractice at(double db) => _practice(
          vocals: [
            _vocal(
              'v1',
              peaks: [0.8],
              sends: [TrackSend(busId: 'rev', levelDb: db)],
            ),
          ],
          buses: [_bus('rev')],
        );
        expect(
          _Host(at(-6.0)).mixer.busWaveformPeaks('rev')[500],
          closeTo(0.8 * 0.501187, 1e-5),
        );
        expect(_Host(at(kSendOffDb)).mixer.busWaveformPeaks('rev')[500], 0.0);
      },
    );

    test('a bus-to-bus send carries the envelope onward too', () {
      final host = _Host(
        _practice(
          vocals: [
            _vocal('v1', peaks: [0.8], busId: 'busB'),
          ],
          buses: [
            _bus('busA'),
            _bus('busB', sends: const [TrackSend(busId: 'busA')]),
          ],
        ),
      );
      // v1 reaches busA only through busB's send, not its main output.
      expect(host.mixer.busWaveformPeaks('busA')[500], closeTo(0.8, 1e-9));
    });

    test('a take reaching a bus two ways takes the LOUDER route', () {
      final host = _Host(
        _practice(
          vocals: [
            _vocal(
              'v1',
              peaks: [0.8],
              busId: 'rev', // unity
              sends: const [TrackSend(busId: 'rev', levelDb: -12.0)],
            ),
          ],
          buses: [_bus('rev')],
        ),
      );
      expect(host.mixer.busWaveformPeaks('rev')[500], closeTo(0.8, 1e-9));
    });
  });
}
