// The timeline scrollbar's thumb geometry. The thumb has a 28 px floor so it
// stays grabbable at deep zoom, which means it is wider than its share of the
// timeline — and its left edge can only travel the groove MINUS that width.
// Mapping the view start onto the whole groove instead ran the thumb off the
// end near the last page, where the groove clipped it to a sliver of the
// view's true share: at a 1 s window on a long song, ~2 px you could not grab.

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';

void main() {
  // A 10-minute song in a 600 px groove viewed through a 1-second window:
  // the view's true share is 1 px, so the thumb sits on its 28 px floor.
  const ext = 600000, span = 1000, groove = 600.0;

  test('at the last page the thumb ends at the groove end, not past it', () {
    final t = timelineScrollThumb(
      startMs: ext - span,
      spanMs: span,
      extentMs: ext,
      grooveW: groove,
    );
    expect(t.width, 28.0);
    expect(t.left + t.width, closeTo(groove, 1e-9));
  });

  test('at the first page the thumb starts at the groove start', () {
    final t = timelineScrollThumb(
      startMs: 0,
      spanMs: span,
      extentMs: ext,
      grooveW: groove,
    );
    expect(t.left, 0.0);
  });

  test('dragging the thumb across its whole travel pans the whole song', () {
    final t = timelineScrollThumb(
      startMs: 0,
      spanMs: span,
      extentMs: ext,
      grooveW: groove,
    );
    expect(t.msPerPx * (groove - t.width), closeTo(ext - span, 1e-6));
  });

  test('at fit-all the thumb fills the groove and does not drag', () {
    final t = timelineScrollThumb(
      startMs: 0,
      spanMs: ext,
      extentMs: ext,
      grooveW: groove,
    );
    expect(t.left, 0.0);
    expect(t.width, groove);
    expect(t.msPerPx, 0.0);
  });

  test('a song shorter than the minimum view span does not divide by the '
      'negative scroll range', () {
    // The view never narrows below 1 s, so a 400 ms song has span > extent.
    final t = timelineScrollThumb(
      startMs: 0,
      spanMs: 1000,
      extentMs: 400,
      grooveW: groove,
    );
    expect(t.left, 0.0);
    expect(t.width, groove);
    expect(t.msPerPx, 0.0);
  });
}
