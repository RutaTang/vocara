import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:drift/drift.dart';
import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import '../sync/sync_writer.dart';
import 'app_database.dart';
import 'durable_fs.dart';

/// Owns immutable audio/media files and the [Assets] indirection table.
///
/// Files are content-addressed: one file per sha256 under
/// `assets/<h0:2>/<h2:4>/<hash>.<ext>`, so a name can never lie about its
/// contents and identical bytes are stored once. The DB never holds a path —
/// rows reference an [assetId]; [resolve] maps that to a live absolute path on
/// this device. The physical location is a per-device detail, so the same row
/// resolves correctly after a reinstall, machine move, or iCloud restore.
class AssetStore {
  AssetStore({
    required this.db,
    required this.root,
    required this.writer,
    this.fs = const DurableFs(),
  });

  final AppDatabase db;

  /// Root directory under which the content-addressed pool lives.
  final Directory root;

  final SyncWriter writer;
  final DurableFs fs;

  /// Imports [sourcePath] into the pool and returns its asset id. Dedups by
  /// content hash: re-importing identical bytes reuses the existing live asset
  /// (and re-materializes the file if it had been GC'd locally).
  Future<String> importFile(String sourcePath, {String kind = 'audio'}) async {
    final src = File(sourcePath);
    final size = await src.length();
    final hash = await _hashFile(src);
    final ext = p.extension(sourcePath).replaceFirst('.', '');

    // content_hash is intentionally NON-unique: identical bytes under two asset
    // ULIDs across devices is the normal content-addressed steady state, so
    // there can be MORE THAN ONE live row for a hash. Take the first
    // deterministically (getSingleOrNull would throw StateError on >1 match).
    final existing =
        await (db.select(db.assets)
              ..where((a) => a.contentHash.equals(hash) & a.deletedAt.isNull())
              ..orderBy([(a) => OrderingTerm(expression: a.hlcPacked)])
              ..limit(1))
            .getSingleOrNull();
    if (existing != null) {
      await _ensureMaterialized(existing, src, hash, ext);
      return existing.id;
    }

    final rel = await _placeFile(src, hash, ext);
    final id = newId();
    final stamp = writer.stamp();
    await db.transaction(() async {
      await db
          .into(db.assets)
          .insert(
            AssetsCompanion.insert(
              id: id,
              contentHash: hash,
              byteSize: size,
              ext: Value(ext),
              kind: Value(kind),
              localRelPath: Value(rel),
              createdAt: stamp.at,
              updatedAt: stamp.at,
              hlcWall: stamp.hlc.wallMs,
              hlcCtr: stamp.hlc.counter,
              hlcNode: stamp.hlc.node,
              hlcPacked: stamp.hlc.packed,
            ),
          );
      await writer.journal(
        table: 'assets',
        id: id,
        op: 'insert',
        hlc: stamp.hlc,
        payloadHash: hash,
      );
    });
    return id;
  }

  /// A stable reference to an asset whose BYTES aren't present locally, encoded
  /// into the domain model's path field so a clip/media survives a reload (and a
  /// later save) instead of being dropped from the model and then tombstoned.
  static String refFor(String assetId) => 'asset://$assetId';

  /// The asset id embedded in a [refFor] reference, or null for a real path.
  static String? assetIdOf(String pathOrRef) =>
      pathOrRef.startsWith('asset://') ? pathOrRef.substring(8) : null;

  /// The asset id for a clip/media reference: if it's an `asset://` ref (bytes
  /// missing locally), the embedded id is reused as-is; otherwise the file at
  /// the path is imported (idempotent + deduped).
  Future<String> resolveOrImport(String pathOrRef, {String kind = 'audio'}) {
    final embedded = assetIdOf(pathOrRef);
    if (embedded != null) return Future.value(embedded);
    return importFile(pathOrRef, kind: kind);
  }

  /// Resolves an asset id to an absolute path on THIS device, or null if the
  /// asset is unknown or its bytes are not present locally.
  Future<String?> resolve(String assetId) async {
    final a = await (db.select(
      db.assets,
    )..where((x) => x.id.equals(assetId))).getSingleOrNull();
    final rel = a?.localRelPath;
    if (rel == null) return null;
    final abs = p.join(root.path, rel);
    return await File(abs).exists() ? abs : null;
  }

  /// The pool-relative path for a (hash, ext), sharded two levels to keep any
  /// one directory small.
  String relPathFor(String hash, String ext) {
    final name = ext.isEmpty ? hash : '$hash.$ext';
    return p.join('assets', hash.substring(0, 2), hash.substring(2, 4), name);
  }

  Future<String> _placeFile(File src, String hash, String ext) async {
    final rel = relPathFor(hash, ext);
    final dest = File(p.join(root.path, rel));
    if (await dest.exists()) return rel; // identical bytes already pooled
    await dest.parent.create(recursive: true);
    final part = File('${dest.path}.part');
    try {
      await src.copy(part.path);
      await fs.fsyncFile(part.path);
      // A concurrent import of the same hash may have placed it first; rename is
      // idempotent enough (overwrites with identical bytes).
      await fs.atomicRename(part.path, dest.path);
      await fs.fsyncDir(dest.parent.path);
    } catch (_) {
      // Don't leave a partial .part on a failed/disk-full copy; surface the
      // error so the save fails loudly instead of silently dropping the take.
      if (await part.exists()) {
        try {
          await part.delete();
        } catch (_) {}
      }
      rethrow;
    }
    return rel;
  }

  /// If a deduped asset's file went missing locally (GC'd, or never downloaded),
  /// re-place it and repoint its (local-only) path — no oplog, it's per-device.
  Future<void> _ensureMaterialized(
    AssetRow existing,
    File src,
    String hash,
    String ext,
  ) async {
    final rel = existing.localRelPath;
    final present =
        rel != null && await File(p.join(root.path, rel)).exists();
    if (present) return;
    final newRel = await _placeFile(src, hash, ext);
    await (db.update(db.assets)..where((a) => a.id.equals(existing.id))).write(
      AssetsCompanion(localRelPath: Value(newRel)),
    );
  }

  Future<String> _hashFile(File f) async {
    final digest = await sha256.bind(f.openRead()).first;
    return digest.toString();
  }

  /// Clears the transient staging trees (`audio/`, `technique_media/`) where
  /// recordings and imports are written before being copied into the pool. After
  /// the drift rebuild nothing persistent references those files (clips/media
  /// reference pool assets), so a leftover there is always a reclaimable orphan.
  /// Meant to run once at startup, when nothing is in flight. Never touches the
  /// content-addressed pool (`assets/`). Best-effort.
  Future<void> sweepStaging() async {
    for (final name in const ['audio', 'technique_media']) {
      try {
        final d = Directory(p.join(root.path, name));
        if (await d.exists()) await d.delete(recursive: true);
      } catch (_) {}
    }
  }

  /// Reclaims local disk: tombstones any live asset row no live clip /
  /// technique-media / practice references, and deletes its pooled file once no
  /// surviving asset shares that content hash (and drops its waveform cache).
  /// Returns the number of assets tombstoned.
  ///
  /// Refcount is DERIVED here (a query), never a stored counter that a crash
  /// could desync. NOTE: this only reclaims LOCAL disk; cloud-byte deletion is a
  /// separate, knowledge-gated step that never runs on a time horizon.
  Future<int> sweepUnreferenced() async {
    final referenced = <String>{};
    for (final sql in const [
      'SELECT DISTINCT asset_id AS a FROM clips WHERE deleted_at IS NULL',
      'SELECT DISTINCT asset_id AS a FROM technique_media WHERE deleted_at IS NULL',
      'SELECT DISTINCT separation_asset_id AS a FROM practices '
          'WHERE deleted_at IS NULL AND separation_asset_id IS NOT NULL',
    ]) {
      for (final r in await db.customSelect(sql).get()) {
        final a = r.data['a'];
        if (a is String) referenced.add(a);
      }
    }

    final live = await db
        .customSelect(
          'SELECT id, content_hash, local_rel_path FROM assets '
          'WHERE deleted_at IS NULL',
        )
        .get();

    var removed = 0;
    for (final a in live) {
      final id = a.read<String>('id');
      if (referenced.contains(id)) continue;

      final s = writer.stamp();
      // The tombstone UPDATE and its oplog entry MUST commit together (a row
      // mutation and its journal entry in the SAME transaction), exactly as
      // importFile does; otherwise a crash between them would tombstone a synced
      // asset with no journal entry and the delete would never propagate.
      await db.transaction(() async {
        await db.customUpdate(
          'UPDATE assets SET deleted_at = ?, updated_at = ?, hlc_wall = ?, '
          'hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
          variables: [
            Variable.withInt(s.at),
            Variable.withInt(s.at),
            Variable.withInt(s.hlc.wallMs),
            Variable.withInt(s.hlc.counter),
            Variable.withString(s.hlc.node),
            Variable.withString(s.hlc.packed),
            Variable.withString(id),
          ],
        );
        await writer.journal(
          table: 'assets',
          id: id,
          op: 'tombstone',
          hlc: s.hlc,
          payloadHash: '',
        );
      });
      removed++;

      // Delete the file only if no surviving asset still maps to this hash.
      final hash = a.read<String>('content_hash');
      final shared = await db
          .customSelect(
            'SELECT COUNT(*) AS n FROM assets WHERE content_hash = ? '
            'AND deleted_at IS NULL',
            variables: [Variable.withString(hash)],
          )
          .getSingle();
      if (shared.read<int>('n') == 0) {
        // No live asset maps to this hash anymore. Delete EVERY pooled file for it,
        // not just this row's local_rel_path: identical bytes imported under a
        // different extension on another device produce `<hash>.wav` AND `<hash>.mp3`
        // as distinct files, so deleting only the current row's path would leak the
        // rest. All files for a hash live in one shard dir (assets/<h0:2>/<h2:4>).
        await _deletePoolFilesForHash(hash);
        await (db.delete(db.waveformCache)
              ..where((w) => w.contentHash.equals(hash)))
            .go();
      }
    }
    return removed;
  }

  /// Deletes every pooled file named `<hash>` or `<hash>.<ext>` from the hash's
  /// shard directory. Best-effort (a lingering file is a reclaimable orphan).
  Future<void> _deletePoolFilesForHash(String hash) async {
    if (hash.length < 4) return;
    final shard = Directory(
      p.join(root.path, 'assets', hash.substring(0, 2), hash.substring(2, 4)),
    );
    if (!await shard.exists()) return;
    await for (final e in shard.list()) {
      if (e is! File) continue;
      final name = p.basename(e.path);
      if (name == hash || name.startsWith('$hash.')) {
        try {
          await e.delete();
        } catch (_) {}
      }
    }
  }

  /// Reclaims pool files that no LIVE asset references. Two cases:
  ///  - a crash between the pool write in [_placeFile] and the asset-row transaction
  ///    in [importFile] leaves a file with NO row at all; and
  ///  - a crash between the tombstone commit and the file unlink in [sweepUnreferenced]
  ///    leaves a file whose only rows are tombstoned (which [sweepUnreferenced] never
  ///    revisits, since it iterates only live rows).
  /// Both are covered by treating only LIVE (deleted_at IS NULL) content hashes as owned.
  /// Meant to run at startup when nothing is in flight. Best-effort; returns the count removed.
  Future<int> sweepOrphanPoolFiles() async {
    final pool = Directory(p.join(root.path, 'assets'));
    if (!await pool.exists()) return 0;
    // Content hashes with a LIVE asset row: a file matching one is legitimately owned.
    // A file matching only a tombstoned hash (or no hash) is an orphan to reclaim.
    final known = <String>{};
    for (final r in await db
        .customSelect(
          'SELECT DISTINCT content_hash AS h FROM assets WHERE deleted_at IS NULL',
        )
        .get()) {
      final h = r.data['h'];
      if (h is String) known.add(h);
    }
    var removed = 0;
    await for (final e in pool.list(recursive: true)) {
      if (e is! File) continue;
      final name = p.basename(e.path);
      if (name.endsWith('.part')) {
        // Transient copy artifact from a crashed import — never a valid pool file.
        try {
          await e.delete();
          removed++;
        } catch (_) {}
        continue;
      }
      // Pool files are named `<sha256>` or `<sha256>.<ext>`.
      final dot = name.indexOf('.');
      final hash = dot < 0 ? name : name.substring(0, dot);
      if (hash.length == 64 && !known.contains(hash)) {
        try {
          await e.delete();
          removed++;
        } catch (_) {}
      }
    }
    return removed;
  }
}
