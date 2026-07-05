import 'dart:convert';

import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';

/// One synced row in transit — the single wire shape shared by the transport
/// (push/pull) and the [SyncApplier]. [fields] carries EVERY column of the row
/// under its SQL name, in the database's value conventions (booleans as ints,
/// REAL as double, NULL as null); the envelope duplicates the identity trio
/// (id / schema_v / hlc_packed) at the top level for cheap routing without
/// parsing the field map — encode/decode verifies the two stay consistent.
class SyncRow {
  /// [fields] is defensively copied and frozen: a SyncRow is a value in
  /// transit, and silent mutation after LWW comparison would be a data bug.
  SyncRow({
    required this.table,
    required Map<String, Object?> fields,
    this.fieldClocks,
  }) : fields = Map.unmodifiable(fields);

  /// The synced table this row belongs to (SQL name, e.g. 'practices').
  final String table;

  /// Every column of the row keyed by SQL column name (SyncCols included).
  final Map<String, Object?> fields;

  /// RESERVED: the column-level-LWW upgrade slot (column name -> packed HLC of
  /// that column's last writer). ALWAYS null in wire format v1 — the builder
  /// never sets it and entity-level LWW ignores it — but the slot is carried
  /// through encode/decode untouched so a future column-LWW peer's clocks
  /// survive a round-trip through a v1 device instead of being stripped.
  final Map<String, String>? fieldClocks;

  String get id => fields['id'] as String;
  int get schemaV => fields['schema_v'] as int;
  String get hlcPacked => fields['hlc_packed'] as String;
  Hlc get hlc => Hlc(
    wallMs: fields['hlc_wall'] as int,
    counter: fields['hlc_ctr'] as int,
    node: fields['hlc_node'] as String,
  );

  Map<String, Object?> toJson() => {
    'table': table,
    'id': id,
    'schemaV': schemaV,
    'hlcPacked': hlcPacked,
    'fields': fields,
    // Emitted even when null so a v1 payload is explicit about the slot.
    'fieldClocks': fieldClocks,
  };

  String encode() => jsonEncode(toJson());

  /// Strict decode: throws [FormatException] on a structurally-invalid payload
  /// (missing/mistyped identity, nested values, an envelope that disagrees
  /// with the field map). Strictness is deliberate — a malformed record must
  /// surface as a per-record failure (and quarantine) at the door, not corrupt
  /// a row deep inside an apply.
  factory SyncRow.fromJson(Map<String, Object?> json) {
    final table = json['table'];
    if (table is! String || table.isEmpty) {
      throw const FormatException('SyncRow: missing/invalid "table"');
    }
    final rawFields = json['fields'];
    if (rawFields is! Map) {
      throw const FormatException('SyncRow: missing/invalid "fields"');
    }
    final fields = <String, Object?>{};
    for (final entry in rawFields.entries) {
      final key = entry.key;
      if (key is! String) {
        throw const FormatException('SyncRow: non-string field name');
      }
      final value = entry.value;
      // Column values are SQL scalars only. Booleans ride as ints by DB
      // convention but a JSON true/false is tolerated (the applier binds
      // bools to ints); structured values are always a malformed payload.
      if (value != null &&
          value is! num &&
          value is! String &&
          value is! bool) {
        throw FormatException('SyncRow: field "$key" is not a scalar');
      }
      fields[key] = value;
    }
    if (fields['id'] is! String ||
        fields['schema_v'] is! int ||
        fields['hlc_packed'] is! String) {
      throw const FormatException(
        'SyncRow: fields must carry id/schema_v/hlc_packed',
      );
    }
    // The envelope trio must agree with the field map — a mismatch means a
    // corrupt or tampered payload and is refused rather than guessed at.
    if (json['id'] != fields['id'] ||
        json['schemaV'] != fields['schema_v'] ||
        json['hlcPacked'] != fields['hlc_packed']) {
      throw const FormatException('SyncRow: envelope/fields mismatch');
    }
    final rawClocks = json['fieldClocks'];
    Map<String, String>? fieldClocks;
    if (rawClocks != null) {
      if (rawClocks is! Map) {
        throw const FormatException('SyncRow: invalid "fieldClocks"');
      }
      fieldClocks = <String, String>{};
      for (final entry in rawClocks.entries) {
        final key = entry.key;
        final value = entry.value;
        if (key is! String || value is! String) {
          throw const FormatException('SyncRow: invalid fieldClocks entry');
        }
        fieldClocks[key] = value;
      }
    }
    return SyncRow(table: table, fields: fields, fieldClocks: fieldClocks);
  }

  factory SyncRow.decode(String json) {
    final Object? obj = jsonDecode(json); // throws FormatException on bad JSON
    if (obj is! Map<String, Object?>) {
      throw const FormatException('SyncRow: payload is not a JSON object');
    }
    return SyncRow.fromJson(obj);
  }

  @override
  String toString() =>
      'SyncRow($table/${fields['id']} @${fields['hlc_packed']})';
}

/// The full SyncCols column set — the structural marker of a synced table.
/// Requiring ALL of them (not just hlc_packed) keeps the registry from ever
/// picking up a local-only table: the oplog carries hlc_packed but no
/// id/deleted_at/schema_v, and sync_quarantine carries id/schema_v but its
/// stamp column is remote_hlc_packed by design (see tables.dart).
const Set<String> _syncColNames = {
  'id',
  'created_at',
  'updated_at',
  'deleted_at',
  'hlc_wall',
  'hlc_ctr',
  'hlc_node',
  'hlc_packed',
  'schema_v',
};

/// Table name -> drift [TableInfo] for exactly the SYNCED tables. Derived
/// structurally from the SyncCols mixin's columns rather than a hand-kept list
/// so a new synced table can never be silently left out (same philosophy as
/// Persistence._seedClock's hlc_packed scan, tightened to the full marker set
/// so local-only bookkeeping tables are excluded).
Map<String, TableInfo<Table, dynamic>> syncedTableRegistry(AppDatabase db) => {
  for (final t in db.allTables)
    if (t.$columns.map((c) => c.name).toSet().containsAll(_syncColNames))
      t.actualTableName: t,
};

/// Reads current rows generically (via the drift [TableInfo] registry) and
/// projects them into the wire format. Every column of a synced table is
/// synced by design, so the projection is simply "all columns".
class SyncRowBuilder {
  SyncRowBuilder(this._db) : _tables = syncedTableRegistry(_db);

  final AppDatabase _db;
  final Map<String, TableInfo<Table, dynamic>> _tables;

  /// The synced tables this builder (and the wire format) recognizes.
  Set<String> get tableNames => _tables.keys.toSet();

  /// Builds the SyncRow for the CURRENT state of ([table], [id]), or null if
  /// no such row exists (e.g. it was hard-deleted as a never-synced draft
  /// before the drain caught up). Throws [ArgumentError] for a non-synced
  /// table name — pushing local-only tables is always a programming error.
  Future<SyncRow?> build(String table, String id) async {
    final info = _tables[table];
    if (info == null) {
      throw ArgumentError.value(table, 'table', 'not a synced table');
    }
    // Select the TableInfo's own column list explicitly (not *) so the
    // projection is exactly the drift schema even if the raw file carries
    // stray columns from some future/debug state.
    final cols = info.$columns.map((c) => '"${c.name}"').join(', ');
    final row = await _db
        .customSelect(
          'SELECT $cols FROM "${info.actualTableName}" WHERE id = ?',
          variables: [Variable.withString(id)],
        )
        .getSingleOrNull();
    if (row == null) return null;
    final fields = <String, Object?>{};
    for (final c in info.$columns) {
      final value = row.data[c.name];
      if (value is List<int>) {
        // No synced table carries a BLOB today; refuse loudly rather than let
        // jsonEncode fail (or silently mangle bytes) if one ever appears.
        throw StateError(
          'SyncRowBuilder: BLOB column ${info.actualTableName}.${c.name} is '
          'not representable in the v1 wire format',
        );
      }
      fields[c.name] = value;
    }
    return SyncRow(table: info.actualTableName, fields: fields);
  }
}
