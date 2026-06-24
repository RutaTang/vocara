import 'dart:convert';

import 'package:drift/drift.dart';

import '../../domain/models/technique.dart';
import '../../domain/models/technique_media.dart';
import '../../utils/result.dart';
import '../db/app_database.dart';
import '../db/asset_store.dart';
import '../sync/reconcile.dart';
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

  String _techniqueHash(
    String name,
    String aliasesJson,
    String tagsJson,
    String summary,
    String description,
    String cues,
    String exercises,
    String acoustic,
  ) => SyncWriter.contentHashOf([
    name,
    aliasesJson,
    tagsJson,
    summary,
    description,
    cues,
    exercises,
    acoustic,
  ]);

  String _mediaHash(String assetId, String kind, String label, int order) =>
      SyncWriter.contentHashOf([assetId, kind, label, order]);

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    try {
      final rows = await (db.select(
        db.techniques,
      )..where((t) => t.deletedAt.isNull())).get();
      rows.sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
      final out = <Technique>[];
      for (final r in rows) {
        out.add(await _hydrate(r));
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
    try {
      final row =
          await (db.select(db.techniques)
                ..where((t) => t.id.equals(id) & t.deletedAt.isNull()))
              .getSingleOrNull();
      if (row == null) {
        return Result.error(Exception('Technique "$id" not found'));
      }
      return Result.ok(await _hydrate(row));
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  @override
  Future<Result<Technique>> saveTechnique(Technique technique) async {
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
      var savedUpdatedAt = _ms(technique.updatedAt);

      await db.transaction(() async {
        // --- the technique row (single-entity reconcile) ---
        final existing = await (db.select(
          db.techniques,
        )..where((t) => t.id.equals(technique.id))).getSingleOrNull();
        final desiredHash = _techniqueHash(
          technique.name,
          aliasesJson,
          tagsJson,
          technique.summary,
          technique.description,
          technique.cues,
          technique.exercises,
          technique.acousticSignature,
        );
        // Tracks whether the technique ROW itself was written, so a media-only
        // edit (row hash unchanged) can still bump updatedAt for list ordering.
        var techniqueWritten = false;

        if (existing == null) {
          final s = writer.stamp();
          techniqueWritten = true;
          savedUpdatedAt = _ms(technique.updatedAt);
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
                  updatedAt: savedUpdatedAt,
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
          final existingHash = _techniqueHash(
            existing.name,
            existing.aliasesJson ?? '[]',
            existing.tagsJson ?? '[]',
            existing.summary,
            existing.description,
            existing.cues,
            existing.exercises,
            existing.acousticSignature,
          );
          if (existingHash != desiredHash) {
            final s = writer.stamp();
            techniqueWritten = true;
            savedUpdatedAt = now;
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
          } else {
            savedUpdatedAt = existing.updatedAt;
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
          for (final m in liveMedia)
            m.id: _mediaHash(m.assetId, m.kind, m.label, m.orderIndex),
        };
        final desired = [
          for (var i = 0; i < technique.media.length; i++)
            (media: technique.media[i], assetId: mediaAssets[i], order: i),
        ];
        final mediaChanges = (await reconcileCollection(
          writer: writer,
          table: 'technique_media',
          currentHashes: currentHashes,
          desired: desired,
          idOf: (d) => d.media.id,
          hashOf: (d) => _mediaHash(
            d.assetId,
            d.media.isVideo ? 'video' : 'audio',
            d.media.label,
            d.order,
          ),
          insert: (d, s) => db
              .into(db.techniqueMedia)
              .insert(
                TechniqueMediaCompanion.insert(
                  id: d.media.id,
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

      // Return the saved copy with resolved (pooled) media paths.
      return getTechnique(technique.id);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

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
  Future<Technique> _hydrate(TechniqueRow row) async {
    final mediaRows =
        await (db.select(db.techniqueMedia)
              ..where(
                (m) => m.techniqueId.equals(row.id) & m.deletedAt.isNull(),
              )
              ..orderBy([(m) => OrderingTerm(expression: m.orderIndex)]))
            .get();
    final media = <TechniqueMedia>[];
    for (final m in mediaRows) {
      // Keep the media even if its bytes are missing locally (carry an
      // `asset://` ref) so a reload + save never drops the example.
      final path =
          await assets.resolve(m.assetId) ?? AssetStore.refFor(m.assetId);
      media.add(
        TechniqueMedia(
          id: m.id,
          path: path,
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
      media: media,
      createdAt: _dt(row.createdAt),
      updatedAt: _dt(row.updatedAt),
    );
  }
}
