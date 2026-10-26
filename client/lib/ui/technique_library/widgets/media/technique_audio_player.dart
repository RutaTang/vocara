import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:media_kit/media_kit.dart';

import '../../../../data/db/asset_store.dart';
import '../../../core/themes/studio.dart';

/// Builds the media_kit [Player] behind every example clip's inline player
/// (audio and video). Production never reassigns it: it is the seam a widget
/// test, which cannot initialize media_kit, swaps a fake through to observe
/// what the players are asked to do (and whether each one is disposed).
Player Function() createExamplePlayer = Player.new;

/// A compact inline audio player: a play/pause button, a scrubbable progress
/// bar, and a time readout. Backed by a media_kit [Player] it owns and disposes.
///
/// The file is opened paused on mount so the duration shows immediately (without
/// having to press play first).
class TechniqueAudioPlayer extends StatefulWidget {
  const TechniqueAudioPlayer({
    required this.path,
    this.onPosition,
    this.seekSignal,
    this.pauseSignal,
    this.playBlocked = false,
    super.key,
  });

  /// Absolute sandbox path to the audio file.
  final String path;

  /// Called as playback progresses (and on a manual scrub), so the analysis
  /// playhead can follow this clip. Null when this clip doesn't drive analysis.
  final ValueChanged<Duration>? onPosition;

  /// When the analysis below is scrubbed, its new time is pushed here so this
  /// player seeks to match (bidirectional sync). Null when this clip doesn't
  /// drive analysis.
  final ValueListenable<Duration>? seekSignal;

  /// Fires when playback must stop (the mic armed for a take): the player
  /// pauses where it is.
  final Listenable? pauseSignal;

  /// While a take records: Play is off, or an example started then played
  /// on speakers into the very take compared against it ([pauseSignal]
  /// only pauses at the arm).
  final bool playBlocked;

  @override
  State<TechniqueAudioPlayer> createState() => _TechniqueAudioPlayerState();
}

class _TechniqueAudioPlayerState extends State<TechniqueAudioPlayer> {
  // Null when media_kit is unavailable (e.g. in widget tests where it isn't
  // initialized) — the control then degrades to a static 0:00 readout.
  Player? _player;
  final _subs = <StreamSubscription<dynamic>>[];

  bool _playing = false;
  Duration _position = Duration.zero;
  Duration _duration = Duration.zero;
  bool _dragging = false;
  double _dragValue = 0;

  /// True when the clip's bytes are not on this device (the repository's
  /// `asset://` placeholder for a missing pooled file): nothing can play it,
  /// and media_kit would mistake the scheme for a bundled Flutter asset.
  late final bool _unavailable = AssetStore.assetIdOf(widget.path) != null;

  @override
  void initState() {
    super.initState();
    if (_unavailable) return;
    Player? built;
    try {
      // Media first: its constructor resolves the URI and can throw, and a
      // player built before it was leaked — the catch dropped the only
      // reference, so nothing ever disposed it.
      final media = Media(widget.path);
      final player = built = createExamplePlayer();
      _player = player;
      _subs.add(
        player.stream.playing.listen((v) {
          if (mounted) setState(() => _playing = v);
        }),
      );
      _subs.add(
        player.stream.position.listen((v) {
          if (mounted && !_dragging) {
            setState(() => _position = v);
            // Only drive the analysis playhead while actually playing — the
            // initial paused 0 (and seeks issued FROM the analysis) must not
            // stomp the playhead or loop back.
            if (_playing) widget.onPosition?.call(v);
          }
        }),
      );
      _subs.add(
        player.stream.duration.listen((v) {
          if (mounted) setState(() => _duration = v);
        }),
      );
      // Open paused so the duration loads up front and a scrub can seek before
      // the first play.
      player.open(media, play: false);
    } catch (_) {
      for (final s in _subs) {
        s.cancel();
      }
      _subs.clear();
      built?.dispose();
      _player = null;
    }
    widget.seekSignal?.addListener(_onSeekSignal);
    widget.pauseSignal?.addListener(_onPauseSignal);
  }

  @override
  void didUpdateWidget(TechniqueAudioPlayer old) {
    super.didUpdateWidget(old);
    if (!identical(old.seekSignal, widget.seekSignal)) {
      old.seekSignal?.removeListener(_onSeekSignal);
      widget.seekSignal?.addListener(_onSeekSignal);
    }
    if (!identical(old.pauseSignal, widget.pauseSignal)) {
      old.pauseSignal?.removeListener(_onPauseSignal);
      widget.pauseSignal?.addListener(_onPauseSignal);
    }
  }

  void _onPauseSignal() => _player?.pause();

  /// The analysis was scrubbed → move playback to match.
  void _onSeekSignal() {
    if (!mounted) return;
    final to = widget.seekSignal?.value;
    if (to == null) return;
    setState(() => _position = to);
    _player?.seek(to);
  }

  @override
  void dispose() {
    widget.seekSignal?.removeListener(_onSeekSignal);
    widget.pauseSignal?.removeListener(_onPauseSignal);
    for (final s in _subs) {
      s.cancel();
    }
    _player?.dispose();
    super.dispose();
  }

  void _seekTo(double fraction) {
    final total = _duration.inMilliseconds;
    if (total <= 0) return;
    final target = Duration(milliseconds: (fraction * total).round());
    // Optimistically move the readout to the target so the thumb stays where it
    // was released instead of bouncing back to a stale position event.
    setState(() => _position = target);
    _player?.seek(target);
    // A manual scrub is a deliberate move → always push it to the analysis,
    // even when paused.
    widget.onPosition?.call(target);
  }

  String _fmt(Duration d) {
    final m = d.inMinutes;
    final s = (d.inSeconds % 60).toString().padLeft(2, '0');
    return '$m:$s';
  }

  @override
  Widget build(BuildContext context) {
    if (_unavailable) return const _UnavailableClip();
    final total = _duration.inMilliseconds;
    final pos = _dragging
        ? (_dragValue * total).round()
        : _position.inMilliseconds.clamp(0, total == 0 ? 0 : total);
    final fraction = total == 0 ? 0.0 : (pos / total).clamp(0.0, 1.0);

    return Container(
      height: 48,
      padding: const EdgeInsets.symmetric(horizontal: 10),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      child: Row(
        children: [
          _RoundIconButton(
            icon: _playing ? FIcons.pause : FIcons.play,
            // Pause stays live; Play waits for the take to end.
            onTap: widget.playBlocked && !_playing
                ? null
                : () => _player?.playOrPause(),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: _Scrubber(
              fraction: fraction,
              onChangeStart: () => setState(() {
                _dragging = true;
                _dragValue = fraction;
              }),
              onChanged: (v) => setState(() => _dragValue = v),
              onChangeEnd: (v) {
                setState(() => _dragging = false);
                _seekTo(v);
              },
            ),
          ),
          const SizedBox(width: 12),
          SizedBox(
            width: 82,
            child: Text(
              '${_fmt(Duration(milliseconds: pos))} / ${_fmt(_duration)}',
              textAlign: TextAlign.right,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 11.5,
                fontFeatures: [FontFeature.tabularFigures()],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The player row for a clip whose file is not on this device: same footprint
/// as the live player, with nothing to press.
class _UnavailableClip extends StatelessWidget {
  const _UnavailableClip();

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 48,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      child: const Row(
        children: [
          Icon(FIcons.fileX, size: 15, color: Studio.textFaint),
          SizedBox(width: 10),
          Expanded(
            child: Text(
              unavailableClipMessage,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(color: Studio.textFaint, fontSize: 12),
            ),
          ),
        ],
      ),
    );
  }
}

/// What an example whose file is missing on this device says instead of
/// playing (shared by the audio and video players).
const String unavailableClipMessage = "This clip's file isn't on this device.";

/// A small coral play/pause disc.
class _RoundIconButton extends StatelessWidget {
  const _RoundIconButton({required this.icon, required this.onTap});

  final IconData icon;

  /// Null shows the button off.
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: onTap == null
          ? SystemMouseCursors.basic
          : SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Opacity(
          opacity: onTap == null ? 0.4 : 1,
          child: Container(
            width: 30,
            height: 30,
            decoration: BoxDecoration(
              color: Studio.accent.withValues(alpha: 0.16),
              shape: BoxShape.circle,
              border: Border.all(color: Studio.accent.withValues(alpha: 0.45)),
            ),
            child: Icon(icon, size: 15, color: Studio.accentBright),
          ),
        ),
      ),
    );
  }
}

/// A thin draggable progress bar (thumb appears on hover/drag), styled for the
/// studio look. The thumb tracks the fill's right edge exactly.
class _Scrubber extends StatefulWidget {
  const _Scrubber({
    required this.fraction,
    required this.onChangeStart,
    required this.onChanged,
    required this.onChangeEnd,
  });

  final double fraction;
  final VoidCallback onChangeStart;
  final ValueChanged<double> onChanged;
  final ValueChanged<double> onChangeEnd;

  @override
  State<_Scrubber> createState() => _ScrubberState();
}

class _ScrubberState extends State<_Scrubber> {
  static const double _thumb = 10;

  bool _hover = false;

  double _fractionFor(Offset local, double width) =>
      (local.dx / width).clamp(0.0, 1.0);

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: LayoutBuilder(
        builder: (context, constraints) {
          final width = constraints.maxWidth;
          return GestureDetector(
            behavior: HitTestBehavior.opaque,
            onHorizontalDragStart: (_) => widget.onChangeStart(),
            onHorizontalDragUpdate: (d) =>
                widget.onChanged(_fractionFor(d.localPosition, width)),
            onHorizontalDragEnd: (_) => widget.onChangeEnd(widget.fraction),
            onTapDown: (d) {
              widget.onChangeStart();
              final f = _fractionFor(d.localPosition, width);
              widget.onChanged(f);
              widget.onChangeEnd(f);
            },
            child: SizedBox(
              height: 24,
              child: Stack(
                alignment: Alignment.centerLeft,
                children: [
                  Container(
                    height: 4,
                    decoration: BoxDecoration(
                      color: Studio.surfaceRaised,
                      borderRadius: BorderRadius.circular(2),
                    ),
                  ),
                  FractionallySizedBox(
                    widthFactor: widget.fraction,
                    child: Container(
                      height: 4,
                      decoration: BoxDecoration(
                        color: Studio.accent,
                        borderRadius: BorderRadius.circular(2),
                      ),
                    ),
                  ),
                  if (_hover)
                    // Center the thumb on the fill's right edge: clamp so it
                    // stays fully on-track at both ends.
                    Positioned(
                      left: (widget.fraction * width - _thumb / 2).clamp(
                        0.0,
                        width - _thumb,
                      ),
                      child: Container(
                        width: _thumb,
                        height: _thumb,
                        decoration: const BoxDecoration(
                          color: Studio.accentBright,
                          shape: BoxShape.circle,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}
