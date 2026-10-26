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

  test('a good copy lands whole', () async {
    final src = File(p.join(tmp.path, 'src.wav'))..writeAsStringSync('audio');
    final dest = p.join(tmp.path, 'out.wav');
    await exportCopy(src, dest);
    expect(File(dest).readAsStringSync(), 'audio');
  });
}
