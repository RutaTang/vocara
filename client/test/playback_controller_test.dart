import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/playback_controller.dart';

/// Transport fake: the position/playing streams are driven by the test, and
/// every engine call is recorded so the controller's pushes are assertable.
class _FakeEngine extends Fake implements PlaybackEngine {
  final _position = StreamController<Duration>.broadcast();
  final _playing = StreamController<bool>.broadcast();
  final List<String> calls = [];

  @override
  Stream<Duration> get position => _position.stream;
  @override
  Stream<bool> get playing => _playing.stream;
  @override
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => false;

  @override
  Future<void> seek(Duration to) async =>
      calls.add('seek:${to.inMilliseconds}');
  @override
  Future<void> play() async => calls.add('play');
  @override
  Future<void> pause() async => calls.add('pause');
  @override
  void setLoop(bool enabled, int startMs, int endMs) =>
      calls.add('loop:$enabled:$startMs:$endMs');
  @override
  Future<void> sync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async {}
  @override
  Future<void> setVolume(
    String id,
    double volume, {
    bool muted = false,
  }) async {}
  @override
  void setTrackPan(String id, double pan) {}
  @override
  void setMasterVolume(double gain) {}
  @override
  void setTrackFx(String trackId, TrackFx fx) {}
  @override
  void setBusFx(String busId, TrackFx fx) {}
  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {}
  @override
  void setBusPan(String busId, double pan) {}
  @override
  double busGainReductionDb(String busId) => 0;
  @override
  double trackGainReductionDb(String trackId) => 0;
  @override
  void setTrackSendLevel(String trackId, int slot, double levelDb) {}
  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  Future<void> dispose() async {}

  void emitPosition(Duration d) => _position.add(d);

  Future<void> close() async {
    await _position.close();
    await _playing.close();
  }
}

/// A practice whose timeline is [ms] long, with one take so contentEndMs is
/// real (the loop clamps to content, not to the padded timeline).
SongPractice _practice(int ms) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  timelineDurationMs: ms,
  vocalTracks: [
    AudioTrack(
      id: 't1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      audioFilePath: '/pool/a.wav',
      durationMs: ms,
      createdAt: DateTime(2026),
    ),
  ],
);

void main() {
  late _FakeEngine engine;
  var recording = false;

  PlaybackController controller({int timelineMs = 300000}) {
    engine = _FakeEngine();
    recording = false;
    final c = PlaybackController(
      engine: engine,
      practice: () => _practice(timelineMs),
      isRecording: () => recording,
    );
    addTearDown(() async {
      c.dispose();
      await engine.close();
    });
    return c;
  }

  group('viewport follow', () {
    test(
      'a playing cursor pages the view forward once it nears the edge',
      () async {
        final c = controller();
        unawaited(c.zoomTimeline(0.1)); // a 30 s window
        final span = c.viewSpanMs;
        expect(c.viewStartMs, 0);

        // Comfortably inside the window: the view stays put (no stutter).
        engine.emitPosition(Duration(milliseconds: span ~/ 2));
        await pumpEventQueue();
        expect(c.viewStartMs, 0);

        // Past the trigger band: page forward.
        engine.emitPosition(Duration(milliseconds: span - 1));
        await pumpEventQueue();
        expect(c.viewStartMs, greaterThan(0));
      },
    );

    test('a manual pan turns follow off and the view stops tracking', () async {
      final c = controller();
      unawaited(c.zoomTimeline(0.1));
      final span = c.viewSpanMs;

      c.panView(1000);
      expect(c.followCursor, isFalse);
      final parked = c.viewStartMs;

      engine.emitPosition(Duration(milliseconds: span * 3));
      await pumpEventQueue();
      expect(c.viewStartMs, parked);
    });

    // Regression guard: while a take is armed the engine's position stream is
    // ignored (the record tick owns the playhead), and the record tick wrote
    // the notifier directly — so auto-scroll silently stopped for the whole
    // take and the record head ran off the right edge of the window.
    test('the RECORDING clock follows the cursor too', () async {
      final c = controller();
      unawaited(c.zoomTimeline(0.1));
      final span = c.viewSpanMs;
      recording = true;

      // The engine keeps streaming during a take; the controller ignores it.
      engine.emitPosition(Duration(milliseconds: span * 2));
      await pumpEventQueue();
      expect(c.viewStartMs, 0, reason: 'the record clock owns the playhead');

      c.setRecordingPosition(Duration(milliseconds: span - 1));
      expect(
        c.viewStartMs,
        greaterThan(0),
        reason: 'the view must track the record head',
      );
      expect(c.position.value, Duration(milliseconds: span - 1));
    });

    test('a recording that stays inside the window does not scroll it', () {
      final c = controller();
      unawaited(c.zoomTimeline(0.1));
      recording = true;

      c.setRecordingPosition(Duration(milliseconds: c.viewSpanMs ~/ 2));
      expect(c.viewStartMs, 0);
    });
  });

  group('loop region', () {
    test('ensureLoopRange defaults the region to the content length', () {
      final c = controller(timelineMs: 60000);
      c.ensureLoopRange();
      expect(c.loopStartMs, 0);
      expect(c.loopEndMs, 60000);
      expect(engine.calls, contains('loop:false:0:60000'));
    });

    test('the loop edges keep a minimum span in both directions', () {
      final c = controller(timelineMs: 60000)..ensureLoopRange();

      c.setLoopStart(59_990);
      expect(c.loopStartMs, 60000 - PlaybackController.minLoopMs);

      c.setLoopEnd(10);
      expect(c.loopEndMs, c.loopStartMs + PlaybackController.minLoopMs);
    });

    test('enabling the loop snaps a playhead sitting outside it inside', () {
      final c = controller(timelineMs: 60000)..ensureLoopRange();
      c.setLoopStart(20000);
      engine.calls.clear();

      c.toggleLoop();

      expect(c.position.value, const Duration(milliseconds: 20000));
      expect(engine.calls, contains('seek:20000'));
    });
  });
}
