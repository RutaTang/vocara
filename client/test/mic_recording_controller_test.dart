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

  /// When set, the arm throws (a native call failing outright).
  bool throwOnStart = false;

  /// When set, starting the live capture throws (after the take armed).
  bool throwOnCapture = false;

  @override
  bool startInputRecording(String path) {
    startCalls++;
    lastRecordingPath = path;
    if (throwOnStart) throw StateError('native arm failed');
    return startResult;
  }

  @override
  bool stopInputRecording() {
    stopCalls++;
    return stopClean;
  }

  @override
  void startCapture() {
    captureStarts++;
    if (throwOnCapture) throw StateError('capture failed');
  }

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
  int pollDeviceEvents(int mask) {
    final events = deviceEvents & mask;
    deviceEvents &= ~mask; // per-bit claim, like the native poll
    return events;
  }

  /// Per-call advance of the input frame counter. The default (a warm stream)
  /// satisfies the controller's ready handshake on its first poll; 0 simulates
  /// a mic that armed but never delivers (async stream open failed).
  int framesReceivedStep = 512;
  int _framesReceived = 0;

  @override
  int inputFramesReceived() => _framesReceived += framesReceivedStep;

  /// Scripted per-take recorded-progress values returned by successive
  /// [recordWrittenFrames] reads; once exhausted, every further read ADVANCES
  /// by [writtenTailStep] (a live stream keeps delivering). The default is a
  /// warm stream; script [0] with tail 0 models a take whose ring never
  /// receives a frame, and a non-empty script with tail 0 models frames that
  /// LANDED once (an outgoing stream's real audio) with the current stream
  /// silent — the delta condition's exact target.
  List<int> recordWrittenScript = [512];
  int writtenTailStep = 480;
  int writtenReads = 0;
  int _writtenValue = 0;

  @override
  int recordWrittenFrames() {
    if (writtenReads < recordWrittenScript.length) {
      _writtenValue = recordWrittenScript[writtenReads];
    } else {
      _writtenValue += writtenTailStep;
    }
    writtenReads++;
    return _writtenValue;
  }

  /// The published input-intent sequence (the arm's watermark).
  int intentSeq = 1;

  @override
  int inputIntentSeq() => intentSeq;

  /// Scripted host-converged pairs returned by successive [inputAppliedState]
  /// reads (last repeats), packed (seq << 24) | generation like the native
  /// word. Default: converged to the arm's intent on generation 1.
  List<int> appliedStateScript = [(1 << 24) | 1];
  int appliedReads = 0;

  @override
  int inputAppliedState() {
    final i = appliedReads < appliedStateScript.length
        ? appliedReads
        : appliedStateScript.length - 1;
    appliedReads++;
    return appliedStateScript[i];
  }

  /// Scripted state words ((generation << 1) | running) for successive
  /// [inputStreamState] reads (last repeats). Default: generation 1, running.
  List<int> streamStateScript = [3];
  int stateReads = 0;

  @override
  int inputStreamState() {
    final i = stateReads < streamStateScript.length
        ? stateReads
        : streamStateScript.length - 1;
    stateReads++;
    return streamStateScript[i];
  }

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
  // A start that threw past its destination left the file behind, and one
  // that threw after arming left the mic recording with nothing to stop it:
  // the owner heard only the throw.
  testWidgets('a start that throws before arming discards its file', (
    tester,
  ) async {
    final engine = _FakeEngine()..throwOnStart = true;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );
    await expectLater(controller.start(dest.strategy), throwsStateError);
    await tester.pump(); // flush the unawaited discard
    expect(controller.isRecording, isFalse);
    expect(controller.isStarting, isFalse);
    expect(dest.discarded, [dest.path]);
    controller.dispose();
  });

  testWidgets('a start that throws after arming stops the mic and discards '
      'its file', (tester) async {
    final engine = _FakeEngine()..throwOnCapture = true;
    final monitor = MonitorService();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      monitor: monitor,
      requestPermission: () async => true,
    );
    await expectLater(controller.start(dest.strategy), throwsStateError);
    await tester.pump();
    expect(controller.isRecording, isFalse);
    expect(controller.feedingMonitor, isFalse);
    expect(engine.stopCalls, 1, reason: 'the armed recording is disarmed');
    expect(engine.captureStops, 1);
    expect(dest.discarded, [dest.path]);
    controller.dispose();
    monitor.dispose();
  });

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

  testWidgets('AEC-bypass and input-fallback events surface as notices while '
      'the take keeps recording (they were silently discarded before)', (
    tester,
  ) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    final notices = <String>[];
    var lost = 0;
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      onInputDeviceLost: () => lost++,
      onDeviceNotice: notices.add,
    );
    addTearDown(controller.dispose);

    await controller.start(dest.strategy);
    engine.deviceEvents = 4; // AEC bypassed
    await tester.pump(const Duration(milliseconds: 50));
    engine.deviceEvents = 8; // selected mic fell back to the default
    await tester.pump(const Duration(milliseconds: 50));

    expect(notices, hasLength(2));
    expect(notices.first, contains('Echo cancellation'));
    expect(notices.last, contains('system default'));
    expect(lost, 0, reason: 'non-fatal degradations must not stop the take');
    expect(controller.isRecording, isTrue);
    controller.stop();
  });

  testWidgets('an input that stops delivering mid-take counts as lost after '
      'the stall limit', (tester) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    var lost = 0;
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      onInputDeviceLost: () => lost++,
    );
    addTearDown(controller.dispose);

    await controller.start(dest.strategy);
    await tester.pump(const Duration(milliseconds: 200)); // delivering
    engine.framesReceivedStep = 0; // the unit stops calling back, no event
    await tester.pump(const Duration(milliseconds: 1500));
    expect(lost, 0, reason: 'not stalled for the limit yet');
    await tester.pump(const Duration(milliseconds: 700));
    expect(lost, 1);
    controller.stop();
  });

  testWidgets('an input that has not delivered yet is not counted lost', (
    tester,
  ) async {
    final engine = _FakeEngine()..framesReceivedStep = 0;
    final dest = _FakeDestination();
    var lost = 0;
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      onInputDeviceLost: () => lost++,
    );
    addTearDown(controller.dispose);

    await controller.start(dest.strategy);
    await tester.pump(const Duration(seconds: 5));
    expect(lost, 0, reason: 'a mic still warming up');
    controller.stop();
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

  // Real time, not testWidgets: the quit's clock is a Stopwatch, which fake
  // time never advances, so the budget would never run out.
  test('a take re-armed while a cut-off quit waited is stopped by the exit '
      'request', () async {
    // The controller registers the same tear-off for every take, marked as
    // work no quit has budgeted: unmarked, the exit request's run found the
    // budget spent and left take 2 recording.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 50),
      totalBudget: const Duration(milliseconds: 60),
    );
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    var stops = 0;
    late final MicRecordingController controller;
    controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      finalizer: finalizer,
      onQuitFinalize: () async {
        if (!controller.isRecording) return;
        stops++;
        controller.stop();
      },
    );
    addTearDown(controller.dispose);
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future)); // a wedged save

    await controller.start(dest.strategy); // take 1
    final close = finalizer.finalizeAll(); // stops take 1, then waits
    await Future<void>.delayed(Duration.zero);
    await controller.start(dest.strategy); // REC again while it waits
    expect(controller.isRecording, isTrue);
    await close.timeout(const Duration(seconds: 2)); // cut off
    await Future<void>.delayed(const Duration(milliseconds: 40)); // spent
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));

    expect(stops, 2, reason: "the exit request's run left take 2 armed");
    expect(controller.isRecording, isFalse);
  });

  testWidgets('guardStopFinalize holds the quit gate over the post-stop '
      'finalize tail and releases it once the tail completes', (tester) async {
    final engine = _FakeEngine();
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );
    addTearDown(controller.dispose);

    await controller.start(dest.strategy);
    expect(controller.stop(), isNotNull);
    // stop() unregistered the armed-window finalizer; the owner's finalize
    // tail (analyze + save) is still ahead, and must re-hold the gate.
    expect(RecordingFinalizer.instance.hasActive, isFalse);

    final gate = Completer<void>();
    final guarded = controller.guardStopFinalize(() => gate.future);
    expect(
      RecordingFinalizer.instance.hasActive,
      isTrue,
      reason: 'the in-flight finalize tail must hold the quit gate',
    );

    var quitDone = false;
    unawaited(
      RecordingFinalizer.instance.finalizeAll().whenComplete(
        () => quitDone = true,
      ),
    );
    await tester.pump(const Duration(milliseconds: 100));
    expect(quitDone, isFalse, reason: 'quit must wait for the in-flight tail');

    gate.complete();
    await tester.pump();
    expect(quitDone, isTrue);
    await guarded;
    expect(RecordingFinalizer.instance.hasActive, isFalse);
  });

  testWidgets('overlapping guarded tails: the earlier tail completing must '
      'not drop the quit hold protecting the later one', (tester) async {
    final controller = MicRecordingController(
      acquireEngine: _FakeEngine.new,
      requestPermission: () async => true,
    );
    addTearDown(controller.dispose);

    final g1 = Completer<void>();
    final g2 = Completer<void>();
    final t1 = controller.guardStopFinalize(() => g1.future);
    final t2 = controller.guardStopFinalize(() => g2.future);
    g1.complete();
    await tester.pump();
    expect(
      RecordingFinalizer.instance.hasActive,
      isTrue,
      reason: 'the second tail is still draining',
    );

    var quitDone = false;
    unawaited(
      RecordingFinalizer.instance.finalizeAll().whenComplete(
        () => quitDone = true,
      ),
    );
    await tester.pump(const Duration(milliseconds: 50));
    expect(quitDone, isFalse);

    g2.complete();
    await tester.pump();
    expect(quitDone, isTrue);
    await t1;
    await t2;
    expect(RecordingFinalizer.instance.hasActive, isFalse);
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
    // The take ring never receives a frame (async input-stream open failed
    // after startInputRecording claimed success). One real millisecond of
    // timeout guarantees the poll body runs at least once — a zero timeout
    // would short-circuit the loop and prove nothing about it.
    final engine = _FakeEngine()
      ..recordWrittenScript = [0]
      ..writtenTailStep = 0;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: const Duration(milliseconds: 1),
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    expect(settled, isTrue, reason: 'the ready wait must hit its deadline');
    final result = await future;
    await tester.pump();
    expect(
      engine.writtenReads,
      greaterThanOrEqualTo(1),
      reason: 'the poll body must actually have run',
    ); // flush the unawaited discard

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

  testWidgets('a dying or foreign stream cannot fake ready: the global frame '
      'counter advancing NEVER arms the take when nothing landed in its '
      'ring', (tester) async {
    // The global counter advances (pre-arm callbacks, an old stream's final
    // push) but recordWrittenFrames stays 0 — nothing entered THIS take's
    // ring, so the recorded-progress ack must refuse across REAL poll
    // iterations, not a short-circuited loop.
    final engine = _FakeEngine()
      ..framesReceivedStep = 512
      ..recordWrittenScript = [0]
      ..writtenTailStep = 0;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: const Duration(milliseconds: 1),
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    expect(settled, isTrue, reason: 'the ready wait must hit its deadline');
    final result = await future;
    await tester.pump();
    expect(
      engine.writtenReads,
      greaterThanOrEqualTo(1),
      reason: 'the poll body must actually have run against the fake',
    );

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(engine.stopCalls, 1);
    expect(dest.discarded, [dest.path]);
    controller.dispose();
  });

  testWidgets('ready lands as soon as frames PROVABLY land in the take ring '
      '(a cold stream delivering on a later poll)', (tester) async {
    // First polls see an empty take ring (the VoiceProcessingIO open is still
    // in flight); frames then land and the very next poll arms the take.
    final engine = _FakeEngine()..recordWrittenScript = [0, 0, 480];
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    final result = await future;

    expect(result.started, isTrue);
    expect(
      engine.writtenReads,
      greaterThanOrEqualTo(3),
      reason: 'ready must have waited out the empty polls',
    );
    controller.stop();
    controller.dispose();
  });

  testWidgets('frames in the ring alone are not ready: the host must also '
      'have converged past the arm (applied watermark)', (tester) async {
    // A warm stream's frames land instantly, but the host has not yet
    // converged to an intent as new as this arm — a queued older teardown
    // could still be ahead of it. Ready must wait for applied >= armSeq
    // (all on the same generation).
    final engine = _FakeEngine()
      ..intentSeq = 5
      ..appliedStateScript = [(3 << 24) | 1, (4 << 24) | 1, (5 << 24) | 1];
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    final result = await future;

    expect(result.started, isTrue);
    expect(
      engine.appliedReads,
      greaterThanOrEqualTo(3),
      reason: 'ready must have waited for the host to converge to the arm',
    );
    controller.stop();
    controller.dispose();
  });

  testWidgets('evidence from two generations never splices into ready: the '
      'applied pair from a dead generation is refused even with frames '
      'written and a newer stream running', (tester) async {
    // The reviewed splice: an old warm stream wrote real frames, its teardown
    // ran, and a FAILED reopen used to advance the watermark anyway. Modeled
    // exactly: applied stays paired with generation 1 (the last SUCCESSFUL
    // convergence), while the current state word shows generation 2 running.
    // The generation-equality condition must refuse; applied never catches up
    // (the reopen keeps failing), so the start times out.
    final engine = _FakeEngine()
      ..intentSeq = 2
      ..appliedStateScript =
          [(2 << 24) | 1] // seq new enough, gen STALE
      ..streamStateScript = [5]; // generation 2, running
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: const Duration(milliseconds: 1),
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    expect(settled, isTrue, reason: 'the ready wait must hit its deadline');
    final result = await future;
    await tester.pump();

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(
      engine.appliedReads,
      greaterThanOrEqualTo(1),
      reason: 'the generation comparison must actually have run',
    );
    controller.dispose();
  });

  testWidgets('the reviewed cross-generation combo is refused: an outgoing '
      'stream\'s frames plus a freshly-converged stream that has not '
      'delivered yet', (tester) async {
    // G1 wrote real frames into the take after the arm, was torn down by a
    // stale off-intent, and G2 then converged SUCCESSFULLY — applied and the
    // state word agree on G2, running. Every absolute condition holds, but
    // the take's counter never moves again (G2 has not delivered): the DELTA
    // condition must refuse until actual growth, which never comes here.
    final engine = _FakeEngine()
      ..recordWrittenScript =
          [480] // G1's frames, already in the WAV
      ..writtenTailStep = 0; // G2 converged but silent
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: const Duration(milliseconds: 1),
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    expect(settled, isTrue, reason: 'the ready wait must hit its deadline');
    final result = await future;
    await tester.pump();

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(
      engine.writtenReads,
      greaterThanOrEqualTo(2),
      reason: 'the baseline and at least one delta comparison must have run',
    );
    controller.dispose();
  });

  testWidgets('the same combo arms once the converged stream actually '
      'delivers (growth over the baseline)', (tester) async {
    // Same shape, but G2 comes alive: the counter grows past the baseline
    // under the unchanged pairing and ready lands.
    final engine = _FakeEngine()
      ..recordWrittenScript = [480, 480, 480]
      ..writtenTailStep = 480;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    final result = await future;

    expect(result.started, isTrue);
    expect(engine.writtenReads, greaterThanOrEqualTo(4));
    controller.stop();
    controller.dispose();
  });

  testWidgets('the growth observation is BRACKETED: a state change between '
      'progress and the confirming re-read refuses ready', (tester) async {
    // Opening reads say running (gen 1) and the counter grows — but by the
    // confirming re-read the state word has changed (a SetInput cleared
    // running; the growth came from the outgoing stream's in-flight
    // callback). The state → progress → state bracket must refuse; the state
    // never recovers here, so the start times out.
    final engine = _FakeEngine()
      ..streamStateScript =
          [3, 2] // running at the open read, gone at recheck
      ..recordWrittenScript = [480]
      ..writtenTailStep = 480; // the counter keeps growing (stale callback)
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
      readyTimeout: const Duration(milliseconds: 1),
    );

    var settled = false;
    final future = controller.start(dest.strategy);
    unawaited(future.whenComplete(() => settled = true));
    for (var i = 0; i < 200 && !settled; i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    expect(settled, isTrue, reason: 'the ready wait must hit its deadline');
    final result = await future;
    await tester.pump();

    expect(result.started, isFalse);
    expect(result.failure, MicRecordingFailure.inputNotDelivering);
    expect(
      engine.stateReads,
      greaterThanOrEqualTo(2),
      reason: 'the confirming re-read must actually have happened',
    );
    controller.dispose();
  });

  testWidgets('pending-start ABA: an invalidated start\'s cleanup never '
      'disarms the native recording a NEWER start armed', (tester) async {
    // A arms natively and parks in its ready wait (nothing ever lands in its
    // ring); A is invalidated and B starts immediately. A's cleanup used to
    // call stopInputRecording() unconditionally — stopping B's native
    // recording. The arm token must make it a no-op.
    final engine = _FakeEngine()
      ..recordWrittenScript = [0]
      ..writtenTailStep = 0; // A never becomes ready
    final destA = _FakeDestination(path: '/fake/a.wav');
    final destB = _FakeDestination(path: '/fake/b.wav');
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    var aSettled = false;
    final a = controller.start(destA.strategy);
    unawaited(a.whenComplete(() => aSettled = true));
    // Let A reach its ready wait (destination created + native arm done).
    for (var i = 0; i < 50 && destA.created.isEmpty; i++) {
      await tester.pump(const Duration(milliseconds: 5));
    }
    expect(engine.startCalls, 1, reason: 'A armed natively');

    controller.invalidatePendingStart();
    // B arms while A's cleanup is still parked behind its poll sleep. Reset
    // the written script so B's ready handshake succeeds.
    engine
      ..recordWrittenScript = [512]
      ..writtenTailStep = 480
      ..writtenReads = 0;
    var bSettled = false;
    final b = controller.start(destB.strategy);
    unawaited(b.whenComplete(() => bSettled = true));
    for (var i = 0; i < 200 && !(aSettled && bSettled); i++) {
      await tester.pump(const Duration(milliseconds: 25));
    }
    final aResult = await a;
    final bResult = await b;
    await tester.pump();

    expect(aResult.started, isFalse, reason: 'A was invalidated');
    expect(bResult.started, isTrue, reason: 'B armed');
    expect(
      engine.stopCalls,
      0,
      reason: "A's invalidation cleanup must not stop B's native recording",
    );
    expect(controller.isRecording, isTrue, reason: 'B survives');
    controller.stop();
    controller.dispose();
  });

  testWidgets('ring overflow is surfaced: stop reports droppedFrames', (
    tester,
  ) async {
    final engine = _FakeEngine()..droppedFrames = 4800;
    final dest = _FakeDestination();
    final controller = MicRecordingController(
      acquireEngine: () => engine,
      requestPermission: () async => true,
    );

    final result = await controller.start(dest.strategy);
    expect(result.started, isTrue);

    final stopped = controller.stop();
    expect(stopped, isNotNull);
    expect(stopped!.clean, isTrue);
    expect(stopped.droppedFrames, 4800);
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
