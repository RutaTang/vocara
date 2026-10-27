import 'dart:convert';
import 'dart:typed_data';

import 'package:drift/drift.dart';

import '../../domain/models/audio_fx.dart';
import '../../domain/models/audio_track.dart';
import '../../domain/models/fx_bus.dart';
import '../../domain/models/lyric_line.dart';
import '../../domain/models/practice_note.dart';
import '../../domain/models/practice_summary.dart';
import '../../domain/models/song_practice.dart';
import '../../domain/models/take_version.dart';
import '../../domain/models/technique_marker.dart';
import '../../utils/id.dart';
import '../../utils/result.dart';
import '../db/app_database.dart';
import '../db/asset_store.dart';
import '../db/tables.dart';
import '../services/diagnostics_log.dart';
import '../sync/reconcile.dart';
import '../sync/sync_hashes.dart';
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
  // Clamped to what a DateTime holds: a stored time past it (written by a
  // peer, or any build that let one through) threw for every list of the
  // library, which then showed nothing.
  static DateTime _dt(int ms) => DateTime.fromMillisecondsSinceEpoch(
    ms.clamp(-8640000000000000, 8640000000000000),
  );

  // A fader level and a pan as the faders reach them, as the JSON readers
  // clamp them: a stored level past the fader (a peer's) went to the engine
  // verbatim, and 1e30 is a gain the master's soft clip only squares off.
  static double _level(double v) => v.clamp(0.0, 1.0);
  static double _pan(double v) => v.clamp(-1.0, 1.0);

  // --- save-side content-hash projections -------------------------------------
  // The MINE side of every three-way merge, hashed once per save by
  // [_sentHashes]; the reconcile passes compare that very map, so "what a
  // save sent" can never drift from what its merge compared. Hydration
  // records the BASE side of the JSON-carrying rows through them too (see
  // [_hydrate]).

  /// The practice's JSON columns exactly as a save writes them.
  static String? _reverbJsonOf(SongPractice p) =>
      p.reverb == null ? null : jsonEncode(p.reverb!.toJson());
  static String _lyricsJsonOf(SongPractice p) =>
      jsonEncode(p.lyrics.map((l) => l.toJson()).toList());

  static String _practiceHash(
    SongPractice p, {
    required String? separationAssetId,
    required String? reverbJson,
    required String lyricsJson,
  }) => practiceContentHash(
    title: p.title,
    timelineDurationMs: p.timelineDurationMs,
    masterVolume: p.masterVolume,
    activeInterpretationId: p.activeVersionId,
    separationAssetId: separationAssetId,
    reverbJson: reverbJson,
    lyricsJson: lyricsJson,
  );

  static String _interpretationHash(TakeVersion v, int order) =>
      interpretationContentHash(name: v.name, orderIndex: order);

  static String _busHash(FxBus b, int order) => fxBusContentHash(
    name: b.name,
    colorValue: b.colorValue,
    volume: b.volume,
    pan: b.pan,
    muted: b.muted,
    soloed: b.soloed,
    outputBusId: b.outputBusId,
    fxJson: jsonEncode(b.fx.toJson()),
    sendsJson: jsonEncode(b.sends.map((s) => s.toJson()).toList()),
    orderIndex: order,
  );

  static String _trackHash(AudioTrack t, int order) => trackContentHash(
    kind: t.kind.name,
    interpretationId: t.versionId,
    displayName: t.displayName,
    colorValue: t.colorValue,
    muted: t.muted,
    soloed: t.soloed,
    volume: t.volume,
    pan: t.pan,
    busId: t.busId,
    fxJson: t.fx == null ? null : jsonEncode(t.fx!.toJson()),
    sendsJson: jsonEncode(t.sends.map((s) => s.toJson()).toList()),
    orderIndex: order,
  );

  /// Each of [tracks] (a save's flattened lanes) with the order_index it is
  /// written at: a vocal take's position among the vocal takes, 0 for the
  /// accompaniment and reference slots. Hydration orders only the takes by
  /// it (a slot is placed by its kind), and counting the slots in front of
  /// the takes renumbered — and so rewrote, under a fresh stamp — every take
  /// whenever a backing came or went. Indices an earlier build wrote with
  /// the slots in front still sort the takes the same way.
  static List<({AudioTrack track, int order})> _trackOrders(
    List<AudioTrack> tracks,
  ) {
    var take = 0;
    return [
      for (final t in tracks) (track: t, order: t.isBacking ? 0 : take++),
    ];
  }

  static String _clipHash(AudioClip c, String assetId, int order) =>
      clipContentHash(
        assetId: assetId,
        startMs: c.startMs,
        durationMs: c.durationMs,
        orderIndex: order,
      );

  static String _noteHash(PracticeNote n) => practiceNoteContentHash(
    positionMs: n.positionMs,
    endMs: n.endMs,
    content: n.content,
  );

  static String _markerHash(TechniqueMarker m) => techniqueMarkerContentHash(
    positionMs: m.positionMs,
    endMs: m.endMs,
    techniqueId: m.techniqueId,
    techniqueName: m.techniqueName,
  );

  /// rowId -> MINE hash of every row of [p] exactly as a save sends it — the
  /// whole aggregate, including children the merge never reached (those of a
  /// track whose remote delete it accepted). See [Loaded.sentHashes].
  static Map<String, String> _sentHashes(
    SongPractice p,
    List<AudioTrack> tracks,
    Map<String, String> clipAsset, {
    required String? separationAssetId,
    required String? reverbJson,
    required String lyricsJson,
  }) => {
    p.id: _practiceHash(
      p,
      separationAssetId: separationAssetId,
      reverbJson: reverbJson,
      lyricsJson: lyricsJson,
    ),
    for (var i = 0; i < p.versions.length; i++)
      p.versions[i].id: _interpretationHash(p.versions[i], i),
    for (var i = 0; i < p.fxBuses.length; i++)
      p.fxBuses[i].id: _busHash(p.fxBuses[i], i),
    for (final d in _trackOrders(tracks))
      d.track.id: _trackHash(d.track, d.order),
    for (final t in tracks) ...{
      for (var i = 0; i < t.clips.length; i++)
        t.clips[i].id: _clipHash(t.clips[i], clipAsset[t.clips[i].id]!, i),
      for (final n in t.notes) n.id: _noteHash(n),
      for (final m in t.techniqueMarkers) m.id: _markerHash(m),
    },
  };

  // --- read -----------------------------------------------------------------

  @override
  Future<Result<List<PracticeSummary>>> listPracticeSummaries() async {
    try {
      final rows = await (db.select(
        db.practices,
      )..where((p) => p.deletedAt.isNull())).get();
      rows.sort((a, b) {
        final c = b.updatedAt.compareTo(a.updatedAt);
        return c != 0 ? c : b.id.compareTo(a.id);
      });
      // The counts hydration would produce, from the live rows it reads: a
      // vocal take is any live track that is not a backing, and the note
      // count spans every live lane.
      final counts = {
        for (final r
            in await db
                .customSelect(
                  'SELECT t.practice_id AS pid, '
                  "SUM(t.kind NOT IN ('accompaniment', 'reference')) AS takes, "
                  "MAX(t.kind = 'accompaniment') AS backing, "
                  'SUM((SELECT COUNT(*) FROM practice_notes n '
                  '  WHERE n.track_id = t.id AND n.deleted_at IS NULL)) AS notes '
                  'FROM tracks t WHERE t.deleted_at IS NULL GROUP BY t.practice_id',
                )
                .get())
          r.read<String>('pid'): r,
      };
      return Result.ok([
        for (final p in rows)
          PracticeSummary(
            id: p.id,
            title: p.title,
            updatedAt: _dt(p.updatedAt),
            takeCount: counts[p.id]?.read<int>('takes') ?? 0,
            hasAccompaniment: (counts[p.id]?.read<int>('backing') ?? 0) != 0,
            noteCount: counts[p.id]?.read<int>('notes') ?? 0,
          ),
      ]);
    } catch (e) {
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  }

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
    final r = await loadPractice(id);
    return switch (r) {
      Ok<Loaded<SongPractice>>(:final value) => Result.ok(value.aggregate),
      Error<Loaded<SongPractice>>(:final error) => Result.error(error),
    };
  }

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async {
    try {
      final row =
          await (db.select(db.practices)
                ..where((p) => p.id.equals(id) & p.deletedAt.isNull()))
              .getSingleOrNull();
      if (row == null) {
        return Result.error(Exception('Practice "$id" not found'));
      }
      // Collect the base hash of every row this read returns, so a later save
      // can three-way merge against exactly what the editor was shown.
      final hashes = <String, String>{};
      final practice = await _hydrate(row, baseHashes: hashes);
      return Result.ok(
        Loaded(practice, Map<String, String>.unmodifiable(hashes)),
      );
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
        // Already gone (a stale list after a remote delete, a second tap) or
        // never there: nothing to delete. Tombstoned again, it journaled a
        // fresh delete — for a row that did not exist, too.
        final parent = await (db.select(
          db.practices,
        )..where((x) => x.id.equals(id))).getSingleOrNull();
        if (parent == null || parent.deletedAt != null) return;
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
    // Base-less save: _save adopts the CURRENT persisted rows as the base,
    // which reproduces the old two-way diff exactly. Safe only for callers
    // that just created or just re-read the aggregate (createPractice, boot
    // recovery); editors must use savePracticeWithBase — see the interface.
    final r = await _save(practice, base: null);
    return switch (r) {
      Ok<Loaded<SongPractice>>(:final value) => Result.ok(value.aggregate),
      Error<Loaded<SongPractice>>(:final error) => Result.error(error),
    };
  }

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) => _save(practice, base: baseHashes);

  /// The one save core. [base] is the per-row content-hash map captured when
  /// the caller loaded the aggregate (three-way merge); null means "adopt the
  /// current persisted state as base" (the legacy two-way diff).
  ///
  /// The WHOLE import+commit sequence runs under the store write gate's
  /// shared side: asset imports land before the clip/media rows referencing
  /// them commit, and a maintenance pass (Persistence.repairLibrary / the
  /// boot sweeps, which hold the gate exclusively) running in that window
  /// would tombstone the just-imported asset and trash its pool file.
  Future<Result<Loaded<SongPractice>>> _save(
    SongPractice practice, {
    required Map<String, String>? base,
  }) => assets.gate.shared(() async {
    try {
      // All tracks flattened (accompaniment + reference + vocals).
      final allTracks = <AudioTrack>[
        if (practice.accompaniment != null) practice.accompaniment!,
        if (practice.reference != null) practice.reference!,
        ...practice.vocalTracks,
      ];

      // A deleted practice takes no save (the transaction refuses it too, see
      // _upsertPractice), and is turned away before the imports below: asked
      // only there, its new files were already imported and journaled for a
      // save that then failed.
      final parent = await (db.select(
        db.practices,
      )..where((x) => x.id.equals(practice.id))).getSingleOrNull();
      if (parent != null && parent.deletedAt != null) {
        throw StateError(
          'practice ${practice.id} is deleted; refusing to save over it',
        );
      }

      // Assets first (idempotent, deduped): every clip's audio + the separation
      // source. A dependent clip row never exists without its asset.
      final clips = [for (final t in allTracks) ...t.clips];
      // resolveOrImportAll: an `asset://` ref (bytes missing locally) or a
      // live asset's pool path (every hydrated clip) keeps its id without
      // re-importing; any other path is imported + deduped — per clip, in
      // order, exactly as resolveOrImport would answer.
      final clipAssetIds = await assets.resolveOrImportAll([
        for (final c in clips) c.audioFilePath,
      ]);
      final clipAsset = <String, String>{}; // clipId -> assetId
      for (var i = 0; i < clips.length; i++) {
        clipAsset[clips[i].id] = clipAssetIds[i];
      }
      await _cacheWaveforms([
        for (var i = 0; i < clips.length; i++)
          (assetId: clipAssetIds[i], peaks: clips[i].waveformPeaks),
      ]);
      String? separationAssetId;
      if (practice.separationSource != null &&
          practice.separationSource!.isNotEmpty) {
        separationAssetId = await assets.resolveOrImport(
          practice.separationSource!,
        );
      }

      final now = _ms(DateTime.now());
      final reverbJson = _reverbJsonOf(practice);
      final lyricsJson = _lyricsJsonOf(practice);
      // Before the write, so nothing after the commit can fail the save.
      final sent = _sentHashes(
        practice,
        allTracks,
        clipAsset,
        separationAssetId: separationAssetId,
        reverbJson: reverbJson,
        lyricsJson: lyricsJson,
      );

      await db.transaction(() async {
        await db.customStatement('PRAGMA defer_foreign_keys = ON');

        // --- practice row (single-entity reconcile) ---
        // Every pass below compares the MINE hashes in [sent], never hashes
        // the model again: what a save sent is then what its merge compared
        // by construction, and the rows are hashed once per save, not twice.
        final practiceWritten = await _upsertPractice(
          practice,
          separationAssetId: separationAssetId,
          reverbJson: reverbJson,
          lyricsJson: lyricsJson,
          now: now,
          base: base,
          mine: sent,
        );

        // --- interpretations ---
        var childChanges = await _reconcileInterpretations(
          practice,
          now,
          base,
          sent,
        );

        // --- fx buses (before tracks so a track's bus_id resolves) ---
        childChanges += await _reconcileFxBuses(practice, now, base, sent);

        // --- tracks (+ cascade their clips / notes / markers) ---
        childChanges += await _reconcileTracks(
          practice,
          allTracks,
          clipAsset,
          now,
          base,
          sent,
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

      // Re-read the saved aggregate WITH fresh base hashes, so the caller's
      // next save merges against what this save actually left in the store
      // (including any concurrent rows the three-way rules kept) — plus the
      // hashes of what it SENT, for a caller whose model moved on meanwhile.
      final reloaded = await loadPractice(practice.id);
      if (reloaded is! Ok<Loaded<SongPractice>>) {
        // Committed all the same. Reported as a failed write, the caller
        // reverted to a store that held it, or took its change back over it
        // (a separation's undo then collided with its own stored backing).
        // What was sent stands in for the re-read: it is what the caller's
        // model derives from, and what the next merge must hold it to.
        DiagnosticsLog.instance.log(
          'save',
          'practice committed; its re-read failed: '
              '${(reloaded as Error<Loaded<SongPractice>>).error.runtimeType}',
        );
        return Result.ok(
          Loaded(practice, sent, sentHashes: sent, reread: false),
        );
      }
      return Result.ok(
        Loaded(
          reloaded.value.aggregate,
          reloaded.value.baseHashes,
          sentHashes: sent,
        ),
      );
    } catch (e) {
      // The data boundary must always return a Result, never throw — a
      // data-layer Error (e.g. a StateError) must still surface as Result.error
      // so the caller's revert/toast runs instead of crashing.
      return Result.error(e is Exception ? e : Exception(e.toString()));
    }
  });

  // --- write helpers --------------------------------------------------------

  /// Writes the practice-level row. Returns whether the row was actually written
  /// (insert or content change) — false on the no-change early return AND on
  /// the three-way "keep theirs" skip, so the caller knows whether updatedAt
  /// was already bumped here.
  Future<bool> _upsertPractice(
    SongPractice p, {
    required String? separationAssetId,
    required String? reverbJson,
    required String lyricsJson,
    required int now,
    required Map<String, String>? base,
    required Map<String, String> mine,
  }) async {
    final existing = await (db.select(
      db.practices,
    )..where((x) => x.id.equals(p.id))).getSingleOrNull();
    if (existing != null && existing.deletedAt != null) {
      // The practice was tombstoned before this save landed (e.g. an in-flight
      // save racing deletePractice, or a synced-in delete). Continuing would
      // NOT revive the practice (the update below never clears deleted_at) yet
      // would still commit LIVE child rows under the tombstoned parent — a
      // journaled zombie subtree. A deleted aggregate must not be implicitly
      // re-saved — for the AGGREGATE ROOT this abort deliberately outranks the
      // per-row edit-vs-delete "active editor wins" rule: abort the whole save
      // (the transaction rolls back, no children are written) so the caller
      // surfaces Result.error, like the pre-upsert-revive PK failure.
      throw StateError('practice ${p.id} is deleted; refusing to save over it');
    }
    final hash = mine[p.id]!;
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
      final existingHash = practiceRowHash(existing);
      if (existingHash == hash) return false;
      // Three-way merge on the parent row itself (same rules as
      // reconcileCollection). A null [base] adopts the current row as base
      // (legacy two-way — the skip below then never fires); a with-base map
      // missing this id means the editor never held the row, which degrades
      // to conflict-local-wins below.
      final baseHash = base == null ? existingHash : base[p.id];
      if (baseHash != null && existingHash != baseHash && hash == baseHash) {
        // The persisted row moved on since this editor loaded it (a remote/
        // newer write) and the editor never touched the practice-level fields:
        // keep the DB state. Writing "mine" would revert the newer row to
        // stale values under a fresh HLC. Not "written" — a child-only change
        // may still bump updatedAt locally in the caller.
        return false;
      }
      // Plain local edit (existingHash == baseHash) or a true conflict (both
      // sides changed): the ACTIVE EDITOR wins by policy and mine is written
      // with a fresh HLC, so it also wins entity-level LWW downstream.
      final s = await writer.stampFor('practices', p.id);
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

  Future<int> _reconcileInterpretations(
    SongPractice p,
    int now,
    Map<String, String>? base,
    Map<String, String> mine,
  ) async {
    final live = await _live(
      db.interpretations,
      (x) => x.practiceId.equals(p.id),
    );
    final current = {for (final r in live) r.id: interpretationRowHash(r)};
    final desired = [
      for (var i = 0; i < p.versions.length; i++) (v: p.versions[i], order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'interpretations',
      // A null base (legacy no-base save) adopts the current persisted rows,
      // reproducing the old two-way diff; see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.v.id,
      hashOf: (d) => mine[d.v.id]!,
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

  Future<int> _reconcileFxBuses(
    SongPractice p,
    int now,
    Map<String, String>? base,
    Map<String, String> mine,
  ) async {
    final live = await _live(db.fxBuses, (x) => x.practiceId.equals(p.id));
    final current = {for (final r in live) r.id: fxBusRowHash(r)};
    final desired = [
      for (var i = 0; i < p.fxBuses.length; i++) (bus: p.fxBuses[i], order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'fx_buses',
      // Null base adopts the current rows (legacy two-way); see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.bus.id,
      hashOf: (d) => mine[d.bus.id]!,
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
    Map<String, String>? base,
    Map<String, String> mine,
  ) async {
    final live = await _live(db.tracks, (x) => x.practiceId.equals(p.id));
    final current = {for (final r in live) r.id: trackRowHash(r)};
    final desired = _trackOrders(tracks);

    // A slot MOVE between live rows — the separation flow's rotation (the old
    // accompaniment re-kinded to reference while a fresh instrumental takes
    // its slot) or a symmetric swap — cannot be saved by any statement order:
    // ux_track_slot is enforced per statement, so whichever row writes first
    // collides with the other still holding its old kind. Two-phase instead:
    // every live slot row this pass WILL re-kind (predicted with
    // [reconcileOpFor], the same rules the pass runs on — a rule-2/5 skip must
    // NOT be vacated, its DB kind is being kept) is parked as 'vocal' first —
    // outside the partial index, still within the kind CHECK. The row's real
    // update then writes its final kind inside this same transaction, so the
    // parked value is never observable outside it.
    const slotKinds = {'accompaniment', 'reference'};
    final desiredById = {for (final d in desired) d.track.id: d};
    for (final r in live) {
      if (!slotKinds.contains(r.kind)) continue;
      final d = desiredById[r.id];
      if (d == null || d.track.kind.name == r.kind) continue;
      final op = reconcileOpFor(
        mine: mine[r.id]!,
        db: current[r.id],
        base: (base ?? current)[r.id],
      );
      if (op != ReconcileOp.update) continue;
      await db.customUpdate(
        'UPDATE tracks SET kind = ? WHERE id = ?',
        variables: [Variable.withString('vocal'), Variable.withString(r.id)],
        updates: {db.tracks},
      );
    }

    var changed = (await reconcileCollection(
      writer: writer,
      table: 'tracks',
      // Null base adopts the current rows (legacy two-way); see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.track.id,
      hashOf: (d) => mine[d.track.id]!,
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

    // Children of every surviving track. Note the composition with the
    // per-row three-way rules: if a track was deleted remotely (cascade-
    // tombstoning its children) and the local editor's track-level edit
    // revives the track row, only children the editor ALSO edited are revived
    // here — untouched children accept the remote delete (rule 5), row by row.
    //
    // A track whose delete was ACCEPTED (rule 5's untouched side: remotely
    // deleted, unedited here) is NOT live after the pass above, and its
    // children must go with it. Reconciling them anyway would revive every
    // child the editor edited — and insert every child it added — as a LIVE
    // row hanging off a tombstoned parent: invisible locally, yet synced out
    // as an orphan. Re-read the live set rather than infer it.
    final liveTrackIds = {
      for (final r in await _live(db.tracks, (x) => x.practiceId.equals(p.id)))
        r.id,
    };
    //
    // Their live children in one set read per table, not three per track (90
    // for a 30-lane practice, on every save). Read after the track pass, whose
    // cascades are the only writes to other tracks' children: each pass below
    // writes its own track's rows alone, since local edits never move a child
    // id to another parent (cross-parent transplants mint fresh ids; see
    // reconcile.dart), so what is listed under a track stays its own until
    // that track's pass.
    final reconciled = [
      for (final t in tracks)
        if (liveTrackIds.contains(t.id)) t,
    ];
    final children = await _liveChildRows([for (final t in reconciled) t.id]);
    for (final t in reconciled) {
      changed += await _reconcileClips(
        t,
        clipAsset,
        now,
        base,
        mine,
        children.clips[t.id] ?? const [],
      );
      changed += await _reconcileNotes(
        t,
        now,
        base,
        mine,
        children.notes[t.id] ?? const [],
      );
      changed += await _reconcileMarkers(
        t,
        now,
        base,
        mine,
        children.markers[t.id] ?? const [],
      );
    }
    return changed;
  }

  Future<int> _reconcileClips(
    AudioTrack t,
    Map<String, String> clipAsset,
    int now,
    Map<String, String>? base,
    Map<String, String> mine,
    List<ClipRow> live,
  ) async {
    final current = {for (final r in live) r.id: clipRowHash(r)};
    final desired = [
      for (var i = 0; i < t.clips.length; i++)
        (clip: t.clips[i], assetId: clipAsset[t.clips[i].id]!, order: i),
    ];
    return (await reconcileCollection(
      writer: writer,
      table: 'clips',
      // Null base adopts the current rows (legacy two-way); see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: desired,
      idOf: (d) => d.clip.id,
      hashOf: (d) => mine[d.clip.id]!,
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

  Future<int> _reconcileNotes(
    AudioTrack t,
    int now,
    Map<String, String>? base,
    Map<String, String> mine,
    List<PracticeNoteRow> live,
  ) async {
    final current = {for (final r in live) r.id: practiceNoteRowHash(r)};
    return (await reconcileCollection(
      writer: writer,
      table: 'practice_notes',
      // Null base adopts the current rows (legacy two-way); see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: t.notes,
      idOf: (n) => n.id,
      hashOf: (n) => mine[n.id]!,
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

  Future<int> _reconcileMarkers(
    AudioTrack t,
    int now,
    Map<String, String>? base,
    Map<String, String> mine,
    List<TechniqueMarkerRow> live,
  ) async {
    final current = {for (final r in live) r.id: techniqueMarkerRowHash(r)};
    return (await reconcileCollection(
      writer: writer,
      table: 'technique_markers',
      // Null base adopts the current rows (legacy two-way); see savePractice.
      baseHashes: base ?? current,
      currentHashes: current,
      desired: t.techniqueMarkers,
      idOf: (m) => m.id,
      hashOf: (m) => mine[m.id]!,
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
  Future<List<D>> _live<T extends SyncCols, D>(
    TableInfo<T, D> table,
    Expression<bool> Function(T) filter,
  ) {
    final q = db.select(table)..where((t) => filter(t) & _notDeleted(t));
    return q.get();
  }

  Expression<bool> _notDeleted(SyncCols t) =>
      (t.deletedAt as GeneratedColumn<int>).isNull();

  /// Soft-deletes a row using a stamp MINTED BY THE CALLER and does NOT journal
  /// — for the reconcile path, where [reconcileCollection] emits the one oplog
  /// entry. Taking the caller's stamp keeps the row's HLC equal to that entry's
  /// (self-journaling here would add a second, HLC-mismatched tombstone op).
  Future<void> _tombstoneRow(
    TableInfo<Table, dynamic> table,
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
    TableInfo<Table, dynamic> table,
    String name,
    String id,
    int now,
  ) async {
    final s = await writer.stampFor(table.actualTableName, id);
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

  /// Caches each clip's peaks under its asset's content hash: one asset read
  /// and ONE batch (a single transaction) for the whole save. It used to be a
  /// select plus an auto-committed upsert per clip — 100 write transactions
  /// on every save of a 100-clip practice, each rewriting a blob that almost
  /// never changed (the peaks were hydrated from this very row). The upsert's
  /// WHERE skips a row whose bytes already match, and the batch runs in clip
  /// order, so the final cache is exactly the per-clip loop's (last write per
  /// hash wins).
  Future<void> _cacheWaveforms(
    List<({String assetId, List<double> peaks})> clips,
  ) async {
    final pending = [
      for (final c in clips)
        if (c.peaks.isNotEmpty) c,
    ];
    if (pending.isEmpty) return;
    final hashes = await _contentHashes({for (final c in pending) c.assetId});
    final writes = [
      for (final c in pending)
        if (hashes[c.assetId] case final hash?) (hash: hash, peaks: c.peaks),
    ];
    if (writes.isEmpty) return;
    await db.batch((b) {
      for (final w in writes) {
        b.customStatement(
          'INSERT INTO waveform_cache (content_hash, algo_version, peaks) '
          'VALUES (?, ?, ?) ON CONFLICT (content_hash, algo_version) '
          'DO UPDATE SET peaks = excluded.peaks '
          'WHERE waveform_cache.peaks IS NOT excluded.peaks',
          [w.hash, _waveformAlgoVersion, _encodePeaks(w.peaks)],
        );
      }
    });
  }

  /// assetId -> content hash for every id in [ids] that has an asset row
  /// (live or tombstoned, like the per-clip lookups it replaces).
  Future<Map<String, String>> _contentHashes(Set<String> ids) async {
    final out = <String, String>{};
    for (final chunk in AssetStore.inChunks(ids.toList())) {
      for (final a in await (db.select(
        db.assets,
      )..where((a) => a.id.isIn(chunk))).get()) {
        out[a.id] = a.contentHash;
      }
    }
    return out;
  }

  // --- hydration ------------------------------------------------------------

  /// Builds the domain aggregate from [row]. When [baseHashes] is given, the
  /// content hash of EVERY row this read returns (the practice row and each
  /// live child row, whether or not it survives domain-level dedup) is
  /// recorded — the base side of a later three-way save. The projections come
  /// from sync_hashes.dart, the SAME functions the save path hashes with.
  ///
  /// A row with JSON columns (the practice's reverb and lyrics, a track's or
  /// bus's chain and sends) is hashed as READ INTO THE MODEL — decoded and
  /// re-encoded the way a save sends it — not as stored. Decoding clamps
  /// out-of-range values and drops keys this build doesn't know, so a stored
  /// form the model can't reproduce (another build's, a hand edit) made every
  /// save see the row as edited: rewritten as a change nobody made, and a
  /// concurrent edit of it by another device reverted. Its own order index
  /// stays the stored one: a renumbering must be written, or the rows it
  /// skipped would interleave stale indices with the ones it wrote.
  Future<SongPractice> _hydrate(
    PracticeRow row, {
    Map<String, String>? baseHashes,
  }) async {
    final interpRows =
        await (db.select(db.interpretations)
              ..where((i) => i.practiceId.equals(row.id) & i.deletedAt.isNull())
              ..orderBy([
                (i) => OrderingTerm(expression: i.orderIndex),
                // Ties are possible (two devices inserting offline pick the same
                // index); without a second key SQLite is free to order them
                // differently on each device, and the next save writes each
                // machine's accidental order back as real indices. The id is a
                // ULID, so it breaks ties by creation time and every replica
                // lands on the same list.
                (i) => OrderingTerm(expression: i.id),
              ]))
            .get();
    final versions = [
      for (final i in interpRows)
        TakeVersion(id: i.id, name: i.name, createdAt: _dt(i.createdAt)),
    ];
    if (baseHashes != null) {
      for (final i in interpRows) {
        baseHashes[i.id] = interpretationRowHash(i);
      }
    }

    final trackRows =
        await (db.select(db.tracks)
              ..where((t) => t.practiceId.equals(row.id) & t.deletedAt.isNull())
              ..orderBy([
                (t) => OrderingTerm(expression: t.orderIndex),
                (t) => OrderingTerm(
                  expression: t.id,
                ), // stable tie-break; see interpretations
              ]))
            .get();
    // Every live child of every track in a handful of set reads, not three
    // queries per track plus three per clip (394 round trips for a 30-lane,
    // 100-clip practice — paid again by every save's re-read).
    final children = await _readChildren([for (final t in trackRows) t.id]);
    AudioTrack? accompaniment;
    AudioTrack? reference;
    final vocals = <AudioTrack>[];
    for (final tr in trackRows) {
      final track = _hydrateTrack(tr, children, baseHashes: baseHashes);
      baseHashes?[tr.id] = _trackHash(track, tr.orderIndex);
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
              ..orderBy([
                (b) => OrderingTerm(expression: b.orderIndex),
                (b) => OrderingTerm(
                  expression: b.id,
                ), // stable tie-break; see interpretations
              ]))
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
          volume: _level(b.volume),
          muted: b.muted,
          soloed: b.soloed,
          pan: _pan(b.pan),
          outputBusId: b.outputBusId,
          sends: _sendsFrom(b.sendsJson),
        ),
    ];
    if (baseHashes != null) {
      for (var i = 0; i < busRows.length; i++) {
        baseHashes[busRows[i].id] = _busHash(buses[i], busRows[i].orderIndex);
      }
    }

    final practice = SongPractice(
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
      masterVolume: _level(row.masterVolume),
      fxBuses: buses,
    );
    baseHashes?[row.id] = _practiceHash(
      practice,
      separationAssetId: row.separationAssetId,
      reverbJson: _reverbJsonOf(practice),
      lyricsJson: _lyricsJsonOf(practice),
    );
    return practice;
  }

  AudioTrack _hydrateTrack(
    TrackRow tr,
    _ChildRows children, {
    Map<String, String>? baseHashes,
  }) {
    final clips = <AudioClip>[];
    for (final c in children.clips[tr.id] ?? const <ClipRow>[]) {
      baseHashes?[c.id] = clipRowHash(c);
      final peaks = children.peaks[c.assetId];
      clips.add(
        AudioClip(
          id: c.id,
          // Keep the clip even when its bytes are missing locally (carry an
          // `asset://` ref) so a reload never drops it — dropping it would
          // make the next save tombstone the row, losing the take permanently.
          audioFilePath:
              children.paths[c.assetId] ?? AssetStore.refFor(c.assetId),
          durationMs: c.durationMs,
          startMs: c.startMs,
          // Decoded per clip: two clips over the same bytes never share one
          // mutable list.
          waveformPeaks: peaks == null ? const [] : _decodePeaks(peaks),
        ),
      );
    }

    final noteRows = children.notes[tr.id] ?? const <PracticeNoteRow>[];
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
    if (baseHashes != null) {
      for (final n in noteRows) {
        baseHashes[n.id] = practiceNoteRowHash(n);
      }
    }

    final markerRows = children.markers[tr.id] ?? const <TechniqueMarkerRow>[];
    if (baseHashes != null) {
      for (final m in markerRows) {
        baseHashes[m.id] = techniqueMarkerRowHash(m);
      }
    }
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
      volume: _level(tr.volume),
      pan: _pan(tr.pan),
    );
  }

  /// The live clips / notes / markers of [trackIds], grouped by track, plus
  /// the clips' resolved paths and cached peaks — six set reads (chunked by
  /// [AssetStore.inChunks]) whatever the practice's size. Each group
  /// keeps the order its old per-track query returned: clips by
  /// (order_index, id); notes and markers in rowid order, which is what the
  /// per-track `track_id = ?` scan over ix_notes_track / ix_markers_track
  /// yielded. Asset rows are read live or tombstoned, like [AssetStore.resolve].
  Future<_ChildRows> _readChildren(List<String> trackIds) async {
    final live = await _liveChildRows(trackIds);
    final clips = [for (final rows in live.clips.values) ...rows];

    final assetRows = <AssetRow>[];
    for (final chunk in AssetStore.inChunks(
      {for (final c in clips) c.assetId}.toList(),
    )) {
      assetRows.addAll(
        await (db.select(db.assets)..where((a) => a.id.isIn(chunk))).get(),
      );
    }
    final peaksByHash = <String, Uint8List>{};
    for (final chunk in AssetStore.inChunks(
      {for (final a in assetRows) a.contentHash}.toList(),
    )) {
      for (final w
          in await (db.select(db.waveformCache)..where(
                (w) =>
                    w.contentHash.isIn(chunk) &
                    w.algoVersion.equals(_waveformAlgoVersion),
              ))
              .get()) {
        peaksByHash[w.contentHash] = w.peaks;
      }
    }
    final paths = <String, String>{};
    final peaks = <String, Uint8List>{};
    await Future.wait([
      for (final a in assetRows)
        assets.resolveRow(a).then((path) {
          if (path != null) paths[a.id] = path;
        }),
    ]);
    for (final a in assetRows) {
      final p = peaksByHash[a.contentHash];
      if (p != null) peaks[a.id] = p;
    }

    return _ChildRows(
      clips: live.clips,
      notes: live.notes,
      markers: live.markers,
      paths: paths,
      peaks: peaks,
    );
  }

  /// The live clips / notes / markers of [trackIds], grouped by track: one
  /// set read per table (chunked by [AssetStore.inChunks]), each group in
  /// the order its old per-track query returned — clips by
  /// (order_index, id); notes and markers in rowid order, which is what the
  /// per-track `track_id = ?` scan over ix_notes_track / ix_markers_track
  /// yielded.
  Future<
    ({
      Map<String, List<ClipRow>> clips,
      Map<String, List<PracticeNoteRow>> notes,
      Map<String, List<TechniqueMarkerRow>> markers,
    })
  >
  _liveChildRows(List<String> trackIds) async {
    final clips = <ClipRow>[];
    final notes = <PracticeNoteRow>[];
    final markers = <TechniqueMarkerRow>[];
    for (final chunk in AssetStore.inChunks(trackIds)) {
      clips.addAll(
        await (db.select(db.clips)
              ..where((c) => c.trackId.isIn(chunk) & c.deletedAt.isNull())
              ..orderBy([
                (c) => OrderingTerm(expression: c.trackId),
                (c) => OrderingTerm(expression: c.orderIndex),
                (c) => OrderingTerm(
                  expression: c.id,
                ), // stable tie-break; see interpretations
              ]))
            .get(),
      );
      notes.addAll(
        await (db.select(db.practiceNotes)
              ..where((n) => n.trackId.isIn(chunk) & n.deletedAt.isNull())
              ..orderBy([
                (n) => OrderingTerm(expression: n.trackId),
                (n) => OrderingTerm(expression: db.practiceNotes.rowId),
              ]))
            .get(),
      );
      markers.addAll(
        await (db.select(db.techniqueMarkers)
              ..where((m) => m.trackId.isIn(chunk) & m.deletedAt.isNull())
              ..orderBy([
                (m) => OrderingTerm(expression: m.trackId),
                (m) => OrderingTerm(expression: db.techniqueMarkers.rowId),
              ]))
            .get(),
      );
    }

    Map<String, List<R>> byTrack<R>(List<R> rows, String Function(R) track) {
      final out = <String, List<R>>{};
      for (final r in rows) {
        (out[track(r)] ??= <R>[]).add(r);
      }
      return out;
    }

    return (
      clips: byTrack(clips, (c) => c.trackId),
      notes: byTrack(notes, (n) => n.trackId),
      markers: byTrack(markers, (m) => m.trackId),
    );
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

/// [SongPracticeRepositoryDrift._readChildren]'s result: live child rows by
/// track id, and per clip asset id its local path (absent = bytes not here)
/// and its cached peak bytes (absent = not cached).
class _ChildRows {
  const _ChildRows({
    required this.clips,
    required this.notes,
    required this.markers,
    required this.paths,
    required this.peaks,
  });

  final Map<String, List<ClipRow>> clips;
  final Map<String, List<PracticeNoteRow>> notes;
  final Map<String, List<TechniqueMarkerRow>> markers;
  final Map<String, String> paths;
  final Map<String, Uint8List> peaks;
}
