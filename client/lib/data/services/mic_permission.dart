import 'package:flutter/services.dart';

/// Bridges to the macOS mic-permission method channel (`vocara/mic`, handled in the Runner).
///
/// The native engine captures via cpal/CoreAudio (raw HAL), which macOS feeds SILENCE when the
/// app lacks microphone permission — and raw HAL access, unlike AVFoundation, never triggers the
/// permission prompt or lists the app under Privacy → Microphone. Requesting through AVFoundation
/// (on a foreground user action) is what actually prompts + grants, so the capture gets real audio.
class MicPermission {
  static const _channel = MethodChannel('vocara/mic');

  /// Request microphone access (prompts on first use). Returns whether it's granted.
  static Future<bool> request() async {
    try {
      return await _channel.invokeMethod<bool>('request') ?? false;
    } catch (_) {
      // No channel (non-macOS / test) → don't block capture.
      return true;
    }
  }

  /// A short status string for diagnostics: whether the channel is wired and the grant state.
  static Future<String> diagnostic() async {
    try {
      final st = await _channel.invokeMethod<bool>('status');
      return 'perm=${st == true ? "granted" : "denied"}';
    } catch (e) {
      return 'perm-channel-MISSING';
    }
  }
}
