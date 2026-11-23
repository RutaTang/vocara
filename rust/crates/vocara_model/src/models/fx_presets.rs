//! One-tap vocal chain and shared-reverb presets (port of
//! `lib/domain/models/fx_presets.dart`).

use super::audio_fx::{
    CompSettings, DeEsserSettings, DelaySettings, DoublerSettings, EqBand, EqBandKind,
    GateSettings, LimiterSettings, ReverbSettings, SaturationSettings, TrackFx,
};

/// A named vocal effect preset (one-tap good sound). [description] is a one-line
/// summary of the character / use-case.
#[derive(Clone, Debug)]
pub struct FxPreset {
    pub name: &'static str,
    pub description: &'static str,
    pub fx: TrackFx,
}

/// A named shared-reverb preset.
#[derive(Clone, Debug)]
pub struct ReverbPreset {
    pub name: &'static str,
    pub reverb: ReverbSettings,
}

/// The arguments of Dart's `_fx` builder: per-plugin on-flags + settings, and
/// the 6 EQ gains (dB). Any plugin left off keeps its default (no-op) settings.
struct Chain {
    eq_on: bool,
    low: f64,
    low_mid: f64,
    mid: f64,
    high_mid: f64,
    presence: f64,
    high: f64,
    comp_on: bool,
    comp: CompSettings,
    de_esser_on: bool,
    de_esser: DeEsserSettings,
    gate_on: bool,
    gate: GateSettings,
    delay_on: bool,
    delay: DelaySettings,
    saturation_on: bool,
    saturation: SaturationSettings,
    doubler_on: bool,
    doubler: DoublerSettings,
    limiter_on: bool,
    limiter: LimiterSettings,
    reverb_on: bool,
    reverb: ReverbSettings,
}

impl Default for Chain {
    fn default() -> Self {
        Chain {
            eq_on: false,
            low: 0.0,
            low_mid: 0.0,
            mid: 0.0,
            high_mid: 0.0,
            presence: 0.0,
            high: 0.0,
            comp_on: false,
            comp: CompSettings::DEFAULT,
            de_esser_on: false,
            de_esser: DeEsserSettings::DEFAULT,
            gate_on: false,
            gate: GateSettings::DEFAULT,
            delay_on: false,
            delay: DelaySettings::DEFAULT,
            saturation_on: false,
            saturation: SaturationSettings::DEFAULT,
            doubler_on: false,
            doubler: DoublerSettings::DEFAULT,
            limiter_on: false,
            limiter: LimiterSettings::DEFAULT,
            reverb_on: false,
            reverb: ReverbSettings::DEFAULT,
        }
    }
}

/// Builds a full-chain [`TrackFx`] from per-plugin on-flags + settings. The 6 EQ
/// gains map onto the fixed band layout the engine expects (low shelf 100 Hz,
/// peaks 250 / 700 / 2k / 6k, high shelf 12k).
fn fx(c: Chain) -> TrackFx {
    let band = |kind, freq, q, gain_db| EqBand {
        kind,
        freq,
        q,
        gain_db,
        enabled: true,
    };
    TrackFx {
        eq: vec![
            band(EqBandKind::LowShelf, 100.0, 0.7, c.low),
            band(EqBandKind::Peaking, 250.0, 1.0, c.low_mid),
            band(EqBandKind::Peaking, 700.0, 1.0, c.mid),
            band(EqBandKind::Peaking, 2000.0, 1.0, c.high_mid),
            band(EqBandKind::Peaking, 6000.0, 1.0, c.presence),
            band(EqBandKind::HighShelf, 12000.0, 0.7, c.high),
        ],
        eq_on: c.eq_on,
        comp: c.comp,
        comp_on: c.comp_on,
        de_esser: c.de_esser,
        de_esser_on: c.de_esser_on,
        gate: c.gate,
        gate_on: c.gate_on,
        delay: c.delay,
        delay_on: c.delay_on,
        saturation: c.saturation,
        saturation_on: c.saturation_on,
        doubler: c.doubler,
        doubler_on: c.doubler_on,
        limiter: c.limiter,
        limiter_on: c.limiter_on,
        reverb: c.reverb,
        reverb_on: c.reverb_on,
        ..TrackFx::flat()
    }
}

fn comp(
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    attack_ms: f64,
    release_ms: f64,
    makeup_db: f64,
) -> CompSettings {
    CompSettings {
        threshold_db,
        ratio,
        knee_db,
        attack_ms,
        release_ms,
        makeup_db,
    }
}

fn de_esser(freq: f64, threshold_db: f64, range_db: f64) -> DeEsserSettings {
    DeEsserSettings {
        freq,
        threshold_db,
        range_db,
    }
}

fn saturation(drive_db: f64, mix: f64) -> SaturationSettings {
    SaturationSettings { drive_db, mix }
}

fn limiter(ceiling_db: f64, release_ms: f64) -> LimiterSettings {
    LimiterSettings {
        ceiling_db,
        release_ms,
    }
}

fn reverb(room_size: f64, damping: f64, wet: f64, width: f64) -> ReverbSettings {
    ReverbSettings {
        room_size,
        damping,
        wet,
        width,
    }
}

fn doubler(delay_ms: f64, depth_ms: f64, rate_hz: f64, mix: f64) -> DoublerSettings {
    DoublerSettings {
        delay_ms,
        depth_ms,
        rate_hz,
        mix,
    }
}

/// `FxPresets.vocal` / `FxPresets.reverb`.
pub struct FxPresets;

impl FxPresets {
    /// Vocal chain presets. Each is a full-chain "one-tap good sound" — values
    /// were designed + range-validated for the built-in plugins.
    pub fn vocal() -> Vec<FxPreset> {
        vec![
            FxPreset {
                name: "Flat",
                description: "Clean reference / reset. Every plugin bypassed, true dry signal for A/B.",
                fx: TrackFx::flat(),
            },
            FxPreset {
                name: "Warm Lead",
                description: "Rich, chest-forward pop lead with low-shelf body and rounded analog warmth.",
                fx: fx(Chain {
                    eq_on: true,
                    low: 2.5,
                    low_mid: 1.5,
                    mid: -1.0,
                    high_mid: 1.0,
                    presence: 1.5,
                    high: 1.5,
                    comp_on: true,
                    comp: comp(-20.0, 3.5, 8.0, 15.0, 150.0, 3.5),
                    de_esser_on: true,
                    de_esser: de_esser(6500.0, -28.0, 7.0),
                    saturation_on: true,
                    saturation: saturation(9.0, 0.3),
                    limiter_on: true,
                    limiter: limiter(-0.5, 100.0),
                    reverb_on: true,
                    reverb: reverb(0.5, 0.65, 0.16, 0.9),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Bright Lead",
                description: "Polished pop lead with forward clarity, crisp presence, and controlled sibilance.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.0,
                    low_mid: -2.0,
                    mid: -1.5,
                    high_mid: 2.5,
                    presence: 3.0,
                    high: 3.5,
                    comp_on: true,
                    comp: comp(-22.0, 4.0, 6.0, 8.0, 120.0, 4.0),
                    de_esser_on: true,
                    de_esser: de_esser(7500.0, -30.0, 8.0),
                    saturation_on: true,
                    saturation: saturation(6.0, 0.25),
                    limiter_on: true,
                    limiter: limiter(-0.5, 80.0),
                    reverb_on: true,
                    reverb: reverb(0.45, 0.5, 0.16, 1.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Airy Breath",
                description: "Light, breathy vocal with silky high-shelf air, soft comp, and floating reverb.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.5,
                    low_mid: -2.5,
                    mid: -1.0,
                    high_mid: 1.5,
                    presence: 3.0,
                    high: 5.0,
                    comp_on: true,
                    comp: comp(-20.0, 3.0, 10.0, 18.0, 140.0, 3.0),
                    de_esser_on: true,
                    de_esser: de_esser(8500.0, -32.0, 9.0),
                    limiter_on: true,
                    limiter: limiter(-0.5, 90.0),
                    reverb_on: true,
                    reverb: reverb(0.7, 0.35, 0.2, 1.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Pop Throw",
                description: "Modern pop lead: polished comp, crisp presence, doubled width, eighth-note delay throws and air reverb.",
                fx: fx(Chain {
                    eq_on: true,
                    low: 1.0,
                    low_mid: -1.5,
                    mid: -1.0,
                    high_mid: 2.0,
                    presence: 3.0,
                    high: 3.0,
                    comp_on: true,
                    comp: comp(-22.0, 4.0, 6.0, 8.0, 120.0, 5.0),
                    de_esser_on: true,
                    de_esser: de_esser(8000.0, -32.0, 7.0),
                    delay_on: true,
                    delay: DelaySettings {
                        time_ms: 330.0,
                        feedback: 0.25,
                        mix: 0.15,
                    },
                    saturation_on: true,
                    saturation: saturation(6.0, 0.3),
                    doubler_on: true,
                    doubler: doubler(14.0, 3.0, 0.7, 0.35),
                    limiter_on: true,
                    limiter: limiter(-0.5, 80.0),
                    reverb_on: true,
                    reverb: reverb(0.5, 0.5, 0.18, 1.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Rap Cut",
                description: "Aggressive hip-hop lead: tight fast comp, forward presence, de-essed saturated grit, brick-walled, no reverb.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.5,
                    low_mid: -2.0,
                    mid: -1.0,
                    high_mid: 2.5,
                    presence: 3.5,
                    high: 1.5,
                    comp_on: true,
                    comp: comp(-24.0, 6.0, 3.0, 3.0, 90.0, 6.0),
                    de_esser_on: true,
                    de_esser: de_esser(6500.0, -32.0, 8.0),
                    saturation_on: true,
                    saturation: saturation(9.0, 0.35),
                    limiter_on: true,
                    limiter: limiter(-0.5, 60.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Silk RnB",
                description: "Smooth, warm R&B lead with subtle doubler width and a lush short reverb wash.",
                fx: fx(Chain {
                    eq_on: true,
                    low: 2.0,
                    low_mid: 1.0,
                    mid: -1.5,
                    high_mid: 1.0,
                    presence: 2.0,
                    high: 3.0,
                    comp_on: true,
                    comp: comp(-20.0, 3.0, 8.0, 15.0, 150.0, 4.0),
                    de_esser_on: true,
                    de_esser: de_esser(7500.0, -30.0, 6.0),
                    saturation_on: true,
                    saturation: saturation(5.0, 0.25),
                    doubler_on: true,
                    doubler: doubler(16.0, 4.0, 0.5, 0.35),
                    limiter_on: true,
                    limiter: limiter(-0.5, 100.0),
                    reverb_on: true,
                    reverb: reverb(0.45, 0.55, 0.2, 1.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Rock Edge",
                description: "Edgy rock vocal: driven saturation, biting presence, tight fast comp and gate to ride over the band.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.0,
                    low_mid: -2.5,
                    mid: 1.0,
                    high_mid: 3.0,
                    presence: 4.0,
                    high: 1.0,
                    comp_on: true,
                    comp: comp(-22.0, 5.0, 4.0, 5.0, 100.0, 5.0),
                    de_esser_on: true,
                    de_esser: de_esser(6000.0, -30.0, 7.0),
                    gate_on: true,
                    gate: GateSettings {
                        threshold_db: -42.0,
                        range_db: 25.0,
                        attack_ms: 1.0,
                        release_ms: 120.0,
                    },
                    saturation_on: true,
                    saturation: saturation(14.0, 0.45),
                    limiter_on: true,
                    limiter: limiter(-0.5, 70.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Folk Air",
                description: "Natural acoustic-singer vocal: light touch, warm body, airy top, gentle dynamics, barely-there room.",
                fx: fx(Chain {
                    eq_on: true,
                    low: 1.5,
                    low_mid: -1.0,
                    mid: 0.0,
                    high_mid: 1.0,
                    presence: 1.5,
                    high: 2.5,
                    comp_on: true,
                    comp: comp(-18.0, 2.5, 10.0, 25.0, 180.0, 2.0),
                    de_esser_on: true,
                    de_esser: de_esser(7000.0, -28.0, 5.0),
                    reverb_on: true,
                    reverb: reverb(0.4, 0.5, 0.13, 1.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Telephone",
                description: "Band-limited megaphone effect: hard low/high cuts and gritty saturated mids for intro or bridge FX.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -12.0,
                    low_mid: -4.0,
                    mid: 5.0,
                    high_mid: 6.0,
                    presence: 3.0,
                    high: -12.0,
                    comp_on: true,
                    comp: comp(-24.0, 8.0, 2.0, 3.0, 90.0, 8.0),
                    saturation_on: true,
                    saturation: saturation(20.0, 0.65),
                    limiter_on: true,
                    limiter: limiter(-1.0, 60.0),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Vintage Slap",
                description: "Rockabilly slap-back echo: short single repeat plus light saturation for a retro vibe.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.5,
                    low_mid: 0.0,
                    mid: 1.0,
                    high_mid: 2.0,
                    presence: 2.0,
                    high: -1.5,
                    comp_on: true,
                    comp: comp(-16.0, 4.0, 6.0, 8.0, 110.0, 4.0),
                    delay_on: true,
                    delay: DelaySettings {
                        time_ms: 115.0,
                        feedback: 0.12,
                        mix: 0.26,
                    },
                    saturation_on: true,
                    saturation: saturation(10.0, 0.35),
                    ..Chain::default()
                }),
            },
            FxPreset {
                name: "Podcast",
                description: "Spoken-word chain: gated, de-essed, firmly leveled and present for intelligibility, no reverb.",
                fx: fx(Chain {
                    eq_on: true,
                    low: -1.5,
                    low_mid: -2.0,
                    mid: 0.0,
                    high_mid: 1.5,
                    presence: 2.5,
                    high: 1.0,
                    comp_on: true,
                    comp: comp(-22.0, 4.0, 8.0, 8.0, 120.0, 5.0),
                    de_esser_on: true,
                    de_esser: de_esser(6500.0, -30.0, 8.0),
                    gate_on: true,
                    gate: GateSettings {
                        threshold_db: -45.0,
                        range_db: 30.0,
                        attack_ms: 2.0,
                        release_ms: 150.0,
                    },
                    limiter_on: true,
                    limiter: limiter(-1.0, 100.0),
                    ..Chain::default()
                }),
            },
        ]
    }

    /// Shared reverb presets.
    pub fn reverb() -> Vec<ReverbPreset> {
        let d = ReverbSettings::DEFAULT;
        vec![
            ReverbPreset {
                name: "Dry",
                reverb: ReverbSettings { wet: 0.0, ..d },
            },
            ReverbPreset {
                name: "Room",
                reverb: ReverbSettings {
                    room_size: 0.4,
                    damping: 0.6,
                    wet: 0.2,
                    ..d
                },
            },
            ReverbPreset {
                name: "Hall",
                reverb: ReverbSettings {
                    room_size: 0.85,
                    damping: 0.3,
                    wet: 0.35,
                    ..d
                },
            },
            ReverbPreset {
                name: "Karaoke",
                reverb: ReverbSettings {
                    room_size: 0.6,
                    damping: 0.4,
                    wet: 0.3,
                    ..d
                },
            },
        ]
    }
}
