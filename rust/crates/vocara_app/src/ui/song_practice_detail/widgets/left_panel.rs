//! Port of `ui/song_practice_detail/widgets/left_panel.dart`: the left
//! inspector — a Logic-style channel strip for the selected take or FX bus,
//! collapsible to a thin rail — and [`voice_summary_view`], the take's
//! voice-analysis summary the bottom dock's "Analysis Summary" tab hosts.
//!
//! The inspector is drawn as the approved workspace mockup draws it
//! (`DESIGN.md`): a 36 px "Inspector" header with its hide button; the
//! channel's identity block (a colour square, the name, its meta); the FX
//! panel (its preset row, then the effect modules); the routing (a "Send"
//! row per aux send with its level, an empty slot that adds one, and
//! "Output", each a pop-up button); and the channel footer pinned at the
//! bottom (M, S, the pan knob, the volume as a thin slider, its dB).
//!
//! Plain data in ([`Inspector`], [`VoiceSummary`]: what the Dart widgets
//! read off the detail view model, its mixer and its interpretations),
//! [`Message`]s out, each named after the view-model or controller method
//! the Dart widget called, for the screen to route.
//!
//! How the Dart structure maps:
//!
//! - The FX panel (`fx_panel.dart`, ported separately) is a slot:
//!   [`left_panel`] takes a function building the shown channel's
//!   `FxPanel(vm, track: ...)` / `FxPanel(vm, bus: ...)` as an element of the
//!   screen's message type, and calls it only when the strip shows one.
//! - `ValueKey('take:<id>')` / `ValueKey('bus:<id>')` on the body: the body
//!   is keyed by what it shows, so another channel selected under a held
//!   fader or pan knob (an import landing selects its take) starts with
//!   fresh widget state instead of handing the next take the drag; the FX
//!   panel inside starts fresh too (its EQ analyzer's smoothed spectrum,
//!   adaptive ceiling and latched "has signal" did not carry over).
//! - The post-frame `healSelectedAnalysis` in `LeftPanel.build`: a view
//!   cannot act, so [`Inspector::analysis_heal`] says what the screen asks
//!   for after each update while it holds.
//! - `showStudioMenu(anchor, menuAnchorBelow(anchor), ...)`: the screen holds
//!   which menu is open ([`Inspector::open_menu`]); the pop-up button that
//!   opens it is a [`menu_host`] anchored below it (each its own, so the
//!   menu drops under the button clicked). Picking a row reports its action;
//!   the screen closes the menu on every message but [`Message::OpenMenu`]
//!   ([`Message::closes_menu`]). A menu opened for one channel shows only
//!   over that channel's strip.
//! - `LayoutBuilder` (the degenerate-height fallback): a `responsive` body.
//! - `RepaintBoundary` / `StableSubtree`: no counterpart needed (see
//!   `stable_subtree`); the canvases cache their drawing.
//!
//! Where iced differs: the range line's baseline alignment is a bottom
//! alignment, and the rail's rotated label is drawn on a canvas.

use std::sync::Arc;

use crate::ui::core::text;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::text::{Alignment, Wrapping};
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, responsive, row, Column, Space,
};
use iced::{
    mouse, Background, Color, Element, Event, Length, Padding, Pixels, Point, Rectangle, Renderer,
    Size, Theme, Vector,
};
use vocara_model::models::audio_fx::{TrackSend, MAX_SENDS, SEND_OFF_DB};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::voice_metrics::{
    note_name_for_midi, Ltas, RegisterMap, Vibrato, VoiceMetrics, VoiceProfile,
};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::round_to_i64;

use super::pitch_lane::{note_vibrato_of, pitch_in_tune_percent, pitch_notes_of, PitchNote};
use super::track_context_menu::{
    menu_host, studio_menu, MenuAnchor, StudioMenuItem, MENU_ANCHOR_BELOW_DY,
};
use super::track_row::{bus_color, channel_color};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, boxed, fill, font, height, type_scale, Studio};
use crate::ui::core::widgets::cap_label::cap_label;
use crate::ui::core::widgets::controls::{
    icon_button, labelled, msr_button, popup_button, thin_slider, MsrKind, PARAMETER_LABEL_WIDTH,
    PARAMETER_VALUE_WIDTH,
};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::line_icon::{self, glyphs, line_icon_inherit};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::studio_controls::{fader_db, studio_pan_knob};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::song_practice_detail::view_model::analysis_cache::DockTab;
use crate::util::to_string_as_fixed;

#[cfg(test)]
mod tests;

/// The open inspector's width (`DESIGN.md`, Geometry: 264).
pub const PANEL_WIDTH: f32 = 264.0;

/// The collapsed rail's width.
pub const RAIL_WIDTH: f32 = 34.0;

/// Below this body height the pinned-footer layout cannot fit its fixed
/// chrome (take header + channel-strip footer) and would overflow; the strip
/// falls back to scrolling as one column instead. Generous on purpose: near
/// this height the FX list has no usable room anyway (`_kMinPinnedHeight`).
pub const MIN_PINNED_HEIGHT: f32 = 420.0;

/// The FX panel's fixed window inside the scrolling column (it needs a
/// bounded height), and the dry backing's message's.
const SCROLLED_FX_HEIGHT: f32 = 220.0;
const SCROLLED_DRY_HEIGHT: f32 = 120.0;

/// The send-target and output menus' widths.
const SEND_MENU_WIDTH: f32 = 160.0;
const OUTPUT_MENU_WIDTH: f32 = 180.0;

/// The output routing's "no bus" destination.
pub const STEREO_OUT: &str = "Stereo Out";

// Pass / warn / flag indicator colors for the scorecard dots (the status
// tokens: in tune, warn, off).
const GOOD: Color = Studio::GOOD;
const WARN: Color = Studio::WARN;
const FLAG: Color = Studio::OUT_OF_TUNE;

// Register split colors (warm chest → cool head).
const CHEST: Color = Studio::REGISTER_CHEST;
const HEAD: Color = Studio::REGISTER_HEAD;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `_fmtDate`: "Nov 23", in the date's own zone.
pub fn fmt_date(d: &DateTime) -> String {
    let p = d.parts();
    let month = MONTHS[(p.month.clamp(1, 12) - 1) as usize];
    format!("{month} {}", p.day)
}

// ---------------------------------------------------------------- messages

/// A mixer channel the inspector shows: a take or an FX bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Track(String),
    Bus(String),
}

/// Which of the strip's menus is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorMenu {
    /// The Output pop-up's destinations.
    Output(Channel),
    /// The empty send slot's buses: those no slot feeds yet.
    AddSend(Channel),
    /// A send slot's target pop-up: its own target plus the unused ones.
    RetargetSend(Channel, usize),
}

/// What the inspector and the voice summary ask of the detail view model.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The rail, or the header's collapse button (`vm.toggleInspectorPanel`).
    ToggleInspectorPanel,
    /// A pop-up button was pressed: open its menu (`showStudioMenu`).
    OpenMenu(InspectorMenu),
    /// A tap outside the open menu, or Escape.
    DismissMenu,
    /// The metrics the inspector shows are missing: compute them
    /// (`vm.analysis.healSelectedAnalysis(spectrogram: false, metrics:
    /// true)`; see [`Inspector::analysis_heal`]).
    HealSelectedAnalysis {
        spectrogram: Option<bool>,
        metrics: Option<bool>,
    },
    /// A summary cell, the range or the register bar
    /// (`vm.analysis.showDockTab`).
    ShowDockTab(DockTab),

    // A take's strip (`vm.mixer`).
    /// `toggleMute`.
    ToggleMute(String),
    /// `toggleSolo`.
    ToggleSolo(String),
    /// `setTrackPanLive`.
    SetTrackPanLive { track_id: String, pan: f64 },
    /// `setTrackVolumeLive`.
    SetTrackVolumeLive { track_id: String, volume: f64 },
    /// `commitTrackVolume`: the end of a take's pan or fader drag.
    CommitTrackVolume,
    /// `setTrackBus`: `None` is the stereo out.
    SetTrackBus {
        track_id: String,
        bus_id: Option<String>,
    },
    /// `addTrackSend`.
    AddTrackSend { track_id: String, bus_id: String },
    /// `removeTrackSend`.
    RemoveTrackSend { track_id: String, slot: usize },
    /// `setTrackSendTarget`.
    SetTrackSendTarget {
        track_id: String,
        slot: usize,
        bus_id: String,
    },
    /// `setTrackSendLevelLive`.
    SetTrackSendLevelLive {
        track_id: String,
        slot: usize,
        level_db: f64,
    },

    // A bus's strip (`vm.mixer`).
    /// `toggleBusMute`.
    ToggleBusMute(String),
    /// `toggleBusSolo`.
    ToggleBusSolo(String),
    /// `setBusPanLive`.
    SetBusPanLive { bus_id: String, pan: f64 },
    /// `setBusVolumeLive`.
    SetBusVolumeLive { bus_id: String, volume: f64 },
    /// `setBusOutput`: `None` is the stereo out.
    SetBusOutput {
        bus_id: String,
        output_bus_id: Option<String>,
    },
    /// `addBusSend`.
    AddBusSend {
        bus_id: String,
        target_bus_id: String,
    },
    /// `removeBusSend`.
    RemoveBusSend { bus_id: String, slot: usize },
    /// `setBusSendTarget`.
    SetBusSendTarget {
        bus_id: String,
        slot: usize,
        target_bus_id: String,
    },
    /// `setBusSendLevelLive`.
    SetBusSendLevelLive {
        bus_id: String,
        slot: usize,
        level_db: f64,
    },
    /// `vm.commitFx`: the end of a send-level drag (a take's or a bus's), or
    /// of a bus's pan or fader drag.
    CommitFx,
}

impl Message {
    /// Whether the screen closes an open menu on this message: on all but
    /// the one that opens a menu (a row picked, or the menu dismissed — while
    /// a menu is open nothing beneath it sees the pointer).
    pub fn closes_menu(&self) -> bool {
        !matches!(self, Message::OpenMenu(_))
    }
}

impl Channel {
    fn add_send(&self, bus_id: &str) -> Message {
        match self {
            Channel::Track(id) => Message::AddTrackSend {
                track_id: id.clone(),
                bus_id: bus_id.to_string(),
            },
            Channel::Bus(id) => Message::AddBusSend {
                bus_id: id.clone(),
                target_bus_id: bus_id.to_string(),
            },
        }
    }

    fn remove_send(&self, slot: usize) -> Message {
        match self {
            Channel::Track(id) => Message::RemoveTrackSend {
                track_id: id.clone(),
                slot,
            },
            Channel::Bus(id) => Message::RemoveBusSend {
                bus_id: id.clone(),
                slot,
            },
        }
    }

    fn retarget_send(&self, slot: usize, bus_id: &str) -> Message {
        match self {
            Channel::Track(id) => Message::SetTrackSendTarget {
                track_id: id.clone(),
                slot,
                bus_id: bus_id.to_string(),
            },
            Channel::Bus(id) => Message::SetBusSendTarget {
                bus_id: id.clone(),
                slot,
                target_bus_id: bus_id.to_string(),
            },
        }
    }

    fn send_level(&self, slot: usize, level_db: f64) -> Message {
        match self {
            Channel::Track(id) => Message::SetTrackSendLevelLive {
                track_id: id.clone(),
                slot,
                level_db,
            },
            Channel::Bus(id) => Message::SetBusSendLevelLive {
                bus_id: id.clone(),
                slot,
                level_db,
            },
        }
    }

    fn route(&self, dest: Option<&str>) -> Message {
        let dest = dest.map(str::to_string);
        match self {
            Channel::Track(id) => Message::SetTrackBus {
                track_id: id.clone(),
                bus_id: dest,
            },
            Channel::Bus(id) => Message::SetBusOutput {
                bus_id: id.clone(),
                output_bus_id: dest,
            },
        }
    }

    fn toggle_mute(&self) -> Message {
        match self {
            Channel::Track(id) => Message::ToggleMute(id.clone()),
            Channel::Bus(id) => Message::ToggleBusMute(id.clone()),
        }
    }

    fn toggle_solo(&self) -> Message {
        match self {
            Channel::Track(id) => Message::ToggleSolo(id.clone()),
            Channel::Bus(id) => Message::ToggleBusSolo(id.clone()),
        }
    }

    fn pan_live(&self, pan: f64) -> Message {
        match self {
            Channel::Track(id) => Message::SetTrackPanLive {
                track_id: id.clone(),
                pan,
            },
            Channel::Bus(id) => Message::SetBusPanLive {
                bus_id: id.clone(),
                pan,
            },
        }
    }

    fn volume_live(&self, volume: f64) -> Message {
        match self {
            Channel::Track(id) => Message::SetTrackVolumeLive {
                track_id: id.clone(),
                volume,
            },
            Channel::Bus(id) => Message::SetBusVolumeLive {
                bus_id: id.clone(),
                volume,
            },
        }
    }

    /// The end of a pan or fader drag: a take's volume commit, a bus's FX
    /// commit.
    fn mix_commit(&self) -> Message {
        match self {
            Channel::Track(_) => Message::CommitTrackVolume,
            Channel::Bus(_) => Message::CommitFx,
        }
    }
}

// ------------------------------------------------------------------- input

/// What the inspector shows: the view-model state `LeftPanel` read.
#[derive(Debug, Clone, Copy)]
pub struct Inspector<'a> {
    /// `vm.inspectorOpen`: the strip, or the thin rail.
    pub open: bool,
    /// `vm.selectedTrack`.
    pub selected_track: Option<&'a AudioTrack>,
    /// `vm.selectedBus` (shown in preference to a take).
    pub selected_bus: Option<&'a FxBus>,
    /// `vm.mixer.fxBuses`.
    pub fx_buses: &'a [Arc<FxBus>],
    /// `vm.mixer.supportsEffects`: whether the engine runs the inserts.
    pub supports_effects: bool,
    /// `vm.versions.vocalTakes`: numbers the take ("Take 2").
    pub vocal_takes: &'a [Arc<AudioTrack>],
    /// `vm.versions.active?.name`.
    pub active_interpretation: Option<&'a str>,
    /// The strip's menu the screen holds open, if any.
    pub open_menu: Option<&'a InspectorMenu>,
}

impl Inspector<'_> {
    /// What the inspector asks of the analysis cache once the screen has
    /// updated: it is always visible, so it must trigger the analysis
    /// compute itself (the dock would otherwise be responsible) the moment a
    /// take is selected but its metrics aren't computed yet
    /// (`lacks_metrics`: `vm.analysis.lacksAnalysis(metrics: true)`). For the
    /// metrics alone: a spectrogram comes with them only from the same
    /// decode, when neither half is here; one dropped alone is not the
    /// inspector's to ask for again.
    pub fn analysis_heal(&self, lacks_metrics: bool) -> Option<Message> {
        let track = self.selected_track?;
        (self.open && !track.is_backing() && lacks_metrics).then_some(
            Message::HealSelectedAnalysis {
                spectrogram: Some(false),
                metrics: Some(true),
            },
        )
    }
}

// ------------------------------------------------------------- pure parts

/// The identity block's colour square: the channel's colour in the arrange
/// (`track_row::channel_color`: its own tint, else its place among the
/// shown vocal takes), so the inspector, the lane and the mixer strip read
/// as one channel.
pub fn take_dot_color(track: &AudioTrack, vocal_takes: &[Arc<AudioTrack>]) -> Color {
    channel_color(track, vocal_takes)
}

/// `_takeLabel`: "Take 2 · Interpretation 1" — the take's place among the
/// active interpretation's takes ("Take" when it is not among them), then
/// the interpretation's name when there is one.
pub fn take_label(
    track_id: &str,
    vocal_takes: &[Arc<AudioTrack>],
    interpretation: Option<&str>,
) -> String {
    let number = match vocal_takes.iter().position(|t| t.id == track_id) {
        Some(i) => format!("Take {}", i + 1),
        None => "Take".to_string(),
    };
    match interpretation {
        Some(name) => format!("{number} · {name}"),
        None => number,
    }
}

/// The line under a take's name ("Interpretation 1 · 0:39 · Nov 21"): what
/// it is (the take's label, its number left out when the name already
/// reads it; "Original" / "Accompaniment" for a backing), its length and
/// its date.
pub fn take_subtitle(
    track: &AudioTrack,
    vocal_takes: &[Arc<AudioTrack>],
    interpretation: Option<&str>,
) -> String {
    let sub = if track.is_backing() {
        if track.is_reference() {
            "Original".to_string()
        } else {
            "Accompaniment".to_string()
        }
    } else {
        let label = take_label(&track.id, vocal_takes, interpretation);
        let name = track.display_name.as_str();
        match label.strip_prefix(name) {
            Some("") => String::new(),
            Some(rest) if rest.starts_with(" · ") => rest[" · ".len()..].to_string(),
            _ => label,
        }
    };
    let rest = format!(
        "{} · {}",
        short_clock(track.duration_ms()),
        fmt_date(&track.created_at)
    );
    if sub.is_empty() {
        rest
    } else {
        format!("{sub} · {rest}")
    }
}

/// A length as the meta line reads it: "0:39", "12:05" (minutes unpadded).
pub fn short_clock(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The Output pop-up's label: the destination bus's name, else
/// [`STEREO_OUT`].
pub fn output_name(dest: Option<&str>, buses: &[Arc<FxBus>]) -> String {
    let mut name = STEREO_OUT.to_string();
    for b in buses {
        if Some(b.id.as_str()) == dest {
            name = b.name.clone();
        }
    }
    name
}

/// A send slot's target name, or "—" for a bus that is not a target.
pub fn send_name(bus_id: &str, targets: &[&FxBus]) -> String {
    targets
        .iter()
        .find(|b| b.id == bus_id)
        .map_or_else(|| "—".to_string(), |b| b.name.clone())
}

/// A send level as its readout shows it: "-∞" when off, else whole dB.
pub fn fmt_send_db(db: f64) -> String {
    if db <= SEND_OFF_DB {
        "-∞".to_string()
    } else {
        round_to_i64(db).to_string()
    }
}

/// The send level at fader position `v`: -48 dB to 0 dB, and the bottom
/// off. The bottom sent at -48 dB under a "-∞" label, so a send pulled all
/// the way down still fed the bus — audible through its compression or
/// makeup gain, and on a reverb's tail.
pub fn send_level_at(v: f64) -> f64 {
    if v <= 0.0 {
        SEND_OFF_DB
    } else {
        v * 48.0 - 48.0
    }
}

/// Where a send's fader sits for its level.
pub fn send_fader_value(level_db: f64) -> f64 {
    ((level_db + 48.0) / 48.0).clamp(0.0, 1.0)
}

/// Whether a send into `bus` would feed it a second time: a slot already
/// feeds it, or it is the channel's output (`dest`). The bus would hear the
/// sum, and neither control would read what you hear.
fn already_fed(sends: &[TrackSend], dest: Option<&str>, bus: &FxBus) -> bool {
    dest == Some(bus.id.as_str()) || sends.iter().any(|s| s.bus_id == bus.id)
}

/// The buses the empty send slot offers: those neither a slot nor the
/// output (`dest`) already feeds.
pub fn addable_targets<'b>(
    sends: &[TrackSend],
    dest: Option<&str>,
    targets: &[&'b FxBus],
) -> Vec<&'b FxBus> {
    targets
        .iter()
        .filter(|b| !already_fed(sends, dest, b))
        .copied()
        .collect()
}

/// Whether the empty send slot shows: a bus left to feed, and a free slot.
pub fn can_add_send(sends: &[TrackSend], dest: Option<&str>, targets: &[&FxBus]) -> bool {
    !addable_targets(sends, dest, targets).is_empty() && sends.len() < MAX_SENDS
}

/// The buses a slot's target pop-up offers: its own target, plus the ones
/// neither a slot nor the output (`dest`) feeds.
pub fn retarget_options<'b>(
    sends: &[TrackSend],
    slot: usize,
    dest: Option<&str>,
    targets: &[&'b FxBus],
) -> Vec<&'b FxBus> {
    targets
        .iter()
        .filter(|b| sends[slot].bus_id == b.id || !already_fed(sends, dest, b))
        .copied()
        .collect()
}

/// A scorecard cell's content (`_Metric`).
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryCell {
    pub label: &'static str,
    pub value: String,
    pub sub: String,
    /// The pass / warn / flag dot, when the metric has a verdict.
    pub dot: Option<Color>,
    /// Where the reading sits on the card's bar, 0 to 1 ([`metric_level`]);
    /// `None` without a reading.
    pub level: Option<f32>,
    /// The dock tab a tap opens.
    pub tab: DockTab,
}

/// The display range of each metric card's bar, low to high: where a
/// reading draws on it, not a verdict. In tune 0–100 %, vibrato rate
/// 0–10 Hz, clarity (CPP) 0–20 dB, ring (the singer's-formant prominence)
/// −30–0 dB, dynamic range 0–40 dB, brightness (α ratio) −30–0 dB.
pub const METRIC_RANGES: [(f64, f64); 6] = [
    (0.0, 100.0),
    (0.0, 10.0),
    (0.0, 20.0),
    (-30.0, 0.0),
    (0.0, 40.0),
    (-30.0, 0.0),
];

/// Where `value` sits on a bar spanning `range`, clamped to it; `None` for a
/// value with no reading.
pub fn metric_level(value: f64, (lo, hi): (f64, f64)) -> Option<f32> {
    value
        .is_finite()
        .then(|| ((value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32)
}

/// Whole dB, or "—" for a value with no reading. (Dart rounded without a
/// guard, which throws on a non-finite value.)
fn whole_db(v: f64) -> String {
    if v.is_finite() {
        format!("{} dB", round_to_i64(v))
    } else {
        "—".to_string()
    }
}

/// The vibrato of the longest sung note (the first, among equals), or
/// `None` with no notes.
fn longest_note_vibrato(m: &Arc<VoiceMetrics>, notes: &[PitchNote]) -> Option<Arc<Vibrato>> {
    let mut longest = notes.first()?;
    for n in notes {
        if n.end_frame - n.start_frame > longest.end_frame - longest.start_frame {
            longest = n;
        }
    }
    Some(note_vibrato_of(m, longest))
}

/// The scorecard's six cells: in tune, vibrato, clarity, ring, dynamics and
/// brightness, each with what it reads and the dock tab it opens.
pub fn summary_cells(
    m: &Arc<VoiceMetrics>,
    profile: &VoiceProfile,
    ltas: Option<&Ltas>,
) -> [SummaryCell; 6] {
    let notes = pitch_notes_of(m);
    let in_tune = (!notes.is_empty()).then(|| pitch_in_tune_percent(&notes));
    let vib = longest_note_vibrato(m, &notes).filter(|v| v.present);
    let ltas = ltas.filter(|l| l.has_data());
    [
        SummaryCell {
            label: "In tune",
            value: in_tune.map_or_else(|| "—".to_string(), |p| format!("{}%", round_to_i64(p))),
            sub: "of notes".to_string(),
            dot: in_tune.map(|p| {
                if p >= 85.0 {
                    GOOD
                } else if p >= 70.0 {
                    WARN
                } else {
                    FLAG
                }
            }),
            level: in_tune.and_then(|p| metric_level(p, METRIC_RANGES[0])),
            tab: DockTab::Pitch,
        },
        SummaryCell {
            label: "Vibrato",
            value: vib.as_ref().map_or_else(
                || "straight".to_string(),
                |v| format!("{} Hz", to_string_as_fixed(v.rate_hz, 1)),
            ),
            sub: vib.as_ref().map_or_else(
                || "no vibrato".to_string(),
                |v| format!("±{}¢", round_to_i64(v.extent_cents)),
            ),
            dot: vib.as_ref().map(|v| {
                if (5.0..=7.0).contains(&v.rate_hz) {
                    GOOD
                } else {
                    WARN
                }
            }),
            level: vib
                .as_ref()
                .and_then(|v| metric_level(v.rate_hz, METRIC_RANGES[1])),
            tab: DockTab::Vibrato,
        },
        SummaryCell {
            label: "Clarity",
            value: if m.cpp_avg.is_nan() {
                "—".to_string()
            } else {
                to_string_as_fixed(m.cpp_avg, 1)
            },
            sub: "CPP".to_string(),
            dot: None,
            level: metric_level(m.cpp_avg, METRIC_RANGES[2]),
            tab: DockTab::Timbre,
        },
        SummaryCell {
            label: "Ring",
            value: ltas.map_or_else(|| "—".to_string(), |l| whole_db(l.singers_formant_db)),
            sub: "singer's formant".to_string(),
            dot: None,
            level: ltas.and_then(|l| metric_level(l.singers_formant_db, METRIC_RANGES[3])),
            tab: DockTab::Ltas,
        },
        SummaryCell {
            label: "Dynamics",
            value: if profile.has_dynamics() {
                whole_db(profile.dynamic_range_db())
            } else {
                "—".to_string()
            },
            sub: "soft → loud".to_string(),
            dot: None,
            level: if profile.has_dynamics() {
                metric_level(profile.dynamic_range_db(), METRIC_RANGES[4])
            } else {
                None
            },
            tab: DockTab::Dynamics,
        },
        SummaryCell {
            label: "Brightness",
            value: ltas.map_or_else(|| "—".to_string(), |l| whole_db(l.alpha_ratio)),
            sub: "α ratio".to_string(),
            dot: None,
            level: ltas.and_then(|l| metric_level(l.alpha_ratio, METRIC_RANGES[5])),
            tab: DockTab::Ltas,
        },
    ]
}

/// The red banner's message when the take is clipped or too noisy to
/// trust, else `None`.
pub fn quality_warning(m: &VoiceMetrics) -> Option<String> {
    if m.is_clipped() {
        Some(format!(
            "Clipped {}% — lower the mic gain and re-record.",
            to_string_as_fixed(m.clipped_fraction * 100.0, 1)
        ))
    } else if m.is_low_snr() {
        Some(format!(
            "Low SNR ({} dB) — record closer to the mic / quieter.",
            round_to_i64(m.snr_db)
        ))
    } else {
        None
    }
}

/// The sung range ("A3 – E5") and its span in semitones, when the profile
/// has one.
pub fn range_line(p: &VoiceProfile) -> Option<(String, i64)> {
    let (lo, hi) = (p.low_midi?, p.high_midi?);
    Some((
        format!("{} – {}", note_name_for_midi(lo), note_name_for_midi(hi)),
        hi - lo,
    ))
}

/// "chest 40 · mix 35 · head 25".
pub fn register_line(reg: &RegisterMap) -> String {
    format!(
        "chest {} · mix {} · head {}",
        round_to_i64(reg.chest_pct),
        round_to_i64(reg.mix_pct),
        round_to_i64(reg.head_pct)
    )
}

/// The register bar's three widths on a bar `width` wide: each share's
/// flex (`(pct * 10).round().clamp(0, 1000)`) of the whole, as a `Row` of
/// `Expanded`s split it (a share of flex 0 takes nothing).
pub fn register_widths(reg: &RegisterMap, width: f32) -> [f32; 3] {
    let flex = |pct: f64| round_to_i64(pct * 10.0).clamp(0, 1000);
    let flexes = [flex(reg.chest_pct), flex(reg.mix_pct), flex(reg.head_pct)];
    let total: i64 = flexes.iter().sum();
    if total == 0 {
        return [0.0; 3];
    }
    flexes.map(|f| width * f as f32 / total as f32)
}

// --------------------------------------------------------------- the panel

/// The inspector's inner hairlines: under the header and the channel
/// block, above the routing and the footer (`#222228`).
pub const HAIRLINE: Color = Color::from_rgb8(0x22, 0x22, 0x28);

/// The header's height, its hairline included.
pub const HEADER_HEIGHT: f32 = 36.0;

/// The panel's side padding.
const INSET: f32 = 14.0;

/// The footer's dB readout's width.
pub const FOOTER_DB_WIDTH: f32 = 46.0;

/// The inspector: the strip [`PANEL_WIDTH`] wide, or the [`RAIL_WIDTH`]
/// rail when closed. `fx_panel` builds the shown channel's FX panel (see
/// the module docs); `on` wraps the inspector's messages in the screen's.
pub fn left_panel<'a, M: Clone + 'a>(
    inspector: Inspector<'a>,
    fx_panel: impl Fn() -> Element<'a, M> + 'a,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    if !inspector.open {
        return rail(on);
    }
    let body: Element<'a, M> = if let Some(bus) = inspector.selected_bus {
        keyed(
            format!("bus:{}", bus.id),
            bus_body(inspector, bus, fx_panel, on),
        )
    } else if let Some(track) = inspector.selected_track {
        keyed(
            format!("take:{}", track.id),
            take_body(inspector, track, fx_panel, on),
        )
    } else {
        empty("Select a take or bus to inspect it.", true, false)
    };
    let content = column![
        header(on),
        hline(HAIRLINE),
        container(body).height(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    container(row![content, vline()])
        .width(PANEL_WIDTH)
        .height(Length::Fill)
        .style(fill(Studio::PANEL))
        .into()
}

/// `_Rail`: the thin collapsed rail (mirrors the right panel's rail,
/// flipped) — the show-inspector icon over "Inspector" turned a quarter
/// clockwise; a press anywhere on it opens the strip.
fn rail<'a, M: Clone + 'a>(on: fn(Message) -> M) -> Element<'a, M> {
    let content = column![
        Space::new().height(6),
        container(line_icon_inherit(glyphs::PANEL_LEFT, line_icon::PANEL_SIZE))
            .center_x(height::PANEL_BUTTON)
            .center_y(height::PANEL_BUTTON),
        Space::new().height(12),
        // Clipped, not overflowing, where the dock leaves the panels less
        // height than the label.
        container(
            canvas_widget(RailLabel)
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true),
    ]
    .align_x(Horizontal::Center)
    .width(Length::Fill)
    .height(Length::Fill);
    let face = button(content)
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(Studio::SURFACE)),
                text_color: if hovered {
                    Studio::TEXT_PRIMARY
                } else {
                    Studio::TEXT_SECONDARY
                },
                ..button::Style::default()
            }
        })
        .on_press(on(Message::ToggleInspectorPanel));
    container(row![
        labelled(
            material_tooltip(face, "Show inspector", 6.0),
            "Show inspector"
        ),
        vline()
    ])
    .width(RAIL_WIDTH)
    .height(Length::Fill)
    .style(fill(Studio::PANEL))
    .into()
}

/// The rail's "Inspector": 11 px semibold `TEXT_SECONDARY` (`DESIGN.md`,
/// sentence case), turned a quarter clockwise: it reads top to bottom from
/// the top of its box, centred across the rail.
struct RailLabel;

impl<M> canvas::Program<M> for RailLabel {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.translate(Vector::new(bounds.width / 2.0, 0.0));
        frame.rotate(std::f32::consts::FRAC_PI_2);
        frame.fill_text(canvas::Text {
            content: "Inspector".to_string(),
            position: Point::ORIGIN,
            color: Studio::TEXT_SECONDARY,
            size: Pixels(type_scale::LABEL),
            line_height: Pixels(type_scale::LINE).into(),
            font: font::SEMIBOLD,
            align_x: Alignment::Left,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
        vec![frame.into_geometry()]
    }
}

/// `_Header`: "Inspector" (12 px semibold) and the hide button, 36 px tall
/// with its hairline.
fn header<'a, M: Clone + 'a>(on: fn(Message) -> M) -> Element<'a, M> {
    container(
        row![
            studio_text("Inspector")
                .size(type_scale::BODY)
                .font(font::SEMIBOLD)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_PRIMARY),
            Space::new().width(Length::Fill),
            icon_button(glyphs::PANEL_LEFT, on(Message::ToggleInspectorPanel))
                .panel()
                .tooltip("Hide inspector"),
        ]
        .align_y(Vertical::Center),
    )
    .height(HEADER_HEIGHT - 1.0)
    .padding(Padding {
        left: INSET,
        right: 8.0,
        ..Padding::ZERO
    })
    .align_y(Vertical::Center)
    .into()
}

/// The body for a selected take: its identity block, then (for a vocal
/// take, when the engine supports effects) the FX panel and the routing
/// (sends and output), else a note that the backing plays dry; the channel
/// footer pinned below.
fn take_body<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    track: &'a AudioTrack,
    fx_panel: impl Fn() -> Element<'a, M> + 'a,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let is_vocal = !track.is_backing() && insp.supports_effects;
    responsive(move |size| {
        let identity = identity_block(
            take_dot_color(track, insp.vocal_takes),
            &track.display_name,
            take_subtitle(track, insp.vocal_takes, insp.active_interpretation),
            // The FX panel's preset row closes the block.
            !is_vocal,
        );
        let strip = Strip {
            channel: Channel::Track(track.id.clone()),
            sends: &track.sends,
            targets: insp.fx_buses.iter().map(Arc::as_ref).collect(),
            dest: track.bus_id.as_deref(),
            outputs: insp.fx_buses.iter().map(Arc::as_ref).collect(),
            muted: track.muted,
            soloed: track.soloed,
            pan: track.pan,
            volume: track.volume,
        };
        let routing = is_vocal.then(|| routing_section(insp, &strip, on));
        let footer = channel_footer(&strip, on);
        let dry = || empty("Backing track plays dry, with no effects.", true, false);
        if size.height < MIN_PINNED_HEIGHT {
            // Degenerate height (tiny window with the dock open): scroll the
            // whole strip. The FX panel needs a bounded height, so it gets a
            // fixed window inside the scrolling column.
            let middle: Element<'a, M> = if is_vocal {
                container(fx_panel())
                    .width(Length::Fill)
                    .height(SCROLLED_FX_HEIGHT)
                    .into()
            } else {
                container(dry())
                    .width(Length::Fill)
                    .height(SCROLLED_DRY_HEIGHT)
                    .into()
            };
            scrolled(column![identity, middle].extend(routing).push(footer))
        } else {
            let middle: Element<'a, M> = if is_vocal {
                container(fx_panel())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                container(dry())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            };
            column![identity, middle]
                .extend(routing)
                .push(footer)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    })
    .into()
}

/// Inspector body for a selected FX bus: its identity block, the
/// (bus-targeted) FX panel, its routing and its channel footer.
fn bus_body<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    bus: &'a FxBus,
    fx_panel: impl Fn() -> Element<'a, M> + 'a,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    responsive(move |size| {
        let identity = identity_block(bus_color(bus), &bus.name, "FX bus".to_string(), false);
        // A bus never routes or sends into itself: the feedback loop is not
        // even offered.
        let others: Vec<&'a FxBus> = insp
            .fx_buses
            .iter()
            .map(Arc::as_ref)
            .filter(|b| b.id != bus.id)
            .collect();
        let strip = Strip {
            channel: Channel::Bus(bus.id.clone()),
            sends: &bus.sends,
            targets: others.clone(),
            dest: bus.output_bus_id.as_deref(),
            outputs: others,
            muted: bus.muted,
            soloed: bus.soloed,
            pan: bus.pan,
            volume: bus.volume,
        };
        let routing = routing_section(insp, &strip, on);
        let footer = channel_footer(&strip, on);
        // The take strip's degenerate-height fallback, for the same reason:
        // pinned, the header and footer overflowed a short panel and pushed
        // the bus's fader, mute/solo and pan under the dock.
        if size.height < MIN_PINNED_HEIGHT {
            let fx = container(fx_panel())
                .width(Length::Fill)
                .height(SCROLLED_FX_HEIGHT);
            scrolled(column![identity, fx, routing, footer])
        } else {
            let fx = container(fx_panel())
                .width(Length::Fill)
                .height(Length::Fill);
            column![identity, fx, routing, footer]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    })
    .into()
}

/// The strip as one scrolling column.
fn scrolled<'a, M: 'a>(content: Column<'a, M>) -> Element<'a, M> {
    scrollable(content.width(Length::Fill))
        .fading()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The selected channel's identity (`_TakeHeader`, `_BusBody`'s header):
/// an 8 px colour square (radius 2) and the name in 13 px semibold, the
/// meta under it in 11 px `TEXT_FAINT`, set in past the square. `closed`:
/// nothing follows inside the block (no preset row), so it ends with its
/// own padding and hairline.
fn identity_block<'a, M: 'a>(
    color: Color,
    name: &'a str,
    meta: String,
    closed: bool,
) -> Element<'a, M> {
    let block = column![
        row![
            container(Space::new().width(8).height(8)).style(move |_| boxed(
                Some(color),
                2.0,
                None
            )),
            studio_text(name)
                .size(type_scale::TITLE)
                .font(font::SEMIBOLD)
                .line_height(Pixels(18.0))
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .ellipsis(1),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
        container(
            studio_text(meta)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_FAINT)
                .ellipsis(1),
        )
        .padding(Padding {
            left: 16.0,
            ..Padding::ZERO
        }),
    ]
    .spacing(2)
    .width(Length::Fill);
    let block = container(block).width(Length::Fill).padding(Padding {
        top: 12.0,
        right: INSET,
        bottom: if closed { 12.0 } else { 0.0 },
        left: INSET,
    });
    if closed {
        column![block, hline(HAIRLINE)].into()
    } else {
        block.into()
    }
}

/// What a channel's routing and footer show and drive.
struct Strip<'a> {
    channel: Channel,
    sends: &'a [TrackSend],
    /// The buses a send may feed.
    targets: Vec<&'a FxBus>,
    /// The output's bus, `None` for the stereo out.
    dest: Option<&'a str>,
    /// The buses the output may route to.
    outputs: Vec<&'a FxBus>,
    muted: bool,
    soloed: bool,
    pan: f64,
    volume: f64,
}

/// A number with a true minus sign (U+2212), as the design sets numbers.
pub fn minus_sign(text: String) -> String {
    text.replace('-', "\u{2212}")
}

/// The footer's level readout: "−1.4 dB", "0.0 dB", "−∞ dB".
pub fn footer_db(volume: f64) -> String {
    minus_sign(format!("{} dB", fader_db(volume)))
}

/// A send's level readout: "−10 dB", "−∞ dB" when off.
pub fn send_db_label(level_db: f64) -> String {
    minus_sign(format!("{} dB", fmt_send_db(level_db)))
}

/// `_TrackChannelFooter` / `_BusChannelFooter`: the channel footer pinned
/// at the bottom — M and S, the pan knob, the volume as a thin slider and
/// its dB — over a hairline. The same widgets for a take and a bus, through
/// each one's own view-model methods: a take's pan and volume commit the
/// take's volume, a bus's commit its FX.
fn channel_footer<'a, M: Clone + 'a>(strip: &Strip<'a>, on: fn(Message) -> M) -> Element<'a, M> {
    let channel = strip.channel.clone();
    let pan_channel = channel.clone();
    let volume_channel = channel.clone();
    let footer = row![
        msr_button(MsrKind::Mute, strip.muted, Some(on(channel.toggle_mute()))).size(22.0, 20.0),
        msr_button(MsrKind::Solo, strip.soloed, Some(on(channel.toggle_solo()))).size(22.0, 20.0),
        studio_pan_knob(
            strip.pan,
            move |p| on(pan_channel.pan_live(p)),
            on(channel.mix_commit()),
        )
        .size(24.0)
        .show_label(false),
        container(thin_slider(
            strip.volume as f32,
            move |v| on(volume_channel.volume_live(f64::from(v))),
            on(channel.mix_commit()),
        ))
        .width(Length::Fill),
        container(
            studio_text(footer_db(strip.volume))
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .align_x(Horizontal::Right)
                .wrapping(Wrapping::None),
        )
        .width(FOOTER_DB_WIDTH),
    ]
    .spacing(10)
    .align_y(Vertical::Center);
    column![
        hline(HAIRLINE),
        container(footer).width(Length::Fill).padding(Padding {
            top: 10.0,
            right: INSET,
            bottom: 12.0,
            left: INSET,
        }),
    ]
    .width(Length::Fill)
    .into()
}

/// The strip's `menu` when it is the open one, as a studio menu `width`
/// wide of `items`.
fn open_menu<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    menu: &InspectorMenu,
    items: impl FnOnce() -> Vec<StudioMenuItem<M>>,
    width: f32,
) -> Option<Element<'a, M>> {
    (insp.open_menu == Some(menu)).then(|| studio_menu(items(), width))
}

/// A pop-up button (`DESIGN.md`) reading `label`, filling its column, that
/// opens `menu` below itself (`menuAnchorBelow(anchor)`).
fn menu_popup<'a, M: Clone + 'a>(
    label: String,
    placeholder: bool,
    menu: InspectorMenu,
    opened: Option<Element<'a, M>>,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let trigger = popup_button(label, Some(on(Message::OpenMenu(menu))))
        .width(Length::Fill)
        .placeholder(placeholder);
    menu_host(trigger, opened, on(Message::DismissMenu))
        .anchor(MenuAnchor::Below(MENU_ANCHOR_BELOW_DY))
        .into()
}

/// A routing row on the parameter row's grid: a 62 px label, the control,
/// a 54 px value column.
fn routing_row<'a, M: 'a>(
    label: &'a str,
    control: Element<'a, M>,
    value: Element<'a, M>,
) -> Element<'a, M> {
    row![
        container(
            studio_text(label)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_SECONDARY)
                .ellipsis(1),
        )
        .width(PARAMETER_LABEL_WIDTH),
        container(control).width(Length::Fill),
        container(value)
            .width(PARAMETER_VALUE_WIDTH)
            .align_x(Horizontal::Right),
    ]
    .spacing(8)
    .height(height::CONTROL)
    .align_y(Vertical::Center)
    .into()
}

/// The routing (`_SendsSection` and the output row), above the footer: a
/// row per post-fader aux send — "Send", its target's pop-up, its level —
/// with the level's slider and a remove button under it; an empty slot
/// ("None") whose pop-up adds a send while a bus is left to feed and a slot
/// is free; then "Output" and its destination's pop-up.
fn routing_section<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    strip: &Strip<'a>,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let sends = strip.sends;
    let channel = &strip.channel;
    let mut col = Column::new().spacing(8).width(Length::Fill);
    for (i, send) in sends.iter().enumerate() {
        let menu = InspectorMenu::RetargetSend(channel.clone(), i);
        let opened = open_menu(
            insp,
            &menu,
            || {
                retarget_options(sends, i, strip.dest, &strip.targets)
                    .iter()
                    .map(|b| {
                        StudioMenuItem::new(b.name.clone(), on(channel.retarget_send(i, &b.id)))
                            .selected(b.id == send.bus_id)
                    })
                    .collect()
            },
            SEND_MENU_WIDTH,
        );
        let level_channel = channel.clone();
        let target = menu_popup(
            send_name(&send.bus_id, &strip.targets),
            false,
            menu,
            opened,
            on,
        );
        let level: Element<'a, M> = studio_text(send_db_label(send.level_db))
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .tabular()
            .color(Studio::TEXT_PRIMARY)
            .wrapping(Wrapping::None)
            .into();
        let slider: Element<'a, M> = thin_slider(
            send_fader_value(send.level_db) as f32,
            move |v| on(level_channel.send_level(i, send_level_at(f64::from(v)))),
            on(Message::CommitFx),
        )
        .into();
        let remove: Element<'a, M> = icon_button(Icon::X, on(channel.remove_send(i)))
            .panel()
            .icon_size(12.0)
            .tooltip(REMOVE_SEND)
            .into();
        col = col.push(column![
            routing_row(if i == 0 { "Send" } else { "" }, target, level),
            row![
                Space::new().width(PARAMETER_LABEL_WIDTH),
                container(slider).width(Length::Fill),
                container(remove)
                    .width(PARAMETER_VALUE_WIDTH)
                    .align_x(Horizontal::Right),
            ]
            .spacing(8)
            .height(height::PANEL_BUTTON)
            .align_y(Vertical::Center),
        ]);
    }
    if can_add_send(sends, strip.dest, &strip.targets) {
        let addable = addable_targets(sends, strip.dest, &strip.targets);
        let menu = InspectorMenu::AddSend(channel.clone());
        let opened = open_menu(
            insp,
            &menu,
            || {
                addable
                    .iter()
                    .map(|b| StudioMenuItem::new(b.name.clone(), on(channel.add_send(&b.id))))
                    .collect()
            },
            SEND_MENU_WIDTH,
        );
        col = col.push(routing_row(
            if sends.is_empty() { "Send" } else { "" },
            menu_popup(EMPTY_SLOT.to_string(), true, menu, opened, on),
            Space::new().into(),
        ));
    }
    let menu = InspectorMenu::Output(strip.channel.clone());
    let dest = strip.dest;
    let opened = open_menu(
        insp,
        &menu,
        || {
            let mut items = vec![
                StudioMenuItem::new(STEREO_OUT, on(strip.channel.route(None)))
                    .selected(dest.is_none()),
            ];
            items.extend(strip.outputs.iter().map(|b| {
                StudioMenuItem::new(b.name.clone(), on(strip.channel.route(Some(&b.id))))
                    .selected(dest == Some(b.id.as_str()))
            }));
            items
        },
        OUTPUT_MENU_WIDTH,
    );
    col = col.push(routing_row(
        "Output",
        menu_popup(output_name(dest, insp.fx_buses), false, menu, opened, on),
        Space::new().into(),
    ));
    column![
        hline(HAIRLINE),
        container(col).width(Length::Fill).padding([10.0, INSET]),
    ]
    .width(Length::Fill)
    .into()
}

/// The empty send slot's pop-up text.
pub const EMPTY_SLOT: &str = "None";

/// A send's remove button's tooltip.
pub const REMOVE_SEND: &str = "Remove send";

/// A `size` px disc (the scorecard's pass / warn dot).
fn disc<'a, M: 'a>(size: f32, color: Color) -> Element<'a, M> {
    container(Space::new().width(size).height(size))
        .style(move |_| boxed(Some(color), size / 2.0, None))
        .into()
}

/// A 1 px line across in `color`.
fn hline<'a, M: 'a>(color: Color) -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(fill(color))
        .into()
}

/// The panel's right border, 1 px down its height.
fn vline<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(1)
        .height(Length::Fill)
        .style(fill(Studio::BORDER))
        .into()
}

/// `_Empty(text, inset: ..., spinner: ...)`: a faint 12 px message, centred
/// both ways (padded 20 in the inset variant), after a spinner when one is
/// asked for.
fn empty<'a, M: 'a>(label: &'a str, inset: bool, with_spinner: bool) -> Element<'a, M> {
    center(container(empty_line(label, with_spinner)).padding(if inset { 20 } else { 0 })).into()
}

/// [`empty`]'s line: what a list shows of it (centred across, its own
/// height).
fn empty_line<'a, M: 'a>(label: &'a str, with_spinner: bool) -> Element<'a, M> {
    let mut line = row![].align_y(Vertical::Center);
    if with_spinner {
        line = line
            .push(spinner(12.0, 2.0, Studio::TEXT_FAINT))
            .push(Space::new().width(8));
    }
    line = line.push(
        text(label)
            .size(12)
            .line_height(Pixels(type_scale::LINE))
            .color(Studio::TEXT_FAINT)
            .align_x(Horizontal::Center),
    );
    container(line)
        .width(Length::Fill)
        .align_x(Horizontal::Center)
        .into()
}

// --------------------------------------------------------- the summary

/// What the voice summary shows: the selected take and its analysis
/// (`vm.selectedTrack`, the loading flags, `vm.selectedMetrics`,
/// `selectedProfile`, `selectedRegister`, `selectedLtas`).
#[derive(Debug, Clone, Copy)]
pub struct VoiceSummary<'a> {
    pub selected_track: Option<&'a AudioTrack>,
    /// The subject's halves, not the lane's: with a clip of a multi-clip
    /// lane selected, the lane's own loading flag is its aggregate's.
    pub spectrogram_loading: bool,
    pub metrics_loading: bool,
    pub metrics: Option<&'a Arc<VoiceMetrics>>,
    pub profile: Option<&'a VoiceProfile>,
    pub register: Option<&'a RegisterMap>,
    pub ltas: Option<&'a Ltas>,
}

// The summary's own imports, kept with it.
use crate::ui::core::theme as studio_theme;

/// `VoiceSummaryView`: the voice-analysis summary for the selected take —
/// range, in-tune %, vibrato, clarity, dynamics, brightness, register.
/// Hosted in the bottom dock's "Summary" tab (moved out of the inspector,
/// which is now a channel strip).
///
/// Drawn as `DESIGN.md`'s metric cards across the dock: a "Voice summary"
/// heading, the range and register cards side by side, then the six metric
/// cards in a row (three or two a row on a narrower dock, see
/// [`summary_columns`]). Each card opens its dock tab.
pub fn voice_summary_view<'a>(summary: VoiceSummary<'a>) -> Element<'a, Message> {
    let Some(track) = summary.selected_track else {
        return empty("Select a take to see its analysis.", true, false);
    };
    if track.is_backing() {
        return empty("Backing track has no voice analysis.", true, false);
    }
    responsive(move |size| {
        let columns = summary_columns(size.width - 2.0 * SUMMARY_INSET);
        scrollable(
            container(voice_summary(summary, columns))
                .width(Length::Fill)
                .padding(Padding {
                    top: 12.0,
                    right: SUMMARY_INSET,
                    bottom: SUMMARY_INSET,
                    left: SUMMARY_INSET,
                }),
        )
        .fading()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
    .into()
}

/// The summary's inset from the dock's edges, and the gap between cards.
const SUMMARY_INSET: f32 = 16.0;
const SUMMARY_GAP: f32 = 8.0;

/// How many metric cards a row holds on a summary `width` wide: all six
/// from 900 px, three from 480, else two.
pub fn summary_columns(width: f32) -> usize {
    if width >= 900.0 {
        6
    } else if width >= 480.0 {
        3
    } else {
        2
    }
}

/// `_VoiceSummary`: the "scorecard" — a quality banner when the take can't
/// be trusted, the range and register cards, and the metric cards,
/// `columns` a row.
fn voice_summary<'a>(s: VoiceSummary<'a>, columns: usize) -> Element<'a, Message> {
    if s.spectrogram_loading || s.metrics_loading {
        return empty_line("Analyzing…", true);
    }
    let (Some(m), Some(profile)) = (s.metrics, s.profile) else {
        return empty_line("No analysis for this take.", false);
    };
    let mut col = Column::new().width(Length::Fill);
    if let Some(warning) = quality_warning(m) {
        col = col.push(quality_banner(warning));
    }
    let mut top = row![range_card(profile)].spacing(SUMMARY_GAP);
    if let Some(reg) = s.register.filter(|r| r.has_data()) {
        top = top.push(register_card(reg));
    }
    col.push(cap_label("Voice summary"))
        .push(Space::new().height(8))
        .push(top)
        .push(Space::new().height(SUMMARY_GAP))
        .push(metric_grid(summary_cells(m, profile, s.ltas), columns))
        .into()
}

/// `_qualityBanner`: a banner when the selected take is clipped or too
/// noisy to trust, on a faint `RECORD` wash.
fn quality_banner<'a>(message: String) -> Element<'a, Message> {
    container(
        container(
            row![
                icons::icon(Icon::TriangleAlert, 14.0, Studio::RECORD_TEXT),
                Space::new().width(8),
                studio_text(message)
                    .size(studio_theme::type_scale::LABEL)
                    .line_height(Pixels(studio_theme::type_scale::LINE))
                    .color(Studio::TEXT_PRIMARY)
                    .width(Length::Fill),
            ]
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .padding([7, 10])
        .style(|_| {
            boxed(
                Some(alpha(Studio::RECORD, 0.10)),
                studio_theme::radius::CARD,
                Some(alpha(Studio::RECORD, 0.32)),
            )
        }),
    )
    .padding(Padding {
        bottom: 10.0,
        ..Padding::ZERO
    })
    .into()
}

/// A summary card (`DESIGN.md`, Cards and Metric cards): `SURFACE`, radius
/// 8, an inset `BORDER` outline; under the pointer `#1C1C21` with a
/// `#3A3A43` outline. A press opens `tab`.
fn summary_card<'a>(
    content: impl Into<Element<'a, Message>>,
    tab: DockTab,
) -> Element<'a, Message> {
    button(content)
        .padding([10, 12])
        .width(Length::Fill)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let face = boxed(
                Some(if hovered {
                    Color::from_rgb8(0x1C, 0x1C, 0x21)
                } else {
                    Studio::SURFACE
                }),
                studio_theme::radius::CARD,
                Some(if hovered {
                    Color::from_rgb8(0x3A, 0x3A, 0x43)
                } else {
                    Studio::BORDER
                }),
            );
            button::Style {
                background: face.background,
                text_color: Studio::TEXT_PRIMARY,
                border: face.border,
                shadow: iced::Shadow::default(),
                snap: true,
            }
        })
        .on_press(Message::ShowDockTab(tab))
        .into()
}

/// A card's label: 11 px `TEXT_SECONDARY`.
fn card_label<'a>(label: &str) -> Element<'a, Message> {
    studio_text(label.to_string())
        .size(studio_theme::type_scale::LABEL)
        .line_height(Pixels(studio_theme::type_scale::LINE))
        .color(Studio::TEXT_SECONDARY)
        .ellipsis(1)
        .into()
}

/// A readout's number and its unit ("86%" → "86" and "%", "5.6 Hz" → "5.6"
/// and "Hz"); a reading with no unit ("straight", "—") is all number.
pub fn split_unit(value: &str) -> (&str, &str) {
    if let Some((number, unit)) = value.split_once(' ') {
        return (number, unit);
    }
    match value.strip_suffix('%') {
        Some(number) if !number.is_empty() => (number, "%"),
        _ => (value, ""),
    }
}

/// A big readout (`DESIGN.md`: 18 px semibold, tabular, its unit 11 px
/// `TEXT_FAINT` after it), labelled with the whole reading for the widget
/// tree.
fn readout<'a>(value: &str) -> Element<'a, Message> {
    let (number, unit) = split_unit(value);
    // A typographic minus: the hyphen sits low and narrow among tabular
    // figures.
    let number = match number.strip_prefix('-') {
        Some(rest) => format!("−{rest}"),
        None => number.to_string(),
    };
    let mut line = row![studio_text(number)
        .size(studio_theme::type_scale::READOUT)
        .line_height(Pixels(22.0))
        .font(font::SEMIBOLD)
        .tabular()
        .color(Studio::TEXT_PRIMARY)
        .ellipsis(1)]
    .spacing(if unit == "%" { 1 } else { 3 })
    .align_y(Vertical::Bottom);
    if !unit.is_empty() {
        line = line.push(
            container(
                studio_text(unit.to_string())
                    .size(studio_theme::type_scale::LABEL)
                    .line_height(Pixels(studio_theme::type_scale::LINE))
                    .color(Studio::TEXT_FAINT),
            )
            .padding(Padding {
                bottom: 2.0,
                ..Padding::ZERO
            }),
        );
    }
    labelled(line, value.to_string())
}

/// `_RangeRow`: the sung range ("A3 – E5", its span in semitones) and a
/// bar (the full range, the tessitura over it, the passaggio's tick); "No
/// pitch detected" without a range.
fn range_card<'a>(p: &'a VoiceProfile) -> Element<'a, Message> {
    let body: Element<'a, Message> = match range_line(p) {
        None => column![
            card_label("Range"),
            Space::new().height(6),
            text("No pitch detected")
                .size(studio_theme::type_scale::BODY)
                .color(Studio::TEXT_FAINT),
        ]
        .into(),
        Some((range, span)) => {
            let mut value = row![
                studio_text(range)
                    .size(studio_theme::type_scale::READOUT)
                    .line_height(Pixels(22.0))
                    .font(font::SEMIBOLD)
                    .tabular()
                    .color(Studio::TEXT_PRIMARY),
                container(
                    studio_text(format!("{span} st"))
                        .size(studio_theme::type_scale::LABEL)
                        .line_height(Pixels(studio_theme::type_scale::LINE))
                        .color(Studio::TEXT_FAINT),
                )
                .padding(Padding {
                    bottom: 2.0,
                    ..Padding::ZERO
                }),
            ]
            .spacing(6)
            .align_y(Vertical::Bottom);
            if let (Some(lo), Some(hi)) = (p.tess_lo_midi, p.tess_hi_midi) {
                value = value.push(Space::new().width(Length::Fill)).push(
                    container(
                        studio_text(format!(
                            "Tessitura {} – {}",
                            note_name_for_midi(lo),
                            note_name_for_midi(hi)
                        ))
                        .size(studio_theme::type_scale::LABEL)
                        .line_height(Pixels(studio_theme::type_scale::LINE))
                        .color(Studio::TEXT_FAINT)
                        .ellipsis(1),
                    )
                    .padding(Padding {
                        bottom: 2.0,
                        ..Padding::ZERO
                    }),
                );
            }
            column![
                card_label("Range"),
                Space::new().height(2),
                value,
                Space::new().height(8),
                canvas_widget(RangeBar { profile: p })
                    .width(Length::Fill)
                    .height(10),
            ]
            .width(Length::Fill)
            .into()
        }
    };
    summary_card(body, DockTab::Range)
}

/// `_RangeBar`: a 4 px `CONTROL_ACTIVE` track (the sung range), the
/// tessitura over it in the good-range green, and the passaggio as a 2 px
/// `ACCENT_BRIGHT` tick.
struct RangeBar<'a> {
    profile: &'a VoiceProfile,
}

/// Where `midi` falls on a range bar `width` wide spanning `lo`..`hi`.
pub fn range_x(midi: i64, lo: i64, hi: i64, width: f32) -> f32 {
    let span = (hi - lo).clamp(1, 1 << 30);
    ((midi - lo) as f64 / span as f64).clamp(0.0, 1.0) as f32 * width
}

impl<M> canvas::Program<M> for RangeBar<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let p = self.profile;
        let (Some(lo), Some(hi)) = (p.low_midi, p.high_midi) else {
            return vec![frame.into_geometry()];
        };
        let (w, h) = (bounds.width, bounds.height);
        let x = |midi| range_x(midi, lo, hi, w);
        let mid = (h / 2.0).round();
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, mid - 2.0),
                Size::new(w, 4.0),
                Radius::from(2.0),
            ),
            Studio::CONTROL_ACTIVE,
        );
        // Tessitura highlight.
        if let (Some(t_lo), Some(t_hi)) = (p.tess_lo_midi, p.tess_hi_midi) {
            let (left, right) = (x(t_lo), x(t_hi));
            if right > left {
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(left, mid - 2.0),
                        Size::new(right - left, 4.0),
                        Radius::from(2.0),
                    ),
                    alpha(Studio::GOOD, 0.85),
                );
            }
        }
        // Passaggio marker.
        if let Some(passaggio) = p.passaggio_midi {
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(x(passaggio) - 1.0, 0.0),
                    Size::new(2.0, h),
                    Radius::from(1.0),
                ),
                Studio::ACCENT_BRIGHT,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The metric cards, `columns` a row, [`SUMMARY_GAP`] apart.
fn metric_grid<'a>(cells: [SummaryCell; 6], columns: usize) -> Element<'a, Message> {
    let mut rows = Column::new().width(Length::Fill).spacing(SUMMARY_GAP);
    let mut cells = cells.into_iter().peekable();
    while cells.peek().is_some() {
        let mut line = row![].spacing(SUMMARY_GAP).width(Length::Fill);
        for _ in 0..columns {
            line = line.push(match cells.next() {
                Some(cell) => metric(cell),
                None => Space::new().width(Length::Fill).into(),
            });
        }
        rows = rows.push(line);
    }
    rows.into()
}

/// The fill of a metric card's bar: neutral; the verdict is the dot's.
const METRIC_BAR_FILL: Color = Color::from_rgb8(0x8A, 0x8A, 0x96);

/// `_Metric`: one metric card (`DESIGN.md`): its label (and a 6 px verdict
/// dot where the metric has one), the reading (see [`readout`]), a 4 px bar
/// at the cell's [`SummaryCell::level`] and the sub-line; a tap opens the
/// matching dock tab.
fn metric<'a>(cell: SummaryCell) -> Element<'a, Message> {
    let mut top =
        row![container(card_label(cell.label)).width(Length::Fill)].align_y(Vertical::Center);
    if let Some(color) = cell.dot {
        top = top.push(disc(6.0, color));
    }
    let level = cell.level;
    summary_card(
        column![
            top,
            Space::new().height(2),
            readout(&cell.value),
            Space::new().height(8),
            canvas_widget(LevelBar { level })
                .width(Length::Fill)
                .height(4),
            Space::new().height(6),
            studio_text(cell.sub)
                .size(studio_theme::type_scale::LABEL)
                .line_height(Pixels(studio_theme::type_scale::LINE))
                .color(Studio::TEXT_FAINT)
                .ellipsis(1),
        ]
        .width(Length::Fill),
        cell.tab,
    )
}

/// A metric card's 4 px bar: a `CONTROL_ACTIVE` track, filled to `level`
/// (0 to 1; none without a reading).
struct LevelBar {
    level: Option<f32>,
}

impl<M> canvas::Program<M> for LevelBar {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let size = bounds.size();
        let r = Radius::from(size.height / 2.0);
        frame.fill(
            &Path::rounded_rectangle(Point::ORIGIN, size, r),
            Studio::CONTROL_ACTIVE,
        );
        if let Some(level) = self.level.filter(|l| l.is_finite()) {
            let w = level.clamp(0.0, 1.0) * size.width;
            if w > 0.0 {
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::ORIGIN,
                        Size::new(w.max(size.height), size.height),
                        r,
                    ),
                    METRIC_BAR_FILL,
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// `_RegisterBar`: the chest / mix / head split: a legend of swatches (read
/// as "chest 40 · mix 35 · head 25") over a 6 px bar, its ends rounded.
fn register_card<'a>(reg: &'a RegisterMap) -> Element<'a, Message> {
    let swatch = |color: Color, label: String| -> Element<'a, Message> {
        row![
            container(Space::new().width(8).height(8)).style(move |_| boxed(
                Some(color),
                2.0,
                None
            )),
            studio_text(label)
                .size(studio_theme::type_scale::LABEL)
                .line_height(Pixels(studio_theme::type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(Vertical::Center)
        .into()
    };
    let legend = row![
        swatch(CHEST, format!("Chest {}%", round_to_i64(reg.chest_pct))),
        swatch(mix_color(), format!("Mix {}%", round_to_i64(reg.mix_pct))),
        swatch(HEAD, format!("Head {}%", round_to_i64(reg.head_pct))),
    ]
    .spacing(14)
    .align_y(Vertical::Center);
    summary_card(
        column![
            card_label("Register"),
            Space::new().height(2),
            container(labelled(legend, register_line(reg)))
                .height(22)
                .align_y(Vertical::Bottom),
            Space::new().height(10),
            canvas_widget(RegisterSplit { reg })
                .width(Length::Fill)
                .height(6),
        ]
        .width(Length::Fill),
        DockTab::Registers,
    )
}

/// Mix is the chest and head colors halfway (`Color.lerp(chest, head, 0.5)`).
pub fn mix_color() -> Color {
    Color {
        r: (CHEST.r + HEAD.r) / 2.0,
        g: (CHEST.g + HEAD.g) / 2.0,
        b: (CHEST.b + HEAD.b) / 2.0,
        a: (CHEST.a + HEAD.a) / 2.0,
    }
}

struct RegisterSplit<'a> {
    reg: &'a RegisterMap,
}

impl<M> canvas::Program<M> for RegisterSplit<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // A 1 px gap between the shares.
        let widths = register_widths(self.reg, bounds.width);
        let colors = [CHEST, mix_color(), HEAD];
        // The bar's rounded ends: the first share rounds the bar's left
        // corners, the last its right ones.
        let first = widths.iter().position(|w| *w > 0.0);
        let last = widths.iter().rposition(|w| *w > 0.0);
        let r = bounds.height / 2.0;
        let mut x = 0.0;
        for (i, (w, color)) in widths.into_iter().zip(colors).enumerate() {
            if w > 0.0 {
                let left = if Some(i) == first { r } else { 0.0 };
                let right = if Some(i) == last { r } else { 0.0 };
                let gap = if Some(i) == last { 0.0 } else { 1.0 };
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(x, 0.0),
                        Size::new((w - gap).max(1.0), bounds.height),
                        Radius {
                            top_left: left,
                            bottom_left: left,
                            top_right: right,
                            bottom_right: right,
                        },
                    ),
                    color,
                );
            }
            x += w;
        }
        vec![frame.into_geometry()]
    }
}

// --------------------------------------------------------------------- keys

/// `KeyedSubtree(key: ValueKey(key), child: content)`: `content` with its
/// own widget state for as long as `key` stays the same; a new key starts it
/// afresh (iced matches state by position, so a different channel shown in
/// the same place would otherwise inherit the last one's).
fn keyed<'a, M: 'a>(key: String, content: Element<'a, M>) -> Element<'a, M> {
    Element::new(Keyed { key, content })
}

struct Keyed<'a, M> {
    key: String,
    content: Element<'a, M>,
}

/// The key the subtree's state was made for.
struct KeyState(String);

impl<M> Widget<M, Theme, Renderer> for Keyed<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<KeyState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(KeyState(self.key.clone()))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<KeyState>();
        if state.0 == self.key {
            tree.diff_children(std::slice::from_ref(&self.content));
        } else {
            state.0.clone_from(&self.key);
            tree.children = vec![Tree::new(&self.content)];
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
