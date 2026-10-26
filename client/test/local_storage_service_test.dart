import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/services/local_storage_service.dart';

void main() {
  late Directory tmp;
  late LocalStorageService storage;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_storage');
    storage = LocalStorageService(root: tmp);
  });
  tearDown(() => tmp.delete(recursive: true));

  Future<void> writeRaw(List<int> bytes) async {
    final dir = Directory(p.join(tmp.path, 'settings'));
    await dir.create(recursive: true);
    await File(p.join(dir.path, 'app.json')).writeAsBytes(bytes);
  }

  test('a document reads back as written', () async {
    await storage.write('settings', 'app', {'a': 1});
    expect(await storage.read('settings', 'app'), {'a': 1});
  });

  test('no file, or one that is not a JSON object, is no document', () async {
    expect(await storage.read('settings', 'app'), isNull);
    await writeRaw('[1, 2]'.codeUnits);
    expect(await storage.read('settings', 'app'), isNull);
    await writeRaw('{"a": '.codeUnits);
    expect(await storage.read('settings', 'app'), isNull);
  });

  test('bytes that are not UTF-8 are a corrupt document, not an unreadable '
      'one', () async {
    // dart:io reports a failed UTF-8 decode as a FileSystemException. Taken
    // as unreadable, the settings it held were never written again: every
    // change re-read the file, failed, and kept it.
    await writeRaw([
      ...'{"outputDeviceName":"Ren'.codeUnits,
      0xE9, // é in Latin-1, as an editor might save it
      ...'"}'.codeUnits,
    ]);
    expect(await storage.read('settings', 'app'), isNull);
  });
}
