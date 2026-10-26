// An export copies a take (or the diagnostics bundle) onto the path the save
// panel picked. A copy failing part-way left a truncated file there.

import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/utils/export_copy.dart';

/// A source whose copy writes [written] bytes of the destination, then fails
/// (a stick filling up); null writes nothing (it failed opening it).
class _FailingSource extends Fake implements File {
  _FailingSource(this.written);

  final String? written;

  @override
  Future<File> copy(String newPath) async {
    final w = written;
    if (w != null) await File(newPath).writeAsString(w);
    throw const FileSystemException('No space left on device');
  }
}

/// [_FailingSource], with the destination's time put back to [stamp] after
/// the write: a rewrite inside one tick of the clock.
class _StampKeepingSource extends _FailingSource {
  _StampKeepingSource(super.written, this.stamp);

  final DateTime stamp;

  @override
  Future<File> copy(String newPath) async {
    try {
      return await super.copy(newPath);
    } finally {
      File(newPath).setLastModifiedSync(stamp);
    }
  }
}

/// A source whose copy writes [content], then runs [probe] before finishing.
class _ProbingSource extends Fake implements File {
  _ProbingSource(this.content, this.probe);

  final String content;
  final Future<void> Function() probe;

  @override
  Future<File> copy(String newPath) async {
    await File(newPath).writeAsString(content);
    await probe();
    return File(newPath);
  }
}

void main() {
  late Directory tmp;
  setUp(() async => tmp = await Directory.systemTemp.createTemp('export_copy'));
  tearDown(() async => tmp.delete(recursive: true));

  test('a copy that fails part-way leaves no truncated file', () async {
    final dest = p.join(tmp.path, 'take.wav');
    await expectLater(
      exportCopy(_FailingSource('RIFF....'), dest),
      throwsA(isA<FileSystemException>()),
    );
    expect(File(dest).existsSync(), isFalse);
  });

  test(
    'a file picked to be replaced goes too once the copy rewrote it',
    () async {
      final dest = File(p.join(tmp.path, 'take.wav'))..writeAsStringSync('old');
      // Let the old file's time differ from the rewrite's.
      dest.setLastModifiedSync(
        DateTime.now().subtract(const Duration(hours: 1)),
      );
      await expectLater(
        exportCopy(_FailingSource('half'), dest.path),
        throwsA(isA<FileSystemException>()),
      );
      expect(dest.existsSync(), isFalse);
    },
  );

  test('a file the failing copy never touched stays', () async {
    final dest = File(p.join(tmp.path, 'take.wav'))..writeAsStringSync('old');
    await expectLater(
      exportCopy(_FailingSource(null), dest.path),
      throwsA(isA<FileSystemException>()),
    );
    expect(dest.readAsStringSync(), 'old');
  });

  test('a file rewritten within the clock tick still goes', () async {
    // Judged on its time alone, a file written and then rewritten by the
    // failing copy inside one tick read as untouched, and stayed half-written.
    final dest = File(p.join(tmp.path, 'take.wav'))..writeAsStringSync('old');
    final stamp = dest.lastModifiedSync();
    await expectLater(
      exportCopy(_StampKeepingSource('half', stamp), dest.path),
      throwsA(isA<FileSystemException>()),
    );
    expect(dest.existsSync(), isFalse);
  });

  test('through a link, the half-written file goes, not the link', () async {
    final target = File(p.join(tmp.path, 'real.wav'))..writeAsStringSync('o');
    target.setLastModifiedSync(
      DateTime.now().subtract(const Duration(hours: 1)),
    );
    final link = p.join(tmp.path, 'link.wav');
    Link(link).createSync(target.path);
    await expectLater(
      exportCopy(_FailingSource('half'), link),
      throwsA(isA<FileSystemException>()),
    );
    expect(target.existsSync(), isFalse);
    expect(Link(link).existsSync(), isTrue);
  });

  test('a link that names nothing yet: the file the copy made goes', () async {
    final target = p.join(tmp.path, 'made.wav');
    final link = p.join(tmp.path, 'link.wav');
    Link(link).createSync('made.wav'); // relative, dangling
    await expectLater(
      exportCopy(_FailingSource('half'), link),
      throwsA(isA<FileSystemException>()),
    );
    expect(File(target).existsSync(), isFalse);
  });

  test('a chain of links naming nothing yet: the made file goes', () async {
    final made = p.join(tmp.path, 'final.wav');
    final first = p.join(tmp.path, 'link1.wav');
    Link(p.join(tmp.path, 'link2.wav')).createSync('final.wav');
    Link(first).createSync('link2.wav');
    await expectLater(
      exportCopy(_FailingSource('half'), first),
      throwsA(isA<FileSystemException>()),
    );
    expect(File(made).existsSync(), isFalse);
    expect(Link(p.join(tmp.path, 'link2.wav')).existsSync(), isTrue);
  });

  // The quit reads this to tell an export still copying from a wedged one.
  test('an export in flight reports what it has written, and nothing once '
      'done', () async {
    final dest = p.join(tmp.path, 'take.wav');
    int? during;
    await exportCopy(
      _ProbingSource('audio', () async => during = await exportBytesWritten()),
      dest,
    );
    expect(during, 5);
    expect(await exportBytesWritten(), 0);
  });

  test('an export ending while the reading waits on a length leaves the '
      'reading whole', () async {
    final release = Completer<void>();
    final written = Completer<void>();
    final export = exportCopy(
      _ProbingSource('audio', () async {
        written.complete();
        await release.future;
      }),
      p.join(tmp.path, 'take.wav'),
    );
    await written.future;
    final reading = exportBytesWritten(); // waits on the file's length
    release.complete(); // the copy ends: its path leaves the set
    await export;
    expect(await reading, isA<int>());
  });

  // One export on a share that stopped answering held up the whole reading,
  // so the quit couldn't see a pool import still moving beside it.
  test(
    'a file that stops answering counts at what it last had, promptly',
    () async {
      final release = Completer<void>();
      final written = Completer<void>();
      final export = exportCopy(
        _ProbingSource('audio', () async {
          written.complete();
          await release.future;
        }),
        p.join(tmp.path, 'take.wav'),
      );
      await written.future;
      expect(await exportBytesWritten(), 5);

      final clock = Stopwatch()..start();
      final stuck = await exportBytesWritten(
        perFile: const Duration(milliseconds: 50),
        lengthOf: (_) => Completer<int>().future,
      );
      expect(stuck, 5);
      expect(clock.elapsed, lessThan(const Duration(seconds: 1)));

      release.complete();
      await export;
      expect(await exportBytesWritten(), 0);
    },
  );

  /// Starts an export to [name] that holds until [release] completes, once
  /// its first bytes are written; [done] completes with it.
  Future<({Future<void> done})> holdExport(
    String name,
    Completer<void> release,
  ) async {
    final written = Completer<void>();
    final export = exportCopy(
      _ProbingSource('audio', () async {
        written.complete();
        await release.future;
      }),
      p.join(tmp.path, name),
    );
    await written.future;
    return (done: export);
  }

  test('a share slow to answer every time still reads as moving', () async {
    // Every stat outlasts the per-file wait. Late answers were dropped, so
    // each reading counted the last on-time one: frozen while it copied, and
    // the quit cut the export off.
    final release = Completer<void>();
    final export = (await holdExport('take.wav', release)).done;
    var copied = 0;
    Future<int> slow(String _) async {
      final n = copied += 100;
      await Future<void>.delayed(const Duration(milliseconds: 60));
      return n;
    }

    const wait = Duration(milliseconds: 10);
    final first = await exportBytesWritten(perFile: wait, lengthOf: slow);
    await Future<void>.delayed(const Duration(milliseconds: 120));
    final second = await exportBytesWritten(perFile: wait, lengthOf: slow);
    expect(second, isNot(first));
    release.complete();
    await export;
  });

  test('an answer landing after its export ended is not counted for a later '
      'export to the same file', () async {
    final release = Completer<void>();
    final export = (await holdExport('take.wav', release)).done;
    final answer = Completer<int>();
    final reading = exportBytesWritten(
      perFile: const Duration(seconds: 5),
      lengthOf: (_) => answer.future,
    );
    release.complete();
    await export; // ended while its length was awaited
    answer.complete(11);
    await reading;

    final next = Completer<void>();
    final again = (await holdExport('take.wav', next)).done;
    expect(
      await exportBytesWritten(
        perFile: const Duration(milliseconds: 20),
        lengthOf: (_) => Completer<int>().future,
      ),
      0,
      reason: 'nothing on record for this export yet',
    );
    next.complete();
    await again;
  });

  test('several stuck files cost one wait, not one each', () async {
    final release = Completer<void>();
    final exports = [
      for (final name in ['a.wav', 'b.wav', 'c.wav', 'd.wav'])
        (await holdExport(name, release)).done,
    ];
    final clock = Stopwatch()..start();
    await exportBytesWritten(
      perFile: const Duration(milliseconds: 100),
      lengthOf: (_) => Completer<int>().future,
    );
    expect(clock.elapsed, lessThan(const Duration(milliseconds: 300)));
    release.complete();
    await Future.wait(exports);
  });

  test('a good copy lands whole', () async {
    final src = File(p.join(tmp.path, 'src.wav'))..writeAsStringSync('audio');
    final dest = p.join(tmp.path, 'out.wav');
    await exportCopy(src, dest);
    expect(File(dest).readAsStringSync(), 'audio');
  });
}
