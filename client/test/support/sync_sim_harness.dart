import 'dart:io';
import 'dart:math';

import 'package:drift/drift.dart' show Variable;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart' show fail;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_state_store.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/result.dart';

import 'fake_sync_transport.dart';

/// Seeded randomized convergence simulation for the P0 sync stack — the
/// Automerge/Ditto-style "fuzz the whole engine, assert convergence at
/// quiescence" suite.
///
/// One episode = N devices (2..4), each a REAL persistence stack (drift DB +
/// repositories + SyncWriter + SyncEngine) over one shared [FakeCloud]. A
/// seeded op mix drives the REPOSITORY layer with realistic editor behavior
/// (held aggregates + base hashes across other devices' syncs — the exact
/// window three-way reconcile exists for), interleaved with seeded transport
/// faults (partitions, paging, held pushes, injected push conflicts, verbatim
/// page re-delivery, killed page commits).
///
/// DETERMINISM: every decision comes from one `Random(seed)`; every HLC stamp
/// comes from per-device logical clocks (`episode logical time + a per-device
/// skew`), and every row id is minted by the harness. The one deliberate
/// exception: repositories internally stamp `created_at`/`updated_at`/
/// `deleted_at` VALUES from the wall clock and asset ids from `newId()` —
/// neither feeds back into any control flow the harness exercises (target
/// selection is registry-index based, LWW is HLC-only), so the op sequence,
/// the merge decisions and the final convergence outcome are reproducible
/// from the seed alone. Differing skews make HLC order genuinely diverge from
/// episode order, so "later real-time edit loses LWW" cases are exercised for
/// real.
Future<void> runSyncSimEpisode({
  required int seed,
  int opCount = 120,
  int? deviceCount,
}) async {
  final episode = _Episode(
    seed: seed,
    opCount: opCount,
    forcedDevices: deviceCount,
  );
  await episode.run();
}

/// A fixed epoch anchor for the logical episode clock (no wall time).
final int simEpochMs = DateTime.utc(2026, 1, 1).millisecondsSinceEpoch;

// ---------------------------------------------------------------------------
// Device stack
// ---------------------------------------------------------------------------

/// One simulated device: a full persistence stack over the shared cloud.
class _Dev {
  _Dev({
    required this.name,
    required this.skewMs,
    required this.store,
    required this.transport,
    required this.engine,
    required this.assetRoot,
  }) : state = SyncStateStore(store.db),
       quarantine = QuarantineStore(store.db),
       checker = IntegrityChecker(store.db, store.writer);

  final String name;

  /// Fixed logical clock skew of this device (models real clock disagreement,
  /// well inside the HLC drift gate so nothing quarantines on time alone).
  final int skewMs;

  final Persistence store;
  final FakeSyncTransport transport;
  final SyncEngine engine;
  final Directory assetRoot;
  final SyncStateStore state;
  final QuarantineStore quarantine;
  final IntegrityChecker checker;

  AppDatabase get db => store.db;

  /// Open editors (0..2): each holds an aggregate + base hashes across other
  /// devices' syncs, exactly like a real open screen.
  final List<_Editor> editors = [];

  /// Armed by the op mix: the next pulled page's commit hook throws once
  /// (the wave-2 killed-commit path); the retry then succeeds.
  bool killNextPageCommit = false;

  /// A pumpPush parked behind [FakeSyncTransport.holdPushes], if any.
  Future<PushRun>? heldPush;
}

/// One open editor: the held aggregate + the base-hash map captured at load,
/// mutated in memory by edit ops and saved (possibly stale) later.
class _Editor {
  _Editor.practice(this.handle, Loaded<SongPractice> l)
    : isPractice = true,
      practice = l.aggregate,
      technique = null,
      base = l.baseHashes;

  _Editor.technique(this.handle, Loaded<Technique> l)
    : isPractice = false,
      practice = null,
      technique = l.aggregate,
      base = l.baseHashes;

  final String handle;
  final bool isPractice;
  SongPractice? practice;
  Technique? technique;
  Map<String, String> base;

  /// Any in-memory edit since open / last successful save.
  bool dirty = false;

  /// An edit that changes the ROOT row's content hash (rename etc.) — the only
  /// kind of edit that can revive an explicitly deleted root under LWW.
  bool rootDirty = false;

  /// (table, id) of child rows this editor removed since the last save; the
  /// tombstones land at save time and are recorded by the oracle then.
  final List<(String, String)> pendingRemovals = [];
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// The harness-side model of what liveness outcomes are legitimate.
class _Oracle {
  /// handle -> table ('practices' | 'techniques') and row id.
  final Map<String, String> rootTable = {};
  final Map<String, String> rootId = {};

  /// Every root LIVE write (creation, root-dirty editor save) and every
  /// explicit delete, as (packed hlc, live). Entity-level LWW makes the max
  /// packed hlc the authoritative liveness — see [expectedLive]. Integrity
  /// heals are the one unrecorded root writer and they only ever run on a row
  /// that is ALREADY live, so they can never flip the expected outcome — any
  /// mismatch is a real resurrection/loss bug.
  final Map<String, List<(String, bool)>> rootWrites = {};

  /// Roots the harness explicitly deleted at least once.
  final Set<String> everDeleted = {};

  /// Child rows any editor content-edited or inserted at ANY point (the
  /// conservative "an active editor held an edit over it" superset): only
  /// these may legitimately be live after an explicit removal.
  final Set<(String, String)> liveTouched = {};

  /// Child rows observed tombstoned right after an editor removal saved.
  final Set<(String, String)> explicitChildDeletes = {};

  void recordRootWrite(String handle, String hlcPacked, {required bool live}) {
    rootWrites.putIfAbsent(handle, () => []).add((hlcPacked, live));
  }

  /// Whether [handle] should be LIVE everywhere at quiescence: the recorded
  /// write with the greatest packed HLC wins (exactly the engine's LWW rule).
  bool expectedLive(String handle) {
    final writes = rootWrites[handle]!;
    var best = writes.first;
    for (final w in writes.skip(1)) {
      if (w.$1.compareTo(best.$1) > 0) best = w;
    }
    return best.$2;
  }
}

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

class _Episode {
  _Episode({required this.seed, required this.opCount, this.forcedDevices})
    : rng = Random(seed);

  final int seed;
  final int opCount;
  final int? forcedDevices;
  final Random rng;

  final FakeCloud cloud = FakeCloud();
  final List<_Dev> devices = [];
  final _Oracle oracle = _Oracle();
  final List<String> opLog = [];

  /// The shared logical episode clock; each device reads it plus its skew.
  int logicalMs = 0;

  /// Monotonic counter behind every harness-minted row id.
  int _idCounter = 0;

  /// Registry of created roots in creation order — the seed-stable identity
  /// space (actual ULID-ish ids are deterministic too, but ops always pick by
  /// registry index so the episode structure is reproducible).
  final List<String> practiceHandles = [];
  final List<String> techniqueHandles = [];

  late final Directory srcDir;
  final List<File> srcFiles = [];

  int _opIndex = 0;

  static const int _stepMs = 40;
  static const int _maxPractices = 6;
  static const int _maxTechniques = 4;
  static const int _maxEditorsPerDevice = 2;

  // --- id + time helpers -----------------------------------------------------

  /// Deterministic 26-char row id (the schema requires 26..36 chars).
  String _mintId(String tag) {
    final n = (_idCounter++).toString().padLeft(5, '0');
    return 'SIM$tag$n'.padRight(26, '0');
  }

  int _devNow(_Dev d) => simEpochMs + logicalMs + d.skewMs;

  DateTime _devDt(_Dev d) => DateTime.fromMillisecondsSinceEpoch(_devNow(d));

  void _tick() => logicalMs += _stepMs;

  void _log(String line) => opLog.add(line);

  Never _fail(String msg) {
    fail(
      '$msg\n\n'
      'SEED: $seed (0x${seed.toRadixString(16)})\n'
      'devices: ${devices.length} '
      '(skews: ${devices.map((d) => d.skewMs).join(', ')})\n'
      'REPLAYABLE OP LOG (${opLog.length} entries):\n${opLog.join('\n')}',
    );
  }

  // --- setup / teardown --------------------------------------------------------

  Future<void> _setUp() async {
    srcDir = await Directory.systemTemp.createTemp('vocara_sim_src');
    // A small pool of deterministic source "audio" files; repeated imports
    // dedup by content hash, which is the production steady state.
    for (var i = 0; i < 3; i++) {
      final f = File('${srcDir.path}/src_$i.wav');
      await f.writeAsBytes(
        List<int>.generate(64, (j) => (seed + i * 131 + j * 7) & 0xff),
      );
      srcFiles.add(f);
    }
    final n = forcedDevices ?? (2 + rng.nextInt(3)); // 2..4 devices
    for (var i = 0; i < n; i++) {
      final skew = rng.nextInt(2400);
      devices.add(_buildDevice(i, skew));
    }
  }

  _Dev _buildDevice(int i, int skewMs) {
    // Fixed 26-char node id: the final LWW tiebreak must be seed-stable.
    final node = 'SIMNODE$i'.padRight(26, '0');
    final db = AppDatabase(NativeDatabase.memory());
    late final _Dev dev;
    final clock = HlcClock(node: node, now: () => _devNow(dev));
    final writer = SyncWriter(db, clock);
    final root = Directory.systemTemp.createTempSync('vocara_sim_d$i');
    final assets = AssetStore(db: db, root: root, writer: writer);
    final store = Persistence(
      db: db,
      writer: writer,
      assets: assets,
      songRepo: SongPracticeRepositoryDrift(
        db: db,
        writer: writer,
        assets: assets,
      ),
      techRepo: TechniqueRepositoryDrift(
        db: db,
        writer: writer,
        assets: assets,
      ),
    );
    final transport = FakeSyncTransport(cloud, name: 'd$i');
    final engine = store.attachSync(
      transport,
      debugBeforePageCommit: (_) async {
        if (dev.killNextPageCommit) {
          dev.killNextPageCommit = false;
          throw StateError('sim: killed page commit on d$i');
        }
      },
    );
    dev = _Dev(
      name: 'd$i',
      skewMs: skewMs,
      store: store,
      transport: transport,
      engine: engine,
      assetRoot: root,
    );
    return dev;
  }

  Future<void> _tearDown() async {
    for (final d in devices) {
      // Unpark anything held so no future is left touching a closed database.
      d.transport.holdPushes = false;
      await d.transport.releaseHeldPushes();
      if (d.heldPush != null) {
        await d.heldPush;
        d.heldPush = null;
      }
      d.engine.dispose();
      await d.db.close();
      if (d.assetRoot.existsSync()) {
        await d.assetRoot.delete(recursive: true);
      }
    }
    if (srcDir.existsSync()) await srcDir.delete(recursive: true);
  }

  // --- top level ---------------------------------------------------------------

  Future<void> run() async {
    await _setUp();
    try {
      await _prologue();
      for (var i = 0; i < opCount; i++) {
        _opIndex = opLog.length;
        _tick();
        await _step();
      }
      await _quiesce();
      await _assertConverged();
      await _assertInvariants();
      await _assertOracle();
    } finally {
      await _tearDown();
    }
  }

  /// Guarantees shared state early: one practice created, pushed, and pulled
  /// by everyone, so every seed exercises cross-device merging.
  Future<void> _prologue() async {
    _tick();
    final d = devices[rng.nextInt(devices.length)];
    await _createPractice(d);
    _tick();
    await _pump(d, push: true);
    for (final other in devices) {
      if (identical(other, d)) continue;
      _tick();
      await _pump(other, push: false);
    }
  }

  // --- op mix --------------------------------------------------------------------

  static final List<String> _weightedOps = [
    ...List.filled(5, 'createPractice'),
    ...List.filled(3, 'createTechnique'),
    ...List.filled(12, 'openEditor'),
    ...List.filled(26, 'editHeld'),
    ...List.filled(14, 'saveHeld'),
    ...List.filled(5, 'reloadHeld'),
    ...List.filled(4, 'closeHeld'),
    ...List.filled(3, 'deletePractice'),
    ...List.filled(2, 'deleteTechnique'),
    ...List.filled(16, 'push'),
    ...List.filled(16, 'pull'),
    ...List.filled(5, 'partitionToggle'),
    ...List.filled(4, 'pageSize'),
    ...List.filled(3, 'injectConflict'),
    ...List.filled(3, 'redeliver'),
    ...List.filled(3, 'killNextPage'),
    ...List.filled(4, 'beginHeldPush'),
    ...List.filled(6, 'releaseHeldPush'),
  ];

  Future<void> _step() async {
    final op = _weightedOps[rng.nextInt(_weightedOps.length)];
    final d = devices[rng.nextInt(devices.length)];
    switch (op) {
      case 'createPractice':
        if (practiceHandles.length >= _maxPractices) {
          _log('#$_opIndex ${d.name} createPractice -> skipped (cap)');
        } else {
          await _createPractice(d);
        }
      case 'createTechnique':
        if (techniqueHandles.length >= _maxTechniques) {
          _log('#$_opIndex ${d.name} createTechnique -> skipped (cap)');
        } else {
          await _createTechnique(d);
        }
      case 'openEditor':
        await _openEditor(d);
      case 'editHeld':
        if (d.editors.isEmpty) {
          await _openEditor(d, note: 'edit->open');
        } else {
          _editHeld(d, d.editors[rng.nextInt(d.editors.length)]);
        }
      case 'saveHeld':
        if (d.editors.isEmpty) {
          _log('#$_opIndex ${d.name} saveHeld -> skipped (no editor)');
        } else {
          await _saveHeld(d, d.editors[rng.nextInt(d.editors.length)]);
        }
      case 'reloadHeld':
        if (d.editors.isEmpty) {
          _log('#$_opIndex ${d.name} reloadHeld -> skipped (no editor)');
        } else {
          await _reloadHeld(d, d.editors[rng.nextInt(d.editors.length)]);
        }
      case 'closeHeld':
        if (d.editors.isEmpty) {
          _log('#$_opIndex ${d.name} closeHeld -> skipped (no editor)');
        } else {
          final e = d.editors.removeAt(rng.nextInt(d.editors.length));
          _log('#$_opIndex ${d.name} closeHeld(${e.handle}) discarded');
        }
      case 'deletePractice':
        await _deleteRoot(d, practice: true);
      case 'deleteTechnique':
        await _deleteRoot(d, practice: false);
      case 'push':
        await _pump(d, push: true);
      case 'pull':
        await _pump(d, push: false);
      case 'partitionToggle':
        d.transport.partitioned = !d.transport.partitioned;
        _log(
          '#$_opIndex ${d.name} partition '
          '-> ${d.transport.partitioned ? 'OFF-LINE' : 'online'}',
        );
      case 'pageSize':
        d.transport.pageSize = 1 + rng.nextInt(5);
        _log('#$_opIndex ${d.name} pageSize=${d.transport.pageSize}');
      case 'injectConflict':
        _injectConflict(d);
      case 'redeliver':
        d.transport.redeliverNextFetches = 1 + rng.nextInt(2);
        _log(
          '#$_opIndex ${d.name} '
          'redeliverNextFetches=${d.transport.redeliverNextFetches}',
        );
      case 'killNextPage':
        d.killNextPageCommit = true;
        _log('#$_opIndex ${d.name} armed killNextPageCommit');
      case 'beginHeldPush':
        await _beginHeldPush(d);
      case 'releaseHeldPush':
        if (d.heldPush == null) {
          _log('#$_opIndex ${d.name} releaseHeldPush -> skipped (none)');
        } else {
          await _releaseHeld(d);
        }
    }
  }

  // --- creation -------------------------------------------------------------------

  Future<void> _createPractice(_Dev d) async {
    final handle = 'P${practiceHandles.length}';
    final id = _mintId('PR');
    final version = TakeVersion(
      id: _mintId('IV'),
      name: 'Interpretation 1',
      createdAt: _devDt(d),
    );
    final practice = SongPractice(
      id: id,
      title: '$handle v0',
      versions: [version],
      activeVersionId: version.id,
      createdAt: _devDt(d),
      updatedAt: _devDt(d),
    );
    // Creation flow: the base-less legacy save is the documented path for a
    // fresh aggregate (see savePractice docs / the round-trip suite).
    final res = await d.store.songRepo.savePractice(practice);
    if (res is! Ok<SongPractice>) {
      _fail(
        '#$_opIndex ${d.name} createPractice($handle) failed: '
        '${(res as Error<SongPractice>).error}',
      );
    }
    practiceHandles.add(handle);
    oracle.rootTable[handle] = 'practices';
    oracle.rootId[handle] = id;
    oracle.recordRootWrite(
      handle,
      await _rootHlc(d, 'practices', id),
      live: true,
    );
    _log('#$_opIndex ${d.name} createPractice($handle)');
  }

  Future<void> _createTechnique(_Dev d) async {
    final handle = 'T${techniqueHandles.length}';
    final id = _mintId('TQ');
    final technique = Technique(
      id: id,
      name: '$handle v0',
      summary: 'summary v0',
      createdAt: _devDt(d),
      updatedAt: _devDt(d),
    );
    final res = await d.store.techRepo.saveTechnique(technique);
    if (res is! Ok<Technique>) {
      _fail(
        '#$_opIndex ${d.name} createTechnique($handle) failed: '
        '${(res as Error<Technique>).error}',
      );
    }
    techniqueHandles.add(handle);
    oracle.rootTable[handle] = 'techniques';
    oracle.rootId[handle] = id;
    oracle.recordRootWrite(
      handle,
      await _rootHlc(d, 'techniques', id),
      live: true,
    );
    _log('#$_opIndex ${d.name} createTechnique($handle)');
  }

  // --- editor lifecycle --------------------------------------------------------------

  /// Roots of [table] that are LIVE on [d] right now, in registry order.
  Future<List<String>> _liveHandlesOn(_Dev d, {required bool practice}) async {
    final handles = practice ? practiceHandles : techniqueHandles;
    final out = <String>[];
    for (final h in handles) {
      if (await _isLive(d, oracle.rootTable[h]!, oracle.rootId[h]!)) out.add(h);
    }
    return out;
  }

  Future<bool> _isLive(_Dev d, String table, String id) async {
    final row = await d.db
        .customSelect(
          'SELECT deleted_at AS x FROM $table WHERE id = ?',
          variables: [Variable.withString(id)],
        )
        .getSingleOrNull();
    return row != null && row.data['x'] == null;
  }

  Future<String> _rootHlc(_Dev d, String table, String id) async {
    final row = await d.db
        .customSelect(
          'SELECT hlc_packed AS h FROM $table WHERE id = ?',
          variables: [Variable.withString(id)],
        )
        .getSingleOrNull();
    if (row == null) {
      _fail('#$_opIndex ${d.name} expected a $table row for $id');
    }
    return row.data['h']! as String;
  }

  Future<void> _openEditor(_Dev d, {String note = ''}) async {
    if (d.editors.length >= _maxEditorsPerDevice) {
      _log('#$_opIndex ${d.name} openEditor -> skipped (2 already open)');
      return;
    }
    final wantPractice = techniqueHandles.isEmpty || rng.nextInt(10) < 7;
    final candidates = await _liveHandlesOn(d, practice: wantPractice);
    if (candidates.isEmpty) {
      _log('#$_opIndex ${d.name} openEditor -> skipped (nothing live)');
      return;
    }
    final handle = candidates[rng.nextInt(candidates.length)];
    final id = oracle.rootId[handle]!;
    if (wantPractice) {
      final res = await d.store.songRepo.loadPractice(id);
      if (res is! Ok<Loaded<SongPractice>>) {
        _log('#$_opIndex ${d.name} openEditor($handle) -> load failed');
        return;
      }
      d.editors.add(_Editor.practice(handle, res.value));
    } else {
      final res = await d.store.techRepo.loadTechnique(id);
      if (res is! Ok<Loaded<Technique>>) {
        _log('#$_opIndex ${d.name} openEditor($handle) -> load failed');
        return;
      }
      d.editors.add(_Editor.technique(handle, res.value));
    }
    _log('#$_opIndex ${d.name} openEditor($handle)$note');
  }

  Future<void> _reloadHeld(_Dev d, _Editor e) async {
    final id = oracle.rootId[e.handle]!;
    if (e.isPractice) {
      final res = await d.store.songRepo.loadPractice(id);
      if (res is Ok<Loaded<SongPractice>>) {
        e.practice = res.value.aggregate;
        e.base = res.value.baseHashes;
        e.dirty = false;
        e.rootDirty = false;
        e.pendingRemovals.clear();
        _log('#$_opIndex ${d.name} reload(${e.handle})');
      } else {
        d.editors.remove(e);
        _log('#$_opIndex ${d.name} reload(${e.handle}) -> gone, editor closed');
      }
    } else {
      final res = await d.store.techRepo.loadTechnique(id);
      if (res is Ok<Loaded<Technique>>) {
        e.technique = res.value.aggregate;
        e.base = res.value.baseHashes;
        e.dirty = false;
        e.rootDirty = false;
        e.pendingRemovals.clear();
        _log('#$_opIndex ${d.name} reload(${e.handle})');
      } else {
        d.editors.remove(e);
        _log('#$_opIndex ${d.name} reload(${e.handle}) -> gone, editor closed');
      }
    }
  }

  // --- in-memory edits -----------------------------------------------------------------

  void _editHeld(_Dev d, _Editor e) {
    if (e.isPractice) {
      _editPractice(d, e);
    } else {
      _editTechnique(d, e);
    }
  }

  void _touch(String table, String id) => oracle.liveTouched.add((table, id));

  void _editPractice(_Dev d, _Editor e) {
    final p = e.practice!;
    final vocals = p.vocalTracks;
    // Weighted list of applicable edit actions (label, apply).
    final actions = <(String, SongPractice Function())>[];

    void add(String label, SongPractice Function() apply, {int weight = 1}) {
      for (var i = 0; i < weight; i++) {
        actions.add((label, apply));
      }
    }

    add('rename', weight: 3, () {
      e.rootDirty = true;
      return p.copyWith(title: '${e.handle} v$_opIndex');
    });
    add('masterVolume', () {
      final v = (1 + rng.nextInt(10)) / 10.0;
      if (v != p.masterVolume) e.rootDirty = true;
      return p.copyWith(masterVolume: v);
    });
    add('addTake', weight: 2, () {
      final id = _mintId('TK');
      final file = srcFiles[rng.nextInt(srcFiles.length)];
      _touch('tracks', id);
      _touch('clips', '$id-c0');
      final take = AudioTrack(
        id: id,
        kind: AudioTrackKind.vocal,
        displayName: 'take v$_opIndex',
        audioFilePath: file.path,
        durationMs: 800 + rng.nextInt(2000),
        createdAt: _devDt(d),
        versionId: p.activeVersionId,
      );
      return p.copyWith(vocalTracks: [...vocals, take]);
    });
    if (vocals.isNotEmpty) {
      add('trackFader', weight: 4, () {
        final i = rng.nextInt(vocals.length);
        final t = vocals[i];
        _touch('tracks', t.id);
        final next = t.copyWith(
          volume: (1 + rng.nextInt(10)) / 10.0,
          pan: (rng.nextInt(9) - 4) / 4.0,
        );
        return p.copyWith(vocalTracks: [...vocals]..[i] = next);
      });
      add('renameTrack', () {
        final i = rng.nextInt(vocals.length);
        _touch('tracks', vocals[i].id);
        return p.copyWith(
          vocalTracks: [...vocals]
            ..[i] = vocals[i].copyWith(displayName: 'take v$_opIndex'),
        );
      });
      add('addNote', weight: 3, () {
        final i = rng.nextInt(vocals.length);
        final t = vocals[i];
        final id = _mintId('NT');
        _touch('practice_notes', id);
        final note = PracticeNote(
          id: id,
          positionMs: rng.nextInt(30) * 100,
          content: 'note v$_opIndex',
          createdAt: _devDt(d),
          updatedAt: _devDt(d),
        );
        return p.copyWith(
          vocalTracks: [...vocals]..[i] = t.copyWith(notes: [...t.notes, note]),
        );
      });
      add('addMarker', weight: 2, () {
        final i = rng.nextInt(vocals.length);
        final t = vocals[i];
        final id = _mintId('MK');
        _touch('technique_markers', id);
        // Markers reference techniques by SOFT id + name snapshot by design.
        final techName = techniqueHandles.isEmpty
            ? 'freehand'
            : techniqueHandles[rng.nextInt(techniqueHandles.length)];
        final marker = TechniqueMarker(
          id: id,
          positionMs: rng.nextInt(30) * 100,
          techniqueId: techniqueHandles.isEmpty
              ? _mintId('XX')
              : oracle.rootId[techName]!,
          techniqueName: techName,
          createdAt: _devDt(d),
          updatedAt: _devDt(d),
        );
        return p.copyWith(
          vocalTracks: [...vocals]
            ..[i] = t.copyWith(
              techniqueMarkers: [...t.techniqueMarkers, marker],
            ),
        );
      });
      add('removeTake', () {
        final i = rng.nextInt(vocals.length);
        final t = vocals[i];
        e.pendingRemovals.add(('tracks', t.id));
        for (final c in t.clips) {
          e.pendingRemovals.add(('clips', c.id));
        }
        for (final n in t.notes) {
          e.pendingRemovals.add(('practice_notes', n.id));
        }
        for (final m in t.techniqueMarkers) {
          e.pendingRemovals.add(('technique_markers', m.id));
        }
        return p.copyWith(vocalTracks: [...vocals]..removeAt(i));
      });
      final withNotes = [
        for (final t in vocals)
          if (t.notes.isNotEmpty) t,
      ];
      if (withNotes.isNotEmpty) {
        add('editNote', weight: 3, () {
          final t = withNotes[rng.nextInt(withNotes.length)];
          final ni = rng.nextInt(t.notes.length);
          _touch('practice_notes', t.notes[ni].id);
          final notes = [...t.notes]
            ..[ni] = t.notes[ni].copyWith(
              content: 'note v$_opIndex edited',
              updatedAt: _devDt(d),
            );
          final ti = vocals.indexOf(t);
          return p.copyWith(
            vocalTracks: [...vocals]..[ti] = t.copyWith(notes: notes),
          );
        });
        add('removeNote', () {
          final t = withNotes[rng.nextInt(withNotes.length)];
          final ni = rng.nextInt(t.notes.length);
          e.pendingRemovals.add(('practice_notes', t.notes[ni].id));
          final ti = vocals.indexOf(t);
          return p.copyWith(
            vocalTracks: [...vocals]
              ..[ti] = t.copyWith(notes: [...t.notes]..removeAt(ni)),
          );
        });
      }
      final withMarkers = [
        for (final t in vocals)
          if (t.techniqueMarkers.isNotEmpty) t,
      ];
      if (withMarkers.isNotEmpty) {
        add('removeMarker', () {
          final t = withMarkers[rng.nextInt(withMarkers.length)];
          final mi = rng.nextInt(t.techniqueMarkers.length);
          e.pendingRemovals.add((
            'technique_markers',
            t.techniqueMarkers[mi].id,
          ));
          final ti = vocals.indexOf(t);
          return p.copyWith(
            vocalTracks: [...vocals]
              ..[ti] = t.copyWith(
                techniqueMarkers: [...t.techniqueMarkers]..removeAt(mi),
              ),
          );
        });
      }
    }
    add('addBus', () {
      final id = _mintId('BU');
      _touch('fx_buses', id);
      final bus = FxBus(
        id: id,
        name: 'bus v$_opIndex',
        fx: TrackFx.flat(),
        volume: (1 + rng.nextInt(10)) / 10.0,
      );
      return p.copyWith(fxBuses: [...p.fxBuses, bus]);
    });
    if (p.fxBuses.isNotEmpty) {
      if (vocals.isNotEmpty) {
        add('routeTrackToBus', weight: 2, () {
          final ti = rng.nextInt(vocals.length);
          final t = vocals[ti];
          _touch('tracks', t.id);
          final unroute = rng.nextInt(4) == 0;
          final next = unroute
              ? t.copyWith(clearBus: true)
              : t.copyWith(busId: p.fxBuses[rng.nextInt(p.fxBuses.length)].id);
          return p.copyWith(vocalTracks: [...vocals]..[ti] = next);
        });
      }
      add('removeBus', () {
        final bi = rng.nextInt(p.fxBuses.length);
        final bus = p.fxBuses[bi];
        e.pendingRemovals.add(('fx_buses', bus.id));
        // A realistic editor clears the routing of tracks feeding the removed
        // bus in the same save (the mixer UI does exactly that).
        final nextVocals = [
          for (final t in vocals)
            if (t.busId == bus.id) t.copyWith(clearBus: true) else t,
        ];
        for (final t in vocals) {
          if (t.busId == bus.id) _touch('tracks', t.id);
        }
        return p.copyWith(
          fxBuses: [...p.fxBuses]..removeAt(bi),
          vocalTracks: nextVocals,
        );
      });
    }
    if (p.accompaniment == null) {
      add('setAccompaniment', () {
        final id = _mintId('AC');
        _touch('tracks', id);
        _touch('clips', '$id-c0');
        final acc = AudioTrack(
          id: id,
          kind: AudioTrackKind.accompaniment,
          displayName: 'accompaniment v$_opIndex',
          audioFilePath: srcFiles[rng.nextInt(srcFiles.length)].path,
          durationMs: 2000,
          createdAt: _devDt(d),
        );
        return p.copyWith(accompaniment: acc);
      });
    } else {
      add('removeAccompaniment', () {
        final acc = p.accompaniment!;
        e.pendingRemovals.add(('tracks', acc.id));
        for (final c in acc.clips) {
          e.pendingRemovals.add(('clips', c.id));
        }
        return p.copyWith(clearAccompaniment: true);
      });
    }

    final (label, apply) = actions[rng.nextInt(actions.length)];
    e.practice = apply();
    e.dirty = true;
    _log('#$_opIndex ${d.name} edit(${e.handle}) $label');
  }

  void _editTechnique(_Dev d, _Editor e) {
    final t = e.technique!;
    final actions = <(String, Technique Function())>[];

    void add(String label, Technique Function() apply, {int weight = 1}) {
      for (var i = 0; i < weight; i++) {
        actions.add((label, apply));
      }
    }

    add('rename', weight: 2, () {
      e.rootDirty = true;
      return t.copyWith(name: '${e.handle} v$_opIndex');
    });
    add('editSummary', weight: 2, () {
      e.rootDirty = true;
      return t.copyWith(summary: 'summary v$_opIndex');
    });
    add('editCues', () {
      e.rootDirty = true;
      return t.copyWith(cues: 'cues v$_opIndex');
    });
    add('addMedia', () {
      final id = _mintId('ME');
      _touch('technique_media', id);
      final media = TechniqueMedia(
        id: id,
        path: srcFiles[rng.nextInt(srcFiles.length)].path,
        kind: TechniqueMediaKind.audio,
        label: 'ex v$_opIndex',
        addedAt: _devDt(d),
      );
      return t.copyWith(media: [...t.media, media]);
    });
    if (t.media.isNotEmpty) {
      add('editMediaLabel', () {
        final mi = rng.nextInt(t.media.length);
        _touch('technique_media', t.media[mi].id);
        final media = [...t.media]
          ..[mi] = t.media[mi].copyWith(label: 'ex v$_opIndex edited');
        return t.copyWith(media: media);
      });
      add('removeMedia', () {
        final mi = rng.nextInt(t.media.length);
        e.pendingRemovals.add(('technique_media', t.media[mi].id));
        return t.copyWith(media: [...t.media]..removeAt(mi));
      });
    }

    final (label, apply) = actions[rng.nextInt(actions.length)];
    e.technique = apply();
    e.dirty = true;
    _log('#$_opIndex ${d.name} edit(${e.handle}) $label');
  }

  // --- saves / deletes ---------------------------------------------------------------

  Future<void> _saveHeld(_Dev d, _Editor e) async {
    final rootDirtyAtSave = e.rootDirty;
    final removals = List.of(e.pendingRemovals);
    Object? error;
    if (e.isPractice) {
      final res = await d.store.songRepo.savePracticeWithBase(
        e.practice!,
        baseHashes: e.base,
      );
      if (res is Ok<Loaded<SongPractice>>) {
        e.practice = res.value.aggregate;
        e.base = res.value.baseHashes;
      } else {
        error = (res as Error<Loaded<SongPractice>>).error;
      }
    } else {
      final res = await d.store.techRepo.saveTechniqueWithBase(
        e.technique!,
        baseHashes: e.base,
      );
      if (res is Ok<Loaded<Technique>>) {
        e.technique = res.value.aggregate;
        e.base = res.value.baseHashes;
      } else {
        error = (res as Error<Loaded<Technique>>).error;
      }
    }
    if (error != null) {
      // A failed save is a legitimate outcome (e.g. the root was tombstoned
      // under the editor): the UI reverts, we close the editor.
      d.editors.remove(e);
      _log(
        '#$_opIndex ${d.name} save(${e.handle}) FAILED '
        '(${_firstLine(error)}), editor closed',
      );
      return;
    }
    e.dirty = false;
    e.rootDirty = false;
    e.pendingRemovals.clear();
    // Oracle: a root-dirty save that committed holds live root content at the
    // row's (possibly fresh) HLC; record it as a live write.
    final table = oracle.rootTable[e.handle]!;
    final id = oracle.rootId[e.handle]!;
    if (rootDirtyAtSave) {
      oracle.recordRootWrite(
        e.handle,
        await _rootHlc(d, table, id),
        live: true,
      );
    }
    // Oracle: removals that actually tombstoned a persisted row are explicit
    // deletes; if such a row is live at quiescence, some editor must have
    // touched it (rule 3/5: active editor wins) — anything else is a
    // resurrection bug.
    for (final (childTable, childId) in removals) {
      final row = await d.db
          .customSelect(
            'SELECT deleted_at AS x FROM $childTable WHERE id = ?',
            variables: [Variable.withString(childId)],
          )
          .getSingleOrNull();
      if (row != null && row.data['x'] != null) {
        oracle.explicitChildDeletes.add((childTable, childId));
      }
    }
    _log(
      '#$_opIndex ${d.name} save(${e.handle})'
      '${rootDirtyAtSave ? ' rootDirty' : ''}'
      '${removals.isEmpty ? '' : ' removals:${removals.length}'}',
    );
  }

  Future<void> _deleteRoot(_Dev d, {required bool practice}) async {
    final candidates = await _liveHandlesOn(d, practice: practice);
    if (candidates.isEmpty) {
      _log(
        '#$_opIndex ${d.name} delete${practice ? 'Practice' : 'Technique'} '
        '-> skipped (nothing live)',
      );
      return;
    }
    final handle = candidates[rng.nextInt(candidates.length)];
    final table = oracle.rootTable[handle]!;
    final id = oracle.rootId[handle]!;
    final res = practice
        ? await d.store.songRepo.deletePractice(id)
        : await d.store.techRepo.deleteTechnique(id);
    if (res is Error<void>) {
      _fail('#$_opIndex ${d.name} delete($handle) failed: ${res.error}');
    }
    oracle.everDeleted.add(handle);
    oracle.recordRootWrite(handle, await _rootHlc(d, table, id), live: false);
    // Editors over the deleted root on THIS device would fail their next save
    // anyway; leave them open — that is exactly the stale-editor case.
    _log('#$_opIndex ${d.name} delete($handle)');
  }

  // --- transport ---------------------------------------------------------------------

  Future<void> _pump(_Dev d, {required bool push}) async {
    // Never queue behind a parked push (the engine serializes push and pull):
    // release it first, exactly like delivery completing before the next op.
    if (d.heldPush != null) await _releaseHeld(d);
    if (push) {
      final run = await d.engine.pumpPush();
      _log('#$_opIndex ${d.name} push -> ${_describePush(run)}');
    } else {
      final run = await d.engine.pumpPull();
      _log('#$_opIndex ${d.name} pull -> ${_describePull(run)}');
    }
  }

  Future<void> _beginHeldPush(_Dev d) async {
    if (d.heldPush != null) {
      _log('#$_opIndex ${d.name} beginHeldPush -> skipped (already held)');
      return;
    }
    d.transport.holdPushes = true;
    d.heldPush = d.engine.pumpPush();
    _log('#$_opIndex ${d.name} beginHeldPush (pump parked)');
  }

  Future<void> _releaseHeld(_Dev d) async {
    final parked = d.heldPush;
    if (parked == null) return;
    // Drop the hold BEFORE releasing so follow-up chunks of the same pump
    // flow through instead of parking again.
    d.transport.holdPushes = false;
    await d.transport.releaseHeldPushes();
    final run = await parked;
    d.heldPush = null;
    _log('#$_opIndex ${d.name} releaseHeldPush -> ${_describePush(run)}');
  }

  void _injectConflict(_Dev d) {
    final all = [...practiceHandles, ...techniqueHandles];
    if (all.isEmpty) {
      _log('#$_opIndex ${d.name} injectConflict -> skipped (no roots)');
      return;
    }
    final handle = all[rng.nextInt(all.length)];
    final times = 1 + rng.nextInt(2);
    d.transport.injectConflictOnce(
      oracle.rootTable[handle]!,
      oracle.rootId[handle]!,
      times: times,
    );
    _log('#$_opIndex ${d.name} injectConflict($handle x$times)');
  }

  String _describePush(PushRun r) => r.failed
      ? 'FAILED(${_firstLine(r.failure!)})'
      : 'pushed:${r.pushed} held:${r.heldBack}';

  String _describePull(PullRun r) => r.failed
      ? 'pages:${r.pages} applied:${r.applied} FAILED(${_firstLine(r.failure!)})'
      : 'pages:${r.pages} applied:${r.applied} skipped:${r.skipped}';

  static String _firstLine(Object o) {
    final s = o.toString();
    final nl = s.indexOf('\n');
    final line = nl < 0 ? s : s.substring(0, nl);
    return line.length > 90 ? '${line.substring(0, 90)}…' : line;
  }

  // --- quiescence ----------------------------------------------------------------------

  Future<void> _quiesce() async {
    // Close every held editor: save or discard per seed (a failing save is a
    // discard). This drains editor intent into the store before settling.
    for (final d in devices) {
      for (final e in List.of(d.editors)) {
        _opIndex = opLog.length;
        _tick();
        if (e.dirty && rng.nextInt(10) < 7) {
          await _saveHeld(d, e);
        } else {
          _log('#$_opIndex ${d.name} close(${e.handle}) discarded at quiesce');
        }
        d.editors.remove(e);
      }
    }
    // Clear every fault so the settle loop can actually converge.
    for (final d in devices) {
      d.transport.partitioned = false;
      d.killNextPageCommit = false;
      d.transport.redeliverNextFetches = 0;
      await _releaseHeld(d);
    }
    // Settle: pump everyone until no device has pending oplog entries and
    // every change token sits at the cloud head. Non-termination is a bug.
    const maxRounds = 40;
    for (var round = 0; round < maxRounds; round++) {
      _opIndex = opLog.length;
      _tick();
      for (final d in devices) {
        final run = await d.engine.pumpPush();
        if (run.failed) {
          _fail('settle round $round: ${d.name} push failed: ${run.failure}');
        }
      }
      for (final d in devices) {
        final run = await d.engine.pumpPull();
        if (run.failed) {
          _fail('settle round $round: ${d.name} pull failed: ${run.failure}');
        }
      }
      var stable = true;
      for (final d in devices) {
        final pending = await d.db.select(d.db.oplog).get();
        final token = await d.state.changeToken();
        if (pending.isNotEmpty ||
            token != '${cloud.latestSeq}' ||
            d.transport.heldPushCount > 0) {
          stable = false;
          break;
        }
      }
      if (stable) {
        _log(
          'settled after ${round + 1} rounds '
          '(cloud rows: ${cloud.latestSeq})',
        );
        return;
      }
    }
    _fail(
      'did not settle within $maxRounds rounds — '
      'sync is not quiescing (non-termination bug)',
    );
  }

  // --- assertions -------------------------------------------------------------------------

  /// Full-DB snapshot of every synced table: 'table/id' ->
  /// 'hlcPacked|deletedAt|contentHash'. Content is hashed through the SAME
  /// wave-1 sync_hashes projections both save and hydrate use, for LIVE rows;
  /// tombstones compare (hlc, deletedAt) only. Assets compare their durable
  /// content identity (hash/size/ext/kind) — local_rel_path and the P2 byte
  /// markers are per-device by design.
  Future<Map<String, String>> _snapshot(_Dev d) async {
    final out = <String, String>{};
    void put(String table, String id, String hlc, int? del, String hash) {
      out['$table/$id'] = '$hlc|$del|${del == null ? hash : '-'}';
    }

    final db = d.db;
    for (final r in await db.select(db.practices).get()) {
      put('practices', r.id, r.hlcPacked, r.deletedAt, practiceRowHash(r));
    }
    for (final r in await db.select(db.interpretations).get()) {
      put(
        'interpretations',
        r.id,
        r.hlcPacked,
        r.deletedAt,
        interpretationRowHash(r),
      );
    }
    for (final r in await db.select(db.tracks).get()) {
      put('tracks', r.id, r.hlcPacked, r.deletedAt, trackRowHash(r));
    }
    for (final r in await db.select(db.clips).get()) {
      put('clips', r.id, r.hlcPacked, r.deletedAt, clipRowHash(r));
    }
    for (final r in await db.select(db.practiceNotes).get()) {
      put(
        'practice_notes',
        r.id,
        r.hlcPacked,
        r.deletedAt,
        practiceNoteRowHash(r),
      );
    }
    for (final r in await db.select(db.techniqueMarkers).get()) {
      put(
        'technique_markers',
        r.id,
        r.hlcPacked,
        r.deletedAt,
        techniqueMarkerRowHash(r),
      );
    }
    for (final r in await db.select(db.fxBuses).get()) {
      put('fx_buses', r.id, r.hlcPacked, r.deletedAt, fxBusRowHash(r));
    }
    for (final r in await db.select(db.techniques).get()) {
      put('techniques', r.id, r.hlcPacked, r.deletedAt, techniqueRowHash(r));
    }
    for (final r in await db.select(db.techniqueMedia).get()) {
      put(
        'technique_media',
        r.id,
        r.hlcPacked,
        r.deletedAt,
        techniqueMediaRowHash(r),
      );
    }
    for (final r in await db.select(db.assets).get()) {
      put(
        'assets',
        r.id,
        r.hlcPacked,
        r.deletedAt,
        '${r.contentHash}:${r.byteSize}:${r.ext}:${r.kind}',
      );
    }
    return out;
  }

  Future<void> _assertConverged() async {
    final reference = await _snapshot(devices.first);
    for (final d in devices.skip(1)) {
      final other = await _snapshot(d);
      if (_mapsEqual(reference, other)) continue;
      // Find and report the FIRST diverging table/id (sorted for stability),
      // with enough context to tell push-side loss from apply-side loss.
      final keys = {...reference.keys, ...other.keys}.toList()..sort();
      final diverging = keys.where((k) => reference[k] != other[k]).toList();
      final k = diverging.first;
      final parts = k.split('/');
      final cloudRow = cloud.serverRowFor(parts[0], parts[1]);
      final quarantined = <String>[];
      for (final dev in devices) {
        for (final q in await dev.quarantine.list()) {
          if (q.tbl == parts[0] && q.id == parts[1]) {
            quarantined.add('${dev.name}: ${q.reason}');
          }
        }
      }
      // For the cloud's copy of the row, resolve every FK-ish field against
      // both devices — tells an FK-sweep drop from a plain LWW mixup at a
      // glance.
      final fkNotes = <String>[];
      if (cloudRow != null) {
        for (final fk in const [
          'practice_id',
          'track_id',
          'technique_id',
          'asset_id',
          'interpretation_id',
          'bus_id',
        ]) {
          final v = cloudRow.fields[fk];
          if (v is! String) continue;
          final parentTable = switch (fk) {
            'practice_id' => 'practices',
            'track_id' => 'tracks',
            'technique_id' => 'techniques',
            'asset_id' => 'assets',
            'interpretation_id' => 'interpretations',
            'bus_id' => 'fx_buses',
            _ => null,
          };
          if (parentTable == null) continue;
          final states = <String>[];
          for (final dev in [devices.first, d]) {
            final row = await dev.db
                .customSelect(
                  'SELECT deleted_at AS x FROM $parentTable WHERE id = ?',
                  variables: [Variable.withString(v)],
                )
                .getSingleOrNull();
            states.add(
              '${dev.name}:${row == null ? 'missing' : (row.data['x'] == null ? 'live' : 'dead')}',
            );
          }
          fkNotes.add('$fk=$v (${states.join(' ')})');
        }
      }
      _fail(
        'CONVERGENCE FAILURE: first diverging row: $k\n'
        '  ${devices.first.name}: ${reference[k] ?? '<missing>'}\n'
        '  ${d.name}: ${other[k] ?? '<missing>'}\n'
        '  cloud: ${cloudRow == null ? '<never accepted>' : '${cloudRow.hlcPacked}|${cloudRow.fields['deleted_at']}'}\n'
        '  fk targets: ${fkNotes.isEmpty ? 'n/a' : fkNotes.join('; ')}\n'
        '  quarantined: ${quarantined.isEmpty ? 'nowhere' : quarantined.join('; ')}\n'
        '  (${diverging.length} diverging rows total: '
        '${diverging.take(8).join(', ')}${diverging.length > 8 ? ', …' : ''})',
      );
    }
  }

  bool _mapsEqual(Map<String, String> a, Map<String, String> b) {
    if (a.length != b.length) return false;
    for (final e in a.entries) {
      if (b[e.key] != e.value) return false;
    }
    return true;
  }

  Future<void> _assertInvariants() async {
    for (final d in devices) {
      // 1. Everything the IntegrityChecker claims to keep must actually hold:
      //    no live child of a tombstoned parent, no dangling scalar pointer
      //    (active_interpretation_id / bus_id / interpretation_id /
      //    output_bus_id / separation_asset_id), no duplicate live slot.
      final violations = await d.checker.check();
      if (violations.isNotEmpty) {
        _fail(
          'INVARIANT FAILURE on ${d.name}: IntegrityChecker.check() found:'
          '\n  ${violations.join('\n  ')}',
        );
      }
      // 2. Every live clip's asset ROW exists (bytes may be absent in P0).
      final orphanClips = await d.db
          .customSelect(
            'SELECT c.id AS id FROM clips c WHERE c.deleted_at IS NULL AND '
            'NOT EXISTS (SELECT 1 FROM assets a WHERE a.id = c.asset_id)',
          )
          .get();
      if (orphanClips.isNotEmpty) {
        _fail(
          'INVARIANT FAILURE on ${d.name}: live clips without asset rows: '
          '${orphanClips.map((r) => r.data['id']).join(', ')}',
        );
      }
      // 3. Oplog fully drained.
      final oplog = await d.db.select(d.db.oplog).get();
      if (oplog.isNotEmpty) {
        _fail(
          'INVARIANT FAILURE on ${d.name}: oplog not drained '
          '(${oplog.length} rows)',
        );
      }
      // 4. Nothing quarantined: the episode injects no HLC drift and no
      //    malformed rows, so any quarantine row is an unexplained rejection.
      final parked = await d.quarantine.list();
      if (parked.isNotEmpty) {
        _fail(
          'INVARIANT FAILURE on ${d.name}: unexplained quarantine rows:\n'
          '  ${parked.map((q) => '${q.tbl}/${q.id}: ${q.reason}').join('\n  ')}',
        );
      }
      // 5. The change token sits exactly at the cloud head.
      final token = await d.state.changeToken();
      if (token != '${cloud.latestSeq}') {
        _fail(
          'INVARIANT FAILURE on ${d.name}: change token $token != '
          'cloud head ${cloud.latestSeq}',
        );
      }
    }
  }

  Future<void> _assertOracle() async {
    // Roots: the recorded write with the max HLC decides liveness everywhere.
    // In particular an explicitly deleted root whose delete out-HLCs every
    // editor save must be tombstoned on ALL devices — tombstones never
    // resurrect; a root revived by a higher-HLC editor save (rule 3 at the
    // LWW level: the active editor wins) must be live on ALL devices.
    for (final handle in [...practiceHandles, ...techniqueHandles]) {
      final table = oracle.rootTable[handle]!;
      final id = oracle.rootId[handle]!;
      final expectLive = oracle.expectedLive(handle);
      for (final d in devices) {
        final row = await d.db
            .customSelect(
              'SELECT deleted_at AS x FROM $table WHERE id = ?',
              variables: [Variable.withString(id)],
            )
            .getSingleOrNull();
        if (row == null) {
          _fail(
            'ORACLE FAILURE: $handle ($table/$id) has NO row on ${d.name} '
            'at quiescence (row lost)',
          );
        }
        final live = row.data['x'] == null;
        if (live != expectLive) {
          final writes = oracle.rootWrites[handle]!
              .map((w) => '${w.$1}=${w.$2 ? 'live' : 'delete'}')
              .join(', ');
          final states = <String>[];
          for (final dev in devices) {
            final r = await dev.db
                .customSelect(
                  'SELECT hlc_packed AS h, deleted_at AS x FROM $table '
                  'WHERE id = ?',
                  variables: [Variable.withString(id)],
                )
                .getSingleOrNull();
            states.add(
              '${dev.name}: '
              '${r == null ? '<missing>' : '${r.data['h']}|${r.data['x']}'}',
            );
          }
          // The cloud log keeps every accepted version in push order — the
          // forensic trail of who uploaded what.
          final history = [
            for (final entry in cloud.log)
              if (entry.row.table == table && entry.row.id == id)
                'seq${entry.seq}: ${entry.row.hlcPacked}'
                    '|${entry.row.fields['deleted_at']}',
          ];
          _fail(
            'ORACLE FAILURE: $handle ($table/$id) on ${d.name} is '
            '${live ? 'LIVE' : 'TOMBSTONED'} but the recorded writes say '
            '${expectLive ? 'live' : 'tombstoned'} '
            '(deleted: ${oracle.everDeleted.contains(handle)})\n'
            '  recorded writes: $writes\n'
            '  row state: ${states.join('; ')}\n'
            '  cloud history:\n    ${history.join('\n    ')}',
          );
        }
      }
    }
    // Children: an explicitly removed child may be live at quiescence ONLY if
    // some editor content-edited or inserted it at some point (the active-
    // editor-wins revive); everything else must stay tombstoned everywhere.
    for (final (table, id) in oracle.explicitChildDeletes) {
      if (oracle.liveTouched.contains((table, id))) continue;
      for (final d in devices) {
        final row = await d.db
            .customSelect(
              'SELECT deleted_at AS x FROM $table WHERE id = ?',
              variables: [Variable.withString(id)],
            )
            .getSingleOrNull();
        if (row != null && row.data['x'] == null) {
          _fail(
            'ORACLE FAILURE: explicitly removed $table/$id resurrected on '
            '${d.name} although no editor ever touched it',
          );
        }
      }
    }
  }
}
