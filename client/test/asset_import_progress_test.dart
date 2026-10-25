import 'dart:async';
import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/db/durable_fs.dart';
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';

/// Flushes that each take [delay] (F_FULLFSYNC of a large file just copied,
/// on a busy disk), recording what the import reported while each ran.
class _SlowFs extends DurableFs {
  _SlowFs(this.delay);

  final Duration delay;
  final Completer<void> firstFlush = Completer<void>();
  final List<int> readings = [];
  final List<int> inFlight = [];

  Future<void> _flush() async {
    if (!firstFlush.isCompleted) firstFlush.complete();
    readings.add(await AssetStore.bytesBeingCopied());
    inFlight.add(AssetStore.copiesInFlight);
    await Future<void>.delayed(delay);
  }

  @override
  Future<void> fsyncFile(String path) => _flush();

  @override
  Future<void> fsyncDir(String path) => _flush();
}

void main() {
  late AppDatabase db;
  late Directory tmp;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_import_progress');
    // Pre-made, so every flush the slow store sees comes AFTER the copy: the
    // landing tail this file is about.
    await Directory(
      p.join(tmp.path, 'assets', '.staging'),
    ).create(recursive: true);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  AssetStore storeWith(DurableFs fs) => AssetStore(
    db: db,
    root: tmp,
    writer: SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0'))),
    fs: fs,
  );

  Future<String> source() async {
    final f = File(p.join(tmp.path, 'video.mov'));
    await f.writeAsBytes(List<int>.filled(256 * 1024, 7));
    return f.path;
  }

  test('an import stays listed, and its progress keeps moving, while the '
      'copied file is flushed, placed and committed', () async {
    final fs = _SlowFs(const Duration(milliseconds: 20));
    final store = storeWith(fs);
    await store.importFile(await source());

    // Shard dirs, the file, its directory entry: four flushes after the copy.
    expect(fs.readings, hasLength(4));
    expect(
      fs.inFlight,
      everyElement(1),
      reason: 'the import was dropped from the list once its copy finished',
    );
    for (var i = 1; i < fs.readings.length; i++) {
      expect(
        fs.readings[i],
        greaterThan(fs.readings[i - 1]),
        reason:
            'the reading stood still across a finished landing step: '
            '${fs.readings}',
      );
    }
    final after = await AssetStore.bytesBeingCopied();
    expect(after, greaterThan(fs.readings.last), reason: 'the commit counts');
    expect(AssetStore.copiesInFlight, 0);
  });

  test('a quit waits for a save still landing a large file past its budget '
      'instead of cutting it off', () async {
    // Each flush is short, but together they outlast one budget window.
    final fs = _SlowFs(const Duration(milliseconds: 150));
    final store = storeWith(fs);
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 400),
      totalBudget: const Duration(milliseconds: 400),
      progressCeiling: const Duration(seconds: 20),
      progress: AssetStore.bytesBeingCopied,
    );
    final src = await source();
    var landed = false;
    final save = finalizer.hold(() async {
      await store.importFile(src);
      landed = true;
    });

    // Quit once the copy is done and the file is landing.
    await fs.firstFlush.future;
    expect(landed, isFalse);
    await finalizer.finalizeAll();
    final landedWhenQuitReturned = landed;
    await save;

    expect(
      landedWhenQuitReturned,
      isTrue,
      reason: 'the quit exited while the save was still landing its file',
    );
  });
}
