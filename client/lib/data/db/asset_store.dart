import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

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
    final ext = p.extension(sourcePath).replaceFirst('.', '');
    // ONE streaming pass produces the staged bytes, the hash AND the byte
    // count together, so all three provably describe the same content. The
    // old stat -> hash -> copy sequence opened the source three times with
    // awaits between them: a source modified mid-import could put one file's
    // bytes under another file's hash with a third file's recorded size.
    final staged = await _stageToPool(File(sourcePath));
    try {
      // Promote BEFORE the row work: idempotent by content address, and it
      // replaces a truncated crash leftover with the verified bytes whichever
      // branch below wins.
      final rel = await _promoteStaged(staged, ext);
      // Dedup lookup and insert share ONE transaction so concurrent imports
      // of the same bytes serialize at the database: check-then-insert across
      // interleaved async flows minted one asset row per caller.
      return await db.transaction<String>(() async {
        // content_hash is intentionally NON-unique: identical bytes under two
        // asset ULIDs across devices is the normal content-addressed steady
        // state, so there can be MORE THAN ONE live row for a hash. Take the
        // first deterministically (getSingleOrNull would throw on >1 match).
        final existing =
            await (db.select(db.assets)
                  ..where(
                    (a) =>
                        a.contentHash.equals(staged.hash) &
                        a.deletedAt.isNull(),
                  )
                  ..orderBy([(a) => OrderingTerm(expression: a.hlcPacked)])
                  ..limit(1))
                .getSingleOrNull();
        if (existing != null) {
          // Deduped: re-point the (per-device) path if this row's bytes went
          // missing locally — the promote above already re-placed them.
          final existingRel = existing.localRelPath;
          final present =
              existingRel != null &&
              await File(p.join(root.path, existingRel)).exists();
          if (!present) {
            await (db.update(db.assets)..where((a) => a.id.equals(existing.id)))
                .write(AssetsCompanion(localRelPath: Value(rel)));
          }
          return existing.id;
        }

        final id = newId();
        final stamp = writer.stamp();
        await db
            .into(db.assets)
            .insert(
              AssetsCompanion.insert(
                id: id,
                contentHash: staged.hash,
                byteSize: staged.size,
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
          payloadHash: staged.hash,
        );
        return id;
      });
    } finally {
      // The promote path renames the staging file away; on every other exit
      // (dedup hit, promote failure, DB failure) it must not linger.
      final tmp = File(staged.tmpPath);
      try {
        if (await tmp.exists()) await tmp.delete();
      } catch (_) {}
    }
  }

  /// A stable reference to an asset whose BYTES aren't present locally, encoded
  /// into the domain model's path field so a clip/media survives a reload (and a
  /// later save) instead of being dropped from the model and then tombstoned.
  static String refFor(String assetId) => 'asset://$assetId';

  /// The asset id embedded in a [refFor] reference, or null for a real path.
  static String? assetIdOf(String pathOrRef) =>
      pathOrRef.startsWith('asset://') ? pathOrRef.substring(8) : null;

  /// The asset id for a clip/media reference: if it's an `asset://` ref (bytes
  /// missing locally), the embedded id is reused as-is; if it's the pool path
  /// of a live asset (what [resolve] hands out, so every hydrated clip/media
  /// comes back to its next save this way), that asset's id is reused too;
  /// otherwise the file at the path is imported (idempotent + deduped).
  ///
  /// The pool-path case must NOT go through [importFile]: that streams a full
  /// copy into staging and re-hashes it only to find the bytes already
  /// pooled. A save that touched no audio would then need free disk for its
  /// largest clip (ENOSPC failing a rename), and a pooled file whose bytes
  /// changed after pooling (the case [verifyPoolIntegrity] reports and
  /// deliberately leaves alone) would be minted a NEW asset and its clip
  /// re-pointed at it — journaled, so the bad bytes would ship fleet-wide.
  ///
  /// Runs under the shared side of [gate], like [importFile].
  Future<String> resolveOrImport(String pathOrRef, {String kind = 'audio'}) {
    final embedded = assetIdOf(pathOrRef);
    if (embedded != null) return Future.value(embedded);
    return gate.shared(
      () async =>
          await _livePoolAssetAt(pathOrRef) ??
          await _importFile(pathOrRef, kind: kind),
    );
  }

  /// [resolveOrImport] for a whole save's references, answered in order.
  ///
  /// Every save re-sends every clip, so resolving them one by one cost a
  /// gate entry plus an owner query per clip on EVERY save of a practice —
  /// even a mute click. Here the pool paths' owners are read in one query
  /// ([_livePoolOwners]) and each element gets exactly the answer
  /// [resolveOrImport] would give at that point of a sequential walk: the
  /// owner map is trusted only until this call imports something, because an
  /// import can mint the live owner of a pool path further down the list
  /// (or re-point a deduped row's local_rel_path), and is re-read after one.
  Future<List<String>> resolveOrImportAll(
    List<String> pathsOrRefs, {
    String kind = 'audio',
  }) => gate.shared(() async {
    final out = <String>[];
    Map<String, String>? owners;
    for (var i = 0; i < pathsOrRefs.length; i++) {
      final path = pathsOrRefs[i];
      final embedded = assetIdOf(path);
      if (embedded != null) {
        out.add(embedded);
        continue;
      }
      final rel = _poolRelOf(path);
      if (rel != null) {
        owners ??= await _livePoolOwners({
          for (final rest in pathsOrRefs.skip(i)) ?_poolRelOf(rest),
        });
        final owner = owners[rel];
        if (owner != null) {
          out.add(owner);
          continue;
        }
      }
      out.add(await _importFile(path, kind: kind));
      owners = null; // see the doc: an import invalidates the owner map
    }
    return out;
  });

  /// The pool-relative path of [path], or null when [path] is not inside the
  /// content-addressed pool.
  String? _poolRelOf(String path) {
    final pool = p.join(root.path, 'assets');
    if (!p.isWithin(pool, path)) return null;
    return p.relative(path, from: root.path);
  }

  /// The live asset whose pooled file is [path], or null when [path] is not a
  /// live asset's pool path. Identity is the recorded local_rel_path itself
  /// ([relPathFor] is a pure function of hash + ext), so nothing is read from
  /// the file. When identical bytes are live under more than one asset id,
  /// the first by HLC wins, mirroring [importFile]'s dedup order.
  Future<String?> _livePoolAssetAt(String path) async {
    final rel = _poolRelOf(path);
    if (rel == null) return null;
    return (await _livePoolOwners({rel}))[rel];
  }

  /// rel -> the live asset owning that pool path, for every rel in [rels]
  /// that has one — [_livePoolAssetAt]'s rule (first by HLC) applied to a
  /// set in one query per [inQueryChunk] paths.
  Future<Map<String, String>> _livePoolOwners(Set<String> rels) async {
    final owners = <String, String>{};
    for (final chunk in inChunks(rels.toList())) {
      final rows =
          await (db.select(db.assets)
                ..where(
                  (a) => a.localRelPath.isIn(chunk) & a.deletedAt.isNull(),
                )
                ..orderBy([(a) => OrderingTerm(expression: a.hlcPacked)]))
              .get();
      for (final r in rows) {
        owners.putIfAbsent(r.localRelPath!, () => r.id);
      }
    }
    return owners;
  }

  /// Bound on the ids/paths one `IN (...)` query binds, far below SQLite's
  /// host-parameter ceiling; set reads go through [inChunks].
  static const int inQueryChunk = 500;

  /// [items] in consecutive slices of at most [inQueryChunk], for set reads.
  static Iterable<List<T>> inChunks<T>(List<T> items) sync* {
    for (var i = 0; i < items.length; i += inQueryChunk) {
      yield items.sublist(i, math.min(i + inQueryChunk, items.length));
    }
  }

  /// Resolves an asset id to an absolute path on THIS device, or null if the
  /// asset is unknown or its bytes are not present locally.
  Future<String?> resolve(String assetId) async {
    final a = await (db.select(
      db.assets,
    )..where((x) => x.id.equals(assetId))).getSingleOrNull();
    return a == null ? null : resolveRow(a);
  }

  /// [resolve] for an asset row the caller already read (a hydration that
  /// reads all of an aggregate's assets in one query).
  Future<String?> resolveRow(AssetRow a) async {
    final rel = a.localRelPath;
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

  /// One streaming pass over [src]: the bytes land in a uniquely named
  /// staging file inside the pool tree while the sha256 and byte count
  /// accumulate over exactly the bytes written. The staging name is a fresh
  /// ULID — concurrent imports of the same content each stage privately and
  /// converge only at the (idempotent) promote rename, where the old shared
  /// `<hash>.part` name made them tear each other's copy apart.
  Future<_StagedImport> _stageToPool(File src) async {
    final stagingDir = Directory(p.join(root.path, 'assets', '.staging'));
    // Durably: on the first import this also creates the pool dir itself,
    // which every pooled file's directory entry hangs off.
    await _createDirDurably(stagingDir);
    final tmp = File(p.join(stagingDir.path, '${newId()}.part'));
    final sink = tmp.openWrite();
    Digest? digest;
    final hasher = sha256.startChunkedConversion(
      ChunkedConversionSink<Digest>.withCallback((ds) => digest = ds.single),
    );
    var size = 0;
    try {
      // addStream applies backpressure (the source pauses while a chunk is
      // being written), so memory stays flat on multi-gigabyte imports.
      await sink.addStream(
        src.openRead().map((chunk) {
          hasher.add(chunk);
          size += chunk.length;
          return chunk;
        }),
      );
      await sink.close();
      hasher.close();
    } catch (_) {
      // Don't leave a partial staging file on a failed/disk-full copy;
      // surface the error so the save fails loudly instead of silently
      // dropping the take.
      try {
        await sink.close();
      } catch (_) {}
      try {
        if (await tmp.exists()) await tmp.delete();
      } catch (_) {}
      rethrow;
    }
    return _StagedImport(
      hash: digest!.toString(),
      size: size,
      tmpPath: tmp.path,
    );
  }

  /// Moves a staged file to its content address. An already-present pool file
  /// is reused only when its length matches the staged byte count — a
  /// truncated crash leftover is replaced by the fresh, verified bytes
  /// instead of being trusted on existence alone.
  ///
  /// Length is deliberately the whole test HERE. It is exact against what this
  /// path can produce (an interrupted copy is short, and a file already at
  /// this hash's address came from the same bytes), and a real content check
  /// would re-read the entire pooled file on every import of bytes the pool
  /// already holds. Content is verified in [verifyPoolIntegrity] instead,
  /// which the repair pass runs once and which also covers what no import
  /// check could: bytes that changed after they were pooled.
  Future<String> _promoteStaged(_StagedImport staged, String ext) async {
    final rel = relPathFor(staged.hash, ext);
    final dest = File(p.join(root.path, rel));
    if (await dest.exists() && await dest.length() == staged.size) {
      return rel; // identical bytes already pooled
    }
    await _createDirDurably(dest.parent);
    await fs.fsyncFile(staged.tmpPath);
    // A concurrent import of the same hash may have placed it first; rename
    // is idempotent enough (overwrites with identical bytes).
    await fs.atomicRename(staged.tmpPath, dest.path);
    await fs.fsyncDir(dest.parent.path);
    return rel;
  }

  /// Creates [dir] and any missing ancestors, flushing each directory THIS
  /// call creates into its parent. Flushing the file's own shard after the
  /// rename only makes the file's entry durable within that shard; a power
  /// loss must also find the shard itself, so a fresh `assets/<ab>/` or
  /// `assets/<ab>/<cd>/` has to be on permanent storage before the row
  /// referencing a file inside it commits. An ancestor that already exists is
  /// not re-flushed: its creator did that (or, for a concurrent import racing
  /// into the same fresh shard, is doing it — the same in-process window as
  /// the already-pooled early return in [_promoteStaged]).
  Future<void> _createDirDurably(Directory dir) async {
    if (await dir.exists()) return;
    await _createDirDurably(dir.parent);
    await dir.create(); // an EEXIST from a concurrent creator is success
    await fs.fsyncDir(dir.parent.path);
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

  /// Reclaims local disk: tombstones any never-synced live asset row no live
  /// clip / technique-media / practice references, and moves its pooled file into the
  /// trash once no surviving asset shares that content hash (and drops its
  /// waveform cache). Returns the number of assets tombstoned.
  ///
  /// Refcount is DERIVED here (a query over [assetRefColumns]), never a stored
  /// counter that a crash could desync. NOTE: this only reclaims LOCAL disk;
  /// cloud-byte deletion is a separate, knowledge-gated step that never runs on
  /// a time horizon.
  ///
  /// Four guards against sweeping assets that are only LOCALLY (or
  /// transiently) unreferenced — a tombstone here is journaled, so once sync
  /// ships it would propagate as a fleet-wide delete, and a moment of local
  /// blindness must never become permanent:
  ///
  ///  * Only NEVER-SYNCED assets (both per-device sync-state columns still at
  ///    their 'localOnly' default — see the Assets doc in tables.dart) are
  ///    sweepable at all. Reachability below is computed from the LOCAL
  ///    database only, and for an asset that has crossed the wire that is not
  ///    knowledge: a device days behind on pull would see a remotely
  ///    referenced asset as garbage and broadcast its delete fleet-wide. An
  ///    asset this device has ever pushed (or whose bytes were ever synced)
  ///    is therefore left alone by this sweep entirely; a future server-side
  ///    refcount — the only party that can see EVERY reference — owns the
  ///    lifecycle of synced assets.
  ///  * Once a transport has EVER been active (the durable
  ///    [SyncStateStore.transportSeenKey] marker, written before the first
  ///    wire call), an asset with a PENDING oplog entry (entity_table
  ///    'assets', matching entity_id) is never swept, even while both
  ///    sync-state columns still read 'localOnly'. The push ack is what
  ///    deletes a journal entry AND (just before it) what marks the record
  ///    synced — and a crash can land between the server durably persisting
  ///    a push and either of those: the row is then already server-side,
  ///    where peers may have pulled it, while this device still looks
  ///    never-synced. The boot GC runs BEFORE any sync pump could repair
  ///    that bookkeeping, so without this guard it would sweep exactly such
  ///    a row and journal a tombstone that broadcasts a fleet-wide delete
  ///    for it. An unacked journal entry therefore means "this row may
  ///    already be server-side; its fate is not locally decidable yet" —
  ///    once the entry IS acked, the row was either marked synced (first
  ///    guard) or truly never left. Failing safe is deliberate: holding a
  ///    sweep leaks a little disk until the ledger drains; sweeping a
  ///    server-held row loses data fleet-wide. On a library where NO
  ///    transport has ever existed the guard stands down — nothing ever
  ///    acks the oplog there (boot compaction keeps one entry per row
  ///    forever), so the guard would otherwise hold EVERY asset and quietly
  ///    turn off local disk reclaim, while the marker's absence is proof no
  ///    server can hold any of these rows.
  ///  * Rows parked in the sync quarantine count as references: a quarantined
  ///    payload (half-delivered page, forward-incompatible record) may carry
  ///    the ONLY pointer at an asset until a later app upgrade re-applies it.
  ///  * An asset row younger than [minAge] (by its created/updated stamp) is
  ///    never swept: a save's clip/media rows commit AFTER its importFile, so
  ///    a brand-new unreferenced row is far more likely an in-flight save (or
  ///    a not-yet-applied sync backlog) than garbage. Real garbage is still
  ///    reclaimed by the next sweep once it ages past the window.
  ///
  /// The transport-seen / pending-journal / never-synced guards are read
  /// TWICE: coarsely in the candidate scan below, and again ATOMICALLY inside
  /// each asset's tombstone transaction (as conditions of the UPDATE itself).
  /// The scan alone is racy: the first-ever push can run concurrently with a
  /// user-triggered repairLibrary sweep, and the durable transport marker,
  /// the asset's journal entry, or its sync-state columns can change between
  /// the scan and the tombstone write — the scan then sweeps (and journals a
  /// fleet-wide delete for) a row the server may already hold. Holding
  /// [gate] exclusively across the push was rejected (it would block repair
  /// across a network await), so the guards are re-verified where they
  /// cannot race: a conditional UPDATE that affects 0 rows means the asset's
  /// fate changed hands mid-sweep, and it is skipped whole — no tombstone,
  /// no journal entry, no file trash.
  ///
  /// [debugBeforeTombstone] is a test-only seam (same pattern as
  /// SyncEngine's debugBeforePageCommit): it runs after the candidate scan
  /// decided to sweep an asset and before that asset's tombstone
  /// transaction — exactly the window the atomic re-verification protects.
  Future<int> sweepUnreferenced({
    Duration minAge = const Duration(hours: 24),
    Future<void> Function(String assetId)? debugBeforeTombstone,
  }) async {
    final referenced = <String>{};
    // Quarantine FIRST, tables second: a concurrent pull's replay can apply a
    // parked clip and delete its quarantine entry between the two scans. In
    // this order the reference is visible either way — still parked when the
    // quarantine is listed, or already a live row when the tables are read.
    // The reverse order has a window where it is visible to neither.
    referenced.addAll(await _quarantineReferencedAssetIds());
    for (final ref in assetRefColumns) {
      final sql =
          'SELECT DISTINCT ${ref.column} AS a FROM ${ref.table} '
          'WHERE deleted_at IS NULL AND ${ref.column} IS NOT NULL';
      for (final r in await db.customSelect(sql).get()) {
        final a = r.data['a'];
        if (a is String) referenced.add(a);
      }
    }

    // The never-synced guard (first bullet of the doc): only assets whose
    // record AND bytes never left this device are candidates. Both columns
    // are per-device bookkeeping that only this device's sync engine ever
    // advances past 'localOnly', so the predicate is exactly "no other device
    // can possibly hold a reference we can't see".
    // The pending-journal guard (second bullet of the doc) rides along as a
    // NOT EXISTS conjunct — but only once a transport has ever existed: it
    // can only shrink the candidate set the other guards produced, and on a
    // never-synced library it would shrink it to nothing forever (see the
    // bullet). Raw key read rather than SyncStateStore: this store must not
    // depend on the sync layer.
    final transportSeen =
        (await db
                .customSelect(
                  'SELECT 1 AS x FROM sync_state WHERE "key" = \'transport_seen\'',
                )
                .get())
            .isNotEmpty;
    final pendingJournalGuard = transportSeen
        ? 'AND NOT EXISTS (SELECT 1 FROM oplog '
              "WHERE entity_table = 'assets' AND entity_id = assets.id)"
        : '';
    final live = await db
        .customSelect(
          'SELECT id, content_hash, local_rel_path, created_at, updated_at '
          'FROM assets WHERE deleted_at IS NULL '
          "AND record_sync_state = 'localOnly' "
          "AND bytes_sync_state = 'localOnly' "
          '$pendingJournalGuard',
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

      if (debugBeforeTombstone != null) await debugBeforeTombstone(id);

      final s = writer.stamp();
      // The tombstone UPDATE and its oplog entry MUST commit together (a row
      // mutation and its journal entry in the SAME transaction), exactly as
      // importFile does; otherwise a crash between them would tombstone a synced
      // asset with no journal entry and the delete would never propagate.
      //
      // The WHERE clause RE-VERIFIES, atomically with the write, every guard
      // the candidate scan read in a separate step (see the doc comment): the
      // row must still be live, still never-synced on both per-device
      // columns, and still clear of the pending-journal guard — expressed as
      // "no transport marker OR no pending oplog entry", the exact predicate
      // the scan applies (the guard engages only once a transport has ever
      // existed). A first push racing this sweep flips one of those between
      // the scan and here; the UPDATE then matches 0 rows and the asset is
      // skipped whole.
      final tombstoned = await db.transaction(() async {
        final changed = await db.customUpdate(
          'UPDATE assets SET deleted_at = ?, updated_at = ?, hlc_wall = ?, '
          'hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ? '
          'AND deleted_at IS NULL '
          "AND record_sync_state = 'localOnly' "
          "AND bytes_sync_state = 'localOnly' "
          'AND (NOT EXISTS (SELECT 1 FROM sync_state '
          "WHERE \"key\" = 'transport_seen') "
          'OR NOT EXISTS (SELECT 1 FROM oplog '
          "WHERE entity_table = 'assets' AND entity_id = assets.id))",
          variables: [
            Variable.withInt(s.at),
            Variable.withInt(s.at),
            Variable.withInt(s.hlc.wallMs),
            Variable.withInt(s.hlc.counter),
            Variable.withString(s.hlc.node),
            Variable.withString(s.hlc.packed),
            Variable.withString(id),
          ],
          updates: {db.assets},
        );
        if (changed == 0) return false;
        // The journal entry is written ONLY when the row actually tombstoned:
        // a journaled tombstone for a row the conditional UPDATE refused
        // would broadcast a fleet-wide delete for a row that is still live.
        await writer.journal(
          table: 'assets',
          id: id,
          op: 'tombstone',
          hlc: s.hlc,
          payloadHash: '',
        );
        return true;
      });
      // Fate changed hands mid-sweep (first push landed, repair raced us):
      // not removed, and the file-trash step below must not run either.
      if (!tombstoned) continue;
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

  /// Re-hashes every pooled file and returns the paths whose CONTENT no longer
  /// matches the content address they are stored under.
  ///
  /// Nothing on the import path can find these. [_promoteStaged] reuses an
  /// already-present pool file whose LENGTH matches the freshly staged bytes,
  /// which is the right guard for what import can actually create — a
  /// truncated crash leftover — but it is not a content check, and paying for
  /// one there would re-read the whole pooled file on every import of bytes
  /// the pool already holds. So the content check lives here instead,
  /// in the user-invoked repair pass, where reading the pool once is
  /// affordable — and where it also catches the case import never could:
  /// bytes that rot or are edited AFTER they were pooled.
  ///
  /// REPORTS ONLY. A mismatched file is still the user's recording, most
  /// likely playable; trashing it (or hiding it behind a bytes-missing row)
  /// on the strength of a checksum would take a take away from them to fix an
  /// accounting problem. The count goes in the repair report and the paths go
  /// to the diagnostics log, and what to do about it stays the user's call.
  Future<List<String>> verifyPoolIntegrity() async {
    final pool = Directory(p.join(root.path, 'assets'));
    if (!await pool.exists()) return const [];
    final bad = <String>[];
    await for (final e in pool.list(recursive: true)) {
      if (e is! File) continue;
      final name = p.basename(e.path);
      if (name.endsWith('.part')) continue; // in-flight/crashed import copy
      final dot = name.indexOf('.');
      final hash = dot < 0 ? name : name.substring(0, dot);
      if (hash.length != 64) continue; // not a content-addressed pool file
      try {
        final actual = (await sha256.bind(e.openRead()).first).toString();
        if (actual != hash) bad.add(e.path);
      } catch (_) {
        // Unreadable right now: that is a different failure (and one the
        // caller will meet again on playback). Not evidence of wrong content.
      }
    }
    return bad;
  }

  /// Reclaims pool files that no LIVE asset references — by moving them into
  /// [trashDir] (recoverable until [pruneTrash] ages them out). Two cases:
  ///  - a crash between the pool write in [_promoteStaged] and the asset-row
  ///    transaction in [importFile] leaves a file with NO row at all; and
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

/// The outcome of [AssetStore._stageToPool]: hash and byte count measured
/// over exactly the bytes staged at [tmpPath] — one pass, one truth.
class _StagedImport {
  const _StagedImport({
    required this.hash,
    required this.size,
    required this.tmpPath,
  });

  final String hash;
  final int size;
  final String tmpPath;
}
