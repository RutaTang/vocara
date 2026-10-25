import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/utils/id.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

/// A structurally-complete field map for a practices row.
Map<String, Object?> _practiceFields(String id, Hlc h, {String title = 't'}) =>
    {
      'id': _id(id),
      'title': title,
      'timeline_duration_ms': 0,
      'master_volume': 0.5,
      'active_interpretation_id': null,
      'separation_asset_id': null,
      'reverb_json': null,
      'lyrics_json': null,
      'created_at': h.wallMs,
      'updated_at': h.wallMs,
      'deleted_at': null,
      'hlc_wall': h.wallMs,
      'hlc_ctr': h.counter,
      'hlc_node': h.node,
      'hlc_packed': h.packed,
      'schema_v': 1,
    };

void main() {
  final hlc = Hlc(wallMs: 1234, counter: 7, node: _id('NODE'));

  group('wire format round-trip', () {
    test('encode/decode preserves every field, including nulls and numeric '
        'runtime types (int vs double, booleans as ints)', () {
      final row = SyncRow(
        table: 'tracks',
        fields: {
          'id': _id('T1'),
          'practice_id': _id('P1'),
          'kind': 'vocal',
          'interpretation_id': null, // null field survives
          'display_name': 'Take 1',
          'color_value': null,
          'muted': 0, // boolean-as-int per DB convention
          'soloed': 1,
          'volume': 1.0, // REAL: must stay a double
          'pan': 0.0,
          'bus_id': null,
          'fx_json': null,
          'sends_json': null,
          'order_index': 3, // INTEGER: must stay an int
          'created_at': 1234,
          'updated_at': 1234,
          'deleted_at': null,
          'hlc_wall': hlc.wallMs,
          'hlc_ctr': hlc.counter,
          'hlc_node': hlc.node,
          'hlc_packed': hlc.packed,
          'schema_v': 1,
        },
      );

      final decoded = SyncRow.decode(row.encode());

      expect(decoded.table, 'tracks');
      expect(decoded.id, _id('T1'));
      expect(decoded.schemaV, 1);
      expect(decoded.hlcPacked, hlc.packed);
      expect(decoded.hlc, hlc);
      expect(decoded.fields, equals(row.fields));
      // Runtime types survive JSON: SQLite binds ints and doubles differently.
      expect(decoded.fields['volume'], isA<double>());
      expect(decoded.fields['pan'], isA<double>());
      expect(decoded.fields['order_index'], isA<int>());
      expect(decoded.fields['muted'], isA<int>());
      expect(decoded.fields['interpretation_id'], isNull);
      expect(decoded.fieldClocks, isNull); // reserved slot stays null in v1
    });

    test('fieldClocks from a future column-LWW peer survive a round-trip '
        'through a v1 device (reserved, not stripped)', () {
      final row = SyncRow(
        table: 'practices',
        fields: _practiceFields('P1', hlc),
        fieldClocks: {'title': hlc.packed},
      );
      final decoded = SyncRow.decode(row.encode());
      expect(decoded.fieldClocks, {'title': hlc.packed});
    });

    test('fields are frozen (a SyncRow is a value in transit)', () {
      final row = SyncRow(
        table: 'practices',
        fields: _practiceFields('P', hlc),
      );
      expect(() => row.fields['title'] = 'mutated', throwsUnsupportedError);
    });

    test('decode rejects an envelope that disagrees with the field map', () {
      final json = SyncRow(
        table: 'practices',
        fields: _practiceFields('P1', hlc),
      ).toJson();
      final tampered = {...json, 'id': _id('OTHER')};
      expect(() => SyncRow.fromJson(tampered), throwsFormatException);
    });

    test('decode rejects structured (non-scalar) field values', () {
      final json = SyncRow(
        table: 'practices',
        fields: _practiceFields('P1', hlc),
      ).toJson();
      final nested = {
        ...json,
        'fields': {
          ...(json['fields'] as Map<String, Object?>),
          'title': {'not': 'a scalar'},
        },
      };
      expect(() => SyncRow.fromJson(nested), throwsFormatException);
    });

    test('decode rejects a number JSON can only spell as an overflow', () {
      // jsonDecode turns 1e999 into Infinity: a "scalar" no column can hold
      // and no peer can re-encode.
      final json = SyncRow(
        table: 'practices',
        fields: _practiceFields('P1', hlc),
      ).encode();
      final overflowed = json.replaceFirst(
        '"master_volume":0.5',
        '"master_volume":1e999',
      );
      expect(overflowed, isNot(json));
      expect(() => SyncRow.decode(overflowed), throwsFormatException);
    });

    test('decode rejects a payload missing the identity trio in fields', () {
      final fields = _practiceFields('P1', hlc)..remove('hlc_packed');
      final json = {
        'table': 'practices',
        'id': fields['id'],
        'schemaV': 1,
        'hlcPacked': hlc.packed,
        'fields': fields,
        'fieldClocks': null,
      };
      expect(() => SyncRow.fromJson(json), throwsFormatException);
    });

    test('decode rejects non-JSON and non-object payloads', () {
      expect(() => SyncRow.decode('not json'), throwsFormatException);
      expect(() => SyncRow.decode('[1,2]'), throwsFormatException);
    });

    test('decode rejects a mistyped or missing HLC quad (the applier must '
        'never hit a TypeError deep inside an apply)', () {
      Map<String, Object?> jsonWith(Map<String, Object?> patch) {
        final fields = {..._practiceFields('P1', hlc), ...patch}
          ..removeWhere((_, v) => v == #absent);
        return {
          'table': 'practices',
          'id': fields['id'],
          'schemaV': fields['schema_v'],
          'hlcPacked': fields['hlc_packed'],
          'fields': fields,
          'fieldClocks': null,
        };
      }

      // Well-formed control: the helper itself must decode.
      expect(SyncRow.fromJson(jsonWith(const {})).hlc, hlc);
      expect(
        () => SyncRow.fromJson(
          jsonWith({'hlc_wall': '${hlc.wallMs}'}), // string, not int
        ),
        throwsFormatException,
      );
      expect(
        () => SyncRow.fromJson(jsonWith(const {'hlc_ctr': 'seven'})),
        throwsFormatException,
      );
      expect(
        () => SyncRow.fromJson(jsonWith(const {'hlc_node': ''})),
        throwsFormatException,
      );
      expect(
        () => SyncRow.fromJson(jsonWith(const {'hlc_wall': #absent})),
        throwsFormatException,
      );
    });

    test('decode rejects an hlc_packed that disagrees with the quad (a forged '
        'LWW key must not pass the drift gate on an honest-looking quad)', () {
      final forged = Hlc(
        wallMs: hlc.wallMs + 999999999, // far future: would win every LWW
        counter: 0,
        node: hlc.node,
      ).packed;
      final fields = {..._practiceFields('P1', hlc), 'hlc_packed': forged};
      final json = {
        'table': 'practices',
        'id': fields['id'],
        'schemaV': 1,
        'hlcPacked': forged, // envelope agrees with fields — only the quad
        'fields': fields, //    (what the drift gate judges) tells the truth
        'fieldClocks': null,
      };
      expect(() => SyncRow.fromJson(json), throwsFormatException);
    });
  });

  group('synced table registry', () {
    late AppDatabase db;
    setUp(() => db = AppDatabase(NativeDatabase.memory()));
    tearDown(() => db.close());

    test('contains exactly the SyncCols tables and none of the local-only '
        'bookkeeping (oplog / sync_state / sync_quarantine / asset_sync / '
        'recordings_inflight / waveform_cache)', () {
      final registry = syncedTableRegistry(db);
      expect(registry.keys.toSet(), {
        'practices',
        'interpretations',
        'tracks',
        'clips',
        'practice_notes',
        'technique_markers',
        'fx_buses',
        'techniques',
        'technique_media',
        'assets',
      });
    });

    test('every *_json column of a synced table has a registered decoder', () {
      // Derive the JSON columns STRUCTURALLY from the live schema. A new
      // `*_json` column that ships without an entry in jsonColumnDecoders is
      // refused wholesale at the door (every record of that table would
      // quarantine), so this must fail here rather than in the field.
      final fromSchema = <String>{
        for (final table in syncedTableRegistry(db).values)
          for (final column in table.$columns)
            if (column.name.endsWith('_json')) column.name,
      };
      expect(fromSchema, isNotEmpty); // the derivation itself must keep working
      expect(
        fromSchema.difference(jsonColumnDecoders.keys.toSet()),
        isEmpty,
        reason:
            'jsonColumnDecoders must say what each *_json column holds: the '
            'gate runs the real decoder so that what commits can hydrate.',
      );
    });
  });

  group('SyncRowBuilder', () {
    late AppDatabase db;
    late SyncRowBuilder builder;

    setUp(() {
      db = AppDatabase(NativeDatabase.memory());
      builder = SyncRowBuilder(db);
    });
    tearDown(() => db.close());

    Future<void> seed() async {
      await db
          .into(db.practices)
          .insert(
            PracticesCompanion.insert(
              id: _id('P1'),
              title: const Value('Song'),
              masterVolume: const Value(0.75),
              createdAt: 1,
              updatedAt: 2,
              hlcWall: hlc.wallMs,
              hlcCtr: hlc.counter,
              hlcNode: hlc.node,
              hlcPacked: hlc.packed,
            ),
          );
      await db
          .into(db.tracks)
          .insert(
            TracksCompanion.insert(
              id: _id('T1'),
              practiceId: _id('P1'),
              kind: 'vocal',
              muted: const Value(true),
              createdAt: 1,
              updatedAt: 2,
              hlcWall: hlc.wallMs,
              hlcCtr: hlc.counter,
              hlcNode: hlc.node,
              hlcPacked: hlc.packed,
            ),
          );
    }

    test('produces ALL of a table\'s columns in DB value conventions', () async {
      await seed();
      final row = await builder.build('tracks', _id('T1'));
      expect(row, isNotNull);
      expect(row!.table, 'tracks');
      // Every column of the drift schema is present — nothing is projected out.
      final expectedColumns = db.tracks.$columns.map((c) => c.name).toSet();
      expect(row.fields.keys.toSet(), expectedColumns);
      expect(row.fields['muted'], 1); // boolean rides as int
      expect(row.fields['volume'], isA<double>());
      expect(row.fields['deleted_at'], isNull);
      expect(row.id, _id('T1'));
      expect(row.hlcPacked, hlc.packed);
      expect(row.schemaV, 1);
      expect(row.fieldClocks, isNull); // v1 never sets the reserved slot
    });

    test('strips the PER-DEVICE asset columns from the wire projection '
        '(local_rel_path and the sync-state bookkeeping stay home)', () async {
      await db
          .into(db.assets)
          .insert(
            AssetsCompanion.insert(
              id: _id('A1'),
              contentHash: 'HASH_A1',
              byteSize: 8,
              ext: const Value('wav'),
              localRelPath: const Value('assets/HA/SH/HASH_A1.wav'),
              recordSyncState: const Value('synced'),
              bytesSyncState: const Value('present'),
              remoteRef: const Value('blob://HASH_A1'),
              createdAt: 1,
              updatedAt: 2,
              hlcWall: hlc.wallMs,
              hlcCtr: hlc.counter,
              hlcNode: hlc.node,
              hlcPacked: hlc.packed,
            ),
          );
      final row = await builder.build('assets', _id('A1'));
      expect(row, isNotNull);
      // Exactly the drift schema minus the per-device set — tables.dart
      // documents these as "stripped from the wire projection".
      final expected = db.assets.$columns.map((c) => c.name).toSet()
        ..removeAll(perDeviceColumns['assets']!);
      expect(row!.fields.keys.toSet(), expected);
      expect(row.fields['content_hash'], 'HASH_A1');
      expect(row.fields.containsKey('local_rel_path'), isFalse);
      expect(row.fields.containsKey('record_sync_state'), isFalse);
      expect(row.fields.containsKey('bytes_sync_state'), isFalse);
      expect(row.fields.containsKey('remote_ref'), isFalse);
    });

    test('returns null for a missing row id', () async {
      await seed();
      expect(await builder.build('practices', _id('NOPE')), isNull);
    });

    test('refuses non-synced tables (local-only or unknown)', () async {
      expect(() => builder.build('oplog', 'x'), throwsArgumentError);
      expect(() => builder.build('sync_state', 'x'), throwsArgumentError);
      expect(() => builder.build('sync_quarantine', 'x'), throwsArgumentError);
      expect(() => builder.build('no_such_table', 'x'), throwsArgumentError);
    });

    test('builder -> wire -> applier: a built row applies verbatim on another '
        'device', () async {
      await seed();
      final built = await builder.build('practices', _id('P1'));
      final wire = built!.encode();

      final other = AppDatabase(NativeDatabase.memory());
      addTearDown(other.close);
      final applier = SyncApplier(
        other,
        HlcClock(node: _id('OTHER'), now: () => hlc.wallMs),
        builder: SyncRowBuilder(other),
      );
      final res = await applier.apply([SyncRow.decode(wire)]);
      expect(res.applied, 1);
      expect(res.failed, 0);

      final landed = await (other.select(
        other.practices,
      )..where((p) => p.id.equals(_id('P1')))).getSingle();
      expect(landed.title, 'Song');
      expect(landed.masterVolume, 0.75);
      expect(landed.hlcPacked, hlc.packed);
    });
  });

  group('path-component id gate', () {
    Map<String, Object?> practiceFields(String id) => {
      'id': id,
      'schema_v': 1,
      'hlc_wall': 1000,
      'hlc_ctr': 0,
      'hlc_node': 'N'.padRight(26, '0'),
      'hlc_packed': '0000000001000-000000-${'N'.padRight(26, '0')}',
    };

    test('a practice id the media services could not use is refused at the '
        'wire, not left to fail locally', () {
      // practices.id and techniques.id become audio/<id>/ and
      // technique_media/<id>/, built and recursively deleted by the media
      // services under a strict shape. The wire's superset otherwise admits a
      // row that syncs cleanly and is then un-operable: every media call for
      // it throws, with nothing to fix it short of editing the database.
      expect(
        () => SyncRow.checkIdShapes('practices', practiceFields('p1')),
        throwsFormatException,
      );
      expect(
        () => SyncRow.checkIdShapes('techniques', practiceFields('short')),
        throwsFormatException,
      );
      // A real id passes.
      expect(
        () => SyncRow.checkIdShapes('practices', practiceFields(newId())),
        returnsNormally,
      );
    });

    test('the wire rule and the media services agree exactly', () {
      // Two rules that drift apart reintroduce the same gap from the other
      // side, so they read the ONE predicate.
      for (final id in [newId(), 'A'.padRight(26, '0'), '0' * 26]) {
        expect(isPathComponentId(id), isTrue, reason: id);
        expect(
          () => SyncRow.checkIdShapes('practices', practiceFields(id)),
          returnsNormally,
          reason: id,
        );
      }
      for (final id in ['p1', 'a' * 26, '${'A' * 25}/', '${'A' * 25}.']) {
        expect(isPathComponentId(id), isFalse, reason: id);
      }
    });

    test('every id is 26–36 characters, the length drift enforces on each '
        'local insert; the shapes minted and seeded today all fit', () {
      Map<String, Object?> clipFields(String id, {String? trackId}) => {
        ...practiceFields(id),
        'track_id': trackId ?? newId(),
      };
      for (final id in ['SHORT', 'A' * 25, 'A' * 37]) {
        expect(
          () => SyncRow.checkIdShapes('clips', clipFields(id)),
          throwsFormatException,
          reason: id,
        );
        expect(
          () =>
              SyncRow.checkIdShapes('clips', clipFields(newId(), trackId: id)),
          throwsFormatException,
          reason: 'a reference is an id too: $id',
        );
      }
      for (final id in [
        newId(),
        '${newId()}-c0', // a legacy take's derived clip
        '${newId()}__v1', // a legacy practice's default interpretation
        'E2EPRACTICESEED00000000000', // the e2e harness's seeded rows
        'A' * 36,
      ]) {
        expect(
          () => SyncRow.checkIdShapes('clips', clipFields(id)),
          returnsNormally,
          reason: id,
        );
      }
    });

    test('a CHILD table keeps the superset (the legacy derived clip shape '
        'never names a directory)', () {
      final trackId = newId();
      expect(
        () => SyncRow.checkIdShapes('clips', {
          ...practiceFields('$trackId-c0'),
          'track_id': trackId,
        }),
        returnsNormally,
      );
    });
  });
}
