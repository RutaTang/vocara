import 'package:flutter/foundation.dart';

/// A selectable audio device (currently output; input lands in a later phase).
/// [uid] is the stable CoreAudio device UID — the persist key — not the display name.
@immutable
class AudioDevice {
  const AudioDevice({
    required this.uid,
    required this.name,
    this.rate = 0,
    this.isDefault = false,
    this.isInput = false,
    this.isBluetooth = false,
  });

  final String uid;
  final String name;

  /// Native sample rate in Hz (0 if unknown).
  final int rate;
  final bool isDefault;
  final bool isInput;

  /// On a Bluetooth transport. Selecting the same BT device for both input and output forces
  /// the headset into low-quality HFP mode (a hardware limit) — the UI warns about this.
  final bool isBluetooth;

  @override
  bool operator ==(Object other) =>
      other is AudioDevice && other.uid == uid && other.isInput == isInput;

  @override
  int get hashCode => Object.hash(uid, isInput);
}
