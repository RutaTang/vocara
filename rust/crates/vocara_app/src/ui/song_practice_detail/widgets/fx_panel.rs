//! Port of `ui/song_practice_detail/widgets/fx_panel.dart`: the FX detail
//! for the selected vocal take or FX bus — the preset pop-up, then the
//! insert chain (equalizer with its analyzer, compressor with its
//! gain-reduction reading, de-esser, gate, delay, saturation, doubler,
//! limiter, reverb), each insert an effect module with a drag handle and a
//! power button that collapses it to its header when off, reordered by
//! dragging the handles.
//!
//! Drawn as `DESIGN.md`'s effect module and the approved workspace mockup
//! draw it: a 24 px header (the handle's dots, the power button, the name
//! in 12 px medium, a right-aligned 11 px `TEXT_FAINT` badge: "4 bands",
//! "GR −2.1 dB", "Bypassed"), then its parameter rows (`controls::
//! parameter_row`: a 62 px label, a thin slider, a 54 px tabular value);
//! 8 / 14 / 12 px of padding and a hairline under each module. A bypassed
//! module is its header alone, at 55 %. The EQ graph is a 92 px well with
//! its decade lines, the 0 dB line, the curve over a soft fill, its band
//! nodes and its axis labels, the live analyzer drawn faintly behind.
//!
//! # Plain data in, messages out
//!
//! The Dart panel read the view model. Here it takes what it shows
//! ([`FxPanel`]: the target's id and chain, the EQ's input column under the
//! playhead, the compressor's gain reduction) and reports a [`Message`] per
//! view-model call the Dart panel made, which the screen routes to the same
//! method:
//!
//! | Message | Dart call |
//! |---|---|
//! | [`Message::SetTrackFxLive`] | `vm.mixer.setTrackFxLive(track.id, fx)` |
//! | [`Message::SetBusFxLive`] | `vm.mixer.setBusFxLive(bus.id, fx)` |
//! | [`Message::CommitFx`] | `vm.commitFx()` |
//! | [`Message::ReorderTrackInserts`] | `vm.mixer.reorderTrackInserts(track.id, order)` |
//! | [`Message::ReorderBusInserts`] | `vm.mixer.reorderBusInserts(bus.id, order)` |
//!
//! A committed edit (a power button, a preset) reports its live edit and
//! then `CommitFx`, as `_setFx(fx, commit: true)` called both; a slider or
//! EQ drag reports live edits on every move and `CommitFx` once on release;
//! a wheel Q edit commits 400 ms after its last tick.
//!
//! The panel keys itself by its target, so another take or bus gets fresh
//! state (the analyzer's smoothed spectrum, a held slider), as Dart's
//! `FxPanel(key: ValueKey(id))` did.
//!
//! A trackpad's two-finger swipe reached Flutter as a pan-zoom gesture,
//! which the sliders' and the EQ's drags (`kPointerDragDevices`) left to
//! the panel's scroll; iced hands it over as wheel events with its phases
//! (`widgets::pan_zoom`): the sliders take no wheel event, and the EQ sets
//! a band's Q on a scroll signal only (a mouse wheel's, or a precise
//! device's without phases), never on a swipe or the momentum after it.
//!
//! # Differences from Flutter
//!
//! - Unmounting mid-gesture: Dart committed a slider or EQ drag in flight,
//!   and flushed a wheel Q edit still inside its debounce, when the panel
//!   was disposed (the selection changed, the inspector collapsed). iced has
//!   no disposal callback; the release that ends a drag still reaches a
//!   panel that is shown, and a panel the screen stops showing mid-drag or
//!   inside the debounce owes `CommitFx` to the screen (which can always
//!   commit when it changes the selection: the live edit is already in the
//!   model).
//! - The pointer: no pointer cancel or second finger reaches a desktop
//!   mouse control, so those Dart cases have no counterpart.
//! - The gain-reduction meter's reading is the one the host passed at its
//!   last view (Dart polled the engine itself); the meter keeps Dart's 50 ms
//!   poll cadence, peak falloff and idle stop over it. It reads in the
//!   compressor's badge, a short bar before it.
//! - A reorder reaches the model (and so the engine's chain) at the drop,
//!   250 ms before Flutter's did: `SliverReorderableList` called
//!   `onReorder` only once the dropped card had glided in (`_dropCompleted`,
//!   when its proxy's 250 ms animation ran back), and anything in between
//!   dropped it unreported. Waiting as it did would port those lost edits
//!   (see `chain`); the cards still slide and land as Flutter's did.

mod chain;
pub mod eq;
#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::time::{Duration, Instant};

use iced::alignment::Vertical;
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{canvas as canvas_widget, column, container, row, Column, Space};
use iced::{
    mouse, window, Color, Element, Length, Padding, Point, Rectangle, Renderer, Size, Theme,
};
use vocara_model::models::audio_fx::{EqBand, FxKind, TrackFx};
use vocara_model::models::fx_presets::FxPresets;

pub use eq::EqInput;

use self::chain::InsertChain;
use self::eq::{EqGraph, EQ_HEIGHT};
use super::mixer_shared::{keyed_row, popup_face, tap, MenuItem, Opens, PopupMenu};
use crate::ui::core::theme::{self, font, type_scale, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};
use crate::ui::core::widgets::controls::{
    draw_thin_slider, parameter_row, power_colors, KNOB, POWER_SIZE, SLIDER_FILL,
};
use crate::ui::core::widgets::fading_scrollbar::{no_scrollbar, scrollable};
use crate::ui::core::widgets::line_icon::{glyphs, line_icon};
use crate::ui::core::widgets::opacity_layer::opacity_layer;
use crate::ui::core::widgets::painter::KeyedCache;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::util::to_string_as_fixed;

/// The hairline under the preset row (`#222228`, the inspector's).
const BLOCK_LINE: Color = Color::from_rgb8(0x22, 0x22, 0x28);
/// The hairline under each module (`#1F1F25`).
pub const MODULE_LINE: Color = Color::from_rgb8(0x1F, 0x1F, 0x25);
/// The handle's dots (`#4A4A54`).
const HANDLE_DOT: Color = Color::from_rgb8(0x4A, 0x4A, 0x54);
/// A bypassed module's opacity.
pub const BYPASSED_OPACITY: f32 = 0.55;

/// A module's padding: 8 above, 14 at the sides, 12 below.
pub const MODULE_PADDING: Padding = Padding {
    top: 8.0,
    right: 14.0,
    bottom: 12.0,
    left: 14.0,
};
/// A module's header row.
pub const MODULE_HEADER: f32 = 24.0;
/// The gap above the EQ's well, and above the parameter rows.
pub const EQ_GAP: f32 = 6.0;
pub const PARAMS_GAP: f32 = 4.0;

/// What the panel edits: exactly one of a vocal take's insert chain or an FX
/// bus's chain.
#[derive(Debug, Clone, Copy)]
pub enum FxTarget<'a> {
    /// A take: its id and `track.fx` (`None`: no FX yet — the flat chain).
    Track {
        id: &'a str,
        fx: Option<&'a TrackFx>,
    },
    /// A bus: its id and `bus.fx`.
    Bus { id: &'a str, fx: &'a TrackFx },
}

impl<'a> FxTarget<'a> {
    pub fn id(&self) -> &'a str {
        match *self {
            FxTarget::Track { id, .. } | FxTarget::Bus { id, .. } => id,
        }
    }

    pub fn is_bus(&self) -> bool {
        matches!(self, FxTarget::Bus { .. })
    }

    /// `_fx`: the chain shown (a take with none shows the flat chain).
    pub fn fx(&self) -> TrackFx {
        match *self {
            FxTarget::Track { fx, .. } => fx.cloned().unwrap_or_else(TrackFx::flat),
            FxTarget::Bus { fx, .. } => fx.clone(),
        }
    }
}

/// The panel's data (the Dart widget's `track` / `bus`, and what it read
/// from the view model).
#[derive(Debug, Clone, Copy)]
pub struct FxPanel<'a> {
    pub target: FxTarget<'a>,
    /// The pre-EQ input spectrum column under the playhead, for the EQ's
    /// analyzer: `vm.analysis.eqInputColumn(track.id)`. Only a take has
    /// one — a bus is a sum of routed tracks, so it shows the response curve
    /// alone, and this is ignored for a bus.
    pub eq_input: Option<EqInput<'a>>,
    /// The compressor's current gain reduction (dB, >= 0), for its meter:
    /// `vm.mixer.gainReductionDb(track.id)` for a take,
    /// `vm.mixer.busGainReductionDb(bus.id)` for a bus.
    pub gain_reduction_db: f64,
}

/// What the panel reports: the view-model calls the Dart panel made (see
/// the module docs).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `vm.mixer.setTrackFxLive(id, fx)`.
    SetTrackFxLive { id: String, fx: TrackFx },
    /// `vm.mixer.setBusFxLive(id, fx)`.
    SetBusFxLive { id: String, fx: TrackFx },
    /// `vm.commitFx()`.
    CommitFx,
    /// `vm.mixer.reorderTrackInserts(id, order)`.
    ReorderTrackInserts { id: String, order: Vec<FxKind> },
    /// `vm.mixer.reorderBusInserts(id, order)`.
    ReorderBusInserts { id: String, order: Vec<FxKind> },
}

/// `_setFx`'s two routes: the live edit for this target.
#[derive(Debug, Clone)]
struct Setter {
    id: String,
    bus: bool,
}

impl Setter {
    fn live(&self, fx: TrackFx) -> Message {
        if self.bus {
            Message::SetBusFxLive {
                id: self.id.clone(),
                fx,
            }
        } else {
            Message::SetTrackFxLive {
                id: self.id.clone(),
                fx,
            }
        }
    }

    /// `_setFx(fx, commit: true)`: the live edit, then the commit.
    fn committed(&self, fx: TrackFx) -> Vec<Message> {
        vec![self.live(fx), Message::CommitFx]
    }

    fn reorder(&self, order: Vec<FxKind>) -> Message {
        if self.bus {
            Message::ReorderBusInserts {
                id: self.id.clone(),
                order,
            }
        } else {
            Message::ReorderTrackInserts {
                id: self.id.clone(),
                order,
            }
        }
    }
}

/// The target's identity, for keying the panel's state to it.
fn target_key(target: &FxTarget<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    target.is_bus().hash(&mut h);
    target.id().hash(&mut h);
    h.finish()
}

/// The preset the chain is (its tone; the plugin order aside), if any.
pub fn current_preset(fx: &TrackFx) -> Option<&'static str> {
    FxPresets::vocal()
        .into_iter()
        .find(|p| {
            TrackFx {
                order: fx.order.clone(),
                ..p.fx.clone()
            } == *fx
        })
        .map(|p| p.name)
}

/// The equalizer's badge: its enabled bands ("4 bands", "1 band").
pub fn bands_badge(bands: &[EqBand]) -> String {
    match bands.iter().filter(|b| b.enabled).count() {
        1 => "1 band".to_string(),
        n => format!("{n} bands"),
    }
}

/// A bypassed module's badge.
pub const BYPASSED: &str = "Bypassed";

/// The compressor's badge: "GR −2.1 dB", "GR 0.0 dB".
pub fn gr_badge(gr: f64) -> String {
    format!("GR {} dB", gr_readout(gr)).replace('-', "\u{2212}")
}

impl<'a> FxPanel<'a> {
    /// The panel for `target`, with no input spectrum and no gain reduction.
    pub fn new(target: FxTarget<'a>) -> Self {
        Self {
            target,
            eq_input: None,
            gain_reduction_db: 0.0,
        }
    }

    /// The input spectrum the EQ's analyzer draws: a take's column, never
    /// a bus's (`_eqInput`).
    pub fn analyzer_input(&self) -> Option<EqInput<'a>> {
        if self.target.is_bus() {
            None
        } else {
            self.eq_input
        }
    }

    /// The panel: the preset row (under the inspector's identity block),
    /// then the insert chain in a scroll view with no scrollbar.
    pub fn view(self) -> Element<'a, Message> {
        let fx = Rc::new(self.target.fx());
        let set = Rc::new(Setter {
            id: self.target.id().to_string(),
            bus: self.target.is_bus(),
        });
        let cards: Vec<(FxKind, Element<'a, Message>)> = fx
            .order
            .iter()
            .map(|&kind| (kind, self.insert_card(kind, &fx, &set)))
            .collect();
        let reorder = Rc::clone(&set);
        // Insert chain — drag a handle to reorder the signal flow; each
        // insert collapses to its header when off.
        let chain = column![
            InsertChain::new(cards, move |order| reorder.reorder(order)),
            Space::new().height(12),
        ]
        .width(Length::Fill);
        let panel = column![
            preset_row(&fx, &set),
            no_scrollbar(scrollable(chain))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .width(Length::Fill)
        .height(Length::Fill);
        keyed_row()
            .push(target_key(&self.target), panel)
            .height(Length::Fill)
            .into()
    }

    /// One insert's module in the chain: a header (drag handle, power,
    /// name, badge) plus, when enabled, the plugin's editor. Opaque in the
    /// panel colour, so a dragged module fully occludes the others.
    fn insert_card(
        &self,
        kind: FxKind,
        fx: &Rc<TrackFx>,
        set: &Rc<Setter>,
    ) -> Element<'a, Message> {
        let f = fx.as_ref();
        // A param slider over `fx`, its change built by `with`.
        let slider = |label: &'static str,
                      value: f64,
                      min: f64,
                      max: f64,
                      unit: &'static str,
                      with: fn(&TrackFx, f64) -> TrackFx|
         -> Element<'a, Message> {
            let fx = Rc::clone(fx);
            let set = Rc::clone(set);
            fx_slider(label, value, min, max, unit, move |v| {
                set.live(with(&fx, v))
            })
        };
        // The power button's committed edit.
        let toggle = |on: bool, with: fn(&TrackFx, bool) -> TrackFx| set.committed(with(f, !on));
        match kind {
            FxKind::Eq => {
                let badge = Badge::Text(bands_badge(&f.eq));
                let mut body = Vec::new();
                if f.eq_on {
                    let fx_c = Rc::clone(fx);
                    let set_c = Rc::clone(set);
                    let input = self.analyzer_input();
                    body.push(
                        container(eq_graph(Rc::clone(fx), input, move |i, band| {
                            set_c.live(TrackFx {
                                eq_on: true,
                                ..fx_c.with_band(i, band)
                            })
                        }))
                        .padding(Padding {
                            top: EQ_GAP,
                            ..Padding::ZERO
                        })
                        .into(),
                    );
                }
                module(
                    "Equalizer",
                    f.eq_on,
                    badge,
                    toggle(f.eq_on, |fx, v| TrackFx {
                        eq_on: v,
                        ..fx.clone()
                    }),
                    Vec::new(),
                    body,
                )
            }
            FxKind::Comp => module(
                "Compressor",
                f.comp_on,
                Badge::Reduction(self.gain_reduction_db),
                toggle(f.comp_on, |fx, v| TrackFx {
                    comp_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider(
                        "Threshold",
                        f.comp.threshold_db,
                        -48.0,
                        0.0,
                        "dB",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.comp.threshold_db = v;
                            n
                        },
                    ),
                    slider("Ratio", f.comp.ratio, 1.0, 12.0, ":1", |fx, v| {
                        let mut n = fx.clone();
                        n.comp.ratio = v;
                        n
                    }),
                    slider("Makeup", f.comp.makeup_db, 0.0, 18.0, "dB", |fx, v| {
                        let mut n = fx.clone();
                        n.comp.makeup_db = v;
                        n
                    }),
                ],
                Vec::new(),
            ),
            FxKind::DeEsser => simple_insert(
                "De-esser",
                f.de_esser_on,
                toggle(f.de_esser_on, |fx, v| TrackFx {
                    de_esser_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider(
                        "Frequency",
                        f.de_esser.freq,
                        2000.0,
                        16000.0,
                        "Hz",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.de_esser.freq = v;
                            n
                        },
                    ),
                    slider(
                        "Threshold",
                        f.de_esser.threshold_db,
                        -60.0,
                        0.0,
                        "dB",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.de_esser.threshold_db = v;
                            n
                        },
                    ),
                    slider("Range", f.de_esser.range_db, 0.0, 24.0, "dB", |fx, v| {
                        let mut n = fx.clone();
                        n.de_esser.range_db = v;
                        n
                    }),
                ],
            ),
            FxKind::Gate => simple_insert(
                "Gate",
                f.gate_on,
                toggle(f.gate_on, |fx, v| TrackFx {
                    gate_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider(
                        "Threshold",
                        f.gate.threshold_db,
                        -80.0,
                        0.0,
                        "dB",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.gate.threshold_db = v;
                            n
                        },
                    ),
                    slider("Range", f.gate.range_db, 0.0, 80.0, "dB", |fx, v| {
                        let mut n = fx.clone();
                        n.gate.range_db = v;
                        n
                    }),
                    slider("Attack", f.gate.attack_ms, 0.1, 50.0, "ms", |fx, v| {
                        let mut n = fx.clone();
                        n.gate.attack_ms = v;
                        n
                    }),
                    slider("Release", f.gate.release_ms, 10.0, 500.0, "ms", |fx, v| {
                        let mut n = fx.clone();
                        n.gate.release_ms = v;
                        n
                    }),
                ],
            ),
            FxKind::Delay => simple_insert(
                "Delay",
                f.delay_on,
                toggle(f.delay_on, |fx, v| TrackFx {
                    delay_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider("Time", f.delay.time_ms, 1.0, 1500.0, "ms", |fx, v| {
                        let mut n = fx.clone();
                        n.delay.time_ms = v;
                        n
                    }),
                    slider("Feedback", f.delay.feedback, 0.0, 0.95, "", |fx, v| {
                        let mut n = fx.clone();
                        n.delay.feedback = v;
                        n
                    }),
                    slider("Mix", f.delay.mix, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.delay.mix = v;
                        n
                    }),
                ],
            ),
            FxKind::Saturation => simple_insert(
                "Saturation",
                f.saturation_on,
                toggle(f.saturation_on, |fx, v| TrackFx {
                    saturation_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider("Drive", f.saturation.drive_db, 0.0, 36.0, "dB", |fx, v| {
                        let mut n = fx.clone();
                        n.saturation.drive_db = v;
                        n
                    }),
                    slider("Mix", f.saturation.mix, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.saturation.mix = v;
                        n
                    }),
                ],
            ),
            FxKind::Doubler => simple_insert(
                "Doubler",
                f.doubler_on,
                toggle(f.doubler_on, |fx, v| TrackFx {
                    doubler_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider("Delay", f.doubler.delay_ms, 1.0, 50.0, "ms", |fx, v| {
                        let mut n = fx.clone();
                        n.doubler.delay_ms = v;
                        n
                    }),
                    slider("Depth", f.doubler.depth_ms, 0.0, 20.0, "ms", |fx, v| {
                        let mut n = fx.clone();
                        n.doubler.depth_ms = v;
                        n
                    }),
                    slider("Rate", f.doubler.rate_hz, 0.05, 8.0, "Hz", |fx, v| {
                        let mut n = fx.clone();
                        n.doubler.rate_hz = v;
                        n
                    }),
                    slider("Mix", f.doubler.mix, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.doubler.mix = v;
                        n
                    }),
                ],
            ),
            FxKind::Limiter => simple_insert(
                "Limiter",
                f.limiter_on,
                toggle(f.limiter_on, |fx, v| TrackFx {
                    limiter_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider(
                        "Ceiling",
                        f.limiter.ceiling_db,
                        -12.0,
                        0.0,
                        "dB",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.limiter.ceiling_db = v;
                            n
                        },
                    ),
                    slider(
                        "Release",
                        f.limiter.release_ms,
                        1.0,
                        500.0,
                        "ms",
                        |fx, v| {
                            let mut n = fx.clone();
                            n.limiter.release_ms = v;
                            n
                        },
                    ),
                ],
            ),
            // Reverb is a normal series insert: positionable, collapsible,
            // per-take.
            FxKind::Reverb => simple_insert(
                "Reverb",
                f.reverb_on,
                toggle(f.reverb_on, |fx, v| TrackFx {
                    reverb_on: v,
                    ..fx.clone()
                }),
                vec![
                    parameter_row(
                        "Space",
                        dropdown(
                            "Space…",
                            true,
                            Length::Fill,
                            DROPDOWN_MENU_WIDTH,
                            FxPresets::reverb()
                                .into_iter()
                                .map(|p| {
                                    MenuItem::new(
                                        p.name,
                                        set.committed(TrackFx {
                                            reverb: p.reverb,
                                            ..f.clone()
                                        }),
                                    )
                                })
                                .collect(),
                        ),
                        "",
                    ),
                    slider("Size", f.reverb.room_size, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.reverb.room_size = v;
                        n
                    }),
                    slider("Damping", f.reverb.damping, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.reverb.damping = v;
                        n
                    }),
                    slider("Mix", f.reverb.wet, 0.0, 1.0, "", |fx, v| {
                        let mut n = fx.clone();
                        n.reverb.wet = v;
                        n
                    }),
                ],
            ),
        }
    }
}

/// The preset row: a preset replaces the whole insert chain (tone), but
/// keeps the user's custom plugin order. That includes `TrackFx`'s legacy
/// reverb send, reset to off: nothing else in the UI sets or clears it, so
/// an old take's audible send goes with a preset rather than staying for
/// good. Aux sends / output routing live on the track, so they're untouched.
/// Its pop-up reads the preset the chain is, else "None"; a hairline closes
/// the inspector's identity block under it.
fn preset_row<'a>(fx: &TrackFx, set: &Setter) -> Element<'a, Message> {
    let items = FxPresets::vocal()
        .into_iter()
        .map(|p| {
            MenuItem::new(
                p.name,
                set.committed(TrackFx {
                    order: fx.order.clone(),
                    ..p.fx
                }),
            )
            .subtitle(Some(p.description.to_string()))
        })
        .collect();
    let current = current_preset(fx);
    let row = row![
        studio_text("Preset")
            .size(type_scale::LABEL)
            .line_height(iced::Pixels(type_scale::LINE))
            .color(Studio::TEXT_SECONDARY),
        Space::new().width(Length::Fill),
        dropdown(
            current.unwrap_or(NO_PRESET),
            current.is_none(),
            Length::Fixed(PRESET_POPUP_WIDTH),
            PRESET_MENU_WIDTH,
            items,
        ),
    ]
    .align_y(Vertical::Center);
    column![
        container(row).width(Length::Fill).padding(Padding {
            top: 10.0,
            right: 14.0,
            bottom: 12.0,
            left: 14.0,
        }),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(BLOCK_LINE)),
    ]
    .into()
}

/// The preset pop-up's text for a chain no preset is.
pub const NO_PRESET: &str = "None";

/// The preset pop-up's width (the mockup's 118 px minimum).
const PRESET_POPUP_WIDTH: f32 = 118.0;
/// The preset menu's width.
const PRESET_MENU_WIDTH: f32 = 280.0;
/// `_Dropdown`'s default menu width.
const DROPDOWN_MENU_WIDTH: f32 = 184.0;

/// `_Dropdown`: a pop-up button reading `label` (a placeholder in
/// `TEXT_SECONDARY`), opening a studio menu of `items` just below it.
fn dropdown<'a>(
    label: &'static str,
    placeholder: bool,
    width: Length,
    menu_width: f32,
    items: Vec<MenuItem<Message>>,
) -> Element<'a, Message> {
    PopupMenu::new(
        popup_face(label, placeholder, width),
        items,
        menu_width,
        Opens::BelowOnTap,
    )
    .into()
}

/// A module's badge.
enum Badge {
    None,
    Text(String),
    /// The compressor's live gain reduction (its meter's reading).
    Reduction(f64),
}

/// A generic insert's module: its header and (when on) a list of
/// parameter rows.
fn simple_insert<'a>(
    title: &'static str,
    on: bool,
    on_toggle: Vec<Message>,
    params: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    module(title, on, Badge::None, on_toggle, params, Vec::new())
}

/// An effect module (`DESIGN.md`): the header, then (when on) `above` (the
/// EQ's well) and the parameter rows 2 px apart; its padding, the panel's
/// opaque fill and a hairline under it. Off, the header alone at 55 % with
/// the "Bypassed" badge.
fn module<'a>(
    title: &'static str,
    on: bool,
    badge: Badge,
    on_toggle: Vec<Message>,
    params: Vec<Element<'a, Message>>,
    above: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let badge = if on {
        badge
    } else {
        Badge::Text(BYPASSED.into())
    };
    let header = module_header(title, on, badge, on_toggle);
    let mut content = Column::new().width(Length::Fill).push(if on {
        header
    } else {
        opacity_layer(BYPASSED_OPACITY, header).into()
    });
    if on {
        content = content.extend(above);
        if !params.is_empty() {
            content = content.push(Column::with_children(params).spacing(2).padding(Padding {
                top: PARAMS_GAP,
                ..Padding::ZERO
            }));
        }
    }
    column![
        container(content)
            .width(Length::Fill)
            .padding(MODULE_PADDING),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(MODULE_LINE)),
    ]
    .width(Length::Fill)
    .into()
}

/// A module's header: the drag handle's dots, the power button, the name
/// (12 px medium) and the badge (11 px `TEXT_FAINT`, tabular). (The
/// handle's drag is the chain's: see `chain`.)
fn module_header<'a>(
    title: &'static str,
    on: bool,
    badge: Badge,
    on_toggle: Vec<Message>,
) -> Element<'a, Message> {
    let badge_text = |text: String| {
        studio_text(text)
            .size(type_scale::LABEL)
            .line_height(iced::Pixels(type_scale::LINE))
            .tabular()
            .color(Studio::TEXT_FAINT)
    };
    let badge: Element<'a, Message> = match badge {
        Badge::None => Space::new().into(),
        Badge::Text(text) => badge_text(text).into(),
        Badge::Reduction(gr) => row![
            canvas_widget(GrBar { gr_db: gr }).width(18).height(6),
            badge_text(gr_badge(gr)),
        ]
        .spacing(6)
        .align_y(Vertical::Center)
        .into(),
    };
    row![
        canvas_widget(HandleDots).width(6).height(10),
        power_toggle(on, on_toggle),
        studio_text(title)
            .size(type_scale::BODY)
            .font(font::MEDIUM)
            .line_height(iced::Pixels(type_scale::LINE))
            .color(Studio::TEXT_PRIMARY)
            .width(Length::Fill),
        badge,
    ]
    .spacing(8)
    .height(MODULE_HEADER)
    .align_y(Vertical::Center)
    .into()
}

/// The drag handle's dots: two columns of three, `#4A4A54`.
struct HandleDots;

impl canvas::Program<Message> for HandleDots {
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
        for col in 0..2 {
            for r in 0..3 {
                frame.fill(
                    &Path::circle(
                        Point::new(1.5 + col as f32 * 3.0, 1.7 + r as f32 * 3.4),
                        1.0,
                    ),
                    HANDLE_DOT,
                );
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

/// How long the power button's fill takes to change (the toggle it
/// replaced kept this time, linear).
pub const TOGGLE_ANIMATION: Duration = Duration::from_millis(130);

/// An effect's power button (`DESIGN.md`, `controls::power_button`'s look:
/// an 18 px circle with the power glyph; on, `ACCENT` at 16 % behind
/// `ACCENT_BRIGHT`; off, `CONTROL` behind `TEXT_DISABLED`). A press
/// reports `on_tap` (the live edit and its commit). The fill changes over
/// 130 ms, linear (`AnimatedContainer`); the glyph's colour at once.
fn power_toggle<'a>(on: bool, on_tap: Vec<Message>) -> Element<'a, Message> {
    let (_, glyph) = power_colors(on, false);
    let face = animated_container(
        line_icon(glyphs::POWER, 9.0, glyph).stroke(1.2),
        TOGGLE_ANIMATION,
    )
    .width(POWER_SIZE)
    .height(POWER_SIZE)
    .align((0.0, 0.0))
    .style(move |hovered| Decoration::fill(power_colors(on, hovered).0, POWER_SIZE / 2.0));
    tap(face, on_tap, Some(mouse::Interaction::Pointer))
}

/// An FX parameter's readout, with a true minus sign. Below 10 Hz or 10 ms
/// it keeps a decimal: the doubler's rate (0.05–8 Hz) read its 0.6 Hz
/// default as "1 Hz", and anything under 0.5 as "0 Hz"; the gate's attack
/// (from 0.1 ms) likewise "0 ms".
pub fn fx_value_label(v: f64, unit: &str) -> String {
    let fine = |x: f64| {
        if x < 10.0 {
            to_string_as_fixed(x, 1)
        } else {
            (x.round() as i64).to_string()
        }
    };
    let label = match unit {
        ":1" => format!("{} : 1", to_string_as_fixed(v, 1)),
        "dB" => format!("{} dB", v.round() as i64),
        "Hz" => {
            if v >= 1000.0 {
                format!("{} kHz", to_string_as_fixed(v / 1000.0, 1))
            } else {
                format!("{} Hz", fine(v))
            }
        }
        "ms" => format!("{} ms", fine(v)),
        _ => format!("{}%", (v * 100.0).round() as i64),
    };
    label.replace('-', "\u{2212}")
}

/// `_slider`: a parameter row (`controls::parameter_row`) — the 62 px
/// label, the thin slider, the value in its 54 px column.
fn fx_slider<'a>(
    label: &'static str,
    value: f64,
    min: f64,
    max: f64,
    unit: &'static str,
    on_changed: impl Fn(f64) -> Message + 'a,
) -> Element<'a, Message> {
    parameter_row(
        label,
        canvas_widget(FxSlider {
            value,
            min,
            max,
            on_changed: Box::new(on_changed),
            on_commit: Message::CommitFx,
        })
        .width(Length::Fill)
        .height(SLIDER_HEIGHT),
        fx_value_label(value, unit),
    )
}

/// The slider's hit row (`controls::SLIDER_HEIGHT`).
pub const SLIDER_HEIGHT: f32 = crate::ui::core::widgets::controls::SLIDER_HEIGHT;

/// The value a slider `width` wide from `min` to `max` sets at `dx` from
/// its left edge.
pub fn fx_slider_value(dx: f32, width: f32, min: f64, max: f64) -> f64 {
    min + (f64::from(dx) / f64::from(width)).clamp(0.0, 1.0) * (max - min)
}

/// `_FxSlider`: the design's thin slider (`controls::draw_thin_slider`: a
/// 2 px track, the neutral fill, a 10 px knob); press or drag anywhere on
/// it to set the value. Every press and every move while held sets the
/// value from the pointer's x; the release commits it once.
struct FxSlider<'a> {
    value: f64,
    min: f64,
    max: f64,
    on_changed: Box<dyn Fn(f64) -> Message + 'a>,
    on_commit: Message,
}

/// A slider's hold: a press or drag that set the value and owes its commit.
#[derive(Default)]
struct SliderState {
    active: bool,
    cache: KeyedCache<(u64, u32, u32)>,
}

impl canvas::Program<Message> for FxSlider<'_> {
    type State = SliderState;

    fn update(
        &self,
        state: &mut SliderState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let value_at = |p: Point| fx_slider_value(p.x - bounds.x, bounds.width, self.min, self.max);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                state.active = true;
                Some(canvas::Action::publish((self.on_changed)(value_at(p))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if state.active => {
                let p = held_pointer(cursor)?;
                Some(canvas::Action::publish((self.on_changed)(value_at(p))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.active =>
            {
                state.active = false;
                Some(canvas::Action::publish(self.on_commit.clone()).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &SliderState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let t = ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        let size = bounds.size();
        vec![state.cache.draw(
            renderer,
            size,
            (t.to_bits(), size.width.to_bits(), size.height.to_bits()),
            |frame| draw_thin_slider(frame, size, t as f32, KNOB, SLIDER_FILL),
        )]
    }

    fn mouse_interaction(
        &self,
        state: &SliderState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.active || cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

/// The cursor's position while a control is held (even over a layer
/// above).
fn held_pointer(cursor: mouse::Cursor) -> Option<Point> {
    match cursor {
        mouse::Cursor::Available(p) | mouse::Cursor::Levitating(p) => Some(p),
        mouse::Cursor::Unavailable => None,
    }
}

/// The interactive EQ, as wide as the module and [`EQ_HEIGHT`] tall.
fn eq_graph<'a>(
    fx: Rc<TrackFx>,
    input: Option<EqInput<'a>>,
    on_changed: impl Fn(usize, EqBand) -> Message + 'a,
) -> Element<'a, Message> {
    canvas_widget(OwnedEq {
        fx,
        input,
        on_changed: Box::new(on_changed),
    })
    .width(Length::Fill)
    .height(EQ_HEIGHT)
    .into()
}

/// The EQ graph over a chain the panel owns for this view.
struct OwnedEq<'a> {
    fx: Rc<TrackFx>,
    input: Option<EqInput<'a>>,
    on_changed: Box<dyn Fn(usize, EqBand) -> Message + 'a>,
}

impl OwnedEq<'_> {
    fn graph(&self) -> EqGraph<'_> {
        EqGraph {
            bands: &self.fx.eq,
            on: self.fx.eq_on,
            input: self.input,
            on_changed: Box::new(|i, band| (self.on_changed)(i, band)),
            on_commit: Message::CommitFx,
        }
    }
}

impl canvas::Program<Message> for OwnedEq<'_> {
    type State = eq::EqState;

    fn update(
        &self,
        state: &mut eq::EqState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        self.graph().update(state, event, bounds, cursor)
    }

    fn draw(
        &self,
        state: &eq::EqState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        self.graph().draw(state, renderer, theme, bounds, cursor)
    }
}

// --------------------------------------------------------------- GR meter

/// The meter's full scale (dB of reduction).
pub const GR_MAX_DB: f64 = 18.0;

/// `_GrMeterState`'s polling: a 50 ms poll that stops once the reading has
/// held still for [`GrMeter::IDLE_POLLS`] polls (it rebuilt at 20 Hz for as
/// long as the compressor was shown, even with nothing playing), and starts
/// again when woken.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GrMeter {
    /// The last reading (dB).
    pub gr: f64,
    /// The falling peak (dB).
    pub peak: f64,
    unchanged: u32,
    polling: bool,
}

impl GrMeter {
    /// Unchanged polls (at [`GrMeter::POLL`]) before polling stops.
    pub const IDLE_POLLS: u32 = 10;
    pub const POLL: Duration = Duration::from_millis(50);
    /// How far the peak falls per poll.
    pub const PEAK_FALL_DB: f64 = 0.8;

    pub fn polling(&self) -> bool {
        self.polling
    }

    /// `_start`: (re)start polling, counting stillness afresh.
    pub fn start(&mut self) {
        self.unchanged = 0;
        self.polling = true;
    }

    /// `_poll` with reading `v`: whether the meter changed.
    pub fn poll(&mut self, v: f64) -> bool {
        let peak = if v > self.peak {
            v
        } else {
            v.max(self.peak - Self::PEAK_FALL_DB)
        };
        if v == self.gr && peak == self.peak {
            self.unchanged += 1;
            if self.unchanged >= Self::IDLE_POLLS {
                self.polling = false;
            }
            return false;
        }
        self.unchanged = 0;
        self.gr = v;
        self.peak = peak;
        true
    }
}

/// `_GrMeter`'s readout: "0.0" under 0.05 dB, else "-6.0".
pub fn gr_readout(gr: f64) -> String {
    if gr < 0.05 {
        "0.0".to_string()
    } else {
        format!("-{}", to_string_as_fixed(gr, 1))
    }
}

/// `_GrMeter`'s bar, polling the reading the host passed in: a short 2 px
/// bar before the compressor's badge, the reduction filling it (the accent
/// deepening to red as it deepens) under a falling peak tick.
struct GrBar {
    gr_db: f64,
}

#[derive(Default)]
struct GrState {
    meter: GrMeter,
    /// When the next poll is due.
    next: Option<Instant>,
    cache: KeyedCache<(u64, u64, u32, u32)>,
}

impl canvas::Program<Message> for GrBar {
    type State = GrState;

    fn update(
        &self,
        state: &mut GrState,
        event: &canvas::Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let canvas::Event::Window(window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        let now = *now;
        if !state.meter.polling() {
            // Woken (a rebuild, the playhead): poll again.
            state.meter.start();
            let next = now + GrMeter::POLL;
            state.next = Some(next);
            return Some(canvas::Action::request_redraw_at(next));
        }
        let due = state.next.unwrap_or(now);
        if now < due {
            return Some(canvas::Action::request_redraw_at(due));
        }
        state.meter.poll(self.gr_db);
        if state.meter.polling() {
            let next = now + GrMeter::POLL;
            state.next = Some(next);
            Some(canvas::Action::request_redraw_at(next))
        } else {
            state.next = None;
            None
        }
    }

    fn draw(
        &self,
        state: &GrState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let t = state.meter.gr / GR_MAX_DB;
        let peak = state.meter.peak / GR_MAX_DB;
        vec![state.cache.draw(
            renderer,
            size,
            (
                t.to_bits(),
                peak.to_bits(),
                size.width.to_bits(),
                size.height.to_bits(),
            ),
            |frame| draw_gr(frame, size, t, peak),
        )]
    }
}

/// Flutter's `Color.lerp`.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// The bar's fill color at `t` of full scale: the accent deepening to
/// `Studio.record`.
pub fn gr_color(t: f64) -> Color {
    lerp_color(
        Studio::ACCENT,
        Studio::RECORD,
        (t.clamp(0.0, 1.0) * 1.6).clamp(0.0, 1.0) as f32,
    )
}

/// `_GrPainter`: a 2 px track (`CONTROL_ACTIVE`), the reduction's fill and
/// a 1 px peak tick the box's height.
fn draw_gr(frame: &mut Frame, size: Size, t: f64, peak: f64) {
    let cy = (size.height / 2.0).round();
    let r = Radius::from(1.0);
    frame.fill(
        &Path::rounded_rectangle(Point::new(0.0, cy - 1.0), Size::new(size.width, 2.0), r),
        Studio::CONTROL_ACTIVE,
    );
    let tc = t.clamp(0.0, 1.0);
    if tc > 0.0 {
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, cy - 1.0),
                Size::new(size.width * tc as f32, 2.0),
                r,
            ),
            gr_color(tc),
        );
    }
    let pc = peak.clamp(0.0, 1.0);
    if pc > 0.01 {
        let x = (size.width * pc as f32).min(size.width - 1.0).max(1.0);
        frame.fill_rectangle(
            Point::new(x - 0.5, 0.0),
            Size::new(1.0, size.height),
            Studio::TEXT_SECONDARY,
        );
    }
}
