import 'dart:convert';
import 'dart:typed_data';

import 'package:drift/drift.dart';

import '../../domain/models/audio_fx.dart';
import '../../domain/models/audio_track.dart';
import '../../domain/models/fx_bus.dart';
import '../../domain/models/lyric_line.dart';
import '../../domain/models/practice_note.dart';
import '../../domain/models/song_practice.dart';
import '../../domain/models/take_version.dart';
import '../../domain/models/technique_marker.dart';
import '../../utils/id.dart';
import '../../utils/result.dart';
import '../db/app_database.dart';
import '../db/asset_store.dart';
import '../sync/reconcile.dart';
import '../sync/sync_writer.dart';
import 'song_practice_repository.dart';

/// Analysis-code version stamped on cached waveform peaks (a pure function of
/// the immutable audio bytes; bump to invalidate after a peaks algorithm change).
const int _waveformAlgoVersion = 1;

/// drift/SQLite implementation of the song-practice store. The full aggregate
/// graph (practice -> interpretations / tracks -> clips / notes / markers,
/// fx buses) is normalized into rows; atomic config (a track/bus FX chain, its
/// sends, the practice reverb + lyric vector) rides as JSON columns. Audio is
/// content-addressed in the [AssetStore]; waveform peaks are cached locally by
/// content hash (regenerable, never synced). Every write goes through the
/// [SyncWriter] chokepoint + per-entity [reconcileCollection].
class SongPracticeRepositoryDrift implements SongPracticeRepository {
  SongPracticeRepositoryDrift({
    required this.db,
    required this.writer,
    required this.assets,
  });

  final AppDatabase db;
  final SyncWriter writer;
  final AssetStore assets;

  static int _ms(DateTime d) => d.millisecondsSinceEpoch;
  static DateTime _dt(int ms) => DateTime.fromMillisecondsSinceEpoch(ms);

  // --- read -----------------------------------------------------------------

  @override
  Future<Result<List<SongPractice>>> listPractices() async {
    try {
      final rows = await (db.select(
        db.practices,
      )..where((p) => p.deletedAt.isNull())).get();
      // Deterministic order: updatedAt desc, then id desc as a total-order
      // tiebreaker so rows sharing a millisecond don't reshuffle between launches
      // (Dart's List.sort is not stable).
      rows.sort((a, b) {
        final c = b.updatedAt.compareTo(a.updatedAt);
        return c != 0 ? c : b.id.compareTo(a.id);
      });
      final out = <SongPractice>[];
      for (final r in rows) {
        try {
          out.add(await _hydrate(r));
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
  Future<Result<SongPractice>> getPractice(String id) async {
    try {
      final row =
          await (db.select(db.practices)
                ..where((p) => p.id.equals(id) & p.deletedAt.isNull()))
              .getSingleOrNull();
      if (row == null) {
        return Result.error(Exception('Practice "$id" not found'));
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
  Future<Result<SongPractice>> createPractice(String title) async {
    final now = DateTime.now();
    final trimmed = title.trim();
    final firstVersion = TakeVersion(
      id: newId(),
      name: 'Interpretation 1',
      createdAt: now,
    );
    final practice = SongPractice(
      id: newId(),
      title: trimmed.isEmpty ? 'Untitled practice' : trimmed,
      versions: [firstVersion],
      activeVersionId: firstVersion.id,
      createdAt: now,
      updatedAt: now,
    );
    return savePractice(practice);
  }

  @override
  Future<Result<void>> deletePractice(String id) async {
    try {
      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');
        final now = _ms(DateTime.now());
        // Cascade-tombstone the whole subtree (children before parents is not
        // required for soft-delete, but we tombstone everything live).
        final trackRows = await (db.select(
          db.tracks,
        )..where((t) => t.practiceId.equals(id) & t.deletedAt.isNull())).get();
        for (final t in trackRows) {
          await _cascadeTombstoneTrack(t.id, now);
        }
        for (final bus in await _live(
          db.fxBuses,
          (b) => b.practiceId.equals(id),
        )) {
          await _tombstone(db.fxBuses, 'fx_buses', bus.id, now);
        }
        for (final iv in await _live(
          db.interpretations,
          (x) => x.practiceId.equals(id),
        )) {
          await _tombstone(db.interpretations, 'interpretations', iv.id, now);
        }
        await _tombstone(db.practices, 'practices', id, now);
      });
      return const Result.ok(null);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  // --- write ----------------------------------------------------------------

  @override
  Future<Result<SongPractice>> savePractice(SongPractice practice) async {
    try {
      // All tracks flattened (accompaniment + reference + vocals).
      final allTracks = <AudioTrack>[
        if (practice.accompaniment != null) practice.accompaniment!,
        if (practice.reference != null) practice.reference!,
        ...practice.vocalTracks,
      ];

      // Assets first (idempotent, deduped): every clip's audio + the separation
      // source. A dependent clip row never exists without its asset.
      final clipAsset = <String, String>{}; // clipId -> assetId
      for (final t in allTracks) {
        for (final c in t.clips) {
          // resolveOrImport: an `asset://` ref (bytes missing locally) keeps its
          // id without re-importing; a real path is imported + deduped.
          final assetId = await assets.resolveOrImport(c.audioFilePath);
          clipAsset[c.id] = assetId;
          await _cacheWaveform(assetId, c.waveformPeaks);
        }
      }
      String? separationAssetId;
      if (practice.separationSource != null &&
          practice.separationSource!.isNotEmpty) {
        separationAssetId = await assets.resolveOrImport(
          practice.separationSource!,
        );
      }

      final now = _ms(DateTime.now());
      final reverbJson = practice.reverb == null
          ? null
          : jsonEncode(practice.reverb!.toJson());
      final lyricsJson = jsonEncode(
        practice.lyrics.map((l) => l.toJson()).toList(),
      );

      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');

        // --- practice row (single-entity reconcile) ---
        final practiceWritten = await _upsertPractice(
          practice,
          separationAssetId: separationAssetId,
          reverbJson: reverbJson,
          lyricsJson: lyricsJson,
          now: now,
        );

        // --- interpretations ---
        var childChanges = await _reconcileInterpretations(practice, now);

        // --- fx buses (before tracks so a track's bus_id resolves) ---
        childChanges += await _reconcileFxBuses(practice, now);

        // --- tracks (+ cascade their clips / notes / markers) ---
        childChanges += await _reconcileTracks(
          practice,
          allTracks,
          clipAsset,
          now,
        );

        // A child-only edit (clip move, note/marker, fader, add/remove an
        // interpretation or FX bus) updates its own row but leaves the practice
        // row's updatedAt stale, so the home list (ordered by updatedAt) wouldn't
        // surface the just-edited practice. Touch updatedAt to the children's true
        // modification time — local ordering ONLY: no HLC tick and no oplog entry
        // (those would manufacture a phantom sync op for a row that didn't change).
        if (!practiceWritten && childChanges > 0) {
          await (db.update(db.practices)
                ..where((x) => x.id.equals(practice.id)))
              .write(PracticesCompanion(updatedAt: Value(now)));
        }
      });

      return getPractice(practice.id);
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

  // --- write helpers --------------------------------------------------------

  /// Writes the practice-level row. Returns whether the row was actually written
  /// (insert or content change) — false on the no-change early return, so the
  /// caller knows whether updatedAt was already bumped here.
  Future<bool> _upsertPractice(
    SongPractice p, {
    required String? separationAssetId,
    required String? reverbJson,
    required String lyricsJson,
    required int now,
  }) async {
    final existing = await (db.select(
      db.practices,
    )..where((x) => x.id.equals(p.id))).getSingleOrNull();
    if (existing != null && existing.deletedAt != null) {
      // The practice was tombstoned before this save landed (e.g. an in-flight
      // save racing deletePractice). Continuing would NOT revive the practice
      // (the update below never clears deleted_at) yet would still commit LIVE
      // child rows under the tombstoned parent — a journaled zombie subtree.
      // A deleted aggregate must not be implicitly re-saved: abort the whole
      // save (the transaction rolls back, no children are written) so the
      // caller surfaces Result.error, like the pre-upsert-revive PK failure.
      throw StateError('practice ${p.id} is deleted; refusing to save over it');
    }
    final hash = SyncWriter.contentHashOf([
      p.title,
      p.timelineDurationMs,
      p.masterVolume,
      p.activeVersionId,
      separationAssetId,
      reverbJson,
      lyricsJson,
    ]);
    if (existing == null) {
      final s = writer.stamp();
      await db
          .into(db.practices)
          .insert(
            PracticesCompanion.insert(
              id: p.id,
              title: Value(p.title),
              timelineDurationMs: Value(p.timelineDurationMs),
              masterVolume: Value(p.masterVolume),
              activeInterpretationId: Value(p.activeVersionId),
              separationAssetId: Value(separationAssetId),
              reverbJson: Value(reverbJson),
              lyricsJson: Value(lyricsJson),
              createdAt: _ms(p.createdAt),
              updatedAt: _ms(p.updatedAt),
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          );
      await writer.journal(
        table: 'practices',
        id: p.id,
        op: 'insert',
        hlc: s.hlc,
        payloadHash: hash,
      );
    } else {
      final existingHash = SyncWriter.contentHashOf([
        existing.title,
        existing.timelineDurationMs,
        existing.masterVolume,
        existing.activeInterpretationId,
        existing.separationAssetId,
        existing.reverbJson,
        existing.lyricsJson,
      ]);
      if (existingHash == hash) return false;
      final s = writer.stamp();
      await (db.update(db.practices)..where((x) => x.id.equals(p.id))).write(
        PracticesCompanion(
          title: Value(p.title),
          timelineDurationMs: Value(p.timelineDurationMs),
          masterVolume: Value(p.masterVolume),
          activeInterpretationId: Value(p.activeVersionId),
          separationAssetId: Value(separationAssetId),
          reverbJson: Value(reverbJson),
          lyricsJson: Value(lyricsJson),
          updatedAt: Value(now),
          hlcWall: Value(s.hlc.wallMs),
          hlcCtr: Value(s.hlc.counter),
          hlcNode: Value(s.hlc.node),
          hlcPacked: Value(s.hlc.packed),
        ),
      );
      await writer.journal(
        table: 'practices',
        id: p.id,
        op: 'update',
        hlc: s.hlc,
        payloadHash: hash,
      );
    }
    return true;
  }

  Future<int> _reconcileInterpretations(SongPractice p, int now) async {
    final live = await _live(
      db.interpretations,
      (x) => x.practiceId.equals(p.id),
    );
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([r.name, r.orderIndex]),
    };
    final desired = [
      for (var i = 0; i < p.versions.length; i++) (v: p.versions[i], order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'interpretations',
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.v.id,
      hashOf: (d) => SyncWriter.contentHashOf([d.v.name, d.order]),
      insert: (d, s) => db
          .into(db.interpretations)
          .insertOnConflictUpdate(
            InterpretationsCompanion.insert(
              id: d.v.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              practiceId: p.id,
              name: Value(d.v.name),
              orderIndex: Value(d.order),
              createdAt: _ms(d.v.createdAt),
              updatedAt: now,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (d, s) =>
          (db.update(
            db.interpretations,
          )..where((x) => x.id.equals(d.v.id))).write(
            InterpretationsCompanion(
              name: Value(d.v.name),
              orderIndex: Value(d.order),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _tombstoneRow(db.interpretations, id, now, s),
    )).total;
  }

  Future<int> _reconcileFxBuses(SongPractice p, int now) async {
    final live = await _live(db.fxBuses, (x) => x.practiceId.equals(p.id));
    String busHash(FxBus b, int order) => SyncWriter.contentHashOf([
      b.name,
      b.colorValue,
      b.volume,
      b.pan,
      b.muted,
      b.soloed,
      b.outputBusId,
      jsonEncode(b.fx.toJson()),
      jsonEncode(b.sends.map((s) => s.toJson()).toList()),
      order,
    ]);
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([
          r.name,
          r.colorValue,
          r.volume,
          r.pan,
          r.muted,
          r.soloed,
          r.outputBusId,
          r.fxJson ?? '',
          r.sendsJson ?? '[]',
          r.orderIndex,
        ]),
    };
    final desired = [
      for (var i = 0; i < p.fxBuses.length; i++) (bus: p.fxBuses[i], order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'fx_buses',
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.bus.id,
      hashOf: (d) => busHash(d.bus, d.order),
      insert: (d, s) => db
          .into(db.fxBuses)
          .insertOnConflictUpdate(
            FxBusesCompanion.insert(
              id: d.bus.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              practiceId: p.id,
              name: Value(d.bus.name),
              colorValue: Value(d.bus.colorValue),
              volume: Value(d.bus.volume),
              pan: Value(d.bus.pan),
              muted: Value(d.bus.muted),
              soloed: Value(d.bus.soloed),
              outputBusId: Value(d.bus.outputBusId),
              fxJson: Value(jsonEncode(d.bus.fx.toJson())),
              sendsJson: Value(
                jsonEncode(d.bus.sends.map((s) => s.toJson()).toList()),
              ),
              orderIndex: Value(d.order),
              createdAt: now,
              updatedAt: now,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (d, s) =>
          (db.update(db.fxBuses)..where((x) => x.id.equals(d.bus.id))).write(
            FxBusesCompanion(
              name: Value(d.bus.name),
              colorValue: Value(d.bus.colorValue),
              volume: Value(d.bus.volume),
              pan: Value(d.bus.pan),
              muted: Value(d.bus.muted),
              soloed: Value(d.bus.soloed),
              outputBusId: Value(d.bus.outputBusId),
              fxJson: Value(jsonEncode(d.bus.fx.toJson())),
              sendsJson: Value(
                jsonEncode(d.bus.sends.map((s) => s.toJson()).toList()),
              ),
              orderIndex: Value(d.order),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _tombstoneRow(db.fxBuses, id, now, s),
    )).total;
  }

  Future<int> _reconcileTracks(
    SongPractice p,
    List<AudioTrack> tracks,
    Map<String, String> clipAsset,
    int now,
  ) async {
    final live = await _live(db.tracks, (x) => x.practiceId.equals(p.id));
    String trackHash(AudioTrack t, int order) => SyncWriter.contentHashOf([
      t.kind.name,
      t.versionId,
      t.displayName,
      t.colorValue,
      t.muted,
      t.soloed,
      t.volume,
      t.pan,
      t.busId,
      t.fx == null ? null : jsonEncode(t.fx!.toJson()),
      jsonEncode(t.sends.map((s) => s.toJson()).toList()),
      order,
    ]);
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([
          r.kind,
          r.interpretationId,
          r.displayName,
          r.colorValue,
          r.muted,
          r.soloed,
          r.volume,
          r.pan,
          r.busId,
          r.fxJson,
          r.sendsJson ?? '[]',
          r.orderIndex,
        ]),
    };
    final desired = [
      for (var i = 0; i < tracks.length; i++) (track: tracks[i], order: i),
    ];
    var changed = (await reconcileCollection(
      writer: writer,
      table: 'tracks',
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.track.id,
      hashOf: (d) => trackHash(d.track, d.order),
      insert: (d, s) => db
          .into(db.tracks)
          .insertOnConflictUpdate(
            TracksCompanion.insert(
              id: d.track.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              practiceId: p.id,
              kind: d.track.kind.name,
              interpretationId: Value(d.track.versionId),
              displayName: Value(d.track.displayName),
              colorValue: Value(d.track.colorValue),
              muted: Value(d.track.muted),
              soloed: Value(d.track.soloed),
              volume: Value(d.track.volume),
              pan: Value(d.track.pan),
              busId: Value(d.track.busId),
              fxJson: Value(
                d.track.fx == null ? null : jsonEncode(d.track.fx!.toJson()),
              ),
              sendsJson: Value(
                jsonEncode(d.track.sends.map((s) => s.toJson()).toList()),
              ),
              orderIndex: Value(d.order),
              createdAt: _ms(d.track.createdAt),
              updatedAt: now,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (d, s) =>
          (db.update(db.tracks)..where((x) => x.id.equals(d.track.id))).write(
            TracksCompanion(
              kind: Value(d.track.kind.name),
              interpretationId: Value(d.track.versionId),
              displayName: Value(d.track.displayName),
              colorValue: Value(d.track.colorValue),
              muted: Value(d.track.muted),
              soloed: Value(d.track.soloed),
              volume: Value(d.track.volume),
              pan: Value(d.track.pan),
              busId: Value(d.track.busId),
              fxJson: Value(
                d.track.fx == null ? null : jsonEncode(d.track.fx!.toJson()),
              ),
              sendsJson: Value(
                jsonEncode(d.track.sends.map((s) => s.toJson()).toList()),
              ),
              orderIndex: Value(d.order),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _cascadeTombstoneTrack(id, now, rowStamp: s),
    )).total;

    // Children of every surviving track.
    for (final t in tracks) {
      changed += await _reconcileClips(t, clipAsset, now);
      changed += await _reconcileNotes(t, now);
      changed += await _reconcileMarkers(t, now);
    }
    return changed;
  }

  Future<int> _reconcileClips(
    AudioTrack t,
    Map<String, String> clipAsset,
    int now,
  ) async {
    final live = await _live(db.clips, (x) => x.trackId.equals(t.id));
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([
          r.assetId,
          r.startMs,
          r.durationMs,
          r.orderIndex,
        ]),
    };
    final desired = [
      for (var i = 0; i < t.clips.length; i++)
        (clip: t.clips[i], assetId: clipAsset[t.clips[i].id]!, order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'clips',
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.clip.id,
      hashOf: (d) => SyncWriter.contentHashOf([
        d.assetId,
        d.clip.startMs,
        d.clip.durationMs,
        d.order,
      ]),
      insert: (d, s) => db
          .into(db.clips)
          .insertOnConflictUpdate(
            ClipsCompanion.insert(
              id: d.clip.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              trackId: t.id,
              assetId: d.assetId,
              startMs: Value(d.clip.startMs),
              durationMs: Value(d.clip.durationMs),
              orderIndex: Value(d.order),
              createdAt: now,
              updatedAt: now,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (d, s) =>
          (db.update(db.clips)..where((x) => x.id.equals(d.clip.id))).write(
            ClipsCompanion(
              assetId: Value(d.assetId),
              startMs: Value(d.clip.startMs),
              durationMs: Value(d.clip.durationMs),
              orderIndex: Value(d.order),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _tombstoneRow(db.clips, id, now, s),
    )).total;
  }

  Future<int> _reconcileNotes(AudioTrack t, int now) async {
    final live = await _live(db.practiceNotes, (x) => x.trackId.equals(t.id));
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([r.positionMs, r.endMs, r.content]),
    };
    return (await reconcileCollection(
      writer: writer,
      table: 'practice_notes',
      currentHashes: current,
      desired: t.notes,
      idOf: (n) => n.id,
      hashOf: (n) =>
          SyncWriter.contentHashOf([n.positionMs, n.endMs, n.content]),
      insert: (n, s) => db
          .into(db.practiceNotes)
          .insertOnConflictUpdate(
            PracticeNotesCompanion.insert(
              id: n.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              trackId: t.id,
              positionMs: Value(n.positionMs),
              endMs: Value(n.endMs),
              content: Value(n.content),
              createdAt: _ms(n.createdAt),
              updatedAt: _ms(n.updatedAt),
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (n, s) =>
          (db.update(db.practiceNotes)..where((x) => x.id.equals(n.id))).write(
            PracticeNotesCompanion(
              positionMs: Value(n.positionMs),
              endMs: Value(n.endMs),
              content: Value(n.content),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _tombstoneRow(db.practiceNotes, id, now, s),
    )).total;
  }

  Future<int> _reconcileMarkers(AudioTrack t, int now) async {
    final live = await _live(
      db.techniqueMarkers,
      (x) => x.trackId.equals(t.id),
    );
    final current = {
      for (final r in live)
        r.id: SyncWriter.contentHashOf([
          r.positionMs,
          r.endMs,
          r.techniqueId,
          r.techniqueNameSnapshot,
        ]),
    };
    return (await reconcileCollection(
      writer: writer,
      table: 'technique_markers',
      currentHashes: current,
      desired: t.techniqueMarkers,
      idOf: (m) => m.id,
      hashOf: (m) => SyncWriter.contentHashOf([
        m.positionMs,
        m.endMs,
        m.techniqueId,
        m.techniqueName,
      ]),
      insert: (m, s) => db
          .into(db.techniqueMarkers)
          .insertOnConflictUpdate(
            TechniqueMarkersCompanion.insert(
              id: m.id,
              // Revive (clear the tombstone) if this id was previously
              // soft-deleted, so a reused ULID upserts instead of a PK crash.
              deletedAt: const Value(null),
              trackId: t.id,
              positionMs: Value(m.positionMs),
              endMs: Value(m.endMs),
              techniqueId: m.techniqueId,
              techniqueNameSnapshot: m.techniqueName,
              createdAt: _ms(m.createdAt),
              updatedAt: _ms(m.updatedAt),
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          ),
      update: (m, s) =>
          (db.update(
            db.techniqueMarkers,
          )..where((x) => x.id.equals(m.id))).write(
            TechniqueMarkersCompanion(
              positionMs: Value(m.positionMs),
              endMs: Value(m.endMs),
              techniqueId: Value(m.techniqueId),
              techniqueNameSnapshot: Value(m.techniqueName),
              updatedAt: Value(now),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            ),
          ),
      tombstone: (id, s) => _tombstoneRow(db.techniqueMarkers, id, now, s),
    )).total;
  }

  /// Tombstones a track and its live clips / notes / markers (a removed track
  /// must never leave live children dangling). The children are not separately
  /// reconciled (only surviving tracks' children are), so they self-journal.
  ///
  /// [rowStamp] is set ONLY when called from the tracks reconcile: there the
  /// track is the reconciled entity, so [reconcileCollection] emits its oplog
  /// entry and the row must take that exact stamp WITHOUT self-journaling (else
  /// the row gets a second, HLC-mismatched tombstone entry). On the standalone
  /// deletePractice path [rowStamp] is null and the track row self-journals.
  Future<void> _cascadeTombstoneTrack(
    String trackId,
    int now, {
    SyncStamp? rowStamp,
  }) async {
    for (final c in await _live(db.clips, (x) => x.trackId.equals(trackId))) {
      await _tombstone(db.clips, 'clips', c.id, now);
    }
    for (final n in await _live(
      db.practiceNotes,
      (x) => x.trackId.equals(trackId),
    )) {
      await _tombstone(db.practiceNotes, 'practice_notes', n.id, now);
    }
    for (final m in await _live(
      db.techniqueMarkers,
      (x) => x.trackId.equals(trackId),
    )) {
      await _tombstone(db.techniqueMarkers, 'technique_markers', m.id, now);
    }
    if (rowStamp != null) {
      await _tombstoneRow(db.tracks, trackId, now, rowStamp);
    } else {
      await _tombstone(db.tracks, 'tracks', trackId, now);
    }
  }

  // --- generic helpers ------------------------------------------------------

  /// Live (non-tombstoned) rows of [table] matching [filter].
  Future<List<D>> _live<T extends Table, D>(
    TableInfo<T, D> table,
    Expression<bool> Function(T) filter,
  ) {
    final q = db.select(table)..where((t) => filter(t) & _notDeleted(t));
    return q.get();
  }

  Expression<bool> _notDeleted(dynamic t) =>
      (t.deletedAt as GeneratedColumn<int>).isNull();

  /// Soft-deletes a row using a stamp MINTED BY THE CALLER and does NOT journal
  /// — for the reconcile path, where [reconcileCollection] emits the one oplog
  /// entry. Taking the caller's stamp keeps the row's HLC equal to that entry's
  /// (self-journaling here would add a second, HLC-mismatched tombstone op).
  Future<void> _tombstoneRow(
    TableInfo table,
    String id,
    int now,
    SyncStamp s,
  ) async {
    await db.customUpdate(
      'UPDATE ${table.actualTableName} SET deleted_at = ?, updated_at = ?, '
      'hlc_wall = ?, hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
      variables: [
        Variable.withInt(now),
        Variable.withInt(now),
        Variable.withInt(s.hlc.wallMs),
        Variable.withInt(s.hlc.counter),
        Variable.withString(s.hlc.node),
        Variable.withString(s.hlc.packed),
        Variable.withString(id),
      ],
      updates: {table},
    );
  }

  /// Soft-deletes a row + journals it. Works for any synced table via raw SQL
  /// so one helper covers them all.
  Future<void> _tombstone(
    TableInfo table,
    String name,
    String id,
    int now,
  ) async {
    final s = writer.stamp();
    await db.customUpdate(
      'UPDATE ${table.actualTableName} SET deleted_at = ?, updated_at = ?, '
      'hlc_wall = ?, hlc_ctr = ?, hlc_node = ?, hlc_packed = ? WHERE id = ?',
      variables: [
        Variable.withInt(now),
        Variable.withInt(now),
        Variable.withInt(s.hlc.wallMs),
        Variable.withInt(s.hlc.counter),
        Variable.withString(s.hlc.node),
        Variable.withString(s.hlc.packed),
        Variable.withString(id),
      ],
      updates: {table},
    );
    await writer.journal(
      table: name,
      id: id,
      op: 'tombstone',
      hlc: s.hlc,
      payloadHash: '',
    );
  }

  Future<void> _cacheWaveform(String assetId, List<double> peaks) async {
    if (peaks.isEmpty) return;
    final asset = await (db.select(
      db.assets,
    )..where((a) => a.id.equals(assetId))).getSingleOrNull();
    if (asset == null) return;
    await db
        .into(db.waveformCache)
        .insertOnConflictUpdate(
          WaveformCacheCompanion.insert(
            contentHash: asset.contentHash,
            algoVersion: _waveformAlgoVersion,
            peaks: _encodePeaks(peaks),
          ),
        );
  }

  // --- hydration ------------------------------------------------------------

  Future<SongPractice> _hydrate(PracticeRow row) async {
    final interpRows =
        await (db.select(db.interpretations)
              ..where((i) => i.practiceId.equals(row.id) & i.deletedAt.isNull())
              ..orderBy([(i) => OrderingTerm(expression: i.orderIndex)]))
            .get();
    final versions = [
      for (final i in interpRows)
        TakeVersion(id: i.id, name: i.name, createdAt: _dt(i.createdAt)),
    ];

    final trackRows =
        await (db.select(db.tracks)
              ..where((t) => t.practiceId.equals(row.id) & t.deletedAt.isNull())
              ..orderBy([(t) => OrderingTerm(expression: t.orderIndex)]))
            .get();
    AudioTrack? accompaniment;
    AudioTrack? reference;
    final vocals = <AudioTrack>[];
    for (final tr in trackRows) {
      final track = await _hydrateTrack(tr);
      switch (tr.kind) {
        case 'accompaniment':
          accompaniment ??= track;
        case 'reference':
          reference ??= track;
        default:
          vocals.add(track);
      }
    }

    final busRows =
        await (db.select(db.fxBuses)
              ..where((b) => b.practiceId.equals(row.id) & b.deletedAt.isNull())
              ..orderBy([(b) => OrderingTerm(expression: b.orderIndex)]))
            .get();
    final buses = [
      for (final b in busRows)
        FxBus(
          id: b.id,
          name: b.name,
          fx: b.fxJson == null
              ? TrackFx.flat()
              : TrackFx.fromJson(jsonDecode(b.fxJson!) as Map<String, dynamic>),
          colorValue: b.colorValue,
          volume: b.volume,
          muted: b.muted,
          soloed: b.soloed,
          pan: b.pan,
          outputBusId: b.outputBusId,
          sends: _sendsFrom(b.sendsJson),
        ),
    ];

    return SongPractice(
      id: row.id,
      title: row.title,
      createdAt: _dt(row.createdAt),
      updatedAt: _dt(row.updatedAt),
      accompaniment: accompaniment,
      reference: reference,
      vocalTracks: vocals,
      versions: versions,
      activeVersionId: row.activeInterpretationId,
      lyrics: _lyricsFrom(row.lyricsJson),
      timelineDurationMs: row.timelineDurationMs,
      separationSource: row.separationAssetId == null
          ? null
          : (await assets.resolve(row.separationAssetId!) ??
                AssetStore.refFor(row.separationAssetId!)),
      reverb: row.reverbJson == null
          ? null
          : ReverbSettings.fromJson(
              jsonDecode(row.reverbJson!) as Map<String, dynamic>,
            ),
      masterVolume: row.masterVolume,
      fxBuses: buses,
    );
  }

  Future<AudioTrack> _hydrateTrack(TrackRow tr) async {
    final clipRows =
        await (db.select(db.clips)
              ..where((c) => c.trackId.equals(tr.id) & c.deletedAt.isNull())
              ..orderBy([(c) => OrderingTerm(expression: c.orderIndex)]))
            .get();
    final clips = <AudioClip>[];
    for (final c in clipRows) {
      // Keep the clip even when its bytes are missing locally (carry an
      // `asset://` ref) so a reload never drops it — dropping it would make the
      // next save tombstone the row, losing the take permanently.
      final path =
          await assets.resolve(c.assetId) ?? AssetStore.refFor(c.assetId);
      clips.add(
        AudioClip(
          id: c.id,
          audioFilePath: path,
          durationMs: c.durationMs,
          startMs: c.startMs,
          waveformPeaks: await _waveformFor(c.assetId),
        ),
      );
    }

    final noteRows = await (db.select(
      db.practiceNotes,
    )..where((n) => n.trackId.equals(tr.id) & n.deletedAt.isNull())).get();
    final notes = [
      for (final n in noteRows)
        PracticeNote(
          id: n.id,
          positionMs: n.positionMs,
          endMs: n.endMs,
          content: n.content,
          createdAt: _dt(n.createdAt),
          updatedAt: _dt(n.updatedAt),
        ),
    ];

    final markerRows = await (db.select(
      db.techniqueMarkers,
    )..where((m) => m.trackId.equals(tr.id) & m.deletedAt.isNull())).get();
    final markers = [
      for (final m in markerRows)
        TechniqueMarker(
          id: m.id,
          positionMs: m.positionMs,
          endMs: m.endMs,
          techniqueId: m.techniqueId,
          techniqueName: m.techniqueNameSnapshot,
          createdAt: _dt(m.createdAt),
          updatedAt: _dt(m.updatedAt),
        ),
    ];

    return AudioTrack(
      id: tr.id,
      kind: AudioTrackKind.values.firstWhere(
        (k) => k.name == tr.kind,
        orElse: () => AudioTrackKind.vocal,
      ),
      displayName: tr.displayName,
      createdAt: _dt(tr.createdAt),
      clips: clips,
      notes: notes,
      techniqueMarkers: markers,
      fx: tr.fxJson == null
          ? null
          : TrackFx.fromJson(jsonDecode(tr.fxJson!) as Map<String, dynamic>),
      busId: tr.busId,
      sends: _sendsFrom(tr.sendsJson),
      versionId: tr.interpretationId,
      colorValue: tr.colorValue,
      muted: tr.muted,
      soloed: tr.soloed,
      volume: tr.volume,
      pan: tr.pan,
    );
  }

  Future<List<double>> _waveformFor(String assetId) async {
    final asset = await (db.select(
      db.assets,
    )..where((a) => a.id.equals(assetId))).getSingleOrNull();
    if (asset == null) return const [];
    final cache =
        await (db.select(db.waveformCache)..where(
              (w) =>
                  w.contentHash.equals(asset.contentHash) &
                  w.algoVersion.equals(_waveformAlgoVersion),
            ))
            .getSingleOrNull();
    return cache == null ? const [] : _decodePeaks(cache.peaks);
  }

  List<LyricLine> _lyricsFrom(String? json) {
    if (json == null) return const [];
    return [
      for (final m in jsonDecode(json) as List)
        LyricLine.fromJson(m as Map<String, dynamic>),
    ];
  }

  List<TrackSend> _sendsFrom(String? json) {
    if (json == null) return const [];
    return [
      for (final m in jsonDecode(json) as List)
        TrackSend.fromJson(m as Map<String, dynamic>),
    ];
  }

  static Uint8List _encodePeaks(List<double> peaks) =>
      Float32List.fromList(peaks).buffer.asUint8List();

  static List<double> _decodePeaks(Uint8List bytes) {
    final copy = Uint8List.fromList(bytes);
    return copy.buffer.asFloat32List(0, copy.lengthInBytes ~/ 4).toList();
  }
}
