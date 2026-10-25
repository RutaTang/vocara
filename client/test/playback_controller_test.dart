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

    // Regression guard: the timeline only grows when a save sees the take,
    // and nothing saves mid-take. A take running past the timeline's end (an
    // a cappella first take on the default 20 s timeline, singing on past the
    // backing) ran the record head and its forming waveform off the window,
    // and the view had nowhere to scroll after them.
    test('a take running past the timeline end stays in view', () {
      final c = controller(timelineMs: 20000);
      var relayouts = 0;
      c.viewport.addListener(() => relayouts++);
      recording = true;

      c.setRecordingPosition(const Duration(milliseconds: 26000));
      expect(
        c.viewStartMs + c.viewSpanMs,
        greaterThan(26000),
        reason: 'the fit-all view must reach the record head',
      );
      expect(relayouts, greaterThan(0), reason: 'the timeline must relayout');

      // Zoomed in, follow pages on past the old end.
      unawaited(c.zoomTimeline(0.25));
      c.setRecordingPosition(const Duration(milliseconds: 40000));
      expect(c.viewStartMs, lessThan(40000));
      expect(c.viewStartMs + c.viewSpanMs, greaterThan(40000));
    });

    test('the record head stretches the extent only while recording', () {
      final c = controller(timelineMs: 20000);
      recording = true;
      c.setRecordingPosition(const Duration(milliseconds: 26000));
      recording = false;
      expect(c.viewSpanMs, 20000, reason: 'the saved timeline takes over');
    });
  });

  // A take plays the backing freely from its start and disables the engine's
  // loop wrap. Every controller push used to re-send the remembered loop, so
  // any save during the take (a fader release → ensureLoopRange), a loop
  // toggle or handle drag re-armed the wrap, and the backing jumped back at
  // loop end under a mic that kept recording straight on: everything sung
  // after the jump landed against the wrong part of the song.
  group('during a take', () {
    PlaybackController looping() {
      final c = controller(timelineMs: 60000)..ensureLoopRange();
      c.setLoopStart(10000);
      c.setLoopEnd(20000);
      c.toggleLoop();
      // REC: the recording controller disables the wrap directly.
      recording = true;
      c.engine.setLoop(false, 0, 0);
      c.setRecordingPosition(const Duration(milliseconds: 30000));
      engine.calls.clear();
      return c;
    }

    test('a save\'s ensureLoopRange keeps the engine wrap off', () {
      final c = looping();
      c.ensureLoopRange();
      expect(engine.calls, ['loop:false:10000:20000']);
      expect(c.loopEnabled, isTrue, reason: 'the region itself is kept');
    });

    test('loop toggles and handle drags neither re-arm the wrap nor seek', () {
      final c = looping();
      c.toggleLoop();
      c.toggleLoop();
      c.setLoopStart(12000);
      c.setLoopEnd(18000);
      expect(
        engine.calls.where(
          (x) => x.startsWith('seek') || x.startsWith('loop:true'),
        ),
        isEmpty,
      );
      expect((c.loopStartMs, c.loopEndMs), (12000, 18000));
      expect(c.position.value, const Duration(milliseconds: 30000));
    });

    test('seek and stop leave the backing alone', () async {
      final c = looping();
      await c.seek(const Duration(milliseconds: 5000));
      await c.stop();
      expect(engine.calls, isEmpty);
    });

    test('the wrap re-arms once the take has stopped', () {
      final c = looping();
      recording = false;
      c.ensureLoopRange();
      expect(engine.calls, ['loop:true:10000:20000']);
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
