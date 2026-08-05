import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/data/services/recording_finalizer.dart';

/// Fake of the native engine: implements only the members the recording
/// controller touches; anything else is a hard failure (the controller must
/// not reach into playback/graph APIs).
class _FakeEngine implements GraphAudioPlaybackService {
  bool available = true;
  bool startResult = true;
  bool stopClean = true;
  double levelValue = 0.0;
  int deviceEvents = 0;
  Float32List frames = Float32List(0);

  int startCalls = 0;
  int stopCalls = 0;
  int captureStarts = 0;
  int captureStops = 0;
  int inputLevelCalls = 0;
  int pullCalls = 0;
  String? lastRecordingPath;

  @override
  bool get isAvailable => available;

  @override
  bool startInputRecording(String path) {
    startCalls++;
    lastRecordingPath = path;
    return startResult;
  }

  @override
  bool stopInputRecording() {
    stopCalls++;
    return stopClean;
  }

  @override
  void startCapture() => captureStarts++;

  @override
  void stopCapture() => captureStops++;

  @override
  Float32List pullInputFrames() {
    pullCalls++;
    return frames;
  }

  @override
  double inputLevel() {
    inputLevelCalls++;
    return levelValue;
  }

  @override
  int pollDeviceEvents() {
    final events = deviceEvents;
    deviceEvents = 0; // clears on read, like the native poll
    return events;
  }

  /// Per-call advance of the input frame counter. The default (a warm stream)
  /// satisfies the controller's ready handshake on its first poll; 0 simulates
  /// a mic that armed but never delivers (async stream open failed).
  int framesReceivedStep = 512;
  int _framesReceived = 0;

  @override
  int inputFramesReceived() => _framesReceived += framesReceivedStep;

  /// Host-authoritative stream state half of the ready handshake. Defaults to
  /// running (a warm stream); false simulates the arm racing a stream that is
  /// down or being torn down.
  bool streamRunning = true;

  @override
  bool inputStreamRunning() => streamRunning;

  /// Frames the fake reports as dropped by the recording ring at stop.
  int droppedFrames = 0;

  @override
  int recordDroppedFrames() => droppedFrames;

  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnsupportedError('FakeEngine: ${invocation.memberName}');
}

/// A path strategy with no real IO, recording create/discard calls, whose
/// create() can be gated on a completer to park a start mid-await.
class _FakeDestination {
  _FakeDestination({this.path = '/fake/take.wav'});

  final String path;
  final created = <String>[];
  final discarded = <String>[];
  Completer<void>? gate;

  late final RecordingPathStrategy strategy = RecordingPathStrategy(
    create: () async {
      await (gate?.future ?? Future<void>.value());
      created.add(path);
      return path;
    },
    discard: (p) async => discarded.add(p),
  );
}

void main() {
  testWidgets(
    'start failure (WAV create): failed result, nothing armed, file discarded',
    (tester) async {
      final engine = _FakeEngine()..startResult = false;
      final dest = _FakeDestination();
      final controller = MicRecordingController(
        acquireEngine: () => engine,
        requestPermission: () async => true,
      );

      final result = await controller.start(dest.strategy);
      await tester.pump(); // flush the unawaited discard

      expect(result.started, isFalse);
      expect(result.failure, MicRecordingFailure.fileCreateFailed);
      expect(controller.isRecording, isFalse);
      expect(controller.isStarting, isFalse);
      expect(dest.discarded, [dest.path]); // the created destination is gone
      expect(RecordingFinalizer.instance.hasActive, isFalse);
      // No timers were armed: time passing never polls the engine level.
      await tester.pump(const Duration(milliseconds: 500));
      expect(engine.inputLevelCalls, 0);
      expect(controller.level.value, 0);
      controller.dispose();
    },
  );

  testWidgets('permission denied: failed result before anything is created', (
    tester,
  ) async {
    var engineAcquired = 0;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () {
        engineAcquired++;
        return _FakeEngine();
      },
      requestPermission: () async => false,
    );

    final result = await controller.start(dest.strategy);

    expect(result.failure, MicRecordingFailure.permissionDenied);
    expect(engineAcquired, 0); // denied → the engine is never touched
    expect(dest.created, isEmpty);
    controller.dispose();
  });

  testWidgets('start → stop dirty: path + clean=false, pumps and monitor '
      'torn down', (tester) async {
    final engine = _FakeEngine()
      ..levelValue = 0.7
      ..stopClean = false;
    final monitor = MonitorService();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      monitor: monitor,
      requestPermission: () async => true,
    );

    final startFuture = controller.start(dest.strategy);
    // The pending flag is set synchronously, before the first await resolves,
    // so a double-tap in the same frame can be guarded.
    expect(controller.isStarting, isTrue);
    final result = await startFuture;

    expect(result.started, isTrue);
    expect(result.path, dest.path);
    expect(engine.lastRecordingPath, dest.path);
    expect(controller.isRecording, isTrue);
    expect(controller.isStarting, isFalse);
    expect(controller.feedingMonitor, isTrue);
    expect(engine.captureStarts, 1);
    expect(monitor.active, isTrue);
    expect(RecordingFinalizer.instance.hasActive, isTrue);

    // Level pump: 50 ms cadence updates the clamped level; capture pump
    // (30 ms) pulls input frames for the monitor.
    await tester.pump(const Duration(milliseconds: 120));
    expect(controller.level.value, closeTo(0.7, 1e-9));
    expect(engine.inputLevelCalls, 2);
    expect(engine.pullCalls, greaterThanOrEqualTo(3));

    final stopped = controller.stop();
    expect(stopped, isNotNull);
    expect(stopped!.path, dest.path);
    expect(stopped.clean, isFalse); // honors the engine's stop bool
    expect(controller.isRecording, isFalse);
    expect(controller.level.value, 0);
    expect(controller.feedingMonitor, isFalse);
    expect(engine.stopCalls, 1);
    expect(engine.captureStops, 1);
    expect(monitor.active, isFalse);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    // A stopped take's file belongs to the owner — never discarded here.
    expect(dest.discarded, isEmpty);
    // Pumps are gone: no further engine polls.
    final levelCalls = engine.inputLevelCalls;
    await tester.pump(const Duration(milliseconds: 300));
    expect(engine.inputLevelCalls, levelCalls);

    // Stopping again is a null no-op.
    expect(controller.stop(), isNull);
    controller.dispose();
  });

  testWidgets('invalidated mid-permission: nothing armed, engine never '
      'acquired', (tester) async {
    var engineAcquired = 0;
    final permission = Completer<bool>();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () {
        engineAcquired++;
        return _FakeEngine();
      },
      requestPermission: () => permission.future,
    );

    final startFuture = controller.start(dest.strategy);
    expect(controller.isStarting, isTrue);

    // The owner cancels the session while the permission dialog pends; the
    // pending flag clears immediately so UI guards unblock.
    controller.invalidatePendingStart();
    expect(controller.isStarting, isFalse);

    permission.complete(true);
    final result = await startFuture;

    expect(result.started, isFalse);
    expect(result.failure, isNull); // silent no-op, not an error
    expect(engineAcquired, 0);
    expect(dest.created, isEmpty);
    expect(controller.isRecording, isFalse);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    controller.dispose();
  });

  testWidgets('invalidated mid path-create: the just-created file is '
      'discarded', (tester) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination()..gate = Completer<void>();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    final startFuture = controller.start(dest.strategy);
    await tester.pump(); // past permission, parked on create()

    controller.invalidatePendingStart();
    dest.gate!.complete();
    final result = await startFuture;
    await tester.pump(); // flush the unawaited discard

    expect(result.started, isFalse);
    expect(result.failure, isNull);
    expect(dest.created, [dest.path]);
    expect(dest.discarded, [dest.path]); // temp cleaned
    expect(engine.startCalls, 0); // hardware never armed
    expect(controller.isRecording, isFalse);
    controller.dispose();
  });

  testWidgets('dispose during start: nothing armed, file discarded, no '
      'finalizer', (tester) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination()..gate = Completer<void>();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    final startFuture = controller.start(dest.strategy);
    await tester.pump(); // past permission, parked on create()

    controller.dispose();
    dest.gate!.complete();
    final result = await startFuture;
    await tester.pump(); // flush the unawaited discard

    expect(result.started, isFalse);
    expect(result.failure, isNull);
    expect(dest.discarded, [dest.path]);
    expect(engine.startCalls, 0);
    expect(controller.isRecording, isFalse);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    await tester.pump(const Duration(milliseconds: 500));
    expect(engine.inputLevelCalls, 0); // no timers ever armed
  });

  testWidgets('input device lost: level tick reports it once and skips the '
      'level update', (tester) async {
    final engine = _FakeEngine()..levelValue = 0.9;
    final dest = _FakeDestination();
    var lost = 0;
    late final MicRecordingController controller;
    controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      // Mirrors the VMs: stop the take, keeping what was captured.
      onInputDeviceLost: () {
        lost++;
        controller.stop();
      },
    );

    await controller.start(dest.strategy);
    expect(controller.isRecording, isTrue);

    engine.deviceEvents = 2; // input-lost bit
    await tester.pump(const Duration(milliseconds: 50)); // one level tick

    expect(lost, 1);
    expect(controller.isRecording, isFalse);
    // The loss tick returned before reading the level, and stop reset it.
    expect(engine.inputLevelCalls, 0);
    expect(controller.level.value, 0);
    controller.dispose();
  });

  testWidgets('abort stops the hardware and discards the take file', (
    tester,
  ) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    await controller.start(dest.strategy);
    expect(controller.isRecording, isTrue);

    await controller.abort();

    expect(controller.isRecording, isFalse);
    expect(engine.stopCalls, 1);
    expect(dest.discarded, [dest.path]);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    controller.dispose();
  });

  testWidgets('quit finalizer: registered only while armed, finalizeAll runs '
      'the owner callback', (tester) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    var finalized = 0;
    late final MicRecordingController controller;
    controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      onQuitFinalize: () async {
        finalized++;
        controller.stop();
      },
    );

    expect(RecordingFinalizer.instance.hasActive, isFalse);
    await controller.start(dest.strategy);
    expect(RecordingFinalizer.instance.hasActive, isTrue);

    await RecordingFinalizer.instance.finalizeAll();

    expect(finalized, 1);
    expect(controller.isRecording, isFalse);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    controller.dispose();
  });

  testWidgets('dispose while recording closes the WAV handle without '
      'discarding the file', (tester) async {
    final engine = _FakeEngine();
    final monitor = MonitorService();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      monitor: monitor,
      requestPermission: () async => true,
    );

    await controller.start(dest.strategy);
    expect(controller.isRecording, isTrue);

    controller.dispose();

    expect(engine.stopCalls, 1); // native handle closed
    expect(engine.captureStops, 1);
    // File cleanup is the OWNER's policy on dispose (the technique VM
    // deletes its temp dirs itself) — the controller must not discard.
    expect(dest.discarded, isEmpty);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    await tester.pump(const Duration(milliseconds: 500));
    expect(engine.inputLevelCalls, 0); // pumps cancelled
  });

  testWidgets('re-entrant start while pending or recording is a silent no-op', (
    tester,
  ) async {
    final engine = _FakeEngine();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    final first = _FakeDestination(path: '/fake/a.wav');
    final second = _FakeDestination(path: '/fake/b.wav');
    final firstFuture = controller.start(first.strategy);
    final r2 = await controller.start(second.strategy);
    await firstFuture;

    // Only the first start armed; the second was rejected outright.
    expect(controller.isRecording, isTrue);
    expect(engine.startCalls, 1);
    expect(engine.lastRecordingPath, first.path);
    expect(r2.started, isFalse);
    expect(r2.failure, isNull);
    expect(second.created, isEmpty);
    controller.stop();
    controller.dispose();
  });

  testWidgets('mic never delivers frames: start fails with '
      'inputNotDelivering, recording disarmed, file discarded', (tester) async {
    // framesReceivedStep 0 = the counter never advances (async input-stream
    // open failed after startInputRecording claimed success). A zero timeout
    // forces the deterministic timeout path (the ready wait polls a REAL
    // stopwatch, which the fake clock can't advance).
    final engine = _FakeEngine()..framesReceivedStep = 0;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: Duration.zero,
    );

    final result = await controller.start(dest.strategy);
    await tester.pump(); // flush the unawaited discard

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(controller.isRecording, isFalse);
    // The armed recording was rolled back and its file discarded.
    expect(engine.startCalls, 1);
    expect(engine.stopCalls, 1);
    expect(dest.discarded, [dest.path]);
    expect(RecordingFinalizer.instance.hasActive, isFalse);
    controller.dispose();
  });

  testWidgets('a dying stream cannot fake ready: advancing frames without the '
      'host-side running flag never arm the take', (tester) async {
    // The counter advances (an old stream's final callbacks) but the host has
    // already marked the input not-running — the handshake must NOT declare
    // ready on frames alone. Zero timeout forces the deterministic path.
    final engine = _FakeEngine()..streamRunning = false;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: Duration.zero,
    );

    final result = await controller.start(dest.strategy);
    await tester.pump();

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(engine.stopCalls, 1);
    expect(dest.discarded, [dest.path]);
    controller.dispose();
  });

  testWidgets('ring overflow is surfaced: stop reports droppedFrames and '
      'framesAtArm tracks the armed window', (tester) async {
    final engine = _FakeEngine()..droppedFrames = 4800;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    expect(controller.framesAtArm, isNull);
    final result = await controller.start(dest.strategy);
    expect(result.started, isTrue);
    // The arm snapshot is exposed for pre-roll measurement while armed…
    expect(controller.framesAtArm, isNotNull);

    final stopped = controller.stop();
    expect(stopped, isNotNull);
    expect(stopped!.clean, isTrue);
    // …the overflow count rides the stop result, and the snapshot clears.
    expect(stopped.droppedFrames, 4800);
    expect(controller.framesAtArm, isNull);
    controller.dispose();
  });

  // Real IO, so a plain async test (the widget tester's fake clock would
  // never complete file-system futures).
  test('tempDir strategy: fresh <prefix> dir with <prefix>.wav; discard '
      'removes the whole dir', () async {
    final strategy = RecordingPathStrategy.tempDir('vocara_test_take');
    final path = await strategy.create();
    final dir = File(path).parent;
    expect(path, endsWith('/vocara_test_take.wav'));
    expect(
      dir.path.split(Platform.pathSeparator).last,
      startsWith('vocara_test_take'),
    );
    expect(dir.existsSync(), isTrue);

    await strategy.discard(path);
    expect(dir.existsSync(), isFalse);

    // The shared helper tolerates null and already-gone paths.
    await deleteTempRecordingDir(null);
    await deleteTempRecordingDir(path);
  });
}
