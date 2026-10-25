import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/result.dart';

void main() {
  test('a save made while Repair library re-hashes the pool does not wait for '
      'the re-hash', () async {
    // The re-hash only reads, yet it ran inside the write gate's exclusive
    // side: every save — an edit, a technique, the save that arms REC — sat
    // behind a pass over the whole pool, minutes long for a large library.
    final persistence = Persistence.memory();
    addTearDown(() async {
      await persistence.db.close();
      final dir = persistence.assets.root;
      if (await dir.exists()) await dir.delete(recursive: true);
    });
    expect(
      await persistence.songRepo.createPractice('Song'),
      isA<Ok<SongPractice>>(),
    );
    // A pooled file a few long takes in size, for the re-hash to chew on.
    final hash = 'ab' * 32;
    final shard = Directory(
      p.join(persistence.assets.root.path, 'assets', 'ab', 'ab'),
    );
    await shard.create(recursive: true);
    final raf = await File(
      p.join(shard.path, '$hash.wav'),
    ).open(mode: FileMode.write);
    final chunk = Uint8List(1 << 20);
    for (var i = 0; i < 128; i++) {
      chunk[0] = i;
      await raf.writeFrom(chunk);
    }
    await raf.close();

    var repaired = false;
    final repair = persistence.repairLibrary().whenComplete(
      () => repaired = true,
    );
    // Past the integrity pass and the sweeps: the re-hash is under way.
    await Future<void>.delayed(const Duration(milliseconds: 200));
    final saved = await persistence.songRepo.createPractice('Edited meanwhile');
    final repairedBeforeTheSave = repaired;
    final report = await repair;

    expect(saved, isA<Ok<SongPractice>>());
    expect(
      repairedBeforeTheSave,
      isFalse,
      reason: 'the save waited for the whole re-hash',
    );
    expect(report.corruptPoolFiles, 1, reason: 'the re-hash still reports');
  });
}
