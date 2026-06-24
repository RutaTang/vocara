import 'dart:io';

import 'package:drift/native.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import '../../utils/id.dart';
import '../repositories/song_practice_repository_drift.dart';
import '../repositories/technique_repository_drift.dart';
import '../sync/hlc.dart';
import '../sync/sync_writer.dart';
import 'app_database.dart';
import 'asset_store.dart';
import 'safe_migrator.dart';

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

  /// Opens the real on-disk store under the app support directory. First
  /// recovers any half-finished schema migration (a crash mid-swap) so the live
  /// file is always consistent before drift opens it.
  static Future<Persistence> open() async {
    final dir = await getApplicationSupportDirectory();
    final dbPath = p.join(dir.path, 'vocara.db');
    await SafeMigrator().recoverIfNeeded(dbPath);
    // (When schemaVersion grows, run SafeMigrator().migrate(...) here, BEFORE
    //  opening, so drift never sees a stale on-disk version.)
    final db = AppDatabase(NativeDatabase.createInBackground(File(dbPath)));
    final node = await _readOrCreateNode(dir);
    final persistence = _wire(db, dir, node);
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

  static Persistence _wire(AppDatabase db, Directory root, String node) {
    final writer = SyncWriter(db, HlcClock(node: node));
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
