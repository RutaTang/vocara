import 'dart:convert';

import 'package:drift/drift.dart';

import '../../domain/models/audio_fx.dart';
import '../../domain/models/lyric_line.dart';
import '../../utils/id.dart';
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
  /// [fields] and [fieldClocks] are defensively copied and frozen: a SyncRow
  /// is a value in transit, and silent mutation after LWW comparison (or
  /// after a future column-clock validation) would be a data bug.
  SyncRow({
    required this.table,
    required Map<String, Object?> fields,
    Map<String, String>? fieldClocks,
  }) : fields = Map.unmodifiable(fields),
       fieldClocks = fieldClocks == null ? null : Map.unmodifiable(fieldClocks);

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
      // jsonDecode spells an overflowing literal (1e999) as ±Infinity, which
      // no column can hold and no peer can re-encode (see checkColumnTypes).
      if (value is double && !value.isFinite) {
        throw FormatException('SyncRow: field "$key" is not a finite number');
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
    // NOTE: [checkIdShapes] deliberately does NOT run here — fromJson decodes
    // whole transport pages, where one hostile record must not fail every
    // good record with it. The applier runs it per record (quarantining the
    // one offender), the push engine per conflicted ack.
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
    // Numeric bounds are ORDERING-CORRECTNESS, not pedantry: the packed
    // encoding is fixed-width (13-digit wall, 6-digit counter), so a counter
    // of 1000000 or a 14-digit wall would repack "consistently" while its
    // string order diverged from the tuple order — the LWW comparison and
    // the drift gate would then disagree about which stamp is newer. The
    // node charset admits real (Crockford-envelope) ids and the reserved
    // '!'-extended derived space, bounded so a hostile stamp cannot bloat
    // every packed key it touches.
    if (hlcCtr < 0 || hlcCtr > HlcClock.maxCounter) {
      throw const FormatException(
        'SyncRow: hlc_ctr outside the packed 6-digit range',
      );
    }
    if (hlcWall < 0 || hlcWall > 9999999999999) {
      throw const FormatException(
        'SyncRow: hlc_wall outside the packed 13-digit range',
      );
    }
    if (hlcNode.length > HlcClock.maxNodeLength ||
        !_nodeShape.hasMatch(hlcNode)) {
      throw const FormatException(
        'SyncRow: hlc_node outside the supported node grammar',
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

  /// Node grammar: a DERIVATION CHAIN — the minting device's 26-character id,
  /// then one `'!'`-joined segment per derived write ([HlcClock.derived]).
  /// Pinning the segment WIDTH, not just the alphabet, is what keeps the
  /// node-space derivation available: an arbitrary blob one character below
  /// the ceiling would force every stamp derived from it into the ceiling
  /// rule. Deliberately a SUPERSET of the Crockford alphabet ids are minted
  /// from, like [_idShape] — the wire depends on the shape, and only the mint
  /// needs the exact alphabet. No dash: the packed encoding uses it as the
  /// field separator.
  static final RegExp _nodeShape = RegExp(
    '^[0-9A-Za-z]{${HlcClock.nodeSegmentLength}}'
    '(?:![0-9A-Za-z]{${HlcClock.nodeSegmentLength}})*'
    r'$',
  );

  /// The envelope every entity id on the wire must fit: 26–36 characters of
  /// `[0-9A-Za-z_-]`. This is a SECURITY gate, not a style check: ids become
  /// filesystem path components (`audio/<practiceId>/`,
  /// `technique_media/<techniqueId>/`) and are handed to recursive deletes —
  /// an id carrying `..`, a separator, or an absolute path would let a
  /// hostile sync record write or delete OUTSIDE the media tree. No dot and
  /// no separator in the alphabet closes that entirely.
  ///
  /// The length is the one every synced table's id column declares
  /// (SyncCols in tables.dart). drift enforces it only on its own inserts,
  /// so the applier's raw-SQL insert would otherwise land a row no local
  /// write could have produced — and that the repository's revive path (a
  /// drift insert) then refuses, failing the save.
  ///
  /// Deliberately a SUPERSET of today's id grammars — [newId] ULIDs plus the
  /// legacy-migration shapes `<trackId>-c0` (clips) and `<practiceId>__v1`
  /// (interpretations) — rather than an exact enumeration: the services
  /// whose components really are pure ULIDs (audio/technique media dirs)
  /// enforce that stricter shape themselves. Quarantine slot keys
  /// (QuarantineStore.sweepSlotId, dead letters) never ride the wire.
  static final RegExp _idShape = RegExp(r'^[0-9A-Za-z_-]{26,36}$');

  /// Tables whose OWN id becomes a filesystem path component: the media
  /// services build `audio/<practiceId>/` and `technique_media/<techniqueId>/`
  /// from them and recursively delete those trees. For these the wire must
  /// enforce the same strict shape the services do ([isPathComponentId]) —
  /// the superset [_idShape] otherwise admits a row that syncs cleanly and is
  /// then un-operable locally, because every media call for it throws.
  ///
  /// Child tables are absent on purpose: a clip carries `<trackId>-c0` under
  /// the legacy shape and never names a directory itself.
  static const Set<String> pathComponentTables = {'practices', 'techniques'};

  /// Validates `id` and every `*_id` column (when non-null) against
  /// [_idShape], plus the stricter [isPathComponentId] for the id of a
  /// [pathComponentTables] row; throws [FormatException] otherwise. Runs at
  /// the same wire doors as [checkHlcConsistency]. `hlc_node` is deliberately
  /// excluded — it never touches a path and has its own rules (the `'!'`
  /// derived space).
  static void checkIdShapes(String table, Map<String, Object?> fields) {
    for (final entry in fields.entries) {
      final key = entry.key;
      if (key != 'id' && !key.endsWith('_id')) continue;
      final value = entry.value;
      if (value == null && key != 'id') continue;
      if (value is! String || !_idShape.hasMatch(value)) {
        throw FormatException('SyncRow: "$key" is not a well-formed entity id');
      }
      if (key == 'id' &&
          pathComponentTables.contains(table) &&
          !isPathComponentId(value)) {
        throw FormatException(
          'SyncRow: "$table.id" must be usable as a path component',
        );
      }
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

/// Decodes one `*_json` column body the way the REPOSITORY that hydrates it
/// does, throwing if the body is not that shape. Keyed by column name (the
/// name carries the shape: `fx_json` is a [TrackFx] object in every table that
/// has one), and exhaustive over the schema — a schema-derived test fails if a
/// synced table gains a `*_json` column with no entry here.
///
/// Parseable JSON is not enough. `lyrics_json = '{}'` is valid JSON that binds
/// to a TEXT column and commits happily, and then the hydrator's
/// `jsonDecode(...) as List` throws — inside a view model, long after the
/// record could have been parked with its evidence, leaving a practice that
/// has vanished from the list and cannot be opened. Running the real decoder
/// at the door makes the gate exactly as strict as the reader, by
/// construction: anything that survives here hydrates.
const Map<String, void Function(Object?)> jsonColumnDecoders = {
  'reverb_json': _decodeReverb,
  'lyrics_json': _decodeLyrics,
  'fx_json': _decodeFx,
  'sends_json': _decodeSends,
  'aliases_json': _decodeStringList,
  'tags_json': _decodeStringList,
};

void _decodeReverb(Object? body) =>
    ReverbSettings.fromJson(body! as Map<String, dynamic>);

void _decodeFx(Object? body) => TrackFx.fromJson(body! as Map<String, dynamic>);

void _decodeLyrics(Object? body) {
  for (final e in body! as List) {
    LyricLine.fromJson(e as Map<String, dynamic>);
  }
}

void _decodeSends(Object? body) {
  for (final e in body! as List) {
    TrackSend.fromJson(e as Map<String, dynamic>);
  }
}

// Converted, not cast lazily: a cast only throws when the UI later iterates
// it, the "fails far away" problem this gate exists to end.
void _decodeStringList(Object? body) => List<String>.from(body! as List);

/// Rejects any wire value that would not BIND to its column, using the live
/// drift schema as the single source of what each column holds. Throws
/// [FormatException] on the first mismatch.
///
/// SQLite's dynamic typing accepts a string into an INTEGER column without
/// complaint, so the failure surfaced far away — when drift hydrated the row
/// and threw inside a view model — rather than at the door where the record
/// could still be quarantined with its evidence. `*_json` columns are parsed
/// too: a malformed body is only detectable there, and it fails the same way.
/// A REAL must be finite: SQLite stores ±Infinity happily, but the content
/// hash every repository computes on load refuses it (the practice could no
/// longer be opened) and no row carrying it can be JSON-encoded again (the
/// FK sweep's park, a re-push).
///
/// Blob and datetime columns are refused outright: the v1 wire format encodes
/// scalars only ([SyncRow.fromJson]), so a synced table gaining such a column
/// is a protocol change that must be designed, not silently admitted through
/// whatever JSON happened to arrive.
void checkColumnTypes(
  TableInfo<Table, dynamic> table,
  Map<String, Object?> fields,
) {
  for (final column in table.$columns) {
    if (!fields.containsKey(column.name)) continue;
    final value = fields[column.name];
    if (value == null) continue; // nullability is the database's own gate
    final ok = switch (column.type) {
      DriftSqlType.int || DriftSqlType.bigInt => value is int,
      DriftSqlType.bool => value is bool || value is int,
      DriftSqlType.double => value is num && value.isFinite,
      DriftSqlType.string => value is String,
      _ => false,
    };
    if (!ok) {
      throw FormatException(
        'SyncRow: "${table.actualTableName}.${column.name}" cannot hold a '
        '${value.runtimeType}',
      );
    }
    final bound = value is int ? _intColumnBound(column.name) : null;
    // Compared on both sides, not by abs(): the most negative int has none.
    if (bound != null && ((value as int) > bound || value < -bound)) {
      throw FormatException(
        'SyncRow: "${table.actualTableName}.${column.name}" is out of range '
        '($value)',
      );
    }
    if (column.name.endsWith('_json') && value is String) {
      final Object? body;
      try {
        body = jsonDecode(value);
      } on FormatException {
        throw FormatException(
          'SyncRow: "${table.actualTableName}.${column.name}" is not JSON',
        );
      }
      final decode = jsonColumnDecoders[column.name];
      if (decode == null) {
        // A synced table gained a JSON column and nobody said what it holds.
        // Refusing it is the same stance the blob/datetime branch takes: an
        // undesigned wire shape is quarantined, never admitted on the strength
        // of "it parsed". The schema-derived test makes this unreachable in a
        // build that ships.
        throw FormatException(
          'SyncRow: "${table.actualTableName}.${column.name}" has no '
          'registered JSON body shape',
        );
      }
      try {
        decode(body);
      } on FormatException {
        rethrow;
      } catch (e) {
        // Every decoder failure must leave here as a FormatException: that is
        // what the applier catches to park the record with its evidence.
        throw FormatException(
          'SyncRow: "${table.actualTableName}.${column.name}" is JSON but not '
          'the shape that column holds (${e.runtimeType})',
        );
      }
    }
  }
}

/// The magnitude an int column may hold on the wire, by its name, or null for
/// none. A time (`*_at`, ms since the epoch) within what a DateTime holds:
/// past it, every list of the library threw. A timeline position or length
/// (`*_ms`) within a day: past it, the timeline was laid out and gridded over
/// trillions of steps and the practice froze.
int? _intColumnBound(String column) {
  if (column.endsWith('_at')) return 8640000000000000;
  if (column.endsWith('_ms')) return 24 * 60 * 60 * 1000;
  return null;
}

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
