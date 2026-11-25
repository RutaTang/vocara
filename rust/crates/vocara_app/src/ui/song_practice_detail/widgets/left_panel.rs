//! Port of `ui/song_practice_detail/widgets/left_panel.dart`: the left
//! inspector — a Logic-style channel strip for the selected take or FX bus
//! (its FX inserts above, the channel-strip footer pinned below: sends,
//! output routing, mute/solo, pan, the fader and its dB), collapsible to a
//! thin rail — and [`voice_summary_view`], the take's voice-analysis
//! summary the bottom dock's "Analysis Summary" tab hosts.
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
//!   which menu is open ([`Inspector::open_menu`]); the chip that opens it
//!   is a [`menu_host`] anchored below the chip (each chip's own, so the
//!   menu drops under the chip clicked). Picking a row reports its action;
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
    mouse, Color, Element, Event, Length, Padding, Pixels, Point, Rectangle, Renderer, Size, Theme,
    Vector,
};
use vocara_model::models::audio_fx::{TrackSend, MAX_SENDS, SEND_OFF_DB};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::voice_metrics::{
    note_name_for_midi, Ltas, RegisterMap, Vibrato, VoiceMetrics, VoiceProfile,
};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::round_to_i64;
use vocara_model::util::format::format_clock_ms;

use super::detail_transport_bar::flexible_row;
use super::pitch_lane::{note_vibrato_of, pitch_in_tune_percent, pitch_notes_of, PitchNote};
use super::track_context_menu::{
    argb, menu_host, studio_menu, MenuAnchor, StudioMenuItem, MENU_ANCHOR_BELOW_DY,
};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, boxed, fill, font, Studio};
use crate::ui::core::widgets::cap_label::cap_label;
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::studio_controls::{fader_db_readout, studio_fader, studio_pan_knob};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::{fill_spaced_text, studio_text};
use crate::ui::song_practice_detail::view_model::analysis_cache::DockTab;
use crate::util::to_string_as_fixed;

#[cfg(test)]
mod tests;

/// The open inspector's width (`_panelWidth`).
pub const PANEL_WIDTH: f32 = 272.0;

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

// Pass / warn / flag indicator colors for the scorecard dots.
const GOOD: Color = Studio::GOOD;
const WARN: Color = Color::from_rgb8(0xE0, 0xA0, 0x4C);
const FLAG: Color = Studio::RECORD;

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
    /// The OUTPUT chip's destinations.
    Output(Channel),
    /// "+ Send"'s buses no slot feeds yet.
    AddSend(Channel),
    /// A send slot's target chip: its own target plus the unused ones.
    RetargetSend(Channel, usize),
}

/// What the inspector and the voice summary ask of the detail view model.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The rail, or the header's collapse button (`vm.toggleInspectorPanel`).
    ToggleInspectorPanel,
    /// A chip was tapped: open its menu (`showStudioMenu`).
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

/// `_TakeHeader`'s dot: the take's own tint, else the accent for a backing
/// and `Studio.waveform` for a take.
pub fn take_dot_color(track: &AudioTrack) -> Color {
    match track.color_value {
        Some(v) => argb(v),
        None if track.is_backing() => Studio::ACCENT,
        None => Studio::WAVEFORM,
    }
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

/// The line under a take's name: what it is (the take's label, or
/// "Original" / "Accompaniment" for a backing), its length and its date.
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
        take_label(&track.id, vocal_takes, interpretation)
    };
    format!(
        "{sub}  ·  {}  ·  {}",
        format_clock_ms(track.duration_ms()),
        fmt_date(&track.created_at)
    )
}

/// The OUTPUT chip's label: the destination bus's name, else
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

/// The buses "+ Send" offers: those no slot already feeds. Two sends into
/// one bus feed it twice, so the bus hears the sum and neither fader reads
/// what you hear.
pub fn addable_targets<'b>(sends: &[TrackSend], targets: &[&'b FxBus]) -> Vec<&'b FxBus> {
    targets
        .iter()
        .filter(|b| !sends.iter().any(|s| s.bus_id == b.id))
        .copied()
        .collect()
}

/// Whether "+ Send" shows: a bus left to feed, and a free slot.
pub fn can_add_send(sends: &[TrackSend], targets: &[&FxBus]) -> bool {
    !addable_targets(sends, targets).is_empty() && sends.len() < MAX_SENDS
}

/// The buses a slot's target chip offers: its own target, plus the ones no
/// slot feeds.
pub fn retarget_options<'b>(
    sends: &[TrackSend],
    slot: usize,
    targets: &[&'b FxBus],
) -> Vec<&'b FxBus> {
    targets
        .iter()
        .filter(|b| sends[slot].bus_id == b.id || !sends.iter().any(|s| s.bus_id == b.id))
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
    /// The dock tab a tap opens.
    pub tab: DockTab,
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
            tab: DockTab::Timbre,
        },
        SummaryCell {
            label: "Ring",
            value: ltas.map_or_else(|| "—".to_string(), |l| whole_db(l.singers_formant_db)),
            sub: "singer's formant".to_string(),
            dot: None,
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
            tab: DockTab::Dynamics,
        },
        SummaryCell {
            label: "Brightness",
            value: ltas.map_or_else(|| "—".to_string(), |l| whole_db(l.alpha_ratio)),
            sub: "α ratio".to_string(),
            dot: None,
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
    let content = column![header(on), hline(), container(body).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);
    container(row![content, vline()])
        .width(PANEL_WIDTH)
        .height(Length::Fill)
        .style(fill(Studio::SURFACE))
        .into()
}

/// `_Rail`: the thin collapsed rail (mirrors the right panel's rail,
/// flipped) — the open-panel icon over a rotated "INSPECTOR"; a tap
/// anywhere opens the strip.
fn rail<'a, M: Clone + 'a>(on: fn(Message) -> M) -> Element<'a, M> {
    let content = column![
        Space::new().height(12),
        icons::icon(Icon::PanelLeftOpen, 15.0, Studio::TEXT_SECONDARY),
        Space::new().height(18),
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
        .style(bare_button)
        .on_press(on(Message::ToggleInspectorPanel));
    container(row![face, vline()])
        .width(RAIL_WIDTH)
        .height(Length::Fill)
        .style(fill(Studio::SURFACE))
        .into()
}

/// The rail's "INSPECTOR", 11 px w700, 1.5 letter spacing, turned a quarter
/// clockwise
/// (`RotatedBox(quarterTurns: 1)`): it reads top to bottom from the top of
/// its box, centred across the rail.
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
        fill_spaced_text(
            &mut frame,
            canvas::Text {
                content: "INSPECTOR".to_string(),
                position: Point::ORIGIN,
                color: Studio::TEXT_SECONDARY,
                size: Pixels(11.0),
                font: font::BOLD,
                align_x: Alignment::Left,
                align_y: Vertical::Center,
                ..canvas::Text::default()
            },
            1.5,
        );
        vec![frame.into_geometry()]
    }
}

/// `_Header`: "INSPECTOR" and the collapse button, 40 px tall.
fn header<'a, M: Clone + 'a>(on: fn(Message) -> M) -> Element<'a, M> {
    container(
        row![
            studio_text("INSPECTOR")
                .size(11)
                .font(font::BOLD)
                .letter_spacing(0.6)
                .color(Studio::TEXT_PRIMARY),
            Space::new().width(Length::Fill),
            button(
                container(icons::icon(
                    Icon::PanelLeftClose,
                    15.0,
                    Studio::TEXT_SECONDARY
                ))
                .padding(8)
            )
            .padding(0)
            .style(bare_button)
            .on_press(on(Message::ToggleInspectorPanel)),
        ]
        .align_y(Vertical::Center),
    )
    .height(40)
    .padding(Padding {
        left: 14.0,
        right: 6.0,
        ..Padding::ZERO
    })
    .align_y(Vertical::Center)
    .into()
}

/// The body for a selected take: a fixed take header, then (for a vocal
/// take, when the engine supports effects) the FX panel, else a note that
/// the backing plays dry; the channel-strip footer pinned below. Analysis
/// moved to the bottom dock's summary tab.
fn take_body<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    track: &'a AudioTrack,
    fx_panel: impl Fn() -> Element<'a, M> + 'a,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let is_vocal = !track.is_backing() && insp.supports_effects;
    responsive(move |size| {
        let header = container(take_header(insp, track))
            .padding(Padding {
                top: 12.0,
                right: 14.0,
                bottom: 0.0,
                left: 14.0,
            })
            .width(Length::Fill);
        let footer = channel_footer(
            insp,
            Strip {
                channel: Channel::Track(track.id.clone()),
                sends: &track.sends,
                targets: insp.fx_buses.iter().map(Arc::as_ref).collect(),
                dest: track.bus_id.as_deref(),
                outputs: insp.fx_buses.iter().map(Arc::as_ref).collect(),
                show_routing: is_vocal,
                muted: track.muted,
                soloed: track.soloed,
                pan: track.pan,
                volume: track.volume,
            },
            on,
        );
        let dry = || empty("Backing track plays dry, with no effects.", false, false);
        if size.height < MIN_PINNED_HEIGHT {
            // Degenerate height (tiny window with the dock open): scroll the
            // whole strip. The FX panel needs a bounded height, so it gets a
            // fixed window inside the scrolling column.
            let middle: Element<'a, M> = if is_vocal {
                container(container(fx_panel()).height(SCROLLED_FX_HEIGHT))
                    .padding(fx_padding())
                    .width(Length::Fill)
                    .into()
            } else {
                container(dry())
                    .width(Length::Fill)
                    .height(SCROLLED_DRY_HEIGHT)
                    .into()
            };
            scrolled(column![header, middle, footer])
        } else {
            let middle: Element<'a, M> = if is_vocal {
                container(fx_panel())
                    .padding(fx_padding())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                container(dry())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            };
            column![header, middle, footer]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    })
    .into()
}

/// Inspector body for a selected FX bus: a header, the (bus-targeted) FX
/// panel and the bus's channel-strip footer.
fn bus_body<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    bus: &'a FxBus,
    fx_panel: impl Fn() -> Element<'a, M> + 'a,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    responsive(move |size| {
        let header = container(bus_header(bus))
            .padding(Padding {
                top: 12.0,
                right: 14.0,
                bottom: 0.0,
                left: 14.0,
            })
            .width(Length::Fill);
        // A bus never routes or sends into itself: the feedback loop is not
        // even offered.
        let others: Vec<&'a FxBus> = insp
            .fx_buses
            .iter()
            .map(Arc::as_ref)
            .filter(|b| b.id != bus.id)
            .collect();
        let footer = channel_footer(
            insp,
            Strip {
                channel: Channel::Bus(bus.id.clone()),
                sends: &bus.sends,
                targets: others.clone(),
                dest: bus.output_bus_id.as_deref(),
                outputs: others,
                show_routing: true,
                muted: bus.muted,
                soloed: bus.soloed,
                pan: bus.pan,
                volume: bus.volume,
            },
            on,
        );
        // The take strip's degenerate-height fallback, for the same reason:
        // pinned, the header and footer overflowed a short panel and pushed
        // the bus's fader, mute/solo and pan under the dock.
        if size.height < MIN_PINNED_HEIGHT {
            let fx = container(container(fx_panel()).height(SCROLLED_FX_HEIGHT))
                .padding(fx_padding())
                .width(Length::Fill);
            scrolled(column![header, fx, footer])
        } else {
            let fx = container(fx_panel())
                .padding(fx_padding())
                .width(Length::Fill)
                .height(Length::Fill);
            column![header, fx, footer]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    })
    .into()
}

fn fx_padding() -> Padding {
    Padding {
        top: 12.0,
        right: 14.0,
        bottom: 0.0,
        left: 14.0,
    }
}

/// The strip as one scrolling column.
fn scrolled<'a, M: 'a>(content: Column<'a, M>) -> Element<'a, M> {
    scrollable(content.width(Length::Fill))
        .fading()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// `_BusBody`'s header: the bus's dot and name, and "FX BUS".
fn bus_header<'a, M: 'a>(bus: &'a FxBus) -> Element<'a, M> {
    let dot_color = bus.color_value.map_or(Studio::ACCENT, argb);
    row![
        dot(dot_color),
        Space::new().width(8),
        studio_text(bus.name.as_str())
            .size(15)
            .font(font::BOLD)
            .color(Studio::TEXT_PRIMARY)
            .width(Length::Fill)
            .ellipsis(1),
        studio_text("FX BUS")
            .size(9)
            .font(font::BOLD)
            .letter_spacing(0.8)
            .color(Studio::TEXT_FAINT),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// `_TakeHeader`: take identity — color dot, name (two lines at most),
/// interpretation/take #, duration + date.
fn take_header<'a, M: 'a>(insp: Inspector<'a>, track: &'a AudioTrack) -> Element<'a, M> {
    column![
        row![
            dot(take_dot_color(track)),
            Space::new().width(8),
            studio_text(track.display_name.as_str())
                .size(13.5)
                .font(font::BOLD)
                .line_height(1.25)
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .ellipsis(2),
        ]
        .align_y(Vertical::Center),
        Space::new().height(8),
        text(take_subtitle(
            track,
            insp.vocal_takes,
            insp.active_interpretation
        ))
        .size(11)
        .color(Studio::TEXT_FAINT),
    ]
    .width(Length::Fill)
    .into()
}

/// A 9 px disc.
fn dot<'a, M: 'a>(color: Color) -> Element<'a, M> {
    disc(9.0, color)
}

fn disc<'a, M: 'a>(size: f32, color: Color) -> Element<'a, M> {
    container(Space::new().width(size).height(size))
        .style(move |_| boxed(Some(color), size / 2.0, None))
        .into()
}

/// What a channel-strip footer shows and drives.
struct Strip<'a> {
    channel: Channel,
    sends: &'a [TrackSend],
    /// The buses a send may feed.
    targets: Vec<&'a FxBus>,
    /// The output's bus, `None` for the stereo out.
    dest: Option<&'a str>,
    /// The buses the output may route to.
    outputs: Vec<&'a FxBus>,
    /// Sends and output routing (a vocal take, or a bus).
    show_routing: bool,
    muted: bool,
    soloed: bool,
    pan: f64,
    volume: f64,
}

/// `_TrackChannelFooter` / `_BusChannelFooter`: the Logic-style channel-strip
/// footer — sends and output routing (when shown), mute/solo on the left
/// and pan on the right, then the fader filling the width with its level to
/// the right. The same widgets for a take and a bus, through each one's own
/// view-model methods: a take's pan and fader commit the take's volume, a
/// bus's commit its FX.
fn channel_footer<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    strip: Strip<'a>,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let channel = strip.channel.clone();
    let mut col = Column::new().width(Length::Fill);
    if strip.show_routing {
        if let Some(sends) = sends_section(insp, &strip, on) {
            col = col.push(sends);
        }
        col = col
            .push(
                row![
                    strip_label("OUTPUT"),
                    // Flush right in the rest of the row, and bounded by it: a
                    // long bus name gives way in the chip instead of running
                    // past the inspector's edge.
                    container(output_chip(insp, &strip, on))
                        .width(Length::Fill)
                        .align_x(Horizontal::Right),
                ]
                .align_y(Vertical::Center),
            )
            .push(Space::new().height(10));
    }
    let pan_channel = channel.clone();
    let volume_channel = channel.clone();
    col = col
        // Mute / solo on the left, pan on the right.
        .push(
            row![
                ms_toggle("M", strip.muted, Studio::RECORD, on(channel.toggle_mute())),
                Space::new().width(8),
                ms_toggle("S", strip.soloed, Studio::ACCENT, on(channel.toggle_solo())),
                Space::new().width(Length::Fill),
                studio_pan_knob(
                    strip.pan,
                    move |p| on(pan_channel.pan_live(p)),
                    on(channel.mix_commit()),
                ),
            ]
            .align_y(Vertical::Center),
        )
        .push(Space::new().height(14))
        // The fader fills the width; its level reads to the right.
        .push(
            row![
                container(studio_fader(
                    strip.volume,
                    Studio::ACCENT,
                    move |v| on(volume_channel.volume_live(v)),
                    on(channel.mix_commit()),
                ))
                .width(Length::Fill),
                Space::new().width(8),
                fader_db_readout(strip.volume),
            ]
            .align_y(Vertical::Center),
        );
    column![
        hline(),
        container(col).padding(Padding {
            top: 10.0,
            right: 14.0,
            bottom: 12.0,
            left: 14.0,
        }),
    ]
    .width(Length::Fill)
    .into()
}

/// `_StripLabel`: a caption for the footer's rows (9 px w700, 0.5 letter
/// spacing, faint).
fn strip_label<'a, M: 'a>(label: &'a str) -> Element<'a, M> {
    studio_text(label)
        .size(9)
        .font(font::BOLD)
        .letter_spacing(0.5)
        .color(Studio::TEXT_FAINT)
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

/// A chip that opens `menu` below itself (`menuAnchorBelow(anchor)`).
fn menu_chip<'a, M: Clone + 'a>(
    chip: Element<'a, M>,
    menu: InspectorMenu,
    opened: Option<Element<'a, M>>,
    on: fn(Message) -> M,
) -> Element<'a, M> {
    let trigger = button(chip)
        .padding(0)
        .style(bare_button)
        .on_press(on(Message::OpenMenu(menu)));
    menu_host(trigger, opened, on(Message::DismissMenu))
        .anchor(MenuAnchor::Below(MENU_ANCHOR_BELOW_DY))
        .into()
}

/// `_OutputChip` (and the bus footer's): "Stereo Out" or a bus name; tap to
/// choose.
fn output_chip<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    strip: &Strip<'a>,
    on: fn(Message) -> M,
) -> Element<'a, M> {
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
    let name = output_name(dest, insp.fx_buses);
    // 9 / 5, and the chip's 1 px border: a decoration's border pads its
    // child too.
    let chip = container(flexible_row(
        vec![
            studio_text(name)
                .size(11)
                .color(Studio::TEXT_SECONDARY)
                .ellipsis(1)
                .into(),
            Space::new().width(4).into(),
            icons::icon(Icon::ChevronDown, 14.0, Studio::TEXT_FAINT).into(),
        ],
        0,
    ))
    .padding([6, 10])
    .style(|_| boxed(Some(Studio::INSET), 6.0, Some(Studio::BORDER)));
    menu_chip(chip.into(), menu, opened, on)
}

/// `_SendsSection`: Logic-style sends — one row per post-fader aux send
/// (target bus chip + an independent level fader + remove), plus a
/// "+ Send" to add one. Nothing at all with no sends and none to add.
fn sends_section<'a, M: Clone + 'a>(
    insp: Inspector<'a>,
    strip: &Strip<'a>,
    on: fn(Message) -> M,
) -> Option<Element<'a, M>> {
    let sends = strip.sends;
    let addable = addable_targets(sends, &strip.targets);
    let can_add = can_add_send(sends, &strip.targets);
    if sends.is_empty() && !can_add {
        return None;
    }
    let channel = &strip.channel;
    let mut heading =
        row![strip_label("SENDS"), Space::new().width(Length::Fill)].align_y(Vertical::Center);
    if can_add {
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
        let chip = container(
            row![
                icons::icon(Icon::Plus, 11.0, Studio::TEXT_SECONDARY),
                Space::new().width(3),
                text("Send").size(10).color(Studio::TEXT_SECONDARY),
            ]
            .align_y(Vertical::Center),
        )
        // 7 / 3, and the chip's 1 px border.
        .padding([4, 8])
        .style(|_| boxed(Some(Studio::SURFACE_RAISED), 5.0, Some(Studio::BORDER)));
        heading = heading.push(menu_chip(chip.into(), menu, opened, on));
    }
    let mut col = column![heading].width(Length::Fill);
    for (i, send) in sends.iter().enumerate() {
        let menu = InspectorMenu::RetargetSend(channel.clone(), i);
        let opened = open_menu(
            insp,
            &menu,
            || {
                retarget_options(sends, i, &strip.targets)
                    .iter()
                    .map(|b| {
                        StudioMenuItem::new(b.name.clone(), on(channel.retarget_send(i, &b.id)))
                    })
                    .collect()
            },
            SEND_MENU_WIDTH,
        );
        let chip = container(
            studio_text(send_name(&send.bus_id, &strip.targets))
                .size(10)
                .color(Studio::TEXT_SECONDARY)
                .ellipsis(1),
        )
        .width(Length::Fill)
        // 6 / 3, and the chip's 1 px border.
        .padding([4, 7])
        .style(|_| boxed(Some(Studio::INSET), 4.0, Some(Studio::BORDER)));
        let level_channel = channel.clone();
        col = col.push(Space::new().height(6)).push(
            row![
                container(menu_chip(chip.into(), menu, opened, on)).width(66),
                Space::new().width(6),
                container(studio_fader(
                    send_fader_value(send.level_db),
                    Studio::ACCENT,
                    move |v| on(level_channel.send_level(i, send_level_at(v))),
                    on(Message::CommitFx),
                ))
                .width(Length::Fill),
                Space::new().width(6),
                container(
                    studio_text(fmt_send_db(send.level_db))
                        .size(9)
                        .tabular()
                        .color(Studio::TEXT_SECONDARY)
                        .wrapping(Wrapping::None),
                )
                .width(26)
                .align_x(Horizontal::Right),
                Space::new().width(4),
                button(icons::icon(Icon::X, 13.0, Studio::TEXT_FAINT))
                    .padding(0)
                    .style(bare_button)
                    .on_press(on(channel.remove_send(i))),
            ]
            .align_y(Vertical::Center),
        );
    }
    Some(col.push(Space::new().height(10)).into())
}

/// `_MsToggle`: a small 30 × 22 mute/solo toggle, tinted `color` when on.
fn ms_toggle<'a, M: Clone + 'a>(
    label: &'a str,
    active: bool,
    color: Color,
    on_tap: M,
) -> Element<'a, M> {
    let face = container(text(label).size(11).font(font::BOLD).color(if active {
        color
    } else {
        Studio::TEXT_SECONDARY
    }))
    .center_x(30)
    .center_y(22)
    .style(move |_| {
        boxed(
            Some(if active {
                alpha(color, 0.2)
            } else {
                Studio::SURFACE_RAISED
            }),
            5.0,
            Some(if active {
                alpha(color, 0.6)
            } else {
                Studio::BORDER
            }),
        )
    });
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

/// A 1 px `Studio.border` line across.
fn hline<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(fill(Studio::BORDER))
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

/// `VoiceSummaryView`: the voice-analysis summary for the selected take —
/// range, in-tune %, vibrato, clarity, dynamics, brightness, register.
/// Hosted in the bottom dock's "Analysis Summary" tab (moved out of the
/// inspector, which is now a channel strip).
pub fn voice_summary_view<'a>(summary: VoiceSummary<'a>) -> Element<'a, Message> {
    let Some(track) = summary.selected_track else {
        return empty("Select a take to see its analysis.", true, false);
    };
    if track.is_backing() {
        return empty("Backing track has no voice analysis.", true, false);
    }
    scrollable(
        container(voice_summary(summary))
            .width(Length::Fill)
            .padding(Padding {
                top: 14.0,
                right: 16.0,
                bottom: 18.0,
                left: 16.0,
            }),
    )
    .fading()
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// `_VoiceSummary`: the "scorecard" — a quality banner when the take can't
/// be trusted, the range, a grid of vital metrics and the register split.
fn voice_summary<'a>(s: VoiceSummary<'a>) -> Element<'a, Message> {
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
    col = col
        .push(
            studio_text("VOICE SUMMARY")
                .size(10)
                .font(font::BOLD)
                .letter_spacing(0.8)
                .color(Studio::TEXT_SECONDARY),
        )
        .push(Space::new().height(10))
        .push(range_row(profile))
        .push(Space::new().height(14))
        .push(grid(summary_cells(m, profile, s.ltas)));
    if let Some(reg) = s.register.filter(|r| r.has_data()) {
        col = col.push(Space::new().height(16)).push(register_bar(reg));
    }
    col.into()
}

/// `_qualityBanner`: a red banner when the selected take is clipped or too
/// noisy to trust.
fn quality_banner<'a>(message: String) -> Element<'a, Message> {
    container(
        container(
            row![
                icons::icon(Icon::TriangleAlert, 14.0, Studio::RECORD),
                Space::new().width(7),
                text(message)
                    .size(11)
                    .line_height(1.3)
                    .color(Studio::TEXT_SECONDARY)
                    .width(Length::Fill),
            ]
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        // 10 / 8, and the banner's 1 px border.
        .padding([9, 11])
        .style(|_| {
            boxed(
                Some(alpha(Studio::RECORD, 0.12)),
                8.0,
                Some(alpha(Studio::RECORD, 0.4)),
            )
        }),
    )
    .padding(Padding {
        bottom: 12.0,
        ..Padding::ZERO
    })
    .into()
}

/// `_Tappable`: a click cursor and a tap opening `tab`.
fn tappable<'a>(content: impl Into<Element<'a, Message>>, tab: DockTab) -> Element<'a, Message> {
    button(content)
        .padding(0)
        .width(Length::Fill)
        .style(bare_button)
        .on_press(Message::ShowDockTab(tab))
        .into()
}

/// `_RangeRow`: the sung-range line and a mini bar (tessitura highlight and
/// passaggio marker); "No pitch detected" without a range.
fn range_row<'a>(p: &'a VoiceProfile) -> Element<'a, Message> {
    let Some((range, span)) = range_line(p) else {
        return text("No pitch detected")
            .size(12)
            .color(Studio::TEXT_FAINT)
            .into();
    };
    tappable(
        column![
            row![
                text(range)
                    .size(15)
                    .font(font::EXTRA_BOLD)
                    .color(Studio::TEXT_PRIMARY),
                Space::new().width(6),
                text(format!("{span} st"))
                    .size(11)
                    .color(Studio::TEXT_FAINT),
                Space::new().width(Length::Fill),
                cap_label("Range"),
            ]
            .align_y(Vertical::Bottom),
            Space::new().height(7),
            canvas_widget(RangeBar { profile: p })
                .width(Length::Fill)
                .height(8),
        ]
        .width(Length::Fill),
        DockTab::Range,
    )
}

/// `_RangeBar`: the track, the tessitura highlight, end caps and the
/// passaggio marker.
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
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, h / 2.0 - 1.5),
                Size::new(w, 3.0),
                Radius::from(2.0),
            ),
            Studio::INSET,
        );
        // Tessitura highlight.
        if let (Some(t_lo), Some(t_hi)) = (p.tess_lo_midi, p.tess_hi_midi) {
            let (left, right) = (x(t_lo), x(t_hi));
            if right > left {
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(left, h / 2.0 - 2.5),
                        Size::new(right - left, 5.0),
                        Radius::from(3.0),
                    ),
                    Studio::ACCENT,
                );
            }
        }
        // End caps.
        frame.fill(
            &Path::circle(Point::new(2.0, h / 2.0), 2.5),
            Studio::TEXT_SECONDARY,
        );
        frame.fill(
            &Path::circle(Point::new(w - 2.0, h / 2.0), 2.5),
            Studio::TEXT_SECONDARY,
        );
        // Passaggio marker.
        if let Some(passaggio) = p.passaggio_midi {
            frame.fill_rectangle(
                Point::new(x(passaggio) - 1.0, 0.0),
                Size::new(2.0, h),
                Studio::ACCENT_BRIGHT,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The scorecard: two cells a row, 8 px apart, 4 px between rows.
fn grid<'a>(cells: [SummaryCell; 6]) -> Element<'a, Message> {
    let mut rows = Column::new().width(Length::Fill);
    let mut cells = cells.into_iter();
    while let Some(first) = cells.next() {
        let second: Element<'a, Message> = match cells.next() {
            Some(cell) => metric(cell),
            None => Space::new().width(Length::Fill).into(),
        };
        rows = rows.push(
            container(
                row![
                    container(metric(first)).width(Length::Fill),
                    Space::new().width(8),
                    container(second).width(Length::Fill),
                ]
                .align_y(Vertical::Top),
            )
            .padding(Padding {
                bottom: 4.0,
                ..Padding::ZERO
            }),
        );
    }
    rows.into()
}

/// `_Metric`: one scorecard cell — label, big value, sub-line, an optional
/// pass/warn dot; a tap opens the matching dock tab.
fn metric<'a>(cell: SummaryCell) -> Element<'a, Message> {
    let mut top =
        row![cap_label(cell.label), Space::new().width(Length::Fill)].align_y(Vertical::Center);
    if let Some(color) = cell.dot {
        top = top.push(disc(7.0, color));
    }
    tappable(
        container(column![
            top,
            Space::new().height(4),
            studio_text(cell.value)
                .size(16)
                .font(font::EXTRA_BOLD)
                .color(Studio::TEXT_PRIMARY)
                .ellipsis(1),
            studio_text(cell.sub)
                .size(9.5)
                .color(Studio::TEXT_FAINT)
                .ellipsis(1),
        ])
        .width(Length::Fill)
        // 10 / 9, and the cell's 1 px border.
        .padding([10, 11])
        .style(|_| boxed(Some(Studio::INSET), 8.0, Some(Studio::BORDER))),
        cell.tab,
    )
}

/// `_RegisterBar`: the chest / mix / head split as a caption and a 7 px
/// bar, its ends rounded.
fn register_bar<'a>(reg: &'a RegisterMap) -> Element<'a, Message> {
    tappable(
        column![
            row![
                cap_label("Register"),
                Space::new().width(Length::Fill),
                text(register_line(reg)).size(10).color(Studio::TEXT_FAINT),
            ]
            .align_y(Vertical::Center),
            Space::new().height(7),
            canvas_widget(RegisterSplit { reg })
                .width(Length::Fill)
                .height(7),
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
        let widths = register_widths(self.reg, bounds.width);
        let colors = [CHEST, mix_color(), HEAD];
        // The bar's 3 px rounded clip: the first share rounds the bar's left
        // corners, the last its right ones.
        let first = widths.iter().position(|w| *w > 0.0);
        let last = widths.iter().rposition(|w| *w > 0.0);
        let mut x = 0.0;
        for (i, (w, color)) in widths.into_iter().zip(colors).enumerate() {
            if w > 0.0 {
                let left = if Some(i) == first { 3.0 } else { 0.0 };
                let right = if Some(i) == last { 3.0 } else { 0.0 };
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(x, 0.0),
                        Size::new(w, bounds.height),
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
