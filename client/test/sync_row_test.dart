import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_row.dart';

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
}
