import 'package:flutter/foundation.dart';

/// An insert plugin type in the vocal channel's effect chain. The enum INDEX is
/// the engine's chain tag (must stay in sync with the Rust `FxKind` mapping); the
/// list order in [TrackFx.order] is the signal-chain order.
enum FxKind {
  eq,
  comp,
  deEsser,
  gate,
  delay,
  saturation,
  doubler,
  limiter,
  reverb,
}

/// The shape of an EQ band. Fixed per slot (matching the engine's default 6-band
/// strip): a low shelf, four peaks, a high shelf.
enum EqBandKind { lowShelf, peaking, highShelf }

/// One parametric EQ band. `freq` in Hz, `gainDb` boost/cut, `q` bandwidth.
@immutable
class EqBand {
  const EqBand({
    required this.kind,
    required this.freq,
    this.q = 1.0,
    this.gainDb = 0.0,
    this.enabled = true,
  });

  final EqBandKind kind;
  final double freq;
  final double q;
  final double gainDb;
  final bool enabled;

  EqBand copyWith({double? freq, double? q, double? gainDb, bool? enabled}) =>
      EqBand(
        kind: kind,
        freq: freq ?? this.freq,
        q: q ?? this.q,
        gainDb: gainDb ?? this.gainDb,
        enabled: enabled ?? this.enabled,
      );

  Map<String, dynamic> toJson() => {
    'kind': kind.name,
    'freq': freq,
    'q': q,
    'gainDb': gainDb,
    'enabled': enabled,
  };

  factory EqBand.fromJson(Map<String, dynamic> j) => EqBand(
    kind: EqBandKind.values.firstWhere(
      (k) => k.name == j['kind'],
      orElse: () => EqBandKind.peaking,
    ),
    freq: (j['freq'] as num).toDouble(),
    q: (j['q'] as num?)?.toDouble() ?? 1.0,
    gainDb: (j['gainDb'] as num?)?.toDouble() ?? 0.0,
    enabled: j['enabled'] as bool? ?? true,
  );
}

/// Feed-forward compressor settings.
@immutable
class CompSettings {
  const CompSettings({
    this.thresholdDb = -18.0,
    this.ratio = 3.0,
    this.kneeDb = 6.0,
    this.attackMs = 10.0,
    this.releaseMs = 120.0,
    this.makeupDb = 0.0,
  });

  final double thresholdDb;
  final double ratio;
  final double kneeDb;
  final double attackMs;
  final double releaseMs;
  final double makeupDb;

  CompSettings copyWith({
    double? thresholdDb,
    double? ratio,
    double? kneeDb,
    double? attackMs,
    double? releaseMs,
    double? makeupDb,
  }) => CompSettings(
    thresholdDb: thresholdDb ?? this.thresholdDb,
    ratio: ratio ?? this.ratio,
    kneeDb: kneeDb ?? this.kneeDb,
    attackMs: attackMs ?? this.attackMs,
    releaseMs: releaseMs ?? this.releaseMs,
    makeupDb: makeupDb ?? this.makeupDb,
  );

  Map<String, dynamic> toJson() => {
    'thresholdDb': thresholdDb,
    'ratio': ratio,
    'kneeDb': kneeDb,
    'attackMs': attackMs,
    'releaseMs': releaseMs,
    'makeupDb': makeupDb,
  };

  factory CompSettings.fromJson(Map<String, dynamic> j) => CompSettings(
    thresholdDb: (j['thresholdDb'] as num?)?.toDouble() ?? -18.0,
    ratio: (j['ratio'] as num?)?.toDouble() ?? 3.0,
    kneeDb: (j['kneeDb'] as num?)?.toDouble() ?? 6.0,
    attackMs: (j['attackMs'] as num?)?.toDouble() ?? 10.0,
    releaseMs: (j['releaseMs'] as num?)?.toDouble() ?? 120.0,
    makeupDb: (j['makeupDb'] as num?)?.toDouble() ?? 0.0,
  );
}

/// De-esser: split-band sibilance reducer.
@immutable
class DeEsserSettings {
  const DeEsserSettings({
    this.freq = 6500,
    this.thresholdDb = -28,
    this.rangeDb = 12,
  });
  final double freq;
  final double thresholdDb;
  final double rangeDb;
  DeEsserSettings copyWith({
    double? freq,
    double? thresholdDb,
    double? rangeDb,
  }) => DeEsserSettings(
    freq: freq ?? this.freq,
    thresholdDb: thresholdDb ?? this.thresholdDb,
    rangeDb: rangeDb ?? this.rangeDb,
  );
  Map<String, dynamic> toJson() => {
    'freq': freq,
    'thresholdDb': thresholdDb,
    'rangeDb': rangeDb,
  };
  factory DeEsserSettings.fromJson(Map<String, dynamic> j) => DeEsserSettings(
    freq: (j['freq'] as num?)?.toDouble() ?? 6500,
    thresholdDb: (j['thresholdDb'] as num?)?.toDouble() ?? -28,
    rangeDb: (j['rangeDb'] as num?)?.toDouble() ?? 12,
  );
}

/// Noise gate / downward expander.
@immutable
class GateSettings {
  const GateSettings({
    this.thresholdDb = -45,
    this.rangeDb = 60,
    this.attackMs = 2,
    this.releaseMs = 120,
  });
  final double thresholdDb;
  final double rangeDb;
  final double attackMs;
  final double releaseMs;
  GateSettings copyWith({
    double? thresholdDb,
    double? rangeDb,
    double? attackMs,
    double? releaseMs,
  }) => GateSettings(
    thresholdDb: thresholdDb ?? this.thresholdDb,
    rangeDb: rangeDb ?? this.rangeDb,
    attackMs: attackMs ?? this.attackMs,
    releaseMs: releaseMs ?? this.releaseMs,
  );
  Map<String, dynamic> toJson() => {
    'thresholdDb': thresholdDb,
    'rangeDb': rangeDb,
    'attackMs': attackMs,
    'releaseMs': releaseMs,
  };
  factory GateSettings.fromJson(Map<String, dynamic> j) => GateSettings(
    thresholdDb: (j['thresholdDb'] as num?)?.toDouble() ?? -45,
    rangeDb: (j['rangeDb'] as num?)?.toDouble() ?? 60,
    attackMs: (j['attackMs'] as num?)?.toDouble() ?? 2,
    releaseMs: (j['releaseMs'] as num?)?.toDouble() ?? 120,
  );
}

/// Feedback delay (echo / throw).
@immutable
class DelaySettings {
  const DelaySettings({this.timeMs = 300, this.feedback = 0.3, this.mix = 0.0});
  final double timeMs;
  final double feedback;
  final double mix;
  DelaySettings copyWith({double? timeMs, double? feedback, double? mix}) =>
      DelaySettings(
        timeMs: timeMs ?? this.timeMs,
        feedback: feedback ?? this.feedback,
        mix: mix ?? this.mix,
      );
  Map<String, dynamic> toJson() => {
    'timeMs': timeMs,
    'feedback': feedback,
    'mix': mix,
  };
  factory DelaySettings.fromJson(Map<String, dynamic> j) => DelaySettings(
    timeMs: (j['timeMs'] as num?)?.toDouble() ?? 300,
    feedback: (j['feedback'] as num?)?.toDouble() ?? 0.3,
    mix: (j['mix'] as num?)?.toDouble() ?? 0.0,
  );
}

/// Soft saturation / harmonic exciter.
@immutable
class SaturationSettings {
  const SaturationSettings({this.driveDb = 6, this.mix = 0.0});
  final double driveDb;
  final double mix;
  SaturationSettings copyWith({double? driveDb, double? mix}) =>
      SaturationSettings(
        driveDb: driveDb ?? this.driveDb,
        mix: mix ?? this.mix,
      );
  Map<String, dynamic> toJson() => {'driveDb': driveDb, 'mix': mix};
  factory SaturationSettings.fromJson(Map<String, dynamic> j) =>
      SaturationSettings(
        driveDb: (j['driveDb'] as num?)?.toDouble() ?? 6,
        mix: (j['mix'] as num?)?.toDouble() ?? 0.0,
      );
}

/// Vocal doubler / chorus.
@immutable
class DoublerSettings {
  const DoublerSettings({
    this.delayMs = 18,
    this.depthMs = 4,
    this.rateHz = 0.6,
    this.mix = 0.0,
  });
  final double delayMs;
  final double depthMs;
  final double rateHz;
  final double mix;
  DoublerSettings copyWith({
    double? delayMs,
    double? depthMs,
    double? rateHz,
    double? mix,
  }) => DoublerSettings(
    delayMs: delayMs ?? this.delayMs,
    depthMs: depthMs ?? this.depthMs,
    rateHz: rateHz ?? this.rateHz,
    mix: mix ?? this.mix,
  );
  Map<String, dynamic> toJson() => {
    'delayMs': delayMs,
    'depthMs': depthMs,
    'rateHz': rateHz,
    'mix': mix,
  };
  factory DoublerSettings.fromJson(Map<String, dynamic> j) => DoublerSettings(
    delayMs: (j['delayMs'] as num?)?.toDouble() ?? 18,
    depthMs: (j['depthMs'] as num?)?.toDouble() ?? 4,
    rateHz: (j['rateHz'] as num?)?.toDouble() ?? 0.6,
    mix: (j['mix'] as num?)?.toDouble() ?? 0.0,
  );
}

/// Brick-wall peak limiter.
@immutable
class LimiterSettings {
  const LimiterSettings({this.ceilingDb = -0.3, this.releaseMs = 80});
  final double ceilingDb;
  final double releaseMs;
  LimiterSettings copyWith({double? ceilingDb, double? releaseMs}) =>
      LimiterSettings(
        ceilingDb: ceilingDb ?? this.ceilingDb,
        releaseMs: releaseMs ?? this.releaseMs,
      );
  Map<String, dynamic> toJson() => {
    'ceilingDb': ceilingDb,
    'releaseMs': releaseMs,
  };
  factory LimiterSettings.fromJson(Map<String, dynamic> j) => LimiterSettings(
    ceilingDb: (j['ceilingDb'] as num?)?.toDouble() ?? -0.3,
    releaseMs: (j['releaseMs'] as num?)?.toDouble() ?? 80,
  );
}

/// A post-fader aux send: a copy of the channel, scaled by [levelDb], routed to
/// the FX bus [busId] — independent of the channel's main output/fader.
@immutable
class TrackSend {
  const TrackSend({required this.busId, this.levelDb = 0.0});
  final String busId;
  final double levelDb;
  TrackSend copyWith({String? busId, double? levelDb}) =>
      TrackSend(busId: busId ?? this.busId, levelDb: levelDb ?? this.levelDb);
  Map<String, dynamic> toJson() => {'busId': busId, 'levelDb': levelDb};
  factory TrackSend.fromJson(Map<String, dynamic> j) => TrackSend(
    busId: j['busId'] as String,
    levelDb: (j['levelDb'] as num?)?.toDouble() ?? 0.0,
  );
}

/// Max post-fader aux sends per channel (matches the engine's `MAX_SENDS`).
const int kMaxSends = 4;

/// Reverb send level meaning "off" — below this is silent.
const double kSendOffDb = -120.0;

/// Per-(vocal-)track effect chain: EQ → compressor + a reverb send level.
@immutable
class TrackFx {
  const TrackFx({
    required this.eq,
    this.eqOn = false,
    this.comp = const CompSettings(),
    this.compOn = false,
    this.deEsser = const DeEsserSettings(),
    this.deEsserOn = false,
    this.gate = const GateSettings(),
    this.gateOn = false,
    this.delay = const DelaySettings(),
    this.delayOn = false,
    this.saturation = const SaturationSettings(),
    this.saturationOn = false,
    this.doubler = const DoublerSettings(),
    this.doublerOn = false,
    this.limiter = const LimiterSettings(),
    this.limiterOn = false,
    this.reverb = const ReverbSettings(),
    this.reverbOn = false,
    this.sendDb = kSendOffDb,
    this.order = FxKind.values,
  });

  final List<EqBand> eq;
  final bool eqOn;
  final CompSettings comp;
  final bool compOn;
  final DeEsserSettings deEsser;
  final bool deEsserOn;
  final GateSettings gate;
  final bool gateOn;
  final DelaySettings delay;
  final bool delayOn;
  final SaturationSettings saturation;
  final bool saturationOn;
  final DoublerSettings doubler;
  final bool doublerOn;
  final LimiterSettings limiter;
  final bool limiterOn;
  final ReverbSettings reverb;
  final bool reverbOn;
  final double sendDb;

  /// The insert plugins in signal-chain order (Source → order[0] → … → fader).
  /// Reordering changes the audible chain; on/off is per-plugin (eqOn/compOn).
  final List<FxKind> order;

  /// The default flat 6-band strip (low shelf, four peaks, high shelf),
  /// everything off. Frequencies match the engine's `EqNode::default_strip`.
  static TrackFx flat() => const TrackFx(eq: _flatBands);

  static const List<EqBand> _flatBands = [
    EqBand(kind: EqBandKind.lowShelf, freq: 100, q: 0.7),
    EqBand(kind: EqBandKind.peaking, freq: 250, q: 1.0),
    EqBand(kind: EqBandKind.peaking, freq: 700, q: 1.0),
    EqBand(kind: EqBandKind.peaking, freq: 2000, q: 1.0),
    EqBand(kind: EqBandKind.peaking, freq: 6000, q: 1.0),
    EqBand(kind: EqBandKind.highShelf, freq: 12000, q: 0.7),
  ];

  /// Normalizes a saved EQ to the fixed 6-slot layout the engine expects (band
  /// kind is fixed per slot — `ve_set_eq_band` never sends a kind). Legacy
  /// 4-band takes are remapped by role so the displayed curve keeps matching the
  /// sound: the low/high shelves and peaks land on like-kind slots; unfilled
  /// slots stay flat at their default frequency. A take already in the canonical
  /// layout is returned untouched.
  static List<EqBand> _normalizeEq(List<EqBand> saved) {
    if (saved.length == _flatBands.length) {
      var matches = true;
      for (var i = 0; i < saved.length; i++) {
        if (saved[i].kind != _flatBands[i].kind) {
          matches = false;
          break;
        }
      }
      if (matches) return saved;
    }
    final lowShelf = saved.where((b) => b.kind == EqBandKind.lowShelf).iterator;
    final highShelf = saved
        .where((b) => b.kind == EqBandKind.highShelf)
        .iterator;
    final peaks = saved.where((b) => b.kind == EqBandKind.peaking).toList();
    var peak = 0;
    return [
      for (var i = 0; i < _flatBands.length; i++)
        switch (_flatBands[i].kind) {
          EqBandKind.lowShelf when lowShelf.moveNext() => lowShelf.current,
          EqBandKind.highShelf when highShelf.moveNext() => highShelf.current,
          EqBandKind.peaking when peak < peaks.length => peaks[peak++],
          _ => _flatBands[i],
        },
    ];
  }

  bool get hasReverbSend => sendDb > kSendOffDb;

  TrackFx copyWith({
    List<EqBand>? eq,
    bool? eqOn,
    CompSettings? comp,
    bool? compOn,
    DeEsserSettings? deEsser,
    bool? deEsserOn,
    GateSettings? gate,
    bool? gateOn,
    DelaySettings? delay,
    bool? delayOn,
    SaturationSettings? saturation,
    bool? saturationOn,
    DoublerSettings? doubler,
    bool? doublerOn,
    LimiterSettings? limiter,
    bool? limiterOn,
    ReverbSettings? reverb,
    bool? reverbOn,
    double? sendDb,
    List<FxKind>? order,
  }) => TrackFx(
    eq: eq ?? this.eq,
    eqOn: eqOn ?? this.eqOn,
    comp: comp ?? this.comp,
    compOn: compOn ?? this.compOn,
    deEsser: deEsser ?? this.deEsser,
    deEsserOn: deEsserOn ?? this.deEsserOn,
    gate: gate ?? this.gate,
    gateOn: gateOn ?? this.gateOn,
    delay: delay ?? this.delay,
    delayOn: delayOn ?? this.delayOn,
    saturation: saturation ?? this.saturation,
    saturationOn: saturationOn ?? this.saturationOn,
    doubler: doubler ?? this.doubler,
    doublerOn: doublerOn ?? this.doublerOn,
    limiter: limiter ?? this.limiter,
    limiterOn: limiterOn ?? this.limiterOn,
    reverb: reverb ?? this.reverb,
    reverbOn: reverbOn ?? this.reverbOn,
    sendDb: sendDb ?? this.sendDb,
    order: order ?? this.order,
  );

  /// Normalizes a chain order: de-dupes, drops unknown kinds, and appends any
  /// known kind missing from a saved order (so new plugin types always appear).
  static List<FxKind> _normalizeOrder(List<FxKind> parsed) {
    final seen = <FxKind>{};
    final out = <FxKind>[];
    for (final k in parsed) {
      if (seen.add(k)) out.add(k);
    }
    for (final k in FxKind.values) {
      if (seen.add(k)) out.add(k);
    }
    return out;
  }

  /// Replace a single EQ band by index.
  TrackFx withBand(int index, EqBand band) {
    if (index < 0 || index >= eq.length) return this;
    final next = [...eq];
    next[index] = band;
    return copyWith(eq: next);
  }

  Map<String, dynamic> toJson() => {
    'eq': [for (final b in eq) b.toJson()],
    'eqOn': eqOn,
    'comp': comp.toJson(),
    'compOn': compOn,
    'deEsser': deEsser.toJson(),
    'deEsserOn': deEsserOn,
    'gate': gate.toJson(),
    'gateOn': gateOn,
    'delay': delay.toJson(),
    'delayOn': delayOn,
    'saturation': saturation.toJson(),
    'saturationOn': saturationOn,
    'doubler': doubler.toJson(),
    'doublerOn': doublerOn,
    'limiter': limiter.toJson(),
    'limiterOn': limiterOn,
    'reverb': reverb.toJson(),
    'reverbOn': reverbOn,
    'sendDb': sendDb,
    'order': [for (final k in order) k.name],
  };

  static T _sub<T>(
    Map<String, dynamic> j,
    String key,
    T Function(Map<String, dynamic>) parse,
    T fallback,
  ) {
    final v = j[key];
    return v is Map<String, dynamic> ? parse(v) : fallback;
  }

  factory TrackFx.fromJson(Map<String, dynamic> j) {
    final eqJson = j['eq'] as List?;
    final eq = eqJson != null && eqJson.isNotEmpty
        ? _normalizeEq([
            for (final b in eqJson) EqBand.fromJson(b as Map<String, dynamic>),
          ])
        : _flatBands;
    final orderJson = j['order'] as List?;
    final order = _normalizeOrder([
      for (final e in orderJson ?? const [])
        FxKind.values.firstWhere((k) => k.name == e, orElse: () => FxKind.eq),
    ]);
    return TrackFx(
      eq: eq,
      eqOn: j['eqOn'] as bool? ?? false,
      comp: _sub(j, 'comp', CompSettings.fromJson, const CompSettings()),
      compOn: j['compOn'] as bool? ?? false,
      deEsser: _sub(
        j,
        'deEsser',
        DeEsserSettings.fromJson,
        const DeEsserSettings(),
      ),
      deEsserOn: j['deEsserOn'] as bool? ?? false,
      gate: _sub(j, 'gate', GateSettings.fromJson, const GateSettings()),
      gateOn: j['gateOn'] as bool? ?? false,
      delay: _sub(j, 'delay', DelaySettings.fromJson, const DelaySettings()),
      delayOn: j['delayOn'] as bool? ?? false,
      saturation: _sub(
        j,
        'saturation',
        SaturationSettings.fromJson,
        const SaturationSettings(),
      ),
      saturationOn: j['saturationOn'] as bool? ?? false,
      doubler: _sub(
        j,
        'doubler',
        DoublerSettings.fromJson,
        const DoublerSettings(),
      ),
      doublerOn: j['doublerOn'] as bool? ?? false,
      limiter: _sub(
        j,
        'limiter',
        LimiterSettings.fromJson,
        const LimiterSettings(),
      ),
      limiterOn: j['limiterOn'] as bool? ?? false,
      reverb: _sub(
        j,
        'reverb',
        ReverbSettings.fromJson,
        const ReverbSettings(),
      ),
      reverbOn: j['reverbOn'] as bool? ?? false,
      sendDb: (j['sendDb'] as num?)?.toDouble() ?? kSendOffDb,
      order: order,
    );
  }
}

/// The one shared reverb bus (Freeverb). `wet`/`width` 0..1; tracks feed it via their send.
@immutable
class ReverbSettings {
  const ReverbSettings({
    this.roomSize = 0.5,
    this.damping = 0.5,
    this.wet = 0.25,
    this.width = 1.0,
  });

  final double roomSize;
  final double damping;
  final double wet;
  final double width;

  ReverbSettings copyWith({
    double? roomSize,
    double? damping,
    double? wet,
    double? width,
  }) => ReverbSettings(
    roomSize: roomSize ?? this.roomSize,
    damping: damping ?? this.damping,
    wet: wet ?? this.wet,
    width: width ?? this.width,
  );

  Map<String, dynamic> toJson() => {
    'roomSize': roomSize,
    'damping': damping,
    'wet': wet,
    'width': width,
  };

  factory ReverbSettings.fromJson(Map<String, dynamic> j) => ReverbSettings(
    // Clamp on load: a synced / migrated / hand-edited value outside [0,1] would otherwise
    // push the engine reverb's comb feedback unstable (the engine clamps too, but keep the
    // model itself sane so the UI and persistence never round-trip a bad value).
    roomSize: ((j['roomSize'] as num?)?.toDouble() ?? 0.5).clamp(0.0, 1.0),
    damping: ((j['damping'] as num?)?.toDouble() ?? 0.5).clamp(0.0, 1.0),
    wet: ((j['wet'] as num?)?.toDouble() ?? 0.25).clamp(0.0, 1.0),
    width: ((j['width'] as num?)?.toDouble() ?? 1.0).clamp(0.0, 1.0),
  );
}
