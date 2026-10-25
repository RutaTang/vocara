// The single-instance guard: only the process holding `<support>/vocara.lock`
// runs boot recovery and the destructive boot sweeps. In a library another
// Vocara process has open, a staged WAV with no row is that process's take
// still recording, and a pool file with no row is its import still landing.

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:sqlite3/sqlite3.dart';
import 'package:vocara/data/db/library_lock.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/result.dart';

/// Whether a `python3` with fcntl is on PATH, to hold the lock from a
/// genuinely separate process (POSIX record locks never conflict within one).
final bool _hasPython = () {
  try {
    return Process.runSync('python3', ['-c', 'import fcntl']).exitCode == 0;
  } catch (_) {
    return false;
  }
}();

const _noPython = 'needs python3 to hold the lock from another process';

/// Holds an exclusive fcntl lock on [path] from another process until
/// [release]d.
class _OtherProcess {
  _OtherProcess._(this._process);

  final Process _process;

  static Future<_OtherProcess> lock(String path) async {
    final process = await Process.start('python3', [
      '-c',
      'import fcntl, sys\n'
          'f = open(sys.argv[1], "a")\n'
          'fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n'
          'print("locked", flush=True)\n'
          'sys.stdin.read()\n',
      path,
    ]);
    final first = await process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .first;
    expect(first, 'locked', reason: 'the helper process took the lock');
    return _OtherProcess._(process);
  }

  Future<void> release() async {
    await _process.stdin.close();
    await _process.exitCode;
  }

  /// Whether another process can take the lock on [path] right now.
  static Future<bool> tryLock(String path) async {
    final result = await Process.run('python3', [
      '-c',
      'import fcntl, sys\n'
          'f = open(sys.argv[1], "a")\n'
          'try:\n'
          '    fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n'
          '    print("got")\n'
          'except OSError:\n'
          '    print("held")\n',
      path,
    ]);
    return (result.stdout as String).trim() == 'got';
  }
}

void main() {
  late Directory root;

  setUp(() async {
    root = await Directory.systemTemp.createTemp('vocara_single_instance');
  });
  tearDown(() async {
    if (await root.exists()) await root.delete(recursive: true);
  });

  Future<WaveformResult> fakeAnalyze(String path) async =>
      (peaks: <double>[0.1, 0.2], durationMs: 500);

  /// Mono 16-bit PCM: the shape the recording engine streams takes in.
  Uint8List monoWav(int samples) => writeWavMono(
    Float64List.fromList([for (var i = 0; i < samples; i++) 0.25]),
    48000,
  );

  /// Creates the library with one practice and one pooled asset (so the
  /// orphan-pool sweep has live hashes to judge against), then closes it.
  /// With [takeLock] false the real lock file is left alone, so another
  /// process can take it next (this process would otherwise hold it for the
  /// rest of the run).
  Future<String> createPracticeAndClose({bool takeLock = true}) async {
    final persistence = await Persistence.open(
      dir: root,
      analyze: fakeAnalyze,
      libraryLock: takeLock ? null : (_) async => true,
    );
    final res = await persistence.songRepo.createPractice('Song');
    final src = File(p.join(root.path, 'backing.wav'));
    await src.writeAsBytes([9, 8, 7], flush: true);
    await persistence.assets.importFile(src.path);
    await persistence.db.close();
    return (res as Ok<SongPractice>).value.id;
  }

  /// What another process's in-flight work looks like on disk: a take still
  /// streaming into staging, an import staged but not saved, and a file
  /// already placed in the pool whose asset row has not committed yet.
  Future<({File take, File import, File pooled})> plantInFlightWork(
    String practiceId,
  ) async {
    final take = File(p.join(root.path, 'audio', practiceId, 'take.wav'));
    await take.parent.create(recursive: true);
    await take.writeAsBytes(monoWav(4800), flush: true);
    final import = File(p.join(root.path, 'technique_media', 'T', 'ex.mov'));
    await import.parent.create(recursive: true);
    await import.writeAsBytes([1, 2, 3], flush: true);
    final hash = 'ab' * 32;
    final pooled = File(p.join(root.path, 'assets', 'ab', 'ab', '$hash.wav'));
    await pooled.parent.create(recursive: true);
    await pooled.writeAsBytes([4, 5, 6], flush: true);
    return (take: take, import: import, pooled: pooled);
  }

  Future<void> expectUntouched(
    Persistence persistence,
    String practiceId,
    ({File take, File import, File pooled}) work,
  ) async {
    expect(await work.take.exists(), isTrue, reason: 'take still recording');
    expect(await work.import.exists(), isTrue, reason: 'import not saved yet');
    expect(await work.pooled.exists(), isTrue, reason: 'import still landing');
    final practice =
        (await persistence.songRepo.getPractice(practiceId) as Ok<SongPractice>)
            .value;
    expect(
      practice.vocalTracks,
      isEmpty,
      reason: 'the live take was adopted as "(recovered)"',
    );
  }

  test('a boot that finds the library held by another process recovers and '
      'sweeps nothing', () async {
    final practiceId = await createPracticeAndClose();
    final work = await plantInFlightWork(practiceId);

    final persistence = await Persistence.open(
      dir: root,
      analyze: fakeAnalyze,
      libraryLock: (_) async => false,
    );
    addTearDown(persistence.db.close);

    expect(persistence.ownsLibrary, isFalse);
    await expectUntouched(persistence, practiceId, work);
    // Repair library reclaims nothing either.
    final report = await persistence.repairLibrary();
    expect(report.assetsSwept, 0);
    expect(await work.pooled.exists(), isTrue);
  });

  test('with a real second process holding vocara.lock, boot leaves its '
      'in-flight take, import and pool file alone', () async {
    final practiceId = await createPracticeAndClose(takeLock: false);
    final work = await plantInFlightWork(practiceId);
    final other = await _OtherProcess.lock(
      p.join(root.path, LibraryLock.fileName),
    );
    addTearDown(other.release);

    final persistence = await Persistence.open(dir: root, analyze: fakeAnalyze);
    addTearDown(persistence.db.close);

    expect(persistence.ownsLibrary, isFalse);
    await expectUntouched(persistence, practiceId, work);
  }, skip: _hasPython ? false : _noPython);

  test(
    'the lock is taken against other processes and released with them',
    () async {
      final other = await _OtherProcess.lock(
        p.join(root.path, LibraryLock.fileName),
      );
      expect(await LibraryLock.acquire(root), isFalse);
      await other.release();
      expect(await LibraryLock.acquire(root), isTrue);
      // ...and from then on it is this process's.
      final again = await _OtherProcess.tryLock(
        p.join(root.path, LibraryLock.fileName),
      );
      expect(again, isFalse, reason: 'another process took a held lock');
    },
    skip: _hasPython ? false : _noPython,
  );

  test('one process opening its own library twice owns it both times, and '
      'boot recovery still runs', () async {
    final practiceId = await createPracticeAndClose();
    final take = File(p.join(root.path, 'audio', practiceId, 'take.wav'));
    await take.parent.create(recursive: true);
    await take.writeAsBytes(monoWav(4800), flush: true);

    final first = await Persistence.open(dir: root, analyze: fakeAnalyze);
    addTearDown(first.db.close);
    expect(first.ownsLibrary, isTrue);
    final practice =
        (await first.songRepo.getPractice(practiceId) as Ok<SongPractice>)
            .value;
    expect(practice.vocalTracks.single.displayName, 'Take 1 (recovered)');

    await first.db.close();
    final second = await Persistence.open(dir: root, analyze: fakeAnalyze);
    addTearDown(second.db.close);
    expect(second.ownsLibrary, isTrue);
  });

  test('a library that needs an upgrade is not migrated under another '
      'process', () async {
    await createPracticeAndClose();
    final dbPath = p.join(root.path, 'vocara.db');
    final raw = sqlite3.open(dbPath);
    raw.execute('DROP TABLE sync_state');
    raw.execute('DROP TABLE sync_quarantine');
    raw.execute('DROP TABLE asset_sync');
    raw.execute('PRAGMA user_version = 1');
    raw.close();

    await expectLater(
      Persistence.open(
        dir: root,
        analyze: fakeAnalyze,
        libraryLock: (_) async => false,
      ),
      throwsA(isA<LibraryInUseException>()),
    );
    final check = sqlite3.open(dbPath, mode: OpenMode.readOnly);
    expect(check.select('PRAGMA user_version').first.values.first, 1);
    check.close();
    expect(
      await File(p.join(root.path, 'vocara.pre-migrate.bak')).exists(),
      isFalse,
      reason: 'no swap ran',
    );
  });
}
