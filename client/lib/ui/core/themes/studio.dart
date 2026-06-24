import 'package:flutter/widgets.dart';

/// Dark "studio" palette — charcoal panels with a warm coral accent, modeled
/// on a modern DAW. Tracks are neutral gray by default; the selected track and
/// primary actions use the coral accent.
abstract final class Studio {
  // Surfaces, darkest to lightest.
  static const background = Color(0xFF131316);
  static const surface = Color(0xFF1B1B21); // rails
  static const panel = Color(0xFF1A1A20); // track rows
  static const surfaceRaised = Color(0xFF24242C);
  static const inset = Color(0xFF0E0E12); // LCD, note lane
  static const hover = Color(0xFF22222A);

  // Lines.
  static const grid = Color(0xFF25252D);
  static const gridStrong = Color(0xFF34343E);
  static const border = Color(0xFF2A2A33);
  static const borderStrong = Color(0xFF3A3A45);

  // Text.
  static const textPrimary = Color(0xFFEAEAF0);
  static const textSecondary = Color(0xFF9595A2);
  static const textFaint = Color(0xFF5E5E6B);

  // Accent (coral) + status.
  static const accent = Color(0xFFE07A6A);
  static const accentBright = Color(0xFFEC8C7E);
  static const record = Color(0xFFE3604E);
  static const playhead = Color(
    0xFFF2EFE9,
  ); // warm white, reads on gray + coral
  static const lcd = Color(0xFFEFEFF3);

  // Waveform when the track is not selected.
  static const waveform = Color(0xFF6A6A77);
  static const toggleOff = Color(0xFF3A3A45);
}
