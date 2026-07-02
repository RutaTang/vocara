import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/durable_fs.dart';

/// DurableFs runs the macOS F_FULLFSYNC via Isolate.run so a multi-second
/// drive-cache flush can't beachball the UI isolate. These tests exercise the
/// real native + spawned-isolate path (on macOS): if the Isolate.run closure
/// captured non-sendable state, or the per-isolate FFI re-lookup broke, every
/// call here would throw instead of completing.
void main() {
  const fs = DurableFs();

  test(
    'fsyncFile flushes an existing file without altering its bytes',
    () async {
      final dir = await Directory.systemTemp.createTemp('vocara_durable');
      addTearDown(() => dir.delete(recursive: true));

      final bytes = Uint8List.fromList([
        for (var i = 0; i < 4096; i++) i & 0xFF,
      ]);
      final f = File('${dir.path}/take.bin');
      await f.writeAsBytes(bytes);

      await fs.fsyncFile(f.path);

      expect(await f.readAsBytes(), bytes);
    },
  );

  test(
    'fsyncDir completes on a real directory (rename durability path)',
    () async {
      final dir = await Directory.systemTemp.createTemp('vocara_durable_dir');
      addTearDown(() => dir.delete(recursive: true));

      await File('${dir.path}/entry.bin').writeAsBytes([1, 2, 3]);
      await fs.fsyncDir(dir.path);

      expect(File('${dir.path}/entry.bin').existsSync(), isTrue);
    },
  );

  test(
    'concurrent fsyncs are safe (per-isolate FFI init under parallel spawns)',
    () async {
      final dir = await Directory.systemTemp.createTemp('vocara_durable_par');
      addTearDown(() => dir.delete(recursive: true));

      final paths = <String>[];
      for (var i = 0; i < 8; i++) {
        final path = '${dir.path}/f$i.bin';
        await File(path).writeAsBytes(List.filled(1024, i));
        paths.add(path);
      }

      await Future.wait([
        for (final p in paths) fs.fsyncFile(p),
        fs.fsyncDir(dir.path),
      ]);

      for (var i = 0; i < paths.length; i++) {
        expect((await File(paths[i]).readAsBytes()).first, i);
      }
    },
  );
}
