// The VOCAL TAKES header: its label, "+ New take" and the interpretation
// dropdown share one row over the centre pane. At the 960 px minimum window
// with both side panels open that pane is under 400 px wide, and the
// dropdown's name (up to 170 px of it) had no way to give: a user-named
// interpretation of a little over a dozen characters overflowed the row and
// clipped the dropdown's chevron off its right edge.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/ui/song_practice_detail/view_model/interpretations_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';

class _FakeVersions extends Fake implements InterpretationsController {
  _FakeVersions(this.name);

  final String name;

  @override
  TakeVersion? get active =>
      TakeVersion(id: 'v1', name: name, createdAt: DateTime(2026));
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  _FakeVm(String name) : fakeVersions = _FakeVersions(name);

  final _FakeVersions fakeVersions;

  @override
  InterpretationsController get versions => fakeVersions;
}

void main() {
  Future<void> pumpHeader(WidgetTester tester, String name) =>
      tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Align(
              alignment: Alignment.topLeft,
              // The centre pane at the minimum window, both panels open.
              child: SizedBox(
                width: 388,
                child: TakesHeader(vm: _FakeVm(name)),
              ),
            ),
          ),
        ),
      );

  testWidgets('a long interpretation name ellipsizes instead of overflowing', (
    tester,
  ) async {
    await pumpHeader(tester, 'Powerful belt, second verse');
    expect(tester.takeException(), isNull);
    expect(find.text('Powerful belt, second verse'), findsOneWidget);
  });

  testWidgets('the dropdown stays flush right', (tester) async {
    await pumpHeader(tester, 'Tender');
    final header = tester.getRect(find.byType(TakesHeader));
    final chevron = tester.getRect(find.byIcon(FIcons.chevronDown));
    expect(header.right - chevron.right, lessThan(40));
  });
}
