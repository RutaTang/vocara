// The single-instance guard: every running Vocara holds `<support>/vocara.lock`
// SHARED for its whole life, and only an instance that can upgrade it to
// EXCLUSIVE (no other process has the library open) runs boot recovery, the
// boot sweeps, or Repair library's reclaim. In a library another process has
// open, a staged WAV with no row is that process's take still recording, and
// a pool file with no row is its import still landing.
//
// POSIX record locks never conflict within one process, so the "other
// process" is a python3 helper; those tests skip without python3. Every
// helper call is bounded, so no test can hang on a lock.

import 'dart:async';
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
/// genuinely separate process.
final bool _hasPython = () {
  try {
    return Process.runSync('python3', ['-c', 'import fcntl']).exitCode == 0;
  } catch (_) {
    return false;
  }
}();

const _noPython = 'needs python3 to hold the lock from another process';

/// The ceiling on every wait for a helper process.
const _helperTimeout = Duration(seconds: 20);

/// Another Vocara process, as far as the lock can tell: a python3 helper
/// holding an fcntl lock on the lock file's library byte (byte 0) until
/// [release]d. It also exits
/// on its own after a minute, so a crashed test can never leave it behind.
class _OtherProcess {
  _OtherProcess._(this._process);

  final Process _process;
  var _released = false;

  static const _holdScript =
      'import fcntl, os, signal, sys, time\n'
      'signal.alarm(60)\n'
      'fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT)\n'
      'modes = {"sh": fcntl.LOCK_SH, "ex": fcntl.LOCK_EX, "gate": fcntl.LOCK_EX}\n'
      'byte = 1 if sys.argv[2] == "gate" else 0\n'
      'try:\n'
      '    fcntl.lockf(fd, modes[sys.argv[2]] | fcntl.LOCK_NB, 1, byte)\n'
      'except OSError as e:\n'
      '    print("refused", e.errno, flush=True)\n'
      '    sys.exit(1)\n'
      'print("locked", flush=True)\n'
      'if len(sys.argv) > 3:\n'
      '    time.sleep(int(sys.argv[3]) / 1000)\n'
      '    fcntl.lockf(fd, fcntl.LOCK_SH, 1, 0)\n'
      'sys.stdin.read()\n';

  /// Holds the lock on [path] shared (a running instance) or exclusive (an
  /// owner mid-boot). With [downgradeAfter], an exclusive hold turns shared
  /// after that long, like an owner finishing its boot.
  static Future<_OtherProcess> hold(
    String path, {
    required bool shared,
    Duration? downgradeAfter,
    bool gate = false,
  }) async {
    final process = await Process.start('python3', [
      '-c',
      _holdScript,
      path,
      gate ? 'gate' : (shared ? 'sh' : 'ex'),
      if (downgradeAfter != null) '${downgradeAfter.inMilliseconds}',
    ]);
    unawaited(process.stderr.drain<void>());
    // Stays subscribed after the first line: cancelling stdout would close
    // the pipe, and the helper dies of it the next time it writes.
    final first = await process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .asBroadcastStream()
        .first
        .timeout(_helperTimeout, onTimeout: () => 'timed out');
    final other = _OtherProcess._(process);
    if (first != 'locked') {
      await other.release();
      fail('the helper process could not take the lock: $first');
    }
    return other;
  }

  /// Ends the helper and waits (bounded) for it to exit. Idempotent.
  Future<void> release() async {
    if (_released) return;
    _released = true;
    try {
      await _process.stdin.close();
    } catch (_) {}
    await _process.exitCode.timeout(
      _helperTimeout,
      onTimeout: () {
        _process.kill(ProcessSignal.sigkill);
        return -1;
      },
    );
  }

  /// Whether a freshly launched process could take the lock on [path] in
  /// that mode right now (a non-blocking attempt; the helper exits at once).
  static Future<bool> couldTake(String path, {required bool shared}) async {
    final process = await Process.start('python3', [
      '-c',
      'import fcntl, os, sys\n'
          'fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT)\n'
          'mode = fcntl.LOCK_SH if sys.argv[2] == "sh" else fcntl.LOCK_EX\n'
          'try:\n'
          '    fcntl.lockf(fd, mode | fcntl.LOCK_NB, 1, 0)\n'
          '    print("got")\n'
          'except OSError:\n'
          '    print("held")\n',
      path,
      shared ? 'sh' : 'ex',
    ]);
    unawaited(process.stderr.drain<void>());
    final out = await process.stdout
        .transform(utf8.decoder)
        .join()
        .timeout(_helperTimeout, onTimeout: () => 'timed out');
    await process.exitCode.timeout(
      _helperTimeout,
      onTimeout: () {
        process.kill(ProcessSignal.sigkill);
        return -1;
      },
    );
    return switch (out.trim()) {
      'got' => true,
      'held' => false,
      final other => fail('the lock probe did not answer: $other'),
    };
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

  String lockPath() => p.join(root.path, LibraryLock.fileName);

  Future<WaveformResult> fakeAnalyze(String path) async =>
      (peaks: <double>[0.1, 0.2], durationMs: 500);

  /// Mono 16-bit PCM: the shape the recording engine streams takes in.
  Uint8List monoWav(int samples) => writeWavMono(
    Float64List.fromList([for (var i = 0; i < samples; i++) 0.25]),
    48000,
  );

  Future<Persistence> boot({LibraryGuard? guard}) => Persistence.open(
    dir: root,
    analyze: fakeAnalyze,
    libraryLock: guard == null ? null : (_) async => guard,
  );

  /// Creates the library with one practice and one pooled asset (so the
  /// orphan-pool sweep has live hashes to judge against), then closes it,
  /// without touching the real lock file (so this process holds nothing on
  /// it until a test opens the library for real).
  Future<String> createPracticeAndClose() async {
    final persistence = await boot(guard: LibraryGuard.fixed(exclusive: true));
    final res = await persistence.songRepo.createPractice('Song');
    final src = File(p.join(root.path, 'backing.wav'));
    await src.writeAsBytes([9, 8, 7], flush: true);
    await persistence.assets.importFile(src.path);
    await persistence.db.close();
    return (res as Ok<SongPractice>).value.id;
  }

  /// What another process's in-flight work looks like on disk: a take still
  /// streaming into staging, an import staged but not saved, a file already
  /// placed in the pool whose asset row has not committed yet, a copy still
  /// being written into the pool's own staging, and a waveform analysis's
  /// intermediate (a take's stop-and-save analyzes it).
  Future<({File take, File import, File pooled, File part, File wave})>
  plantInFlightWork(String practiceId) async {
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
    final part = File(p.join(root.path, 'assets', '.staging', 'copy.part'));
    await part.parent.create(recursive: true);
    await part.writeAsBytes([7, 8], flush: true);
    final wave = File(
      p.join(root.path, WaveformService.scratchDirName, 'analysis.wave'),
    );
    await wave.parent.create(recursive: true);
    await wave.writeAsBytes([1], flush: true);
    return (take: take, import: import, pooled: pooled, part: part, wave: wave);
  }

  Future<void> expectUntouched(
    Persistence persistence,
    String practiceId,
    ({File take, File import, File pooled, File part, File wave}) work,
  ) async {
    expect(await work.take.exists(), isTrue, reason: 'take still recording');
    expect(await work.import.exists(), isTrue, reason: 'import not saved yet');
    expect(await work.pooled.exists(), isTrue, reason: 'import still landing');
    expect(await work.part.exists(), isTrue, reason: 'copy still running');
    expect(await work.wave.exists(), isTrue, reason: 'analysis in flight');
    final practice =
        (await persistence.songRepo.getPractice(practiceId) as Ok<SongPractice>)
            .value;
    expect(
      practice.vocalTracks,
      isEmpty,
      reason: 'the live take was adopted as "(recovered)"',
    );
  }

  test('a boot that cannot have the library to itself recovers and sweeps '
      'nothing, and its Repair library reclaims nothing', () async {
    final practiceId = await createPracticeAndClose();
    final work = await plantInFlightWork(practiceId);

    final persistence = await boot(guard: LibraryGuard.fixed(exclusive: false));
    addTearDown(persistence.db.close);

    expect(persistence.ownsLibrary, isFalse);
    await expectUntouched(persistence, practiceId, work);
    final report = await persistence.repairLibrary();
    expect(report.assetsSwept, 0);
    await expectUntouched(persistence, practiceId, work);
  });

  test('with another running instance holding the lock shared, boot leaves its '
      'in-flight take, imports and pool files alone', () async {
    final practiceId = await createPracticeAndClose();
    final work = await plantInFlightWork(practiceId);
    final other = await _OtherProcess.hold(lockPath(), shared: true);
    addTearDown(other.release);

    final persistence = await boot();
    addTearDown(persistence.db.close);

    expect(persistence.ownsLibrary, isFalse);
    await expectUntouched(persistence, practiceId, work);
  }, skip: _hasPython ? false : _noPython);

  test('an instance that started second keeps guarding the library after the '
      'first one quits: a new launch cannot own it while it runs', () async {
    await createPracticeAndClose();
    final first = await _OtherProcess.hold(lockPath(), shared: true);
    addTearDown(first.release);

    final second = await boot();
    addTearDown(second.db.close);
    expect(second.ownsLibrary, isFalse);

    await first.release();
    expect(
      await _OtherProcess.couldTake(lockPath(), shared: false),
      isFalse,
      reason:
          'this instance is still running, but a relaunch could take the '
          'library exclusively and run its boot recovery and sweeps under '
          'it',
    );
    // A relaunch still opens alongside it, as a non-owner.
    expect(await _OtherProcess.couldTake(lockPath(), shared: true), isTrue);
  }, skip: _hasPython ? false : _noPython);

  test(
    'an owner hands the lock back to shared once its boot is done',
    () async {
      final persistence = await boot();
      addTearDown(persistence.db.close);
      expect(persistence.ownsLibrary, isTrue);

      // Another launch can now open alongside it...
      expect(await _OtherProcess.couldTake(lockPath(), shared: true), isTrue);
      // ...but never own the library while this instance runs.
      expect(await _OtherProcess.couldTake(lockPath(), shared: false), isFalse);
    },
    skip: _hasPython ? false : _noPython,
  );

  test(
    'Repair library in the boot owner reclaims nothing once another instance '
    'has opened the library, and reclaims again once it is alone',
    () async {
      final practiceId = await createPracticeAndClose();
      final owner = await boot();
      addTearDown(owner.db.close);
      expect(owner.ownsLibrary, isTrue);

      // A second instance opens the library and starts an import.
      final other = await _OtherProcess.hold(lockPath(), shared: true);
      addTearDown(other.release);
      final work = await plantInFlightWork(practiceId);

      final held = await owner.repairLibrary();
      expect(held.assetsSwept, 0);
      expect(await work.pooled.exists(), isTrue, reason: 'import landing');
      expect(await work.part.exists(), isTrue, reason: 'copy still running');

      await other.release();
      final alone = await owner.repairLibrary();
      expect(alone.assetsSwept, greaterThan(0));
      expect(await work.pooled.exists(), isFalse, reason: 'a real orphan now');
    },
    skip: _hasPython ? false : _noPython,
  );

  test("a launch during an owner's boot waits for it, then opens shared as a "
      'non-owner', () async {
    final owner = await _OtherProcess.hold(
      lockPath(),
      shared: false,
      downgradeAfter: const Duration(milliseconds: 400),
    );
    addTearDown(owner.release);

    final sw = Stopwatch()..start();
    final lock = await LibraryLock.forLibrary(
      root,
      wait: const Duration(seconds: 15),
      poll: const Duration(milliseconds: 20),
    );
    expect(lock.holdsLock, isTrue, reason: 'shared, once the boot was done');
    expect(sw.elapsed, lessThan(const Duration(seconds: 15)));
    expect(await lock.tryExclusive(), isFalse, reason: 'the owner runs');
    expect(lock.holdsLock, isTrue, reason: 'a refused upgrade keeps shared');
  }, skip: _hasPython ? false : _noPython);

  test("a launch keeps waiting out an owner's long boot, tells the app once, "
      'and opens holding the lock', () async {
    // It used to give up after a deadline and run with no hold at all: the
    // owner's boot sweeps, cleared before it started, then trashed the take
    // it was recording.
    final owner = await _OtherProcess.hold(lockPath(), shared: false);
    addTearDown(owner.release);

    var told = 0;
    LibraryLock? opened;
    final launch = LibraryLock.forLibrary(
      root,
      wait: const Duration(milliseconds: 200),
      poll: const Duration(milliseconds: 20),
      onStillWaiting: () => told++,
    ).then((l) => opened = l);
    await Future<void>.delayed(const Duration(milliseconds: 800));
    expect(opened, isNull, reason: 'went on without a hold');
    expect(told, 1, reason: 'the app is told once, to show its window');

    await owner.release();
    final lock = await launch.timeout(_helperTimeout);
    expect(lock.holdsLock, isTrue);
    expect(told, 1);
    // The owner went from exclusive straight to nothing (its boot died): the
    // waiter claimed the library outright, not shared-then-upgrade, which
    // two waiters taking shared at that moment refused each other.
    expect(
      await _OtherProcess.couldTake(lockPath(), shared: true),
      isFalse,
      reason: 'the waiter took shared, leaving the library to be raced for',
    );
    expect(await lock.tryExclusive(), isTrue, reason: 'alone now');
    await lock.releaseExclusive();
    expect(await _OtherProcess.couldTake(lockPath(), shared: false), isFalse);
  }, skip: _hasPython ? false : _noPython);

  test(
    'a launch decides only inside the launch gate',
    () async {
      // Its shared hold and its upgrade happen while it holds the gate, so no
      // other launch can take shared in between: two launches racing for a
      // library nobody held refused each other's upgrade, leaving it unowned.
      final other = await _OtherProcess.hold(
        lockPath(),
        shared: false,
        gate: true,
      );
      addTearDown(other.release);
      LibraryLock? opened;
      final launch = LibraryLock.forLibrary(
        root,
        poll: const Duration(milliseconds: 20),
      ).then((l) => opened = l);
      await Future<void>.delayed(const Duration(milliseconds: 400));
      expect(
        opened,
        isNull,
        reason: 'decided while another launch held the gate',
      );
      await other.release();
      final lock = await launch.timeout(_helperTimeout);
      expect(await lock.tryExclusive(), isTrue, reason: 'alone: the owner');
      await lock.releaseExclusive();
    },
    skip: _hasPython ? false : _noPython,
  );

  test('a launch on an idle library takes it exclusive from nothing, so of '
      'launches racing there exactly one owns it', () async {
    // Shared first, then an upgrade, let two launches at the same moment
    // refuse each other: neither owned the library, and a pending upgrade
    // was refused by both ("another Vocara is using it").
    final lock = await LibraryLock.forLibrary(root);
    expect(
      await _OtherProcess.couldTake(lockPath(), shared: true),
      isFalse,
      reason: 'a second launch must not get in before this boot is done',
    );
    expect(await lock.tryExclusive(), isTrue, reason: "the boot's claim");
    await lock.releaseExclusive();
    expect(
      await _OtherProcess.couldTake(lockPath(), shared: true),
      isTrue,
      reason: 'handed back to shared once the boot is done',
    );
    expect(await _OtherProcess.couldTake(lockPath(), shared: false), isFalse);
  }, skip: _hasPython ? false : _noPython);

  test('one process opening its own library twice owns it both times, and '
      'boot recovery still runs', () async {
    final practiceId = await createPracticeAndClose();
    final take = File(p.join(root.path, 'audio', practiceId, 'take.wav'));
    await take.parent.create(recursive: true);
    await take.writeAsBytes(monoWav(4800), flush: true);

    final first = await boot();
    addTearDown(first.db.close);
    expect(first.ownsLibrary, isTrue);
    final practice =
        (await first.songRepo.getPractice(practiceId) as Ok<SongPractice>)
            .value;
    expect(practice.vocalTracks.single.displayName, 'Take 1 (recovered)');

    await first.db.close();
    final second = await boot();
    addTearDown(second.db.close);
    expect(second.ownsLibrary, isTrue);
    // Both stores live in this process, which still has the library to
    // itself: its Repair library may reclaim.
    final guard = await LibraryLock.forLibrary(root);
    expect(await guard.tryExclusive(), isTrue);
    await guard.releaseExclusive();
  });

  test('a second instance saving while the first one writes waits its turn '
      'instead of failing the save', () async {
    // Two running instances share the library (each holds the lock shared).
    // Each writes through its own SQLite connection, and with no busy timeout
    // a write that met the other's open transaction failed at once with
    // "database is locked": the save reported failed and its edit reverted.
    await createPracticeAndClose();
    final first = await boot(guard: LibraryGuard.fixed(exclusive: true));
    addTearDown(first.db.close);
    final second = await boot(guard: LibraryGuard.fixed(exclusive: false));
    addTearDown(second.db.close);

    final writing = Completer<void>();
    final held = first.db.transaction(() async {
      await first.db.customStatement(
        "INSERT INTO sync_state (\"key\", value) VALUES ('probe', 'x')",
      );
      writing.complete();
      await Future<void>.delayed(const Duration(milliseconds: 300));
    });
    await writing.future;
    final saved = await second.songRepo.createPractice('Second');
    await held;
    expect(saved, isA<Ok<SongPractice>>(), reason: '$saved');
  });

  test('a non-owner refuses a library whose owner died mid-migration', () async {
    // Its live file may be missing (moved to the backup, the migrated copy not
    // yet in): opened as it stood, that became a new EMPTY library, and the
    // next owner's recovery took it for the migrated one and dropped the
    // backup — the data left only in an orphaned copy.
    final practiceId = await createPracticeAndClose();
    final dbPath = p.join(root.path, 'vocara.db');
    final bak = p.join(root.path, 'vocara.pre-migrate.bak');
    await File(dbPath).copy(p.join(root.path, 'vocara.migrating.db'));
    await File(dbPath).rename(bak);
    await File(
      p.join(root.path, 'migration.json'),
    ).writeAsString('{"state":"swapping","to":99}');

    await expectLater(
      boot(guard: LibraryGuard.fixed(exclusive: false)),
      throwsA(isA<LibraryInUseException>()),
    );
    expect(await File(dbPath).exists(), isFalse, reason: 'nothing created');

    // The owner's recovery promotes the migrated copy: the data is there.
    final owner = await boot(guard: LibraryGuard.fixed(exclusive: true));
    addTearDown(owner.db.close);
    expect(
      await owner.songRepo.getPractice(practiceId),
      isA<Ok<SongPractice>>(),
    );
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
      boot(guard: LibraryGuard.fixed(exclusive: false)),
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
