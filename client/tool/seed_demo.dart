// Standalone helper (run with `dart run tool/seed_demo.dart`) that writes a
// demo multi-track practice into the macOS app sandbox so the DAW UI and the
// Voiceprint spectrogram can be previewed/screenshotted. Prints the new
// practice id. Not part of the app.
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

const _sr = 44100;
const _durSec = 8;
const _durMs = _durSec * 1000;
int _noteCounter = 0;

double _env(double t, int seed) {
  final phrase = sin(t * pi * (5 + seed)).abs();
  final syllable = 0.5 + 0.5 * sin(t * 2 * pi * (38 + seed * 9));
  var gap = 1.0;
  if (sin(t * 2 * pi * (2.5 + seed * 0.7) + seed) < -0.55) gap = 0.04;
  return ((0.2 + 0.8 * phrase) * (0.35 + 0.65 * syllable) * gap)
      .clamp(0.0, 1.0);
}

List<double> _peaks(int seed, {int n = 1000}) =>
    [for (var i = 0; i < n; i++) _env(i / n, seed)];

Map<String, dynamic> _note(int positionMs, String content, String now) => {
      'id': 'demo-note-${_noteCounter++}',
      'positionMs': positionMs,
      'content': content,
      'createdAt': now,
      'updatedAt': now,
    };

Map<String, dynamic> _track({
  required String id,
  required String kind,
  required String name,
  required String audioPath,
  required int seed,
  required double volume,
  required String now,
  int startMs = 0,
  List<Map<String, dynamic>> notes = const [],
}) =>
    {
      'id': id,
      'kind': kind,
      'displayName': name,
      'audioFilePath': audioPath,
      'durationMs': _durMs,
      'startMs': startMs,
      'waveformPeaks': _peaks(seed),
      'notes': notes,
      'colorValue': null,
      'muted': false,
      'volume': volume,
      'createdAt': now,
    };

/// Synthesizes a voice-like WAV: harmonics of a ~196 Hz F0 (with vibrato),
/// shaped by moving F1/F2 formants plus a Singer's Formant near 3 kHz — so the
/// spectrogram shows harmonic lines, formant bands and the 3 kHz "ring".
void _writeVoiceWav(File file) {
  final n = _sr * _durSec;
  final tmp = Float64List(n);
  const k = 45;
  final phase = List<double>.filled(k + 1, 0);
  var peak = 1e-9;

  double formant(double f, double t) {
    final f1 = 500 + 250 * sin(2 * pi * 0.18 * t);
    final f2 = 1300 + 700 * sin(2 * pi * 0.18 * t + 1.0);
    final centers = [f1, f2, 2600.0, 3000.0];
    const bw = [90.0, 110.0, 160.0, 120.0];
    const gain = [1.0, 0.7, 0.5, 0.65];
    var g = 0.0;
    for (var i = 0; i < centers.length; i++) {
      final d = (f - centers[i]) / bw[i];
      g += gain[i] / (1 + d * d);
    }
    return g;
  }

  for (var i = 0; i < n; i++) {
    final t = i / _sr;
    final f0 = 196.0 *
        (1 + 0.025 * sin(2 * pi * 5.2 * t)) *
        (1 + 0.05 * sin(2 * pi * 0.25 * t));
    var v = 0.0;
    for (var h = 1; h <= k; h++) {
      final f = h * f0;
      if (f > 5200) break;
      phase[h] += 2 * pi * f / _sr;
      v += (1.0 / h) * formant(f, t) * sin(phase[h]);
    }
    tmp[i] = v;
    if (v.abs() > peak) peak = v.abs();
  }

  final norm = 0.85 / peak;
  final dataSize = n * 2;
  final b = BytesBuilder();
  void s(String v) => b.add(v.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  s('RIFF');
  u32(36 + dataSize);
  s('WAVE');
  s('fmt ');
  u32(16);
  u16(1);
  u16(1);
  u32(_sr);
  u32(_sr * 2);
  u16(2);
  u16(16);
  s('data');
  u32(dataSize);
  final pcm = ByteData(dataSize);
  for (var i = 0; i < n; i++) {
    pcm.setInt16(
        i * 2, (tmp[i] * norm * 32767).round().clamp(-32768, 32767), Endian.little);
  }
  b.add(pcm.buffer.asUint8List());
  file.writeAsBytesSync(b.toBytes());
}

void main() {
  final home = Platform.environment['HOME']!;
  final base =
      '$home/Library/Containers/com.vocara.vocara/Data/Library/Application Support/com.vocara.vocara';
  final practiceId = 'demo-${DateTime.now().millisecondsSinceEpoch}';
  final audioDir = Directory('$base/audio/$practiceId')..createSync(recursive: true);
  final wavPath = '${audioDir.path}/voice.wav';
  _writeVoiceWav(File(wavPath));

  final now = DateTime.now().toIso8601String();

  final practice = {
    'id': practiceId,
    'title': '海阔天空',
    'accompaniment': _track(
      id: '$practiceId-acc',
      kind: 'accompaniment',
      name: 'Backing track — 海阔天空 (Beyond)',
      audioPath: wavPath,
      seed: 0,
      volume: 0.85,
      now: now,
    ),
    'reference': _track(
      id: '$practiceId-ref',
      kind: 'reference',
      name: 'Original — 海阔天空 (Beyond)',
      audioPath: wavPath,
      seed: 5,
      volume: 0.9,
      now: now,
    ),
    'vocalTracks': [
      _track(
        id: '$practiceId-v1',
        kind: 'vocal',
        name: 'Take 1 · first try',
        audioPath: wavPath,
        seed: 1,
        volume: 1.0,
        now: now,
        notes: [
          _note(1500, 'Breath runs out before the line ends', now),
          _note(3800, 'High note is flat — needs more support', now),
          _note(6200, 'Nice ring here, keep it', now),
        ],
      ),
      _track(
        id: '$practiceId-v2',
        kind: 'vocal',
        name: 'Take 2 · full voice',
        audioPath: wavPath,
        seed: 2,
        volume: 0.95,
        now: now,
        notes: [
          _note(2500, 'Better support this time', now),
          _note(5500, 'Small crack — ease into chest voice', now),
        ],
      ),
    ],
    'lyrics': [
      {'text': '今天我 寒夜里看雪飘过', 'timeMs': 0},
      {'text': '怀着冷却了的心 窝漂远方', 'timeMs': 2400},
      {'text': '风雨里追赶 雾里分不清影踪', 'timeMs': 4800},
      {'text': '', 'timeMs': null},
      {'text': '天空海阔 你与我', 'timeMs': 6400},
      {'text': '可会变 谁没在变', 'timeMs': 7600},
    ],
    'createdAt': now,
    'updatedAt': now,
  };

  File('$base/practices/$practiceId.json')
      .writeAsStringSync(jsonEncode(practice));
  // ignore: avoid_print
  print(practiceId);
}
