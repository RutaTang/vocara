//! Port of `ui/song_practice_detail/widgets/mixer_strip.dart`: the MIXER tab
//! of the bottom dock — one console-style channel strip per track (mute /
//! solo / pan / a vertical volume fader / output routing / delete), aligned
//! left-to-right like a mixing desk, then the FX buses and "+ Bus", then the
//! master strip set apart on the right; with the native effects engine, the
//! mic-input strip (input meter, record gain, monitor gain) leads. The
//! selected channel is tinted; clicking a strip selects it.
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
//!   those Dart cases have no counterpart; a trackpad's two-finger swipe is
//!   a wheel event the faders and knobs ignore, and the desk scrolls.
//! - The rename and delete-bus prompts are the host's: the desk reports
//!   [`Message::RenameBus`] / [`Message::DeleteBus`] where the Dart desk
//!   opened `showRenameDialog` / `confirmDeleteBus` and then called the
//!   mixer.
//! - iced text has no ellipsis (one-line labels clip), no letter spacing and
//!   no tabular figures; the disabled MON fader fades its colors (iced has
//!   no subtree opacity).

#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, responsive, row, scrollable, text,
    tooltip, Column, Space,
};
use iced::{mouse, Color, Element, Length, Padding, Point, Rectangle, Renderer, Size, Theme};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::fx_bus::FxBus;
use vocara_services::app_settings_service::AppSettingsService;

use super::mixer_shared::{argb, keyed_row, tap, MenuItem, Opens, PopupMenu};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::painter::{color_key, KeyedCache};
use crate::ui::core::widgets::studio_controls::{fader_db, studio_pan_knob};

/// The desk's data: the Dart widget's `tracks`, and what it read from the
/// view model.
#[derive(Debug, Clone, Copy)]
pub struct MixerStrip<'a> {
    /// One strip each, left to right.
    pub tracks: &'a [Arc<AudioTrack>],
    /// `vm.mixer.fxBuses`.
    pub buses: &'a [Arc<FxBus>],
    /// `vm.mixer.supportsEffects`: the mic-input strip, the bus section and
    /// "+ Bus" are only meaningful on the native engine (device I/O,
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
    /// `settings.suggestHeadphonesMark`: a locked MON can be unlocked by
    /// marking the output as headphones in Settings.
    pub suggest_headphones_mark: bool,
    /// `vm.recording.inputLevel` (0..1).
    pub input_level: f64,
}

impl Default for MicInput {
    fn default() -> Self {
        Self {
            record_gain_db: AppSettingsService::RECORD_GAIN_MIN_DB,
            monitor_gain: 0.0,
            monitor_allowed: true,
            suggest_headphones_mark: false,
            input_level: 0.0,
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
    /// `onDeleteTrack(track)`: the take strip's trash icon.
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
    /// The bus strip's trash icon or its menu's "Delete bus": the host asks
    /// first (`confirmDeleteBus(name)`, as the timeline lane's Delete does:
    /// the bus's FX chain and every send into it go with it, with no undo)
    /// and, confirmed, calls `vm.mixer.deleteBus(id)`.
    DeleteBus { id: String, name: String },
    /// `vm.mixer.createBus()`: "+ Bus".
    CreateBus,
    /// `vm.mixer.setMasterVolumeLive(gain)`.
    SetMasterVolumeLive { gain: f64 },
    /// `vm.mixer.commitMasterVolume()`.
    CommitMasterVolume,
    /// `vm.appSettings.setRecordGainDbLive(db)`.
    SetRecordGainDbLive { db: f64 },
    /// `vm.appSettings.setMonitorGainLive(gain)`.
    SetMonitorGainLive { gain: f64 },
    /// `vm.appSettings.commitAudioGains()`: REC or MON released.
    CommitAudioGains,
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
/// fallback. Derived from the tallest strip's fixed rows: title ≈14, M/S row
/// 20, pan knob 26 + its label ≈14, dB readout ≈13, routing chip ≈19, delete
/// icon 14 and the gaps between them (10+10+10+8+8+10 = 56) ≈ 176 in total,
/// plus ~56px of minimum usable fader travel.
pub const STRIP_MIN_CONTENT_HEIGHT: f32 = 232.0;

/// Fixed fader height used in the squeezed scrolling fallback, where the
/// column's height is unbounded so the fader cannot fill it.
pub const SQUEEZED_FADER_HEIGHT: f32 = 120.0;

/// A channel strip's width, and the gap after it.
const STRIP_WIDTH: f32 = 92.0;
const STRIP_GAP: f32 = 10.0;

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

    /// The desk: a horizontally scrolling row of strips.
    pub fn view(self) -> Element<'a, Message> {
        let buses = self.buses;
        if self.tracks.is_empty() && buses.is_empty() {
            return center(text("No tracks yet").size(12).color(Studio::TEXT_FAINT)).into();
        }
        let mut desk = keyed_row().height(Length::Fill).padding(12);
        // Mic input strip: the live input's meter + record gain + monitor
        // (hear-yourself) gain. Only meaningful on the native engine (device
        // I/O + monitoring).
        if self.supports_effects {
            desk = desk
                .push(StripKey::Mic, gap_after(mic_input_channel(self.mic)))
                .push(StripKey::MicRule, separator(12.0));
        }
        for t in self.tracks {
            let selected =
                self.selected_track_id == Some(t.id.as_str()) && self.selected_bus_id.is_none();
            // Backing tracks stay dry → no output routing chip.
            let routes: &[Arc<FxBus>] = if t.is_backing() { &[] } else { buses };
            desk = desk.push(
                StripKey::Track(id_key(&t.id)),
                gap_after(track_channel(t, selected, routes)),
            );
        }
        // FX buses (DAW groups). Only meaningful on the native effects
        // engine.
        if self.supports_effects {
            // Separator between the vocal takes and the bus section (incl.
            // + Bus).
            desk = desk.push(StripKey::BusRule, separator(12.0));
            for b in buses {
                let selected = self.selected_bus_id == Some(b.id.as_str());
                desk = desk.push(
                    StripKey::Bus(id_key(&b.id)),
                    gap_after(bus_channel(b, selected, buses)),
                );
            }
            desk = desk.push(StripKey::AddBus, gap_after(add_bus_button()));
        }
        // The master bus strip — the whole mix sums here. Set apart on the
        // right.
        desk = desk.push(StripKey::MasterRule, separator(14.0)).push(
            StripKey::Master,
            gap_after(master_channel(self.master_volume)),
        );
        scrollable(desk)
            .direction(Direction::Horizontal(Scrollbar::default()))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::scrollbar)
            .into()
    }
}

/// `margin: EdgeInsets.only(right: 10)`.
fn gap_after<'a>(strip: Element<'a, Message>) -> Element<'a, Message> {
    container(strip)
        .height(Length::Fill)
        .padding(Padding {
            right: STRIP_GAP,
            ..Padding::ZERO
        })
        .into()
}

/// The 1 px rule between sections (margin 2 / 14 / `right` / 14).
fn separator<'a>(right: f32) -> Element<'a, Message> {
    container(
        container(Space::new())
            .width(1)
            .height(Length::Fill)
            .style(theme::fill(Studio::BORDER)),
    )
    .height(Length::Fill)
    .padding(Padding {
        top: 14.0,
        right,
        bottom: 14.0,
        left: 2.0,
    })
    .into()
}

// The preset lane tints and the default take tint are the context menu's
// (`kTrackColorPalette` and `defaultTakeColor` lived in
// `track_context_menu.dart`).
pub use super::track_context_menu::{default_take_color, TRACK_COLOR_PALETTE};

/// A take strip's accent: its tint, the coral for the backing, else its
/// default take color.
pub fn track_accent(track: &AudioTrack) -> Color {
    match track.color_value {
        Some(v) => argb(v),
        None if track.is_backing() => Studio::ACCENT,
        None => default_take_color(&track.id),
    }
}

/// A bus strip's accent: its tint, else the coral.
pub fn bus_accent(bus: &FxBus) -> Color {
    bus.color_value.map_or(Studio::ACCENT, argb)
}

/// Display name of the bus `id` routes to, or "Master" when it's
/// none/missing.
pub fn bus_name<'b>(buses: &'b [Arc<FxBus>], id: Option<&str>) -> &'b str {
    id.and_then(|id| buses.iter().find(|b| b.id == id))
        .map_or("Master", |b| b.name.as_str())
}

/// A strip's frame: 92 wide, padded, tinted `Studio.hover` with an accent
/// outline when selected.
fn strip_frame<'a>(
    content: Element<'a, Message>,
    width: f32,
    background: Color,
    border: Color,
) -> Element<'a, Message> {
    container(content)
        .width(width)
        .height(Length::Fill)
        // 12 / 10 inside the 1 px border (the decoration's padding).
        .padding([13, 11])
        .style(move |_| boxed(Some(background), 8.0, Some(border)))
        .into()
}

/// `_StripBody`: at normal dock heights, the plain column — the fader fills
/// the leftover height, console style. Squeezed below
/// [`STRIP_MIN_CONTENT_HEIGHT`], a vertically scrollable column with the
/// fader at a fixed height instead, rather than collapsing the fader to
/// nothing and clipping the dB readout / routing chip / delete control.
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
            .style(theme::scrollbar)
            .into()
        }
    })
    .into()
}

/// The fader slot inside a strip column: fills the leftover height at
/// normal dock heights, a fixed (still usable) height in the squeezed
/// fallback.
fn fader_slot<'a>(squeezed: bool, child: Element<'a, Message>) -> Element<'a, Message> {
    container(child)
        .width(Length::Fill)
        .height(if squeezed {
            Length::Fixed(SQUEEZED_FADER_HEIGHT)
        } else {
            Length::Fill
        })
        .align_x(Horizontal::Center)
        .into()
}

fn gap<'a>(height: f32) -> Element<'a, Message> {
    Space::new().height(height).into()
}

/// A strip's one-line title.
fn title<'a>(label: &str, color: Color) -> Element<'a, Message> {
    container(
        text(label.to_string())
            .size(11)
            .font(font::BOLD)
            .color(color)
            .wrapping(Wrapping::None)
            .align_x(Horizontal::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .clip(true)
    .into()
}

/// A strip's "-6.0 dB" readout.
fn db_readout<'a>(volume: f64) -> Element<'a, Message> {
    text(format!("{} dB", fader_db(volume)))
        .size(10)
        .font(font::SEMIBOLD)
        .color(Studio::TEXT_SECONDARY)
        .into()
}

/// The strip's trash icon.
fn trash<'a>(on_tap: Message) -> Element<'a, Message> {
    button(icons::icon(Icon::Trash2, 14.0, Studio::TEXT_FAINT))
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

/// The M / S pair.
fn mute_solo<'a>(muted: bool, soloed: bool, mute: Message, solo: Message) -> Element<'a, Message> {
    row![
        ms_toggle("M", muted, Studio::RECORD, mute),
        Space::new().width(6),
        ms_toggle("S", soloed, Studio::ACCENT, solo),
    ]
    .into()
}

/// `_Toggle`: a 24 × 20 M or S button, tinted `active_color` when on.
fn ms_toggle<'a>(
    label: &'static str,
    active: bool,
    active_color: Color,
    on_tap: Message,
) -> Element<'a, Message> {
    let face = container(text(label).size(11).font(font::BOLD).color(if active {
        active_color
    } else {
        Studio::TEXT_SECONDARY
    }))
    .center_x(24)
    .center_y(20)
    .style(move |_| {
        boxed(
            Some(if active {
                alpha(active_color, 0.2)
            } else {
                Studio::SURFACE_RAISED
            }),
            4.0,
            Some(if active {
                alpha(active_color, 0.6)
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

/// `_RouteChip`: "→ Master" / "→ Bus name", opening its destinations
/// (Master first) in a menu just below it.
fn route_chip<'a>(
    label: &str,
    accent: Color,
    options: Vec<MenuItem<Message>>,
) -> Element<'a, Message> {
    let face = container(
        text(format!("→ {label}"))
            .size(9)
            .font(font::SEMIBOLD)
            .color(alpha(accent, 0.9))
            .wrapping(Wrapping::None),
    )
    .width(Length::Fill)
    .padding([3, 6])
    .align_x(Horizontal::Center)
    .clip(true)
    .style(|_| boxed(Some(Studio::SURFACE_RAISED), 4.0, Some(Studio::BORDER)));
    PopupMenu::new(face, options, ROUTE_MENU_WIDTH, Opens::BelowOnTap).into()
}

/// The route and bus menus' width.
const ROUTE_MENU_WIDTH: f32 = 160.0;

/// `_Channel`: a take's strip.
fn track_channel<'a>(
    t: &'a AudioTrack,
    selected: bool,
    buses: &'a [Arc<FxBus>],
) -> Element<'a, Message> {
    let accent = track_accent(t);
    let id = t.id.as_str();
    let body = strip_body(move |squeezed| {
        let mut children = vec![
            title(&t.display_name, Studio::TEXT_PRIMARY),
            gap(10.0),
            mute_solo(
                t.muted,
                t.soloed,
                Message::ToggleMute { id: id.to_string() },
                Message::ToggleSolo { id: id.to_string() },
            ),
            gap(10.0),
            studio_pan_knob(
                t.pan,
                move |pan| Message::SetTrackPanLive {
                    id: id.to_string(),
                    pan,
                },
                Message::CommitTrackVolume,
            )
            .size(26.0)
            .accent(accent)
            .into(),
            gap(10.0),
            fader_slot(
                squeezed,
                vertical_fader(
                    t.volume,
                    accent,
                    46.0,
                    move |volume| Message::SetTrackVolumeLive {
                        id: id.to_string(),
                        volume,
                    },
                    Message::CommitTrackVolume,
                ),
            ),
            gap(8.0),
            db_readout(t.volume),
        ];
        // Output routing chip — vocal takes only, when buses exist.
        if !t.is_backing() && !buses.is_empty() {
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
            children.push(gap(8.0));
            children.push(route_chip(bus_name(buses, routed), accent, options));
        }
        children.push(gap(10.0));
        children.push(trash(Message::DeleteTrack { id: id.to_string() }));
        children
    });
    let frame = strip_frame(
        body,
        STRIP_WIDTH,
        if selected {
            Studio::HOVER
        } else {
            Studio::PANEL
        },
        if selected {
            alpha(accent, 0.6)
        } else {
            Studio::BORDER
        },
    );
    tap(
        frame,
        vec![Message::SelectTrack { id: id.to_string() }],
        None,
    )
}

/// `_BusChannel`: an FX bus's strip — select (opens its FX in the
/// inspector), mute/solo, pan, fader, output routing, delete; rename and
/// delete on a right click, at the click (anchored below this full-height
/// strip, the menu opened at the bottom of the mixer).
fn bus_channel<'a>(b: &'a FxBus, selected: bool, buses: &'a [Arc<FxBus>]) -> Element<'a, Message> {
    let accent = bus_accent(b);
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
            title(&b.name, accent),
            gap(10.0),
            mute_solo(
                b.muted,
                b.soloed,
                Message::ToggleBusMute { id: id.to_string() },
                Message::ToggleBusSolo { id: id.to_string() },
            ),
            gap(10.0),
            studio_pan_knob(
                b.pan,
                move |pan| Message::SetBusPanLive {
                    id: id.to_string(),
                    pan,
                },
                Message::CommitFx,
            )
            .size(26.0)
            .accent(accent)
            .into(),
            gap(10.0),
            fader_slot(
                squeezed,
                vertical_fader(
                    b.volume,
                    accent,
                    46.0,
                    move |volume| Message::SetBusVolumeLive {
                        id: id.to_string(),
                        volume,
                    },
                    Message::CommitFx,
                ),
            ),
            gap(8.0),
            db_readout(b.volume),
            gap(8.0),
            route_chip(bus_name(buses, b.output_bus_id.as_deref()), accent, options),
            gap(10.0),
            trash(delete_bus(b)),
        ]
    });
    let frame = strip_frame(
        body,
        STRIP_WIDTH,
        if selected {
            Studio::HOVER
        } else {
            Studio::PANEL
        },
        if selected {
            alpha(accent, 0.6)
        } else {
            Studio::BORDER
        },
    );
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

/// The "+ Bus" button that adds a new FX bus to the mixer.
fn add_bus_button<'a>() -> Element<'a, Message> {
    let face = container(
        column![
            icons::icon(Icon::Plus, 16.0, Studio::TEXT_SECONDARY),
            Space::new().height(6),
            text("BUS")
                .size(9)
                .font(font::BOLD)
                .color(Studio::TEXT_FAINT),
        ]
        .align_x(Horizontal::Center),
    )
    .width(56)
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .align_x(Horizontal::Center)
    .style(|_| boxed(Some(alpha(Studio::PANEL, 0.4)), 8.0, Some(Studio::BORDER)));
    button(face)
        .padding(0)
        .height(Length::Fill)
        .style(bare_button)
        .on_press(Message::CreateBus)
        .into()
}

/// `_MasterChannel`: like a track strip but it controls the overall output
/// gain (no mute/solo, no delete). Accent-tinted.
fn master_channel<'a>(value: f64) -> Element<'a, Message> {
    let body = strip_body(move |squeezed| {
        vec![
            title("MASTER", Studio::ACCENT),
            // Pad where the M/S row + pan knob sit on a track strip, so the
            // fader lines up with the channel faders.
            gap(92.0),
            fader_slot(
                squeezed,
                vertical_fader(
                    value,
                    Studio::ACCENT,
                    46.0,
                    |gain| Message::SetMasterVolumeLive { gain },
                    Message::CommitMasterVolume,
                ),
            ),
            gap(8.0),
            db_readout(value),
            // Pad where the delete button sits on a track strip.
            gap(24.0),
        ]
    });
    strip_frame(
        body,
        STRIP_WIDTH,
        alpha(Studio::ACCENT, 0.06),
        alpha(Studio::ACCENT, 0.4),
    )
}

/// The REC fader's position for a record gain (dB).
pub fn record_gain_position(db: f64) -> f64 {
    let (min, max) = (
        AppSettingsService::RECORD_GAIN_MIN_DB,
        AppSettingsService::RECORD_GAIN_MAX_DB,
    );
    ((db - min) / (max - min)).clamp(0.0, 1.0)
}

/// The record gain (dB) at a REC fader position.
pub fn record_gain_at(position: f64) -> f64 {
    let (min, max) = (
        AppSettingsService::RECORD_GAIN_MIN_DB,
        AppSettingsService::RECORD_GAIN_MAX_DB,
    );
    min + position * (max - min)
}

/// The MON fader's readout: "spkr" while locked, "off" at nothing, else the
/// percentage.
pub fn monitor_readout(mic: &MicInput) -> String {
    if !mic.monitor_allowed {
        "spkr".to_string()
    } else if mic.monitor_gain <= 0.001 {
        "off".to_string()
    } else {
        format!("{}%", (mic.monitor_gain * 100.0).round() as i64)
    }
}

/// Why MON is locked, and where to unlock it when it can be: none while it
/// is allowed.
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

/// `_MicInputChannel`: the shared input level meter + a record-gain fader
/// (how loud the captured voice records) + a monitor fader (how loud you
/// hear yourself; 0 = off, e.g. on a speaker where it would feed back).
fn mic_input_channel<'a>(mic: MicInput) -> Element<'a, Message> {
    let body = strip_body(move |squeezed| {
        let rec = fader_column(
            "REC",
            record_gain_position(mic.record_gain_db),
            Studio::RECORD,
            format!("{} dB", mic.record_gain_db.round() as i64),
            true,
            |p| Message::SetRecordGainDbLive {
                db: record_gain_at(p),
            },
        );
        let mon = fader_column(
            "MON",
            if mic.monitor_allowed {
                mic.monitor_gain
            } else {
                0.0
            },
            Studio::ACCENT,
            monitor_readout(&mic),
            mic.monitor_allowed,
            |gain| Message::SetMonitorGainLive { gain },
        );
        // A locked MON says why, and where to unlock it when it can be.
        let mon = match monitor_lock_message(&mic) {
            Some(why) => tooltip(
                mon,
                container(text(why).size(theme::TOOLTIP_TEXT_SIZE))
                    .max_width(280)
                    .padding(theme::TOOLTIP_PADDING)
                    .style(theme::tooltip),
                // Material's tooltip prefers below.
                tooltip::Position::Bottom,
            )
            .into(),
            None => mon,
        };
        vec![
            text("MIC")
                .size(11)
                .font(font::BOLD)
                .color(Studio::RECORD)
                .into(),
            gap(8.0),
            // Shared input level meter (monitor + record are the same mic).
            canvas_widget(InputMeter {
                level: mic.input_level,
            })
            .width(Length::Fill)
            .height(6)
            .into(),
            gap(12.0),
            fader_slot(
                squeezed,
                row![
                    container(rec).width(Length::Fill).height(Length::Fill),
                    Space::new().width(8),
                    container(mon).width(Length::Fill).height(Length::Fill),
                ]
                .height(Length::Fill)
                .into(),
            ),
        ]
    });
    strip_frame(
        body,
        138.0,
        alpha(Studio::RECORD, 0.06),
        alpha(Studio::RECORD, 0.4),
    )
}

/// `_FaderColumn`: a labelled vertical fader (label on top, fader, value
/// readout below). Disabled (e.g. monitor on a speaker → feedback), it is
/// greyed out and ignores input.
fn fader_column<'a>(
    label: &'static str,
    value: f64,
    accent: Color,
    readout: String,
    enabled: bool,
    on_changed: impl Fn(f64) -> Message + 'a,
) -> Element<'a, Message> {
    let fader: Element<'a, Message> = if enabled {
        vertical_fader(value, accent, 40.0, on_changed, Message::CommitAudioGains)
    } else {
        canvas_widget(VerticalFader {
            value: value.clamp(0.0, 1.0),
            accent,
            opacity: 0.35,
            input: None,
        })
        .width(40)
        .height(Length::Fill)
        .into()
    };
    column![
        text(label)
            .size(9)
            .font(font::BOLD)
            .color(alpha(accent, if enabled { 0.9 } else { 0.35 })),
        gap(6.0),
        container(fader)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center),
        gap(6.0),
        text(readout)
            .size(9)
            .font(font::SEMIBOLD)
            .color(if enabled {
                Studio::TEXT_SECONDARY
            } else {
                Studio::TEXT_FAINT
            }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Horizontal::Center)
    .into()
}

/// The shared input meter: `Studio.record` filling an inset well to the
/// level, its corners rounded 3.
struct InputMeter {
    level: f64,
}

impl canvas::Program<Message> for InputMeter {
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
        let (w, h) = (bounds.width, bounds.height);
        frame.fill(
            &Path::rounded_rectangle(Point::ORIGIN, bounds.size(), Radius::from(3.0)),
            Studio::INSET,
        );
        let fill = w * self.level.clamp(0.0, 1.0) as f32;
        if fill > 0.0 {
            // The well's rounded clip: the fill's left corners, and its right
            // ones once it reaches the well's own.
            let left = 3.0f32.min(fill / 2.0).min(h / 2.0);
            let right = (3.0 - (w - fill)).clamp(0.0, left);
            frame.fill(
                &Path::rounded_rectangle(
                    Point::ORIGIN,
                    Size::new(fill, h),
                    Radius {
                        top_left: left,
                        top_right: right,
                        bottom_right: right,
                        bottom_left: left,
                    },
                ),
                Studio::RECORD,
            );
        }
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------------------ fader

/// The fader's track inset at each end.
pub const FADER_PAD: f32 = 8.0;

/// The level a vertical fader `height` tall reads at `dy` from its top.
/// The span is at least 1 px: laid out below 2 × the pad (the master strip
/// squeezed to the minimum dock height), a span under 1 would have flipped
/// the clamp's bounds (Dart's clamp threw there).
pub fn vertical_fader_value(dy: f32, height: f32) -> f64 {
    let pad = f64::from(FADER_PAD);
    let span = (f64::from(height) - 2.0 * pad).max(1.0);
    (1.0 - (f64::from(dy) - pad) / span).clamp(0.0, 1.0)
}

/// `_VerticalFader` `width` wide and as tall as its slot.
fn vertical_fader<'a>(
    value: f64,
    accent: Color,
    width: f32,
    on_changed: impl Fn(f64) -> Message + 'a,
    on_change_end: Message,
) -> Element<'a, Message> {
    canvas_widget(VerticalFader {
        value: value.clamp(0.0, 1.0),
        accent,
        opacity: 1.0,
        input: Some(FaderInput {
            on_changed: Box::new(on_changed),
            on_change_end,
        }),
    })
    .width(width)
    .height(Length::Fill)
    .into()
}

/// A console-style vertical volume fader: a grooved track with tick marks
/// and a draggable cap. Drag up for louder, down for quieter.
///
/// It takes its pointer on contact: a press sets the level at once; every
/// move while held sets it from the pointer's y; the release saves it, once,
/// whether the press was a click or turned into a drag.
struct VerticalFader<'a> {
    value: f64,
    accent: Color,
    /// 1, or 0.35 when disabled.
    opacity: f32,
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
    cache: KeyedCache<(u64, [u32; 4], u32, u32, u32)>,
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
            color_key(self.accent),
            self.opacity.to_bits(),
            size.width.to_bits(),
            size.height.to_bits(),
        );
        vec![state.cache.draw(renderer, size, key, |frame| {
            #[cfg(test)]
            FADER_DRAWS.with(|n| n.set(n.get() + 1));
            draw_vertical_fader(frame, size, self.value as f32, self.accent, self.opacity);
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

/// `_VFaderPainter`, its colors faded to `opacity`.
fn draw_vertical_fader(frame: &mut Frame, size: Size, value: f32, accent: Color, opacity: f32) {
    let fade = |c: Color| Color {
        a: c.a * opacity,
        ..c
    };
    let cx = size.width / 2.0;
    let top = FADER_PAD;
    let bottom = size.height - FADER_PAD;
    let span = bottom - top;
    let cap_y = bottom - value * span;
    let line = |color: Color, width: f32| {
        Stroke::default()
            .with_color(fade(color))
            .with_width(width)
            .with_line_cap(LineCap::Round)
    };
    // Groove.
    frame.stroke(
        &Path::line(Point::new(cx, top), Point::new(cx, bottom)),
        line(Studio::GRID_STRONG, 4.0),
    );
    // Filled portion below the cap.
    frame.stroke(
        &Path::line(Point::new(cx, bottom), Point::new(cx, cap_y)),
        line(alpha(accent, 0.55), 4.0),
    );
    // Scale ticks down the right side (incl. a 0 dB mark at the top).
    let tick = Stroke::default()
        .with_color(fade(Studio::BORDER))
        .with_width(1.0);
    for i in 0..=4 {
        let ty = top + i as f32 / 4.0 * span;
        frame.stroke(
            &Path::line(Point::new(cx + 9.0, ty), Point::new(cx + 14.0, ty)),
            tick,
        );
    }
    // Fader cap (console handle).
    let cap = Path::rounded_rectangle(
        Point::new(cx - 14.0, cap_y - 6.5),
        Size::new(28.0, 13.0),
        Radius::from(3.0),
    );
    frame.fill(&cap, fade(Studio::SURFACE_RAISED));
    frame.stroke(
        &cap,
        Stroke::default().with_color(fade(accent)).with_width(1.5),
    );
    frame.stroke(
        &Path::line(Point::new(cx - 10.0, cap_y), Point::new(cx + 10.0, cap_y)),
        line(accent, 2.0),
    );
}
