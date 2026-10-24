import 'package:flutter/foundation.dart';

import 'audio_fx.dart';
import 'audio_track.dart';
import 'fx_bus.dart';
import 'lyric_line.dart';
import 'take_version.dart';

/// A single singing-practice project — the unit shown as a card on the home
/// screen and opened in the multi-track detail view.
///
/// It holds one optional [accompaniment] track (the imported backing track,
/// rendered at the top of the detail view) and any number of [vocalTracks]
/// the user records or imports to compare their takes.
@immutable
class SongPractice {
  const SongPractice({
    required this.id,
    required this.title,
    required this.createdAt,
    required this.updatedAt,
    this.accompaniment,
    this.reference,
    this.vocalTracks = const [],
    this.versions = const [],
    this.activeVersionId,
    this.lyrics = const [],
    this.timelineDurationMs = 0,
    this.separationSource,
    this.reverb,
    this.masterVolume = 1.0,
    this.fxBuses = const [],
  });

  final String id;

  /// User-facing name of the practice (e.g. the song title).
  final String title;

  /// The imported backing track. Null until the user drags one in.
  final AudioTrack? accompaniment;

  /// The original recording of the song (with the original vocal), to A/B
  /// against the accompaniment. Null until the user drags one in.
  final AudioTrack? reference;

  /// Vocal takes across every interpretation, in display order. Each vocal
  /// take's [AudioTrack.versionId] says which [versions] entry it belongs to;
  /// the view shows only the active interpretation's takes.
  final List<AudioTrack> vocalTracks;

  /// The interpretations (emotional/technical readings) of this song. Always at
  /// least one. Vocal takes are grouped under these.
  final List<TakeVersion> versions;

  /// The interpretation currently shown/played. Null only for a not-yet-migrated
  /// legacy practice (resolved on load).
  final String? activeVersionId;

  /// Lyrics for the song, in order; each line optionally synced to a timeline
  /// position. Empty until the user pastes/imports lyrics.
  final List<LyricLine> lyrics;

  /// The total length of the shared timeline, independent of the content (set
  /// by the user, like a project length in Logic). 0 means "not yet set" — the
  /// view model defaults it to fit the content. The view maps time→x with this
  /// so moving clips never rescales the timeline. Never less than [contentEndMs].
  final int timelineDurationMs;

  final DateTime createdAt;
  final DateTime updatedAt;

  /// Sandbox path to the kept original full-mix song that the accompaniment +
  /// reference were separated from. Null when the backing wasn't produced by
  /// stem separation. Preserved across separations so it can be re-separated.
  final String? separationSource;

  /// The shared reverb bus settings for this practice's vocal takes. Null = the default
  /// (a medium room at 25% wet, which is silent until a take's reverb send is raised).
  final ReverbSettings? reverb;

  /// The master bus output gain (linear, 0..1, 1.0 = unity; clamped to 1.0 on
  /// load to match the master fader's range) — the overall level the whole mix
  /// passes through before the master soft-clip.
  final double masterVolume;

  /// DAW-style FX buses (group/aux tracks) vocal takes can route into. Project-
  /// scoped and shared across interpretations. Empty until the user adds one.
  final List<FxBus> fxBuses;

  FxBus? busById(String? id) {
    if (id == null) return null;
    for (final b in fxBuses) {
      if (b.id == id) return b;
    }
    return null;
  }

  bool get hasAccompaniment => accompaniment != null;
  bool get hasReference => reference != null;

  /// The active interpretation (falls back to the first if the id is stale).
  TakeVersion? get activeVersion {
    for (final v in versions) {
      if (v.id == activeVersionId) return v;
    }
    return versions.isEmpty ? null : versions.first;
  }

  /// Vocal takes belonging to the active interpretation (see [listsTake]).
  /// When no interpretation is resolved (legacy), all takes are returned.
  List<AudioTrack> get activeVocalTracks {
    if (activeVersion == null) return vocalTracks;
    return [
      for (final t in vocalTracks)
        if (listsTake(t)) t,
    ];
  }

  /// Whether any clip of any track still plays [path]. The asset pool is
  /// content-addressed, so two lanes over the same bytes (a duplicated take,
  /// one file imported twice) share one path: removing one of them must not
  /// discard what the other still shows (its analysis, its file).
  bool referencesAudio(String path) {
    for (final t in [?accompaniment, ?reference, ...vocalTracks]) {
      for (final c in t.clips) {
        if (c.audioFilePath == path) return true;
      }
    }
    return false;
  }

  /// Whether vocal take [t] is listed under the active interpretation: its
  /// own, or — when its interpretation resolves to none of [versions] — the
  /// active one, so it is never invisible. Sync can leave such a take: the
  /// integrity repair nulls a pointer at an interpretation another device
  /// deleted, and one still in the sync quarantine is not here yet. Only the
  /// listing falls back; the stored pointer is left exactly as it is, since
  /// rewriting it at load would be an edit nobody made (one that outranked
  /// other devices' changes and the parked interpretation's own).
  bool listsTake(AudioTrack t) {
    final active = activeVersion;
    if (active == null || t.versionId == active.id) return true;
    return !versions.any((v) => v.id == t.versionId);
  }

  /// One past the highest existing "Take N" / "Take N (recovered)" label among
  /// the active interpretation's takes, so a new take never reuses a number —
  /// not one freed by a delete, and not one held by a recovered take. THE one
  /// numbering implementation: normal take naming (the detail view model) and
  /// boot recovery (Persistence) both hand out numbers over the same set, so
  /// each side must count the other's names or the two paths produce
  /// duplicate take numbers.
  int get nextTakeNumber {
    final re = RegExp(r'^Take (\d+)(?: \(recovered\))?$');
    var max = 0;
    for (final t in activeVocalTracks) {
      final m = re.firstMatch(t.displayName);
      if (m != null) {
        // Names are user-editable and sync-delivered, so the digits can be
        // anything: 20 of them overflowed int.parse (every new take and every
        // boot recovery threw), and max-int wrapped max + 1 negative. A
        // number no real numbering reaches is skipped like a foreign label —
        // it can't collide, since handed-out numbers stay far below it.
        final n = int.tryParse(m.group(1)!);
        if (n != null && n < 1 << 31 && n > max) max = n;
      }
    }
    return max + 1;
  }

  /// The end of the latest-finishing lane (its last clip's end) — the minimum
  /// length the timeline must span so no audio is clipped off the end.
  int get contentEndMs {
    var max = accompaniment?.endMs ?? 0;
    if ((reference?.endMs ?? 0) > max) max = reference!.endMs;
    for (final track in vocalTracks) {
      if (track.endMs > max) max = track.endMs;
    }
    return max;
  }

  /// Total number of notes across every lane, in every interpretation — the
  /// home card semantics ("everything this practice holds"). The reference
  /// lane counts too: no local UI attaches notes to it today, but note
  /// updates route to it (see the detail view model's _updateTrack), so
  /// reference notes — e.g. arriving via sync — must never be silently
  /// uncounted.
  int get noteCount {
    var count = accompaniment?.notes.length ?? 0;
    count += reference?.notes.length ?? 0;
    for (final track in vocalTracks) {
      count += track.notes.length;
    }
    return count;
  }

  /// Notes across the accompaniment + reference + the active interpretation's
  /// takes. BOTH backings count here even though the workspace shows only one
  /// at a time: the Accompaniment ⇄ Original choice is transient view-state
  /// on the detail screen's mixer (MixerController.playReference), which this
  /// model deliberately cannot know — so this getter is "active
  /// interpretation", not "visible on screen". The count that matches what
  /// the user actually sees lives with that view-state
  /// (MixerController.visibleNoteCount); [noteCount] keeps the every-lane
  /// total for the home card.
  int get activeNoteCount {
    var count = accompaniment?.notes.length ?? 0;
    count += reference?.notes.length ?? 0;
    for (final t in activeVocalTracks) {
      count += t.notes.length;
    }
    return count;
  }

  SongPractice copyWith({
    String? title,
    AudioTrack? accompaniment,
    bool clearAccompaniment = false,
    AudioTrack? reference,
    bool clearReference = false,
    List<AudioTrack>? vocalTracks,
    List<TakeVersion>? versions,
    String? activeVersionId,
    List<LyricLine>? lyrics,
    DateTime? updatedAt,
    int? timelineDurationMs,
    String? separationSource,
    bool clearSeparationSource = false,
    ReverbSettings? reverb,
    double? masterVolume,
    List<FxBus>? fxBuses,
  }) {
    return SongPractice(
      id: id,
      title: title ?? this.title,
      accompaniment: clearAccompaniment
          ? null
          : (accompaniment ?? this.accompaniment),
      reference: clearReference ? null : (reference ?? this.reference),
      // Defensive copies: never retain a caller's list, so mutating it after
      // the call can't reach into this (otherwise immutable) value.
      vocalTracks: vocalTracks != null
          ? List.of(vocalTracks)
          : this.vocalTracks,
      versions: versions != null ? List.of(versions) : this.versions,
      activeVersionId: activeVersionId ?? this.activeVersionId,
      lyrics: lyrics != null ? List.of(lyrics) : this.lyrics,
      createdAt: createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      timelineDurationMs: timelineDurationMs ?? this.timelineDurationMs,
      separationSource: clearSeparationSource
          ? null
          : (separationSource ?? this.separationSource),
      reverb: reverb ?? this.reverb,
      masterVolume: masterVolume ?? this.masterVolume,
      fxBuses: fxBuses != null ? List.of(fxBuses) : this.fxBuses,
    );
  }

  factory SongPractice.fromJson(Map<String, dynamic> json) {
    final id = json['id'] as String;
    final createdAt = DateTime.parse(json['createdAt'] as String);
    var vocalTracks = (json['vocalTracks'] as List<dynamic>? ?? const [])
        .map((e) => AudioTrack.fromJson(e as Map<String, dynamic>))
        .toList();
    var versions = (json['versions'] as List<dynamic>? ?? const [])
        .map((e) => TakeVersion.fromJson(e as Map<String, dynamic>))
        .toList();
    var activeVersionId = json['activeVersionId'] as String?;

    // Migration: a legacy practice has no interpretations — wrap all existing
    // takes in a default "Interpretation 1" and make it active.
    if (versions.isEmpty) {
      final def = TakeVersion(
        id: '${id}__v1',
        name: 'Interpretation 1',
        createdAt: createdAt,
      );
      versions = [def];
      activeVersionId = def.id;
    } else if (activeVersionId == null ||
        !versions.any((v) => v.id == activeVersionId)) {
      activeVersionId = versions.first.id;
    }
    // Any take whose versionId doesn't resolve to an existing interpretation
    // (an untagged legacy take, or an id orphaned by partial/hand-edited JSON)
    // is reattached to the active interpretation so no take is ever hidden.
    final validVersionIds = {for (final v in versions) v.id};
    vocalTracks = [
      for (final t in vocalTracks)
        validVersionIds.contains(t.versionId)
            ? t
            : t.copyWith(versionId: activeVersionId),
    ];

    return SongPractice(
      id: id,
      title: json['title'] as String,
      accompaniment: json['accompaniment'] == null
          ? null
          : AudioTrack.fromJson(json['accompaniment'] as Map<String, dynamic>),
      reference: json['reference'] == null
          ? null
          : AudioTrack.fromJson(json['reference'] as Map<String, dynamic>),
      vocalTracks: vocalTracks,
      versions: versions,
      activeVersionId: activeVersionId,
      lyrics: (json['lyrics'] as List<dynamic>? ?? const [])
          .map((e) => LyricLine.fromJson(e as Map<String, dynamic>))
          .toList(),
      timelineDurationMs: json['timelineDurationMs'] as int? ?? 0,
      createdAt: createdAt,
      updatedAt: DateTime.parse(json['updatedAt'] as String),
      separationSource: json['separationSource'] as String?,
      reverb: json['reverb'] != null
          ? ReverbSettings.fromJson(json['reverb'] as Map<String, dynamic>)
          : null,
      // Clamp on load (like ReverbSettings): a synced / migrated / hand-edited
      // value outside the master fader's range must not reach the engine
      // verbatim (the master soft-clip runs after this gain).
      masterVolume: ((json['masterVolume'] as num?)?.toDouble() ?? 1.0).clamp(
        0.0,
        1.0,
      ),
      fxBuses: (json['fxBuses'] as List<dynamic>? ?? const [])
          .map((e) => FxBus.fromJson(e as Map<String, dynamic>))
          .toList(),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'title': title,
      'accompaniment': accompaniment?.toJson(),
      'reference': reference?.toJson(),
      'vocalTracks': vocalTracks.map((e) => e.toJson()).toList(),
      'versions': versions.map((e) => e.toJson()).toList(),
      'activeVersionId': activeVersionId,
      'lyrics': lyrics.map((e) => e.toJson()).toList(),
      'timelineDurationMs': timelineDurationMs,
      'createdAt': createdAt.toIso8601String(),
      'updatedAt': updatedAt.toIso8601String(),
      'separationSource': separationSource,
      'reverb': reverb?.toJson(),
      'masterVolume': masterVolume,
      'fxBuses': fxBuses.map((e) => e.toJson()).toList(),
    };
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is SongPractice &&
          other.id == id &&
          other.title == title &&
          other.accompaniment == accompaniment &&
          other.reference == reference &&
          listEquals(other.vocalTracks, vocalTracks) &&
          listEquals(other.versions, versions) &&
          other.activeVersionId == activeVersionId &&
          listEquals(other.lyrics, lyrics) &&
          other.timelineDurationMs == timelineDurationMs &&
          other.createdAt == createdAt &&
          other.updatedAt == updatedAt &&
          other.separationSource == separationSource &&
          other.reverb == reverb &&
          other.masterVolume == masterVolume &&
          listEquals(other.fxBuses, fxBuses);

  @override
  int get hashCode => Object.hash(
    id,
    title,
    accompaniment,
    reference,
    Object.hashAll(vocalTracks),
    Object.hashAll(versions),
    activeVersionId,
    Object.hashAll(lyrics),
    timelineDurationMs,
    createdAt,
    updatedAt,
    separationSource,
    reverb,
    masterVolume,
    Object.hashAll(fxBuses),
  );
}
