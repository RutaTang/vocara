// The transport bar: play / stop / loop / follow-cursor / monitor / REC, plus
// the take-and-note counts and the zoom control. It had no coverage at all.
//
// What is worth pinning is the GATING, because every gate here protects a
// state the engine cannot recover from on its own: play and stop are dead
// with nothing loaded, play is dead WHILE RECORDING (starting playback into
// a live take is how you get the backing bleeding into the capture), and the
// loop toggle is dead until a loop region exists. A gate that silently opens
// is not visible anywhere below this widget.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/playback_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/recording_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/detail_transport_bar.dart';

class _FakeMixer extends Fake implements MixerController {
  _FakeMixer(this.notes);
  final int notes;

  @override
  int get visibleNoteCount => notes;
}

class _FakePlayback extends Fake implements PlaybackController {
  _FakePlayback({this.playing = false, this.hasLoopRegion = true});

  final bool playing;
  final bool hasLoopRegion;

  int plays = 0, stops = 0, loopToggles = 0, followToggles = 0;
  final zooms = <double>[];

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  final Listenable viewport = ChangeNotifier();

  @override
  bool get isPlaying => playing;

  @override
  bool get loopEnabled => false;

  @override
  bool get hasLoop => hasLoopRegion;

  @override
  bool get followCursor => true;

  @override
  double get timelineZoom => 0.5;

  @override
  Future<void> togglePlay() async => plays++;

  @override
  Future<void> stop() async => stops++;

  @override
  void toggleLoop() => loopToggles++;

  @override
  void toggleFollowCursor() => followToggles++;

  @override
  Future<void> zoomTimeline(double factor) async => zooms.add(factor);

  @override
  void setTimelineZoomLive(double v) {}

  @override
  Future<void> commitTimelineZoom() async {}
}

class _FakeRecording extends Fake implements RecordingController {
  _FakeRecording({required this.arming});

  /// REC's arm window: tapped, but the mic not yet delivering.
  final bool arming;
  int starts = 0, monitorToggles = 0;

  @override
  bool get isStartingTake => arming;

  @override
  bool get inputMonitorEnabled => false;

  @override
  Future<void> toggleInputMonitor() async => monitorToggles++;

  @override
  Future<void> startRecording() async => starts++;
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  _FakeVm({
    required this.fakePlayback,
    required this.fakeMixer,
    this.recordingNow = false,
    this.busyNow = false,
    bool armingNow = false,
  }) : fakeRecording = _FakeRecording(arming: armingNow);

  final _FakePlayback fakePlayback;
  final _FakeMixer fakeMixer;
  final _FakeRecording fakeRecording;
  final bool recordingNow;
  final bool busyNow;
  int stopRecordings = 0;

  @override
  PlaybackController get playback => fakePlayback;

  @override
  MixerController get mixer => fakeMixer;

  @override
  RecordingController get recording => fakeRecording;

  @override
  ValueNotifier<Duration> get position => fakePlayback.position;

  @override
  bool get isRecording => recordingNow;

  @override
  bool get busy => busyNow;

  @override
  Future<void> stopRecording() async => stopRecordings++;
}

AudioTrack _take(String id) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: id,
  audioFilePath: '/$id.wav',
  durationMs: 4000,
  createdAt: DateTime(2026),
  versionId: 'v1',
);

SongPractice _practice({List<AudioTrack> takes = const []}) => SongPractice(
  id: 'p1',
  title: 'Hai Kuo',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  vocalTracks: takes,
  timelineDurationMs: 30000,
);

void main() {
  setUp(() {
    // A real window, not the 800x600 test default. The bar is a Stack: the
    // centre transport row paints last and therefore wins the hit test
    // wherever it overlaps the right-aligned zoom controls. At 800 px the
    // zoom-out icon's centre (x=645) falls inside the transport row (right
    // edge 654) and is unclickable; at the app's enforced 960 px minimum
    // window it clears by 71 px, so the overlap is not reachable in the
    // shipped app — but a test on the default surface would "prove" the zoom
    // control broken.
    final view =
        TestWidgetsFlutterBinding.instance.platformDispatcher.views.first;
    view.physicalSize = const Size(1440, 900);
    view.devicePixelRatio = 1.0;
    addTearDown(() {
      view.resetPhysicalSize();
      view.resetDevicePixelRatio();
    });
  });

  Future<_FakeVm> pump(
    WidgetTester tester, {
    SongPractice? practice,
    bool recording = false,
    bool arming = false,
    bool busy = false,
    bool hasLoop = true,
    bool playing = false,
    int notes = 0,
  }) async {
    final vm = _FakeVm(
      fakePlayback: _FakePlayback(playing: playing, hasLoopRegion: hasLoop),
      fakeMixer: _FakeMixer(notes),
      recordingNow: recording,
      busyNow: busy,
      armingNow: arming,
    );
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: DetailTransportBar(
            vm: vm,
            practice: practice ?? _practice(),
            onBack: () {},
          ),
        ),
      ),
    );
    return vm;
  }

  /// A round transport button is identified by its icon.
  Finder button(IconData icon) => find.ancestor(
    of: find.byIcon(icon),
    matching: find.byType(GestureDetector),
  );

  bool enabled(WidgetTester tester, IconData icon) =>
      tester.widget<GestureDetector>(button(icon).first).onTap != null;

  testWidgets('an empty practice has play and stop dead', (tester) async {
    await pump(tester);
    expect(enabled(tester, FIcons.play), isFalse);
    expect(enabled(tester, FIcons.square), isFalse);
    expect(
      enabled(tester, FIcons.locateFixed),
      isTrue,
      reason: 'follow-cursor is a view preference, not a transport action',
    );
  });

  testWidgets('a practice with a take can play and stop', (tester) async {
    final vm = await pump(tester, practice: _practice(takes: [_take('t1')]));
    expect(enabled(tester, FIcons.play), isTrue);

    await tester.tap(button(FIcons.play).first);
    await tester.tap(button(FIcons.square).first);
    await tester.pump();
    expect(vm.fakePlayback.plays, 1);
    expect(vm.fakePlayback.stops, 1);
  });

  testWidgets('play is dead WHILE RECORDING even with takes loaded', (
    tester,
  ) async {
    await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      recording: true,
    );
    expect(
      enabled(tester, FIcons.play),
      isFalse,
      reason: 'starting playback into a live take bleeds the backing into it',
    );
  });

  testWidgets('stop WHILE RECORDING ends the take, not the transport', (
    tester,
  ) async {
    // The transport stop paused and rewound the backing under a mic that
    // kept recording, misaligning everything sung after it.
    final vm = await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      recording: true,
    );
    await tester.tap(button(FIcons.square).first);
    await tester.pump();
    expect(vm.stopRecordings, 1);
    expect(vm.fakePlayback.stops, 0);
  });

  testWidgets('the loop toggle is dead until a loop region exists', (
    tester,
  ) async {
    await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      hasLoop: false,
    );
    expect(enabled(tester, FIcons.repeat), isFalse);

    final withRegion = await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
    );
    expect(enabled(tester, FIcons.repeat), isTrue);
    await tester.tap(button(FIcons.repeat).first);
    await tester.pump();
    expect(withRegion.fakePlayback.loopToggles, 1);
  });

  testWidgets('the play icon follows the transport state', (tester) async {
    await pump(tester, practice: _practice(takes: [_take('t1')]));
    expect(find.byIcon(FIcons.play), findsOneWidget);
    expect(find.byIcon(FIcons.pause), findsNothing);

    await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      playing: true,
    );
    expect(find.byIcon(FIcons.pause), findsOneWidget);
    expect(find.byIcon(FIcons.play), findsNothing);
  });

  testWidgets('REC starts a take, and stops the one in flight', (tester) async {
    final idle = await pump(tester, practice: _practice(takes: [_take('t1')]));
    await tester.tap(find.byType(GestureDetector).last);
    await tester.pump();

    final live = await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      recording: true,
    );
    await tester.tap(find.byType(GestureDetector).last);
    await tester.pump();

    expect(idle.fakeRecording.starts + live.stopRecordings, 2);
    expect(live.fakeRecording.starts, 0, reason: 'a live take never restarts');
  });

  testWidgets('stop and REC while the take is still arming cancel it', (
    tester,
  ) async {
    // isRecording flips only once the mic delivers (permission, take setup
    // and the ready handshake come first), so a Stop there only stopped the
    // transport and the take rolled moments later.
    final stop = await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      arming: true,
    );
    await tester.tap(button(FIcons.square).first);
    await tester.pump();
    expect(stop.stopRecordings, 1);
    expect(stop.fakePlayback.stops, 0);

    final rec = await pump(
      tester,
      practice: _practice(takes: [_take('t1')]),
      arming: true,
    );
    await tester.tap(find.byType(GestureDetector).last);
    await tester.pump();
    expect(rec.stopRecordings, 1);
    expect(rec.fakeRecording.starts, 0);
  });

  testWidgets('the zoom icons step the timeline out and in', (tester) async {
    final vm = await pump(tester, practice: _practice(takes: [_take('t1')]));
    await tester.tap(button(FIcons.zoomOut).first);
    await tester.tap(button(FIcons.zoomIn).first);
    await tester.pump();
    expect(vm.fakePlayback.zooms, [1.25, 0.8]);
  });

  testWidgets('the counts read from the mixer, singular at one take', (
    tester,
  ) async {
    await pump(tester, practice: _practice(takes: [_take('t1')]), notes: 3);
    expect(find.text('1 take · 3 notes'), findsOneWidget);

    await pump(
      tester,
      practice: _practice(takes: [_take('t1'), _take('t2')]),
      notes: 0,
    );
    expect(find.text('2 takes · 0 notes'), findsOneWidget);
  });

  testWidgets('busy shows the spinner', (tester) async {
    await pump(tester);
    expect(find.byType(CircularProgressIndicator), findsNothing);
    await pump(tester, busy: true);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
  });
}
