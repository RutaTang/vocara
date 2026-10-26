import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/ui/song_practice_detail/view_model/interpretations_controller.dart';
import 'package:vocara/utils/busy_gate.dart';

/// Transport fake: records the pause/play order so the switch flow
/// (pause → save → resume-only-if-playing) is observable. [onPause] lets a
/// test mutate the host's practice DURING the pause await, modelling a take
/// committed concurrently.
class _FakeEngine extends Fake implements PlaybackEngine {
  bool transportRunning = false;
  final List<String> log = [];
  void Function()? onPause;

  @override
  bool get isPlaying => transportRunning;

  @override
  Future<void> pause() async {
    log.add('pause');
    transportRunning = false;
    onPause?.call();
  }

  @override
  Future<void> play() async {
    log.add('play');
    transportRunning = true;
  }
}

class _FakeAnalysis extends Fake implements AnalysisCache {
  bool open = false;
  bool ensured = false;
  final List<String> droppedLanes = [];
  final List<String> evicted = [];

  @override
  bool get dockOpen => open;

  @override
  bool get dockShowsTake => true;

  @override
  Future<void> ensureSelectedSpectrogram() async => ensured = true;

  @override
  void dropLaneState(String trackId) => droppedLanes.add(trackId);

  @override
  void evictAnalysis(String path, {bool recompute = false}) =>
      evicted.add(path);
}

class _FakeFiles extends Fake implements AudioFileService {
  final List<String> deleted = [];

  @override
  Future<void> deleteAudio(String path) async => deleted.add(path);
}

/// A live host harness: the practice model and the closures the controller
/// edits it through, mirroring the view model's wiring.
class _Host {
  _Host(this.practice);

  SongPractice? practice;
  final engine = _FakeEngine();
  final analysis = _FakeAnalysis();
  final files = _FakeFiles();
  final toasts = <String>[];
  bool saveResult = true;
  int saves = 0;
  int ensureSelectionCalls = 0;

  /// The active interpretation at each engine resync, before its save.
  final resyncedOn = <String?>[];
  bool recording = false;
  bool separating = false;

  /// Runs inside the save closure, so a test can e.g. dispose the controller
  /// mid-save (route popped while a switch persists).
  void Function()? onSave;

  late final BusyGate busyGate = BusyGate(onError: (e) => toasts.add('$e'));

  late final InterpretationsController controller = InterpretationsController(
    engine: engine,
    analysis: analysis,
    busyGate: busyGate,
    audioFiles: files,
    practice: () => practice,
    setPractice: (p) => practice = p,
    save: () async {
      saves++;
      onSave?.call();
      return saveResult;
    },
    resyncPlayback: () async => resyncedOn.add(practice?.activeVersionId),
    ensureSelection: () => ensureSelectionCalls++,
    toast: toasts.add,
    isRecording: () => recording,
    isSeparating: () => separating,
  );
}

TakeVersion _version(String id, String name) =>
    TakeVersion(id: id, name: name, createdAt: DateTime(2026));

AudioTrack _take(String id, String versionId, {List<String> files = const []}) {
  return AudioTrack(
    id: id,
    kind: AudioTrackKind.vocal,
    displayName: 'Take $id',
    createdAt: DateTime(2026),
    versionId: versionId,
    clips: [
      for (final (i, f) in files.indexed)
        AudioClip(id: '$id-c$i', audioFilePath: f, durationMs: 1000),
    ],
  );
}

SongPractice _practice({
  List<TakeVersion> versions = const [],
  String? activeVersionId,
  List<AudioTrack> vocalTracks = const [],
}) {
  return SongPractice(
    id: 'p1',
    title: 'Song',
    createdAt: DateTime(2026),
    updatedAt: DateTime(2026),
    versions: versions,
    activeVersionId: activeVersionId,
    vocalTracks: vocalTracks,
  );
}

void main() {
  group('vocalTakes', () {
    test('returns only the active interpretation\'s takes', () {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v2',
          vocalTracks: [_take('t1', 'v1'), _take('t2', 'v2')],
        ),
      );
      expect(host.controller.vocalTakes.map((t) => t.id), ['t2']);
      expect(host.controller.active?.id, 'v2');
      expect(host.controller.all.map((v) => v.id), ['v1', 'v2']);
    });
  });

  group('setActive', () {
    test(
      'pauses, switches, saves, and resumes only if it was playing',
      () async {
        final host = _Host(
          _practice(
            versions: [_version('v1', 'One'), _version('v2', 'Two')],
            activeVersionId: 'v1',
          ),
        );
        host.engine.transportRunning = true;
        await host.controller.setActive('v2');

        expect(host.practice!.activeVersionId, 'v2');
        expect(host.engine.log, ['pause', 'play']);
        expect(host.saves, 1);
        expect(host.ensureSelectionCalls, 1);
        expect(host.toasts, isEmpty);
      },
    );

    test(
      'loads the new interpretation into the engine before its save',
      () async {
        // Shown at once, the new lanes sat over an engine still holding the
        // old takes until the save landed: Play meanwhile played those.
        final host = _Host(
          _practice(
            versions: [_version('v1', 'One'), _version('v2', 'Two')],
            activeVersionId: 'v1',
          ),
        );
        String? resyncedBeforeSave;
        host.onSave = () => resyncedBeforeSave = host.resyncedOn.lastOrNull;
        await host.controller.setActive('v2');
        expect(resyncedBeforeSave, 'v2');
      },
    );

    test('does not resume when the transport was paused', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      await host.controller.setActive('v2');
      expect(host.engine.log, ['pause']);
    });

    test('refreshes the open dock\'s analysis after the switch', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      host.analysis.open = true;
      await host.controller.setActive('v2');
      expect(host.analysis.ensured, isTrue);
    });

    test('is guarded while recording (toast, no change)', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      host.recording = true;
      await host.controller.setActive('v2');

      expect(host.practice!.activeVersionId, 'v1');
      expect(host.toasts, ['Stop recording to switch interpretation.']);
      expect(host.saves, 0);
      expect(host.engine.log, isEmpty);
    });

    test('ignores an unknown id and the already-active id', () async {
      final host = _Host(
        _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
      );
      await host.controller.setActive('nope');
      await host.controller.setActive('v1');
      expect(host.saves, 0);
      expect(host.engine.log, isEmpty);
    });

    test('picking the interpretation shown when the stored id resolves to '
        'none is no switch', () async {
      // Sync can leave the stored active id null or pointing at a deleted
      // interpretation; the first one is then what is shown.
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'gone',
        ),
      );
      await host.controller.setActive('v1');
      expect(host.saves, 0);
      expect(host.engine.log, isEmpty);
    });

    test(
      'a take committed during the pause await survives the switch '
      '(rebuilds from the LIVE practice, not the pre-await snapshot)',
      () async {
        final host = _Host(
          _practice(
            versions: [_version('v1', 'One'), _version('v2', 'Two')],
            activeVersionId: 'v1',
          ),
        );
        // A recording finalize lands a new take while pause() is in flight.
        host.engine.onPause = () {
          final cur = host.practice!;
          host.practice = cur.copyWith(
            vocalTracks: [...cur.vocalTracks, _take('t-new', 'v1')],
          );
        };
        await host.controller.setActive('v2');

        expect(host.practice!.activeVersionId, 'v2');
        expect(
          host.practice!.vocalTracks.map((t) => t.id),
          contains('t-new'),
          reason: 'the concurrently-committed take must not be clobbered',
        );
      },
    );

    test('an early return after the pause (target became active meanwhile) '
        'resumes playback instead of stranding the transport paused', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      host.engine.transportRunning = true;
      // The target becomes active DURING the pause await (a racing switch or
      // a remote apply): the post-pause re-read bails out — but playback was
      // already paused and must be handed back.
      host.engine.onPause = () {
        host.practice = host.practice!.copyWith(activeVersionId: 'v2');
      };
      await host.controller.setActive('v2');

      expect(host.engine.log, ['pause', 'play']);
      expect(host.saves, 0, reason: 'nothing changed; no save');
    });

    test('does not resume playback when disposed during the save', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      host.engine.transportRunning = true;
      host.onSave = host.controller.dispose; // route popped mid-save
      await host.controller.setActive('v2');

      expect(host.engine.log, ['pause']); // no play() on the disposed wiring
    });
  });

  group('create', () {
    test('appends a default-named interpretation and switches to it', () async {
      final host = _Host(
        _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
      );
      await host.controller.create('   ');

      final versions = host.practice!.versions;
      expect(versions, hasLength(2));
      expect(versions.last.name, 'Interpretation 2');
      expect(host.practice!.activeVersionId, versions.last.id);
      expect(host.saves, 1);
      expect(host.engine.log, ['pause']);
    });

    test('uses the trimmed custom name', () async {
      final host = _Host(
        _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
      );
      await host.controller.create('  Soft ballad  ');
      expect(host.practice!.versions.last.name, 'Soft ballad');
    });

    test('is guarded while recording', () async {
      final host = _Host(
        _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
      );
      host.recording = true;
      await host.controller.create('X');
      expect(host.practice!.versions, hasLength(1));
      expect(host.toasts, ['Stop recording to add an interpretation.']);
    });

    test(
      'a take committed during the pause await survives the create',
      () async {
        final host = _Host(
          _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
        );
        host.engine.onPause = () {
          final cur = host.practice!;
          host.practice = cur.copyWith(
            vocalTracks: [...cur.vocalTracks, _take('t-new', 'v1')],
          );
        };
        await host.controller.create('Soft ballad');

        expect(host.practice!.versions, hasLength(2));
        expect(host.practice!.vocalTracks.map((t) => t.id), contains('t-new'));
      },
    );
  });

  group('rename', () {
    test('renames exactly the target (trimmed) and saves', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One'), _version('v2', 'Two')],
          activeVersionId: 'v1',
        ),
      );
      await host.controller.rename('v2', '  Belted  ');
      expect(host.practice!.versions.map((v) => v.name), ['One', 'Belted']);
      expect(host.saves, 1);
    });

    test('a blank name is a no-op', () async {
      final host = _Host(
        _practice(versions: [_version('v1', 'One')], activeVersionId: 'v1'),
      );
      await host.controller.rename('v1', '   ');
      expect(host.practice!.versions.single.name, 'One');
      expect(host.saves, 0);
    });
  });

  group('delete', () {
    _Host twoVersions() => _Host(
      _practice(
        versions: [_version('v1', 'One'), _version('v2', 'Two')],
        activeVersionId: 'v1',
        vocalTracks: [
          _take('t1', 'v1', files: ['a1.wav', 'a2.wav']),
          _take('t2', 'v2', files: ['b.wav']),
        ],
      ),
    );

    test('removes the version + its takes, falls back the active id, and '
        'cleans up every clip file', () async {
      final host = twoVersions();
      await host.controller.delete('v1');

      expect(host.practice!.versions.map((v) => v.id), ['v2']);
      expect(host.practice!.activeVersionId, 'v2');
      expect(host.practice!.vocalTracks.map((t) => t.id), ['t2']);
      // Every clip of the doomed take — a multi-clip take must not orphan its
      // trailing clip's audio.
      expect(host.files.deleted, ['a1.wav', 'a2.wav']);
      expect(host.analysis.evicted, ['a1.wav', 'a2.wav']);
      expect(host.analysis.droppedLanes, ['t1']);
      expect(host.saves, 1);
      expect(host.toasts, isEmpty);
    });

    test('deleting a non-active version keeps the active id', () async {
      final host = twoVersions();
      await host.controller.delete('v2');
      expect(host.practice!.activeVersionId, 'v1');
      expect(host.files.deleted, ['b.wav']);
    });

    test('a failed save keeps the audio files (no orphan deletes)', () async {
      final host = twoVersions();
      host.saveResult = false;
      await host.controller.delete('v1');
      expect(host.files.deleted, isEmpty);
      expect(host.analysis.droppedLanes, isEmpty);
    });

    test('refuses to remove the last interpretation', () async {
      final host = _Host(
        _practice(
          versions: [_version('v1', 'One')],
          activeVersionId: 'v1',
          vocalTracks: [
            _take('t1', 'v1', files: ['a.wav']),
          ],
        ),
      );
      await host.controller.delete('v1');
      expect(host.practice!.versions, hasLength(1));
      expect(host.toasts, ['Keep at least one interpretation.']);
      expect(host.saves, 0);
    });

    test(
      'is blocked while separating (structural mutex), and says so',
      () async {
        // Silent, the confirm dialog's "Delete" did nothing at all.
        final host = twoVersions();
        host.separating = true;
        await host.controller.delete('v1');
        expect(host.practice!.versions, hasLength(2));
        expect(host.saves, 0);
        expect(host.toasts, ['Wait for the current edit to finish.']);
      },
    );

    test('is guarded while recording', () async {
      final host = twoVersions();
      host.recording = true;
      await host.controller.delete('v1');
      expect(host.practice!.versions, hasLength(2));
      expect(host.toasts, ['Stop recording to delete an interpretation.']);
    });
  });
}
