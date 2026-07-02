import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../../data/services/playback_engine.dart';
import '../../../domain/models/song_practice.dart';

/// Transport, seek, loop/cycle region, the master-clock [position] notifier,
/// and timeline zoom / viewport pan / follow-cursor for the song detail
/// screen. Owns the engine's position/playing stream subscriptions; does NOT
/// own the engine itself (the view model creates, attaches and disposes it).
class PlaybackController extends ChangeNotifier {
  PlaybackController({
    required PlaybackEngine engine,
    required SongPractice? Function() practice,
    required bool Function() isRecording,
  }) : _engine = engine,
       _practice = practice,
       _isRecording = isRecording {
    _playingSub = _engine.playing.listen((playing) {
      _isPlaying = playing;
      notifyListeners();
    });
    _positionSub = _engine.position.listen((d) {
      // While recording, the playhead follows the recording clock instead
      // (the recording controller's level tick writes [position] directly).
      if (!_isRecording()) {
        position.value = d;
        _maybeFollowCursor(d.inMilliseconds);
      }
    });
  }

  final PlaybackEngine _engine;

  /// The live practice model (timeline/content extents for clamping).
  final SongPractice? Function() _practice;

  /// Whether a take is being recorded — the position stream must yield to the
  /// recording clock then, or the playhead fights the record tick.
  final bool Function() _isRecording;

  /// The engine, for sibling controllers' transport primitives (recording
  /// seeks/plays/pauses and disables the loop wrap through the SAME instance).
  /// Ownership (creation, settings attach, disposal) stays with the VM.
  PlaybackEngine get engine => _engine;

  StreamSubscription<Duration>? _positionSub;
  StreamSubscription<bool>? _playingSub;

  /// Master-clock playhead position. Drives the playhead overlay only.
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  bool _isPlaying = false;
  bool get isPlaying => _isPlaying;

  /// Set in [dispose]. Guards stream callbacks that can race the tear-down.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  // --- Transport -----------------------------------------------------------

  Future<void> togglePlay() async {
    if (_engine.isPlaying) {
      await _engine.pause();
    } else {
      await _engine.play();
    }
  }

  Future<void> seek(Duration to) => _engine.seek(to);

  /// Stops playback and returns the playhead to the start of the play range.
  Future<void> stop() async {
    await _engine.pause();
    await _engine.seek(Duration(milliseconds: _loopStartMs));
  }

  // --- Loop / cycle region ------------------------------------------------

  bool _loopEnabled = false;
  bool get loopEnabled => _loopEnabled;

  int _loopStartMs = 0;
  int _loopEndMs = 0;
  int get loopStartMs => _loopStartMs;
  int get loopEndMs => _loopEndMs;
  bool get hasLoop => _loopEndMs > _loopStartMs;

  /// Shortest allowed loop span — also the "too close to a clip edge" guard
  /// the view model's split action reuses.
  static const int minLoopMs = 150;

  // The timeline extent (the full arrangement, persisted) is the "fit all" view.
  // The zoom narrows the *visible* span — anchored at the start — down to a
  // 1-second window, so the grid can refine to one line per second. The view
  // span is transient (not persisted); it resets to fit-all on reopen.
  static const int _viewMinMs = 1000; // most zoomed in: a 1-second window
  int _viewSpanMs = 0; // 0 = fit all (the whole extent)

  int get _extentMs {
    final c = _practice()?.timelineDurationMs ?? 0;
    return c < _viewMinMs ? _viewMinMs : c;
  }

  /// The visible timeline span in ms — between a 1-second window and fit-all.
  int get viewSpanMs {
    final ext = _extentMs;
    final span = _viewSpanMs <= 0 ? ext : _viewSpanMs;
    return span.clamp(_viewMinMs, ext);
  }

  /// Current zoom as a slider value in [0,1]: 0 = fully zoomed out (fit all),
  /// 1 = fully zoomed in (1-second window).
  double get timelineZoom {
    final lo = _viewMinMs, hi = _extentMs;
    if (hi <= lo) return 0.0;
    return (1.0 - math.log(viewSpanMs / lo) / math.log(hi / lo)).clamp(
      0.0,
      1.0,
    );
  }

  /// Sets the zoom from the slider live (transient view state).
  void setTimelineZoomLive(double value) {
    final lo = _viewMinMs, hi = _extentMs;
    if (hi <= lo) return;
    final t = value.clamp(0.0, 1.0);
    final span = (lo * math.pow(hi / lo, 1 - t)).round().clamp(lo, hi);
    if (span == viewSpanMs) return;
    _viewSpanMs = span;
    _recenterViewOnPlayhead();
    notifyListeners();
  }

  Future<void> commitTimelineZoom() async {}

  /// Nudges the zoom by [factor] (>1 zooms out / widens, <1 zooms in), for the
  /// slider's end icons.
  Future<void> zoomTimeline(double factor) async {
    final span = (viewSpanMs * factor).round().clamp(_viewMinMs, _extentMs);
    if (span == viewSpanMs) return;
    _viewSpanMs = span;
    _recenterViewOnPlayhead();
    notifyListeners();
  }

  // --- Viewport pan / follow -----------------------------------------------

  int _viewStartMs = 0; // left edge of the visible window (ms)

  /// The left edge of the visible window, clamped so it never scrolls past the
  /// extent. 0 when fit-all.
  int get viewStartMs {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    return _viewStartMs.clamp(0, maxStart);
  }

  bool _followCursor = true;

  /// Whether the timeline auto-scrolls to keep the playhead in view (on by
  /// default; a manual scroll turns it off).
  bool get followCursor => _followCursor;
  void toggleFollowCursor() {
    _followCursor = !_followCursor;
    if (_followCursor) _recenterViewOnPlayhead();
    notifyListeners();
  }

  /// Pans the visible window by [deltaMs] (horizontal scroll / drag). Turns off
  /// follow so a manual scroll sticks.
  void panView(int deltaMs) {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    if (maxStart <= 0) return;
    final next = (viewStartMs + deltaMs).clamp(0, maxStart);
    if (next == _viewStartMs) {
      if (_followCursor) {
        _followCursor = false;
        notifyListeners();
      }
      return;
    }
    _followCursor = false;
    _viewStartMs = next;
    notifyListeners();
  }

  void _recenterViewOnPlayhead() {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    _viewStartMs = (position.value.inMilliseconds - viewSpanMs ~/ 2).clamp(
      0,
      maxStart,
    );
  }

  void _maybeFollowCursor(int playheadMs) {
    if (!_followCursor) return;
    final span = viewSpanMs;
    final maxStart = (_extentMs - span).clamp(0, _extentMs);
    if (maxStart <= 0) return;
    final start = viewStartMs;
    // Page-scroll: while the playhead is comfortably inside the window it moves
    // smoothly across a *static* view (no stutter); only when it nears the right
    // edge (or falls behind the view) do we jump forward by a page.
    if (playheadMs >= start && playheadMs < start + (span * 88 ~/ 100)) return;
    final next = (playheadMs - span ~/ 10).clamp(0, maxStart);
    if (next == _viewStartMs) return;
    _viewStartMs = next;
    notifyListeners();
  }

  /// Defaults the play region to the content length (the longest take) on first
  /// load, and keeps it within the (possibly longer) timeline afterwards. Also
  /// what re-arms the loop after recording disabled the engine's wrap.
  void ensureLoopRange() {
    final timeline = _practice()?.timelineDurationMs ?? 0;
    // Clamp the loop to real audio (content), not the padded timeline, and pull
    // it BACK when content shrinks — e.g. deleting the longest take must not
    // leave loopEnd cycling through a now-silent tail.
    final max = _loopMaxMs;
    if (_loopEndMs <= 0 || _loopEndMs > max) _loopEndMs = max;
    if (_loopStartMs < 0) _loopStartMs = 0;
    if (_loopStartMs > _loopEndMs - minLoopMs) {
      _loopStartMs = (_loopEndMs - minLoopMs).clamp(0, timeline);
    }
    _engine.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
  }

  /// When the loop is active, bring the playhead inside [loopStart, loopEnd).
  /// The native engine only WRAPS at loopEnd; it never snaps a playhead that sits
  /// BEFORE loopStart up into the window, so without this enabling a loop while
  /// the playhead is to its left plays a long out-of-region pass before wrapping.
  void _snapPlayheadIntoLoop() {
    if (!_loopEnabled || !hasLoop) return;
    final posMs = position.value.inMilliseconds;
    if (posMs < _loopStartMs || posMs >= _loopEndMs) {
      position.value = Duration(milliseconds: _loopStartMs);
      unawaited(_engine.seek(Duration(milliseconds: _loopStartMs)));
    }
  }

  /// The furthest a loop edge may go: the end of real audio, NOT the padded
  /// timeline — looping into the ~15% silent tail just plays dead air each cycle.
  int get _loopMaxMs {
    final content = _practice()?.contentEndMs ?? 0;
    return content > 0 ? content : (_practice()?.timelineDurationMs ?? 0);
  }

  /// Drags the left edge of the loop region (from the ruler handle).
  void setLoopStart(int ms) {
    // Guard the upper bound: for sub-minLoopMs content (a tiny clip) the naive
    // `_loopEndMs - minLoopMs` goes negative, and num.clamp asserts (debug) or
    // returns a negative upperLimit (release) when upper < lower.
    final hi = math.max(0, _loopEndMs - minLoopMs);
    _loopStartMs = ms.clamp(0, hi);
    _engine.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  /// Drags the right edge of the loop region.
  void setLoopEnd(int ms) {
    // Guard the lower bound symmetrically: for sub-minLoopMs content
    // `_loopStartMs + minLoopMs` can exceed _loopMaxMs (lower > upper).
    final lo = math.min(_loopStartMs + minLoopMs, _loopMaxMs);
    _loopEndMs = ms.clamp(lo, _loopMaxMs);
    _engine.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  void toggleLoop() {
    _loopEnabled = !_loopEnabled;
    _engine.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _positionSub?.cancel();
    _playingSub?.cancel();
    position.dispose();
    super.dispose();
  }
}
