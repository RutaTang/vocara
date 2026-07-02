// Value-equality semantics for the domain model value types, plus the
// defensive-copy / no-shared-mutable-state guarantees around their list fields.
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_device.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

final _t0 = DateTime.utc(2026, 1, 1);
final _t1 = DateTime.utc(2026, 1, 2);

/// Asserts [a] and [b] are equal by value with matching hashCodes, and that
/// both differ from [other].
void _expectValueEquality(Object a, Object b, Object other) {
  expect(identical(a, b), isFalse, reason: 'test needs distinct instances');
  expect(a, equals(b));
  expect(a.hashCode, b.hashCode);
  expect(a, isNot(equals(other)));
}

TrackFx _richFx() => TrackFx.flat().copyWith(
  eqOn: true,
  comp: const CompSettings(thresholdDb: -22, ratio: 4),
  compOn: true,
  delay: const DelaySettings(timeMs: 250, feedback: 0.4, mix: 0.2),
  sendDb: -9,
  order: FxKind.values.reversed.toList(),
);

VoiceMetrics _metrics() {
  Float32List flat(double v) => Float32List(4)..fillRange(0, 4, v);
  return VoiceMetrics(
    frames: 4,
    hop: 512,
    sampleRate: 44100,
    ring: flat(0),
    tilt: flat(0),
    cpp: flat(0),
    loudness: flat(-20),
    f0: flat(200),
    f1: flat(double.nan),
    f2: flat(double.nan),
    f3: flat(double.nan),
    h1h2: flat(double.nan),
    active: Uint8List(4),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
    peakDbfs: -6,
    clippedFraction: 0,
    snrDb: double.nan,
  );
}

void main() {
  group('TrackFx.flat aliasing regression', () {
    test('the flat band list is immutable (cannot become shared mutable '
        'state across tracks)', () {
      final a = TrackFx.flat();
      expect(
        () => a.eq[0] = const EqBand(kind: EqBandKind.lowShelf, freq: 100),
        throwsUnsupportedError,
      );
      expect(() => a.eq.add(a.eq.first), throwsUnsupportedError);
    });

    test('editing one flat instance via withBand leaves another untouched', () {
      final a = TrackFx.flat();
      final b = TrackFx.flat();
      final edited = a.withBand(0, a.eq[0].copyWith(gainDb: 6));
      expect(edited.eq[0].gainDb, 6);
      expect(a.eq.every((band) => band.gainDb == 0), isTrue);
      expect(b.eq.every((band) => band.gainDb == 0), isTrue);
    });

    test('the fromJson flat fallback shares no mutable state either', () {
      final fromEmpty = TrackFx.fromJson(const {});
      expect(
        () => fromEmpty.eq[0] = fromEmpty.eq[0].copyWith(gainDb: 6),
        throwsUnsupportedError,
      );
    });
  });

  group('copyWith defensive copies', () {
    test('TrackFx.copyWith copies the provided eq and order lists', () {
      final bands = TrackFx.flat().eq.toList();
      final order = FxKind.values.toList();
      final fx = TrackFx.flat().copyWith(eq: bands, order: order);
      bands[0] = bands[0].copyWith(gainDb: 12);
      order.removeLast();
      expect(fx.eq[0].gainDb, 0);
      expect(fx.order, FxKind.values);
    });

    test('FxBus.copyWith copies the provided sends list', () {
      final sends = [const TrackSend(busId: 'b2', levelDb: -6)];
      final bus = FxBus(
        id: 'b1',
        name: 'Bus',
        fx: TrackFx.flat(),
      ).copyWith(sends: sends);
      sends.clear();
      expect(bus.sends, hasLength(1));
    });

    test('SongPractice.copyWith copies the provided lists', () {
      final lyrics = [const LyricLine(text: 'la', timeMs: 100)];
      final versions = [TakeVersion(id: 'v1', name: 'One', createdAt: _t0)];
      final practice = SongPractice(
        id: 'p1',
        title: 'Song',
        createdAt: _t0,
        updatedAt: _t0,
      ).copyWith(lyrics: lyrics, versions: versions);
      lyrics.clear();
      versions.clear();
      expect(practice.lyrics, hasLength(1));
      expect(practice.versions, hasLength(1));
    });

    test('Technique.copyWith copies the provided lists', () {
      final tags = ['Quality'];
      final technique = Technique(
        id: 't1',
        name: 'Twang',
        createdAt: _t0,
        updatedAt: _t0,
      ).copyWith(tags: tags);
      tags.add('Register');
      expect(technique.tags, ['Quality']);
    });
  });

  group('FX settings value equality', () {
    // Note: two identical const invocations canonicalize to ONE instance, so
    // the equal-by-value partner is derived with a no-arg copyWith (a fresh
    // runtime instance with the same field values).
    test('EqBand', () {
      const a = EqBand(kind: EqBandKind.peaking, freq: 700, q: 1.2, gainDb: 3);
      _expectValueEquality(a, a.copyWith(), a.copyWith(gainDb: 4));
    });

    test('CompSettings', () {
      const a = CompSettings(thresholdDb: -22, ratio: 4);
      _expectValueEquality(a, a.copyWith(), a.copyWith(ratio: 5));
    });

    test('DeEsserSettings', () {
      const a = DeEsserSettings(freq: 7000);
      _expectValueEquality(a, a.copyWith(), a.copyWith(freq: 7500));
    });

    test('GateSettings', () {
      const a = GateSettings(thresholdDb: -40);
      _expectValueEquality(a, a.copyWith(), a.copyWith(thresholdDb: -42));
    });

    test('DelaySettings', () {
      const a = DelaySettings(timeMs: 250, mix: 0.2);
      _expectValueEquality(a, a.copyWith(), a.copyWith(timeMs: 300));
    });

    test('SaturationSettings', () {
      const a = SaturationSettings(driveDb: 9, mix: 0.3);
      _expectValueEquality(a, a.copyWith(), a.copyWith(mix: 0.4));
    });

    test('DoublerSettings', () {
      const a = DoublerSettings(delayMs: 14, mix: 0.35);
      _expectValueEquality(a, a.copyWith(), a.copyWith(delayMs: 16));
    });

    test('LimiterSettings', () {
      const a = LimiterSettings(ceilingDb: -0.5);
      _expectValueEquality(a, a.copyWith(), a.copyWith(ceilingDb: -1));
    });

    test('ReverbSettings', () {
      const a = ReverbSettings(roomSize: 0.6, wet: 0.3);
      _expectValueEquality(a, a.copyWith(), a.copyWith(wet: 0.35));
    });

    test('TrackSend', () {
      const a = TrackSend(busId: 'b1', levelDb: -6);
      _expectValueEquality(a, a.copyWith(), a.copyWith(busId: 'b2'));
    });
  });

  group('TrackFx value equality', () {
    test('equal by value across distinct list instances', () {
      // copyWith stores fresh list copies, so these two hold distinct (but
      // element-equal) eq/order lists.
      _expectValueEquality(
        _richFx(),
        _richFx(),
        _richFx().copyWith(eqOn: false),
      );
    });

    test('copyWith round-trip preserves equality', () {
      final fx = _richFx();
      expect(fx.copyWith(), equals(fx));
      expect(fx.copyWith(sendDb: 0).copyWith(sendDb: fx.sendDb), equals(fx));
    });

    test('JSON round-trip preserves equality', () {
      final fx = _richFx();
      expect(TrackFx.fromJson(fx.toJson()), equals(fx));
    });

    test('a differing band or order breaks equality', () {
      final fx = _richFx();
      expect(fx.withBand(1, fx.eq[1].copyWith(gainDb: 2)), isNot(equals(fx)));
      expect(fx.copyWith(order: FxKind.values), isNot(equals(fx)));
    });
  });

  group('aggregate value equality', () {
    FxBus bus() => FxBus(
      id: 'b1',
      name: 'Verb',
      fx: _richFx(),
      colorValue: 0xFF112233,
      volume: 0.8,
      pan: -0.2,
      outputBusId: 'b2',
      sends: [const TrackSend(busId: 'b3', levelDb: -12)],
    );

    test('FxBus', () {
      _expectValueEquality(bus(), bus(), bus().copyWith(muted: true));
      expect(FxBus.fromJson(bus().toJson()), equals(bus()));
    });

    SongPractice practice() => SongPractice(
      id: 'p1',
      title: 'Song',
      createdAt: _t0,
      updatedAt: _t1,
      versions: [TakeVersion(id: 'v1', name: 'One', createdAt: _t0)],
      activeVersionId: 'v1',
      lyrics: [const LyricLine(text: 'la', timeMs: 100)],
      timelineDurationMs: 60000,
      reverb: const ReverbSettings(wet: 0.3),
      masterVolume: 0.9,
      fxBuses: [bus()],
    );

    test('SongPractice', () {
      _expectValueEquality(
        practice(),
        practice(),
        practice().copyWith(title: 'Other'),
      );
      expect(practice().copyWith(), equals(practice()));
      expect(SongPractice.fromJson(practice().toJson()), equals(practice()));
    });

    test('TakeVersion', () {
      _expectValueEquality(
        TakeVersion(id: 'v1', name: 'One', createdAt: _t0),
        TakeVersion(id: 'v1', name: 'One', createdAt: _t0),
        TakeVersion(id: 'v1', name: 'Two', createdAt: _t0),
      );
    });

    test('PracticeNote', () {
      PracticeNote note() => PracticeNote(
        id: 'n1',
        positionMs: 1000,
        endMs: 2000,
        content: 'breath',
        createdAt: _t0,
        updatedAt: _t1,
      );
      _expectValueEquality(note(), note(), note().copyWith(content: 'pitch'));
    });

    TechniqueMedia media() => TechniqueMedia(
      id: 'm1',
      path: '/sandbox/clip.m4a',
      kind: TechniqueMediaKind.audio,
      label: 'Example',
      addedAt: _t0,
    );

    test('TechniqueMedia', () {
      _expectValueEquality(media(), media(), media().copyWith(label: 'Other'));
    });

    Technique technique() => Technique(
      id: 't1',
      name: 'Twang',
      aliases: const ['Edge'],
      tags: const ['Quality'],
      summary: 'Bright, brassy quality.',
      media: [media()],
      createdAt: _t0,
      updatedAt: _t1,
    );

    test('Technique', () {
      _expectValueEquality(
        technique(),
        technique(),
        technique().copyWith(name: 'Curbing'),
      );
      expect(Technique.fromJson(technique().toJson()), equals(technique()));
    });

    test('AudioDevice compares every field', () {
      // Built through a runtime parameter so the two equal instances are not
      // const-canonicalized into one.
      AudioDevice device(String name) =>
          AudioDevice(uid: 'u1', name: name, rate: 48000, isInput: true);
      _expectValueEquality(
        device('Mic'),
        device('Mic'),
        // Same physical device (uid) but renamed: no longer equal by value.
        device('Mic 2'),
      );
    });
  });

  group('AudioClip value equality', () {
    AudioClip clip() => const AudioClip(
      id: 'c1',
      audioFilePath: '/sandbox/take.wav',
      durationMs: 4000,
      startMs: 1500,
      waveformPeaks: [0.1, 0.8, 0.4],
    );

    test('equal by value with matching hashCode', () {
      _expectValueEquality(
        clip(),
        clip().copyWith(),
        clip().copyWith(startMs: 0),
      );
    });

    test('differing waveform peaks break equality', () {
      expect(
        clip().copyWith(waveformPeaks: [0.1, 0.8, 0.5]),
        isNot(equals(clip())),
      );
    });

    test('copyWith round-trip and JSON round-trip preserve equality', () {
      final c = clip();
      expect(c.copyWith(startMs: 0).copyWith(startMs: c.startMs), equals(c));
      expect(AudioClip.fromJson(c.toJson()), equals(c));
    });

    test('copyWith copies the provided waveform peaks list', () {
      final peaks = [0.2, 0.6];
      final c = clip().copyWith(waveformPeaks: peaks);
      peaks[0] = 0.9;
      expect(c.waveformPeaks, [0.2, 0.6]);
    });
  });

  group('AudioTrack value equality', () {
    AudioClip clip(String id, int startMs) => AudioClip(
      id: id,
      audioFilePath: '/sandbox/$id.wav',
      durationMs: 2000,
      startMs: startMs,
      waveformPeaks: const [0.3, 0.7],
    );

    AudioTrack track() => AudioTrack(
      id: 'tr1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      createdAt: _t0,
      clips: [clip('c1', 0), clip('c2', 5000)],
      notes: [
        PracticeNote(
          id: 'n1',
          positionMs: 1000,
          content: 'breath',
          createdAt: _t0,
          updatedAt: _t1,
        ),
      ],
      colorValue: 0xFF445566,
      volume: 0.8,
      pan: -0.1,
      versionId: 'v1',
      fx: _richFx(),
      busId: 'b1',
      sends: [const TrackSend(busId: 'b2', levelDb: -6)],
    );

    test('equal by value with matching hashCode', () {
      _expectValueEquality(track(), track(), track().copyWith(muted: true));
    });

    test('copyWith round-trip preserves equality', () {
      final t = track();
      expect(t.copyWith(), equals(t));
      expect(t.copyWith(volume: 0.5).copyWith(volume: t.volume), equals(t));
    });

    test('JSON round-trip preserves equality', () {
      final t = track();
      expect(AudioTrack.fromJson(t.toJson()), equals(t));
    });

    test('two tracks differing only in clip order are NOT equal', () {
      // Clips are kept in creation order (a punch-in can land before an
      // existing clip), so order is part of the lane's value.
      final a = track();
      final b = a.copyWith(clips: a.clips.reversed.toList());
      expect(b, isNot(equals(a)));
    });

    test('a differing nested clip, note, or send breaks equality', () {
      final t = track();
      expect(
        t.copyWith(clips: [clip('c1', 0), clip('c2', 5001)]),
        isNot(equals(t)),
      );
      expect(t.copyWith(notes: const []), isNot(equals(t)));
      expect(
        t.copyWith(sends: [const TrackSend(busId: 'b2', levelDb: -7)]),
        isNot(equals(t)),
      );
    });

    test('copyWith copies the provided clips, notes and sends lists', () {
      final clips = [clip('c1', 0)];
      final sends = [const TrackSend(busId: 'b2', levelDb: -6)];
      final t = track().copyWith(clips: clips, notes: [], sends: sends);
      clips.clear();
      sends.clear();
      expect(t.clips, hasLength(1));
      expect(t.sends, hasLength(1));
    });
  });

  group('TechniqueAnalysis equality', () {
    test(
      'field-wise, with heavy members deliberately compared by identity',
      () {
        final metrics = _metrics();
        final spectrogram = Spectrogram(
          frames: 2,
          bins: 2,
          maxHz: 5000,
          sampleRate: 44100,
          fftSize: 2048,
          hop: 512,
          db: Float32List(4),
          minDb: -80,
          maxDb: 0,
        );
        final ltas = Ltas(
          db: Float32List(4),
          binHz: 21.5,
          maxHz: 5000,
          maxDb: -20,
          alphaRatio: -12,
          singersFormantDb: 3,
          frameCount: 2,
        );
        const profile = VoiceProfile(
          lowMidi: 48,
          highMidi: 72,
          tessLoMidi: 55,
          tessHiMidi: 65,
          passaggioMidi: null,
          passaggioJumpDb: 0,
          softDb: -30,
          loudDb: -10,
          medianDb: -20,
        );
        final register = RegisterMap(
          score: Float32List(4),
          chestPct: 50,
          mixPct: 30,
          headPct: 20,
          voicedFrames: 4,
        );
        TechniqueAnalysis analysis({VoiceMetrics? m}) => TechniqueAnalysis(
          metrics: m ?? metrics,
          spectrogram: spectrogram,
          ltas: ltas,
          profile: profile,
          register: register,
          durationMs: 1000,
        );

        // Same artifact instances: equal by value.
        _expectValueEquality(
          analysis(),
          analysis(),
          // A content-identical but DISTINCT VoiceMetrics instance is unequal:
          // the data-heavy analysis types keep identity equality on purpose
          // (comparing big Float32 buffers element-wise is a perf trap).
          analysis(m: _metrics()),
        );
      },
    );
  });
}
