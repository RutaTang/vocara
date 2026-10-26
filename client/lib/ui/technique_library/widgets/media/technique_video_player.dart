import 'dart:async';

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';

import '../../../../data/db/asset_store.dart';
import '../../../core/themes/studio.dart';
import 'technique_audio_player.dart'
    show createExamplePlayer, pauseWhilePlayBlocked, unavailableClipMessage;

/// An inline video player for a technique example clip. To avoid spinning up a
/// libmpv instance and a GPU texture for every clip just by showing the viewer,
/// the [Player]/[VideoController] are created LAZILY on the first play press; a
/// lightweight poster is shown until then. media_kit's hover controls handle
/// play/seek/volume/fullscreen once started.
class TechniqueVideoPlayer extends StatefulWidget {
  const TechniqueVideoPlayer({
    required this.path,
    this.pauseSignal,
    this.playBlocked = false,
    super.key,
  });

  /// Absolute sandbox path to the video file.
  final String path;

  /// Fires when playback must stop (the mic armed for a take): the player
  /// pauses where it is.
  final Listenable? pauseSignal;

  /// While a take records: the poster and the controls take no input, or a
  /// video started then played on speakers into the take.
  final bool playBlocked;

  @override
  State<TechniqueVideoPlayer> createState() => _TechniqueVideoPlayerState();
}

class _TechniqueVideoPlayerState extends State<TechniqueVideoPlayer> {
  Player? _player;
  VideoController? _controller;
  StreamSubscription<bool>? _blockedPlay;

  /// True when the clip cannot be played here: its bytes are not on this
  /// device (the repository's `asset://` placeholder, which media_kit would
  /// mistake for a bundled Flutter asset), or opening it failed.
  late bool _unavailable = AssetStore.assetIdOf(widget.path) != null;

  void _start() {
    if (_player != null || _unavailable) return;
    Player? built;
    try {
      // Media first: its constructor resolves the URI and can throw, and it
      // used to throw out of this tap handler after the player was built.
      final media = Media(widget.path);
      final player = built = createExamplePlayer();
      _blockedPlay = pauseWhilePlayBlocked(
        player,
        () => mounted && widget.playBlocked,
      );
      final controller = VideoController(player);
      player.open(media);
      _player = player;
      _controller = controller;
    } catch (_) {
      unawaited(_blockedPlay?.cancel());
      _blockedPlay = null;
      built?.dispose();
      _unavailable = true;
    }
    setState(() {});
  }

  @override
  void initState() {
    super.initState();
    widget.pauseSignal?.addListener(_onPauseSignal);
  }

  @override
  void didUpdateWidget(TechniqueVideoPlayer old) {
    super.didUpdateWidget(old);
    if (!identical(old.pauseSignal, widget.pauseSignal)) {
      old.pauseSignal?.removeListener(_onPauseSignal);
      widget.pauseSignal?.addListener(_onPauseSignal);
    }
  }

  void _onPauseSignal() => _player?.pause();

  @override
  void dispose() {
    widget.pauseSignal?.removeListener(_onPauseSignal);
    unawaited(_blockedPlay?.cancel());
    _player?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = _controller;
    return ClipRRect(
      borderRadius: BorderRadius.circular(10),
      child: Container(
        constraints: const BoxConstraints(maxWidth: 520),
        decoration: BoxDecoration(
          color: Colors.black,
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: Studio.border),
        ),
        child: AspectRatio(
          aspectRatio: 16 / 9,
          // Neither the pointer nor the keyboard: the controls' own Space /
          // media-key shortcuts started a video into the take (and anything
          // that slips through is paused again, see pauseWhilePlayBlocked).
          child: IgnorePointer(
            ignoring: widget.playBlocked,
            child: ExcludeFocus(
              excluding: widget.playBlocked,
              child: controller == null
                  ? _Poster(onPlay: _start, unavailable: _unavailable)
                  // No volume on the wheel (or a drag): the controls' default
                  // turned every scroll of the page that passed over a started
                  // video into a volume change, quietly fading it toward mute.
                  : MaterialDesktopVideoControlsTheme(
                      normal: _noWheelVolume,
                      fullscreen: _noWheelVolume,
                      child: Video(
                        controller: controller,
                        controls: AdaptiveVideoControls,
                        fill: Colors.black,
                      ),
                    ),
            ),
          ),
        ),
      ),
    );
  }
}

/// The pre-load placeholder: a dark surface with a centered play button, or
/// the unavailable notice when the clip cannot be played here.
class _Poster extends StatelessWidget {
  const _Poster({required this.onPlay, required this.unavailable});

  final VoidCallback onPlay;
  final bool unavailable;

  @override
  Widget build(BuildContext context) {
    if (unavailable) {
      return const ColoredBox(
        color: Colors.black,
        child: Center(
          child: Text(
            unavailableClipMessage,
            style: TextStyle(color: Studio.textFaint, fontSize: 12),
          ),
        ),
      );
    }
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onPlay,
        child: ColoredBox(
          color: Colors.black,
          child: Center(
            child: Container(
              width: 54,
              height: 54,
              decoration: BoxDecoration(
                color: Studio.accent.withValues(alpha: 0.9),
                shape: BoxShape.circle,
              ),
              child: const Icon(FIcons.play, color: Colors.white, size: 24),
            ),
          ),
        ),
      ),
    );
  }
}

const _noWheelVolume = MaterialDesktopVideoControlsThemeData(
  modifyVolumeOnScroll: false,
);
