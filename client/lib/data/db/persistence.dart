import 'dart:async';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:drift/native.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import '../../domain/models/audio_track.dart';
import '../../domain/models/song_practice.dart';
import '../../utils/id.dart';
import '../../utils/result.dart';
import '../repositories/song_practice_repository_drift.dart';
import '../repositories/technique_repository_drift.dart';
import '../services/audio_file_service.dart';
import '../services/diagnostics_log.dart';
import '../services/local_storage_service.dart';
import '../services/waveform_service.dart';
import '../sync/hlc.dart';
import '../sync/integrity_checker.dart';
import '../sync/oplog_drain.dart';
import '../sync/quarantine_store.dart';
import '../sync/sync_applier.dart';
import '../sync/sync_engine.dart';
import '../sync/sync_row.dart';
import '../sync/sync_state_store.dart';
import '../sync/sync_transport.dart';
import '../sync/sync_writer.dart';
import 'app_database.dart';
import 'asset_store.dart';
import 'durable_fs.dart';
import 'safe_migrator.dart';
import 'write_gate.dart';

/// Signature of the audio analyzer used to rebuild a recovered take's waveform
/// peaks + duration — [WaveformService.analyze] in production, injectable so
/// tests don't need the platform decoder. Must throw on undecodable audio.
typedef WavAnalyzer = Future<WaveformResult> Function(String path);

/// Thrown by [Persistence.open] when the on-disk library's schema
/// `user_version` is HIGHER than this build's [AppDatabase.currentSchemaVersion]:
/// the library was written by a newer Vocara. Opening it anyway would run this
/// build against tables and columns it doesn't understand, and drift's own
/// version check only fires lazily on the first query — long after the app
/// started against a half-dead store. Typed so main() can show a dedicated
/// "please update Vocara" window instead of a generic failure.
class NewerLibraryException implements Exception {
  NewerLibraryException({
    required this.libraryVersion,
    required this.appVersion,
  });

  /// The schema `user_version` found in the on-disk database.
  final int libraryVersion;

  /// The schema version this build understands.
  final int appVersion;

  @override
  String toString() =>
      'NewerLibraryException: the library schema is v$libraryVersion but this '
      'build understands up to v$appVersion';
}

/// What one [Persistence.repairLibrary] pass found and did.
class RepairReport {
  const RepairReport({
    required this.violationsFound,
    required this.violationsRepaired,
    required this.assetsSwept,
  });

  /// Cross-row integrity violations detected before repairing.
  final int violationsFound;

  /// How many of those the repair pass resolved.
  final int violationsRepaired;

  /// Unreferenced assets tombstoned plus orphaned pool files reclaimed.
  final int assetsSwept;

  @override
  String toString() =>
      'RepairReport(found: $violationsFound, repaired: $violationsRepaired, '
      'assetsSwept: $assetsSwept)';
}

/// The wired-up persistence layer: one drift database, the write chokepoint, the
/// asset pool, and the two repositories built on them. Constructed ONCE at app
/// startup (the database file + the per-install device node id + the asset pool
/// all live under the app support directory).
class Persistence {
  Persistence({
    required this.db,
    required this.writer,
    required this.assets,
    required this.songRepo,
    required this.techRepo,
  });

  final AppDatabase db;
  final SyncWriter writer;
  final AssetStore assets;
  final SongPracticeRepositoryDrift songRepo;
  final TechniqueRepositoryDrift techRepo;

  /// The app-wide store write gate: saves/imports run under its shared side,
  /// [repairLibrary] and the boot sweeps take it exclusively — see [WriteGate].
  WriteGate get writeGate => assets.gate;

  /// "Remote changes were applied by sync" notifications for the UI layer:
  /// one event per committed pull page that changed rows, fired strictly
  /// AFTER that page's transaction (and the follow-up integrity repair)
  /// committed — never from inside an open transaction. View models listen to
  /// refresh what they display; skipping a reload is always safe because
  /// three-way saves merge against base hashes (freshness, not safety).
  Stream<RemoteApplied> get remoteApplied => _remoteApplied.stream;
  final StreamController<RemoteApplied> _remoteApplied =
      StreamController<RemoteApplied>.broadcast();

  /// The sync engine, once [attachSync] wired one (null until then — P0 has
  /// no production transport; tests and the future server transport attach).
  SyncEngine? get sync => _sync;
  SyncEngine? _sync;

  /// Builds and wires the [SyncEngine] over this store: the applier (with a
  /// persistent quarantine), the oplog drain, the row builder, the sync-state
  /// and integrity collaborators, and the [remoteApplied] broadcast. The
  /// engine starts PASSIVE — callers pump it explicitly; production calls
  /// [SyncEngine.start] to also subscribe the debounced push to committed
  /// oplog writes. Attach at most once per Persistence.
  SyncEngine attachSync(
    SyncTransport transport, {
    SyncTimerFactory? timers,
    Future<void> Function(FetchPage page)? debugBeforePageCommit,
  }) {
    if (_sync != null) {
      throw StateError('a SyncEngine is already attached to this store');
    }
    // One builder shared by the engine (push projection) and the applier
    // (the FK sweep's park-instead-of-lose snapshot) — same schema, same
    // memoized table ranks.
    final builder = SyncRowBuilder(db);
    final engine = SyncEngine(
      db: db,
      applier: SyncApplier(
        db,
        writer.clock,
        builder: builder,
        quarantine: QuarantineStore(db),
      ),
      drain: OplogDrain(db),
      builder: builder,
      state: SyncStateStore(db),
      integrity: IntegrityChecker(db, writer),
      transport: transport,
      onRemoteApplied: _remoteApplied.add,
      timers: timers,
      debugBeforePageCommit: debugBeforePageCommit,
    );
    _sync = engine;
    return engine;
  }

  /// Opens the real on-disk store under the app support directory. First
  /// recovers any half-finished schema migration (a crash mid-swap) so the live
  /// file is always consistent before drift opens it.
  ///
  /// [dir] and [analyze] are test seams (the store's on-disk root and the
  /// recovery waveform analyzer); production passes neither.
  ///
  /// Throws [NewerLibraryException] when the library was written by a newer
  /// Vocara; any other throw is a fatal open, surfaced by main() in a
  /// dedicated error window.
  static Future<Persistence> open({
    Directory? dir,
    WavAnalyzer? analyze,
  }) async {
    dir ??= await getApplicationSupportDirectory();
    final dbPath = p.join(dir.path, 'vocara.db');
    await SafeMigrator().recoverIfNeeded(dbPath);
    // Refuse a library from a NEWER Vocara BEFORE building the store: this
    // build cannot understand (let alone migrate) a higher schema, so nothing
    // may touch it — not drift, not the sweeps. A downgrade path does not
    // exist by design; the on-disk file is left byte-for-byte untouched.
    if (await File(dbPath).exists()) {
      final onDiskVersion = SafeMigrator().readUserVersion(dbPath);
      if (onDiskVersion > AppDatabase.currentSchemaVersion) {
        throw NewerLibraryException(
          libraryVersion: onDiskVersion,
          appVersion: AppDatabase.currentSchemaVersion,
        );
      }
    }
    // Upgrade a stale on-disk schema BEFORE drift opens the file: drift's own
    // onUpgrade deliberately throws (the copy/validate/swap protocol owns
    // upgrades). A fresh install (no file) is a no-op here — drift's onCreate
    // builds the current schema directly.
    await SafeMigrator().upgradeIfNeeded(
      dbPath: dbPath,
      targetVersion: AppDatabase.currentSchemaVersion,
    );
    final db = AppDatabase(NativeDatabase.createInBackground(File(dbPath)));
    final node = await _readOrCreateNode(dir);
    // Restore the HLC clock's monotonicity across restarts BEFORE wiring the
    // writer (the startup sweeps below tick it), so a new local edit can never
    // stamp LOWER than one this install already pushed. See _seedClock.
    final clock = await _seedClock(db, node);
    final persistence = _wire(db, dir, node, clock: clock);
    // Recover hard-kill orphaned take recordings BEFORE the staging sweep
    // deletes them: a force quit / crash / kill -9 mid-record skips the
    // graceful stop + save, leaving the streamed WAV in staging as the ONLY
    // copy of the take (the engine keeps its header valid by patching it in
    // place ~1x/s while recording). Best-effort; never blocks launch.
    //
    // GATE for every destructive sweep below: the sweeps derive their
    // keep-sets from the database, and sweepStaging clears the staging tree
    // outright — so they may only run once the database has answered real
    // reads AND recovery completed. If the probe or recovery threw (an
    // unreadable table, a failed staging scan), a staged WAV may still be the
    // ONLY copy of a take, and sweeping it would be unrecoverable data loss.
    // Skipping every sweep on a sick boot merely leaks disk until a healthy
    // boot; the sweeps themselves remain best-effort and never block launch.
    var sweepGateOpen = false;
    try {
      await _probeDbReadable(db);
      final wavAnalyzer =
          analyze ?? WaveformService(storage: LocalStorageService()).analyze;
      await persistence.recoverStagedRecordings(analyze: wavAnalyzer);
      sweepGateOpen = true;
    } catch (e) {
      DiagnosticsLog.instance.log(
        'persistence',
        'boot health probe / staged-recording recovery failed '
            '(${e.runtimeType}); skipping all destructive sweeps this boot',
      );
    }
    if (sweepGateOpen) {
      // The whole boot maintenance pass holds the write gate exclusively.
      // Nothing user-facing is running yet, so this is uncontended today; it
      // exists so the sweeps and a save can structurally never interleave
      // (the same discipline repairLibrary follows on a live store).
      await persistence.writeGate.exclusive(() async {
        // Reclaim staging leftovers: after a save, a recording/import has been copied
        // into the content-addressed pool and its working file under audio/ or
        // technique_media/ is an orphan (nothing persistent references it — clips
        // and media reference pool assets). At startup nothing is in flight, so the
        // staging tree is safe to clear. Best-effort; never blocks launch.
        // Files whose recovery ATTEMPT threw are kept for a retry next boot
        // (see recoverStagedRecordings) instead of being funneled to trash by
        // one environment-wide analyzer failure.
        await persistence.assets.sweepStaging(
          keep: persistence._recoveryRetryPaths,
        );
        // Reclaim pooled bytes for assets no live clip/media/practice references any
        // more (removed takes, replaced accompaniment, deleted examples). Derives the
        // refcount from a query, so it's safe to run at startup when nothing is in
        // flight. Best-effort — a GC failure must never block launch.
        try {
          await persistence.assets.sweepUnreferenced();
        } catch (e) {
          DiagnosticsLog.instance.log(
            'persistence',
            'sweepUnreferenced failed (${e.runtimeType})',
          );
        }
        // Reclaim pool files left with no asset row at all (a crash between the pool write and the
        // asset-row commit). Best-effort — a GC failure must never block launch.
        try {
          await persistence.assets.sweepOrphanPoolFiles();
        } catch (e) {
          DiagnosticsLog.instance.log(
            'persistence',
            'sweepOrphanPoolFiles failed (${e.runtimeType})',
          );
        }
        // The sweeps above park reclaimed files in the asset trash rather than
        // unlinking them; age trash out ONLY after a fully healthy boot, so a
        // sick boot can never shorten the recovery window.
        try {
          await persistence.assets.pruneTrash();
        } catch (e) {
          DiagnosticsLog.instance.log(
            'persistence',
            'pruneTrash failed (${e.runtimeType})',
          );
        }
        // Compact the durability ledger: production attaches no sync
        // transport today, so nothing ever acks (deletes) oplog rows — every
        // save appends and the ledger only grows. Safe under the drain
        // contract (see [compactOplog]); best-effort like the other
        // maintenance steps.
        try {
          await persistence.compactOplog();
        } catch (e) {
          DiagnosticsLog.instance.log(
            'persistence',
            'compactOplog failed (${e.runtimeType})',
          );
        }
      });
    }
    return persistence;
  }

  /// One real read against every table the boot sweeps derive their keep-sets
  /// from. A miss (missing or corrupt table) throws, which closes the sweep
  /// gate in [open]: when reference derivation can't be trusted, no
  /// destructive sweep may run.
  static Future<void> _probeDbReadable(AppDatabase db) async {
    for (final table in const [
      'practices',
      'tracks',
      'clips',
      'technique_media',
      'assets',
    ]) {
      await db.customSelect('SELECT COUNT(*) AS n FROM $table').getSingle();
    }
  }

  /// A synchronous in-memory instance (host sqlite3) for tests / widget boot.
  factory Persistence.memory() {
    final dir = Directory.systemTemp.createTempSync('vocara_mem');
    return _wire(AppDatabase(NativeDatabase.memory()), dir, newId());
  }

  /// One user-invoked "repair library" pass: runs the cross-row integrity
  /// repair to a fixpoint, then reclaims unreferenced assets and orphaned
  /// pool files. Repairs journal through the normal write chokepoint like any
  /// edit. Safe on a live store because the whole pass holds [writeGate]
  /// exclusively: an in-flight save (import committed, clip/media rows not
  /// yet) finishes first, and no new save starts mid-pass — so the GC can
  /// never tombstone an asset a save just imported, or trash its in-flight
  /// pool file. Meant for a diagnostics/settings action, never the boot path
  /// (boot has its own gated sweeps in [open]).
  Future<RepairReport> repairLibrary() => writeGate.exclusive(() async {
    final checker = IntegrityChecker(db, writer);
    final found = await checker.check();
    // One transaction around the whole repair fixpoint, exactly like the sync
    // engine's end-of-pull repair: each individual repair is a row UPDATE plus
    // its oplog entry, and the invariant "a row mutation and its journal entry
    // commit in the SAME transaction" must hold here too — two autocommit
    // statements would let a crash between them tombstone/heal a synced row
    // with no journal entry, so the change would never propagate.
    final unrepaired = await db.transaction(() => checker.repair());
    // A cascade can repair MORE than the initial scan saw; the report never
    // claims a negative remainder.
    final repaired = (found.length - unrepaired.length).clamp(0, found.length);
    final tombstoned = await assets.sweepUnreferenced();
    final orphaned = await assets.sweepOrphanPoolFiles();
    final report = RepairReport(
      violationsFound: found.length,
      violationsRepaired: repaired,
      assetsSwept: tombstoned + orphaned,
    );
    DiagnosticsLog.instance.log('persistence', 'repairLibrary: $report');
    return report;
  });

  /// Compacts the oplog: for every (entity_table, entity_id), keeps only the
  /// LATEST journaled op and deletes the superseded history. Returns the
  /// number of rows deleted.
  ///
  /// This preserves the drain contract exactly: the ledger's only consumer,
  /// [OplogDrain.readPending], already collapses each (table, id) to its
  /// latest op before pushing (the pusher uploads CURRENT row state, so
  /// intermediate ops are redundant by design — a trailing tombstone is the
  /// latest op and survives). Two details are handled deliberately:
  ///
  ///  * The asset gate: the drain's collapse keeps the latest NON-null
  ///    depends_on_asset_id across a row's entries (a content-only update
  ///    must not shed its insert's dependency), so before deleting we carry
  ///    that value onto the surviving row in the same transaction.
  ///  * synced = true rows cannot exist: nothing ever sets the flag — the ack
  ///    path hard-DELETES instead (see [OplogDrain.ack]) — so the SQL needs
  ///    no synced filter; keep-latest-per-entity alone is exact. If a future
  ///    change ever starts marking rows synced, revisit this predicate.
  ///
  /// Run from the gated boot maintenance pass (under [writeGate] exclusive,
  /// nothing in flight); one transaction so a crash can never leave a
  /// dependency carried but its history half-deleted, or vice versa.
  Future<int> compactOplog() => db.transaction(() async {
    await db.customStatement(
      'UPDATE oplog SET depends_on_asset_id = ('
      'SELECT o.depends_on_asset_id FROM oplog o '
      'WHERE o.entity_table = oplog.entity_table '
      'AND o.entity_id = oplog.entity_id '
      'AND o.depends_on_asset_id IS NOT NULL '
      'ORDER BY o.seq DESC LIMIT 1'
      ') WHERE depends_on_asset_id IS NULL AND seq IN ('
      'SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id)',
    );
    return db.customUpdate(
      'DELETE FROM oplog WHERE seq NOT IN ('
      'SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id)',
    );
  });

  /// Boot-time recovery of hard-kill orphaned take recordings. Returns the
  /// number of takes recovered.
  ///
  /// Recordings stream to `audio/<practiceId>/<id>.wav` (the parent directory
  /// name IS the practice id — [AudioFileService.newRecordingPath]) and are
  /// copied into the content-addressed pool by the save at the end of a
  /// graceful stop, so after a save the staging file is a reclaimable leftover.
  /// A TRUE hard kill (force quit / crash / kill -9) skips stop + save
  /// entirely: the staging file is then the ONLY copy of the take. The
  /// engine's recording writer patches the WAV header in place about once per
  /// second, so that file is a valid WAV covering the take up to the last
  /// patch — re-attach it to its practice as a new vocal take, mirroring the
  /// normal stop-recording finalize (waveform analysis, clip creation, save).
  ///
  /// Must run BEFORE [AssetStore.sweepStaging]: whatever this pass does not
  /// recover (already-saved leftovers, empty/unparseable files, non-recording
  /// audio, files of deleted practices) is deleted by the sweep exactly as
  /// before.
  ///
  /// Crash-safety / idempotency (this recovery can itself be hard-killed): the
  /// per-file order is (1) pool import + take/clip row commit — both inside
  /// [SongPracticeRepositoryDrift.savePractice], rows in one transaction —
  /// then (2) delete the staging file. A file is only recovered when NO
  /// clip / technique-media / separation reference — live OR tombstoned —
  /// carries its content hash (bytes that match anything EVER saved are
  /// either a normal already-pooled leftover or a deliberately deleted take;
  /// see [_referencedContentHashes]), so:
  ///  * killed before the rows commit → nothing references the hash yet; the
  ///    next boot retries the whole recovery (the pool import dedups by hash,
  ///    so a half-done import is reused, never duplicated);
  ///  * killed between the rows commit and the file delete → the new clip row
  ///    references the hash; the next boot SKIPS the file and the sweep
  ///    deletes it — the take can never be attached twice.
  Future<int> recoverStagedRecordings({required WavAnalyzer analyze}) async {
    _recoveryRetryPaths.clear();
    final staging = Directory(p.join(assets.root.path, 'audio'));
    if (!await staging.exists()) return 0;

    // Content hashes referenced by any clip / technique-media / separation
    // row, live or tombstoned — see [_referencedContentHashes] for why the
    // tombstoned ones count. A staging file whose bytes match one was already
    // saved into the pool (or deliberately deleted) and must NOT become a
    // second take. Note the set covers only rows a save COMPLETED for: an
    // asset row alone (a crash between the pool import and the row commit)
    // does not mark its file as saved, so that take is still recovered on the
    // retry.
    final referenced = await _referencedContentHashes();

    var recovered = 0;
    await for (final dir in staging.list()) {
      if (dir is! Directory) continue;
      final practiceId = p.basename(dir.path);
      await for (final f in dir.list()) {
        if (f is! File || p.extension(f.path).toLowerCase() != '.wav') {
          continue;
        }
        try {
          if (await _recoverOrphanTake(f, practiceId, referenced, analyze)) {
            recovered++;
          }
        } catch (e) {
          // Best-effort per file: a failed recovery never blocks the other
          // orphans or the launch. But a THROWN attempt (decode error, disk
          // error) is an environmental failure, not proof the file is junk —
          // a systemic analyzer breakage would otherwise funnel EVERY orphan
          // to trash this boot, with staging empty next boot so recovery
          // never retries. Exempt the file from this boot's staging sweep so
          // the next boot tries again, bounded by age: a file that has kept
          // failing for over 7 days goes to the sweep after all (the trash
          // then still holds it for 30 days).
          if (await _youngEnoughToRetry(f)) {
            _recoveryRetryPaths.add(f.path);
          }
          DiagnosticsLog.instance.log(
            'persistence',
            'one staged recording failed to recover (${e.runtimeType}); '
                'exempted from this boot\'s sweep if young enough to retry',
          );
        }
      }
    }
    return recovered;
  }

  /// Staging files whose recovery attempt THREW this boot — exempted from the
  /// boot staging sweep so the next boot's recovery retries them. Repopulated
  /// by every [recoverStagedRecordings] run.
  Set<String> get recoveryRetryPaths => Set.unmodifiable(_recoveryRetryPaths);
  final Set<String> _recoveryRetryPaths = <String>{};

  /// How long a repeatedly-failing staged recording is protected from the
  /// boot sweep before it is released to it (trash retention then still
  /// applies on top).
  static const Duration _recoveryRetryMaxAge = Duration(days: 7);

  /// Whether [f] is young enough (by mtime) to hold back for a retry. An
  /// unstat-able file is kept — lingering is always safer than trashing.
  static Future<bool> _youngEnoughToRetry(File f) async {
    try {
      final modified = (await f.stat()).modified;
      return DateTime.now().difference(modified) < _recoveryRetryMaxAge;
    } catch (_) {
      return true;
    }
  }

  /// Recovers one staged WAV as a new vocal take of [practiceId]. False means
  /// "not recovered" — the file is left in place for the staging sweep to
  /// delete, exactly like before recovery existed.
  Future<bool> _recoverOrphanTake(
    File file,
    String practiceId,
    Set<String> referencedHashes,
    WavAnalyzer analyze,
  ) async {
    // Only what the recording engine writes is a candidate: a canonical-header
    // MONO WAV — 32-bit float today, 16-bit PCM from pre-float versions — with
    // a non-empty payload. Staged imports are almost never that shape, and
    // misreviving an import as a "take" would be worse than sweeping it. An
    // empty payload (killed before the first ~1 s header patch) carries no
    // audio worth keeping.
    final probe = await AudioFileService.probeWav(file);
    if (probe == null ||
        probe.channels != 1 ||
        (probe.bitsPerSample != 16 && probe.bitsPerSample != 32) ||
        probe.dataBytes <= 0) {
      return false;
    }

    // Bytes already referenced by a completed save → a normal leftover, not an
    // orphaned take (this is also what makes a re-run after a mid-recovery
    // crash a no-op instead of a duplicate take).
    final hash = (await sha256.bind(file.openRead()).first).toString();
    if (referencedHashes.contains(hash)) return false;

    // The practice must still exist and be live (getPractice filters
    // tombstones); an underivable id lands here too and the file is swept.
    final res = await songRepo.getPractice(practiceId);
    if (res is! Ok<SongPractice>) return false;
    final practice = res.value;

    // Same take construction as a graceful stop with nothing armed: analyze
    // the audio, append a fresh single-clip vocal take to the active
    // interpretation, save. The killed session's playhead is unknowable, so
    // the clip lands at the timeline origin.
    final analysis = await analyze(file.path);
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take ${_nextTakeNumber(practice)} (recovered)',
      audioFilePath: file.path,
      durationMs: analysis.durationMs,
      startMs: 0,
      waveformPeaks: analysis.peaks,
      createdAt: DateTime.now(),
      versionId: practice.activeVersion?.id,
    );
    final saved = await songRepo.savePractice(
      practice.copyWith(vocalTracks: [...practice.vocalTracks, track]),
    );
    if (saved is! Ok<SongPractice>) return false;

    // The bytes are pooled and the rows are committed: the staging copy is now
    // an ordinary leftover. Delete it here; if the delete fails the sweep
    // (this boot) or the referenced-hash skip (next boot) covers it.
    try {
      await file.delete();
    } catch (e) {
      DiagnosticsLog.instance.log(
        'persistence',
        'recovered take staging copy not deleted (${e.runtimeType}); '
            'the sweep covers it',
      );
    }
    return true;
  }

  /// One past the highest existing "Take N" / "Take N (recovered)" label in
  /// the practice's ACTIVE interpretation — the same numbering a normal new
  /// take gets, so a recovered take slots in as "Take N (recovered)" without
  /// colliding with or double-counting user takes.
  static int _nextTakeNumber(SongPractice practice) {
    final re = RegExp(r'^Take (\d+)(?: \(recovered\))?$');
    var max = 0;
    for (final t in practice.activeVocalTracks) {
      final m = re.firstMatch(t.displayName);
      if (m != null) {
        final n = int.parse(m.group(1)!);
        if (n > max) max = n;
      }
    }
    return max + 1;
  }

  /// Content hashes of every asset ANY referencing row — live or tombstoned —
  /// ever pointed at: bytes a save COMPLETED for at some point. Derived from
  /// [AssetStore.assetRefColumns], the single source of truth for
  /// asset-referencing columns (guarded by a schema-derived test), so a future
  /// referencing column cannot be silently missed here while the GC honors it.
  ///
  /// Tombstoned references (and tombstoned assets) count on purpose: a
  /// graceful stop deliberately leaves the streamed staging WAV for the boot
  /// sweep, and deleteAudio refuses pool paths — so when the user deletes
  /// that take in the same session, the staging copy survives while the clip
  /// row (and possibly the asset row) is only tombstoned. If this set covered
  /// live rows only, the next boot would adopt those bytes as a "(recovered)"
  /// take and resurrect the deleted recording. Bytes matching anything EVER
  /// saved are never adopted; they're left to the sweep. The crash-retry
  /// window is unaffected: a crash between the pool import and the clip-row
  /// commit leaves NO referencing row at all (live or tombstoned), so that
  /// take is still recovered.
  Future<Set<String>> _referencedContentHashes() async {
    final union = AssetStore.assetRefColumns
        .map(
          (r) =>
              'SELECT ${r.column} FROM ${r.table} '
              'WHERE ${r.column} IS NOT NULL',
        )
        .join(' UNION ');
    final rows = await db
        .customSelect(
          'SELECT DISTINCT content_hash AS h FROM assets '
          'WHERE id IN ($union)',
        )
        .get();
    return {
      for (final r in rows)
        if (r.data['h'] is String) r.data['h'] as String,
    };
  }

  static Persistence _wire(
    AppDatabase db,
    Directory root,
    String node, {
    HlcClock? clock,
  }) {
    final writer = SyncWriter(db, clock ?? HlcClock(node: node));
    final assets = AssetStore(db: db, root: root, writer: writer);
    return Persistence(
      db: db,
      writer: writer,
      assets: assets,
      songRepo: SongPracticeRepositoryDrift(
        db: db,
        writer: writer,
        assets: assets,
      ),
      techRepo: TechniqueRepositoryDrift(
        db: db,
        writer: writer,
        assets: assets,
      ),
    );
  }

  /// Restores the [HlcClock]'s monotonicity across app restarts by seeding it
  /// with the highest packed HLC ever persisted on this device.
  ///
  /// [HlcClock.tick] guarantees a strictly-increasing stamp only relative to its
  /// IN-MEMORY high-water mark (_last / _maxWallEverSeen). A fresh clock starts
  /// at wall 0, so after a restart that follows a forward clock excursion (the
  /// wall had been held ahead of real time only in memory) a new local edit would
  /// stamp a LOWER hlc than ones this install already pushed to peers — and each
  /// peer's LWW check silently DROPS that edit. Reseeding from the durable max
  /// closes the gap: every synced row (and the oplog) carries hlc_packed — the
  /// fixed-width, lexicographically ordered LWW key — so the global MAX is the
  /// greatest stamp this install ever produced or adopted from a peer.
  static Future<HlcClock> _seedClock(AppDatabase db, String node) async {
    Hlc? max;
    // Seeding is a monotonicity safety net, not a launch precondition: one
    // malformed hlc_packed (or one unreadable table) must never brick startup
    // by throwing out of open() before the window ever shows. Parse per table
    // so a bad value only skips ITS table, and wrap the whole scan so any
    // other failure degrades to whatever was gathered (possibly an unseeded
    // clock) — silent by design, like the other best-effort startup sweeps.
    try {
      // Any table carrying the hlc_packed column (all synced tables + the oplog).
      for (final table in db.allTables.where(
        (t) => t.$columns.any((c) => c.name == 'hlc_packed'),
      )) {
        try {
          final row = await db
              .customSelect(
                'SELECT MAX(hlc_packed) AS m FROM ${table.actualTableName}',
              )
              .getSingleOrNull();
          final packed = row?.readNullable<String>('m');
          if (packed == null) continue;
          final hlc = Hlc.parse(packed);
          if (max == null || hlc > max) max = hlc;
        } catch (e) {
          // Malformed value or unreadable table: keep scanning the others.
          DiagnosticsLog.instance.log(
            'persistence',
            'seedClock skipped ${table.actualTableName} (${e.runtimeType})',
          );
        }
      }
    } catch (e) {
      // Even enumerating the tables failed: fall through with the partial max.
      DiagnosticsLog.instance.log(
        'persistence',
        'seedClock table enumeration failed (${e.runtimeType}); '
            'clock seeded from a partial max',
      );
    }
    return HlcClock(node: node, initial: max);
  }

  /// The shape of a valid device node id: exactly the 26 Crockford base32
  /// characters a [newId] ULID produces (I, L, O, U excluded).
  static final RegExp _nodeIdShape = RegExp(
    r'^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{26}$',
  );

  /// Reads (or mints + persists once) the stable per-install device node id —
  /// the final HLC tiebreak, kept distinct from the data so reinstalling the app
  /// is a new node.
  ///
  /// Durability: the mint path writes temp → fsync → atomic rename → dir
  /// fsync (the [DurableFs] idiom used by the asset pool and the migrator),
  /// because a power cut mid-`writeAsString` could leave an empty or
  /// truncated file — and the node id is this install's sync identity, which
  /// must never silently change. Symmetrically, the read path treats an
  /// unreadable, empty, or malformed existing file as ABSENT (mint fresh)
  /// rather than adopting a corrupted identity.
  static Future<String> _readOrCreateNode(Directory dir) async {
    final f = File(p.join(dir.path, 'device_node'));
    if (await f.exists()) {
      String s = '';
      try {
        s = (await f.readAsString()).trim();
      } catch (_) {
        // Unreadable/undecodable (e.g. truncated to invalid UTF-8): treat as
        // absent — never let a corrupt file brick startup or leak in as an id.
      }
      if (_nodeIdShape.hasMatch(s)) return s;
    }
    final node = newId();
    const fs = DurableFs();
    final tmp = File('${f.path}.tmp');
    await tmp.writeAsString(node, flush: true);
    await fs.fsyncFile(tmp.path);
    await fs.atomicRename(tmp.path, f.path);
    await fs.fsyncDir(dir.path);
    return node;
  }
}
