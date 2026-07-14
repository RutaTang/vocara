import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:drift/drift.dart';
import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import '../sync/quarantine_store.dart';
import '../sync/sync_writer.dart';
import 'app_database.dart';
import 'durable_fs.dart';
import 'write_gate.dart';

/// Owns immutable audio/media files and the [Assets] indirection table.
///
/// Files are content-addressed: one file per sha256 under
/// `assets/<h0:2>/<h2:4>/<hash>.<ext>`, so a name can never lie about its
/// contents and identical bytes are stored once. The DB never holds a path —
/// rows reference an [assetId]; [resolve] maps that to a live absolute path on
/// this device. The physical location is a per-device detail, so the same row
/// resolves correctly after a reinstall, machine move, or cloud restore.
class AssetStore {
  AssetStore({
    required this.db,
    required this.root,
    required this.writer,
    this.fs = const DurableFs(),
    WriteGate? gate,
  }) : gate = gate ?? WriteGate();

  final AppDatabase db;

  /// Root directory under which the content-addressed pool lives.
  final Directory root;

  final SyncWriter writer;
  final DurableFs fs;

  /// The app-wide store write gate (see [WriteGate]): saves/imports run under
  /// its shared side, repairLibrary and the boot sweeps take it exclusively so
  /// a GC pass can never race an import+commit sequence. Owned here because
  /// every writer already holds an [AssetStore]; Persistence exposes it too.
  final WriteGate gate;

  /// Imports [sourcePath] into the pool and returns its asset id. Dedups by
  /// content hash: re-importing identical bytes reuses the existing live asset
  /// (and re-materializes the file if it had been GC'd locally).
  ///
  /// Runs under the shared side of [gate] (nesting inside a caller's shared
  /// section is fine — the gate is reader-preferring), so an exclusive
  /// maintenance pass can never interleave with the pool write + asset-row
  /// commit and trash the in-flight `.part` or the just-placed file.
  Future<String> importFile(String sourcePath, {String kind = 'audio'}) =>
      gate.shared(() => _importFile(sourcePath, kind: kind));

  Future<String> _importFile(String sourcePath, {required String kind}) async {
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
    final present = rel != null && await File(p.join(root.path, rel)).exists();
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
  /// Each tree is MOVED into [trashDir] (recoverable until [pruneTrash] ages it
  /// out), never deleted in place. Meant to run once at startup, when nothing
  /// is in flight. Never touches the content-addressed pool (`assets/`).
  /// Best-effort.
  ///
  /// [keep]: absolute paths of files that must SURVIVE this sweep — staged
  /// recordings whose boot recovery ATTEMPT threw (an environment-wide decoder
  /// or disk failure, not per-file junk). Sweeping those would destroy the
  /// only copy of a take with no retry ever; leaving them lets the next boot's
  /// recovery try again. Persistence bounds the exemption by file age.
  Future<void> sweepStaging({Set<String> keep = const {}}) async {
    for (final name in const ['audio', 'technique_media']) {
      try {
        final d = Directory(p.join(root.path, name));
        if (!await d.exists()) continue;
        if (keep.isEmpty) {
          await _moveToTrash(d);
          continue;
        }
        await _sweepTreeExcept(d, keep);
        // Drop the top-level tree itself once nothing kept remains inside.
        try {
          if (await d.list().isEmpty) await d.delete();
        } catch (_) {}
      } catch (_) {}
    }
  }

  /// Moves every entry under [dir] (recursively) into the trash except files
  /// whose exact path is in [keep]; subdirectories emptied by the sweep are
  /// removed. Best-effort per entry, like the whole-tree path.
  Future<void> _sweepTreeExcept(Directory dir, Set<String> keep) async {
    await for (final e in dir.list()) {
      if (e is Directory) {
        await _sweepTreeExcept(e, keep);
        try {
          if (await e.list().isEmpty) await e.delete();
        } catch (_) {}
      } else {
        if (keep.contains(e.path)) continue;
        await _moveToTrash(e);
      }
    }
  }

  /// Every (table, column) that can reference an asset id — THE single source
  /// of truth the GC derives reachability from. It MUST cover every
  /// `.references(Assets)` column in the schema: a column missing here makes
  /// [sweepUnreferenced] tombstone assets that are still in use and reclaim
  /// their bytes. Guarded by a test (asset_gc_test.dart) that derives this set
  /// structurally from the live schema and fails on any drift.
  ///
  /// NOTE: Persistence._referencedContentHashes derives its hash set from
  /// this same constant, but WITHOUT the live-row filter — the staged
  /// recording recovery must respect tombstoned references too (a deleted
  /// take must not be resurrected); see its doc for why the filters differ.
  static const List<({String table, String column})> assetRefColumns = [
    (table: 'clips', column: 'asset_id'),
    (table: 'technique_media', column: 'asset_id'),
    (table: 'practices', column: 'separation_asset_id'),
  ];

  /// Reclaims local disk: tombstones any live asset row no live clip /
  /// technique-media / practice references, and moves its pooled file into the
  /// trash once no surviving asset shares that content hash (and drops its
  /// waveform cache). Returns the number of assets tombstoned.
  ///
  /// Refcount is DERIVED here (a query over [assetRefColumns]), never a stored
  /// counter that a crash could desync. NOTE: this only reclaims LOCAL disk;
  /// cloud-byte deletion is a separate, knowledge-gated step that never runs on
  /// a time horizon.
  ///
  /// Two guards against sweeping assets that are only TRANSIENTLY
  /// unreferenced (a tombstone here is journaled, so once sync ships it would
  /// propagate as a fleet-wide delete — a moment of local blindness must
  /// never become permanent):
  ///
  ///  * Rows parked in the sync quarantine count as references: a quarantined
  ///    payload (half-delivered page, forward-incompatible record) may carry
  ///    the ONLY pointer at an asset until a later app upgrade re-applies it.
  ///  * An asset row younger than [minAge] (by its created/updated stamp) is
  ///    never swept: a save's clip/media rows commit AFTER its importFile, so
  ///    a brand-new unreferenced row is far more likely an in-flight save (or
  ///    a not-yet-applied sync backlog) than garbage. Real garbage is still
  ///    reclaimed by the next sweep once it ages past the window.
  Future<int> sweepUnreferenced({
    Duration minAge = const Duration(hours: 24),
  }) async {
    final referenced = <String>{};
    for (final ref in assetRefColumns) {
      final sql =
          'SELECT DISTINCT ${ref.column} AS a FROM ${ref.table} '
          'WHERE deleted_at IS NULL AND ${ref.column} IS NOT NULL';
      for (final r in await db.customSelect(sql).get()) {
        final a = r.data['a'];
        if (a is String) referenced.add(a);
      }
    }
    referenced.addAll(await _quarantineReferencedAssetIds());

    final live = await db
        .customSelect(
          'SELECT id, content_hash, local_rel_path, created_at, updated_at '
          'FROM assets WHERE deleted_at IS NULL',
        )
        .get();

    final nowMs = DateTime.now().millisecondsSinceEpoch;
    final minAgeMs = minAge.inMilliseconds;
    var removed = 0;
    for (final a in live) {
      final id = a.read<String>('id');
      if (referenced.contains(id)) continue;
      // The age grace window (see doc): a young unreferenced row is treated
      // as in flight, not as garbage.
      final createdAt = a.read<int>('created_at');
      final updatedAt = a.read<int>('updated_at');
      final newest = createdAt > updatedAt ? createdAt : updatedAt;
      if (nowMs - newest < minAgeMs) continue;

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

      // Reclaim the file only if no surviving asset still maps to this hash.
      final hash = a.read<String>('content_hash');
      final shared = await db
          .customSelect(
            'SELECT COUNT(*) AS n FROM assets WHERE content_hash = ? '
            'AND deleted_at IS NULL',
            variables: [Variable.withString(hash)],
          )
          .getSingle();
      if (shared.read<int>('n') == 0) {
        // No live asset maps to this hash anymore. Trash EVERY pooled file for it,
        // not just this row's local_rel_path: identical bytes imported under a
        // different extension on another device produce `<hash>.wav` AND `<hash>.mp3`
        // as distinct files, so reclaiming only the current row's path would leak the
        // rest. All files for a hash live in one shard dir (assets/<h0:2>/<h2:4>).
        await _trashPoolFilesForHash(hash);
        await (db.delete(
          db.waveformCache,
        )..where((w) => w.contentHash.equals(hash))).go();
      }
    }
    return removed;
  }

  /// Asset ids referenced by row payloads parked in the sync quarantine.
  ///
  /// Quarantined records are rejected-but-not-dropped remote state (see
  /// QuarantineStore): a parked clip/media/practice payload may be the only
  /// thing pointing at an asset until an app upgrade or manual review
  /// re-applies it. Read through the store's own API and the SyncRow wire
  /// shape (`{"fields": {<column>: <value>, ...}}`), scanning exactly the
  /// [assetRefColumns] columns. Best-effort: a malformed payload (or an
  /// unreadable quarantine table) contributes no references — the age grace
  /// window in [sweepUnreferenced] still softens that case.
  Future<Set<String>> _quarantineReferencedAssetIds() async {
    final out = <String>{};
    final colsByTable = <String, List<String>>{};
    for (final ref in assetRefColumns) {
      (colsByTable[ref.table] ??= []).add(ref.column);
    }
    try {
      for (final row in await QuarantineStore(db).list()) {
        final cols = colsByTable[row.tbl];
        if (cols == null) continue;
        try {
          final decoded = jsonDecode(row.payload);
          if (decoded is! Map) continue;
          final fields = decoded['fields'];
          if (fields is! Map) continue;
          for (final col in cols) {
            final v = fields[col];
            if (v is String && v.isNotEmpty) out.add(v);
          }
        } catch (_) {
          // Undecodable payload: nothing to protect from this row.
        }
      }
    } catch (_) {
      // Quarantine unreadable: fall through with whatever was gathered.
    }
    return out;
  }

  /// Moves every pooled file named `<hash>` or `<hash>.<ext>` from the hash's
  /// shard directory into the trash — never a hard delete, so a GC mistake
  /// stays recoverable. Best-effort (a lingering file is a reclaimable orphan).
  Future<void> _trashPoolFilesForHash(String hash) async {
    if (hash.length < 4) return;
    final shard = Directory(
      p.join(root.path, 'assets', hash.substring(0, 2), hash.substring(2, 4)),
    );
    if (!await shard.exists()) return;
    await for (final e in shard.list()) {
      if (e is! File) continue;
      final name = p.basename(e.path);
      if (name == hash || name.startsWith('$hash.')) {
        await _moveToTrash(e);
      }
    }
  }

  /// Reclaims pool files that no LIVE asset references — by moving them into
  /// [trashDir] (recoverable until [pruneTrash] ages them out). Two cases:
  ///  - a crash between the pool write in [_placeFile] and the asset-row transaction
  ///    in [importFile] leaves a file with NO row at all; and
  ///  - a crash between the tombstone commit and the file reclaim in [sweepUnreferenced]
  ///    leaves a file whose only rows are tombstoned (which [sweepUnreferenced] never
  ///    revisits, since it iterates only live rows).
  /// Both are covered by treating only LIVE (deleted_at IS NULL) content hashes as owned.
  ///
  /// FUSE: when the assets table holds ZERO live rows while the pool holds
  /// files, the sweep SKIPS entirely. No normal state looks like that — a
  /// fresh install has no pool yet — so it means this database was lost or
  /// replaced while the bytes survived (wiped/restored DB file), and sweeping
  /// would reclaim every byte the user could still recover.
  ///
  /// Meant to run at startup when nothing is in flight. Best-effort; returns the count removed.
  Future<int> sweepOrphanPoolFiles() async {
    final pool = Directory(p.join(root.path, 'assets'));
    if (!await pool.exists()) return 0;
    // Content hashes with a LIVE asset row: a file matching one is legitimately owned.
    // A file matching only a tombstoned hash (or no hash) is an orphan to reclaim.
    final known = <String>{};
    for (final r
        in await db
            .customSelect(
              'SELECT DISTINCT content_hash AS h FROM assets WHERE deleted_at IS NULL',
            )
            .get()) {
      final h = r.data['h'];
      if (h is String) known.add(h);
    }
    if (known.isEmpty) {
      // The fuse (see doc): with no live assets at all, a non-empty pool means
      // "database lost, bytes survived" — protect the bytes and touch nothing.
      await for (final e in pool.list(recursive: true)) {
        if (e is File) {
          debugPrint(
            'AssetStore.sweepOrphanPoolFiles: skipped — the assets table has '
            'zero live rows but the pool holds files; protecting the bytes.',
          );
          return 0;
        }
      }
      return 0; // no live assets AND no pool files: nothing to sweep anyway
    }
    var removed = 0;
    await for (final e in pool.list(recursive: true)) {
      if (e is! File) continue;
      final name = p.basename(e.path);
      if (name.endsWith('.part')) {
        // Transient copy artifact from a crashed import — never a valid pool
        // file, but trash it anyway (uniform no-hard-delete policy).
        if (await _moveToTrash(e)) removed++;
        continue;
      }
      // Pool files are named `<sha256>` or `<sha256>.<ext>`.
      final dot = name.indexOf('.');
      final hash = dot < 0 ? name : name.substring(0, dot);
      if (hash.length == 64 && !known.contains(hash)) {
        if (await _moveToTrash(e)) removed++;
      }
    }
    return removed;
  }

  // --- Trash (GC recoverability) --------------------------------------------

  /// The container-local trash under [root]. Every pool/staging reclaim in this
  /// class MOVES bytes here instead of unlinking them, so no single GC pass (or
  /// GC bug) can ever destroy the only copy of a take. Entries are named
  /// `<epochMs>-<originalName>`; [pruneTrash] ages them out.
  Directory get trashDir => Directory(p.join(root.path, 'trash'));

  /// Moves [entity] (a pool/staging file, or a whole staging tree) into
  /// [trashDir] as `<epochMs>-<originalName>`. Best-effort: returns whether it
  /// moved; on failure the entity is left in place (a lingering orphan is
  /// always safer than an unlink).
  Future<bool> _moveToTrash(FileSystemEntity entity) async {
    try {
      final dir = trashDir;
      await dir.create(recursive: true);
      final base = p.basename(entity.path);
      final stampMs = DateTime.now().millisecondsSinceEpoch;
      var dest = p.join(dir.path, '$stampMs-$base');
      // Uniquify if the same name lands twice within one millisecond — never
      // overwrite an existing trash entry.
      var n = 1;
      while (await FileSystemEntity.type(dest) !=
          FileSystemEntityType.notFound) {
        dest = p.join(dir.path, '$stampMs-${n++}-$base');
      }
      await entity.rename(dest);
      return true;
    } catch (_) {
      return false;
    }
  }

  /// Hard-deletes trash entries older than [retention], dated by the epoch-ms
  /// prefix [_moveToTrash] stamped into each entry's name. An entry without a
  /// parseable prefix is never deleted (don't age out bytes we can't date).
  /// Returns the number of entries removed.
  ///
  /// Deliberately called by NOTHING in this class: the boot sequence wires it
  /// to run once AFTER a successful store open, so trash only ages out on a
  /// boot that proved the database (and with it the pool's ownership info) is
  /// healthy.
  Future<int> pruneTrash({
    Duration retention = const Duration(days: 30),
  }) async {
    final dir = trashDir;
    if (!await dir.exists()) return 0;
    final cutoffMs = DateTime.now().subtract(retention).millisecondsSinceEpoch;
    var removed = 0;
    await for (final e in dir.list()) {
      final name = p.basename(e.path);
      final dash = name.indexOf('-');
      final stampMs = dash > 0 ? int.tryParse(name.substring(0, dash)) : null;
      if (stampMs == null || stampMs >= cutoffMs) continue;
      try {
        await e.delete(recursive: true);
        removed++;
      } catch (_) {}
    }
    return removed;
  }
}
