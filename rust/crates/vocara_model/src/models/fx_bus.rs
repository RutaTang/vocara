//! A DAW-style FX bus (port of `lib/domain/models/fx_bus.dart`).

use serde_json::{json, Value};

use super::audio_fx::{TrackFx, TrackSend};
use super::json::{
    as_map, double, field, list_of, nullable, opt_bool, opt_int, opt_num, opt_str, req_str,
};
use crate::util::dart_num::clamp_f64;
use crate::Result;

/// A DAW-style FX bus (group / aux track): a named mixer strip with its own
/// EQ → Comp insert chain, fader, mute, solo, and reverb send. Vocal takes route
/// their output into a bus to share processing; a bus itself routes to the
/// master or into another bus. Backing tracks never route to a bus (stay dry).
/// (Dart's `copyWith` is struct update syntax; `clearColor` / `clearOutput`
/// are `color_value: None` / `output_bus_id: None`.)
#[derive(Clone, Debug, PartialEq)]
pub struct FxBus {
    pub id: String,
    pub name: String,
    /// The bus's insert chain (EQ + compressor) plus its reverb send level —
    /// reuses [`TrackFx`] verbatim (a bus is just a channel strip with no source).
    pub fx: TrackFx,
    /// Optional ARGB tint, like a take's color.
    pub color_value: Option<i64>,
    /// Bus fader (linear, 0..1+).
    pub volume: f64,
    pub muted: bool,
    /// When any track or bus is soloed, only soloed sources (and the routing
    /// chain that carries them to the master) are heard. Soloing a bus keeps the
    /// takes feeding it audible; see the ViewModel's solo resolution.
    pub soloed: bool,
    /// Stereo pan in [-1, 1] (0 = center).
    pub pan: f64,
    /// Output routing: another bus's id, or `None` = straight to the master.
    pub output_bus_id: Option<String>,
    /// Post-fader aux sends to other FX buses (independent of the main output).
    pub sends: Vec<TrackSend>,
}

impl FxBus {
    /// `FxBus(id:, name:, fx:)` with the defaults: no tint, unity, unmuted,
    /// unsoloed, centered, to the master, no sends.
    pub fn new(id: impl Into<String>, name: impl Into<String>, fx: TrackFx) -> FxBus {
        FxBus {
            id: id.into(),
            name: name.into(),
            fx,
            color_value: None,
            volume: 1.0,
            muted: false,
            soloed: false,
            pan: 0.0,
            output_bus_id: None,
            sends: Vec::new(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "fx": self.fx.to_json(),
            "colorValue": nullable(self.color_value),
            "volume": double(self.volume),
            "muted": self.muted,
            "soloed": self.soloed,
            "pan": double(self.pan),
            "outputBusId": nullable(self.output_bus_id.clone()),
            "sends": self.sends.iter().map(TrackSend::to_json).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(json: &Value) -> Result<FxBus> {
        let j = as_map(json, "FxBus")?;
        Ok(FxBus {
            id: req_str(j, "id")?,
            name: opt_str(j, "name")?.unwrap_or_else(|| "Bus".to_string()),
            fx: match field(j, "fx") {
                Some(fx) => TrackFx::from_json(fx)?,
                None => TrackFx::flat(),
            },
            color_value: opt_int(j, "colorValue")?,
            // Within the fader's reach, as a track's (see AudioTrack::from_json).
            volume: clamp_f64(opt_num(j, "volume")?.unwrap_or(1.0), 0.0, 1.0),
            muted: opt_bool(j, "muted")?.unwrap_or(false),
            soloed: opt_bool(j, "soloed")?.unwrap_or(false),
            pan: clamp_f64(opt_num(j, "pan")?.unwrap_or(0.0), -1.0, 1.0),
            output_bus_id: opt_str(j, "outputBusId")?,
            sends: list_of(j, "sends", TrackSend::from_json)?,
        })
    }
}
