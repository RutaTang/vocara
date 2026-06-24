import 'package:flutter/foundation.dart';

import '../../../data/repositories/song_practice_repository.dart';
import '../../../domain/models/song_practice.dart';
import '../../../utils/command.dart';
import '../../../utils/result.dart';

/// ViewModel for the home screen: the collection of song practices.
class HomeViewModel extends ChangeNotifier {
  HomeViewModel({required SongPracticeRepository repository})
    : _repository = repository {
    load = Command0(_load)..execute();
    createProject = Command1(_createProject);
    deleteProject = Command1(_deleteProject);
  }

  final SongPracticeRepository _repository;

  /// Loads the practice list (runs once on creation).
  late final Command0<List<SongPractice>> load;

  /// Creates a new practice from a title; result carries the created practice.
  late final Command1<SongPractice, String> createProject;

  /// Deletes a practice by id.
  late final Command1<void, String> deleteProject;

  List<SongPractice> _projects = const [];
  List<SongPractice> get projects => _projects;

  Future<Result<List<SongPractice>>> _load() async {
    final result = await _repository.listPractices();
    if (result is Ok<List<SongPractice>>) {
      _projects = result.value;
      notifyListeners();
    }
    return result;
  }

  Future<Result<SongPractice>> _createProject(String title) async {
    final result = await _repository.createPractice(title);
    if (result is Ok<SongPractice>) {
      _projects = [result.value, ..._projects];
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
}
