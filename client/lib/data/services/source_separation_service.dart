import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:path/path.dart' as p;

import 'local_storage_service.dart';

/// The two stems produced by separating a full song: the instrumental (to sing
/// over) and the isolated vocal (a reference to A/B against).
typedef SeparationResult = ({String instrumentalPath, String vocalPath});

/// The default separation model. MUST mirror `separate.py`'s DEFAULT_MODEL — it
/// only feeds the cache key (so a model change invalidates stems), since the
/// sidecar applies its own default when `--model` is omitted.
const String kDefaultSeparationModel =
    'mel_band_roformer_instrumental_becruily.ckpt';

/// Thrown by [SourceSeparationService.separate] when the user cancels — callers
/// treat this as a benign stop, not an error.
class SeparationCancelled implements Exception {
  const SeparationCancelled();
  @override
  String toString() => 'Separation cancelled';
}

/// Splits a full song into an instrumental + an isolated vocal stem.
///
/// This is an engine-agnostic abstraction: the UI/ViewModel never depend on
/// *how* separation happens. The prototype runs a Python sidecar as a
/// subprocess ([ProcessSourceSeparationService]); a shipped build swaps in a
/// bundled, self-contained engine behind the same interface so end users never
/// install anything. Tests use [FakeSourceSeparationService].
abstract class SourceSeparationService {
  /// Whether the engine can run on this machine (e.g. the interpreter exists).
  Future<bool> isAvailable();

  /// Separates [sourcePath] into two stems. [onProgress] reports best-effort
  /// progress in [0, 1]. Returns the two stem file paths (16-bit PCM WAV, which
  /// is what the analysis pipeline accepts). Throws on failure, or
  /// [SeparationCancelled] if [cancel] was called mid-run.
  Future<SeparationResult> separate(
    String sourcePath, {
    void Function(double progress)? onProgress,
  });

  /// Aborts an in-flight [separate] (best-effort); a no-op when idle.
  void cancel();
}

/// A no-op engine for tests/dev: it doesn't actually separate, it just copies
/// the source to both stem paths so the surrounding flow can be exercised.
class FakeSourceSeparationService implements SourceSeparationService {
  FakeSourceSeparationService({this.available = true});

  final bool available;

  @override
  Future<bool> isAvailable() async => available;

  @override
  void cancel() {
    /* the fake completes instantly; nothing to abort */
  }

  @override
  Future<SeparationResult> separate(
    String sourcePath, {
    void Function(double progress)? onProgress,
  }) async {
    onProgress?.call(0.2);
    final dir = await Directory.systemTemp.createTemp('vocara_sep_fake');
    final inst = p.join(dir.path, 'instrumental.wav');
    final voc = p.join(dir.path, 'vocal.wav');
    final src = File(sourcePath);
    if (await src.exists()) {
      await src.copy(inst);
      await src.copy(voc);
    } else {
      await File(inst).create();
      await File(voc).create();
    }
    onProgress?.call(1.0);
    return (instrumentalPath: inst, vocalPath: voc);
  }
}

/// Runs the Python separation sidecar (`assets/separation/separate.py`) as a
/// subprocess. Results are cached on disk keyed by a content hash of the source
/// song, so re-separating the same file is instant.
class ProcessSourceSeparationService implements SourceSeparationService {
  ProcessSourceSeparationService({
    required LocalStorageService storage,
    this.pythonExecutable = 'python3',
    this.modelName,
  }) : _storage = storage;

  final LocalStorageService _storage;

  /// The interpreter that runs the sidecar. In a shipped build this is swapped
  /// for a bundled, self-contained binary (users never install anything).
  final String pythonExecutable;

  /// Optional override for the separation model the sidecar loads.
  final String? modelName;

  // The in-flight sidecar process and a flag distinguishing a user cancel from
  // a real failure. Only one separation runs at a time (the VM guards re-entry).
  Process? _active;
  bool _cancelled = false;

  // The resolved interpreter that actually has audio-separator, discovered once
  // and cached. A macOS app gets a minimal PATH (no conda/homebrew), so bare
  // `python3` resolves to the system Python that lacks the engine — we probe
  // common locations and pick the first that has it.
  String? _resolvedPython;
  bool _resolvedOnce = false;

  List<String> get _pythonCandidates {
    final home = Platform.environment['HOME'];
    return <String>[
      pythonExecutable, // explicit override / --dart-define / 'python3'
      'python3',
      '/opt/miniconda3/bin/python3',
      '/opt/anaconda3/bin/python3',
      '/opt/homebrew/bin/python3',
      '/usr/local/bin/python3',
      if (home != null) '$home/miniconda3/bin/python3',
      if (home != null) '$home/anaconda3/bin/python3',
    ];
  }

  Future<bool> _hasEngine(String python) async {
    try {
      // find_spec verifies audio-separator is importable without the heavy
      // `import audio_separator` (which would pull in torch).
      final r = await Process.run(python, const [
        '-c',
        'import importlib.util,sys;'
            'sys.exit(0 if importlib.util.find_spec("audio_separator") else 1)',
      ]);
      return r.exitCode == 0;
    } catch (_) {
      return false; // interpreter not found / not runnable
    }
  }

  /// Finds (once, cached) a Python interpreter that has audio-separator.
  Future<String?> _resolvePython() async {
    if (_resolvedOnce) return _resolvedPython;
    final tried = <String>{};
    for (final candidate in _pythonCandidates) {
      if (candidate.isEmpty || !tried.add(candidate)) continue;
      if (await _hasEngine(candidate)) {
        _resolvedPython = candidate;
        break;
      }
    }
    _resolvedOnce = true;
    return _resolvedPython;
  }

  @override
  void cancel() {
    _cancelled = true;
    final proc = _active;
    if (proc == null) return;
    proc.kill(); // SIGTERM
    // A native torch/ML process may ignore SIGTERM while in a long compute
    // call, which would hang the teardown (and the overlay). Escalate to
    // SIGKILL if it hasn't exited shortly — SIGKILL can't be ignored, so stdout
    // closes and exitCode resolves.
    Future.delayed(const Duration(seconds: 3), () {
      if (_cancelled && identical(_active, proc)) {
        proc.kill(ProcessSignal.sigkill);
      }
    });
  }

  @override
  Future<bool> isAvailable() async => (await _resolvePython()) != null;

  /// The effective model identity: explicit override wins, else the env var,
  /// else mirrors the sidecar's own default. Folded into the cache key so a
  /// model change invalidates prior stems.
  String _effectiveModel() =>
      modelName ??
      Platform.environment['VOCARA_SEP_MODEL'] ??
      kDefaultSeparationModel;

  @override
  Future<SeparationResult> separate(
    String sourcePath, {
    void Function(double progress)? onProgress,
  }) async {
    // Clear the cancel flag at entry (before any await) so a cancel arriving
    // during the hashing/cache phase below is preserved, not clobbered.
    _cancelled = false;
    onProgress?.call(0.0);
    final cacheDir = await _storage.assetDir('stems_cache');
    final model = _effectiveModel();
    // Key by source content AND model, so switching models doesn't return
    // stale stems for the same song.
    final modelTag = md5.convert(utf8.encode(model)).toString().substring(0, 8);
    final key = '${await _hashFile(sourcePath)}.$modelTag';
    final inst = File(p.join(cacheDir.path, '$key.instrumental.wav'));
    final voc = File(p.join(cacheDir.path, '$key.vocal.wav'));

    // Cache hit — separation is expensive, so reuse prior stems verbatim (but
    // still honor a cancel that landed during hashing).
    if (await inst.exists() && await voc.exists()) {
      if (_cancelled) throw const SeparationCancelled();
      onProgress?.call(1.0);
      return (instrumentalPath: inst.path, vocalPath: voc.path);
    }

    final python = await _resolvePython();
    if (python == null) {
      throw const FormatException(
        'Separation engine not found. The audio-separator tool is not installed in '
        'any known Python.',
      );
    }

    // Write to temp paths and atomically rename in only on success, so the
    // cache never holds a partial/old pair from a failed run.
    final instTmp = File('${inst.path}.tmp');
    final vocTmp = File('${voc.path}.tmp');
    final script = await _ensureScript();
    final args = <String>[
      script,
      '--input',
      sourcePath,
      '--instrumental',
      instTmp.path,
      '--vocal',
      vocTmp.path,
      '--model',
      model,
    ];

    final Process proc;
    try {
      proc = await Process.start(
        python,
        args,
        environment: _augmentedEnv(python),
      );
    } catch (_) {
      throw const FormatException(
        'Could not start the separation engine. Is Python installed?',
      );
    }
    _active = proc;
    // A cancel that arrived during startup (now preserved, since the reset is at
    // function entry) still aborts the just-started process.
    if (_cancelled) proc.kill();

    // Set once the stems are renamed into the cache; until then the .tmp files are
    // orphans the finally must clean up on ANY early/error exit.
    var renamedIn = false;
    try {
      // The sidecar's coarse stdout markers bracket the phases; during the long
      // inference phase, audio-separator prints a tqdm bar to stderr ("45%|…"),
      // which we map into the 0.15–0.85 band so the bar actually moves instead
      // of sticking at 15%. We only trust tqdm once inference has started (so a
      // first-run model-download bar doesn't drive it backwards).
      final errBuf = StringBuffer();
      final tqdm = RegExp(r'(\d{1,3})%\|');
      var inInference = false;
      final stderrDone = proc.stderr.transform(utf8.decoder).forEach((chunk) {
        errBuf.write(chunk);
        if (!inInference) return;
        final matches = tqdm.allMatches(chunk).toList();
        if (matches.isEmpty) return;
        final pct = int.tryParse(matches.last.group(1)!);
        if (pct != null) {
          onProgress?.call(
            (0.15 + pct.clamp(0, 100) / 100 * 0.70).clamp(0.0, 1.0),
          );
        }
      });
      await for (final line
          in proc.stdout
              .transform(utf8.decoder)
              .transform(const LineSplitter())) {
        final m = RegExp(r'^PROGRESS\s+([0-9.]+)').firstMatch(line.trim());
        if (m != null) {
          final v = double.tryParse(m.group(1)!);
          if (v != null) {
            inInference = v >= 0.15 && v < 0.85;
            onProgress?.call(v.clamp(0.0, 1.0));
          }
        }
      }

      final code = await proc.exitCode;
      await stderrDone; // ensure stderr is fully consumed before reading errBuf
      final detail = errBuf.toString().trim();

      if (_cancelled) {
        await _deleteQuietly(instTmp);
        await _deleteQuietly(vocTmp);
        throw const SeparationCancelled();
      }
      // Existence is not enough: an empty/edge-case model output re-encodes to a
      // structurally-valid 0-frame WAV (44-byte header, exit 0), which would be
      // committed and imported as a silent zero-duration take. Require real audio
      // frames (a canonical PCM header is 44 bytes, so > 44 means it has data).
      final instOk = await instTmp.exists() && await instTmp.length() > 44;
      final vocOk = await vocTmp.exists() && await vocTmp.length() > 44;
      if (code != 0 || !instOk || !vocOk) {
        await _deleteQuietly(instTmp);
        await _deleteQuietly(vocTmp);
        // Surface the first non-empty stderr line (our sidecar's cause line).
        final cause = detail
            .split('\n')
            .map((l) => l.trim())
            .firstWhere((l) => l.isNotEmpty, orElse: () => '');
        throw FormatException(
          cause.isNotEmpty
              ? 'Separation failed: $cause'
              : code != 0
              ? 'Separation failed (exit $code).'
              : 'Separation produced empty stems.',
        );
      }
      await instTmp.rename(inst.path);
      await vocTmp.rename(voc.path);
      renamedIn = true;
      // Keep the (deliberately persistent) stems cache bounded — each pair is
      // tens of MB and nothing else reclaims it; the kept instrumental already
      // lives content-addressed in the asset pool.
      await _capStemsCache(cacheDir, _stemsCacheMaxBytes, keep: key);
      onProgress?.call(1.0);
      return (instrumentalPath: inst.path, vocalPath: voc.path);
    } finally {
      _active = null;
      // Clean up orphan .tmp files on any path that didn't rename them in — an
      // unexpected stream/pipe/decode error (the await-for / exitCode / stderrDone
      // awaits), or the cancel/failure branches. The stems-cache GC only reclaims
      // .wav files, so a stray .tmp would accumulate unbounded across failed runs.
      // _deleteQuietly never throws, so it can't mask the original exception.
      if (!renamedIn) {
        await _deleteQuietly(instTmp);
        await _deleteQuietly(vocTmp);
      }
    }
  }

  /// Upper bound on the persistent stems cache; oldest pairs are evicted first.
  static const _stemsCacheMaxBytes = 3 * 1024 * 1024 * 1024; // 3 GB

  /// Evicts the oldest stem PAIRS until the cache directory is under [maxBytes].
  /// Best-effort: never throws. [keep] is the cache key of the run that just wrote
  /// (its pair is the return value), which is NEVER evicted even if that single
  /// pair exceeds [maxBytes] — otherwise the loop would delete the files we're
  /// about to return (a >3GB pair from a very long source), leaving dead paths.
  ///
  /// Eviction is by cache key (the `<hash>.<model>` prefix shared by a song's instrumental
  /// + vocal stems), not by individual file: a cache hit needs BOTH files, so deleting only
  /// one would leave a useless orphan that still counts toward the budget and forces a
  /// needless re-separation. Any pre-existing orphan (its own group) is reclaimed too.
  Future<void> _capStemsCache(
    Directory dir,
    int maxBytes, {
    String? keep,
  }) async {
    try {
      final groups = <String, List<File>>{};
      final groupBytes = <String, int>{};
      final groupMtime = <String, DateTime>{};
      var total = 0;
      await for (final e in dir.list()) {
        if (e is! File || !e.path.endsWith('.wav')) continue;
        final name = p.basename(e.path);
        final key = name.endsWith('.instrumental.wav')
            ? name.substring(0, name.length - '.instrumental.wav'.length)
            : name.endsWith('.vocal.wav')
            ? name.substring(0, name.length - '.vocal.wav'.length)
            : name; // an unexpected stray file is its own group
        final len = await e.length();
        final mtime = (await e.stat()).modified;
        groups.putIfAbsent(key, () => []).add(e);
        groupBytes[key] = (groupBytes[key] ?? 0) + len;
        total += len;
        // A pair's age is its most-recently-modified file.
        final cur = groupMtime[key];
        if (cur == null || mtime.isAfter(cur)) groupMtime[key] = mtime;
      }
      if (total <= maxBytes) return;
      final keys = groups.keys.toList()
        ..sort((a, b) => groupMtime[a]!.compareTo(groupMtime[b]!));
      for (final key in keys) {
        if (total <= maxBytes) break;
        // Never evict the run that just wrote (its pair is the return value); if it
        // alone exceeds the cap the cache transiently stays over maxBytes — far
        // better than returning paths to files we just deleted. The next normal-
        // sized separation resumes bounding the cache.
        if (key == keep) continue;
        for (final f in groups[key]!) {
          try {
            await f.delete();
          } catch (_) {}
        }
        total -= groupBytes[key]!;
      }
    } catch (_) {}
  }

  Future<void> _deleteQuietly(File f) async {
    try {
      if (await f.exists()) await f.delete();
    } catch (_) {
      /* best-effort cleanup */
    }
  }

  /// PATH augmented with common tool locations. A macOS app gets a minimal PATH
  /// (no homebrew/conda), and audio-separator shells out to ffmpeg to write the
  /// stems — so without this the separation fails near the very end with an
  /// "ffmpeg not found" error.
  Map<String, String> _augmentedEnv(String python) {
    final extra = <String>[
      p.dirname(python), // the chosen interpreter's bin (e.g. conda)
      '/opt/homebrew/bin',
      '/usr/local/bin',
      '/opt/miniconda3/bin',
    ];
    final current = Platform.environment['PATH'] ?? '/usr/bin:/bin';
    return {
      'PATH': [...extra, current].join(':'),
    };
  }

  /// Copies the bundled sidecar out of the asset bundle to a stable path the
  /// interpreter can run, rewriting only when the script changes.
  Future<String> _ensureScript() async {
    final dir = await _storage.assetDir('separation');
    final dest = File(p.join(dir.path, 'separate.py'));
    final src = await rootBundle.loadString('assets/separation/separate.py');
    if (!await dest.exists() || await dest.readAsString() != src) {
      await dest.writeAsString(src);
    }
    return dest.path;
  }

  Future<String> _hashFile(String path) async {
    final digest = await sha256.bind(File(path).openRead()).first;
    return digest.toString();
  }
}
