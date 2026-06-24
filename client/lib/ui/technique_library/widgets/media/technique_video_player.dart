import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';

import '../../../core/themes/studio.dart';

/// An inline video player for a technique example clip. To avoid spinning up a
/// libmpv instance and a GPU texture for every clip just by showing the viewer,
/// the [Player]/[VideoController] are created LAZILY on the first play press; a
/// lightweight poster is shown until then. media_kit's hover controls handle
/// play/seek/volume/fullscreen once started.
class TechniqueVideoPlayer extends StatefulWidget {
  const TechniqueVideoPlayer({required this.path, super.key});

  /// Absolute sandbox path to the video file.
  final String path;

  @override
  State<TechniqueVideoPlayer> createState() => _TechniqueVideoPlayerState();
}

class _TechniqueVideoPlayerState extends State<TechniqueVideoPlayer> {
  Player? _player;
  VideoController? _controller;

  void _start() {
    if (_player != null) return;
    final player = Player();
    _player = player;
    _controller = VideoController(player);
    player.open(Media(widget.path));
    setState(() {});
  }

  @override
  void dispose() {
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
          child: controller == null
              ? _Poster(onPlay: _start)
              : Video(
                  controller: controller,
                  controls: AdaptiveVideoControls,
                  fill: Colors.black,
                ),
        ),
      ),
    );
  }
}

/// The pre-load placeholder: a dark surface with a centered play button.
class _Poster extends StatelessWidget {
  const _Poster({required this.onPlay});

  final VoidCallback onPlay;

  @override
  Widget build(BuildContext context) {
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
