import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

/// Persists JSON documents as one file per record under the macOS app support
/// directory, and hands out per-collection directories for binary assets
/// (e.g. imported/recorded audio). Stateless apart from a cached root dir.
class LocalStorageService {
  Directory? _root;

  Future<Directory> _collectionDir(String collection) async {
    _root ??= await getApplicationSupportDirectory();
    final dir = Directory(p.join(_root!.path, collection));
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// The document, or null when there is none to keep: no file, or one that
  /// is not a JSON object. Throws when the file is there but could not be
  /// read: its values still exist, and a caller that took that as "no
  /// document" wrote its defaults over them.
  Future<Map<String, dynamic>?> read(String collection, String id) async {
    final dir = await _collectionDir(collection);
    final file = File(p.join(dir.path, '$id.json'));
    if (!await file.exists()) return null;
    try {
      final decoded = jsonDecode(await file.readAsString());
      return decoded is Map<String, dynamic> ? decoded : null;
    } on FileSystemException {
      rethrow;
    } catch (_) {
      // A truncated / corrupt / non-object file (partial write after a crash,
      // manual edit) must not crash startup — treat it as "no value", so the
      // caller falls back to defaults instead of throwing an unhandled error.
      return null;
    }
  }

  // Per-file write chains: serialize writes to the same document so a burst (e.g. a
  // settings slider firing a persist on every drag tick) can never interleave two
  // truncating writes into a torn, invalid-JSON file.
  final Map<String, Future<void>> _writeChains = {};
  int _writeSeq = 0;

  Future<void> write(String collection, String id, Map<String, dynamic> json) {
    final key = '$collection/$id';
    final prior = _writeChains[key];
    // Chain after any in-flight write to the same file, ignoring its outcome so one
    // failure doesn't poison the chain (this caller still observes its OWN write result).
    final gate = prior == null
        ? Future<void>.value()
        : prior.then((_) {}, onError: (_) {});
    final done = gate.then((_) => _writeAtomic(collection, id, json));
    _writeChains[key] = done;
    done.whenComplete(() {
      if (identical(_writeChains[key], done)) _writeChains.remove(key);
    }).ignore();
    return done;
  }

  /// Writes to a unique temp file then atomically renames over the target, so a reader
  /// (or a crash) never observes a half-written document.
  Future<void> _writeAtomic(
    String collection,
    String id,
    Map<String, dynamic> json,
  ) async {
    final dir = await _collectionDir(collection);
    final file = File(p.join(dir.path, '$id.json'));
    final tmp = File('${file.path}.${_writeSeq++}.tmp');
    try {
      await tmp.writeAsString(jsonEncode(json), flush: true);
      await tmp.rename(file.path);
    } catch (_) {
      try {
        if (await tmp.exists()) await tmp.delete();
      } catch (_) {}
      rethrow;
    }
  }

  /// Returns (creating if needed) a directory for storing binary assets that
  /// belong to [collection], e.g. `audio/`. Callers join their own filenames.
  Future<Directory> assetDir(String collection) => _collectionDir(collection);

  /// Where [collection]'s assets live, without creating the COLLECTION
  /// directory. Guard checks and delete paths must use this instead of
  /// [assetDir]: a delete must never mkdir the collection as a side effect,
  /// and resolving a path guard must still succeed while the storage tree
  /// is being torn down. Caveat, stated honestly: the FIRST call on a cold
  /// instance resolves the app-support ROOT via path_provider, which may
  /// create that root directory — everything after (and every warmed
  /// instance) is pure path math.
  Future<String> assetPath(String collection) async {
    _root ??= await getApplicationSupportDirectory();
    return p.join(_root!.path, collection);
  }
}
