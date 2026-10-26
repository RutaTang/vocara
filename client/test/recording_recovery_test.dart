// Boot-time recovery of hard-kill orphaned recordings.
//
// A force quit / crash / kill -9 mid-record leaves the streamed take WAV in the
// audio staging tree (audio/<practiceId>/<id>.wav) with no rows referencing it;
// the engine keeps that file's header valid by patching it in place ~1x/s.
// Persistence.recoverStagedRecordings must re-attach such a file to its
// practice as a new vocal take (analysis, clip, save — the normal finalize
// shape), be idempotent across re-runs (a crash during recovery must not
// duplicate the take), and leave everything else to the staging sweep.

import 'dart:io';
import 'dart:typed_data';

import 'package:drift/drift.dart' show Variable;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Whether the suite runs as root, which reads a file whatever its permission
/// bits: a `chmod 000` makes nothing unreadable then (a container's default).
final bool _runsAsRoot =
    !Platform.isWindows &&
    (Process.runSync('id', ['-u']).stdout as String).trim() == '0';

/// A real file whose `open()` fails after a successful stat — what a
/// permission bit does to anyone but root.
class _OpenFails implements File {
  _OpenFails(this._real);
  final File _real;

  @override
  Future<int> length() => _real.length();

  @override
  Future<RandomAccessFile> open({FileMode mode = FileMode.read}) =>
      throw const FileSystemException('simulated permission denied', 'open');

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// A real file whose `length()` (stat) fails — the hardware-fault shape no
/// permission bit can produce, and the one `probeWav` used to answer "null,
/// so: not a recording" for.
class _StatFails implements File {
  _StatFails(this._real);
  final File _real;

  @override
  Future<int> length() =>
      throw const FileSystemException('simulated I/O error', 'length');

  @override
  Future<RandomAccessFile> open({FileMode mode = FileMode.read}) =>
      _real.open(mode: mode);

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// A real file whose header READ fails after a successful stat and open — the
/// other step that used to swallow its error.
class _ReadFails implements File {
  _ReadFails(this._real);
  final File _real;

  @override
  Future<int> length() => _real.length();

  @override
  Future<RandomAccessFile> open({FileMode mode = FileMode.read}) async =>
      _FailingRead(await _real.open(mode: mode));

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _FailingRead implements RandomAccessFile {
  _FailingRead(this._real);
  final RandomAccessFile _real;

  @override
  Future<Uint8List> read(int count) =>
      throw const FileSystemException('simulated I/O error', 'read');

  @override
  Future<void> close() => _real.close();

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// Storage rooted at the persistence root, the way production shares one
/// application-support directory between the store and the file services.
class _RootedStorage extends LocalStorageService {
  _RootedStorage(this.root);
  final Directory root;

  @override
  Future<Directory> assetDir(String collection) async {
    final dir = Directory(p.join(root.path, collection));
    await dir.create(recursive: true);
    return dir;
  }

  @override
  Future<String> assetPath(String collection) async =>
      p.join(root.path, collection);
}

void main() {
  late Persistence persistence;
  late Directory root;

  setUp(() {
    persistence = Persistence.memory();
    root = persistence.assets.root;
  });

  tearDown(() async {
    await persistence.db.close();
    if (await root.exists()) await root.delete(recursive: true);
  });

  /// The analyzer a real recovery would run through WaveformService; tests
  /// stand in a fixed result (the platform decoder isn't available here).
  Future<WaveformResult> fakeAnalyze(String path) async =>
      (peaks: <double>[0.1, 0.2], durationMs: 500);

  /// Mono 16-bit PCM WAV bytes with [samples] ramp samples — the exact shape
  /// the recording engine writes.
  Uint8List monoWav(int samples, {double gain = 0.5}) => writeWavMono(
    Float64List.fromList([
      for (var i = 0; i < samples; i++) gain * (i % 100) / 100,
    ]),
    48000,
  );

  /// Simulates the hard-kill file shape: real PCM bytes exist PAST what the
  /// last in-place header patch covers (the header sizes are left as-is).
  Uint8List withUnpatchedTail(Uint8List wav, int extraBytes) =>
      Uint8List.fromList([...wav, ...List.filled(extraBytes, 7)]);

  /// Writes an orphaned recording into the staging tree exactly where
  /// AudioFileService.newRecordingPath would have placed it.
  Future<File> stageOrphan(String practiceId, List<int> bytes) async {
    final f = File(p.join(root.path, 'audio', practiceId, '${newId()}.wav'));
    await f.parent.create(recursive: true);
    await f.writeAsBytes(bytes, flush: true);
    return f;
  }

  Future<SongPractice> createPractice() async {
    final res = await persistence.songRepo.createPractice('Song');
    return (res as Ok<SongPractice>).value;
  }

  Future<SongPractice> reload(String id) async {
    final res = await persistence.songRepo.getPractice(id);
    return (res as Ok<SongPractice>).value;
  }

  test('orphaned recording is recovered as a new vocal take', () async {
    final practice = await createPractice();
    final orphan = await stageOrphan(
      practice.id,
      withUnpatchedTail(monoWav(4800), 120),
    );

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);

    final after = await reload(practice.id);
    expect(after.vocalTracks, hasLength(1));
    final take = after.vocalTracks.single;
    expect(take.kind, AudioTrackKind.vocal);
    expect(take.displayName, 'Take 1 (recovered)');
    expect(take.versionId, after.activeVersionId);
    final clip = take.primaryClip!;
    expect(clip.durationMs, 500); // from the (injected) waveform analysis
    expect(clip.startMs, 0); // the killed session's playhead is unknowable
    expect(clip.waveformPeaks, hasLength(2));
    expect(clip.waveformPeaks[0], closeTo(0.1, 1e-6));
    expect(clip.waveformPeaks[1], closeTo(0.2, 1e-6));
    // The audio was imported into the content-addressed pool; the clip
    // resolves to a live pool file, not the staging path.
    expect(p.isWithin(p.join(root.path, 'assets'), clip.audioFilePath), isTrue);
    expect(await File(clip.audioFilePath).exists(), isTrue);
    // The staging copy was deleted after the rows committed.
    expect(await orphan.exists(), isFalse);
  });

  test('recovery leaves every row but the new take as it was stored', () async {
    // Recovery saved without a base, so the store's own rows stood in as one:
    // a row this build re-encodes differently than it was stored (a field
    // another build wrote) read as a local edit, and was rewritten with the
    // field stripped, and journaled, though nothing touched it.
    final practice = await createPractice();
    await persistence.db.customStatement(
      "UPDATE practices SET reverb_json = json_set(COALESCE(reverb_json, '{}'), '\$.future', 1), "
      'lyrics_json = ? WHERE id = ?',
      ['[{"text":"la","timeMs":10,"style":"bold"}]', practice.id],
    );
    Future<List<Object?>> row() async {
      final r = await persistence.db
          .customSelect(
            'SELECT reverb_json, lyrics_json FROM practices WHERE id = ?',
            variables: [Variable.withString(practice.id)],
          )
          .getSingle();
      return [r.data['reverb_json'], r.data['lyrics_json']];
    }

    Future<int> journaled() async =>
        (await persistence.db
                .customSelect(
                  "SELECT COUNT(*) AS n FROM oplog WHERE entity_table = 'practices' "
                  'AND entity_id = ?',
                  variables: [Variable.withString(practice.id)],
                )
                .getSingle())
            .read<int>('n');
    final before = await row();
    expect(before[0], contains('"future":1'));
    final journalBefore = await journaled();

    await stageOrphan(practice.id, monoWav(4800));
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);

    expect((await reload(practice.id)).vocalTracks, hasLength(1));
    expect(await row(), before, reason: 'the practice row is untouched');
    expect(await journaled(), journalBefore, reason: 'and not journaled');
  });

  test('a recovered take past the stored timeline still leaves the practice '
      'row as stored', () async {
    // Growing the stored timeline to hold the take made the practice row a
    // local edit again: rewritten (another build's fields stripped) and
    // journaled at boot. The editor grows the timeline in memory on open.
    final created = await createPractice();
    final stored = await persistence.songRepo.savePractice(
      created.copyWith(timelineDurationMs: 20000),
    );
    final practice = (stored as Ok<SongPractice>).value;
    await persistence.db.customStatement(
      "UPDATE practices SET reverb_json = json_set(COALESCE(reverb_json, '{}'), "
      "'\$.future', 1) WHERE id = ?",
      [practice.id],
    );
    Future<Object?> reverb() async =>
        (await persistence.db
                .customSelect(
                  'SELECT reverb_json FROM practices WHERE id = ?',
                  variables: [Variable.withString(practice.id)],
                )
                .getSingle())
            .data['reverb_json'];
    final before = await reverb();
    expect(before, contains('"future":1'));
    await stageOrphan(practice.id, monoWav(4800));
    Future<WaveformResult> long(String path) async =>
        (peaks: <double>[0.1, 0.2], durationMs: 600000);
    expect(await persistence.recoverStagedRecordings(analyze: long), 1);
    final after = await reload(practice.id);
    expect(after.contentEndMs, 600000);
    expect(after.timelineDurationMs, 20000);
    expect(await reverb(), before);
  });

  test('second run is a no-op', () async {
    final practice = await createPractice();
    await stageOrphan(practice.id, monoWav(4800));
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));
  });

  test('two byte-identical staging files recover as ONE take in the same '
      'boot', () async {
    // The referenced-hash set is read once, before the scan. Without adding
    // each recovery's own hash to it, the second file still looked like bytes
    // nothing had ever saved and became a second "(recovered)" take of the
    // same audio — only the NEXT boot's re-read of the set closed it. Two
    // silent takes of equal length (a muted or unplugged input) produce
    // exactly these bytes.
    final practice = await createPractice();
    final bytes = monoWav(4800);
    final first = await stageOrphan(practice.id, bytes);
    final second = await stageOrphan(practice.id, bytes);

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));

    // The recovered one was deleted; the duplicate is left to the sweep.
    expect(await first.exists() && await second.exists(), isFalse);
    await persistence.assets.sweepStaging();
    expect(await first.exists(), isFalse);
    expect(await second.exists(), isFalse);
  });

  test('a boot with many staged files reaches the same outcome for each '
      'with their hashes computed ahead of the walk', () async {
    // The hashes are prefetched in background isolates while the walk runs
    // file by file; every per-file decision must still be the walk's own.
    final a = await createPractice();
    final b = await createPractice();
    final twin = monoWav(4800, gain: 0.3);
    final twins = [
      await stageOrphan(a.id, twin),
      await stageOrphan(a.id, twin),
    ];
    final distinct = [
      await stageOrphan(b.id, monoWav(4800, gain: 0.6)),
      await stageOrphan(b.id, monoWav(5200, gain: 0.6)),
    ];
    final notATake = await stageOrphan(a.id, [1, 2, 3, 4, 5, 6, 7, 8]);
    final orphanedPractice = await stageOrphan(newId(), monoWav(4800));

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 3);
    expect((await reload(a.id)).vocalTracks, hasLength(1)); // twins: ONE take
    expect((await reload(b.id)).vocalTracks, hasLength(2));
    // Recovered files are deleted; the rest is the sweep's.
    expect([
      for (final f in twins) await f.exists(),
    ], unorderedEquals([true, false]));
    for (final f in distinct) {
      expect(await f.exists(), isFalse);
    }
    expect(await notATake.exists(), isTrue);
    expect(await orphanedPractice.exists(), isTrue);
    expect(persistence.recoveryRetryPaths, isEmpty);
    // A second boot finds nothing new to adopt.
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
  });

  test('a crash between the row commit and the file delete cannot duplicate '
      'the take on the next boot', () async {
    final practice = await createPractice();
    final bytes = monoWav(4800);
    final orphan = await stageOrphan(practice.id, bytes);
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);

    // Simulate the crash window: the take's rows are committed but the
    // staging file survived (recreate the identical bytes at the same path).
    await orphan.writeAsBytes(bytes, flush: true);
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));
    // The skipped leftover is then deleted by the sweep, as before.
    await persistence.assets.sweepStaging();
    expect(await orphan.exists(), isFalse);
  });

  test('a crash between the pool import and the row commit still recovers on '
      'the retry, reusing the half-imported asset', () async {
    final practice = await createPractice();
    final orphan = await stageOrphan(practice.id, monoWav(4800));
    // Simulate the crash window: the asset row + pooled bytes exist but no
    // clip row ever committed (savePractice imports first, then writes rows).
    await persistence.assets.importFile(orphan.path);

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);
    final after = await reload(practice.id);
    expect(after.vocalTracks, hasLength(1));
    // Deduped: the retry reused the existing asset instead of minting a
    // second one for the same bytes.
    final count = await persistence.db
        .customSelect(
          'SELECT COUNT(*) AS n FROM assets WHERE deleted_at IS NULL',
        )
        .getSingle();
    expect(count.data['n'], 1);
    expect(await orphan.exists(), isFalse);
  });

  /// Runs the graceful-stop + same-session-delete sequence: saves a take from
  /// its staging WAV (the save pools the bytes and deliberately leaves the
  /// staging copy for the boot sweep), then deletes the take (tombstoning the
  /// clip row while the staging copy survives — deleteAudio refuses pool
  /// paths). Returns the surviving staging file.
  Future<File> saveThenDeleteTake(SongPractice practice) async {
    final staged = await stageOrphan(practice.id, monoWav(4800));
    final saved = await persistence.songRepo.savePractice(
      practice.copyWith(
        vocalTracks: [
          AudioTrack(
            id: newId(),
            kind: AudioTrackKind.vocal,
            displayName: 'Take 1',
            audioFilePath: staged.path,
            durationMs: 100,
            createdAt: DateTime.now(),
            versionId: practice.activeVersionId,
          ),
        ],
      ),
    );
    final withTake = (saved as Ok<SongPractice>).value;
    expect(await staged.exists(), isTrue);
    final deleted = await persistence.songRepo.savePractice(
      withTake.copyWith(vocalTracks: []),
    );
    expect(deleted, isA<Ok<SongPractice>>());
    return staged;
  }

  test('a saved-then-deleted take is never resurrected: its staging copy '
      'matches a tombstoned reference', () async {
    final practice = await createPractice();
    final staged = await saveThenDeleteTake(practice);

    // Next boot: recovery must NOT adopt the leftover as a "(recovered)"
    // take — the bytes match a reference that was deliberately deleted.
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, isEmpty);
    // The leftover is the sweep's to reclaim, exactly as before.
    await persistence.assets.sweepStaging();
    expect(await staged.exists(), isFalse);
  });

  test('a deleted take stays deleted even after the GC tombstoned its asset '
      'and reclaimed its pool file', () async {
    final practice = await createPractice();
    await saveThenDeleteTake(practice);
    // Settle the asset's journal entry as a push ack would (the ack
    // hard-deletes): the sweep holds back any asset whose entry is still
    // pending, since a pushed-but-unacked row may already be server-side.
    await persistence.db.customStatement(
      "DELETE FROM oplog WHERE entity_table = 'assets'",
    );
    // The GC pass a later boot would run: the take's asset is unreferenced
    // by live rows, so it is tombstoned and its pooled bytes reclaimed.
    expect(
      await persistence.assets.sweepUnreferenced(minAge: Duration.zero),
      1,
    );

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, isEmpty);
  });

  test(
    'files under audio/<pid>/edits/ are invisible to recovery but swept',
    () async {
      // Split/concat/punch-in outputs stage under audio/<pid>/edits/ exactly
      // so a kill between the file write and the save commit cannot become a
      // phantom "(recovered)" take — recovery only adopts files DIRECTLY
      // under audio/<pid>/, while the staging sweep reclaims the whole tree.
      final practice = await createPractice();
      final f = File(
        p.join(root.path, 'audio', practice.id, 'edits', '${newId()}.wav'),
      );
      await f.parent.create(recursive: true);
      await f.writeAsBytes(monoWav(4800), flush: true);

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect((await reload(practice.id)).vocalTracks, isEmpty);
      expect(await f.exists(), isTrue); // untouched by recovery...
      await persistence.assets.sweepStaging();
      expect(await f.exists(), isFalse); // ...reclaimed by the boot sweep
    },
  );

  test('a thrown recovery attempt is exempted from this boot\'s sweep and '
      'retried next boot', () async {
    final practice = await createPractice();
    final failing = await stageOrphan(practice.id, monoWav(4800));
    final junk = await stageOrphan(
      practice.id,
      List<int>.generate(64, (i) => i),
    );

    // A systemic failure (broken decoder, disk error) — NOT per-file junk.
    Future<WaveformResult> brokenAnalyze(String path) async =>
        throw StateError('decoder down fleet-wide');
    expect(
      await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
      0,
    );
    expect(persistence.recoveryRetryPaths, {failing.path});

    // The boot sweep spares exactly the failed attempt; ordinary junk goes.
    await persistence.assets.sweepStaging(keep: persistence.recoveryRetryPaths);
    expect(await failing.exists(), isTrue);
    expect(await junk.exists(), isFalse);

    // Next boot, healthy decoder: the exempted take is recovered normally.
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));
  });

  test(
    'a recording whose clip an edit replaced is not recovered again',
    () async {
      // A graceful stop pools the take and leaves its staging WAV for the boot
      // sweep, which decides "already saved" by asking whether any row — live
      // or tombstoned — still references those bytes. Split and merge replace
      // the edited clip with a NEW row, so the old one is tombstoned with its
      // asset intact and the original stays referenced. (Re-pointing the same
      // clip id at the edited file overwrote that reference, and the original
      // came back as a whole "(recovered)" take.)
      final practice = await createPractice();
      final rec = await stageOrphan(practice.id, monoWav(4800));
      final take = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        audioFilePath: rec.path,
        durationMs: 100,
        createdAt: DateTime.now(),
        versionId: practice.activeVersion?.id,
      );
      await persistence.songRepo.savePractice(
        practice.copyWith(vocalTracks: [take]),
      );

      final edited = File(p.join(root.path, 'edits', '${newId()}.wav'));
      await edited.parent.create(recursive: true);
      await edited.writeAsBytes(monoWav(2400, gain: 0.3), flush: true);
      final loaded = await reload(practice.id);
      final lane = loaded.vocalTracks.single;
      await persistence.songRepo.savePractice(
        loaded.copyWith(
          vocalTracks: [
            lane.copyWith(
              clips: [
                AudioClip(
                  id: newId(),
                  audioFilePath: edited.path,
                  durationMs: 50,
                ),
              ],
            ),
          ],
        ),
      );

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect((await reload(practice.id)).vocalTracks, hasLength(1));
    },
  );

  test(
    'probeWav THROWS on an unreadable file instead of answering null',
    () async {
      // The contract the recovery classification rests on: null means "provably
      // not the recorder's shape", and only the shape checks may produce it. If
      // an I/O failure could answer null, a take that merely could not be read
      // would be classified as junk and swept. Reachable here through the
      // permission case; a hardware fault on the stat or the header read takes
      // the same route by construction, now that nothing in probeWav swallows
      // an IOException.
      final f = File(p.join(root.path, 'unreadable.wav'));
      await f.parent.create(recursive: true);
      await f.writeAsBytes(monoWav(4800), flush: true);
      if (!_runsAsRoot) {
        await Process.run('chmod', ['000', f.path]);
        addTearDown(() => Process.run('chmod', ['644', f.path]));
        await expectLater(
          AudioFileService.probeWav(f),
          throwsA(isA<FileSystemException>()),
        );
      }

      // The permission case above fails at open() — injected too, for a run as
      // root. The stat and the header read are the two steps that used to
      // swallow their errors, and no permission bit reaches them — only a real
      // fault does (EIO, a bad sector, a volume pulled between the directory
      // listing and the probe). Inject each.
      final good = File(p.join(root.path, 'good.wav'));
      await good.writeAsBytes(monoWav(4800), flush: true);
      await expectLater(
        AudioFileService.probeWav(_OpenFails(good)),
        throwsA(isA<FileSystemException>()),
        reason: 'a file that cannot be opened is not shown to be junk',
      );
      await expectLater(
        AudioFileService.probeWav(_StatFails(good)),
        throwsA(isA<FileSystemException>()),
        reason: 'a failing stat is not evidence that this is not a recording',
      );
      await expectLater(
        AudioFileService.probeWav(_ReadFails(good)),
        throwsA(isA<FileSystemException>()),
        reason: 'nor is a failing header read',
      );

      // A file that is merely GONE is still a plain null: nothing to recover,
      // and nothing to hold back for.
      expect(
        await AudioFileService.probeWav(File(p.join(root.path, 'gone.wav'))),
        isNull,
      );
    },
  );

  test(
    'an UNREADABLE take is held back for retry, not classified as junk',
    () async {
      // probeWav answers null for SHAPE only. When it could not read the file at
      // all, answering "not a recording" would sweep the only copy of a crashed
      // take into the trash on the strength of a read that never happened — and
      // the trash is never rescanned, so the 30-day prune finishes the job.
      final practice = await createPractice();
      final unreadable = await stageOrphan(practice.id, monoWav(4800));
      await Process.run('chmod', ['000', unreadable.path]);
      addTearDown(() => Process.run('chmod', ['644', unreadable.path]));

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect(
        persistence.recoveryRetryPaths,
        {unreadable.path},
        reason: 'an I/O failure is an environmental failure, not junk',
      );

      await persistence.assets.sweepStaging(
        keep: persistence.recoveryRetryPaths,
      );
      expect(await unreadable.exists(), isTrue, reason: 'spared by the sweep');

      // Readable again next boot: recovered normally.
      await Process.run('chmod', ['644', unreadable.path]);
      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        1,
      );
      expect((await reload(practice.id)).vocalTracks, hasLength(1));
    },
    // Only a permission bit makes a real file in the staging tree unreadable
    // here, and root reads it anyway.
    skip: _runsAsRoot
        ? 'root reads a file whatever its permission bits'
        : false,
  );

  test(
    'a recovery whose SAVE failed is exempted too, not just a thrown one',
    () async {
      final practice = await createPractice();
      final failing = await stageOrphan(practice.id, monoWav(4800));

      // The store reports failure as a Result, never a throw: block the pool
      // tree (a regular file where the `assets/` directory must be) so the
      // save's asset import cannot land. Without this case being treated as an
      // environmental failure, the only copy of the take goes to the sweep and
      // no later boot ever retries it.
      final blocker = File(p.join(root.path, 'assets'));
      await blocker.parent.create(recursive: true);
      await blocker.writeAsString('not a directory', flush: true);

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect(persistence.recoveryRetryPaths, {failing.path});

      await persistence.assets.sweepStaging(
        keep: persistence.recoveryRetryPaths,
      );
      expect(await failing.exists(), isTrue);

      // Next boot, healthy disk: the exempted take is recovered normally.
      await blocker.delete();
      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        1,
      );
      expect((await reload(practice.id)).vocalTracks, hasLength(1));
    },
  );

  test('a staged file that has kept failing for over 7 days is released to '
      'the sweep after all', () async {
    final practice = await createPractice();
    final stale = await stageOrphan(practice.id, monoWav(4800));
    final stamp = File('${stale.path}.retry');
    final firstFailure = DateTime.now().subtract(const Duration(days: 8));
    // Two boots failed it already; this is the third.
    await stamp.writeAsString('${firstFailure.millisecondsSinceEpoch} 2');

    Future<WaveformResult> brokenAnalyze(String path) async =>
        throw StateError('decoder down');
    expect(
      await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
      0,
    );
    // Too old to keep protecting: the exemption lapses and the sweep takes it
    // (the trash still holds it for its own retention window).
    expect(persistence.recoveryRetryPaths, isEmpty);
    await persistence.assets.sweepStaging(keep: persistence.recoveryRetryPaths);
    expect(await stale.exists(), isFalse);
    expect(await stamp.exists(), isFalse);
  });

  test('a crash left untouched for over 7 days still gets its retries: the '
      'window opens at its first failed attempt', () async {
    // The app was not opened for a week after the crash; the first boot's
    // attempt then failed (disk full, decoder down). Measured from the
    // file's modification time, the take went to the sweep on that very
    // first attempt.
    final practice = await createPractice();
    final old = await stageOrphan(practice.id, monoWav(4800));
    await old.setLastModified(DateTime.now().subtract(const Duration(days: 8)));

    Future<WaveformResult> brokenAnalyze(String path) async =>
        throw StateError('decoder down');
    expect(
      await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
      0,
    );
    expect(persistence.recoveryRetryPaths, {old.path});
    await persistence.assets.sweepStaging(keep: persistence.recoveryRetryPaths);
    expect(await old.exists(), isTrue);
    // The first failure's time stays with it for the next boot's verdict.
    final stamp = File('${old.path}.retry');
    expect(await stamp.exists(), isTrue);
    final firstMs = int.parse((await stamp.readAsString()).split(' ').first);
    final first = DateTime.fromMillisecondsSinceEpoch(firstMs);
    expect(
      DateTime.now().difference(first),
      lessThan(const Duration(hours: 1)),
    );

    // A later failure measures from that first one, not from now.
    expect(
      await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
      0,
    );
    expect(await stamp.readAsString(), '$firstMs 2');
    expect(persistence.recoveryRetryPaths, {old.path});

    // Healthy again: recovered normally.
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 1);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));
  });

  test('a file that failed once and then waited out the week still gets '
      'more attempts', () async {
    // The window alone, measured from the first failure, released the take
    // on its second attempt when the app then went unopened for a week.
    final practice = await createPractice();
    final staged = await stageOrphan(practice.id, monoWav(4800));
    final stamp = File('${staged.path}.retry');
    final firstFailure = DateTime.now().subtract(const Duration(days: 8));
    await stamp.writeAsString('${firstFailure.millisecondsSinceEpoch} 1');

    Future<WaveformResult> brokenAnalyze(String path) async =>
        throw StateError('decoder down');
    expect(
      await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
      0,
    );
    expect(persistence.recoveryRetryPaths, {staged.path});
    expect(
      await stamp.readAsString(),
      '${firstFailure.millisecondsSinceEpoch} 2',
    );
  });

  test(
    'a recorded hash does not speak for a file that grew since its import',
    () async {
      // A power cut rolls the staging file back, never forward: a longer file
      // was written to after the import, and those bytes were never saved.
      final practice = await createPractice();
      final staged = await stageOrphan(practice.id, monoWav(2400));
      final saved = await persistence.songRepo.savePractice(
        practice.copyWith(
          vocalTracks: [
            AudioTrack(
              id: newId(),
              kind: AudioTrackKind.vocal,
              displayName: 'Take 1',
              audioFilePath: staged.path,
              durationMs: 50,
              createdAt: DateTime.now(),
              versionId: practice.activeVersionId,
            ),
          ],
        ),
      );
      expect(saved, isA<Ok<SongPractice>>());
      await staged.writeAsBytes(monoWav(4800), flush: true);

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        1,
      );
      expect((await reload(practice.id)).vocalTracks, hasLength(2));
    },
  );

  test('a take whose staging copy lost its last writes to a power cut after '
      'its save is not recovered a second time', () async {
    // Graceful stop: the save pools the take and leaves the staging copy for
    // the boot sweep, recording the hash it pooled beside it.
    final practice = await createPractice();
    final staged = await stageOrphan(practice.id, monoWav(4800));
    final saved = await persistence.songRepo.savePractice(
      practice.copyWith(
        vocalTracks: [
          AudioTrack(
            id: newId(),
            kind: AudioTrackKind.vocal,
            displayName: 'Take 1',
            audioFilePath: staged.path,
            durationMs: 100,
            createdAt: DateTime.now(),
            versionId: practice.activeVersionId,
          ),
        ],
      ),
    );
    expect(saved, isA<Ok<SongPractice>>());
    expect(await File('${staged.path}.sha256').exists(), isTrue);

    // The power cut: the recorder never flushed the staging file, so it comes
    // back holding only what reached the disk — older, shorter bytes whose
    // own hash matches nothing saved.
    await staged.writeAsBytes(monoWav(2400), flush: true);

    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, hasLength(1));
    await persistence.assets.sweepStaging();
    expect(await staged.exists(), isFalse);
  });

  test(
    'a held-back recording keeps its recorded hash through the sweep',
    () async {
      // The hash a save recorded is what tells a power-cut leftover from an
      // orphan (see above); a boot whose attempt failed must not sweep it
      // away from the file it describes.
      final practice = await createPractice();
      final staged = await stageOrphan(practice.id, monoWav(4800));
      await persistence.assets.importFile(staged.path);
      final sidecar = File('${staged.path}.sha256');
      expect(await sidecar.exists(), isTrue);

      Future<WaveformResult> brokenAnalyze(String path) async =>
          throw StateError('decoder down');
      expect(
        await persistence.recoverStagedRecordings(analyze: brokenAnalyze),
        0,
      );
      await persistence.assets.sweepStaging(
        keep: persistence.recoveryRetryPaths,
      );
      expect(await staged.exists(), isTrue);
      expect(await sidecar.exists(), isTrue);
    },
  );

  test(
    'numbering continues after existing takes in the interpretation',
    () async {
      var practice = await createPractice();
      // An already-saved ordinary take named by the normal scheme.
      final existing = File(p.join(root.path, 'existing.wav'));
      await existing.writeAsBytes(monoWav(1200, gain: 0.3), flush: true);
      final saved = await persistence.songRepo.savePractice(
        practice.copyWith(
          vocalTracks: [
            AudioTrack(
              id: newId(),
              kind: AudioTrackKind.vocal,
              displayName: 'Take 3',
              audioFilePath: existing.path,
              durationMs: 100,
              createdAt: DateTime.now(),
              versionId: practice.activeVersionId,
            ),
          ],
        ),
      );
      practice = (saved as Ok<SongPractice>).value;

      await stageOrphan(practice.id, monoWav(4800));
      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        1,
      );
      final after = await reload(practice.id);
      expect(
        after.vocalTracks.map((t) => t.displayName),
        containsAll(['Take 3', 'Take 4 (recovered)']),
      );
    },
  );

  test(
    'an orphan of a deleted or unknown practice is left for the sweep',
    () async {
      final practice = await createPractice();
      await persistence.songRepo.deletePractice(practice.id);
      final tombstoned = await stageOrphan(practice.id, monoWav(4800));
      final unknown = await stageOrphan('no-such-practice', monoWav(4800));

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect(await tombstoned.exists(), isTrue); // untouched, then swept
      expect(await unknown.exists(), isTrue);
      await persistence.assets.sweepStaging();
      expect(await tombstoned.exists(), isFalse);
      expect(await unknown.exists(), isFalse);
    },
  );

  test(
    'empty, unparseable and non-recording files are never recovered',
    () async {
      final practice = await createPractice();
      // Killed before the first header patch: valid WAV, zero data bytes.
      final empty = await stageOrphan(practice.id, monoWav(0));
      // Not a WAV at all.
      final garbage = await stageOrphan(
        practice.id,
        List<int>.generate(128, (i) => i),
      );
      // A stereo file (an import, not something the recorder writes): flip the
      // channel-count field of an otherwise valid header.
      final stereoBytes = monoWav(4800);
      stereoBytes.buffer.asByteData().setUint16(22, 2, Endian.little);
      final stereo = await stageOrphan(practice.id, stereoBytes);

      expect(
        await persistence.recoverStagedRecordings(analyze: fakeAnalyze),
        0,
      );
      expect((await reload(practice.id)).vocalTracks, isEmpty);
      for (final f in [empty, garbage, stereo]) {
        expect(await f.exists(), isTrue); // untouched, then swept
      }
      await persistence.assets.sweepStaging();
      for (final f in [empty, garbage, stereo]) {
        expect(await f.exists(), isFalse);
      }
    },
  );

  test('an import cut off before its save is not adopted as a take, and '
      'still pools and sweeps', () async {
    // A mono 48 kHz WAV with the canonical header is exactly the recorder's
    // shape, and ensureStreamableWav passes it through unchanged, so the staged
    // copy IS the track's file until the save pools it. A quit or crash in that
    // window (the quit does not wait for an import) left it where recovery
    // adopts orphans: the next boot showed it as "Take N (recovered)".
    final practice = await createPractice();
    final ext = await Directory.systemTemp.createTemp('vocara_import_src');
    addTearDown(() => ext.delete(recursive: true));
    final src = File(p.join(ext.path, 'guide vocal.wav'));
    await src.writeAsBytes(monoWav(48000));
    final files = AudioFileService(storage: _RootedStorage(root));

    final staged = await files.importAudio(
      practiceId: practice.id,
      sourcePath: src.path,
    );
    expect(await File(staged).exists(), isTrue);
    expect(
      p.isWithin(p.join(root.path, 'audio', practice.id), staged),
      isTrue,
      reason: 'imports still stage inside the practice tree',
    );

    // The app quits here, before the save. Next boot:
    expect(await persistence.recoverStagedRecordings(analyze: fakeAnalyze), 0);
    expect((await reload(practice.id)).vocalTracks, isEmpty);
    expect(persistence.recoveryRetryPaths, isEmpty);

    // Had the save run instead, it pools the staged file as usual.
    final pooled = await files.importAudio(
      practiceId: practice.id,
      sourcePath: src.path,
    );
    final saved = await persistence.songRepo.savePractice(
      practice.copyWith(
        vocalTracks: [
          AudioTrack(
            id: newId(),
            kind: AudioTrackKind.vocal,
            displayName: 'Guide',
            audioFilePath: pooled,
            durationMs: 1000,
            waveformPeaks: const [0.1],
            createdAt: DateTime.now(),
            versionId: practice.activeVersion?.id,
          ),
        ],
      ),
    );
    expect(saved, isA<Ok<SongPractice>>());
    final clip = (await reload(practice.id)).vocalTracks.single.primaryClip!;
    expect(p.isWithin(p.join(root.path, 'assets'), clip.audioFilePath), isTrue);

    // And the boot sweep still reclaims every staged import.
    await persistence.assets.sweepStaging();
    expect(await File(staged).exists(), isFalse);
    expect(await File(pooled).exists(), isFalse);
  });
}
