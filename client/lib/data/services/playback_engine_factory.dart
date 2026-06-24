import 'graph_audio_playback_service.dart';
import 'playback_engine.dart';

/// Constructs the app's playback engine: the native Rust real-time graph engine
/// ([GraphAudioPlaybackService]). If no output device opens, the engine reports
/// `isAvailable == false` and every call no-ops, so the app stays usable (just
/// silent) instead of failing.
PlaybackEngine createPlaybackEngine() => GraphAudioPlaybackService();
