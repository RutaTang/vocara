import 'package:flutter/foundation.dart';

import 'audio_fx.dart';

/// A DAW-style FX bus (group / aux track): a named mixer strip with its own
/// EQ → Comp insert chain, fader, mute, solo, and reverb send. Vocal takes route
/// their output into a bus to share processing; a bus itself routes to the
/// master or into another bus. Backing tracks never route to a bus (stay dry).
@immutable
class FxBus {
  const FxBus({
    required this.id,
    required this.name,
    required this.fx,
    this.colorValue,
    this.volume = 1.0,
    this.muted = false,
    this.soloed = false,
    this.pan = 0.0,
    this.outputBusId,
    this.sends = const [],
  });

  final String id;
  final String name;

  /// The bus's insert chain (EQ + compressor) plus its reverb send level —
  /// reuses [TrackFx] verbatim (a bus is just a channel strip with no source).
  final TrackFx fx;

  /// Optional ARGB tint, like a take's color.
  final int? colorValue;

  /// Bus fader (linear, 0..1+).
  final double volume;
  final bool muted;

  /// When any track or bus is soloed, only soloed sources (and the routing
  /// chain that carries them to the master) are heard. Soloing a bus keeps the
  /// takes feeding it audible; see the ViewModel's solo resolution.
  final bool soloed;

  /// Stereo pan in [-1, 1] (0 = center).
  final double pan;

  /// Output routing: another bus's id, or null = straight to the master.
  final String? outputBusId;

  /// Post-fader aux sends to other FX buses (independent of the main output).
  final List<TrackSend> sends;

  FxBus copyWith({
    String? name,
    TrackFx? fx,
    int? colorValue,
    bool clearColor = false,
    double? volume,
    bool? muted,
    bool? soloed,
    double? pan,
    String? outputBusId,
    bool clearOutput = false,
    List<TrackSend>? sends,
  }) => FxBus(
    id: id,
    name: name ?? this.name,
    fx: fx ?? this.fx,
    colorValue: clearColor ? null : (colorValue ?? this.colorValue),
    volume: volume ?? this.volume,
    muted: muted ?? this.muted,
    soloed: soloed ?? this.soloed,
    pan: pan ?? this.pan,
    outputBusId: clearOutput ? null : (outputBusId ?? this.outputBusId),
    // Defensive copy: never retain a caller's list, so mutating it after the
    // call can't reach into this (otherwise immutable) value.
    sends: sends != null ? List.of(sends) : this.sends,
  );

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'fx': fx.toJson(),
    'colorValue': colorValue,
    'volume': volume,
    'muted': muted,
    'soloed': soloed,
    'pan': pan,
    'outputBusId': outputBusId,
    'sends': sends.map((s) => s.toJson()).toList(),
  };

  factory FxBus.fromJson(Map<String, dynamic> j) => FxBus(
    id: j['id'] as String,
    name: j['name'] as String? ?? 'Bus',
    fx: j['fx'] != null
        ? TrackFx.fromJson(j['fx'] as Map<String, dynamic>)
        : TrackFx.flat(),
    colorValue: j['colorValue'] as int?,
    volume: (j['volume'] as num?)?.toDouble() ?? 1.0,
    muted: j['muted'] as bool? ?? false,
    soloed: j['soloed'] as bool? ?? false,
    pan: (j['pan'] as num?)?.toDouble() ?? 0.0,
    outputBusId: j['outputBusId'] as String?,
    sends: (j['sends'] as List<dynamic>? ?? const [])
        .map((e) => TrackSend.fromJson(e as Map<String, dynamic>))
        .toList(),
  );

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is FxBus &&
          other.id == id &&
          other.name == name &&
          other.fx == fx &&
          other.colorValue == colorValue &&
          other.volume == volume &&
          other.muted == muted &&
          other.soloed == soloed &&
          other.pan == pan &&
          other.outputBusId == outputBusId &&
          listEquals(other.sends, sends);

  @override
  int get hashCode => Object.hash(
    id,
    name,
    fx,
    colorValue,
    volume,
    muted,
    soloed,
    pan,
    outputBusId,
    Object.hashAll(sends),
  );
}
