import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../data/services/audio_file_service.dart';
import '../../../data/services/playback_engine.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/song_practice.dart';
import '../../../domain/models/take_version.dart';
import '../../../utils/busy_gate.dart';
import '../../../utils/id.dart';
import 'analysis_cache.dart';

/// Vocal-take versions ("interpretations") of a song practice: the list/active
/// getters and the four mutators — switch (pause → save → resume), create-and-
/// switch, rename, and delete-with-file-cleanup under the shared busy gate.
/// All state lives in the practice model (versions/activeVersionId), edited
/// through the host closures; the engine is driven directly so the playing
/// flag stays stream-driven.
class InterpretationsController extends ChangeNotifier {
  InterpretationsController({
    required PlaybackEngine engine,
    required AnalysisCache analysis,
    required BusyGate busyGate,
    required AudioFileService audioFiles,
    required SongPractice? Function() practice,
    required void Function(SongPractice) setPractice,
    required Future<bool> Function() save,
    required void Function() ensureSelection,
    required void Function(String) toast,
    required bool Function() isRecording,
    required bool Function() isSeparating,
  }) : _playback = engine,
       _analysis = analysis,
       _busyGate = busyGate,
       _audioFiles = audioFiles,
       _practiceOf = practice,
       _setPractice = setPractice,
       _save = save,
       _ensureSelection = ensureSelection,
       _toast = toast,
       _isRecording = isRecording,
       _isSeparating = isSeparating;

  final PlaybackEngine _playback;
  final AnalysisCache _analysis;
  final BusyGate _busyGate;
  final AudioFileService _audioFiles;
  final SongPractice? Function() _practiceOf;
  final void Function(SongPractice) _setPractice;
  final Future<bool> Function() _save;
  final void Function() _ensureSelection;
  final void Function(String) _toast;
  final bool Function() _isRecording;
  final bool Function() _isSeparating;

  /// Set in [dispose]; guards notifications racing the tear-down.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  /// All interpretations of this song, in creation order.
  List<TakeVersion> get all => _practiceOf()?.versions ?? const [];

  /// The interpretation currently shown and played.
  TakeVersion? get active => _practiceOf()?.activeVersion;

  /// Vocal takes belonging to the active interpretation — the only takes shown
  /// in the lane list and loaded into playback.
  List<AudioTrack> get vocalTakes =>
      _practiceOf()?.activeVocalTracks ?? const [];

  /// Switches which interpretation is shown/played; the other interpretations'
  /// takes stop playing. Because the loaded track set changes, this is a pause →
  /// reload → resume (playback continues from the same spot if it was playing).
  Future<void> setActive(String versionId) async {
    final practice = _practiceOf();
    if (practice == null || practice.activeVersionId == versionId) return;
    if (_isRecording()) {
      _toast('Stop recording to switch interpretation.');
      return;
    }
    if (!practice.versions.any((v) => v.id == versionId)) return;
    final wasPlaying = _playback.isPlaying;
    await _playback.pause();
    _setPractice(practice.copyWith(activeVersionId: versionId));
    _ensureSelection();
    await _save();
    if (_analysis.dockOpen && _analysis.dockShowsTake) {
      unawaited(_analysis.ensureSelectedSpectrogram());
    }
    if (wasPlaying) await _playback.play();
  }

  /// Creates a new (empty) interpretation and switches to it, ready to record a
  /// fresh emotional/technical reading of the song.
  Future<void> create([String name = '']) async {
    final practice = _practiceOf();
    if (practice == null) return;
    if (_isRecording()) {
      _toast('Stop recording to add an interpretation.');
      return;
    }
    final trimmed = name.trim();
    final version = TakeVersion(
      id: newId(),
      name: trimmed.isEmpty
          ? 'Interpretation ${practice.versions.length + 1}'
          : trimmed,
      createdAt: DateTime.now(),
    );
    await _playback.pause();
    _setPractice(
      practice.copyWith(
        versions: [...practice.versions, version],
        activeVersionId: version.id,
      ),
    );
    _ensureSelection();
    await _save();
  }

  Future<void> rename(String versionId, String name) async {
    final practice = _practiceOf();
    final trimmed = name.trim();
    if (practice == null || trimmed.isEmpty) return;
    _setPractice(
      practice.copyWith(
        versions: [
          for (final v in practice.versions)
            if (v.id == versionId) v.copyWith(name: trimmed) else v,
        ],
      ),
    );
    await _save();
  }

  /// Deletes an interpretation and every take in it (audio files included).
  /// Refuses to remove the last one — a practice always keeps one.
  Future<void> delete(String versionId) async {
    final practice = _practiceOf();
    if (practice == null) return;
    if (_isRecording()) {
      _toast('Stop recording to delete an interpretation.');
      return;
    }
    // No concurrent structural edit.
    if (_busyGate.busy || _isSeparating()) return;
    if (practice.versions.length <= 1) {
      _toast('Keep at least one interpretation.');
      return;
    }
    await _busyGate.run(() async {
      await _playback.pause();
      // Re-read the LIVE practice after the pause await so a take recorded into a
      // DIFFERENT interpretation meanwhile isn't dropped by a stale-snapshot
      // rebuild; re-check the guard too (it may have changed during the await).
      final cur = _practiceOf();
      if (cur == null || cur.versions.length <= 1) return;
      final doomed = cur.vocalTracks
          .where((t) => t.versionId == versionId)
          .toList();
      final remaining = cur.versions.where((v) => v.id != versionId).toList();
      if (remaining.isEmpty) return; // the target was the only one left
      _setPractice(
        cur.copyWith(
          versions: remaining,
          activeVersionId: cur.activeVersionId == versionId
              ? remaining.first.id
              : cur.activeVersionId,
          vocalTracks: cur.vocalTracks
              .where((t) => t.versionId != versionId)
              .toList(),
        ),
      );
      _ensureSelection();
      // Persist the dropped state BEFORE deleting files, and ONLY delete when the
      // save succeeded — a failed save reverts the model back to referencing
      // these takes, so their audio must survive (deleteAudio already refuses
      // pooled files; this gate also covers any not-yet-pooled working file).
      if (await _save()) {
        for (final t in doomed) {
          _analysis.dropLaneState(t.id);
          // Delete EVERY clip's file, not just the primary — a multi-clip take
          // would otherwise orphan its trailing clips' audio.
          for (final c in t.clips) {
            _analysis.evictAnalysis(c.audioFilePath);
            await _audioFiles.deleteAudio(c.audioFilePath);
          }
        }
      }
    });
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
