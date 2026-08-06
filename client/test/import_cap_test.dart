import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';

/// Storage rooted at a temp dir (no path_provider in VM tests).
class _TempStorage extends LocalStorageService {
  _TempStorage(this.root);

  final Directory root;

  @override
  Future<Directory> assetDir(String collection) async {
    final dir = Directory(p.join(root.path, collection));
    if (!await dir.exists()) await dir.create(recursive: true);
    return dir;
  }

  @override
  Future<String> assetPath(String collection) async =>
      p.join(root.path, collection);
}

void main() {
  late Directory tmp;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_import_cap');
  });
  tearDown(() async {
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  /// An APFS-sparse file whose LENGTH exceeds the import cap without the
  /// disk cost: seek past the cap, write one byte.
  Future<String> oversized() async {
    final f = File(p.join(tmp.path, 'huge.wav'));
    final raf = await f.open(mode: FileMode.write);
    await raf.setPosition(AudioFileService.maxImportBytes + 1);
    await raf.writeByte(0);
    await raf.close();
    return f.path;
  }

  test('importAudio refuses an oversized source from a stat, before any '
      'bytes are copied', () async {
    final files = AudioFileService(storage: _TempStorage(tmp));
    final src = await oversized();
    await expectLater(
      files.importAudio(
        practiceId: 'P0000000000000000000000000',
        sourcePath: src,
      ),
      throwsFormatException,
    );
    // Nothing copied: the practice's audio dir was never even created.
    expect(
      Directory(p.join(tmp.path, 'audio')).existsSync(),
      isFalse,
      reason: 'the refusal must precede every filesystem side effect',
    );
  });

  test('importMedia refuses an oversized source the same way', () async {
    final media = TechniqueMediaService(storage: _TempStorage(tmp));
    final src = await oversized();
    await expectLater(
      media.importMedia(
        techniqueId: 'T1000000000000000000000000',
        sourcePath: src,
      ),
      throwsFormatException,
    );
    expect(
      Directory(p.join(tmp.path, 'technique_media')).existsSync(),
      isFalse,
    );
  });

  test('a normal-sized source still imports', () async {
    final files = AudioFileService(storage: _TempStorage(tmp));
    final src = File(p.join(tmp.path, 'small.wav'));
    await src.writeAsBytes(const [1, 2, 3, 4]);
    final dest = await files.importAudio(
      practiceId: 'P0000000000000000000000000',
      sourcePath: src.path,
    );
    expect(await File(dest).length(), 4);
  });
}
