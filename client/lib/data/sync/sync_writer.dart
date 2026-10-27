import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';
import 'sync_state_store.dart';

/// The Hybrid Logical Clock stamp + bookkeeping for one local write.
class SyncStamp {
  SyncStamp(this.hlc);

  final Hlc hlc;

  /// Wall time used for created_at / updated_at (the HLC's physical component).
  int get at => hlc.wallMs;
}

/// The SINGLE chokepoint for LOCAL writes. It is the only producer of HLC
/// stamps for local edits and the only writer of the oplog, so a row mutation
/// and its journal entry always commit together (the durability ledger).
///
/// Remote (synced-in) changes do NOT go through here — they adopt the remote
/// HLC and append no oplog (see the future applyRemote two-door path), so an
/// applied remote change is never re-journaled into a ping-pong.
class SyncWriter {
  SyncWriter(this._db, this._clock);

  final AppDatabase _db;
  final HlcClock _clock;

  HlcClock get clock => _clock;

  /// Stamps a local write (ticks the clock). Splice [SyncStamp.hlc] into the
  /// row companion's SyncCols, then call [journal] in the SAME transaction.
  /// For a row that already exists, use [stampFor].
  SyncStamp stamp() => SyncStamp(_clock.tick());

  /// Stamps a local write over the row [id] of [table] (one that exists, or
  /// may: a reused id revives a tombstone). The stamp orders after the row's
  /// stored stamp and after the durable high-water mark of accepted remote
  /// stamps, as well as after this clock's own last one. Run inside the
  /// write's transaction.
  ///
  /// The clock alone does not guarantee that. A second running instance
  /// shares the library (see LibraryLock) with a clock of its own, so a row
  /// it wrote or applied can hold a stamp ahead of this clock, and so can a
  /// newer state of the row it parked (whose stamp only the high-water mark
  /// records, see SyncStateStore.hlcHighWaterKey). An edit stamped from the
  /// clock alone then lost entity-level LWW to the very version it replaced
  /// (or to the parked one, on its replay). Both stamps merge without a
  /// drift re-check, as boot seeds the clock from the same two sources; one
  /// that does not parse is skipped.
  Future<SyncStamp> stampFor(String table, String id) async {
    final info = _db.allTables.where((t) => t.actualTableName == table);
    if (info.isEmpty) throw ArgumentError.value(table, 'table');
    final row = await _db
        .customSelect(
          'SELECT (SELECT hlc_packed FROM ${info.single.actualTableName} '
          'WHERE id = ?) AS held, (SELECT value FROM sync_state '
          'WHERE key = ?) AS high',
          variables: [
            Variable.withString(id),
            Variable.withString(SyncStateStore.hlcHighWaterKey),
          ],
        )
        .getSingle();
    for (final packed in [
      row.readNullable<String>('held'),
      row.readNullable<String>('high'),
    ]) {
      if (packed == null) continue;
      final Hlc seen;
      try {
        seen = Hlc.parse(packed);
      } on FormatException {
        continue;
      }
      if (seen >= _clock.last) _clock.mergeVerified(seen);
    }
    return stamp();
  }

  /// Appends one oplog entry. MUST be called inside the same transaction as the
  /// row write so the ledger and the data are atomic.
  Future<void> journal({
    required String table,
    required String id,
    required String op, // insert | update | tombstone
    required Hlc hlc,
    required String payloadHash,
    String? dependsOnAssetId,
  }) {
    return _db
        .into(_db.oplog)
        .insert(
          OplogCompanion.insert(
            entityTable: table,
            entityId: id,
            op: op,
            hlcPacked: hlc.packed,
            payloadHash: payloadHash,
            dependsOnAssetId: Value(dependsOnAssetId),
            createdAt: hlc.wallMs,
          ),
        );
  }

  /// A stable content hash over a row's CONTENT columns (bookkeeping/SyncCols
  /// excluded by the caller). Used both as the oplog payload hash and for
  /// reconcile change-detection. Values are canonicalized (a fixed encoding)
  /// so a re-serialization that only reorders fields is not a phantom change.
  ///
  /// Non-finite doubles (NaN / ±Infinity) are refused loudly with a clear
  /// [ArgumentError]: jsonEncode would throw on them anyway, but with an
  /// unactionable message — and letting one anywhere near a row write would
  /// be worse (SQLite stores NaN as NULL, so the hash would stop describing
  /// the persisted row). No bounded UI input produces one today; this is the
  /// deterministic guard at the hashing boundary (repositories surface it as
  /// a save-time Result.error, like every data-layer refusal).
  static String contentHashOf(List<Object?> contentColumns) {
    final canonical = jsonEncode(contentColumns.map(_canonical).toList());
    return sha256.convert(utf8.encode(canonical)).toString();
  }

  static Object? _canonical(Object? v) {
    if (v is double && !v.isFinite) {
      throw ArgumentError.value(
        v,
        'contentColumns',
        'non-finite double is not representable in a sync content hash',
      );
    }
    if (v is Map) {
      final keys = v.keys.map((k) => k.toString()).toList()..sort();
      return {for (final k in keys) k: _canonical(v[k])};
    }
    if (v is List) return v.map(_canonical).toList();
    return v;
  }
}
