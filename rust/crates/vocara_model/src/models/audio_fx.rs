//! The vocal channel's insert chain and the shared reverb bus (port of
//! `lib/domain/models/audio_fx.dart`).
//!
//! Every `from_json` clamps its values into the ranges the editors (and the
//! engine) accept, as the Dart factories do, so a synced, migrated or
//! hand-edited value never reaches the engine verbatim. A field of the wrong
//! JSON type fails the parse ([`crate::Error::Type`]), where Dart's cast
//! throws.

use serde_json::{json, Value};

use super::json::{as_map, double, field, opt_bool, opt_list, opt_num, opt_str, JsonMap};
use crate::util::dart_num::clamp_f64;
use crate::Result;

/// An insert plugin type in the vocal channel's effect chain. The enum INDEX is
/// the engine's chain tag (must stay in sync with the Rust `FxKind` mapping); the
/// list order in [`TrackFx::order`] is the signal-chain order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FxKind {
    Eq,
    Comp,
    DeEsser,
    Gate,
    Delay,
    Saturation,
    Doubler,
    Limiter,
    Reverb,
}

impl FxKind {
    /// `FxKind.values`, in declaration (default chain) order.
    pub const VALUES: [FxKind; 9] = [
        FxKind::Eq,
        FxKind::Comp,
        FxKind::DeEsser,
        FxKind::Gate,
        FxKind::Delay,
        FxKind::Saturation,
        FxKind::Doubler,
        FxKind::Limiter,
        FxKind::Reverb,
    ];

    /// The Dart enum name, as stored in `order`.
    pub fn name(self) -> &'static str {
        match self {
            FxKind::Eq => "eq",
            FxKind::Comp => "comp",
            FxKind::DeEsser => "deEsser",
            FxKind::Gate => "gate",
            FxKind::Delay => "delay",
            FxKind::Saturation => "saturation",
            FxKind::Doubler => "doubler",
            FxKind::Limiter => "limiter",
            FxKind::Reverb => "reverb",
        }
    }

    /// The Dart enum index: the engine's chain tag.
    pub fn index(self) -> usize {
        self as usize
    }

    /// `FxKind.values.asNameMap()[name]`.
    pub fn from_name(name: &str) -> Option<FxKind> {
        FxKind::VALUES.into_iter().find(|k| k.name() == name)
    }
}

/// The shape of an EQ band. Fixed per slot (matching the engine's default 6-band
/// strip): a low shelf, four peaks, a high shelf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EqBandKind {
    LowShelf,
    Peaking,
    HighShelf,
}

impl EqBandKind {
    pub const VALUES: [EqBandKind; 3] = [
        EqBandKind::LowShelf,
        EqBandKind::Peaking,
        EqBandKind::HighShelf,
    ];

    /// The Dart enum name, as stored in a band's `kind`.
    pub fn name(self) -> &'static str {
        match self {
            EqBandKind::LowShelf => "lowShelf",
            EqBandKind::Peaking => "peaking",
            EqBandKind::HighShelf => "highShelf",
        }
    }
}

/// One parametric EQ band. `freq` in Hz, `gain_db` boost/cut, `q` bandwidth.
/// (Dart's `copyWith` is struct update syntax; `kind` is fixed per slot.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub kind: EqBandKind,
    pub freq: f64,
    pub q: f64,
    pub gain_db: f64,
    pub enabled: bool,
}

impl EqBand {
    /// `EqBand(kind:, freq:)` with the defaults `q = 1.0`, `gainDb = 0.0`,
    /// `enabled = true`.
    pub const fn new(kind: EqBandKind, freq: f64) -> EqBand {
        EqBand {
            kind,
            freq,
            q: 1.0,
            gain_db: 0.0,
            enabled: true,
        }
    }

    /// [`EqBand::new`] with [q].
    pub const fn with_q(kind: EqBandKind, freq: f64, q: f64) -> EqBand {
        EqBand {
            kind,
            freq,
            q,
            gain_db: 0.0,
            enabled: true,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.name(),
            "freq": double(self.freq),
            "q": double(self.q),
            "gainDb": double(self.gain_db),
            "enabled": self.enabled,
        })
    }

    /// [slot_kind] is the kind the band's slot holds, when the list it came in
    /// is the fixed 6-slot layout: a band with no (or an unknown) kind takes
    /// it. Read as a peak, a low shelf missing its kind broke the layout check
    /// and shifted every band one slot over (see `TrackFx::normalize_eq`).
    pub fn from_json(j: &Value, slot_kind: Option<EqBandKind>) -> Result<EqBand> {
        let j = as_map(j, "EqBand")?;
        let kind = match field(j, "kind") {
            Some(Value::String(name)) => EqBandKind::VALUES
                .into_iter()
                .find(|k| k.name() == name)
                .unwrap_or(slot_kind.unwrap_or(EqBandKind::Peaking)),
            _ => slot_kind.unwrap_or(EqBandKind::Peaking),
        };
        // Parse freq defensively like every other numeric field: a malformed /
        // missing freq must not throw and fail the whole practice load. Fall back to
        // the canonical default frequency for this band kind (matches FLAT_BANDS).
        let default_freq = match kind {
            EqBandKind::LowShelf => 100.0,
            EqBandKind::Peaking => 1000.0,
            EqBandKind::HighShelf => 12000.0,
        };
        Ok(EqBand {
            kind,
            // Clamp on load (see ReverbSettings): freq to the audible band the graph
            // edits (the engine clamps again to its own Nyquist, and the panel clamps
            // for drawing — this keeps the MODEL from carrying a value neither of
            // them would agree to), q to the EQ graph's scroll-edit range (0.3..14 in
            // fx_panel.dart) and gainDb to the graph's editable ±15 dB (_eqMaxDb) —
            // a shelf amplitude of 10^(gainDb/40) with an absurd stored gain would
            // otherwise blow the engine's filter up.
            freq: clamp_f64(opt_num(j, "freq")?.unwrap_or(default_freq), 10.0, 24000.0),
            q: clamp_f64(opt_num(j, "q")?.unwrap_or(1.0), 0.3, 14.0),
            gain_db: clamp_f64(opt_num(j, "gainDb")?.unwrap_or(0.0), -15.0, 15.0),
            enabled: opt_bool(j, "enabled")?.unwrap_or(true),
        })
    }
}

/// Feed-forward compressor settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompSettings {
    pub threshold_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub makeup_db: f64,
}

impl CompSettings {
    pub const DEFAULT: CompSettings = CompSettings {
        threshold_db: -18.0,
        ratio: 3.0,
        knee_db: 6.0,
        attack_ms: 10.0,
        release_ms: 120.0,
        makeup_db: 0.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "thresholdDb": double(self.threshold_db),
            "ratio": double(self.ratio),
            "kneeDb": double(self.knee_db),
            "attackMs": double(self.attack_ms),
            "releaseMs": double(self.release_ms),
            "makeupDb": double(self.makeup_db),
        })
    }

    // Clamp on load (like ReverbSettings): a synced / migrated / hand-edited value
    // outside the editable range must not reach the engine verbatim (ratio < 1 is
    // invalid; absurd times destabilize the envelope follower). Threshold / ratio /
    // makeup bounds mirror the panel sliders in fx_panel.dart; knee / attack /
    // release have no editor, so they keep engine-safe bounds.
    pub fn from_json(j: &Value) -> Result<CompSettings> {
        let j = as_map(j, "CompSettings")?;
        Ok(CompSettings {
            threshold_db: clamp_f64(opt_num(j, "thresholdDb")?.unwrap_or(-18.0), -48.0, 0.0),
            ratio: clamp_f64(opt_num(j, "ratio")?.unwrap_or(3.0), 1.0, 12.0),
            knee_db: clamp_f64(opt_num(j, "kneeDb")?.unwrap_or(6.0), 0.0, 24.0),
            attack_ms: clamp_f64(opt_num(j, "attackMs")?.unwrap_or(10.0), 0.1, 500.0),
            release_ms: clamp_f64(opt_num(j, "releaseMs")?.unwrap_or(120.0), 1.0, 5000.0),
            makeup_db: clamp_f64(opt_num(j, "makeupDb")?.unwrap_or(0.0), 0.0, 18.0),
        })
    }
}

impl Default for CompSettings {
    fn default() -> Self {
        CompSettings::DEFAULT
    }
}

/// De-esser: split-band sibilance reducer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeEsserSettings {
    pub freq: f64,
    pub threshold_db: f64,
    pub range_db: f64,
}

impl DeEsserSettings {
    pub const DEFAULT: DeEsserSettings = DeEsserSettings {
        freq: 6500.0,
        threshold_db: -28.0,
        range_db: 12.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "freq": double(self.freq),
            "thresholdDb": double(self.threshold_db),
            "rangeDb": double(self.range_db),
        })
    }

    // Clamp on load (see ReverbSettings): keep a migrated / hand-edited value in
    // the editable range before it reaches the engine.
    pub fn from_json(j: &Value) -> Result<DeEsserSettings> {
        let j = as_map(j, "DeEsserSettings")?;
        Ok(DeEsserSettings {
            freq: clamp_f64(opt_num(j, "freq")?.unwrap_or(6500.0), 2000.0, 16000.0),
            threshold_db: clamp_f64(opt_num(j, "thresholdDb")?.unwrap_or(-28.0), -60.0, 0.0),
            range_db: clamp_f64(opt_num(j, "rangeDb")?.unwrap_or(12.0), 0.0, 24.0),
        })
    }
}

impl Default for DeEsserSettings {
    fn default() -> Self {
        DeEsserSettings::DEFAULT
    }
}

/// Noise gate / downward expander.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateSettings {
    pub threshold_db: f64,
    pub range_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
}

impl GateSettings {
    pub const DEFAULT: GateSettings = GateSettings {
        threshold_db: -45.0,
        range_db: 60.0,
        attack_ms: 2.0,
        release_ms: 120.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "thresholdDb": double(self.threshold_db),
            "rangeDb": double(self.range_db),
            "attackMs": double(self.attack_ms),
            "releaseMs": double(self.release_ms),
        })
    }

    // Clamp on load (see ReverbSettings): keep a migrated / hand-edited value in
    // the editable range before it reaches the engine.
    pub fn from_json(j: &Value) -> Result<GateSettings> {
        let j = as_map(j, "GateSettings")?;
        Ok(GateSettings {
            threshold_db: clamp_f64(opt_num(j, "thresholdDb")?.unwrap_or(-45.0), -80.0, 0.0),
            range_db: clamp_f64(opt_num(j, "rangeDb")?.unwrap_or(60.0), 0.0, 80.0),
            attack_ms: clamp_f64(opt_num(j, "attackMs")?.unwrap_or(2.0), 0.1, 50.0),
            release_ms: clamp_f64(opt_num(j, "releaseMs")?.unwrap_or(120.0), 10.0, 500.0),
        })
    }
}

impl Default for GateSettings {
    fn default() -> Self {
        GateSettings::DEFAULT
    }
}

/// Feedback delay (echo / throw).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DelaySettings {
    pub time_ms: f64,
    pub feedback: f64,
    pub mix: f64,
}

impl DelaySettings {
    pub const DEFAULT: DelaySettings = DelaySettings {
        time_ms: 300.0,
        feedback: 0.3,
        mix: 0.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "timeMs": double(self.time_ms),
            "feedback": double(self.feedback),
            "mix": double(self.mix),
        })
    }

    // Clamp on load (see ReverbSettings): feedback >= 1.0 self-oscillates and
    // blows the delay line up, so a synced / migrated / hand-edited value must
    // never reach the engine verbatim.
    pub fn from_json(j: &Value) -> Result<DelaySettings> {
        let j = as_map(j, "DelaySettings")?;
        Ok(DelaySettings {
            time_ms: clamp_f64(opt_num(j, "timeMs")?.unwrap_or(300.0), 1.0, 1500.0),
            feedback: clamp_f64(opt_num(j, "feedback")?.unwrap_or(0.3), 0.0, 0.95),
            mix: clamp_f64(opt_num(j, "mix")?.unwrap_or(0.0), 0.0, 1.0),
        })
    }
}

impl Default for DelaySettings {
    fn default() -> Self {
        DelaySettings::DEFAULT
    }
}

/// Soft saturation / harmonic exciter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SaturationSettings {
    pub drive_db: f64,
    pub mix: f64,
}

impl SaturationSettings {
    pub const DEFAULT: SaturationSettings = SaturationSettings {
        drive_db: 6.0,
        mix: 0.0,
    };

    pub fn to_json(&self) -> Value {
        json!({"driveDb": double(self.drive_db), "mix": double(self.mix)})
    }

    // Clamp on load (see ReverbSettings): keep a migrated / hand-edited value
    // in the editable range before it reaches the engine.
    pub fn from_json(j: &Value) -> Result<SaturationSettings> {
        let j = as_map(j, "SaturationSettings")?;
        Ok(SaturationSettings {
            drive_db: clamp_f64(opt_num(j, "driveDb")?.unwrap_or(6.0), 0.0, 36.0),
            mix: clamp_f64(opt_num(j, "mix")?.unwrap_or(0.0), 0.0, 1.0),
        })
    }
}

impl Default for SaturationSettings {
    fn default() -> Self {
        SaturationSettings::DEFAULT
    }
}

/// Vocal doubler / chorus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoublerSettings {
    pub delay_ms: f64,
    pub depth_ms: f64,
    pub rate_hz: f64,
    pub mix: f64,
}

impl DoublerSettings {
    pub const DEFAULT: DoublerSettings = DoublerSettings {
        delay_ms: 18.0,
        depth_ms: 4.0,
        rate_hz: 0.6,
        mix: 0.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "delayMs": double(self.delay_ms),
            "depthMs": double(self.depth_ms),
            "rateHz": double(self.rate_hz),
            "mix": double(self.mix),
        })
    }

    // Clamp on load (see ReverbSettings): keep a migrated / hand-edited value in
    // the editable range before it reaches the engine.
    pub fn from_json(j: &Value) -> Result<DoublerSettings> {
        let j = as_map(j, "DoublerSettings")?;
        Ok(DoublerSettings {
            delay_ms: clamp_f64(opt_num(j, "delayMs")?.unwrap_or(18.0), 1.0, 50.0),
            depth_ms: clamp_f64(opt_num(j, "depthMs")?.unwrap_or(4.0), 0.0, 20.0),
            rate_hz: clamp_f64(opt_num(j, "rateHz")?.unwrap_or(0.6), 0.05, 8.0),
            mix: clamp_f64(opt_num(j, "mix")?.unwrap_or(0.0), 0.0, 1.0),
        })
    }
}

impl Default for DoublerSettings {
    fn default() -> Self {
        DoublerSettings::DEFAULT
    }
}

/// Brick-wall peak limiter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterSettings {
    pub ceiling_db: f64,
    pub release_ms: f64,
}

impl LimiterSettings {
    pub const DEFAULT: LimiterSettings = LimiterSettings {
        ceiling_db: -0.3,
        release_ms: 80.0,
    };

    pub fn to_json(&self) -> Value {
        json!({"ceilingDb": double(self.ceiling_db), "releaseMs": double(self.release_ms)})
    }

    // Clamp on load (see ReverbSettings): keep a migrated / hand-edited value in
    // the editable range before it reaches the engine.
    pub fn from_json(j: &Value) -> Result<LimiterSettings> {
        let j = as_map(j, "LimiterSettings")?;
        Ok(LimiterSettings {
            ceiling_db: clamp_f64(opt_num(j, "ceilingDb")?.unwrap_or(-0.3), -12.0, 0.0),
            release_ms: clamp_f64(opt_num(j, "releaseMs")?.unwrap_or(80.0), 1.0, 500.0),
        })
    }
}

impl Default for LimiterSettings {
    fn default() -> Self {
        LimiterSettings::DEFAULT
    }
}

/// A post-fader aux send: a copy of the channel, scaled by [level_db], routed to
/// the FX bus [bus_id] — independent of the channel's main output/fader.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSend {
    pub bus_id: String,
    pub level_db: f64,
}

impl TrackSend {
    /// `TrackSend(busId:)` at the default 0 dB.
    pub fn new(bus_id: impl Into<String>, level_db: f64) -> TrackSend {
        TrackSend {
            bus_id: bus_id.into(),
            level_db,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({"busId": self.bus_id, "levelDb": double(self.level_db)})
    }

    pub fn from_json(j: &Value) -> Result<TrackSend> {
        let j = as_map(j, "TrackSend")?;
        Ok(TrackSend {
            // Guard against a null/missing busId (partial or hand-edited JSON): an empty
            // id resolves to no bus (routes to master) instead of throwing on load.
            bus_id: opt_str(j, "busId")?.unwrap_or_default(),
            // Clamp on load (see TrackFx.sendDb): a send is a dB level in
            // [SEND_OFF_DB, 0]; a hand-edited positive value would over-drive the bus.
            level_db: clamp_f64(opt_num(j, "levelDb")?.unwrap_or(0.0), SEND_OFF_DB, 0.0),
        })
    }
}

/// Max post-fader aux sends per channel (matches the engine's `MAX_SENDS`).
pub const MAX_SENDS: usize = 4;

/// Reverb send level meaning "off" — below this is silent.
pub const SEND_OFF_DB: f64 = -120.0;

/// The default flat 6-band strip (low shelf, four peaks, high shelf).
/// Frequencies match the engine's `EqNode::default_strip`.
pub const FLAT_BANDS: [EqBand; 6] = [
    EqBand::with_q(EqBandKind::LowShelf, 100.0, 0.7),
    EqBand::with_q(EqBandKind::Peaking, 250.0, 1.0),
    EqBand::with_q(EqBandKind::Peaking, 700.0, 1.0),
    EqBand::with_q(EqBandKind::Peaking, 2000.0, 1.0),
    EqBand::with_q(EqBandKind::Peaking, 6000.0, 1.0),
    EqBand::with_q(EqBandKind::HighShelf, 12000.0, 0.7),
];

/// Per-(vocal-)track effect chain: the inserts in [order], each with its own
/// on-flag, plus a reverb send level. (Dart's `copyWith` is struct update
/// syntax.)
#[derive(Clone, Debug, PartialEq)]
pub struct TrackFx {
    pub eq: Vec<EqBand>,
    pub eq_on: bool,
    pub comp: CompSettings,
    pub comp_on: bool,
    pub de_esser: DeEsserSettings,
    pub de_esser_on: bool,
    pub gate: GateSettings,
    pub gate_on: bool,
    pub delay: DelaySettings,
    pub delay_on: bool,
    pub saturation: SaturationSettings,
    pub saturation_on: bool,
    pub doubler: DoublerSettings,
    pub doubler_on: bool,
    pub limiter: LimiterSettings,
    pub limiter_on: bool,
    pub reverb: ReverbSettings,
    pub reverb_on: bool,
    pub send_db: f64,
    /// The insert plugins in signal-chain order (Source → order[0] → … → fader).
    /// Reordering changes the audible chain; on/off is per-plugin (eqOn/compOn).
    pub order: Vec<FxKind>,
}

impl TrackFx {
    /// `TrackFx(eq:)` with every plugin off at its defaults, the send off and
    /// the default chain order.
    pub fn new(eq: Vec<EqBand>) -> TrackFx {
        TrackFx {
            eq,
            eq_on: false,
            comp: CompSettings::DEFAULT,
            comp_on: false,
            de_esser: DeEsserSettings::DEFAULT,
            de_esser_on: false,
            gate: GateSettings::DEFAULT,
            gate_on: false,
            delay: DelaySettings::DEFAULT,
            delay_on: false,
            saturation: SaturationSettings::DEFAULT,
            saturation_on: false,
            doubler: DoublerSettings::DEFAULT,
            doubler_on: false,
            limiter: LimiterSettings::DEFAULT,
            limiter_on: false,
            reverb: ReverbSettings::DEFAULT,
            reverb_on: false,
            send_db: SEND_OFF_DB,
            order: FxKind::VALUES.to_vec(),
        }
    }

    /// The default flat 6-band strip (low shelf, four peaks, high shelf),
    /// everything off. Frequencies match the engine's `EqNode::default_strip`.
    pub fn flat() -> TrackFx {
        TrackFx::new(FLAT_BANDS.to_vec())
    }

    /// Normalizes a saved EQ to the fixed 6-slot layout the engine expects (band
    /// kind is fixed per slot — `ve_set_eq_band` never sends a kind). Legacy
    /// 4-band takes are remapped by role so the displayed curve keeps matching the
    /// sound: the low/high shelves and peaks land on like-kind slots; unfilled
    /// slots stay flat at their default frequency. A take already in the canonical
    /// layout is returned untouched.
    fn normalize_eq(saved: Vec<EqBand>) -> Vec<EqBand> {
        if saved.len() == FLAT_BANDS.len()
            && saved
                .iter()
                .zip(FLAT_BANDS.iter())
                .all(|(s, f)| s.kind == f.kind)
        {
            return saved;
        }
        let mut low_shelf = saved.iter().filter(|b| b.kind == EqBandKind::LowShelf);
        let mut high_shelf = saved.iter().filter(|b| b.kind == EqBandKind::HighShelf);
        let peaks: Vec<&EqBand> = saved
            .iter()
            .filter(|b| b.kind == EqBandKind::Peaking)
            .collect();
        let mut peak = 0;
        FLAT_BANDS
            .iter()
            .map(|slot| match slot.kind {
                EqBandKind::LowShelf => low_shelf.next().copied().unwrap_or(*slot),
                EqBandKind::HighShelf => high_shelf.next().copied().unwrap_or(*slot),
                EqBandKind::Peaking if peak < peaks.len() => {
                    peak += 1;
                    *peaks[peak - 1]
                }
                EqBandKind::Peaking => *slot,
            })
            .collect()
    }

    pub fn has_reverb_send(&self) -> bool {
        self.send_db > SEND_OFF_DB
    }

    /// Normalizes a chain order: de-dupes, drops unknown kinds, and restores
    /// any known kind missing from a saved order (so new plugin types always
    /// appear) right after the last kind the default chain puts before it.
    /// Appended, a de-esser missing from an old order ran after the reverb.
    fn normalize_order(parsed: Vec<FxKind>) -> Vec<FxKind> {
        let mut out: Vec<FxKind> = Vec::new();
        for k in parsed {
            if !out.contains(&k) {
                out.push(k);
            }
        }
        for k in FxKind::VALUES {
            if out.contains(&k) {
                continue;
            }
            let mut at = 0;
            for (i, o) in out.iter().enumerate() {
                if o.index() < k.index() {
                    at = i + 1;
                }
            }
            out.insert(at, k);
        }
        out
    }

    /// Replace a single EQ band by index (out of range: unchanged).
    pub fn with_band(&self, index: usize, band: EqBand) -> TrackFx {
        let mut next = self.clone();
        if let Some(slot) = next.eq.get_mut(index) {
            *slot = band;
        }
        next
    }

    pub fn to_json(&self) -> Value {
        json!({
            "eq": self.eq.iter().map(EqBand::to_json).collect::<Vec<_>>(),
            "eqOn": self.eq_on,
            "comp": self.comp.to_json(),
            "compOn": self.comp_on,
            "deEsser": self.de_esser.to_json(),
            "deEsserOn": self.de_esser_on,
            "gate": self.gate.to_json(),
            "gateOn": self.gate_on,
            "delay": self.delay.to_json(),
            "delayOn": self.delay_on,
            "saturation": self.saturation.to_json(),
            "saturationOn": self.saturation_on,
            "doubler": self.doubler.to_json(),
            "doublerOn": self.doubler_on,
            "limiter": self.limiter.to_json(),
            "limiterOn": self.limiter_on,
            "reverb": self.reverb.to_json(),
            "reverbOn": self.reverb_on,
            "sendDb": double(self.send_db),
            "order": self.order.iter().map(|k| k.name()).collect::<Vec<_>>(),
        })
    }

    /// `_sub`: a nested settings map, or the default when the key holds
    /// anything but a map.
    fn sub<T>(j: &JsonMap, key: &str, parse: fn(&Value) -> Result<T>, fallback: T) -> Result<T> {
        match j.get(key) {
            Some(v @ Value::Object(_)) => parse(v),
            _ => Ok(fallback),
        }
    }

    pub fn from_json(j: &Value) -> Result<TrackFx> {
        let j = as_map(j, "TrackFx")?;
        let eq_json = opt_list(j, "eq")?;
        let canonical = eq_json.is_some_and(|e| e.len() == FLAT_BANDS.len());
        let eq = match eq_json {
            Some(bands) if !bands.is_empty() => {
                let mut parsed = Vec::with_capacity(bands.len());
                for (i, b) in bands.iter().enumerate() {
                    let slot_kind = if canonical {
                        Some(FLAT_BANDS[i].kind)
                    } else {
                        None
                    };
                    parsed.push(EqBand::from_json(b, slot_kind)?);
                }
                TrackFx::normalize_eq(parsed)
            }
            _ => FLAT_BANDS.to_vec(),
        };
        // Skip unknown kind names (e.g. a plugin added by a newer schema) instead
        // of aliasing them to eq: normalize_order must genuinely drop them, and its
        // restore pass puts any absent known kind back after its default
        // predecessor.
        let order = TrackFx::normalize_order(
            opt_list(j, "order")?
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|e| e.as_str().and_then(FxKind::from_name))
                        .collect()
                })
                .unwrap_or_default(),
        );
        Ok(TrackFx {
            eq,
            eq_on: opt_bool(j, "eqOn")?.unwrap_or(false),
            comp: TrackFx::sub(j, "comp", CompSettings::from_json, CompSettings::DEFAULT)?,
            comp_on: opt_bool(j, "compOn")?.unwrap_or(false),
            de_esser: TrackFx::sub(
                j,
                "deEsser",
                DeEsserSettings::from_json,
                DeEsserSettings::DEFAULT,
            )?,
            de_esser_on: opt_bool(j, "deEsserOn")?.unwrap_or(false),
            gate: TrackFx::sub(j, "gate", GateSettings::from_json, GateSettings::DEFAULT)?,
            gate_on: opt_bool(j, "gateOn")?.unwrap_or(false),
            delay: TrackFx::sub(j, "delay", DelaySettings::from_json, DelaySettings::DEFAULT)?,
            delay_on: opt_bool(j, "delayOn")?.unwrap_or(false),
            saturation: TrackFx::sub(
                j,
                "saturation",
                SaturationSettings::from_json,
                SaturationSettings::DEFAULT,
            )?,
            saturation_on: opt_bool(j, "saturationOn")?.unwrap_or(false),
            doubler: TrackFx::sub(
                j,
                "doubler",
                DoublerSettings::from_json,
                DoublerSettings::DEFAULT,
            )?,
            doubler_on: opt_bool(j, "doublerOn")?.unwrap_or(false),
            limiter: TrackFx::sub(
                j,
                "limiter",
                LimiterSettings::from_json,
                LimiterSettings::DEFAULT,
            )?,
            limiter_on: opt_bool(j, "limiterOn")?.unwrap_or(false),
            reverb: TrackFx::sub(
                j,
                "reverb",
                ReverbSettings::from_json,
                ReverbSettings::DEFAULT,
            )?,
            reverb_on: opt_bool(j, "reverbOn")?.unwrap_or(false),
            // Clamp on load (see ReverbSettings): the reverb send is a dB level in
            // [SEND_OFF_DB, 0]; a hand-edited positive value would over-drive the bus.
            send_db: clamp_f64(
                opt_num(j, "sendDb")?.unwrap_or(SEND_OFF_DB),
                SEND_OFF_DB,
                0.0,
            ),
            order,
        })
    }
}

/// The one shared reverb bus (Freeverb). `wet`/`width` 0..1; tracks feed it via their send.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbSettings {
    pub room_size: f64,
    pub damping: f64,
    pub wet: f64,
    pub width: f64,
}

impl ReverbSettings {
    pub const DEFAULT: ReverbSettings = ReverbSettings {
        room_size: 0.5,
        damping: 0.5,
        wet: 0.25,
        width: 1.0,
    };

    pub fn to_json(&self) -> Value {
        json!({
            "roomSize": double(self.room_size),
            "damping": double(self.damping),
            "wet": double(self.wet),
            "width": double(self.width),
        })
    }

    pub fn from_json(j: &Value) -> Result<ReverbSettings> {
        let j = as_map(j, "ReverbSettings")?;
        Ok(ReverbSettings {
            // Clamp on load: a synced / migrated / hand-edited value outside [0,1] would otherwise
            // push the engine reverb's comb feedback unstable (the engine clamps too, but keep the
            // model itself sane so the UI and persistence never round-trip a bad value).
            room_size: clamp_f64(opt_num(j, "roomSize")?.unwrap_or(0.5), 0.0, 1.0),
            damping: clamp_f64(opt_num(j, "damping")?.unwrap_or(0.5), 0.0, 1.0),
            wet: clamp_f64(opt_num(j, "wet")?.unwrap_or(0.25), 0.0, 1.0),
            width: clamp_f64(opt_num(j, "width")?.unwrap_or(1.0), 0.0, 1.0),
        })
    }
}

impl Default for ReverbSettings {
    fn default() -> Self {
        ReverbSettings::DEFAULT
    }
}
