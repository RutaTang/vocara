/// Formats a duration as `mm:ss` for transport/clock displays.
String formatClock(Duration duration) {
  final ms = duration.inMilliseconds < 0 ? 0 : duration.inMilliseconds;
  final totalSeconds = ms ~/ 1000;
  final minutes = totalSeconds ~/ 60;
  final seconds = totalSeconds % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return '${two(minutes)}:${two(seconds)}';
}

/// Formats a millisecond offset as `mm:ss`.
String formatClockMs(int milliseconds) =>
    formatClock(Duration(milliseconds: milliseconds));
