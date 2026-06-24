import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_presets.dart';
import 'package:vocara/domain/models/song_practice.dart';

void main() {
  group('TrackFx', () {
    test('flat default is transparent and off', () {
      final fx = TrackFx.flat();
      expect(fx.eq.length, 6);
      expect(fx.eq.every((b) => b.gainDb == 0.0), isTrue);
      expect(fx.eqOn, isFalse);
      expect(fx.compOn, isFalse);
      expect(fx.hasReverbSend, isFalse);
    });

    test('round-trips through JSON', () {
      final fx = TrackFx.flat().copyWith(
        eqOn: true,
        compOn: true,
        comp: const CompSettings(thresholdDb: -22, ratio: 4, makeupDb: 3),
        sendDb: -9,
      ).withBand(0, const EqBand(kind: EqBandKind.lowShelf, freq: 100, q: 0.7, gainDb: 3));

      final back = TrackFx.fromJson(fx.toJson());
      expect(back.eqOn, isTrue);
      expect(back.compOn, isTrue);
      expect(back.comp.thresholdDb, -22);
      expect(back.comp.makeupDb, 3);
      expect(back.sendDb, -9);
      expect(back.eq.first.gainDb, 3);
      expect(back.eq.first.kind, EqBandKind.lowShelf);
    });

    test('legacy 4-band EQ migrates onto the 6 fixed slots by role', () {
      // A pre-migration take saved with the old [lowShelf, peak, peak, highShelf]
      // layout. The high shelf must NOT end up on a peaking engine slot.
      const legacy = TrackFx(eq: [
        EqBand(kind: EqBandKind.lowShelf, freq: 120, q: 0.7, gainDb: 4),
        EqBand(kind: EqBandKind.peaking, freq: 500, q: 1.0, gainDb: -2),
        EqBand(kind: EqBandKind.peaking, freq: 3000, q: 1.0, gainDb: 1),
        EqBand(kind: EqBandKind.highShelf, freq: 8000, q: 0.7, gainDb: 3),
      ]);
      final back = TrackFx.fromJson(legacy.toJson());

      expect(back.eq.length, 6);
      expect(
        back.eq.map((b) => b.kind),
        const [
          EqBandKind.lowShelf,
          EqBandKind.peaking,
          EqBandKind.peaking,
          EqBandKind.peaking,
          EqBandKind.peaking,
          EqBandKind.highShelf,
        ],
      );
      // Shelves preserved at the shelf slots (0 and 5), not lost to a peak slot.
      expect(back.eq.first.freq, 120);
      expect(back.eq.first.gainDb, 4);
      expect(back.eq.last.freq, 8000);
      expect(back.eq.last.gainDb, 3);
      // The two old peaks fill the first two peak slots.
      expect(back.eq[1].freq, 500);
      expect(back.eq[2].freq, 3000);
    });

    test('insert-chain order round-trips; legacy JSON normalizes to all kinds',
        () {
      // Default chain = every plugin kind, in canonical order.
      expect(TrackFx.flat().order, FxKind.values);

      // A saved custom order keeps its leading arrangement; missing kinds are
      // appended (so newly-added plugin types always show up).
      final fx = TrackFx.flat().copyWith(order: const [FxKind.comp, FxKind.eq]);
      final back = TrackFx.fromJson(fx.toJson());
      expect(back.order.take(2), [FxKind.comp, FxKind.eq]);
      expect(back.order.toSet(), FxKind.values.toSet());
      expect(back.order.length, FxKind.values.length);

      // Legacy take without an `order` key → all kinds, canonical order.
      final legacy = Map<String, dynamic>.from(TrackFx.flat().toJson())
        ..remove('order');
      expect(TrackFx.fromJson(legacy).order, FxKind.values);
    });
  });

  test('ReverbSettings round-trips', () {
    const r = ReverbSettings(roomSize: 0.8, damping: 0.3, wet: 0.4, width: 0.9);
    final back = ReverbSettings.fromJson(r.toJson());
    expect(back.roomSize, 0.8);
    expect(back.damping, 0.3);
    expect(back.wet, 0.4);
    expect(back.width, 0.9);
  });

  group('non-breaking persistence', () {
    test('AudioTrack with fx round-trips; legacy JSON has null fx', () {
      final t = AudioTrack(
        id: 't1',
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        audioFilePath: '/x.wav',
        durationMs: 1000,
        createdAt: DateTime(2026, 6, 18),
        fx: TrackFx.flat().copyWith(eqOn: true, sendDb: -6),
      );
      final back = AudioTrack.fromJson(t.toJson());
      expect(back.fx, isNotNull);
      expect(back.fx!.eqOn, isTrue);
      expect(back.fx!.sendDb, -6);

      // Legacy JSON without 'fx' → null (no migration step needed).
      final legacy = Map<String, dynamic>.from(t.toJson())..remove('fx');
      expect(AudioTrack.fromJson(legacy).fx, isNull);
    });

    test('SongPractice with reverb round-trips; legacy JSON has null reverb', () {
      final p = SongPractice(
        id: 'p1',
        title: 'Song',
        createdAt: DateTime(2026, 6, 18),
        updatedAt: DateTime(2026, 6, 18),
        reverb: const ReverbSettings(wet: 0.3),
      );
      final back = SongPractice.fromJson(p.toJson());
      expect(back.reverb, isNotNull);
      expect(back.reverb!.wet, 0.3);

      final legacy = Map<String, dynamic>.from(p.toJson())..remove('reverb');
      expect(SongPractice.fromJson(legacy).reverb, isNull);
    });
  });

  group('presets', () {
    test('vocal presets are well-formed', () {
      final presets = FxPresets.vocal;
      expect(presets.length, greaterThanOrEqualTo(12));
      expect(presets.map((p) => p.name),
          containsAll(['Flat', 'Warm Lead', 'Bright Lead', 'Pop Throw']));
      // First slot is the dry reference with everything bypassed.
      final flat = presets.first;
      expect(flat.name, 'Flat');
      expect(flat.fx.eqOn, isFalse);
      expect(flat.fx.compOn, isFalse);
      expect(flat.fx.reverbOn, isFalse);
      for (final p in presets) {
        expect(p.fx.eq.length, 6);
        expect(p.description, isNotEmpty);
      }
      final warm = presets.firstWhere((p) => p.name == 'Warm Lead').fx;
      expect(warm.compOn, isTrue);
    });

    test('every enabled plugin param is within its allowed range', () {
      for (final p in FxPresets.vocal) {
        final fx = p.fx;
        for (final b in fx.eq) {
          expect(b.gainDb, inInclusiveRange(-12, 12), reason: p.name);
        }
        if (fx.compOn) {
          expect(fx.comp.thresholdDb, inInclusiveRange(-60, 0), reason: p.name);
          expect(fx.comp.ratio, inInclusiveRange(1, 20), reason: p.name);
          expect(fx.comp.makeupDb, inInclusiveRange(0, 18), reason: p.name);
        }
        if (fx.deEsserOn) {
          expect(fx.deEsser.freq, inInclusiveRange(2000, 16000), reason: p.name);
          expect(fx.deEsser.rangeDb, inInclusiveRange(0, 24), reason: p.name);
        }
        if (fx.gateOn) {
          expect(fx.gate.thresholdDb, inInclusiveRange(-80, 0), reason: p.name);
          expect(fx.gate.rangeDb, inInclusiveRange(0, 80), reason: p.name);
        }
        if (fx.delayOn) {
          expect(fx.delay.timeMs, inInclusiveRange(1, 1500), reason: p.name);
          expect(fx.delay.feedback, inInclusiveRange(0, 0.95), reason: p.name);
          // An enabled time-based FX must be audible (non-zero mix).
          expect(fx.delay.mix, greaterThan(0), reason: p.name);
        }
        if (fx.saturationOn) {
          expect(fx.saturation.driveDb, inInclusiveRange(0, 36), reason: p.name);
          expect(fx.saturation.mix, greaterThan(0), reason: p.name);
        }
        if (fx.doublerOn) {
          expect(fx.doubler.delayMs, inInclusiveRange(1, 50), reason: p.name);
          expect(fx.doubler.mix, greaterThan(0), reason: p.name);
        }
        if (fx.limiterOn) {
          expect(fx.limiter.ceilingDb, inInclusiveRange(-12, 0), reason: p.name);
        }
        if (fx.reverbOn) {
          expect(fx.reverb.wet, greaterThan(0), reason: p.name);
          expect(fx.reverb.wet, inInclusiveRange(0, 1), reason: p.name);
        }
      }
    });

    test('reverb presets include Dry (silent) and wet rooms', () {
      final dry = FxPresets.reverb.firstWhere((p) => p.name == 'Dry').reverb;
      expect(dry.wet, 0.0);
      final hall = FxPresets.reverb.firstWhere((p) => p.name == 'Hall').reverb;
      expect(hall.wet, greaterThan(0));
    });
  });
}
