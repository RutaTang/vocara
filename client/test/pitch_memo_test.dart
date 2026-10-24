// The pitch views derive notes, a semitone range, flagged notes and per-note
// vibrato from a take's metrics on every rebuild; those derivations are now
// memoized per take. These pin that a memo is exactly the fresh computation,
// is computed once, and never leaks between takes.

import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';

/// 1.6 s notes with a ±25¢ 5.5 Hz vibrato, every fourth sung [detuneCents]
/// sharp.
VoiceMetrics _metrics({int frames = 4000, double detuneCents = 40}) {
  const sr = 48000, hop = 512;
  const fps = sr / hop;
  Float32List curve(double Function(int) f) =>
      Float32List.fromList([for (var i = 0; i < frames; i++) f(i)]);
  bool voiced(int i) => i % 160 < 150;
  double hz(int i) {
    if (!voiced(i)) return double.nan;
    final note = i ~/ 160;
    final cents =
        (note % 4 == 0 ? detuneCents : 0) +
        25 * math.sin(2 * math.pi * 5.5 * i / fps);
    return 220 *
        math.pow(2, (note % 7) / 12) *
        math.pow(2, cents / 1200).toDouble();
  }

  return VoiceMetrics(
    frames: frames,
    hop: hop,
    sampleRate: sr,
    ring: curve((_) => 0),
    tilt: curve((_) => 0),
    cpp: curve((_) => 0),
    loudness: curve((_) => -20),
    f0: curve(hz),
    f1: curve((_) => double.nan),
    f2: curve((_) => double.nan),
    f3: curve((_) => double.nan),
    h1h2: curve((_) => double.nan),
    active: Uint8List.fromList([
      for (var i = 0; i < frames; i++) voiced(i) ? 1 : 0,
    ]),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
  );
}

void _expectSameNotes(List<PitchNote> a, List<PitchNote> b) {
  expect(a.length, b.length);
  for (var i = 0; i < a.length; i++) {
    expect(a[i].startFrame, b[i].startFrame);
    expect(a[i].endFrame, b[i].endFrame);
    expect(a[i].semitone, b[i].semitone);
    expect(a[i].medianCents, b[i].medianCents);
  }
}

void _expectSameVibrato(Vibrato a, Vibrato b) {
  bool same(double x, double y) => x == y || (x.isNaN && y.isNaN);
  expect(a.present, b.present);
  expect(same(a.rateHz, b.rateHz), isTrue);
  expect(same(a.extentCents, b.extentCents), isTrue);
  expect(same(a.regularity, b.regularity), isTrue);
  expect(same(a.jitter, b.jitter), isTrue);
  expect(same(a.onsetMs, b.onsetMs), isTrue);
  expect(a.deviationCents, b.deviationCents);
}

void main() {
  test('the memoized notes are the fresh segmentation, computed once', () {
    final m = _metrics();
    final notes = pitchNotesOf(m);
    expect(notes, isNotEmpty);
    _expectSameNotes(notes, segmentPitchNotes(m));
    expect(identical(pitchNotesOf(m), notes), isTrue);
    expect(() => notes.add(notes.first), throwsUnsupportedError);
  });

  test('the memoized range is the fresh range', () {
    final m = _metrics();
    expect(pitchRangeOf(m), pitchSemitoneRange(m));
    expect(identical(pitchRangeOf(m), pitchRangeOf(m)), isTrue);
  });

  test('the memoized flagged notes are the up-to-3 most off-pitch ones', () {
    final m = _metrics();
    final notes = pitchNotesOf(m);
    final worst = worstNotesOf(notes);
    // The same rule, computed fresh: by |cents| descending, beyond 30¢, top 3.
    final byCents = [for (var i = 0; i < notes.length; i++) i]
      ..sort(
        (a, b) =>
            notes[b].medianCents.abs().compareTo(notes[a].medianCents.abs()),
      );
    final want = byCents
        .where((i) => notes[i].medianCents.abs() > 30)
        .take(3)
        .toSet();
    expect(want, isNotEmpty, reason: 'the detuned notes should be flagged');
    expect(worst, want);
    expect(identical(worstNotesOf(notes), worst), isTrue);
  });

  test(
    'the memoized vibrato of a note is the fresh analysis, computed once',
    () {
      final m = _metrics();
      final notes = pitchNotesOf(m);
      var present = 0;
      for (final n in notes) {
        final v = noteVibratoOf(m, n);
        _expectSameVibrato(v, analyzeVibrato(m, n.startFrame, n.endFrame));
        expect(identical(noteVibratoOf(m, n), v), isTrue);
        if (v.present) present++;
      }
      expect(present, greaterThan(0), reason: 'the notes carry a vibrato');
    },
  );

  test('memos never leak between takes', () {
    final a = _metrics(detuneCents: 0);
    final b = _metrics(frames: 3000, detuneCents: 45);
    final notesA = pitchNotesOf(a);
    final notesB = pitchNotesOf(b);
    expect(identical(notesA, notesB), isFalse);
    _expectSameNotes(notesA, segmentPitchNotes(a));
    _expectSameNotes(notesB, segmentPitchNotes(b));
    expect(worstNotesOf(notesA), isEmpty);
    expect(worstNotesOf(notesB), isNotEmpty);
    expect(pitchRangeOf(b), pitchSemitoneRange(b));
  });
}
