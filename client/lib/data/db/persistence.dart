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
import 'safe_migrator.dart';

/// Signature of the audio analyzer used to rebuild a recovered take's waveform
/// peaks + duration — [WaveformService.analyze] in production, injectable so
/// tests don't need the platform decoder. Must throw on undecodable audio.
typedef WavAnalyzer = Future<WaveformResult> Function(String path);

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
    final engine = SyncEngine(
      db: db,
      applier: SyncApplier(db, writer.clock, quarantine: QuarantineStore(db)),
      drain: OplogDrain(db),
      builder: SyncRowBuilder(db),
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
  static Future<Persistence> open() async {
    final dir = await getApplicationSupportDirectory();
    final dbPath = p.join(dir.path, 'vocara.db');
    await SafeMigrator().recoverIfNeeded(dbPath);
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
    try {
      final waveforms = WaveformService(storage: LocalStorageService());
      await persistence.recoverStagedRecordings(analyze: waveforms.analyze);
    } catch (_) {}
    // Reclaim staging leftovers: after a save, a recording/import has been copied
    // into the content-addressed pool and its working file under audio/ or
    // technique_media/ is an orphan (nothing persistent references it — clips
    // and media reference pool assets). At startup nothing is in flight, so the
    // staging tree is safe to clear. Best-effort; never blocks launch.
    await persistence.assets.sweepStaging();
    // Reclaim pooled bytes for assets no live clip/media/practice references any
    // more (removed takes, replaced accompaniment, deleted examples). Derives the
    // refcount from a query, so it's safe to run at startup when nothing is in
    // flight. Best-effort — a GC failure must never block launch.
    try {
      await persistence.assets.sweepUnreferenced();
    } catch (_) {}
    // Reclaim pool files left with no asset row at all (a crash between the pool write and the
    // asset-row commit). Best-effort — a GC failure must never block launch.
    try {
      await persistence.assets.sweepOrphanPoolFiles();
    } catch (_) {}
    return persistence;
  }

  /// A synchronous in-memory instance (host sqlite3) for tests / widget boot.
  factory Persistence.memory() {
    final dir = Directory.systemTemp.createTempSync('vocara_mem');
    return _wire(AppDatabase(NativeDatabase.memory()), dir, newId());
  }

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
  /// then (2) delete the staging file. A file is only recovered when NO live
  /// clip / technique-media / separation reference carries its content hash,
  /// so:
  ///  * killed before the rows commit → nothing references the hash yet; the
  ///    next boot retries the whole recovery (the pool import dedups by hash,
  ///    so a half-done import is reused, never duplicated);
  ///  * killed between the rows commit and the file delete → the new clip row
  ///    references the hash; the next boot SKIPS the file and the sweep
  ///    deletes it — the take can never be attached twice.
  Future<int> recoverStagedRecordings({required WavAnalyzer analyze}) async {
    final staging = Directory(p.join(assets.root.path, 'audio'));
    if (!await staging.exists()) return 0;

    // Content hashes referenced by any live row (clips, technique media, the
    // kept separation source) — the same reference derivation as
    // [AssetStore.sweepUnreferenced]. A staging file whose bytes match one was
    // already saved into the pool and must NOT become a second take. Note the
    // set covers only rows a save COMPLETED for: an asset row alone (a crash
    // between the pool import and the row commit) does not mark its file as
    // saved, so that take is still recovered on the retry.
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
        } catch (_) {
          // Best-effort per file: a failed recovery (decode error, disk
          // error) leaves the file to the sweep and never blocks the other
          // orphans or the launch.
        }
      }
    }
    return recovered;
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
    // 16-bit PCM MONO WAV with a non-empty payload. Staged imports are almost
    // never that shape, and misreviving an import as a "take" would be worse
    // than sweeping it. An empty payload (killed before the first ~1 s header
    // patch) carries no audio worth keeping.
    final probe = await AudioFileService.probeWav(file);
    if (probe == null ||
        probe.channels != 1 ||
        probe.bitsPerSample != 16 ||
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
    } catch (_) {}
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

  /// Content hashes of every asset a live clip / technique media / practice
  /// separation source references — bytes a completed save already owns.
  Future<Set<String>> _referencedContentHashes() async {
    final rows = await db
        .customSelect(
          'SELECT DISTINCT content_hash AS h FROM assets '
          'WHERE deleted_at IS NULL AND id IN ('
          'SELECT asset_id FROM clips WHERE deleted_at IS NULL '
          'UNION SELECT asset_id FROM technique_media WHERE deleted_at IS NULL '
          'UNION SELECT separation_asset_id FROM practices '
          'WHERE deleted_at IS NULL AND separation_asset_id IS NOT NULL)',
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
        } catch (_) {
          // Malformed value or unreadable table: keep scanning the others.
        }
      }
    } catch (_) {
      // Even enumerating the tables failed: fall through with the partial max.
    }
    return HlcClock(node: node, initial: max);
  }

  /// Reads (or mints + persists once) the stable per-install device node id —
  /// the final HLC tiebreak, kept distinct from the data so reinstalling the app
  /// is a new node.
  static Future<String> _readOrCreateNode(Directory dir) async {
    final f = File(p.join(dir.path, 'device_node'));
    if (await f.exists()) {
      final s = (await f.readAsString()).trim();
      if (s.isNotEmpty) return s;
    }
    final node = newId();
    await f.writeAsString(node);
    return node;
  }
}
