// Writes models_json.txt: what the Flutter app's own model code makes of a
// set of stored JSON shapes, for the Rust port to reproduce byte for byte.
//
// Each line is `<kind>\t<input>\t<output>`:
// - for a model, <input> is JSON text and <output> is
//   `jsonEncode(Model.fromJson(jsonDecode(input)).toJson())`, or THROWS when
//   the Dart factory throws;
// - `datetime` lines hold a `DateTime.parse` input and its
//   `toIso8601String()` (THROWS when parse throws), plus `|<isUtc>|<micros>`
//   for UTC values;
// - `preset` / `reverbPreset` / `flat` lines hold the JSON of the built-in
//   presets.
//
// Run with the client's model sources and a stand-in for
// package:flutter/foundation.dart (immutable, listEquals, visibleForTesting),
// e.g. a package_config.json mapping `vocara` to client/ and `flutter` to a
// stub package, then: TZ=UTC dart run --packages=<config> models_json.gen.dart
import 'dart:convert';

import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/fx_presets.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/domain/models/technique_media.dart';

typedef Parse = Map<String, dynamic> Function(Map<String, dynamic>);

final parsers = <String, Parse>{
  'EqBand': (j) => EqBand.fromJson(j).toJson(),
  'CompSettings': (j) => CompSettings.fromJson(j).toJson(),
  'DeEsserSettings': (j) => DeEsserSettings.fromJson(j).toJson(),
  'GateSettings': (j) => GateSettings.fromJson(j).toJson(),
  'DelaySettings': (j) => DelaySettings.fromJson(j).toJson(),
  'SaturationSettings': (j) => SaturationSettings.fromJson(j).toJson(),
  'DoublerSettings': (j) => DoublerSettings.fromJson(j).toJson(),
  'LimiterSettings': (j) => LimiterSettings.fromJson(j).toJson(),
  'ReverbSettings': (j) => ReverbSettings.fromJson(j).toJson(),
  'TrackSend': (j) => TrackSend.fromJson(j).toJson(),
  'TrackFx': (j) => TrackFx.fromJson(j).toJson(),
  'AudioClip': (j) => AudioClip.fromJson(j).toJson(),
  'AudioTrack': (j) => AudioTrack.fromJson(j).toJson(),
  'FxBus': (j) => FxBus.fromJson(j).toJson(),
  'LyricLine': (j) => LyricLine.fromJson(j).toJson(),
  'PracticeNote': (j) => PracticeNote.fromJson(j).toJson(),
  'TechniqueMarker': (j) => TechniqueMarker.fromJson(j).toJson(),
  'TakeVersion': (j) => TakeVersion.fromJson(j).toJson(),
  'TechniqueMedia': (j) => TechniqueMedia.fromJson(j).toJson(),
  'Technique': (j) => Technique.fromJson(j).toJson(),
  'SongPractice': (j) => SongPractice.fromJson(j).toJson(),
};

const t0 = '"2026-01-01T00:00:00.000Z"';
const t1 = '"2026-06-18T12:34:56.789012"';

final cases = <String, List<String>>{
  'EqBand': [
    '{}',
    '{"kind":"lowShelf"}',
    '{"kind":"highShelf"}',
    '{"kind":"highShelf","freq":30000,"q":0.1,"gainDb":-99,"enabled":false}',
    '{"kind":"peaking","freq":5,"q":99.5,"gainDb":99}',
    '{"kind":"bogus","freq":440}',
    '{"kind":5,"freq":440}',
    '{"kind":null,"freq":null,"q":null,"gainDb":null,"enabled":null}',
    '{"freq":"x"}',
    '{"enabled":"yes"}',
    '{"gainDb":-0.0,"q":1,"freq":1e3}',
    '{"gainDb":-0}',
    '{"freq":9223372036854775808}',
    '{"freq":123.456789012345678}',
  ],
  'CompSettings': [
    '{}',
    '{"thresholdDb":-60,"ratio":20,"kneeDb":-1,"attackMs":0,"releaseMs":9999,"makeupDb":24}',
    '{"thresholdDb":-22,"ratio":4,"makeupDb":3}',
    '{"ratio":0.5}',
    '{"ratio":"x"}',
    '{"thresholdDb":0.0}',
    '{"makeupDb":-0.0}',
  ],
  'DeEsserSettings': [
    '{}',
    '{"freq":100,"thresholdDb":10,"rangeDb":-5}',
    '{"freq":20000.5,"thresholdDb":-99,"rangeDb":99}',
    '{"freq":true}',
  ],
  'GateSettings': [
    '{}',
    '{"thresholdDb":-100,"rangeDb":100,"attackMs":0,"releaseMs":1}',
    '{"thresholdDb":5,"rangeDb":-1,"attackMs":60,"releaseMs":600}',
    '{"rangeDb":[]}',
  ],
  'DelaySettings': [
    '{}',
    '{"timeMs":0,"feedback":1.5,"mix":2}',
    '{"timeMs":2000,"feedback":-1,"mix":-0.5}',
    '{"timeMs":330,"feedback":0.25,"mix":0.15}',
  ],
  'SaturationSettings': [
    '{}',
    '{"driveDb":50,"mix":-1}',
    '{"driveDb":-0.0,"mix":0.3}',
  ],
  'DoublerSettings': [
    '{}',
    '{"delayMs":0,"depthMs":30,"rateHz":0,"mix":5}',
    '{"delayMs":60,"depthMs":-1,"rateHz":10,"mix":0.35}',
  ],
  'LimiterSettings': [
    '{}',
    '{"ceilingDb":1,"releaseMs":0}',
    '{"ceilingDb":-20,"releaseMs":600}',
  ],
  'ReverbSettings': [
    '{}',
    '{"roomSize":2,"damping":-1,"wet":-0.0,"width":1.5}',
    '{"roomSize":0.8,"damping":0.3,"wet":0.4,"width":0.9}',
    '{"wet":"loud"}',
  ],
  'TrackSend': [
    '{}',
    '{"busId":"b1","levelDb":24}',
    '{"busId":null,"levelDb":-300}',
    '{"busId":"b1","levelDb":-9}',
    '{"busId":3}',
  ],
  'TrackFx': [
    '{}',
    jsonEncode(TrackFx.flat().toJson()),
    jsonEncode(
      TrackFx.flat()
          .copyWith(
            eqOn: true,
            compOn: true,
            comp: const CompSettings(thresholdDb: -22, ratio: 4, makeupDb: 3),
            sendDb: -9,
            order: FxKind.values.reversed.toList(),
          )
          .toJson(),
    ),
    '{"eq":[{"kind":"lowShelf","freq":120,"q":0.7,"gainDb":4},{"kind":"peaking","freq":500,"q":1,"gainDb":-2},{"kind":"peaking","freq":3000,"q":1,"gainDb":1},{"kind":"highShelf","freq":8000,"q":0.7,"gainDb":3}]}',
    '{"eq":[{"freq":100,"gainDb":3},{"gainDb":3},{"gainDb":3},{"gainDb":3},{"gainDb":3},{"gainDb":3}]}',
    '{"eq":[{"kind":"highShelf"},{"kind":"highShelf"},{"kind":"lowShelf"},{"kind":"peaking","freq":1},{"kind":"peaking"}]}',
    '{"eq":[{"kind":"peaking"},{"kind":"peaking"},{"kind":"peaking"},{"kind":"peaking"},{"kind":"peaking"},{"kind":"peaking"},{"kind":"peaking"}]}',
    '{"eq":[]}',
    '{"eq":"x"}',
    '{"eq":[1]}',
    '{"order":["spaceEcho","comp","eq","deEsser"]}',
    '{"order":["reverb","comp","eq"]}',
    '{"order":["eq","eq",3,null,"comp","limiter","limiter"]}',
    '{"order":"x"}',
    '{"comp":"x","deEsser":[],"gate":3,"reverb":null}',
    '{"comp":{"ratio":"x"}}',
    '{"sendDb":10}',
    '{"sendDb":-500.5,"reverbOn":true,"limiterOn":false}',
    '{"eqOn":1}',
  ],
  'AudioClip': [
    '{"id":"c"}',
    '{"id":"c1","audioFilePath":"/a.wav","durationMs":4000,"startMs":1500,"waveformPeaks":[0.1,0.8,1,0]}',
    '{"audioFilePath":"/a.wav"}',
    '{"id":"c","waveformPeaks":["x"]}',
    '{"id":"c","durationMs":1.5}',
    '{"id":"c","waveformPeaks":null,"startMs":null}',
  ],
  'AudioTrack': [
    '{"id":"t","kind":"vocal","displayName":"Take 1","createdAt":$t0}',
    '{"id":"t","kind":"vocal","displayName":"Take 1","createdAt":$t1,"audioFilePath":"/x.wav","durationMs":1000,"startMs":5,"waveformPeaks":[1,0.5]}',
    '{"id":"t","kind":"accompaniment","displayName":"B","createdAt":$t0,"audioFilePath":""}',
    '{"id":"t","kind":"reference","displayName":"R","createdAt":$t0,"clips":[],"audioFilePath":"/ignored.wav"}',
    '{"id":"t","kind":"vocal","displayName":"T","createdAt":$t0,"clips":[{"id":"c1","audioFilePath":"/c1.wav","durationMs":2000},{"id":"c2","audioFilePath":"/c2.wav","durationMs":3000,"startMs":5000}],"notes":[{"id":"n1","positionMs":10,"content":"breath","createdAt":$t0,"updatedAt":$t1,"endMs":20}],"techniqueMarkers":[{"id":"m1","positionMs":5,"createdAt":$t0,"updatedAt":$t0}],"colorValue":4294967295,"muted":true,"soloed":true,"volume":2,"pan":-3,"versionId":"v1","fx":{},"busId":"b1","sends":[{"busId":"b2","levelDb":-6}]}',
    '{"id":"t","kind":"vocal","displayName":"T","createdAt":$t0,"fx":null,"volume":-0.0,"pan":0.25}',
    '{"id":"t","kind":"choir","displayName":"T","createdAt":$t0}',
    '{"id":"t","kind":"vocal","displayName":"T","createdAt":"yesterday"}',
    '{"id":"t","kind":"vocal","displayName":"T","createdAt":$t0,"fx":"x"}',
    '{"id":"t","kind":"vocal","displayName":"T","createdAt":$t0,"clips":"x"}',
    '{"id":7,"kind":"vocal","displayName":"T","createdAt":$t0,"audioFilePath":"/x.wav"}',
    '{"kind":"vocal","displayName":"T","createdAt":$t0}',
  ],
  'FxBus': [
    '{"id":"b"}',
    '{"id":"b","name":null,"fx":null}',
    '{"id":"b1","name":"Verb","fx":{"eqOn":true,"sendDb":-9},"colorValue":4279312947,"volume":1.5,"muted":true,"soloed":true,"pan":-2,"outputBusId":"b2","sends":[{"busId":"b3","levelDb":-12}]}',
    '{"name":"x"}',
  ],
  'LyricLine': [
    '{}',
    '{"text":"la","timeMs":100}',
    '{"text":"la","timeMs":null}',
    '{"text":null}',
    '{"text":"la","timeMs":1.5}',
    '{"text":3}',
  ],
  'PracticeNote': [
    '{"id":"n","positionMs":1,"content":"c","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"n","positionMs":1,"content":"c","createdAt":$t0,"updatedAt":$t1,"endMs":3000}',
    '{"id":"n","content":"c","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"n","positionMs":1,"createdAt":$t0,"updatedAt":$t1}',
    '{"id":"n","positionMs":1,"content":"c","createdAt":"2026-01-01T00:00:00+01:00","updatedAt":"20260101T0000Z"}',
  ],
  'TechniqueMarker': [
    '{"id":"m","positionMs":1,"createdAt":$t0,"updatedAt":$t1}',
    '{"id":"m","positionMs":1,"endMs":4000,"techniqueId":"t","techniqueName":"Twang","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"m","positionMs":"1","createdAt":$t0,"updatedAt":$t1}',
  ],
  'TakeVersion': [
    '{"id":"v","name":"Tender","createdAt":$t0}',
    '{"id":"v","createdAt":$t0}',
    '{"id":"v","name":"x","createdAt":"2026-02-30T25:61:61.9999999"}',
  ],
  'TechniqueMedia': [
    '{"id":"m","path":"/a.m4a","addedAt":$t0}',
    '{"id":"m","path":"/a.mp4","kind":"video","label":"Example","addedAt":$t1}',
    '{"id":"m","path":"/a.mp4","kind":"VIDEO","addedAt":$t0}',
    '{"id":"m","path":"/a.mp4","kind":2,"addedAt":$t0}',
    '{"id":"m","addedAt":$t0}',
  ],
  'Technique': [
    '{"id":"t","name":"Twang","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"t","name":"Twang","aliases":["Edge"],"tags":["Quality","Register"],"summary":"s","description":"d","cues":"c","exercises":"e","acousticSignature":"a","media":[{"id":"m","path":"/a.m4a","addedAt":$t0}],"createdAt":$t0,"updatedAt":$t1}',
    '{"id":"t","name":"Twang","aliases":[1],"createdAt":$t0,"updatedAt":$t1}',
    '{"id":"t","name":"Twang","summary":null,"createdAt":$t0,"updatedAt":$t1}',
  ],
  'SongPractice': [
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"p","title":"Song","createdAt":$t1,"updatedAt":$t1,"vocalTracks":[{"id":"a","kind":"vocal","displayName":"Take 1","createdAt":$t0},{"id":"b","kind":"vocal","displayName":"Take 2","createdAt":$t0,"versionId":"gone"}]}',
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1,"versions":[{"id":"v1","name":"A","createdAt":$t0},{"id":"v2","name":"B","createdAt":$t0}],"activeVersionId":"stale","vocalTracks":[{"id":"a","kind":"vocal","displayName":"T","createdAt":$t0,"versionId":"v2"},{"id":"b","kind":"vocal","displayName":"T","createdAt":$t0}]}',
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1,"versions":[{"id":"v1","name":"A","createdAt":$t0}],"activeVersionId":null}',
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1,"accompaniment":{"id":"acc","kind":"accompaniment","displayName":"B","createdAt":$t0,"audioFilePath":"/b.wav","durationMs":9000},"reference":null,"lyrics":[{"text":"la","timeMs":100},{"text":""}],"timelineDurationMs":60000,"separationSource":"/orig.mp3","reverb":{"wet":0.3},"masterVolume":1.5,"fxBuses":[{"id":"x","name":"Verb"}]}',
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1,"masterVolume":-1,"reverb":{}}',
    '{"id":"p","title":"Song","createdAt":$t0}',
    '{"id":"p","createdAt":$t0,"updatedAt":$t1}',
    '{"id":"p","title":"Song","createdAt":$t0,"updatedAt":$t1,"reverb":"x"}',
  ],
};

const dates = [
  '2026-01-01T00:00:00.000Z',
  '2026-06-18T12:34:56.789Z',
  '2026-06-18T12:34:56.789012Z',
  '2026-06-18T12:34:56.7890129Z',
  '2026-06-18T12:34:56,5z',
  '2026-06-18 12:34Z',
  '2026-06-18T12Z',
  '20260618T123456Z',
  '20260618',
  '2026-06-18T12:34:56+05:30',
  '2026-06-18T12:34:56 -0130',
  '2026-06-18T12:34:56-01',
  '+002026-06-18T00:00:00Z',
  '-002026-06-18T00:00:00Z',
  '+10000-01-01T00:00:00.000Z',
  '0000-01-01T00:00:00Z',
  '1969-12-31T23:59:59.999999Z',
  '1600-02-29T12:00:00Z',
  '2026-13-45T25:61:61Z',
  '2026-00-00T00:00:00Z',
  '+275760-09-13T00:00:00Z',
  '+275760-09-13T00:00:00.001Z',
  '-271821-04-20T00:00:00Z',
  '-271821-04-19T23:59:59.999Z',
  '2026-06-18T12:00:00',
  '2026-06-18T12:00:00.5',
  '2026-06-18',
  '2026-6-18',
  '2026-06-18T',
  '2026-06-18Z',
  '2026-06-18T12:3',
  '2026-06-18T12:34:56.',
  '2026-06-18t12:00:00Z',
  ' 2026-06-18T12:00:00Z',
  '2026-06-18T12:00:00Z ',
  '２０２６-06-18',
  '',
];

void main() {
  for (final MapEntry(key: kind, value: inputs) in cases.entries) {
    final parse = parsers[kind]!;
    for (final input in inputs) {
      String out;
      try {
        out = jsonEncode(parse(jsonDecode(input) as Map<String, dynamic>));
      } catch (_) {
        out = 'THROWS';
      }
      print('$kind\t$input\t$out');
    }
  }
  for (final d in dates) {
    String out;
    try {
      final v = DateTime.parse(d);
      out = v.toIso8601String();
      if (v.isUtc) out += '|true|${v.microsecondsSinceEpoch}';
    } catch (_) {
      out = 'THROWS';
    }
    print('datetime\t${jsonEncode(d)}\t$out');
  }
  for (final p in FxPresets.vocal) {
    print('preset\t${jsonEncode(p.name)}\t${jsonEncode(p.fx.toJson())}');
  }
  for (final p in FxPresets.reverb) {
    print('reverbPreset\t${jsonEncode(p.name)}\t${jsonEncode(p.reverb.toJson())}');
  }
  print('flat\t""\t${jsonEncode(TrackFx.flat().toJson())}');
}
