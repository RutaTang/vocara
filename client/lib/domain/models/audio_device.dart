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
    this.transport = AudioTransport.unknown,
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

  /// How the device is attached. Decides which outputs the user may mark as headphones-only
  /// (see [AudioTransport.canCarryHeadphonesMark]).
  final AudioTransport transport;

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is AudioDevice &&
          other.uid == uid &&
          other.name == name &&
          other.rate == rate &&
          other.isDefault == isDefault &&
          other.isInput == isInput &&
          other.isBluetooth == isBluetooth &&
          other.transport == transport;

  @override
  int get hashCode =>
      Object.hash(uid, name, rate, isDefault, isInput, isBluetooth, transport);
}

/// How a device is attached — the engine's `device::Transport`, in the same order (the index
/// crosses the FFI as `VeDeviceInfo.transport`).
enum AudioTransport {
  unknown,
  builtIn,
  usb,
  bluetooth,
  hdmi,
  displayPort,
  network,
  thunderbolt,
  fireWire,
  pci,
  aggregate,
  virtual,
  other;

  /// The engine's code, [unknown] for one this build does not know.
  static AudioTransport fromCode(int code) =>
      code >= 0 && code < values.length ? values[code] : unknown;

  /// Whether a device on this transport can be a pair of headphones the user marks as such: a
  /// product of its own (USB, Bluetooth, an interface). Never a port or a sink with speakers of
  /// its own — built-in (the jack, and an Intel Mac's one device for jack AND speakers), HDMI,
  /// DisplayPort, AirPlay — nor an aggregate or virtual device, which could be anything.
  bool get canCarryHeadphonesMark => switch (this) {
    usb || bluetooth || thunderbolt || fireWire || pci => true,
    _ => false,
  };
}
