import 'dart:convert';

import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';

/// One synced row in transit — the single wire shape shared by the transport
/// (push/pull) and the [SyncApplier]. [fields] carries every SYNCED column of
/// the row under its SQL name (all columns minus the few [perDeviceColumns]),
/// in the database's value conventions (booleans as ints, REAL as double,
/// NULL as null); the envelope duplicates the identity trio
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
    // The two HLC representations must agree — see [checkHlcConsistency].
    // Checking here (the wire door) also removes the last unchecked casts
    // behind the [hlc] getter: a mistyped quad fails HERE as a per-record
    // parse error instead of a TypeError deep inside an apply.
    checkHlcConsistency(fields);
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

  /// Verifies that [fields] carries a well-typed HLC quad
  /// (hlc_wall / hlc_ctr / hlc_node) plus a String hlc_packed the quad
  /// re-packs to EXACTLY (via the same [Hlc.packed] routine every stamp in
  /// the codebase uses); throws [FormatException] otherwise.
  ///
  /// This is the SINGLE consistency rule for a row's two HLC
  /// representations: LWW compares the PACKED string while the drift gate
  /// (and [HlcClock.receive]) judge the QUAD's wall — if the two could
  /// disagree, a record with an honest-looking quad but a forged far-future
  /// hlc_packed would sail through the drift gate and then win every LWW
  /// comparison fleet-wide. Shared by [fromJson] (the pull-side wire door)
  /// and the push engine's conflicted-ack validation (a serverRow driving
  /// the CAS retry-or-drop decision must satisfy the same rule) —
  /// deliberately ONE routine so the two doors can never diverge.
  static void checkHlcConsistency(Map<String, Object?> fields) {
    final hlcPacked = fields['hlc_packed'];
    if (hlcPacked is! String) {
      throw const FormatException(
        'SyncRow: fields must carry a String hlc_packed',
      );
    }
    final hlcWall = fields['hlc_wall'];
    final hlcCtr = fields['hlc_ctr'];
    final hlcNode = fields['hlc_node'];
    if (hlcWall is! int ||
        hlcCtr is! int ||
        hlcNode is! String ||
        hlcNode.isEmpty) {
      throw const FormatException(
        'SyncRow: fields must carry an int hlc_wall/hlc_ctr and a '
        'non-empty hlc_node',
      );
    }
    final repacked = Hlc(
      wallMs: hlcWall,
      counter: hlcCtr,
      node: hlcNode,
    ).packed;
    if (repacked != hlcPacked) {
      throw const FormatException(
        'SyncRow: hlc_packed disagrees with the hlc_wall/hlc_ctr/hlc_node quad',
      );
    }
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

/// The record schema version this build writes (and fully understands).
/// Every synced table's schema_v column defaults to it (tables.dart), and the
/// [SyncApplier] quarantines a record claiming a NEWER version: such a
/// record's shape is by definition not fully understood here, and the
/// quarantine replay lets it in after the app upgrade that is. Bump ONLY
/// together with the tables.dart default when the wire record shape changes.
const int currentRecordSchemaV = 1;

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

/// PER-DEVICE columns, by table: local bookkeeping that must never cross the
/// wire (the Assets doc in tables.dart — "stripped from the wire
/// projection"). [SyncRowBuilder] omits them from the outbound projection and
/// [SyncApplier] drops them from inbound records, so one device's bookkeeping
/// can never clobber another's (e.g. A's `<hash>.wav` local_rel_path
/// overwriting the path of B's already-materialized `<hash>.mp3`, making B's
/// resolve() miss bytes that exist). An absent column is the applier's normal
/// missing-column case: an UPDATE leaves the local value untouched and an
/// INSERT takes the schema default.
const Map<String, Set<String>> perDeviceColumns = {
  'assets': {
    'local_rel_path',
    'record_sync_state',
    'bytes_sync_state',
    'remote_ref',
  },
};

/// Per-device column VALUES the [SyncApplier] force-sets on an INBOUND row
/// (insert and update alike), after stripping [perDeviceColumns]. Kept next to
/// that map — the two are the single structural description of how per-device
/// bookkeeping behaves at the remote door — so a new override can never be
/// string-scattered through the applier.
///
/// Why `assets.record_sync_state = 'synced'`: a row that arrived over the
/// wire has BY DEFINITION crossed it, and another device may already reference
/// the asset. Left at the drift default 'localOnly' (which is exactly what an
/// insert of a stripped row lands with), the asset-GC's never-synced guard
/// (AssetStore.sweepUnreferenced) would treat the very rows it must protect as
/// sweepable, tombstone them off local knowledge alone, and replicate the
/// delete fleet-wide. `bytes_sync_state` is deliberately NOT overridden: it
/// tracks this device's blob upload/download state, which has no transport yet
/// — and the GC predicate requires BOTH columns to be 'localOnly', so marking
/// the record side alone already protects every wire-arrived row.
const Map<String, Map<String, Object?>> inboundPerDeviceOverrides = {
  'assets': {'record_sync_state': 'synced'},
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
/// synced by design, so the projection is "all columns" minus the few
/// [perDeviceColumns] (local bookkeeping that must not leave this device).
class SyncRowBuilder {
  SyncRowBuilder(this._db) : _tables = syncedTableRegistry(_db);

  final AppDatabase _db;
  final Map<String, TableInfo<Table, dynamic>> _tables;
  Map<String, int>? _ranks;

  /// The synced tables this builder (and the wire format) recognizes.
  Set<String> get tableNames => _tables.keys.toSet();

  /// Parent-before-child upload rank per synced table, derived from the live
  /// SQLite foreign-key graph: a table ranks strictly above every synced
  /// table it references, so sorting a batch by rank puts every parent row
  /// before its children.
  ///
  /// This exists for the pusher: the oplog's collapsed order is LATEST-op
  /// order, and a cascade delete journals children before their parent, so a
  /// deleted aggregate would otherwise be uploaded child-first. A receiver
  /// that has never seen those rows applies them as fresh inserts, and a page
  /// boundary inside the cascade would leave a child whose parent never
  /// arrived — the applier's FK sweep then drops it with the change token
  /// already advanced, losing the row permanently (found by the convergence
  /// simulation, seed 0x5eed0001). Sorting by this rank restores the
  /// [FetchPage] ordering contract for every push.
  ///
  /// Derived (not hand-kept) so a new synced table can never be missed;
  /// memoized because the schema is fixed for the life of the process.
  Future<Map<String, int>> tableRanks() async {
    final cached = _ranks;
    if (cached != null) return cached;
    final deps = <String, Set<String>>{};
    for (final table in _tables.keys) {
      final fks = await _db
          .customSelect('PRAGMA foreign_key_list("$table")')
          .get();
      deps[table] = {
        for (final fk in fks)
          if (_tables.containsKey(fk.read<String>('table')) &&
              fk.read<String>('table') != table)
            fk.read<String>('table'),
      };
    }
    final ranks = <String, int>{};
    int rankOf(String table, Set<String> visiting) {
      final known = ranks[table];
      if (known != null) return known;
      if (!visiting.add(table)) {
        // An FK cycle would make parent-first ordering undefined. No synced
        // table participates in one today; fail loudly if that ever changes.
        throw StateError('SyncRowBuilder: FK cycle involving "$table"');
      }
      var rank = 0;
      for (final parent in deps[table]!) {
        final r = rankOf(parent, visiting) + 1;
        if (r > rank) rank = r;
      }
      visiting.remove(table);
      ranks[table] = rank;
      return rank;
    }

    for (final table in _tables.keys) {
      rankOf(table, <String>{});
    }
    return _ranks = Map.unmodifiable(ranks);
  }

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
    final perDevice =
        perDeviceColumns[info.actualTableName] ?? const <String>{};
    final fields = <String, Object?>{};
    for (final c in info.$columns) {
      if (perDevice.contains(c.name)) continue; // never leaves this device
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
