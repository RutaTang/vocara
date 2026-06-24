import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:drift/drift.dart';

import '../db/app_database.dart';
import 'hlc.dart';

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
  SyncStamp stamp() => SyncStamp(_clock.tick());

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
  static String contentHashOf(List<Object?> contentColumns) {
    final canonical = jsonEncode(contentColumns.map(_canonical).toList());
    return sha256.convert(utf8.encode(canonical)).toString();
  }

  static Object? _canonical(Object? v) {
    if (v is Map) {
      final keys = v.keys.map((k) => k.toString()).toList()..sort();
      return {for (final k in keys) k: _canonical(v[k])};
    }
    if (v is List) return v.map(_canonical).toList();
    return v;
  }
}
