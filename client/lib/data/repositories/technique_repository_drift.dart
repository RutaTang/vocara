import 'dart:convert';

import 'package:drift/drift.dart';

import '../../domain/models/technique.dart';
import '../../domain/models/technique_media.dart';
import '../../utils/result.dart';
import '../db/app_database.dart';
import '../db/asset_store.dart';
import '../sync/reconcile.dart';
import '../sync/sync_hashes.dart';
import '../sync/sync_writer.dart';
import 'technique_repository.dart';

/// drift/SQLite implementation of the technique library.
///
/// Domain models are unchanged: a [TechniqueMedia] still carries a file [path].
/// The content hash bridges path <-> asset — on save each media file is imported
/// into the content-addressed [AssetStore] (idempotent, deduped) to get its
/// asset id; on load the asset id resolves back to a local path. Writes go
/// through the [SyncWriter] chokepoint + per-entity [reconcileCollection], so
/// editing one field dirties exactly one row.
class TechniqueRepositoryDrift implements TechniqueRepository {
  TechniqueRepositoryDrift({
    required this.db,
    required this.writer,
    required this.assets,
  });

  final AppDatabase db;
  final SyncWriter writer;
  final AssetStore assets;

  static int _ms(DateTime d) => d.millisecondsSinceEpoch;
  static DateTime _dt(int ms) => DateTime.fromMillisecondsSinceEpoch(ms);

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    try {
      final rows = await (db.select(
        db.techniques,
      )..where((t) => t.deletedAt.isNull())).get();
      // Deterministic order: updatedAt desc, then id desc as a total-order
      // tiebreaker so rows sharing a millisecond don't reshuffle between launches
      // (Dart's List.sort is not stable).
      rows.sort((a, b) {
        final c = b.updatedAt.compareTo(a.updatedAt);
        return c != 0 ? c : b.id.compareTo(a.id);
      });
      // Every technique's media (and their assets) in one set read, not a
      // query per technique plus one per example: 601 statements and 400
      // file checks for a 200-technique library, on every library open and
      // every "add technique" picker. Should that read fail, each row falls
      // back to its own read inside the per-row guard below — the old path
      // exactly, so an unreadable row still only costs itself.
      _MediaRows? media;
      try {
        media = await _readMedia([for (final r in rows) r.id]);
      } catch (_) {
        media = null;
      }
      final out = <Technique>[];
      for (final r in rows) {
        try {
          out.add(await _hydrate(r, media: media));
        } catch (_) {
          // Skip a single corrupt row (e.g. a malformed JSON column) rather than
          // failing the whole list — the other readable records must still show.
          continue;
        }
      }
      return Result.ok(out);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  @override
  Future<Result<Technique>> getTechnique(String id) async {
    final r = await loadTechnique(id);
    return switch (r) {
      Ok<Loaded<Technique>>(:final value) => Result.ok(value.aggregate),
      Error<Loaded<Technique>>(:final error) => Result.error(error),
    };
  }

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async {
    try {
      final row =
          await (db.select(db.techniques)
                ..where((t) => t.id.equals(id) & t.deletedAt.isNull()))
              .getSingleOrNull();
      if (row == null) {
        return Result.error(Exception('Technique "$id" not found'));
      }
      // Collect the base hash of every row this read returns, so a later save
      // can three-way merge against exactly what the editor was shown.
      final hashes = <String, String>{};
      final technique = await _hydrate(row, baseHashes: hashes);
      return Result.ok(
        Loaded(technique, Map<String, String>.unmodifiable(hashes)),
      );
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  @override
  Future<Result<Technique>> saveTechnique(Technique technique) async {
    // Base-less save: _save adopts the CURRENT persisted rows as the base,
    // which reproduces the old two-way diff exactly. Safe only for callers
    // that just created or just re-read the technique (the library's create
    // flow); editors must use saveTechniqueWithBase — see the interface.
    final r = await _save(technique, base: null);
    return switch (r) {
      Ok<Loaded<Technique>>(:final value) => Result.ok(value.aggregate),
      Error<Loaded<Technique>>(:final error) => Result.error(error),
    };
  }

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique technique, {
    required Map<String, String> baseHashes,
  }) => _save(technique, base: baseHashes);

  /// The one save core. [base] is the per-row content-hash map captured when
  /// the caller loaded the technique (three-way merge); null means "adopt the
  /// current persisted state as base" (the legacy two-way diff).
  ///
  /// The WHOLE import+commit sequence runs under the store write gate's
  /// shared side: asset imports land before the media rows referencing them
  /// commit, and a maintenance pass (Persistence.repairLibrary / the boot
  /// sweeps, which hold the gate exclusively) running in that window would
  /// tombstone the just-imported asset and trash its pool file.
  Future<Result<Loaded<Technique>>> _save(
    Technique technique, {
    required Map<String, String>? base,
  }) => assets.gate.shared(() async {
    try {
      // Assets first (outside the row transaction, idempotent + deduped): a
      // dependent media row never exists without its asset.
      final mediaAssets = <String>[]; // assetId per media, by index
      for (final m in technique.media) {
        mediaAssets.add(
          await assets.resolveOrImport(
            m.path,
            kind: m.isVideo ? 'video' : 'audio',
          ),
        );
      }

      final aliasesJson = jsonEncode(technique.aliases);
      final tagsJson = jsonEncode(technique.tags);
      final now = _ms(DateTime.now());

      await db.transaction(() async {
        // --- the technique row (single-entity reconcile) ---
        final existing = await (db.select(
          db.techniques,
        )..where((t) => t.id.equals(technique.id))).getSingleOrNull();
        if (existing != null && existing.deletedAt != null) {
          // The technique was tombstoned before this save landed (a save
          // racing deleteTechnique, or a synced-in delete). Continuing would
          // NOT revive the row (the update below never clears deleted_at) yet
          // would still commit LIVE media rows under the tombstoned parent — a
          // journaled zombie subtree. Same policy as savePractice: a deleted
          // aggregate must never be implicitly re-saved; abort so the whole
          // transaction rolls back and the caller surfaces Result.error.
          throw StateError(
            'technique ${technique.id} is deleted; refusing to save over it',
          );
        }
        final desiredHash = techniqueContentHash(
          name: technique.name,
          aliasesJson: aliasesJson,
          tagsJson: tagsJson,
          summary: technique.summary,
          description: technique.description,
          cues: technique.cues,
          exercises: technique.exercises,
          acousticSignature: technique.acousticSignature,
        );
        // Tracks whether the technique ROW itself was written, so a media-only
        // edit (row hash unchanged) can still bump updatedAt for list ordering.
        var techniqueWritten = false;

        if (existing == null) {
          final s = writer.stamp();
          techniqueWritten = true;
          await db
              .into(db.techniques)
              .insert(
                TechniquesCompanion.insert(
                  id: technique.id,
                  name: Value(technique.name),
                  aliasesJson: Value(aliasesJson),
                  tagsJson: Value(tagsJson),
                  summary: Value(technique.summary),
                  description: Value(technique.description),
                  cues: Value(technique.cues),
                  exercises: Value(technique.exercises),
                  acousticSignature: Value(technique.acousticSignature),
                  createdAt: _ms(technique.createdAt),
                  updatedAt: _ms(technique.updatedAt),
                  hlcWall: s.hlc.wallMs,
                  hlcCtr: s.hlc.counter,
                  hlcNode: s.hlc.node,
                  hlcPacked: s.hlc.packed,
                ),
              );
          await writer.journal(
            table: 'techniques',
            id: technique.id,
            op: 'insert',
            hlc: s.hlc,
            payloadHash: desiredHash,
          );
        } else {
          final existingHash = techniqueRowHash(existing);
          // Three-way merge on the technique row (same rules as
          // reconcileCollection). A null [base] adopts the current row as base
          // (legacy two-way — the skip below then never fires); a with-base
          // map missing this id degrades to conflict-local-wins.
          final baseHash = base == null ? existingHash : base[technique.id];
          final keepTheirs =
              existingHash != desiredHash &&
              baseHash != null &&
              existingHash != baseHash &&
              desiredHash == baseHash;
          // keepTheirs: the persisted row moved on since this editor loaded it
          // (a remote/newer write) and the editor never touched the technique
          // fields — keep the DB state; writing "mine" would revert the newer
          // row to stale values under a fresh HLC.
          if (existingHash != desiredHash && !keepTheirs) {
            // Plain local edit, or a true conflict (both sides changed): the
            // ACTIVE EDITOR wins by policy and mine is written with a fresh
            // HLC, so it also wins entity-level LWW downstream.
            final s = writer.stamp();
            techniqueWritten = true;
            await (db.update(
              db.techniques,
            )..where((t) => t.id.equals(technique.id))).write(
              TechniquesCompanion(
                name: Value(technique.name),
                aliasesJson: Value(aliasesJson),
                tagsJson: Value(tagsJson),
                summary: Value(technique.summary),
                description: Value(technique.description),
                cues: Value(technique.cues),
                exercises: Value(technique.exercises),
                acousticSignature: Value(technique.acousticSignature),
                updatedAt: Value(now),
                hlcWall: Value(s.hlc.wallMs),
                hlcCtr: Value(s.hlc.counter),
                hlcNode: Value(s.hlc.node),
                hlcPacked: Value(s.hlc.packed),
              ),
            );
            await writer.journal(
              table: 'techniques',
              id: technique.id,
              op: 'update',
              hlc: s.hlc,
              payloadHash: desiredHash,
            );
          }
        }

        // --- the media collection ---
        final liveMedia =
            await (db.select(db.techniqueMedia)..where(
                  (m) =>
                      m.techniqueId.equals(technique.id) & m.deletedAt.isNull(),
                ))
                .get();
        final currentHashes = {
          for (final m in liveMedia) m.id: techniqueMediaRowHash(m),
        };
        final desired = [
          for (var i = 0; i < technique.media.length; i++)
            (media: technique.media[i], assetId: mediaAssets[i], order: i),
        ];
        final mediaChanges = (await reconcileCollection(
          writer: writer,
          table: 'technique_media',
          // Null base adopts the current rows (legacy two-way); see
          // saveTechnique.
          baseHashes: base ?? currentHashes,
          currentHashes: currentHashes,
          desired: desired,
          idOf: (d) => d.media.id,
          hashOf: (d) => techniqueMediaContentHash(
            assetId: d.assetId,
            kind: d.media.isVideo ? 'video' : 'audio',
            label: d.media.label,
            orderIndex: d.order,
          ),
          insert: (d, s) => db
              .into(db.techniqueMedia)
              .insertOnConflictUpdate(
                TechniqueMediaCompanion.insert(
                  id: d.media.id,
                  // Revive (clear the tombstone) if this id was previously
                  // soft-deleted, so a reused ULID upserts instead of a PK crash.
                  deletedAt: const Value(null),
                  techniqueId: technique.id,
                  assetId: d.assetId,
                  kind: Value(d.media.isVideo ? 'video' : 'audio'),
                  label: Value(d.media.label),
                  orderIndex: Value(d.order),
                  createdAt: _ms(d.media.addedAt),
                  updatedAt: now,
                  hlcWall: s.hlc.wallMs,
                  hlcCtr: s.hlc.counter,
                  hlcNode: s.hlc.node,
                  hlcPacked: s.hlc.packed,
                ),
              ),
          update: (d, s) =>
              (db.update(
                db.techniqueMedia,
              )..where((m) => m.id.equals(d.media.id))).write(
                TechniqueMediaCompanion(
                  assetId: Value(d.assetId),
                  kind: Value(d.media.isVideo ? 'video' : 'audio'),
                  label: Value(d.media.label),
                  orderIndex: Value(d.order),
                  updatedAt: Value(now),
                  hlcWall: Value(s.hlc.wallMs),
                  hlcCtr: Value(s.hlc.counter),
                  hlcNode: Value(s.hlc.node),
                  hlcPacked: Value(s.hlc.packed),
                ),
              ),
          tombstone: (id, s) =>
              (db.update(
                db.techniqueMedia,
              )..where((m) => m.id.equals(id))).write(
                TechniqueMediaCompanion(
                  deletedAt: Value(now),
                  updatedAt: Value(now),
                  hlcWall: Value(s.hlc.wallMs),
                  hlcCtr: Value(s.hlc.counter),
                  hlcNode: Value(s.hlc.node),
                  hlcPacked: Value(s.hlc.packed),
                ),
              ),
        )).total;

        // A media-only edit (add/remove/reorder/relabel an example) leaves the
        // technique row's hash unchanged, so the row UPDATE above is skipped and
        // updatedAt would stay stale — mis-ordering the just-edited technique in
        // the updatedAt-sorted library list. Touch updatedAt locally (plain drift
        // update, NO journal / HLC tick) so the row rises, mirroring savePractice.
        if (!techniqueWritten && mediaChanges > 0) {
          await (db.update(db.techniques)
                ..where((t) => t.id.equals(technique.id)))
              .write(TechniquesCompanion(updatedAt: Value(now)));
        }
      });

      // Return the saved copy with resolved (pooled) media paths, plus fresh
      // base hashes so the caller's next save merges against what this save
      // actually left in the store.
      return loadTechnique(technique.id);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  });

  @override
  Future<Result<void>> deleteTechnique(String id) async {
    try {
      await db.transaction(() async {
        final media = await (db.select(
          db.techniqueMedia,
        )..where((m) => m.techniqueId.equals(id) & m.deletedAt.isNull())).get();
        final now = _ms(DateTime.now());
        for (final m in media) {
          final s = writer.stamp();
          await (db.update(
            db.techniqueMedia,
          )..where((x) => x.id.equals(m.id))).write(
            TechniqueMediaCompanion(
              deletedAt: Value(now),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          );
          await writer.journal(
            table: 'technique_media',
            id: m.id,
            op: 'tombstone',
            hlc: s.hlc,
            payloadHash: '',
          );
        }
        final s = writer.stamp();
        await (db.update(db.techniques)..where((t) => t.id.equals(id))).write(
          TechniquesCompanion(
            deletedAt: Value(now),
            updatedAt: Value(now),
            hlcWall: Value(s.hlc.wallMs),
            hlcCtr: Value(s.hlc.counter),
            hlcNode: Value(s.hlc.node),
            hlcPacked: Value(s.hlc.packed),
          ),
        );
        await writer.journal(
          table: 'techniques',
          id: id,
          op: 'tombstone',
          hlc: s.hlc,
          payloadHash: '',
        );
      });
      return const Result.ok(null);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  /// Builds a domain [Technique] from a row + its live media (paths resolved).
  /// When [baseHashes] is given, the content hash of every row this read
  /// returns is recorded — the base side of a later three-way save, hashed
  /// with the SAME sync_hashes.dart projections the save path uses.
  Future<Technique> _hydrate(
    TechniqueRow row, {
    _MediaRows? media,
    Map<String, String>? baseHashes,
  }) async {
    baseHashes?[row.id] = techniqueRowHash(row);
    final read = media ?? await _readMedia([row.id]);
    final out = <TechniqueMedia>[];
    for (final m in read.media[row.id] ?? const <TechniqueMediaRow>[]) {
      baseHashes?[m.id] = techniqueMediaRowHash(m);
      out.add(
        TechniqueMedia(
          id: m.id,
          // Keep the media even if its bytes are missing locally (carry an
          // `asset://` ref) so a reload + save never drops the example.
          path: read.paths[m.assetId] ?? AssetStore.refFor(m.assetId),
          kind: m.kind == 'video'
              ? TechniqueMediaKind.video
              : TechniqueMediaKind.audio,
          label: m.label,
          addedAt: _dt(m.createdAt),
        ),
      );
    }
    return Technique(
      id: row.id,
      name: row.name,
      aliases: (jsonDecode(row.aliasesJson ?? '[]') as List).cast<String>(),
      tags: (jsonDecode(row.tagsJson ?? '[]') as List).cast<String>(),
      summary: row.summary,
      description: row.description,
      cues: row.cues,
      exercises: row.exercises,
      acousticSignature: row.acousticSignature,
      media: out,
      createdAt: _dt(row.createdAt),
      updatedAt: _dt(row.updatedAt),
    );
  }

  /// The live media of [techniqueIds] grouped by technique, each group in
  /// (order_index, id) order — stable tie-break for equal indices
  /// (concurrent offline inserts): ULID ids order by creation time, so every
  /// replica agrees — plus each media asset's local path (absent = bytes not
  /// here). Asset rows are read live or tombstoned, like [AssetStore.resolve].
  Future<_MediaRows> _readMedia(List<String> techniqueIds) async {
    final rows = <TechniqueMediaRow>[];
    for (final chunk in AssetStore.inChunks(techniqueIds)) {
      rows.addAll(
        await (db.select(db.techniqueMedia)
              ..where((m) => m.techniqueId.isIn(chunk) & m.deletedAt.isNull())
              ..orderBy([
                (m) => OrderingTerm(expression: m.techniqueId),
                (m) => OrderingTerm(expression: m.orderIndex),
                (m) => OrderingTerm(expression: m.id),
              ]))
            .get(),
      );
    }
    final assetRows = <AssetRow>[];
    for (final chunk in AssetStore.inChunks(
      {for (final m in rows) m.assetId}.toList(),
    )) {
      assetRows.addAll(
        await (db.select(db.assets)..where((a) => a.id.isIn(chunk))).get(),
      );
    }
    final paths = <String, String>{};
    await Future.wait([
      for (final a in assetRows)
        assets.resolveRow(a).then((path) {
          if (path != null) paths[a.id] = path;
        }),
    ]);
    final media = <String, List<TechniqueMediaRow>>{};
    for (final m in rows) {
      (media[m.techniqueId] ??= <TechniqueMediaRow>[]).add(m);
    }
    return _MediaRows(media: media, paths: paths);
  }
}

/// [TechniqueRepositoryDrift._readMedia]'s result.
class _MediaRows {
  const _MediaRows({required this.media, required this.paths});

  final Map<String, List<TechniqueMediaRow>> media;
  final Map<String, String> paths;
}
