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
  /// the practices table. Skipped while any command runs — a skipped reload
  /// can never lose data (three-way saves merge against base hashes); the
  /// list simply refreshes on the next event or navigation.
  void _onRemoteApplied(RemoteApplied event) {
    if (event.practiceIds.isEmpty) return;
    if (load.running || createProject.running || deleteProject.running) return;
    load.execute();
  }

  Future<Result<List<PracticeSummary>>> _load() async {
    final result = await _repository.listPracticeSummaries();
    if (result is Ok<List<PracticeSummary>>) {
      _projects = result.value;
      notifyListeners();
    }
    return result;
  }

  Future<Result<SongPractice>> _createProject(String title) async {
    final result = await _repository.createPractice(title);
    if (result is Ok<SongPractice>) {
      _projects = [PracticeSummary.of(result.value), ..._projects];
      notifyListeners();
    }
    return result;
  }

  Future<Result<void>> _deleteProject(String id) async {
    final result = await _repository.deletePractice(id);
    if (result is Ok<void>) {
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
