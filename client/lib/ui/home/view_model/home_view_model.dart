import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../data/repositories/song_practice_repository.dart';
import '../../../data/sync/sync_engine.dart';
import '../../../domain/models/practice_summary.dart';
import '../../../domain/models/song_practice.dart';
import '../../../utils/command.dart';
import '../../../utils/result.dart';

/// ViewModel for the home screen: the collection of song practices.
class HomeViewModel extends ChangeNotifier {
  HomeViewModel({
    required SongPracticeRepository repository,
    Stream<RemoteApplied>? remoteApplied,
  }) : _repository = repository {
    load = Command0(_load)..execute();
    createProject = Command1(_createProject);
    deleteProject = Command1(_deleteProject);
    // Each command notifies as it finishes, its running flag already clear.
    for (final command in [load, createProject, deleteProject]) {
      command.addListener(_replayMissed);
    }
    _remoteSub = remoteApplied?.listen(_onRemoteApplied);
  }

  final SongPracticeRepository _repository;

  /// Set in [dispose]. Every command below notifies AFTER an await, so the
  /// route can be gone by then (leaving home while the first list read — or a
  /// remote-applied reload — is still in flight). Same guard every other view
  /// model in the app carries.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  /// Loads the practice list (runs once on creation).
  late final Command0<List<PracticeSummary>> load;

  /// Creates a new practice from a title; result carries the created practice.
  late final Command1<SongPractice, String> createProject;

  /// Deletes a practice by id.
  late final Command1<void, String> deleteProject;

  StreamSubscription<RemoteApplied>? _remoteSub;

  List<PracticeSummary> _projects = const [];
  List<PracticeSummary> get projects => _projects;

  /// Freshness-only reload when sync applied remote changes to any practice —
  /// its own row (a new/renamed/deleted practice) or a child row the cards
  /// count (a take, a note, a backing track), which ships without touching
  /// the practices table. Held while any command runs and loaded after it
  /// (see [_replayMissed]): dropped, a rename landing during a load kept its
  /// old title until some later reload. (The technique library's pattern.)
  void _onRemoteApplied(RemoteApplied event) {
    if (event.practiceIds.isEmpty) return;
    _missedRemote = true;
    _replayMissed();
  }

  /// Loads again on return from a practice, for what was changed there. A
  /// load already running read the list before those changes, so another is
  /// owed after it: re-run directly, the Command dropped it as re-entry.
  void refresh() {
    _missedRemote = true;
    _replayMissed();
  }

  /// A remote change, a return from a practice, or a list read overtaken by a
  /// create or delete, that a fresh load still owes the cards.
  bool _missedRemote = false;

  bool get _busy =>
      load.running || createProject.running || deleteProject.running;

  /// Loads again for what [_missedRemote] owes, once no command runs (each
  /// command asks again as it finishes).
  void _replayMissed() {
    if (_disposed || !_missedRemote || _busy) return;
    _missedRemote = false;
    load.execute();
  }

  /// Bumped by a create or delete changing the list: a load that read it
  /// before them would land over theirs (a deleted card back on the list).
  int _listEpoch = 0;

  /// Whether a read of the list has ever succeeded. Until one has, a create
  /// leaves the list alone: added to a list never read, the new card alone
  /// replaced the read error, and the rest of the library looked lost (the
  /// error shows only over an empty list). Not even while a read is still
  /// running, which may fail yet; the read the create owes brings the card.
  bool _listed = false;

  Future<Result<List<PracticeSummary>>> _load() async {
    final epoch = _listEpoch;
    final result = await _repository.listPracticeSummaries();
    if (epoch != _listEpoch) {
      // Read before a create or delete landed: owe a fresh one instead.
      _missedRemote = true;
    } else if (result is Ok<List<PracticeSummary>>) {
      _listed = true;
      _projects = result.value;
      notifyListeners();
    }
    return result;
  }

  Future<Result<SongPractice>> _createProject(String title) async {
    final result = await _repository.createPractice(title);
    if (result is Ok<SongPractice>) {
      _listEpoch++;
      if (_listed) {
        _projects = [PracticeSummary.of(result.value), ..._projects];
        notifyListeners();
      }
    }
    return result;
  }

  Future<Result<void>> _deleteProject(String id) async {
    final result = await _repository.deletePractice(id);
    if (result is Ok<void>) {
      _listEpoch++;
      _projects = _projects.where((p) => p.id != id).toList();
      notifyListeners();
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
