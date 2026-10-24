import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/playback_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/recording_controller.dart';
import 'package:vocara/utils/busy_gate.dart';

/// Transport log double: records every transport call so the tests can assert
/// which TAKE'S continuation drove it.
class _LogEngine extends Fake implements PlaybackEngine {
  final calls = <String>[];

  @override
  void setLoop(bool enabled, int startMs, int endMs) => calls.add('setLoop');

  @override
  Future<void> seek(Duration position) async =>
      calls.add('seek:${position.inMilliseconds}');

  @override
  Future<void> play() async => calls.add('play');

  @override
  Future<void> pause() async => calls.add('pause');
}

class _FakePlayback extends Fake implements PlaybackController {
  _FakePlayback(this._engine);
  final _LogEngine _engine;

  @override
  PlaybackEngine get engine => _engine;

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  /// The record clock's playhead write (it also follows the viewport on the
  /// real controller; here only the value matters).
  @override
  void setRecordingPosition(Duration to) => position.value = to;

  int loopRearms = 0;

  @override
  void ensureLoopRange() => loopRearms++;
}

/// resyncPlayback parks each call on its own completer, so a take's
/// continuation can be held mid-await while the world moves on — the exact
/// window every ABA scenario needs.
class _FakeMixer extends Fake implements MixerController {
  final gates = <Completer<void>>[];

  @override
  Future<void> resyncPlayback() {
    final c = Completer<void>();
    gates.add(c);
    return c.future;
  }
}

class _FakeAnalysis extends Fake implements AnalysisCache {
  @override
  Future<void> setLiveMonitor(bool on, {bool force = false}) async {}

  @override
  void onTrackAudioChanged(String trackId) {}
}

class _FakeFiles extends Fake implements AudioFileService {
  var n = 0;

  @override
  Future<String> newRecordingPath({
    required String practiceId,
    String extension = 'wav',
  }) async => '/fake/take_${n++}.wav';

  @override
  Future<void> deleteAudio(String path) async {}
}

class _FakeWaveforms extends Fake implements WaveformService {
  @override
  Future<WaveformResult> analyze(
    String path, {
    int targetBuckets = 800,
  }) async => (peaks: const <double>[], durationMs: 100);
}

class _FakeSettings extends Fake implements AppSettingsService {
  @override
  bool get bluetoothInOutConflict => false;
}

/// Minimal native-engine double: only what the recording flow and the 1 s
/// device watch touch. Anything else is a loud failure.
class _FakeGraph extends Fake implements GraphAudioPlaybackService {
  @override
  bool get isAvailable => true;

  @override
  int pollDeviceEvents(int mask) => 0;

  // The 1 s device watch reads this on every tick; a Fake without it throws
  // from inside the timer callback once a test pumps past one second.
  @override
  int streamXruns() => 0;

  @override
  int inputFramesReceived() => 0;

  @override
  int recordWrittenFrames() => 0;

  @override
  double recordLatencyMs() => 0;
}

/// Recorder double faithful to the real one where the controller depends on
/// it: [start] runs the destination's create() (that is where the VM take
/// setup — including the generation bump — lives), and [stop] flips the
/// armed flag synchronously.
class _FakeMic extends Fake implements MicRecordingController {
  bool recording = false;
  int startCalls = 0;
  int stopCalls = 0;
  String? lastPath;

  @override
  final ValueNotifier<double> level = ValueNotifier(0);

  @override
  bool get isRecording => recording;

  @override
  bool get isStarting => false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    startCalls++;
    final path = await destination.create();
    lastPath = path;
    recording = true;
    return MicRecordingStart.started(path);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    stopCalls++;
    recording = false;
    return (path: lastPath!, clean: true, droppedFrames: 0);
  }

  /// Faithful to the real guard where the controller depends on it: the
  /// finalize tail is registered with the quit finalizer while in flight, so
  /// the tests can drive a quit against the stop-finalize window.
  final inFlightTails = <Future<void>>{};

  @override
  Future<void> guardStopFinalize(Future<void> Function() work) async {
    RecordingFinalizer.instance.register(_holdQuit);
    final tail = work();
    inFlightTails.add(tail);
    try {
      await tail;
    } finally {
      inFlightTails.remove(tail);
      if (inFlightTails.isEmpty) {
        RecordingFinalizer.instance.unregister(_holdQuit);
      }
    }
  }

  Future<void> _holdQuit() => Future.wait(inFlightTails.toList());

  @override
  void invalidatePendingStart() {}

  @override
  void dispose() {
    level.dispose();
  }
}

void main() {
  late _LogEngine engine;
  late _FakePlayback playback;
  late _FakeMixer mixer;
  late _FakeMic mic;
  late RecordingController controller;
  late SongPractice practice;
  // Swappable save so a test can park the finalize tail mid-save.
  late Future<bool> Function() saveImpl;

  setUp(() {
    final now = DateTime(2026, 6, 20);
    practice = SongPractice(
      id: 'p1',
      title: 'P',
      createdAt: now,
      updatedAt: now,
      activeVersionId: 'v1',
      versions: [TakeVersion(id: 'v1', name: 'A', createdAt: now)],
      vocalTracks: [
        AudioTrack(
          id: 't1',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 1',
          createdAt: now,
          versionId: 'v1',
        ),
      ],
    );
    saveImpl = () async => true;
    engine = _LogEngine();
    playback = _FakePlayback(engine);
    mixer = _FakeMixer();
    mic = _FakeMic();
    final graph = _FakeGraph();
    controller = RecordingController(
      graphEngine: () => graph,
      playback: playback,
      mixer: mixer,
      analysis: _FakeAnalysis(),
      busyGate: BusyGate(onError: (_) {}),
      audioFiles: _FakeFiles(),
      waveforms: _FakeWaveforms(),
      appSettings: _FakeSettings(),
      practiceId: 'p1',
      finalizer: RecordingFinalizer.instance,
      practice: () => practice,
      selectedTrackId: () => 't1',
      trackById: (id) =>
          practice.vocalTracks.where((t) => t.id == id).firstOrNull,
      updateTrack: (id, fn) {},
      appendVocal: ({required path, required analysis, required startMs}) =>
          't1',
      createEmptyVocalTake: () async {},
      selectRecordedClip: (t, c) {},
      save: () => saveImpl(),
      toast: (_) {},
      friendlyError: (e) => '$e',
      mic: mic,
    );
  });

  tearDown(() {
    controller.dispose();
  });

  test('happy-path ABA: take A\'s continuation parked across A\'s stop and '
      'take B\'s arm never drives B\'s transport', () async {
    // A arms and parks inside resyncPlayback.
    final a = controller.startRecording();
    await pumpUntil(() => mixer.gates.length == 1);
    expect(controller.isRecording, isTrue);

    // A stops (synchronously disarms + runs the gated finalize)…
    await controller.stopRecording();
    expect(controller.isRecording, isFalse);

    // …and B arms, parking on ITS resync.
    final b = controller.startRecording();
    await pumpUntil(() => mixer.gates.length == 2);
    expect(controller.isRecording, isTrue, reason: 'B is armed');

    // A's parked continuation resumes NOW — isRecording is true again (B's!),
    // and only the generation token tells the takes apart.
    mixer.gates[0].complete();
    await a;
    expect(
      engine.calls.where((c) => c == 'play'),
      isEmpty,
      reason: "A's stale continuation must not start B's transport",
    );

    // B's own continuation completes normally.
    mixer.gates[1].complete();
    await b;
    expect(engine.calls.where((c) => c == 'play'), hasLength(1));
    await controller.stopRecording();
  });

  test('exception-path ABA: take A throwing after A\'s stop and B\'s arm '
      'must not stop take B', () async {
    final a = controller.startRecording();
    await pumpUntil(() => mixer.gates.length == 1);
    await controller.stopRecording();
    expect(mic.stopCalls, 1);

    final b = controller.startRecording();
    await pumpUntil(() => mixer.gates.length == 2);
    expect(controller.isRecording, isTrue, reason: 'B is armed');

    // A's parked resync THROWS: the catch sees isRecording == true — B's —
    // and before the generation guard it stopped "the" take, killing B.
    mixer.gates[0].completeError(StateError('resync failed'));
    await a; // the controller catches internally; the future completes
    expect(
      mic.stopCalls,
      1,
      reason: "A's failure cleanup must not stop take B",
    );
    expect(controller.isRecording, isTrue, reason: 'B survives');

    mixer.gates[1].complete();
    await b;
    await controller.stopRecording();
  });

  test(
    'stopping a take re-arms the loop wrap without waiting on a save',
    () async {
      // The take disables the engine's wrap; only the finalize's save used to
      // re-arm it, so a finalize that drops an unreadable partial take (no
      // save) left the loop button lit over a transport that no longer looped.
      // Here the save re-arms nothing, so the stop itself must.
      final a = controller.startRecording();
      await pumpUntil(() => mixer.gates.length == 1);
      mixer.gates[0].complete();
      await a;
      expect(playback.loopRearms, 0);

      await controller.stopRecording();
      expect(playback.loopRearms, 1);
    },
  );

  test(
    'quit during the stop-finalize window waits for the take save',
    () async {
      // The armed-window finalizer is unregistered synchronously inside
      // _mic.stop(), but the take's row only commits at the end of the finalize
      // tail (analyze + save). A quit landing in that window used to find
      // nothing registered, exit mid-save, and strand the take for boot
      // recovery at timeline zero.
      final saveGate = Completer<bool>();
      var saves = 0;
      saveImpl = () {
        saves++;
        return saveGate.future;
      };

      final a = controller.startRecording();
      await pumpUntil(() => mixer.gates.length == 1);
      mixer.gates[0].complete();
      await a;
      expect(controller.isRecording, isTrue);

      // Stop: the finalize tail parks on the in-flight save.
      final stopping = controller.stopRecording();
      await pumpUntil(() => saves == 1);

      // Quit now. finalizeAll must hold the exit until the save committed.
      var quitDone = false;
      final quit = RecordingFinalizer.instance.finalizeAll().whenComplete(
        () => quitDone = true,
      );
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(
        quitDone,
        isFalse,
        reason: 'the quit gate must cover the stop-finalize window',
      );

      saveGate.complete(true);
      await quit;
      await stopping;
      expect(saves, 1, reason: 'the quit awaited the SAME save, no re-run');
      expect(RecordingFinalizer.instance.hasActive, isFalse);
    },
  );
}

/// Spin the event loop until [ready] (bounded).
Future<void> pumpUntil(bool Function() ready) async {
  for (var i = 0; i < 400 && !ready(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 2));
  }
  expect(ready(), isTrue, reason: 'condition never became true');
}
