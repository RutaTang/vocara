import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/diagnostics_log.dart';
import '../../../data/services/technique_media_service.dart';
import '../../../data/sync/sync_engine.dart';
import '../../../domain/models/technique.dart';
import '../../../utils/command.dart';
import '../../../utils/id.dart';
import '../../../utils/result.dart';

/// A tag plus how many techniques carry it — the facets shown as filter chips.
@immutable
class TagFacet {
  const TagFacet(this.tag, this.count);
  final String tag;
  final int count;
}

/// The sentinel tag used to filter techniques that have no tags at all.
const String kUntaggedFilter = '\u0000untagged';

/// ViewModel for the technique library home: a FLAT pool of techniques browsed
/// by a tag filter plus free-text search. There is no collection layer — tags
/// (including framework tags like "EVT"/"CVT") are the only organizing axis.
class TechniqueLibraryViewModel extends ChangeNotifier {
  TechniqueLibraryViewModel({
    required TechniqueRepository repository,
    required TechniqueMediaService mediaService,
    Stream<RemoteApplied>? remoteApplied,
  }) : _repository = repository,
       _mediaService = mediaService {
    load = Command0(_load)..execute();
    createTechnique = Command1(_createTechnique);
    deleteTechnique = Command1(_deleteTechnique);
    // Each command notifies as it finishes, its running flag already clear.
    for (final command in [load, createTechnique, deleteTechnique]) {
      command.addListener(_replayMissed);
    }
    _remoteSub = remoteApplied?.listen(_onRemoteApplied);
  }

  final TechniqueRepository _repository;
  final TechniqueMediaService _mediaService;

  StreamSubscription<RemoteApplied>? _remoteSub;

  /// Set in [dispose]. Guards the notifyListeners of a load/create/delete
  /// resolving after the route was popped (the standard sibling-VM pattern).
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  /// Freshness-only reload when sync applied remote changes to any technique:
  /// its own row, or only its example clips (which the cards count, and which
  /// ship without touching the techniques table). Skipped while any command
  /// runs — a skipped reload can never lose data (three-way saves merge
  /// against base hashes).
  void _onRemoteApplied(RemoteApplied event) {
    if (event.techniqueIds.isEmpty) return;
    _missedRemote = true;
    _replayMissed();
  }

  /// A remote change, or a list read overtaken by a create or delete, that a
  /// fresh load still owes the cards. Skipped and forgotten, a remote rename
  /// that landed during a load kept its old name until some later reload.
  bool _missedRemote = false;

  bool get _busy =>
      load.running || createTechnique.running || deleteTechnique.running;

  /// Loads again for what [_missedRemote] owes, once no command runs (each
  /// command asks again as it finishes).
  void _replayMissed() {
    if (_disposed || !_missedRemote || _busy) return;
    _missedRemote = false;
    load.execute();
  }

  /// Bumped by a create or delete changing the list: a load that read it
  /// before them would land over theirs (a deleted card back on the grid).
  int _listEpoch = 0;

  /// Loads all techniques (runs once on creation; re-run on return from detail).
  late final Command0<List<Technique>> load;

  /// Creates a named technique; result carries the created technique.
  late final Command1<Technique, String> createTechnique;

  /// Deletes a technique by id (also removes its media files).
  late final Command1<void, String> deleteTechnique;

  List<Technique> _all = const [];
  List<Technique> get all => _all;

  /// The active tag filter, or null for "All". May be [kUntaggedFilter].
  String? _activeTag;
  String? get activeTag => _activeTag;

  String _query = '';
  String get query => _query;

  void setTag(String? tag) {
    if (_activeTag == tag) return;
    _activeTag = tag;
    notifyListeners();
  }

  /// Whether [tag] is the active filter, compared case-insensitively (tags are
  /// folded by case, so the active value may differ in casing from a card's tag
  /// string).
  bool isTagActive(String tag) =>
      _activeTag != null &&
      _activeTag != kUntaggedFilter &&
      _activeTag!.toLowerCase() == tag.toLowerCase();

  void setQuery(String q) {
    if (_query == q) return;
    _query = q;
    notifyListeners();
  }

  /// All tags in the library with their counts, alphabetically. Used for the
  /// filter chips. Tags are folded case-insensitively (so "EVT" and "evt" are
  /// one chip), keeping the first-seen casing as the display label — consistent
  /// with how parseCommaList dedupes tags within a single technique.
  List<TagFacet> get tagFacets {
    final counts = <String, int>{}; // key: lowercased tag
    final display = <String, String>{}; // key: lowercased tag -> shown casing
    for (final t in _all) {
      final seen = <String>{};
      for (final tag in t.tags) {
        final key = tag.toLowerCase();
        if (!seen.add(key)) continue; // count each technique once per tag
        counts[key] = (counts[key] ?? 0) + 1;
        display.putIfAbsent(key, () => tag);
      }
    }
    final facets = [
      for (final e in counts.entries) TagFacet(display[e.key]!, e.value),
    ]..sort((a, b) => a.tag.toLowerCase().compareTo(b.tag.toLowerCase()));
    return facets;
  }

  /// How many techniques carry no tags (for the optional "Untagged" chip).
  int get untaggedCount => _all.where((t) => t.tags.isEmpty).length;

  /// Techniques matching the active tag filter and search query.
  List<Technique> get filtered {
    final q = _query.trim().toLowerCase();
    final activeLower = _activeTag?.toLowerCase();
    bool matchesTag(Technique t) {
      if (_activeTag == null) return true;
      if (_activeTag == kUntaggedFilter) return t.tags.isEmpty;
      return t.tags.any((tag) => tag.toLowerCase() == activeLower);
    }

    bool matchesQuery(Technique t) {
      if (q.isEmpty) return true;
      if (t.name.toLowerCase().contains(q)) return true;
      if (t.summary.toLowerCase().contains(q)) return true;
      for (final a in t.aliases) {
        if (a.toLowerCase().contains(q)) return true;
      }
      for (final tag in t.tags) {
        if (tag.toLowerCase().contains(q)) return true;
      }
      return false;
    }

    return [
      for (final t in _all)
        if (matchesTag(t) && matchesQuery(t)) t,
    ];
  }

  Future<Result<List<Technique>>> _load() async {
    final epoch = _listEpoch;
    final result = await _repository.listTechniques();
    if (epoch != _listEpoch) {
      // Read before a create or delete landed: owe a fresh one instead.
      _missedRemote = true;
    } else if (result is Ok<List<Technique>>) {
      _all = result.value;
      _dropStaleTagFilter();
      notifyListeners();
    }
    return result;
  }

  /// Drops an active filter that can no longer match after edits/deletes, so
  /// we never get stuck on an empty grid with the filter's chip gone. Covers
  /// both a tag that no longer exists (case-insensitive) and the "Untagged"
  /// filter once every technique has a tag. Both paths that can shrink [_all]
  /// run it — a delete prunes the list without a reload, so deleting the last
  /// technique a filter matches would otherwise strand it. (A create only
  /// adds, so it can never make a filter stale.)
  void _dropStaleTagFilter() {
    if (_activeTag == null) return;
    final activeLower = _activeTag!.toLowerCase();
    final stale = _activeTag == kUntaggedFilter
        ? _all.every((t) => t.tags.isNotEmpty)
        : !_all.any(
            (t) => t.tags.any((tag) => tag.toLowerCase() == activeLower),
          );
    if (stale) _activeTag = null;
  }

  Future<Result<Technique>> _createTechnique(String name) async {
    final now = DateTime.now();
    final trimmed = name.trim();
    final technique = Technique(
      id: newId(),
      name: trimmed.isEmpty ? 'New technique' : trimmed,
      createdAt: now,
      updatedAt: now,
    );
    final result = await _repository.saveTechnique(technique);
    if (result is Ok<Technique>) {
      _listEpoch++;
      _all = [result.value, ..._all];
      notifyListeners();
    }
    return result;
  }

  Future<Result<void>> _deleteTechnique(String id) async {
    final result = await _repository.deleteTechnique(id);
    if (result is Ok<void>) {
      _listEpoch++;
      _all = _all.where((t) => t.id != id).toList();
      _dropStaleTagFilter();
      notifyListeners();
      // Best effort: the row is deleted and the card gone, so a failed
      // cleanup must not report the delete as failed. What it leaves is an
      // orphan for the boot sweep.
      try {
        await _mediaService.deleteTechniqueMedia(id);
      } catch (e) {
        DiagnosticsLog.instance.log(
          'technique',
          'media cleanup after delete failed: ${e.runtimeType}',
        );
      }
    }
    return result;
  }

  @override
  void dispose() {
    _disposed = true;
    unawaited(_remoteSub?.cancel());
    super.dispose();
  }
}
