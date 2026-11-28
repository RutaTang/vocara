//! Port of `ui/song_practice_detail/widgets/mixer_strip.dart`: the Mixer tab
//! of the bottom dock — one console-style channel strip per track (mute /
//! solo / pan / a vertical volume fader with its meters / the dB field /
//! output routing), aligned left-to-right like a mixing desk, then the FX
//! buses, then the master strip and the add-bus tile; with the native
//! effects engine, the input strip (record and input monitoring, the
//! monitor level, the record gain over the input's meter) leads. The
//! selected channel is lit; clicking a strip selects it, and a right click
//! opens its menu (delete; a bus's rename and delete).
//!
//! Drawn as the approved mixer mockup draws it (`DESIGN.md`, Mixer strip):
//! strips 84 px wide (the input 72, the master 104) on the desk's darker
//! ground, each with its 3 px colour cap, its name, M / S (the input: R /
//! I), the pan knob and its value, the fader (`controls::draw_fader`) and
//! its meters (`controls::draw_meter`), the dB field and the output's
//! pop-up; the groups (input | takes | buses | master) split by a 1 px
//! divider. The engine reports no per-channel level, so the take, bus and
//! master meters stand empty; the input's reads the live input.
//!
//! # Plain data in, messages out
//!
//! The Dart desk read the view model. Here it takes what it shows
//! ([`MixerStrip`]) and reports a [`Message`] per call the Dart desk made,
//! which the screen routes to the same method (see [`Message`]'s variants
//! for each one's Dart call). A fader or knob reports live edits on every
//! move and its commit once on release.
//!
//! Each strip keeps its widget state by what it shows (Dart keyed them
//! `track:<id>`, `bus:<id>` and `master`): unkeyed, a strip removed before
//! one being dragged handed that fader's state, and the rest of its drag, to
//! the next lane's strip, and the master's drag stopped following the
//! pointer as a take came or went.
//!
//! A trackpad's two-finger swipe reached Flutter as a pan-zoom gesture,
//! which the faders' and knobs' drags (`kPointerDragDevices`) left to the
//! desk's scroll; iced hands it over as wheel events (with their phases,
//! `widgets::pan_zoom`), which the faders and knobs take none of, and the
//! desk's horizontal view drags and flings with it (`fading_scrollbar`).
//!
//! # Differences from Flutter
//!
//! - `StableSubtree` kept unchanged strips from rebuilding at pointer rate;
//!   iced rebuilds the view, and each fader's drawing is cached by its
//!   level, so a tick redraws the one fader that moved (see
//!   `core::widgets::stable_subtree`).
//! - Unmounting mid-drag: Dart saved a fader's level when a sideways wheel
//!   scrolled its strip out of the lazy list. iced's strip list is not lazy:
//!   the strip stays while the list scrolls, and the release that ends the
//!   drag reaches it wherever the pointer is. A strip the screen stops
//!   showing mid-drag owes its commit to the screen.
//! - No pointer cancel or second finger reaches a desktop mouse control, so
//!   those Dart cases have no counterpart.
//! - The rename and delete prompts are the host's: the desk reports
//!   [`Message::RenameBus`] / [`Message::DeleteBus`] where the Dart desk
//!   opened `showRenameDialog` / `confirmDeleteBus` and then called the
//!   mixer. A take's delete moved from its strip's trash icon to its right
//!   click menu (the bus's always had one).

#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::ui::core::text;
use iced::alignment::Horizontal;
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, responsive, row, stack, Column,
    Space,
};
use iced::{
    mouse, Background, Border, Color, Element, Length, Padding, Point, Rectangle, Renderer, Size,
    Theme,
};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::fx_bus::FxBus;
use vocara_services::app_settings_service::AppSettingsService;

use super::mixer_shared::{keyed_row, tap, MenuItem, Opens, PopupMenu};
use super::track_row::{bus_color, take_color};
use crate::ui::core::theme::{self, alpha, font, radius, type_scale, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};
use crate::ui::core::widgets::controls::{
    draw_fader, draw_meter, labelled, msr_button, MsrKind, FADER_CAP, FADER_METER_GAP, FADER_WIDTH,
    METER_GAP, METER_WIDTH,
};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::line_icon::{glyphs, line_icon_inherit};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::opacity_layer::opacity_layer;
use crate::ui::core::widgets::painter::KeyedCache;
use crate::ui::core::widgets::studio_controls::{fader_db, studio_pan_knob, PAN_KNOB_SIZE};
use crate::ui::core::widgets::studio_text::studio_text;

/// The desk's data: the Dart widget's `tracks`, and what it read from the
/// view model.
#[derive(Debug, Clone, Copy)]
pub struct MixerStrip<'a> {
    /// One strip each, left to right.
    pub tracks: &'a [Arc<AudioTrack>],
    /// `vm.mixer.fxBuses`.
    pub buses: &'a [Arc<FxBus>],
    /// `vm.mixer.supportsEffects`: the mic-input strip, the bus section and
    /// the add-bus tile are only meaningful on the native engine (device I/O,
    /// monitoring, effects).
    pub supports_effects: bool,
    /// `vm.selectedTrackId`.
    pub selected_track_id: Option<&'a str>,
    /// `vm.selectedBusId`.
    pub selected_bus_id: Option<&'a str>,
    /// `vm.mixer.masterVolume`.
    pub master_volume: f64,
    /// What the mic-input strip shows.
    pub mic: MicInput,
}

/// The mic-input strip's data: the global audio settings it is wired to and
/// the shared input meter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicInput {
    /// `settings.recordGainDb`.
    pub record_gain_db: f64,
    /// `settings.monitorGain` (0..1).
    pub monitor_gain: f64,
    /// `settings.monitorAllowed`: hearing yourself is possible on this
    /// output (not a speaker that would feed back).
    pub monitor_allowed: bool,
    /// `settings.suggestHeadphonesMark`: a locked monitor can be unlocked by
    /// marking the output as headphones in Settings.
    pub suggest_headphones_mark: bool,
    /// `vm.recording.inputLevel` (0..1).
    pub input_level: f64,
    /// `vm.recording.inputMonitorEnabled`: the mic is armed (I lit).
    pub monitoring: bool,
    /// A take in flight, recording or still arming (R lit; a press ends
    /// it), as the toolbar's Record reads it.
    pub taking: bool,
}

impl Default for MicInput {
    fn default() -> Self {
        Self {
            record_gain_db: AppSettingsService::RECORD_GAIN_MIN_DB,
            monitor_gain: 0.0,
            monitor_allowed: true,
            suggest_headphones_mark: false,
            input_level: 0.0,
            monitoring: false,
            taking: false,
        }
    }
}

/// What the desk reports: each the call the Dart desk made.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `vm.selectTrack(id)`: a take's strip was clicked.
    SelectTrack { id: String },
    /// `vm.selectBus(id)`: a bus's strip was clicked.
    SelectBus { id: String },
    /// `vm.mixer.toggleMute(id)`.
    ToggleMute { id: String },
    /// `vm.mixer.toggleSolo(id)`.
    ToggleSolo { id: String },
    /// `vm.mixer.setTrackVolumeLive(id, volume)`.
    SetTrackVolumeLive { id: String, volume: f64 },
    /// `vm.mixer.commitTrackVolume()`: a take's fader or pan knob released.
    CommitTrackVolume,
    /// `vm.mixer.setTrackPanLive(id, pan)`.
    SetTrackPanLive { id: String, pan: f64 },
    /// `vm.mixer.setTrackBus(id, busId)`: a take's output (`None` = master).
    SetTrackBus { id: String, bus_id: Option<String> },
    /// `onDeleteTrack(track)`: the take strip's menu's "Delete take" (a
    /// right click on the strip).
    DeleteTrack { id: String },
    /// `vm.mixer.toggleBusMute(id)`.
    ToggleBusMute { id: String },
    /// `vm.mixer.toggleBusSolo(id)`.
    ToggleBusSolo { id: String },
    /// `vm.mixer.setBusVolumeLive(id, volume)`.
    SetBusVolumeLive { id: String, volume: f64 },
    /// `vm.mixer.setBusPanLive(id, pan)`.
    SetBusPanLive { id: String, pan: f64 },
    /// `vm.commitFx()`: a bus's fader or pan knob released.
    CommitFx,
    /// `vm.mixer.setBusOutput(id, output)`: a bus's output (`None` =
    /// master).
    SetBusOutput {
        id: String,
        output_bus_id: Option<String>,
    },
    /// The bus menu's Rename: the host asks for a name
    /// (`showRenameDialog(current: name, title: 'Rename bus', hint: 'Bus
    /// name')`) and, unless cancelled, calls `vm.mixer.renameBus(id, name)`.
    RenameBus { id: String, current: String },
    /// The bus strip's menu's "Delete bus": the host asks first (`confirmDeleteBus(name)`, as the timeline lane's Delete does:
    /// the bus's FX chain and every send into it go with it, with no undo)
    /// and, confirmed, calls `vm.mixer.deleteBus(id)`.
    DeleteBus { id: String, name: String },
    /// `vm.mixer.createBus()`: the add-bus tile.
    CreateBus,
    /// `vm.mixer.setMasterVolumeLive(gain)`.
    SetMasterVolumeLive { gain: f64 },
    /// `vm.mixer.commitMasterVolume()`.
    CommitMasterVolume,
    /// `vm.appSettings.setRecordGainDbLive(db)`.
    SetRecordGainDbLive { db: f64 },
    /// `vm.appSettings.setMonitorGainLive(gain)`.
    SetMonitorGainLive { gain: f64 },
    /// `vm.appSettings.commitAudioGains()`: the record gain's fader or the
    /// monitor knob released.
    CommitAudioGains,
    /// The input strip's R with no take in flight: start one, as the
    /// toolbar's Record does (`recording.startRecording`).
    StartRecording,
    /// R while a take is recording or arming: end (or cancel) it
    /// (`vm.stopRecording`).
    StopRecording,
    /// The input strip's I: arm or disarm the mic, as the toolbar's input
    /// monitoring does (`recording.toggleInputMonitor`).
    ToggleInputMonitor,
}

/// A strip's key in the desk (Dart's `ValueKey`s; the rest by place).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StripKey {
    Mic,
    MicRule,
    Track(u64),
    BusRule,
    Bus(u64),
    AddBus,
    MasterRule,
    Master,
}

fn id_key(id: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

/// Inner strip height (px) below which a strip switches to its scrolling
/// fallback: the strip's fixed rows — the 3 px cap, the 26 px name, the M /
/// S row (18 + 8), the pan knob with its label (26 + 2 + 10 + 6), the fader
/// area's padding (4 + 6), the 20 px dB field and the 30 px output row,
/// 159 px in all — plus ~56 px of minimum usable fader travel.
pub const STRIP_MIN_CONTENT_HEIGHT: f32 = 215.0;

/// Fixed fader height used in the squeezed scrolling fallback, where the
/// column's height is unbounded so the fader cannot fill it.
pub const SQUEEZED_FADER_HEIGHT: f32 = 120.0;

/// A channel strip's width (`DESIGN.md`, Mixer strip), the input strip's
/// and the master's.
pub const STRIP_WIDTH: f32 = 84.0;
pub const INPUT_STRIP_WIDTH: f32 = 72.0;
pub const MASTER_STRIP_WIDTH: f32 = 104.0;
/// The gap between two strips.
pub const STRIP_GAP: f32 = 6.0;
/// The desk's padding.
pub const DESK_PADDING: Padding = Padding {
    top: 10.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};
/// The add-bus tile's width.
pub const ADD_BUS_WIDTH: f32 = 40.0;

/// The desk's background (`#131316`).
const DESK: Color = Color::from_rgb8(0x13, 0x13, 0x16);
/// A strip's inset outline, and a selected strip's fill and outline.
const STRIP_EDGE: Color = Color::from_rgb8(0x24, 0x24, 0x2B);
const SELECTED_FILL: Color = Color::from_rgb8(0x20, 0x20, 0x26);
const SELECTED_EDGE: Color = Color::from_rgb8(0x4A, 0x4A, 0x54);

/// The fader's scale ticks, from the top (the mockup's).
pub const FADER_TICKS: [f32; 7] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.75, 1.0];

/// The text of the add-bus tile.
pub const ADD_BUS_LABEL: &str = "Bus";

impl<'a> MixerStrip<'a> {
    /// The desk over `tracks` and `buses`: nothing selected, the master at
    /// unity, no effects engine.
    pub fn new(tracks: &'a [Arc<AudioTrack>], buses: &'a [Arc<FxBus>]) -> Self {
        Self {
            tracks,
            buses,
            supports_effects: false,
            selected_track_id: None,
            selected_bus_id: None,
            master_volume: 1.0,
            mic: MicInput::default(),
        }
    }

    /// The desk: a horizontally scrolling row of strips on the desk's
    /// background — the input | the takes | the buses | the master, the
    /// groups split by a 1 px divider, then the add-bus tile.
    pub fn view(self) -> Element<'a, Message> {
        let buses = self.buses;
        // Nothing to mix. Not on the native engine: its input strip holds
        // the record gain and the monitor level, which want setting before
        // the first take (a cappella, or a song with nothing imported yet).
        if self.tracks.is_empty() && buses.is_empty() && !self.supports_effects {
            return container(center(
                text("No tracks yet")
                    .size(type_scale::BODY)
                    .color(Studio::TEXT_FAINT),
            ))
            .style(theme::fill(DESK))
            .into();
        }
        let mut desk = keyed_row().height(Length::Fill).padding(DESK_PADDING);
        // The input strip: the live input's meter, record gain and monitor
        // (hear-yourself) gain, record and input monitoring. Only
        // meaningful on the native engine (device I/O + monitoring).
        if self.supports_effects {
            desk = desk
                .push(StripKey::Mic, gap_after(input_channel(self.mic)))
                .push(StripKey::MicRule, divider());
        }
        for t in self.tracks {
            let selected =
                self.selected_track_id == Some(t.id.as_str()) && self.selected_bus_id.is_none();
            // Backing tracks stay dry → no output routing.
            let routes: &[Arc<FxBus>] = if t.is_backing() { &[] } else { buses };
            desk = desk.push(
                StripKey::Track(id_key(&t.id)),
                gap_after(track_channel(t, track_accent(t), selected, routes)),
            );
        }
        // FX buses (DAW groups). Only meaningful on the native effects
        // engine.
        if self.supports_effects && !buses.is_empty() {
            desk = desk.push(StripKey::BusRule, divider());
            for b in buses {
                let selected = self.selected_bus_id == Some(b.id.as_str());
                desk = desk.push(
                    StripKey::Bus(id_key(&b.id)),
                    gap_after(bus_channel(b, selected, buses)),
                );
            }
        }
        // The master bus strip — the whole mix sums here.
        desk = desk.push(StripKey::MasterRule, divider()).push(
            StripKey::Master,
            gap_after(master_channel(self.master_volume)),
        );
        if self.supports_effects {
            desk = desk.push(StripKey::AddBus, add_bus_tile());
        }
        // The horizontal `ListView`: Material's scroll behavior gave a
        // horizontal scrollable no scrollbar.
        container(
            scrollable(desk)
                .horizontal()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::fill(DESK))
        .into()
    }
}

/// A strip and the gap after it.
fn gap_after<'a>(strip: Element<'a, Message>) -> Element<'a, Message> {
    container(strip)
        .height(Length::Fill)
        .padding(Padding {
            right: STRIP_GAP,
            ..Padding::ZERO
        })
        .into()
}

/// The 1 px `BORDER` divider between groups, 6 px clear of the strip
/// before it (after its gap) and 12 of the one after.
fn divider<'a>() -> Element<'a, Message> {
    container(
        container(Space::new())
            .width(1)
            .height(Length::Fill)
            .style(theme::fill(Studio::BORDER)),
    )
    .height(Length::Fill)
    .padding(Padding {
        top: 0.0,
        right: 12.0,
        bottom: 0.0,
        left: 6.0,
    })
    .into()
}

// The preset lane tints and the default take tint are the context menu's
// (`kTrackColorPalette` and `defaultTakeColor` lived in
// `track_context_menu.dart`).
pub use super::track_context_menu::{default_take_color, TRACK_COLOR_PALETTE};

/// A take strip's colour cap: its lane's colour in the arrange
/// (`track_row::take_color`), so the strip and the lane read as one
/// channel.
pub fn track_accent(track: &AudioTrack) -> Color {
    take_color(track)
}

/// A bus strip's colour cap: its lane's (`track_row::bus_color`).
pub fn bus_accent(bus: &FxBus) -> Color {
    bus_color(bus)
}

/// Display name of the bus `id` routes to, or "Master" when it's
/// none/missing.
pub fn bus_name<'b>(buses: &'b [Arc<FxBus>], id: Option<&str>) -> &'b str {
    id.and_then(|id| buses.iter().find(|b| b.id == id))
        .map_or("Master", |b| b.name.as_str())
}

/// A strip's frame (`DESIGN.md`, Mixer strip): `width` wide, radius 6,
/// `SURFACE` with an inset `#24242B` outline (selected: `#202026` with
/// `#4A4A54`), the 3 px colour cap along its top.
fn strip_frame<'a>(
    content: Element<'a, Message>,
    width: f32,
    cap: Color,
    selected: bool,
) -> Element<'a, Message> {
    let cap =
        container(Space::new().width(Length::Fill).height(3)).style(move |_| container::Style {
            background: Some(Background::Color(cap)),
            border: Border {
                radius: Radius {
                    top_left: radius::WELL,
                    top_right: radius::WELL,
                    bottom_right: 0.0,
                    bottom_left: 0.0,
                },
                ..Border::default()
            },
            ..container::Style::default()
        });
    container(
        column![cap, content]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(width)
    .height(Length::Fill)
    .style(move |_| {
        theme::boxed(
            Some(if selected {
                SELECTED_FILL
            } else {
                Studio::SURFACE
            }),
            radius::WELL,
            Some(if selected { SELECTED_EDGE } else { STRIP_EDGE }),
        )
    })
    .into()
}

/// `_StripBody`: at normal dock heights, the plain column — the fader fills
/// the leftover height, console style. Squeezed below
/// [`STRIP_MIN_CONTENT_HEIGHT`], a vertically scrollable column with the
/// fader at a fixed height instead, rather than collapsing the fader to
/// nothing and clipping the dB field and the output.
fn strip_body<'a>(build: impl Fn(bool) -> Vec<Element<'a, Message>> + 'a) -> Element<'a, Message> {
    responsive(move |size| {
        if size.height >= STRIP_MIN_CONTENT_HEIGHT {
            Column::with_children(build(false))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .into()
        } else {
            scrollable(
                Column::with_children(build(true))
                    .width(Length::Fill)
                    .align_x(Horizontal::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .fading()
            .into()
        }
    })
    .into()
}

/// The fader slot inside a strip column (its 4 / 6 px padding): fills the
/// leftover height at normal dock heights, a fixed (still usable) height in
/// the squeezed fallback.
fn fader_slot<'a>(squeezed: bool, child: Element<'a, Message>) -> Element<'a, Message> {
    container(child)
        .width(Length::Fill)
        .height(if squeezed {
            Length::Fixed(SQUEEZED_FADER_HEIGHT + 10.0)
        } else {
            Length::Fill
        })
        .padding(Padding {
            top: 4.0,
            right: 0.0,
            bottom: 6.0,
            left: 0.0,
        })
        .align_x(Horizontal::Center)
        .into()
}

/// A strip's name: 26 px, 11 px medium, centred, ellipsized (white when
/// the strip is selected).
fn title<'a>(label: &str, selected: bool) -> Element<'a, Message> {
    container(
        studio_text(label.to_string())
            .size(type_scale::LABEL)
            .font(font::MEDIUM)
            .line_height(iced::Pixels(type_scale::LINE))
            .color(if selected {
                Color::WHITE
            } else {
                Studio::TEXT_PRIMARY
            })
            .align_x(Horizontal::Center)
            .width(Length::Fill)
            .ellipsis(1),
    )
    .width(Length::Fill)
    .height(26)
    .padding(Padding::from([0, 6]))
    .center_y(26)
    .into()
}

/// A strip's buttons row (M / S, R / I): 22 × 18, 3 px apart, 8 px above
/// the knob.
fn buttons_row<'a>(buttons: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    container(row(buttons).spacing(3)).height(18.0 + 8.0).into()
}

/// The pan knob and its value (26 px; 6 px under it).
fn knob_slot<'a>(knob: Element<'a, Message>) -> Element<'a, Message> {
    container(knob)
        .padding(Padding {
            bottom: 6.0,
            ..Padding::ZERO
        })
        .into()
}

/// What the buttons row and the knob slot take, for the master strip,
/// which has neither (its fader lines up with the others').
const BUTTONS_AND_KNOB: f32 = 18.0 + 8.0 + PAN_KNOB_SIZE + 2.0 + KNOB_LABEL_HEIGHT + 6.0;

/// A knob's value label's height: 10 px text on a 10 px line (`studio_text`'s
/// default line).
const KNOB_LABEL_HEIGHT: f32 = 10.0;

/// A level with a true minus sign.
fn minus_sign(text: String) -> String {
    text.replace('-', "\u{2212}")
}

/// A strip's level as its dB field reads it: "−1.4", "0.0", "−∞".
pub fn strip_db(volume: f64) -> String {
    minus_sign(fader_db(volume))
}

/// The input strip's record gain as its dB field reads it: "+12.0".
pub fn record_gain_label(db: f64) -> String {
    minus_sign(format!("{db:+.1}"))
}

/// The dB field (`DESIGN.md`, Fader): 20 px, `INSET`, radius 4, 8 px in
/// from the strip's sides, 11 px tabular `TEXT_PRIMARY`.
fn db_field<'a>(label: String) -> Element<'a, Message> {
    container(
        container(
            studio_text(label)
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_PRIMARY)
                .wrapping(Wrapping::None),
        )
        .width(Length::Fill)
        .height(20)
        .center_x(Length::Fill)
        .center_y(20)
        .style(|_| theme::boxed(Some(Studio::INSET), radius::SMALL, None)),
    )
    .padding(Padding::from([0, 8]))
    .width(Length::Fill)
    .into()
}

/// The output row (30 px): the output's pop-up, centred.
fn output_row<'a>(output: Option<Element<'a, Message>>) -> Element<'a, Message> {
    container(output.unwrap_or_else(|| Space::new().into()))
        .width(Length::Fill)
        .height(30)
        .center_x(Length::Fill)
        .center_y(30)
        .into()
}

/// A strip's output pop-up's face: 20 px, radius 4, `SURFACE_RAISED` with
/// an inset `BORDER_STRONG` outline (`CONTROL_ACTIVE` under the pointer
/// when it opens a menu), 10 px `TEXT_SECONDARY`, 8 px in; as wide as the
/// strip allows, ellipsized.
fn output_face<'a>(label: &str, live: bool) -> Element<'a, Message> {
    animated_container(
        studio_text(label.to_string())
            .size(type_scale::AXIS)
            .line_height(iced::Pixels(14.0))
            .color(Studio::TEXT_SECONDARY)
            .ellipsis(1),
        std::time::Duration::ZERO,
    )
    .height(20)
    .padding(Padding::from([0, 8]))
    .align((0.0, 0.0))
    .style(move |hovered| {
        Decoration::boxed(
            if hovered && live {
                Studio::CONTROL_ACTIVE
            } else {
                Studio::SURFACE_RAISED
            },
            radius::SMALL,
            Studio::BORDER_STRONG,
            1.0,
        )
    })
    .into()
}

/// The output's pop-up, opening its destinations (Master first) in a menu
/// just below it.
fn route_popup<'a>(label: &str, options: Vec<MenuItem<Message>>) -> Element<'a, Message> {
    container(PopupMenu::new(
        output_face(label, true),
        options,
        ROUTE_MENU_WIDTH,
        Opens::BelowOnTap,
    ))
    .padding(Padding::from([0, 6]))
    .into()
}

/// An output that routes nowhere else (the backing's, a take's with no
/// bus, the master's): its face alone.
fn fixed_output<'a>(label: &str) -> Element<'a, Message> {
    container(output_face(label, false))
        .padding(Padding::from([0, 6]))
        .into()
}

/// The route and bus menus' width.
const ROUTE_MENU_WIDTH: f32 = 160.0;

/// A strip's fader and its meters: a fader column (`controls::draw_fader`)
/// and `meters` channel meters beside it (`controls::draw_meter`), as wide
/// as they are, as tall as the slot.
fn fader_meters<'a>(
    value: f64,
    meters: Vec<f32>,
    input: Option<FaderInput<'a>>,
) -> Element<'a, Message> {
    let width = fader_meters_width(meters.len());
    canvas_widget(VerticalFader {
        value: value.clamp(0.0, 1.0),
        meters,
        input,
    })
    .width(width)
    .height(Length::Fill)
    .into()
}

/// The width of a fader with `meters` meters beside it.
pub fn fader_meters_width(meters: usize) -> f32 {
    if meters == 0 {
        FADER_WIDTH
    } else {
        FADER_WIDTH
            + FADER_METER_GAP
            + meters as f32 * METER_WIDTH
            + (meters - 1) as f32 * METER_GAP
    }
}

/// A stereo channel's meters, at rest: the engine reports no per-channel
/// level, so a take's, a bus's and the master's meters stand empty.
const STEREO_AT_REST: [f32; 2] = [0.0, 0.0];

/// `_Channel`: a take's strip. A right click opens its menu (delete).
fn track_channel<'a>(
    t: &'a AudioTrack,
    accent: Color,
    selected: bool,
    buses: &'a [Arc<FxBus>],
) -> Element<'a, Message> {
    let id = t.id.as_str();
    let body = strip_body(move |squeezed| {
        let output = if !t.is_backing() && !buses.is_empty() {
            let routed = t.bus_id.as_deref();
            let mut options = vec![MenuItem::new(
                "Master",
                vec![Message::SetTrackBus {
                    id: id.to_string(),
                    bus_id: None,
                }],
            )
            .selected(routed.is_none())];
            for b in buses {
                options.push(
                    MenuItem::new(
                        b.name.clone(),
                        vec![Message::SetTrackBus {
                            id: id.to_string(),
                            bus_id: Some(b.id.clone()),
                        }],
                    )
                    .selected(routed == Some(b.id.as_str())),
                );
            }
            route_popup(bus_name(buses, routed), options)
        } else {
            fixed_output("Master")
        };
        vec![
            title(&t.display_name, selected),
            buttons_row(vec![
                msr_button(
                    MsrKind::Mute,
                    t.muted,
                    Some(Message::ToggleMute { id: id.to_string() }),
                )
                .into(),
                msr_button(
                    MsrKind::Solo,
                    t.soloed,
                    Some(Message::ToggleSolo { id: id.to_string() }),
                )
                .into(),
            ]),
            knob_slot(
                studio_pan_knob(
                    t.pan,
                    move |pan| Message::SetTrackPanLive {
                        id: id.to_string(),
                        pan,
                    },
                    Message::CommitTrackVolume,
                )
                .into(),
            ),
            fader_slot(
                squeezed,
                fader_meters(
                    t.volume,
                    STEREO_AT_REST.to_vec(),
                    Some(FaderInput {
                        on_changed: Box::new(move |volume| Message::SetTrackVolumeLive {
                            id: id.to_string(),
                            volume,
                        }),
                        on_change_end: Message::CommitTrackVolume,
                    }),
                ),
            ),
            db_field(strip_db(t.volume)),
            output_row(Some(output)),
        ]
    });
    let frame = strip_frame(body, STRIP_WIDTH, accent, selected);
    let selectable = tap(
        frame,
        vec![Message::SelectTrack { id: id.to_string() }],
        None,
    );
    PopupMenu::new(
        selectable,
        vec![MenuItem::new(
            if t.is_backing() {
                "Delete"
            } else {
                "Delete take"
            },
            vec![Message::DeleteTrack { id: id.to_string() }],
        )
        .danger(true)],
        ROUTE_MENU_WIDTH,
        Opens::AtSecondaryPress,
    )
    .into()
}

/// `_BusChannel`: an FX bus's strip — select (opens its FX in the
/// inspector), mute/solo, pan, fader, output routing; rename and delete on
/// a right click, at the click (anchored below this full-height strip, the
/// menu opened at the bottom of the mixer).
fn bus_channel<'a>(b: &'a FxBus, selected: bool, buses: &'a [Arc<FxBus>]) -> Element<'a, Message> {
    let id = b.id.as_str();
    let body = strip_body(move |squeezed| {
        let mut options = vec![MenuItem::new(
            "Master",
            vec![Message::SetBusOutput {
                id: id.to_string(),
                output_bus_id: None,
            }],
        )
        .selected(b.output_bus_id.is_none())];
        for other in buses.iter().filter(|o| o.id != b.id) {
            options.push(
                MenuItem::new(
                    other.name.clone(),
                    vec![Message::SetBusOutput {
                        id: id.to_string(),
                        output_bus_id: Some(other.id.clone()),
                    }],
                )
                .selected(b.output_bus_id.as_deref() == Some(other.id.as_str())),
            );
        }
        vec![
            title(&b.name, selected),
            buttons_row(vec![
                msr_button(
                    MsrKind::Mute,
                    b.muted,
                    Some(Message::ToggleBusMute { id: id.to_string() }),
                )
                .into(),
                msr_button(
                    MsrKind::Solo,
                    b.soloed,
                    Some(Message::ToggleBusSolo { id: id.to_string() }),
                )
                .into(),
            ]),
            knob_slot(
                studio_pan_knob(
                    b.pan,
                    move |pan| Message::SetBusPanLive {
                        id: id.to_string(),
                        pan,
                    },
                    Message::CommitFx,
                )
                .into(),
            ),
            fader_slot(
                squeezed,
                fader_meters(
                    b.volume,
                    STEREO_AT_REST.to_vec(),
                    Some(FaderInput {
                        on_changed: Box::new(move |volume| Message::SetBusVolumeLive {
                            id: id.to_string(),
                            volume,
                        }),
                        on_change_end: Message::CommitFx,
                    }),
                ),
            ),
            db_field(strip_db(b.volume)),
            output_row(Some(route_popup(
                bus_name(buses, b.output_bus_id.as_deref()),
                options,
            ))),
        ]
    });
    let frame = strip_frame(body, STRIP_WIDTH, bus_accent(b), selected);
    let selectable = tap(frame, vec![Message::SelectBus { id: id.to_string() }], None);
    PopupMenu::new(
        selectable,
        vec![
            MenuItem::new(
                "Rename",
                vec![Message::RenameBus {
                    id: id.to_string(),
                    current: b.name.clone(),
                }],
            ),
            MenuItem::new("Delete bus", vec![delete_bus(b)]).danger(true),
        ],
        ROUTE_MENU_WIDTH,
        Opens::AtSecondaryPress,
    )
    .into()
}

/// Deleting `b` (asked first by the host, see [`Message::DeleteBus`]).
fn delete_bus(b: &FxBus) -> Message {
    Message::DeleteBus {
        id: b.id.clone(),
        name: b.name.clone(),
    }
}

/// The add-bus tile that adds a new FX bus to the mixer: 40 px wide,
/// radius 6, a dashed `GRID_STRONG` outline, a plus over "Bus" in
/// `TEXT_FAINT` (`TEXT_SECONDARY` under the pointer).
fn add_bus_tile<'a>() -> Element<'a, Message> {
    let face = column![
        line_icon_inherit(glyphs::PLUS, 12.0),
        studio_text(ADD_BUS_LABEL)
            .size(type_scale::AXIS)
            .line_height(iced::Pixels(14.0)),
    ]
    .spacing(4)
    .align_x(Horizontal::Center);
    let tile = stack![
        canvas_widget(DashedOutline)
            .width(Length::Fill)
            .height(Length::Fill),
        container(face).center(Length::Fill),
    ];
    let target = button(tile)
        .padding(0)
        .width(ADD_BUS_WIDTH)
        .height(Length::Fill)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(alpha(Studio::HOVER, 0.5))),
                text_color: if hovered {
                    Studio::TEXT_SECONDARY
                } else {
                    Studio::TEXT_FAINT
                },
                border: Border {
                    radius: radius::WELL.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        })
        .on_press(Message::CreateBus);
    labelled(material_tooltip(target, "Add a bus", 6.0), "Add a bus")
}

/// The add-bus tile's dashed outline (`1px dashed #34343D`, radius 6).
struct DashedOutline;

impl canvas::Program<Message> for DashedOutline {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.stroke(
            &Path::rounded_rectangle(
                Point::new(0.5, 0.5),
                Size::new(bounds.width - 1.0, bounds.height - 1.0),
                Radius::from(radius::WELL - 0.5),
            ),
            Stroke {
                line_dash: canvas::LineDash {
                    segments: &[3.0, 3.0],
                    offset: 0,
                },
                ..Stroke::default()
                    .with_color(Studio::GRID_STRONG)
                    .with_width(1.0)
            },
        );
        vec![frame.into_geometry()]
    }
}

/// `_MasterChannel`: like a take's strip but it controls the overall
/// output gain (no mute/solo, no pan, no delete); 104 px wide, the master's
/// colour cap, its output the stereo out.
fn master_channel<'a>(value: f64) -> Element<'a, Message> {
    let body = strip_body(move |squeezed| {
        vec![
            title("Master", false),
            // Where the M/S row and the pan knob sit on a take's strip, so
            // the fader lines up with the channels' faders.
            Space::new().height(BUTTONS_AND_KNOB).into(),
            fader_slot(
                squeezed,
                fader_meters(
                    value,
                    STEREO_AT_REST.to_vec(),
                    Some(FaderInput {
                        on_changed: Box::new(|gain| Message::SetMasterVolumeLive { gain }),
                        on_change_end: Message::CommitMasterVolume,
                    }),
                ),
            ),
            db_field(strip_db(value)),
            output_row(Some(fixed_output(STEREO_OUT))),
        ]
    });
    strip_frame(body, MASTER_STRIP_WIDTH, Studio::TRACK_MASTER, false)
}

/// The master's destination.
pub const STEREO_OUT: &str = "Stereo Out";

/// The record-gain fader's position for a record gain (dB).
pub fn record_gain_position(db: f64) -> f64 {
    let (min, max) = (
        AppSettingsService::RECORD_GAIN_MIN_DB,
        AppSettingsService::RECORD_GAIN_MAX_DB,
    );
    ((db - min) / (max - min)).clamp(0.0, 1.0)
}

/// The record gain (dB) at a record-gain fader position.
pub fn record_gain_at(position: f64) -> f64 {
    let (min, max) = (
        AppSettingsService::RECORD_GAIN_MIN_DB,
        AppSettingsService::RECORD_GAIN_MAX_DB,
    );
    min + position * (max - min)
}

/// The monitor knob's readout: "spkr" while locked, "off" at nothing, else
/// the percentage.
pub fn monitor_readout(mic: &MicInput) -> String {
    if !mic.monitor_allowed {
        "spkr".to_string()
    } else if mic.monitor_gain <= 0.001 {
        "off".to_string()
    } else {
        format!("{}%", (mic.monitor_gain * 100.0).round() as i64)
    }
}

/// Why the monitor is locked, and where to unlock it when it can be: none
/// while it is allowed.
pub fn monitor_lock_message(mic: &MicInput) -> Option<&'static str> {
    if mic.monitor_allowed {
        None
    } else if mic.suggest_headphones_mark {
        Some(
            "Hearing yourself is off here so speakers can't feed back. Listening on \
             headphones? Turn on \"Headphones only\" in Settings.",
        )
    } else {
        Some("Hearing yourself is off on this output so speakers can't feed back.")
    }
}

/// The monitor knob's tooltip while it is allowed.
pub const MONITOR_TIP: &str = "Monitor level";

/// `_MicInputChannel`, the input strip (72 px, the record colour's cap):
/// R starts a take (or ends the one in flight, lit while it is) and I arms
/// input monitoring (the meter goes live), as the toolbar's Record and
/// input monitoring do; the knob sets how loud you hear yourself (0 = off;
/// locked on a speaker, where it would feed back); the fader sets how loud
/// the voice records, its meter the live input; the dB field reads the
/// record gain.
fn input_channel<'a>(mic: MicInput) -> Element<'a, Message> {
    let body = strip_body(move |squeezed| {
        let knob = level_knob(
            if mic.monitor_allowed {
                mic.monitor_gain
            } else {
                0.0
            },
            monitor_readout(&mic),
            mic.monitor_allowed,
        );
        // A locked monitor says why, and where to unlock it when it can be.
        let knob: Element<'a, Message> = match monitor_lock_message(&mic) {
            // Material's tooltip prefers below.
            Some(why) => material_tooltip(knob, why, 0.0).max_width(280.0).into(),
            None => material_tooltip(knob, MONITOR_TIP, 0.0).into(),
        };
        vec![
            title("Input", false),
            buttons_row(vec![
                msr_button(
                    MsrKind::Record,
                    mic.taking,
                    Some(if mic.taking {
                        Message::StopRecording
                    } else {
                        Message::StartRecording
                    }),
                )
                .into(),
                // Lit as solo is (`WARN`) while the mic is armed.
                msr_button(
                    MsrKind::Solo,
                    mic.monitoring,
                    Some(Message::ToggleInputMonitor),
                )
                .label("I")
                .into(),
            ]),
            knob_slot(knob),
            fader_slot(
                squeezed,
                fader_meters(
                    record_gain_position(mic.record_gain_db),
                    vec![mic.input_level as f32],
                    Some(FaderInput {
                        on_changed: Box::new(|p| Message::SetRecordGainDbLive {
                            db: record_gain_at(p),
                        }),
                        on_change_end: Message::CommitAudioGains,
                    }),
                ),
            ),
            db_field(record_gain_label(mic.record_gain_db)),
            output_row(None),
        ]
    });
    strip_frame(body, INPUT_STRIP_WIDTH, Studio::RECORD, false)
}

// ------------------------------------------------------------- level knob

/// How many pixels of horizontal drag sweep the monitor knob over its whole
/// range.
pub const LEVEL_DRAG_PIXELS: f64 = 160.0;

/// The level a drag reaches from `from` after a horizontal move of `dx`.
pub fn level_after_drag(from: f64, dx: f64) -> f64 {
    (from + dx / LEVEL_DRAG_PIXELS).clamp(0.0, 1.0)
}

/// The input strip's monitor knob: the pan knob's look (`DESIGN.md`, Pan
/// knob: a 270° track, a 6.5 px cap with its pointer) for a level from 0
/// to 1, its value arc from the start of the track; its reading under it
/// at 10 px. Drag sideways to set it (the release commits the audio
/// gains); disabled, it is drawn at 35 % and takes no input.
fn level_knob<'a>(value: f64, readout: String, enabled: bool) -> Element<'a, Message> {
    let knob: Element<'a, Message> = canvas_widget(LevelKnob {
        value: value.clamp(0.0, 1.0),
        enabled,
    })
    .width(PAN_KNOB_SIZE)
    .height(PAN_KNOB_SIZE)
    .into();
    let knob: Element<'a, Message> = if enabled {
        knob
    } else {
        opacity_layer(0.35, knob).into()
    };
    column![
        knob,
        Space::new().height(2),
        studio_text(readout)
            .size(type_scale::AXIS)
            .tabular()
            .color(if enabled {
                Studio::TEXT_FAINT
            } else {
                Studio::TEXT_DISABLED
            })
            .width(PAN_KNOB_SIZE + 8.0)
            .align_x(Horizontal::Center),
    ]
    .align_x(Horizontal::Center)
    .into()
}

struct LevelKnob {
    value: f64,
    enabled: bool,
}

/// A knob drag: where it was pressed and the level then.
#[derive(Debug, Default)]
struct KnobDrag {
    from: Option<(f32, f64)>,
    moved: bool,
}

impl canvas::Program<Message> for LevelKnob {
    type State = KnobDrag;

    fn update(
        &self,
        state: &mut KnobDrag,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if !self.enabled {
            return None;
        }
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                *state = KnobDrag {
                    from: Some((p.x, self.value)),
                    moved: false,
                };
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let (x, from) = state.from?;
                state.moved = true;
                let gain = level_after_drag(from, f64::from(position.x - x));
                Some(canvas::Action::publish(Message::SetMonitorGainLive { gain }).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.from.take()?;
                let moved = std::mem::take(&mut state.moved);
                Some(if moved {
                    canvas::Action::publish(Message::CommitAudioGains).and_capture()
                } else {
                    canvas::Action::capture()
                })
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &KnobDrag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        use iced::widget::canvas::path::Arc;
        use iced::Radians;
        let mut frame = Frame::new(renderer, bounds.size());
        let unit = bounds.width.min(bounds.height) / 26.0;
        let c = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let arc = |from: f32, to: f32| {
            Path::new(|b| {
                b.arc(Arc {
                    center: c,
                    radius: 10.0 * unit,
                    start_angle: Radians(from),
                    end_angle: Radians(to),
                });
            })
        };
        let line = |color: Color, width: f32| {
            Stroke::default()
                .with_color(color)
                .with_width(width * unit)
                .with_line_cap(LineCap::Round)
        };
        // Angles from the +x axis, clockwise on screen: the top is -90°.
        let top = -std::f32::consts::FRAC_PI_2;
        let sweep = 0.75 * std::f32::consts::PI;
        let start = top - sweep;
        frame.stroke(&arc(start, top + sweep), line(Studio::CONTROL_ACTIVE, 2.2));
        let value = self.value as f32;
        let to = start + value * 2.0 * sweep;
        if value >= 0.005 {
            frame.stroke(&arc(start, to), line(Studio::TEXT_SECONDARY, 2.2));
        }
        let cap = Path::circle(c, 6.5 * unit);
        frame.fill(&cap, Color::from_rgb8(0x20, 0x20, 0x26));
        frame.stroke(
            &cap,
            Stroke::default()
                .with_color(Studio::SEGMENT_SELECTED)
                .with_width(unit),
        );
        let dir = iced::Vector::new(to.cos(), to.sin());
        frame.stroke(
            &Path::line(c, c + dir * (5.5 * unit)),
            line(Studio::TEXT_PRIMARY, 1.6),
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &KnobDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if self.enabled && (state.from.is_some() || cursor.is_over(bounds)) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}

// ------------------------------------------------------------------ fader

/// The fader's travel inset at each end: the cap's half height, so the
/// cap's centre runs the travel.
pub const FADER_PAD: f32 = FADER_CAP.height / 2.0;

/// The level a vertical fader `height` tall reads at `dy` from its top.
/// The span is at least 1 px: laid out below 2 × the pad (the master strip
/// squeezed to the minimum dock height), a span under 1 would have flipped
/// the clamp's bounds (Dart's clamp threw there).
pub fn vertical_fader_value(dy: f32, height: f32) -> f64 {
    let pad = f64::from(FADER_PAD);
    let span = (f64::from(height) - 2.0 * pad).max(1.0);
    (1.0 - (f64::from(dy) - pad) / span).clamp(0.0, 1.0)
}

/// A console-style vertical volume fader (`controls::draw_fader`: a 3 px
/// slot, scale ticks, the 21 × 12 cap) and its meters
/// (`controls::draw_meter`). Drag up for louder, down for quieter; the
/// whole column, meters included, takes the pointer.
///
/// It takes its pointer on contact: a press sets the level at once; every
/// move while held sets it from the pointer's y; the release saves it, once,
/// whether the press was a click or turned into a drag.
struct VerticalFader<'a> {
    value: f64,
    /// Each meter's level (0 to 1), left to right.
    meters: Vec<f32>,
    /// What it reports; none when disabled (`IgnorePointer`).
    input: Option<FaderInput<'a>>,
}

struct FaderInput<'a> {
    on_changed: Box<dyn Fn(f64) -> Message + 'a>,
    on_change_end: Message,
}

/// A drag in flight: it has moved the level and owes its save.
#[derive(Default)]
struct FaderState {
    active: bool,
    cache: KeyedCache<(u64, Vec<u32>, u32, u32)>,
}

#[cfg(test)]
thread_local! {
    /// How many times a fader recorded its drawing (its cache missed).
    static FADER_DRAWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn fader_draws() -> usize {
    FADER_DRAWS.with(std::cell::Cell::get)
}

impl canvas::Program<Message> for VerticalFader<'_> {
    type State = FaderState;

    fn update(
        &self,
        state: &mut FaderState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let input = self.input.as_ref()?;
        let level_at = |p: Point| vertical_fader_value(p.y - bounds.y, bounds.height);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                state.active = true;
                Some(canvas::Action::publish((input.on_changed)(level_at(p))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => {
                let p = match cursor {
                    mouse::Cursor::Available(p) | mouse::Cursor::Levitating(p) => p,
                    mouse::Cursor::Unavailable => return None,
                };
                Some(canvas::Action::publish((input.on_changed)(level_at(p))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.active =>
            {
                state.active = false;
                Some(canvas::Action::publish(input.on_change_end.clone()).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &FaderState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let key = (
            self.value.to_bits(),
            self.meters.iter().map(|m| m.to_bits()).collect(),
            size.width.to_bits(),
            size.height.to_bits(),
        );
        vec![state.cache.draw(renderer, size, key, |frame| {
            #[cfg(test)]
            FADER_DRAWS.with(|n| n.set(n.get() + 1));
            draw_fader(
                frame,
                Rectangle::new(Point::ORIGIN, Size::new(FADER_WIDTH, size.height)),
                self.value as f32,
                &FADER_TICKS,
            );
            for (i, level) in self.meters.iter().enumerate() {
                let x = FADER_WIDTH + FADER_METER_GAP + i as f32 * (METER_WIDTH + METER_GAP);
                draw_meter(
                    frame,
                    Rectangle::new(Point::new(x, 0.0), Size::new(METER_WIDTH, size.height)),
                    *level,
                );
            }
        })]
    }

    fn mouse_interaction(
        &self,
        _state: &FaderState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if self.input.is_some() && cursor.is_over(bounds) {
            mouse::Interaction::ResizingVertically
        } else {
            mouse::Interaction::default()
        }
    }
}
