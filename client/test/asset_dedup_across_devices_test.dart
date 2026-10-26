import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

RemoteRecord _row(String table, Map<String, Object?> cols, Hlc h) =>
    RemoteRecord(
      table: table,
      columns: {
        ...cols,
        'created_at': h.wallMs,
        'updated_at': h.wallMs,
        'deleted_at': null,
        'hlc_wall': h.wallMs,
        'hlc_ctr': h.counter,
        'hlc_node': h.node,
        'hlc_packed': h.packed,
        'schema_v': 1,
      },
    );

// Identical bytes under two asset ids is the normal cross-device state. An
// import used to dedup to the first row by HLC whether or not its bytes were
// here, and re-pointed a synced-in row (another device's earlier import, no
// bytes here) at the pool file a local row already owned: two live rows on one
// path, and a clip loaded through that path came back on a save as the other
// asset — an edit nobody made, which won over a peer's real one.
void main() {
  test('an import dedups to the row whose bytes are here, and a save that '
      'never touched a clip leaves a peer\'s edit of it alone', () async {
    final ps = Persistence.memory();
    final db = ps.db;
    final applier = SyncApplier(
      db,
      ps.writer.clock,
      builder: SyncRowBuilder(db),
      quarantine: QuarantineStore(db),
    );
    final dir = await Directory.systemTemp.createTemp('sweep47');
    final src = File(p.join(dir.path, 'song.wav'))
      ..writeAsBytesSync(List.generate(4096, (i) => i % 251));

    // 1. Local practice with one take whose clip C1 -> local asset A1.
    final created =
        (await ps.songRepo.createPractice('P') as Ok<SongPractice>).value;
    final take = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: src.path,
      durationMs: 1000,
      createdAt: DateTime.now(),
      versionId: created.activeVersion?.id,
    );
    await ps.songRepo.savePractice(created.copyWith(vocalTracks: [take]));
    final a1 = (await db.select(db.clips).getSingle()).assetId;
    final a1Row = await (db.select(
      db.assets,
    )..where((a) => a.id.equals(a1))).getSingle();

    // 2. Sync delivers another device's earlier import of the SAME bytes (A2,
    //    lower HLC): the cross-device steady state, per-device path stripped.
    final a2 = newId();
    final r = await applier.apply([
      _row('assets', {
        'id': a2,
        'content_hash': a1Row.contentHash,
        'byte_size': a1Row.byteSize,
        'ext': 'wav',
        'kind': 'audio',
      }, Hlc(wallMs: 1000, counter: 0, node: 'Y' * 26)),
    ]);
    expect(r.applied, 1);

    // 3. The user imports the same file again (e.g. into another practice):
    //    it dedups to A1, whose bytes are here, and A2 keeps no path.
    final again = File(p.join(dir.path, 'again.wav'))
      ..writeAsBytesSync(src.readAsBytesSync());
    expect(await ps.assets.importFile(again.path), a1);
    final a2Row = await (db.select(
      db.assets,
    )..where((a) => a.id.equals(a2))).getSingle();
    expect(a2Row.localRelPath, isNull);

    // 4. The editor loads P; meanwhile a peer moves C1 (remote edit, newer HLC).
    final loaded =
        (await ps.songRepo.loadPractice(created.id) as Ok<Loaded<SongPractice>>)
            .value;
    final clipRow = await db.select(db.clips).getSingle();
    final moved = Hlc(
      wallMs: ps.writer.clock.last.wallMs + 10,
      counter: 0,
      node: 'Y' * 26,
    );
    final r2 = await applier.apply([
      _row('clips', {
        'id': clipRow.id,
        'track_id': clipRow.trackId,
        'asset_id': a1,
        'start_ms': 5000,
        'duration_ms': clipRow.durationMs,
        'order_index': clipRow.orderIndex,
      }, moved),
    ]);
    expect(r2.applied, 1);
    final opsBefore = (await db.select(db.oplog).get())
        .where((o) => o.entityTable == 'clips')
        .length;

    // 5. The editor saves an edit that never touches C1 (a title rename).
    final res = await ps.songRepo.savePracticeWithBase(
      loaded.aggregate.copyWith(title: 'Renamed'),
      baseHashes: loaded.baseHashes,
    );
    expect(res, isA<Ok<Loaded<SongPractice>>>());

    final after = await db.select(db.clips).getSingle();
    final clipOps = (await db.select(db.oplog).get())
        .where((o) => o.entityTable == 'clips')
        .length;
    expect(
      after.startMs,
      5000,
      reason: 'the peer moved C1 and this save never touched it',
    );
    expect(after.assetId, a1);
    expect(clipOps - opsBefore, 0);
    await db.close();
    await dir.delete(recursive: true);
  });
}
