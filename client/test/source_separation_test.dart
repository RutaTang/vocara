import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/source_separation_service.dart';

/// Storage fake serving collection directories under one temp root, so the
/// process service's stems cache / script dir land in an isolated sandbox.
class _DirStorage implements LocalStorageService {
  _DirStorage(this.root);

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

  @override
  Future<Map<String, dynamic>?> read(String collection, String id) async =>
      null;

  @override
  Future<void> write(
    String collection,
    String id,
    Map<String, dynamic> json,
  ) async {}
}

void main() {
  // The process-service tests exercise _ensureScript, which loads the bundled
  // sidecar via rootBundle — that needs the test binding's asset resolution.
  TestWidgetsFlutterBinding.ensureInitialized();

  group('FakeSourceSeparationService', () {
    test(
      'produces two distinct stem files and reports progress to 1.0',
      () async {
        final src = File(
          '${Directory.systemTemp.path}/vocara_src_'
          '${DateTime.now().microsecondsSinceEpoch}.wav',
        );
        await src.writeAsBytes(List<int>.filled(64, 7));
        addTearDown(() => src.delete());

        final svc = FakeSourceSeparationService();
        final progress = <double>[];
        final result = await svc.separate(src.path, onProgress: progress.add);

        expect(await File(result.instrumentalPath).exists(), isTrue);
        expect(await File(result.vocalPath).exists(), isTrue);
        expect(result.instrumentalPath, isNot(result.vocalPath));
        // The Fake copies the source verbatim into both stems.
        expect(
          await File(result.instrumentalPath).readAsBytes(),
          hasLength(64),
        );
        expect(progress, isNotEmpty);
        expect(progress.last, 1.0);
      },
    );

    test('isAvailable reflects the configured flag', () async {
      expect(await FakeSourceSeparationService().isAvailable(), isTrue);
      expect(
        await FakeSourceSeparationService(available: false).isAvailable(),
        isFalse,
      );
    });

    test('still yields two files when the source is missing', () async {
      final result = await FakeSourceSeparationService().separate(
        '/no/such/file.wav',
      );
      expect(await File(result.instrumentalPath).exists(), isTrue);
      expect(await File(result.vocalPath).exists(), isTrue);
    });

    test('cancel() is a safe no-op on the fake', () {
      expect(FakeSourceSeparationService().cancel, returnsNormally);
    });
  });

  group('ProcessSourceSeparationService', () {
    late Directory root;

    setUp(() async {
      root = await Directory.systemTemp.createTemp('vocara_sep_proc_test');
    });

    tearDown(() async {
      try {
        await root.delete(recursive: true);
      } catch (_) {}
    });

    /// Writes an executable interpreter stub. It answers the `-c` engine
    /// probe with success; a real (sidecar) invocation logs a line to
    /// `starts` and its argv to `argv`, then runs [body].
    Future<String> writeStub(String body) async {
      final f = File(p.join(root.path, 'stub.sh'));
      await f.writeAsString(
        '#!/bin/bash\n'
        'if [ "\$1" = "-c" ]; then exit 0; fi\n'
        'dir="\$(dirname "\$0")"\n'
        'echo start >> "\$dir/starts"\n'
        'echo "\$@" >> "\$dir/argv"\n'
        '$body\n',
      );
      await Process.run('chmod', ['+x', f.path]);
      return f.path;
    }

    // Stub bodies: block until killed / write valid stems and succeed.
    // `exec` replaces bash so the kill signal reaches sleep directly.
    const sleepBody = 'exec sleep 300';
    // Models a torch sidecar in a long compute call: SIGTERM is ignored
    // (the disposition survives exec), only SIGKILL ends it. Records its PID
    // so tests can watch the process itself, not just the run's verdict.
    const stubbornBody =
        'trap "" TERM\n'
        'echo \$\$ >> "\$dir/pids"\n'
        'exec sleep 300';
    const successBody = '''
inst=""; voc=""
while [ \$# -gt 0 ]; do
  case "\$1" in
    --instrumental) inst="\$2"; shift 2;;
    --vocal) voc="\$2"; shift 2;;
    *) shift;;
  esac
done
head -c 100 /dev/zero > "\$inst"
head -c 100 /dev/zero > "\$voc"
exit 0''';

    ProcessSourceSeparationService buildService(String stubPath) =>
        ProcessSourceSeparationService(
          storage: _DirStorage(root),
          pythonExecutable: stubPath,
          modelName: 'test-model',
        );

    Future<String> writeSource() async {
      final f = File(p.join(root.path, 'song.wav'));
      await f.writeAsBytes(List<int>.filled(256, 3));
      return f.path;
    }

    /// How many sidecar (non-probe) invocations the stub has served.
    int starts() {
      final f = File(p.join(root.path, 'starts'));
      return f.existsSync() ? f.readAsLinesSync().length : 0;
    }

    List<String> tmpFilesInCache() {
      final dir = Directory(p.join(root.path, 'stems_cache'));
      if (!dir.existsSync()) return const [];
      return [
        for (final e in dir.listSync())
          if (e is File && e.path.endsWith('.tmp')) e.path,
      ];
    }

    Future<void> waitFor(bool Function() cond, String what) async {
      final deadline = DateTime.now().add(const Duration(seconds: 15));
      while (!cond()) {
        if (DateTime.now().isAfter(deadline)) fail('timed out waiting: $what');
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
    }

    /// Resolves with the run's outcome: the result on success, the error
    /// object otherwise (so tests can await either verdict without rethrow).
    Future<Object> outcome(Future<SeparationResult> run) =>
        run.then<Object>((r) => r, onError: (Object e) => e);

    test('a successful run commits stems and leaves no .tmp behind', () async {
      final stub = await writeStub(successBody);
      final svc = buildService(stub);
      final src = await writeSource();

      final result = await svc.separate(src);
      expect(result.instrumentalPath, endsWith('.instrumental.wav'));
      expect(result.vocalPath, endsWith('.vocal.wav'));
      expect(await File(result.instrumentalPath).length(), greaterThan(44));
      expect(await File(result.vocalPath).length(), greaterThan(44));
      expect(tmpFilesInCache(), isEmpty);

      // A repeat separate() of the same source is a cache hit: no new sidecar.
      final again = await svc.separate(src);
      expect(again.instrumentalPath, result.instrumentalPath);
      expect(starts(), 1);
    });

    test(
      'a second separate() cancels the draining old run; the old run reports '
      'cancelled and the new run stays registered and cancellable',
      () async {
        final stub = await writeStub(sleepBody);
        final svc = buildService(stub);
        final src = await writeSource();

        final sepA = outcome(svc.separate(src));
        await waitFor(() => starts() >= 1, 'run A sidecar start');

        // Run B enters while A's sidecar is alive (the pre-fix scenario:
        // the old VM died mid-run, a new screen starts a fresh separation).
        final sepB = outcome(svc.separate(src));

        // A dies cleanly reporting CANCELLED — with the shared flag it would
        // have misreported a FormatException (or hung forever un-killed).
        expect(await sepA, isA<SeparationCancelled>());

        // B starts only after A wound down, and it must still be registered:
        // with the unconditional `_active = null` in A's teardown, cancel()
        // here would be a no-op and sepB would hang forever.
        await waitFor(() => starts() >= 2, 'run B sidecar start');
        svc.cancel();
        expect(await sepB, isA<SeparationCancelled>());

        // Each run passed its OWN nonce'd .tmp paths to the sidecar (a shared
        // deterministic path would let A's cleanup delete B's live output).
        final argv = await File(p.join(root.path, 'argv')).readAsLines();
        expect(argv, hasLength(2));
        String instArg(String line) {
          final parts = line.split(' ');
          return parts[parts.indexOf('--instrumental') + 1];
        }

        expect(instArg(argv[0]), endsWith('.tmp'));
        expect(instArg(argv[1]), endsWith('.tmp'));
        expect(instArg(argv[0]), isNot(instArg(argv[1])));
        expect(tmpFilesInCache(), isEmpty);
      },
    );

    test('cancel() right after entry aborts before/at sidecar start', () async {
      final stub = await writeStub(sleepBody);
      final svc = buildService(stub);
      final src = await writeSource();

      final sep = outcome(svc.separate(src));
      // The run is registered synchronously at entry, so a cancel landing
      // during the hashing/cache phase must stick to THIS run's token.
      svc.cancel();
      expect(await sep, isA<SeparationCancelled>());
    });

    test('a cancel at ~100% (after the sidecar exited) still reports '
        'cancelled, not stems for the caller to import', () async {
      // `cancelled` was checked once, right after the sidecar exited; the
      // commit awaits after it (size checks, renames, cache cap) could take a
      // Cancel and the run still returned success. Cancelling on the final
      // progress tick lands the cancel after every one of those awaits.
      final stub = await writeStub(successBody);
      final svc = buildService(stub);
      final src = await writeSource();

      final sep = outcome(
        svc.separate(
          src,
          onProgress: (v) {
            if (v >= 1.0) svc.cancel();
          },
        ),
      );
      expect(await sep, isA<SeparationCancelled>());
      expect(tmpFilesInCache(), isEmpty);
    });

    test('the quit guard kills the in-flight sidecar', () async {
      final stub = await writeStub(sleepBody);
      final svc = buildService(stub);
      final src = await writeSource();

      final sep = outcome(svc.separate(src));
      await waitFor(() => starts() >= 1, 'sidecar start');

      // main.dart's quit paths call this; the run winds down as cancelled
      // (SIGKILL, no escalation wait) instead of orphaning the subprocess.
      SeparationQuitGuard.instance.killAll();
      expect(await sep, isA<SeparationCancelled>());
    });

    test(
      "the quit sweep reaches a superseded run's still-alive sidecar",
      () async {
        // separate() hands the registration to run B BEFORE waiting run A
        // out, so at quit the registered run has no process yet while A's
        // SIGTERM-ignoring sidecar is still alive — protected only by a 3 s
        // escalation timer that dies with the exiting app. The kill sweep
        // must reach EVERY live sidecar, not just the registered run's.
        final stub = await writeStub(stubbornBody);
        final svc = buildService(stub);
        final src = await writeSource();

        final pids = File(p.join(root.path, 'pids'));
        final sepA = outcome(svc.separate(src));
        await waitFor(
          () => pids.existsSync() && pids.readAsLinesSync().isNotEmpty,
          'run A sidecar start',
        );
        final pidA = int.parse(pids.readAsLinesSync().last);

        // Run B supersedes A: it takes the registration and SIGTERMs A —
        // which A ignores — then parks awaiting A's wind-down.
        final sepB = outcome(svc.separate(src));
        SeparationQuitGuard.instance.killAll();

        // A must die from the sweep's own SIGKILL — well before the 3 s
        // escalation timer (which the real quit never lives to run) fires.
        final deadline = DateTime.now().add(const Duration(seconds: 2));
        while (Process.runSync('kill', ['-0', '$pidA']).exitCode == 0) {
          if (DateTime.now().isAfter(deadline)) {
            // Clean up so a failed run doesn't leak the 5-minute sleeper.
            Process.runSync('kill', ['-9', '$pidA']);
            fail('superseded sidecar survived the quit kill sweep');
          }
          await Future<void>.delayed(const Duration(milliseconds: 20));
        }

        expect(await sepA, isA<SeparationCancelled>());
        expect(await sepB, isA<SeparationCancelled>());
        expect(starts(), 1, reason: 'no sidecar may be spawned while quitting');
      },
    );

    test('a quit during the hashing phase never spawns a sidecar', () async {
      // Regression: at quit there is no process left to run the
      // SIGTERM->SIGKILL escalation, so a sidecar spawned AFTER the kill
      // sweep (cancel landed while the run was still hashing/resolving)
      // could ignore a lone SIGTERM and orphan. The pre-Process.start
      // cancelled check must abort the run before anything is spawned.
      final stub = await writeStub(sleepBody);
      final svc = buildService(stub);
      final src = await writeSource();

      final sep = outcome(svc.separate(src));
      // The run is suspended in its hashing/cache phase; the quit sweep
      // arrives before any Process.start.
      SeparationQuitGuard.instance.killAll();
      expect(await sep, isA<SeparationCancelled>());
      expect(starts(), 0, reason: 'no sidecar may be spawned while quitting');
    });

    test('construction sweeps stale .tmp orphans, keeps fresh + wav', () async {
      final cache = Directory(p.join(root.path, 'stems_cache'));
      await cache.create(recursive: true);
      final stale = File(p.join(cache.path, 'aa.instrumental.wav.1.tmp'));
      final fresh = File(p.join(cache.path, 'bb.vocal.wav.2.tmp'));
      final wav = File(p.join(cache.path, 'cc.instrumental.wav'));
      await stale.writeAsBytes(List<int>.filled(10, 1));
      await fresh.writeAsBytes(List<int>.filled(10, 1));
      await wav.writeAsBytes(List<int>.filled(10, 1));
      await stale.setLastModified(
        DateTime.now().subtract(const Duration(hours: 2)),
      );

      final svc = buildService('/no/such/python');
      await svc.staleTmpSweepDone;

      expect(await stale.exists(), isFalse); // orphan of a dead run: swept
      expect(await fresh.exists(), isTrue); // could be a live run: kept
      expect(await wav.exists(), isTrue); // committed stems are never swept
    });
  });
}
