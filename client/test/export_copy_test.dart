// An export copies a take (or the diagnostics bundle) onto the path the save
// panel picked. A copy failing part-way left a truncated file there.

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

  test('a good copy lands whole', () async {
    final src = File(p.join(tmp.path, 'src.wav'))..writeAsStringSync('audio');
    final dest = p.join(tmp.path, 'out.wav');
    await exportCopy(src, dest);
    expect(File(dest).readAsStringSync(), 'audio');
  });
}
