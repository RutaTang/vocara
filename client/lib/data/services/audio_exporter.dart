import 'dart:convert';
import 'dart:io';

import 'package:file_selector/file_selector.dart' show getSaveLocation;
import 'package:path/path.dart' as p;

import '../../utils/export_copy.dart';
import '../db/asset_store.dart';
import 'audio_file_service.dart';
import 'recording_finalizer.dart';

/// Copies one audio file out of the app to a place the user picks: the
/// guarantee that the user's audio is never trapped in the app's private
/// container. Shared by the song screen (takes, clips, backings) and the
/// technique page (examples), so both refuse and say the same things.
///
/// One export at a time per exporter; every outcome but a cancelled save
/// panel comes back as a message to show.
class AudioExporter {
  AudioExporter({
    required AudioFileService audioFiles,
    Future<String?> Function(String suggestedName)? pickSaveLocation,
    RecordingFinalizer? finalizer,
  }) : _audioFiles = audioFiles,
       _pickSaveLocation = pickSaveLocation ?? systemSaveLocation,
       _finalizer = finalizer ?? RecordingFinalizer.instance;

  final AudioFileService _audioFiles;

  /// Resolves where an export is written (the platform save panel by
  /// default); injected so tests can drive an export without the plugin.
  final Future<String?> Function(String suggestedName) _pickSaveLocation;
  final RecordingFinalizer _finalizer;

  /// The production picker: the macOS save panel via file_selector. Returns
  /// null when the user cancels.
  static Future<String?> systemSaveLocation(String suggestedName) async {
    final location = await getSaveLocation(suggestedName: suggestedName);
    return location?.path;
  }

  /// `"{title} - {item name}{source extension}"` (a practice and its take, a
  /// technique and its example), with path-hostile
  /// characters flattened so the suggestion is a valid file name. The source
  /// extension is kept (an imported mp3 exports as .mp3; recordings are .wav).
  /// A file name holds 255 bytes, and neither name is bounded: the suggestion
  /// the copy then failed on is cut to fit, the title first (the take's name
  /// says which file it is), and never the [suffix] after the take's name
  /// (cut with it, two clips of a take suggested one name).
  static String fileName(
    String title,
    String itemName,
    String sourcePath, {
    String suffix = '',
  }) {
    final ext = p.extension(sourcePath);
    String flat(String s) => s.replaceAll(RegExp(r'[/\\:]'), '-');
    final name = _fitBytes(flat(itemName), 120 - utf8.encode(suffix).length);
    final tail = ' - $name$suffix';
    final base = _fitBytes(flat(title), 200 - utf8.encode(tail).length) + tail;
    return '$base${ext.isEmpty ? '.wav' : ext}';
  }

  /// [s]'s longest leading run of whole characters within [maxBytes] of UTF-8.
  static String _fitBytes(String s, int maxBytes) {
    final out = StringBuffer();
    var bytes = 0;
    for (final rune in s.runes) {
      final c = String.fromCharCode(rune);
      bytes += utf8.encode(c).length;
      if (bytes > maxBytes) break;
      out.write(c);
    }
    return out.toString();
  }

  bool _exporting = false;

  /// Whether an export (its save panel or its copy) is under way.
  bool get exporting => _exporting;

  /// Copies [sourcePath] to a location the user picks, suggesting
  /// [suggestedName]. [noun] names what is exported in the messages ("take",
  /// "track", "example") and [medium] what it holds ("audio", or "video" for
  /// a video example). Returns the outcome to show, or null when the user
  /// cancelled the save panel.
  Future<String?> export(
    String sourcePath, {
    required String suggestedName,
    required String noun,
    String medium = 'audio',
  }) async {
    // One at a time: nothing showed a copy running (to a slow drive it runs
    // a while), and a second export to the same chosen name had two copies
    // writing one file, each then saying "Exported".
    if (_exporting) return 'Wait for the current export to finish.';
    // Bytes another device holds, not yet here: as edits of it say, not
    // "missing".
    if (AssetStore.assetIdOf(sourcePath) != null) {
      return "This $noun's $medium isn't on this device yet, so it can't be "
          'exported.';
    }
    // Claimed before the first await: set after one, a double-click while a
    // slow drive answered let both through, two panels and two copies.
    _exporting = true;
    try {
      final source = File(sourcePath);
      final missing = 'The $medium file for this $noun is missing.';
      if (sourcePath.isEmpty || !await source.exists()) return missing;
      return await _exportPicked(source, suggestedName, noun, medium, missing);
    } finally {
      _exporting = false;
    }
  }

  /// The save panel and the copy of [export].
  Future<String?> _exportPicked(
    File source,
    String suggestedName,
    String noun,
    String medium,
    String missing,
  ) async {
    final String? destPath;
    try {
      destPath = await _pickSaveLocation(suggestedName);
    } catch (_) {
      return 'Could not open the save dialog.';
    }
    if (destPath == null) return null; // user cancelled
    final dest = destPath;
    // Again, after the panel: it can stay open indefinitely, and the file can
    // go meanwhile (a sync removed it, a failed import's revert deleted its
    // copy) — the copy then failed as if the destination had.
    if (!await source.exists()) return missing;
    // Onto its own file (the save panel can browse into the app's storage
    // and confirm Replace): the copy truncated it to nothing, the audio
    // gone, and said "Exported".
    if (await sameFile(source.path, dest)) {
      return "That is the $noun's own $medium file. Export it somewhere else.";
    }
    // Into the app's storage: another take's audio, or the library itself.
    if (await _audioFiles.isAppStorage(dest)) {
      return "That folder holds Vocara's own files. Export somewhere else.";
    }
    try {
      // Overwrite is intentional: the save panel already confirmed replacing.
      // Held: a quit mid-copy exited under it and left the destination cut
      // short (an old file there already replaced).
      await _finalizer.hold(() => exportCopy(source, dest));
    } catch (_) {
      // Blamed on the destination only when the source is still there.
      return await source.exists()
          ? 'Could not export the $medium to that location.'
          : missing;
    }
    return 'Exported to $dest';
  }

  /// Whether [a] and [b] are one file (a symlink or hard link to it too).
  static Future<bool> sameFile(String a, String b) async {
    try {
      if (!await File(b).exists()) return false;
      // identical() judges a link itself, not what it points to.
      return await FileSystemEntity.identical(
        await File(a).resolveSymbolicLinks(),
        await File(b).resolveSymbolicLinks(),
      );
    } on FileSystemException {
      return false;
    }
  }
}
