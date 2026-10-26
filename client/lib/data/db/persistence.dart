import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:drift/native.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;
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
import '../sync/sync_hashes.dart' show Loaded;
import '../sync/sync_row.dart';
import '../sync/sync_state_store.dart';
import '../sync/sync_transport.dart';
import '../sync/sync_writer.dart';
import 'app_database.dart';
import 'asset_store.dart';
import 'durable_fs.dart';
import 'library_lock.dart';
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

/// Thrown by [Persistence.open] when the library needs a schema upgrade but
/// another Vocara process holds it ([LibraryLock]). The upgrade swaps the
/// database file, which must never happen under a process that has it open,
/// and drift cannot open the stale file without it.
class LibraryInUseException implements Exception {
  const LibraryInUseException();

  @override
  String toString() =>
      'LibraryInUseException: the library needs an upgrade but another '
      'Vocara is using it';
}

/// What one [Persistence.repairLibrary] pass found and did.
class RepairReport {
  const RepairReport({
    required this.violationsFound,
    required this.violationsRepaired,
    required this.assetsSwept,
    this.corruptPoolFiles = 0,
  });

  /// Cross-row integrity violations detected before repairing.
  final int violationsFound;

  /// How many of those the repair pass resolved.
  final int violationsRepaired;

  /// Unreferenced assets tombstoned plus orphaned pool files reclaimed.
  final int assetsSwept;

  /// Pooled files whose bytes no longer hash to the address they are stored
  /// under (see [AssetStore.verifyPoolIntegrity]). Reported, never touched:
  /// the file is still the user's recording.
  final int corruptPoolFiles;

  @override
  String toString() =>
      'RepairReport(found: $violationsFound, repaired: $violationsRepaired, '
      'assetsSwept: $assetsSwept, corruptPoolFiles: $corruptPoolFiles)';
}

/// How one staged WAV's boot recovery ended. [junk] and [failed] are both
/// "not recovered", but only [junk] lets the staging sweep take the file:
/// nothing may trash the only copy of a take on the strength of a failure the
/// environment caused.
enum _RecoveryOutcome {
  /// Attached to its practice as a new take; the staging copy is a leftover.
  recovered,

  /// Provably not a recoverable take: not the shape the recorder writes, bytes
  /// that some save already committed, or a practice that no longer exists.
  junk,

  /// The attempt could not complete — an unreadable file, a decoder failure, a
  /// store that reported an error. Held back for the next boot to retry.
  failed,
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
    this.ownsLibrary = true,
    LibraryGuard? guard,
  }) : _guard = guard ?? LibraryGuard.fixed(exclusive: true);

  /// Whether this instance booted as the library's owner: it had the
  /// single-instance lock ([LibraryLock]) exclusively, so no other Vocara
  /// process had the library open. Only then did [open] run boot recovery
  /// and the boot sweeps: in a library another process has open, its
  /// in-flight recordings and imports look exactly like the leftovers those
  /// passes reclaim.
  final bool ownsLibrary;

  /// The single-instance guard [repairLibrary] asks again before it
  /// reclaims anything: another instance may have opened the library since
  /// boot, or the one that was open then may have quit.
  final LibraryGuard _guard;

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
  /// [dir], [analyze] and [libraryLock] are test seams (the store's on-disk
  /// root, the recovery waveform analyzer, and the single-instance lock,
  /// which cannot conflict within one process); production passes none.
  ///
  /// Throws [NewerLibraryException] when the library was written by a newer
  /// Vocara, and [LibraryInUseException] when it needs an upgrade while
  /// another Vocara process holds it; any other throw is a fatal open,
  /// surfaced by main() in a dedicated error window. A library another
  /// process holds otherwise opens normally, minus boot recovery and the
  /// boot sweeps (see [ownsLibrary]).
  ///
  /// [onWaitingForLibrary] is called once if another Vocara's boot keeps this
  /// launch waiting for its hold on the library (see [LibraryLock]): the app
  /// shows a window meanwhile. The wait has no deadline — going on without
  /// the hold left the work of this instance where that boot's sweeps could
  /// trash it.
  static Future<Persistence> open({
    Directory? dir,
    WavAnalyzer? analyze,
    Future<LibraryGuard> Function(Directory dir)? libraryLock,
    void Function()? onWaitingForLibrary,
  }) async {
    dir ??= await getApplicationSupportDirectory();
    // The single-instance guard comes first: a half-finished migration's
    // marker may belong to another process that is migrating right now. This
    // instance holds the lock from here on (see [LibraryLock]); it boots as
    // the owner only if it took the lock exclusive, which no process can
    // while any other has the library open, and hands it back to shared when
    // the boot is done.
    final guard =
        await (libraryLock ??
            (d) => LibraryLock.forLibrary(
              d,
              onStillWaiting: onWaitingForLibrary,
            ))(dir);
    final ownsLibrary = await guard.tryExclusive();
    try {
      return await _open(dir, analyze, guard, ownsLibrary);
    } finally {
      if (ownsLibrary) await guard.releaseExclusive();
    }
  }

  static Future<Persistence> _open(
    Directory dir,
    WavAnalyzer? analyze,
    LibraryGuard guard,
    bool ownsLibrary,
  ) async {
    final dbPath = p.join(dir.path, 'vocara.db');
    if (ownsLibrary) {
      await SafeMigrator().recoverIfNeeded(dbPath);
    } else {
      // A migration another process left unfinished (it died mid-swap) is
      // the owner's to recover; opened as it stands, a missing live file
      // became an empty library.
      if (await SafeMigrator().migrationPending(dbPath)) {
        throw const LibraryInUseException();
      }
      DiagnosticsLog.instance.log(
        'persistence',
        'another Vocara process holds this library; skipping migration '
            'recovery, boot recovery and every boot sweep',
      );
    }
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
    if (ownsLibrary) {
      await SafeMigrator().upgradeIfNeeded(
        dbPath: dbPath,
        targetVersion: AppDatabase.currentSchemaVersion,
      );
    } else if (await File(dbPath).exists()) {
      final onDiskVersion = SafeMigrator().readUserVersion(dbPath);
      if (onDiskVersion != 0 &&
          onDiskVersion < AppDatabase.currentSchemaVersion) {
        throw const LibraryInUseException();
      }
    }
    final db = AppDatabase(NativeDatabase.createInBackground(File(dbPath)));
    final node = await _readOrCreateNode(dir);
    // Restore the HLC clock's monotonicity across restarts BEFORE wiring the
    // writer (the startup sweeps below tick it), so a new local edit can never
    // stamp LOWER than one this install already pushed. See _seedClock.
    final clock = await _seedClock(db, node);
    final persistence = _wire(
      db,
      dir,
      node,
      clock: clock,
      ownsLibrary: ownsLibrary,
      guard: guard,
    );
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
    //
    // And only in the process that holds the library ([ownsLibrary]): in
    // another one's library, a staged WAV with no row is its take still
    // recording, not a crashed one.
    var sweepGateOpen = false;
    try {
      if (ownsLibrary) {
        await _probeDbReadable(db);
        final wavAnalyzer =
            analyze ?? WaveformService(storage: LocalStorageService()).analyze;
        await persistence.recoverStagedRecordings(analyze: wavAnalyzer);
        sweepGateOpen = true;
      }
    } catch (e) {
      DiagnosticsLog.instance.log(
        'persistence',
        'boot health probe / staged-recording recovery failed '
            '(${e.runtimeType}); skipping all destructive sweeps this boot',
      );
    }
    // Waveform intermediates an analysis left when the app quit or was killed
    // mid-way. Derived scratch, never a copy of anything, so it needs no gate;
    // but only now, once recovery's own analyses (which write there too) are
    // done, and before any screen can start one. And only as the owner: in a
    // library another instance has open, a .wave there is its analysis in
    // flight (a take's stop-and-save among them).
    if (ownsLibrary) await WaveformService.sweepScratch(dir);
    if (sweepGateOpen) {
      // A migration's rollback copy has served its purpose once the migrated
      // database has answered real reads and recovery ran on it. Kept on any
      // boot that got less far.
      try {
        await SafeMigrator().discardBackup(dbPath);
      } catch (e) {
        DiagnosticsLog.instance.log(
          'persistence',
          'pre-migration backup not discarded (${e.runtimeType})',
        );
      }
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
  ///
  /// With sync attached, the pass also runs in the engine's serial section:
  /// between two pages of a pull run the store can hold a half-delivered
  /// aggregate (a live child whose parent's revive is on the next page), and
  /// a repair judged against it would journal derived intent that reaches
  /// every device. That section is taken FIRST and the gate inside it: held
  /// the other way round, the gate's exclusive side stayed held for as long
  /// as the pull run took to finish, and every save, import and take stalled
  /// behind it. Neither order deadlocks — the engine never takes [writeGate],
  /// and nothing holding the gate waits on a pump.
  ///
  /// The pool's content check ([AssetStore.verifyPoolIntegrity]) runs after
  /// both, outside them: it only reads, and inside the gate a re-hash of the
  /// whole pool held every save — the one that arms REC included — for as long
  /// as it took, minutes for a large library.
  Future<RepairReport> repairLibrary() async {
    final sync = _sync;
    final repaired = await (sync == null
        ? _repairPass()
        : sync.runExclusive(_repairPass));
    // After the sweeps, so files on their way out are not re-hashed; one a
    // later sweep takes mid-pass reads as unreadable, not corrupt. Reports
    // only — see AssetStore.verifyPoolIntegrity for why nothing is touched.
    final corrupt = await assets.verifyPoolIntegrity();
    for (final path in corrupt) {
      DiagnosticsLog.instance.log(
        'persistence',
        'pooled file does not match its content address: $path',
      );
    }
    final report = RepairReport(
      violationsFound: repaired.violationsFound,
      violationsRepaired: repaired.violationsRepaired,
      assetsSwept: repaired.assetsSwept,
      corruptPoolFiles: corrupt.length,
    );
    DiagnosticsLog.instance.log('persistence', 'repairLibrary: $report');
    return report;
  }

  Future<RepairReport> _repairPass() => writeGate.exclusive(() async {
    final checker = IntegrityChecker(db, writer);
    final found = await checker.check();
    // One transaction around the whole repair fixpoint, exactly like the
    // sync engine's end-of-pull repair: each individual repair is a row
    // UPDATE plus its oplog entry, and the invariant "a row mutation and
    // its journal entry commit in the SAME transaction" must hold here too
    // — two autocommit statements would let a crash between them
    // tombstone/heal a synced row with no journal entry, so the change
    // would never propagate.
    final unrepaired = await db.transaction(() => checker.repair());
    // A cascade can repair MORE than the initial scan saw; the report never
    // claims a negative remainder.
    final repaired = (found.length - unrepaired.length).clamp(0, found.length);
    // Reclaiming needs the library to this process alone, asked NOW, not at
    // boot: another instance's in-flight import is an unreferenced asset, a
    // pool file with no row or a `.part` in the pool's staging until its save
    // commits. Without exclusive, reclaim nothing.
    final reclaim = await _guard.tryExclusive();
    var tombstoned = 0;
    var orphaned = 0;
    try {
      if (reclaim) {
        tombstoned = await assets.sweepUnreferenced();
        orphaned = await assets.sweepOrphanPoolFiles();
      }
    } finally {
      if (reclaim) await _guard.releaseExclusive();
    }
    return RepairReport(
      violationsFound: found.length,
      violationsRepaired: repaired,
      assetsSwept: tombstoned + orphaned,
    );
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
  ///    [OplogDrain.ack] carries it the same way for the same reason — any
  ///    deletion of oplog history owes the survivors that inheritance. Not to
  ///    a TOMBSTONE though: a delete references no bytes, and gating one on a
  ///    since-collected asset holds it — and, through the ack watermark, the
  ///    whole ledger behind it — forever. The subquery needs no upper seq
  ///    bound here because the target is already each entity's MAX(seq), so
  ///    nothing can follow it (unlike [OplogDrain.ack], which repairs rows
  ///    that still have newer entries above them).
  ///  * synced = true rows cannot exist: nothing ever sets the flag — the ack
  ///    path hard-DELETES instead (see [OplogDrain.ack]) — so the SQL needs
  ///    no synced filter; keep-latest-per-entity alone is exact. If a future
  ///    change ever starts marking rows synced, revisit this predicate.
  ///
  /// Run from the gated boot maintenance pass (under [writeGate] exclusive,
  /// nothing in flight); one transaction so a crash can never leave a
  /// dependency carried but its history half-deleted, or vice versa.
  Future<int> compactOplog() => db.transaction(() async {
    await db.customStatement(compactCarrySql);
    return db.customUpdate(
      'DELETE FROM oplog WHERE seq NOT IN ('
      'SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id)',
    );
  });

  /// The dependency carry-forward [compactOplog] runs before it collapses
  /// history. Its correlated subqueries run once per surviving entry, so they
  /// must resolve through ix_oplog_entity (a test pins the plan). Only an
  /// entry that actually inherits something is written (see
  /// [OplogDrain.ackCarrySql]: an UPDATE rewrites every row it matches, and
  /// this one used to match the whole surviving ledger on every boot).
  @visibleForTesting
  static const String compactCarrySql =
      'UPDATE oplog SET depends_on_asset_id = ('
      'SELECT o.depends_on_asset_id FROM oplog o '
      'WHERE o.entity_table = oplog.entity_table '
      'AND o.entity_id = oplog.entity_id '
      'AND o.depends_on_asset_id IS NOT NULL '
      'ORDER BY o.seq DESC LIMIT 1'
      ') WHERE depends_on_asset_id IS NULL AND op <> \'tombstone\' '
      'AND seq IN ('
      'SELECT MAX(seq) FROM oplog GROUP BY entity_table, entity_id) '
      'AND EXISTS (SELECT 1 FROM oplog o '
      'WHERE o.entity_table = oplog.entity_table '
      'AND o.entity_id = oplog.entity_id '
      'AND o.depends_on_asset_id IS NOT NULL)';

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
    //
    // GROWS as this pass runs: each recovery adds its own hash, so two staging
    // files with identical bytes cannot both become takes in one boot.
    final referenced = await _referencedContentHashes();

    final candidates = <({String practiceId, File file})>[];
    await for (final dir in staging.list()) {
      if (dir is! Directory) continue;
      final practiceId = p.basename(dir.path);
      // Files directly in the practice folder only: imports and edit outputs
      // stage in subdirectories precisely so this scan never adopts them.
      await for (final f in dir.list()) {
        if (f is! File || p.extension(f.path).toLowerCase() != '.wav') {
          continue;
        }
        candidates.add((practiceId: practiceId, file: f));
      }
    }
    // Every staged take is hashed before it can be judged, and a graceful
    // stop leaves each of the last session's takes here for the sweep, so
    // this boot pass re-reads all of them before the window shows. Hash them
    // ahead of the sequential walk, a few background isolates at a time; the
    // walk itself — and so every outcome — is unchanged.
    final hashes = _StagedHashes([for (final c in candidates) c.file]);

    var recovered = 0;
    for (final (:practiceId, file: f) in candidates) {
      try {
        switch (await _recoverOrphanTake(
          f,
          practiceId,
          referenced,
          analyze,
          hashes,
        )) {
          case _RecoveryOutcome.recovered:
            recovered++;
          case _RecoveryOutcome.junk:
            break; // provably not a recoverable take: leave it to the sweep
          case _RecoveryOutcome.failed:
            await _holdBackForRetry(f, 'the store reported a failure');
        }
      } catch (e) {
        await _holdBackForRetry(f, 'the attempt threw (${e.runtimeType})');
      }
    }
    return recovered;
  }

  /// Staging files whose recovery attempt FAILED this boot — exempted from the
  /// boot staging sweep so the next boot's recovery retries them. Repopulated
  /// by every [recoverStagedRecordings] run.
  Set<String> get recoveryRetryPaths => Set.unmodifiable(_recoveryRetryPaths);
  final Set<String> _recoveryRetryPaths = <String>{};

  /// Exempts [f] from this boot's staging sweep so the next boot's recovery
  /// tries again.
  ///
  /// A failed recovery never blocks the other orphans or the launch, but a
  /// failure is an environmental symptom (decode error, disk error, a store
  /// that could not commit), not proof the file is junk — one systemic
  /// breakage would otherwise funnel EVERY orphan to trash this boot, with
  /// staging empty next boot so recovery never retries. Bounded: a file that
  /// has kept failing for over 7 days since its first failed attempt, and on
  /// at least [_recoveryRetryMinAttempts] boots, goes to the sweep after all
  /// (the trash then still holds it for 30 days).
  Future<void> _holdBackForRetry(File f, String why) async {
    if (await _stillRetrying(f)) {
      _recoveryRetryPaths.add(f.path);
    }
    DiagnosticsLog.instance.log(
      'persistence',
      'one staged recording failed to recover ($why); exempted from this '
          'boot\'s sweep if it has not kept failing too long',
    );
  }

  /// How long a repeatedly-failing staged recording is protected from the
  /// boot sweep before it is released to it (trash retention then still
  /// applies on top).
  static const Duration _recoveryRetryMaxAge = Duration(days: 7);

  /// How many boots must have failed to recover a staged recording before
  /// [_recoveryRetryMaxAge] can release it.
  static const int _recoveryRetryMinAttempts = 3;

  /// Where a held-back staged recording keeps the time of its first failed
  /// recovery (milliseconds since the epoch) and how many boots have failed
  /// it since. The sweep keeps it with the file and takes it with the file.
  static String _firstFailureStamp(String path) => '$path.retry';

  /// Whether [f] is still held back for another attempt: it first failed to
  /// recover less than [_recoveryRetryMaxAge] ago, or has failed on fewer
  /// than [_recoveryRetryMinAttempts] boots, counting this one. Measured
  /// from the file's modification time, a crash left untouched for a week
  /// (the app not opened since) was released on its first failed attempt;
  /// measured by time alone, on its second after another week away.
  ///
  /// A stamp that cannot be read or lies in the future starts over, and
  /// anything that throws keeps the file: lingering is always safer than
  /// trashing.
  static Future<bool> _stillRetrying(File f) async {
    try {
      final stamp = File(_firstFailureStamp(f.path));
      final now = DateTime.now();
      DateTime? first;
      var attempts = 0;
      try {
        final parts = (await stamp.readAsString()).trim().split(' ');
        final ms = int.tryParse(parts.first);
        if (ms != null) {
          first = DateTime.fromMillisecondsSinceEpoch(ms);
          attempts = parts.length > 1 ? int.tryParse(parts[1]) ?? 1 : 1;
        }
      } catch (_) {
        // None yet (this is the first failure), or unreadable.
      }
      if (first == null || first.isAfter(now)) {
        first = now;
        attempts = 0;
      }
      attempts++;
      await stamp.writeAsString(
        '${first.millisecondsSinceEpoch} $attempts',
        flush: true,
      );
      return now.difference(first) < _recoveryRetryMaxAge ||
          attempts < _recoveryRetryMinAttempts;
    } catch (_) {
      return true;
    }
  }

  /// Recovers one staged WAV as a new vocal take of [practiceId].
  ///
  /// The two non-recovered outcomes are deliberately NOT the same answer: only
  /// [_RecoveryOutcome.junk] releases the file to the staging sweep. Every
  /// store call below reports failure as a [Result], not a throw (see
  /// [SongPracticeRepositoryDrift]), so a disk-full or unreadable-database
  /// boot would otherwise look exactly like "this file is not a take" and
  /// trash the only copy of a recording — the very case the retry hold-back
  /// exists for.
  Future<_RecoveryOutcome> _recoverOrphanTake(
    File file,
    String practiceId,
    Set<String> referencedHashes,
    WavAnalyzer analyze,
    _StagedHashes hashes,
  ) async {
    // Only what the recording engine writes is a candidate: a canonical-header
    // MONO WAV — 32-bit float today, 16-bit PCM from pre-float versions — with
    // a non-empty payload. Imports and edit outputs can have that shape, so
    // they stage one level down (`imports/`, `edits/`), which the scan above
    // never enters: misreviving one as a "take" would be worse than sweeping
    // it. An empty payload (killed before the first ~1 s header patch) carries
    // no audio worth keeping.
    // An UNREADABLE file is not junk. `probeWav` answers null only for shape
    // (too short / wrong layout / not PCM / gone) and THROWS on any other I/O
    // failure, so a permission denial or a hardware fault leaves here as an
    // exception into the caller's retry hold-back. Answering "not a recording"
    // on the strength of a read that never happened would sweep the only copy
    // of a crashed take into the trash, where the 30-day prune eventually
    // deletes it — and no later boot rescans the trash.
    if (!_isRecorderShaped(await AudioFileService.probeWav(file))) {
      return _RecoveryOutcome.junk;
    }

    // Bytes already referenced by a completed save → a normal leftover, not an
    // orphaned take (this is also what makes a re-run after a mid-recovery
    // crash a no-op instead of a duplicate take). Asked first by the hash the
    // save recorded when it pooled this file: a power cut soon after that
    // save can bring the never-flushed staging file back holding older bytes,
    // whose own hash matches nothing (see [AssetStore.importedHashOf]).
    final imported = await AssetStore.importedHashOf(file);
    if (imported != null && referencedHashes.contains(imported)) {
      return _RecoveryOutcome.junk;
    }
    final hash =
        await hashes.of(file) ?? await AssetStore.sha256OfFile(file.path);
    if (referencedHashes.contains(hash)) return _RecoveryOutcome.junk;

    // The practice must still exist and be live (tombstones filtered); an
    // underivable id lands here too and the file is swept. Asked as its own
    // query first BECAUSE getPractice collapses "gone" and "the read failed"
    // into one Result.error, and only the former is proof the file is junk —
    // this select throws on a database failure and holds the file back.
    if (!await _practiceIsLive(practiceId)) return _RecoveryOutcome.junk;
    // Loaded with its base, and saved against it as an editor saves: the
    // base-less save took the store's own rows as the base, so every row this
    // build re-encodes differently than it was stored (a field another build
    // wrote) read as a local edit. The practice row and every take's and
    // bus's chain were rewritten (that field stripped) and journaled, though
    // only the recovered take was new.
    final res = await songRepo.loadPractice(practiceId);
    if (res is! Ok<Loaded<SongPractice>>) return _RecoveryOutcome.failed;
    final practice = res.value.aggregate;

    // Same take construction as a graceful stop with nothing armed: analyze
    // the audio, append a fresh single-clip vocal take to the active
    // interpretation, save. The killed session's playhead is unknowable, so
    // the clip lands at the timeline origin.
    final analysis = await analyze(file.path);
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take ${practice.nextTakeNumber} (recovered)',
      audioFilePath: file.path,
      durationMs: analysis.durationMs,
      startMs: 0,
      waveformPeaks: analysis.peaks,
      createdAt: DateTime.now(),
      versionId: practice.activeVersion?.id,
    );
    // Only the take is new: the practice row is left as stored even when the
    // take runs past the stored timeline (the editor grows it in memory on
    // open, and nothing else reads it). Growing it here made the row a local
    // edit again: rewritten with fields another build wrote stripped, and
    // journaled at boot, before sync pulls, as a whole-row write that would
    // override what another device changed in it meanwhile.
    final saved = await songRepo.savePracticeWithBase(
      practice.copyWith(vocalTracks: [...practice.vocalTracks, track]),
      baseHashes: res.value.baseHashes,
    );
    if (saved is! Ok<Loaded<SongPractice>>) return _RecoveryOutcome.failed;

    // These bytes are now referenced by a committed clip row, so the SAME BOOT
    // must treat a second file carrying them as an already-saved leftover — the
    // set was read once before the scan and would otherwise still say "never
    // saved". Two byte-identical staging WAVs (two silent takes of equal
    // length, or the same file left in two practice folders) each became their
    // own "(recovered)" take. Only the next boot's re-read closed it.
    referencedHashes.add(hash);

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
    return _RecoveryOutcome.recovered;
  }

  /// The shape the recording engine writes (see [_recoverOrphanTake]).
  static bool _isRecorderShaped(WavProbe? probe) =>
      probe != null &&
      probe.channels == 1 &&
      (probe.bitsPerSample == 16 || probe.bitsPerSample == 32) &&
      probe.dataBytes > 0;

  /// Whether a LIVE (non-tombstoned) practice row with [id] exists. Throws on
  /// a database failure — the caller depends on that to tell "gone" apart
  /// from "could not be read".
  Future<bool> _practiceIsLive(String id) async {
    final row = await (db.select(
      db.practices,
    )..where((p) => p.id.equals(id))).getSingleOrNull();
    return row != null && row.deletedAt == null;
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
    bool ownsLibrary = true,
    LibraryGuard? guard,
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
      ownsLibrary: ownsLibrary,
      guard: guard,
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
  ///
  /// The tables are not the whole story, which is why the applier's durable
  /// high-water mark ([SyncStateStore.hlcHighWaterKey]) is folded in as well: a
  /// verified remote stamp whose record the FK sweep parked advanced the live
  /// clock but left no row behind to be found here.
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
    // The stamps that never became rows (see the doc above). Same best-effort
    // stance: a malformed or unreadable mark only costs its own contribution.
    try {
      final packed = await SyncStateStore(db).hlcHighWater();
      if (packed != null) {
        final hlc = Hlc.parse(packed);
        if (max == null || hlc > max) max = hlc;
      }
    } catch (e) {
      DiagnosticsLog.instance.log(
        'persistence',
        'seedClock skipped the durable high-water mark (${e.runtimeType})',
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

/// Boot recovery's staged-take hashes, computed AHEAD of its sequential walk
/// in up to [parallel] background isolates, in walk order — so by the time the
/// walk reaches a file its hash is usually done, and the main isolate never
/// spends the read. Only recorder-shaped files are hashed (a cheap header
/// probe first), matching what the walk itself would hash.
///
/// Purely an accelerator: any failure here — a probe or read that throws, a
/// file that changed shape — just answers null, and the walk then probes and
/// hashes the file itself exactly as before, meeting the same error at the
/// same point. A hash is a pure function of the bytes, and nothing writes a
/// staged file during boot, so a prefetched hash IS the one the walk would
/// have computed — and so is the one its save's import already measured
/// ([AssetStore.knownHashOf]), which spares re-reading the session's takes.
class _StagedHashes {
  _StagedHashes(this._files, {int parallel = 4}) {
    for (final f in _files) {
      _results[f.path] = Completer<String?>();
    }
    final workers = math.min(
      math.min(parallel, Platform.numberOfProcessors),
      _files.length,
    );
    for (var i = 0; i < workers; i++) {
      unawaited(_work());
    }
  }

  final List<File> _files;
  final Map<String, Completer<String?>> _results = {};
  var _next = 0;

  Future<void> _work() async {
    while (_next < _files.length) {
      final f = _files[_next++];
      String? hash;
      try {
        if (Persistence._isRecorderShaped(await AudioFileService.probeWav(f))) {
          hash =
              await AssetStore.knownHashOf(f) ??
              await AssetStore.sha256OfFile(f.path);
        }
      } catch (_) {
        hash = null; // the walk meets this again itself (see the class doc)
      }
      _results[f.path]!.complete(hash);
    }
  }

  /// [file]'s prefetched sha256, or null when the walk must hash it itself.
  Future<String?> of(File file) =>
      _results[file.path]?.future ?? Future<String?>.value();
}
