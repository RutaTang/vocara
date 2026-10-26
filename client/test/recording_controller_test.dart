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

  /// The transport Stop: logged as one call (the real one pauses and seeks).
  @override
  Future<void> stop() async => _engine.calls.add('stop');

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

  // The 1 s device watch reads this on every tick.
  @override
  bool get liveMonitor => false;
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
  void syncInputChannel() {}

  @override
  bool get bluetoothInOutConflict => false;

  @override
  bool bluetoothInOutConflictNow() => false;

  // The 1 s device watch calls this on every tick, like streamXruns below.
  @override
  void reconcileMonitorSafety() {}
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

/// A mic the test feeds by hand: [frames] is the engine's input counter, and
/// [polls] counts the 1 s device watch's ticks (each polls the events once),
/// so a test can wait out whole ticks whatever their phase.
class _HandFedGraph extends _FakeGraph {
  int frames = 0;
  int polls = 0;

  @override
  int pollDeviceEvents(int mask) {
    polls++;
    return 0;
  }

  @override
  int inputFramesReceived() => frames;

  @override
  void startMonitor() {}

  @override
  void stopMonitor() {}

  @override
  double inputLevel() => 0;
}

/// Enough engine for the REAL recorder to arm: a converged, running input
/// stream whose take ring grows on every read, so its ready handshake passes
/// at once and its level timer drives the take's clock.
class _RollingGraph extends _FakeGraph {
  int _written = 0;

  @override
  int inputIntentSeq() => 1;

  @override
  int inputAppliedState() => (1 << 24) | 7;

  @override
  int inputStreamState() => (7 << 1) | 1;

  @override
  int recordWrittenFrames() => _written += 480;

  @override
  int inputFramesReceived() => _written;

  @override
  bool startInputRecording(String path) => true;

  @override
  bool stopInputRecording() => true;

  @override
  int recordDroppedFrames() => 0;

  @override
  double inputLevel() => 0.5;
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

  /// What the next [stop] reports for the writer: false = it hit an IO error.
  bool clean = true;

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
    return (path: lastPath!, clean: clean, droppedFrames: 0);
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

/// [_FakeMic] whose start parks in its arm window (after the take setup, where
/// the real recorder waits out the mic's ready handshake) until [ready], and —
/// like the real one — arms nothing once [invalidatePendingStart] landed
/// meanwhile.
class _ArmingMic extends _FakeMic {
  final ready = Completer<void>();
  bool starting = false;
  int _generation = 0;

  @override
  bool get isStarting => starting;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    startCalls++;
    starting = true;
    final generation = _generation;
    final path = await destination.create();
    await ready.future;
    if (_generation != generation) {
      return const MicRecordingStart.invalidated();
    }
    starting = false;
    lastPath = path;
    recording = true;
    return MicRecordingStart.started(path);
  }

  @override
  void invalidatePendingStart() {
    _generation++;
    starting = false;
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

  /// A null [recorder] records through the real [MicRecordingController].
  RecordingController build(
    MicRecordingController? recorder, {
    String? Function()? selectedTrackId,
    Future<String?> Function()? createEmptyVocalTake,
    Future<void> Function(String)? discardEmptyTake,
    GraphAudioPlaybackService? graph,
    void Function(String)? toast,
  }) {
    final engine = graph ?? _FakeGraph();
    return RecordingController(
      graphEngine: () => engine,
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
      selectedTrackId: selectedTrackId ?? () => 't1',
      trackById: (id) =>
          practice.vocalTracks.where((t) => t.id == id).firstOrNull,
      // Applied, as the host does: the finalize checks the model holds the take.
      updateTrack: (id, fn) => practice = practice.copyWith(
        vocalTracks: [
          for (final t in practice.vocalTracks) t.id == id ? fn(t) : t,
        ],
      ),
      appendVocal: ({required path, required analysis, required startMs}) =>
          't1',
      createEmptyVocalTake: createEmptyVocalTake ?? () async => null,
      discardEmptyTake: discardEmptyTake ?? (_) async {},
      selectRecordedClip: (t, c) {},
      save: () => saveImpl(),
      toast: toast ?? (_) {},
      friendlyError: (e) => '$e',
      mic: recorder,
    );
  }

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
    controller = build(mic);
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
    'a take that never lands still unmutes the take it punched into',
    () async {
      // The armed take is silent in the engine for the whole recording, and
      // only the finalize's save resynced it. A disk error that leaves an
      // unreadable partial drops the take before that save, so the lane stayed
      // silent on playback with its M button unlit.
      var saves = 0;
      saveImpl = () async {
        saves++;
        return true;
      };
      mic.clean = false; // the partial is not even on disk here
      final a = controller.startRecording();
      await pumpUntil(() => mixer.gates.length == 1);
      mixer.gates[0].complete();
      await a;

      final stopping = controller.stopRecording();
      await pumpUntil(() => mixer.gates.length == 2);
      mixer.gates[1].complete();
      await stopping;
      expect(saves, 0, reason: 'nothing landed to save');
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

  test('a finalize waiting on the busy gate still commits after the screen '
      'is disposed', () async {
    // Stop, then Back: the route stops waiting once isRecording reads false.
    // The finalize used to give up on its next retry if the gate was held,
    // stranding the take until boot recovery re-adopted it at 0:00.
    final gate = BusyGate(onError: (_) {});
    var saves = 0;
    final c = RecordingController(
      graphEngine: _FakeGraph.new,
      playback: playback,
      mixer: mixer,
      analysis: _FakeAnalysis(),
      busyGate: gate,
      audioFiles: _FakeFiles(),
      waveforms: _FakeWaveforms(),
      appSettings: _FakeSettings(),
      practiceId: 'p1',
      finalizer: RecordingFinalizer.instance,
      practice: () => practice,
      selectedTrackId: () => 't1',
      trackById: (id) =>
          practice.vocalTracks.where((t) => t.id == id).firstOrNull,
      // Applied, as the host does: the finalize checks the model holds the take.
      updateTrack: (id, fn) => practice = practice.copyWith(
        vocalTracks: [
          for (final t in practice.vocalTracks) t.id == id ? fn(t) : t,
        ],
      ),
      appendVocal: ({required path, required analysis, required startMs}) =>
          't1',
      createEmptyVocalTake: () async => null,
      discardEmptyTake: (_) async {},
      selectRecordedClip: (t, c) {},
      save: () async {
        saves++;
        return true;
      },
      toast: (_) {},
      friendlyError: (e) => '$e',
      mic: _FakeMic(),
    );
    final start = c.startRecording();
    await pumpUntil(() => mixer.gates.length == 1);
    mixer.gates.single.complete();
    await start;
    expect(c.isRecording, isTrue);

    final hold = Completer<void>();
    final held = gate.run(() => hold.future);
    final stop = c.stopRecording();
    await Future<void>.delayed(const Duration(milliseconds: 20));
    c.dispose();
    await Future<void>.delayed(const Duration(milliseconds: 120));
    hold.complete();
    await held;
    await stop;
    expect(saves, 1, reason: 'the take never committed');
  });

  test('Stop while REC is still arming cancels the take', () async {
    // The arm window (permission, take setup, the mic's ready handshake) can
    // last a couple of seconds. isRecording is still false there, so Stop only
    // stopped the transport, and the take then armed and rolled anyway.
    final arming = _ArmingMic();
    final c = build(arming);
    addTearDown(c.dispose);
    var notified = 0;
    c.addListener(() => notified++);

    final start = c.startRecording();
    await pumpUntil(() => c.isStartingTake);
    expect(notified, greaterThan(0), reason: 'the transport shows the arm');

    await c.stopRecording();
    // A REC tap while the cancelled start unwinds must not interleave with
    // its take setup.
    unawaited(c.startRecording());
    arming.ready.complete();
    await pumpUntil(() => !c.isStartingTake);

    expect(c.isRecording, isFalse, reason: 'nothing records after Stop');
    expect(arming.startCalls, 1);
    await start;
    expect(engine.calls, isNot(contains('play')));
  });

  test('Stop while REC is arming pauses the transport where it is', () async {
    // A REC pressed over a playing backing (a punch on the fly) left the
    // backing playing on after the cancel. Like stopping an armed take, the
    // cancel pauses without rewinding the playhead.
    final arming = _ArmingMic();
    final c = build(arming);
    addTearDown(c.dispose);
    final start = c.startRecording();
    await pumpUntil(() => c.isStartingTake);
    engine.calls.clear();
    await c.stopRecording();
    expect(engine.calls, ['pause']);
    arming.ready.complete();
    await start;
  });

  test('a start dropping its empty take still reads as starting', () async {
    // The drop is a structural edit holding the busy gate: a REC in that
    // moment is refused, so the transport must show the take as in flight.
    final arming = _ArmingMic();
    final dropping = Completer<void>();
    final c = build(
      arming,
      selectedTrackId: () => null,
      createEmptyVocalTake: () async => 'fresh',
      discardEmptyTake: (_) => dropping.future,
    );
    addTearDown(c.dispose);
    final start = c.startRecording();
    await pumpUntil(() => c.isStartingTake);
    await c.stopRecording();
    arming.ready.complete();
    await Future<void>.delayed(const Duration(milliseconds: 10));
    expect(c.isStartingTake, isTrue, reason: 'the empty take is being dropped');
    dropping.complete();
    await start;
    expect(c.isStartingTake, isFalse);
  });

  test('a REC that never arms drops the empty take it created', () async {
    // With nothing armed, REC creates (and saves) an empty "Take N" to record
    // into; a start cancelled by Stop, or one that fails, left it behind.
    final arming = _ArmingMic();
    final discarded = <String>[];
    final c = build(
      arming,
      selectedTrackId: () => null,
      createEmptyVocalTake: () async => 'fresh',
      discardEmptyTake: (id) async => discarded.add(id),
    );
    addTearDown(c.dispose);
    final start = c.startRecording();
    await pumpUntil(() => c.isStartingTake);
    await c.stopRecording();
    arming.ready.complete();
    await start;
    expect(discarded, ['fresh']);
  });

  test(
    'a REC cancelled as the screen closes still drops its empty take',
    () async {
      // Back in REC's arm window cancels the start, and the page is gone
      // before that start unwinds: the drop skipped a disposed controller, so
      // the empty "Take N" REC had saved stayed in the practice for good.
      final arming = _ArmingMic();
      final discarded = <String>[];
      final c = build(
        arming,
        selectedTrackId: () => null,
        createEmptyVocalTake: () async => 'fresh',
        discardEmptyTake: (id) async => discarded.add(id),
      );
      final start = c.startRecording();
      await pumpUntil(() => c.isStartingTake);
      await c.stopRecording();
      c.dispose();
      arming.ready.complete();
      await start;
      expect(discarded, ['fresh']);
    },
  );

  test('a REC that arms keeps the take it created', () async {
    final arming = _ArmingMic()..ready.complete();
    final discarded = <String>[];
    final c = build(
      arming,
      selectedTrackId: () => null,
      createEmptyVocalTake: () async => 'fresh',
      discardEmptyTake: (id) async => discarded.add(id),
    );
    addTearDown(c.dispose);
    final start = c.startRecording();
    await pumpUntil(() => mixer.gates.isNotEmpty);
    mixer.gates.single.complete();
    await start;
    expect(c.isRecording, isTrue);
    expect(discarded, isEmpty);
    await c.stopRecording();
  });

  test('a REC after a cancelled arm records normally', () async {
    final arming = _ArmingMic();
    final c = build(arming);
    addTearDown(c.dispose);
    final first = c.startRecording();
    await pumpUntil(() => c.isStartingTake);
    await c.stopRecording();
    arming.ready.complete();
    await pumpUntil(() => !c.isStartingTake);
    expect(c.isRecording, isFalse);
    await first;

    final second = c.startRecording();
    await pumpUntil(() => mixer.gates.isNotEmpty);
    mixer.gates.single.complete();
    await second;
    expect(c.isRecording, isTrue);
    await c.stopRecording();
  });

  /// Waits until the device watch has ticked [n] times in all.
  Future<void> untilTicks(_HandFedGraph graph, int n) async {
    for (var i = 0; i < 400 && graph.polls < n; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    expect(graph.polls, greaterThanOrEqualTo(n), reason: 'the watch stalled');
  }

  test('the record clock keeps real time through a stalled tick', () async {
    // The clock counted level ticks, and a periodic timer skips the ticks a
    // stalled isolate overran (a long frame, a GC pause): the playhead, the
    // clock and the forming waveform fell behind the backing for the rest of
    // the take.
    final c = build(null, graph: _RollingGraph());
    addTearDown(c.dispose);
    playback.position.value = const Duration(seconds: 10);
    final start = c.startRecording();
    await pumpUntil(() => mixer.gates.isNotEmpty);
    mixer.gates.single.complete();
    await start;
    expect(c.isRecording, isTrue);
    final wall = Stopwatch()..start();
    final clock0 = c.recordingDurationMs;

    await Future<void>.delayed(const Duration(milliseconds: 300));
    final stall = Stopwatch()..start();
    while (stall.elapsedMilliseconds < 400) {} // the isolate stalls
    await Future<void>.delayed(const Duration(milliseconds: 300));

    final elapsed = wall.elapsedMilliseconds;
    final clock = c.recordingDurationMs - clock0;
    final head = playback.position.value.inMilliseconds;
    await c.stopRecording();
    expect(clock, inInclusiveRange(elapsed - 150, elapsed + 150));
    expect(head - 10000, inInclusiveRange(elapsed - 150, elapsed + 150));
    expect(
      c.recordingPeaks.value,
      isEmpty,
      reason: 'the stop clears the forming waveform',
    );
  });

  test('monitoring outlasts a mic still warming up', () async {
    // A Bluetooth headset switching to its headset profile delivers nothing
    // for a second or two, which the engine allows for before it reports the
    // input lost. The 1 s watchdog counted those silent ticks too and, two
    // ticks after the headphone button, stopped monitoring as "disconnected".
    final graph = _HandFedGraph();
    final toasts = <String>[];
    final c = build(_FakeMic(), graph: graph, toast: toasts.add);
    addTearDown(c.dispose);
    await c.toggleInputMonitor();
    final on = graph.polls;
    await untilTicks(graph, on + 2);
    expect(c.inputMonitorEnabled, isTrue, reason: 'the mic is warming up');
    expect(toasts, isEmpty);

    graph.frames = 4800; // the mic starts delivering
    await untilTicks(graph, on + 3);
    expect(c.inputMonitorEnabled, isTrue);
  });

  test('monitoring still stops when a delivering mic goes silent', () async {
    final graph = _HandFedGraph();
    final toasts = <String>[];
    final c = build(_FakeMic(), graph: graph, toast: toasts.add);
    addTearDown(c.dispose);
    await c.toggleInputMonitor();
    final on = graph.polls;
    graph.frames = 4800; // delivering by the first tick, then nothing
    await untilTicks(graph, on + 3);
    expect(c.inputMonitorEnabled, isFalse);
    expect(toasts, [
      'Input monitoring stopped: the input device was disconnected.',
    ]);
  });
}

/// Spin the event loop until [ready] (bounded).
Future<void> pumpUntil(bool Function() ready) async {
  for (var i = 0; i < 400 && !ready(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 2));
  }
  expect(ready(), isTrue, reason: 'condition never became true');
}
