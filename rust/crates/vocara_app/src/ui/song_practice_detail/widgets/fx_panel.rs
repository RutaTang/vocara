//! Port of `ui/song_practice_detail/widgets/fx_panel.dart`: the FX detail
//! for the selected vocal take or FX bus — presets, the insert chain
//! (equalizer with its analyzer, compressor with its gain-reduction meter,
//! de-esser, gate, delay, saturation, doubler, limiter, reverb), each insert
//! a card with a drag handle and an on/off toggle that collapses it to its
//! header when off, reordered by dragging the handles.
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
//! A committed edit (a toggle, a preset) reports its live edit and then
//! `CommitFx`, as `_setFx(fx, commit: true)` called both; a slider or EQ
//! drag reports live edits on every move and `CommitFx` once on release; a
//! wheel Q edit commits 400 ms after its last tick.
//!
//! The panel keys itself by its target, so another take or bus gets fresh
//! state (the analyzer's smoothed spectrum, a held slider), as Dart's
//! `FxPanel(key: ValueKey(id))` did.
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
//!   mouse control, so those Dart cases have no counterpart; a trackpad's
//!   two-finger swipe is a pixel-delta wheel event the sliders ignore and
//!   the EQ leaves to the panel's scroll (only a line-delta mouse wheel sets
//!   a band's Q).
//! - The gain-reduction meter's reading is the one the host passed at its
//!   last view (Dart polled the engine itself); the meter keeps Dart's 50 ms
//!   poll cadence, peak falloff and idle stop over it.
//! - Flutter animated the reorder; iced moves the cards at once (see
//!   `chain`). iced text has no letter spacing (the titles' 0.8) and no
//!   ellipsis.

mod chain;
pub mod eq;
#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{canvas as canvas_widget, column, container, row, scrollable, text, Space};
use iced::{
    mouse, window, Color, Element, Length, Padding, Point, Rectangle, Renderer, Size, Theme,
};
use vocara_model::models::audio_fx::{EqBand, FxKind, TrackFx};
use vocara_model::models::fx_presets::FxPresets;

pub use eq::EqInput;

use self::chain::InsertChain;
use self::eq::{EqGraph, EQ_HEIGHT};
use super::mixer_shared::{keyed_row, tap, MenuItem, Opens, PopupMenu};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::Curve;
use crate::ui::core::theme::{self, boxed, font, Studio};
use crate::ui::core::widgets::animated::{animated_align, animated_container, Decoration};
use crate::ui::core::widgets::cap_label::cap_label;
use crate::ui::core::widgets::painter::KeyedCache;
use crate::ui::core::widgets::studio_controls::fixed_width_readout;
use crate::util::to_string_as_fixed;

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

    /// The panel: the preset row, then the insert chain, in a scroll view
    /// with no scrollbar.
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
        let body = column![
            preset_row(&fx, &set),
            Space::new().height(14),
            // Insert chain — drag a handle to reorder the signal flow; each
            // insert collapses to its header when off.
            InsertChain::new(cards, move |order| reorder.reorder(order)),
            Space::new().height(4),
        ]
        .width(Length::Fill)
        .padding(Padding {
            top: 4.0,
            right: 2.0,
            bottom: 16.0,
            left: 2.0,
        });
        let panel = scrollable(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::no_scrollbar);
        keyed_row()
            .push(target_key(&self.target), panel)
            .height(Length::Fill)
            .into()
    }

    /// One insert card in the chain: a header (drag handle + on/off) plus,
    /// when enabled, the plugin's editor. Opaque in the panel color, so a
    /// dragged card fully occludes the others.
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
        // The header toggle's committed edit.
        let toggle = |on: bool, with: fn(&TrackFx, bool) -> TrackFx| set.committed(with(f, !on));
        let body: Element<'a, Message> = match kind {
            FxKind::Eq => {
                let mut card = column![insert_header(
                    "Equalizer",
                    f.eq_on,
                    toggle(f.eq_on, |fx, v| TrackFx {
                        eq_on: v,
                        ..fx.clone()
                    })
                )];
                if f.eq_on {
                    let fx_c = Rc::clone(fx);
                    let set_c = Rc::clone(set);
                    let input = self.analyzer_input();
                    card = card.push(Space::new().height(8)).push(eq_graph(
                        Rc::clone(fx),
                        input,
                        move |i, band| {
                            set_c.live(TrackFx {
                                eq_on: true,
                                ..fx_c.with_band(i, band)
                            })
                        },
                    ));
                }
                card.into()
            }
            FxKind::Comp => {
                let mut card = column![insert_header(
                    "Compressor",
                    f.comp_on,
                    toggle(f.comp_on, |fx, v| TrackFx {
                        comp_on: v,
                        ..fx.clone()
                    })
                )];
                if f.comp_on {
                    card = card
                        .push(Space::new().height(6))
                        .push(slider(
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
                        ))
                        .push(slider("Ratio", f.comp.ratio, 1.0, 12.0, ":1", |fx, v| {
                            let mut n = fx.clone();
                            n.comp.ratio = v;
                            n
                        }))
                        .push(slider(
                            "Makeup",
                            f.comp.makeup_db,
                            0.0,
                            18.0,
                            "dB",
                            |fx, v| {
                                let mut n = fx.clone();
                                n.comp.makeup_db = v;
                                n
                            },
                        ))
                        .push(Space::new().height(2))
                        .push(gr_meter(self.gain_reduction_db));
                }
                card.into()
            }
            FxKind::DeEsser => simple_insert(
                "De-esser",
                f.de_esser_on,
                toggle(f.de_esser_on, |fx, v| TrackFx {
                    de_esser_on: v,
                    ..fx.clone()
                }),
                vec![
                    slider("Freq", f.de_esser.freq, 2000.0, 16000.0, "Hz", |fx, v| {
                        let mut n = fx.clone();
                        n.de_esser.freq = v;
                        n
                    }),
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
                    container(
                        row![
                            cap_label("Space"),
                            Space::new().width(Length::Fill),
                            dropdown(
                                "Space…",
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
                        ]
                        .align_y(Vertical::Center),
                    )
                    .padding([4, 0])
                    .into(),
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
        };
        container(body)
            .width(Length::Fill)
            .padding(Padding {
                bottom: 18.0,
                ..Padding::ZERO
            })
            .style(theme::fill(Studio::SURFACE))
            .into()
    }
}

/// The preset row: a preset replaces the whole insert chain (tone), but
/// keeps the user's custom plugin order. That includes `TrackFx`'s legacy
/// reverb send, reset to off: nothing else in the UI sets or clears it, so
/// an old take's audible send goes with a preset rather than staying for
/// good. Aux sends / output routing live on the track, so they're untouched.
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
    row![
        cap_label("Preset"),
        Space::new().width(Length::Fill),
        dropdown("Choose…", PRESET_MENU_WIDTH, items),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// The preset menu's width.
const PRESET_MENU_WIDTH: f32 = 280.0;
/// `_Dropdown`'s default menu width.
const DROPDOWN_MENU_WIDTH: f32 = 184.0;

/// `_Dropdown`: an inset box reading `hint` with a chevron, opening a
/// studio menu of `items` just below it.
fn dropdown<'a>(
    hint: &'static str,
    menu_width: f32,
    items: Vec<MenuItem<Message>>,
) -> Element<'a, Message> {
    let face = container(
        row![
            text(hint).size(11).color(Studio::TEXT_SECONDARY),
            Space::new().width(4),
            icons::icon(Icon::ChevronDown, 14.0, Studio::TEXT_FAINT),
        ]
        .align_y(Vertical::Center),
    )
    .padding([5, 10])
    .style(|_| boxed(Some(Studio::INSET), 6.0, Some(Studio::BORDER)));
    PopupMenu::new(face, items, menu_width, Opens::BelowOnTap).into()
}

/// A generic insert card: header + (when on) a list of param sliders.
fn simple_insert<'a>(
    title: &'static str,
    on: bool,
    on_toggle: Vec<Message>,
    body: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut card = column![insert_header(title, on, on_toggle)];
    if on {
        card = card.push(Space::new().height(6)).extend(body);
    }
    card.into()
}

/// An insert header: a drag handle, the title, and an on/off toggle. (The
/// handle's drag is the chain's: see `chain`.)
fn insert_header<'a>(title: &str, on: bool, on_toggle: Vec<Message>) -> Element<'a, Message> {
    row![
        container(icons::icon(Icon::GripVertical, 16.0, Studio::TEXT_FAINT)).padding(Padding {
            right: 6.0,
            ..Padding::ZERO
        }),
        text(title.to_uppercase())
            .size(10.5)
            .font(font::BOLD)
            .color(if on {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_SECONDARY
            }),
        Space::new().width(Length::Fill),
        fx_toggle(on, on_toggle),
    ]
    .height(chain::HEADER_HEIGHT)
    .align_y(Vertical::Center)
    .into()
}

/// How long [`fx_toggle`]'s color and thumb take to change.
pub const TOGGLE_ANIMATION: Duration = Duration::from_millis(130);

/// `_Toggle`: a compact studio-style on/off switch, 34 × 18 — the accent
/// with a white thumb on the right when on, the border color with a grey
/// thumb on the left when off. A tap reports `on_tap`. The color changes
/// over 130 ms, linear (`AnimatedContainer`), and the thumb slides over
/// 130 ms, easing out (`AnimatedAlign`); its own color changes at once.
fn fx_toggle<'a>(on: bool, on_tap: Vec<Message>) -> Element<'a, Message> {
    let thumb = container(Space::new().width(13).height(13)).style(move |_| {
        boxed(
            Some(if on {
                Color::WHITE
            } else {
                Studio::TEXT_SECONDARY
            }),
            6.5,
            None,
        )
    });
    let knob = animated_align(
        container(thumb).padding(2.5),
        (if on { 1.0 } else { -1.0 }, 0.0),
        TOGGLE_ANIMATION,
        Curve::EaseOut,
    );
    let track = animated_container(knob, TOGGLE_ANIMATION)
        .width(34)
        .height(18)
        .decoration(Decoration::fill(
            if on { Studio::ACCENT } else { Studio::BORDER },
            9.0,
        ));
    tap(track, on_tap, None)
}

/// An FX parameter's readout. Below 10 Hz or 10 ms it keeps a decimal: the
/// doubler's rate (0.05–8 Hz) read its 0.6 Hz default as "1 Hz", and
/// anything under 0.5 as "0 Hz"; the gate's attack (from 0.1 ms) likewise
/// "0 ms".
pub fn fx_value_label(v: f64, unit: &str) -> String {
    let fine = |x: f64| {
        if x < 10.0 {
            to_string_as_fixed(x, 1)
        } else {
            (x.round() as i64).to_string()
        }
    };
    match unit {
        ":1" => format!("{}:1", to_string_as_fixed(v, 1)),
        "dB" => format!("{} dB", v.round() as i64),
        "Hz" => {
            if v >= 1000.0 {
                format!("{}k", to_string_as_fixed(v / 1000.0, 1))
            } else {
                format!("{} Hz", fine(v))
            }
        }
        "ms" => format!("{} ms", fine(v)),
        _ => format!("{}%", (v * 100.0).round() as i64),
    }
}

/// `_slider`: a labelled parameter row — the 64 px label, the slider, and
/// the value in a 42 px box.
fn fx_slider<'a>(
    label: &'static str,
    value: f64,
    min: f64,
    max: f64,
    unit: &'static str,
    on_changed: impl Fn(f64) -> Message + 'a,
) -> Element<'a, Message> {
    container(
        row![
            container(text(label).size(10.5).color(Studio::TEXT_FAINT)).width(64),
            canvas_widget(FxSlider {
                value,
                min,
                max,
                on_changed: Box::new(on_changed),
                on_commit: Message::CommitFx,
            })
            .width(Length::Fill)
            .height(SLIDER_HEIGHT),
            Space::new().width(8),
            fixed_width_readout(fx_value_label(value, unit), 42.0, 10.5),
        ]
        .align_y(Vertical::Center),
    )
    .padding([4, 0])
    .into()
}

/// The slider's height.
pub const SLIDER_HEIGHT: f32 = 20.0;

/// The value a slider `width` wide from `min` to `max` sets at `dx` from
/// its left edge.
pub fn fx_slider_value(dx: f32, width: f32, min: f64, max: f64) -> f64 {
    min + (f64::from(dx) / f64::from(width)).clamp(0.0, 1.0) * (max - min)
}

/// `_FxSlider`: a clean minimal slider — a thin track with an accent fill
/// and a small thumb; press or drag anywhere on it to set the value. Every
/// press and every move while held sets the value from the pointer's x;
/// the release commits it once.
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
            |frame| draw_slider(frame, size, t as f32),
        )]
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

/// `_SliderPainter`.
fn draw_slider(frame: &mut Frame, size: Size, t: f32) {
    let cy = size.height / 2.0;
    let r = Radius::from(1.5);
    frame.fill(
        &Path::rounded_rectangle(Point::new(0.0, cy - 1.5), Size::new(size.width, 3.0), r),
        Studio::BORDER,
    );
    let fill_w = (size.width * t).clamp(0.0, size.width);
    if fill_w > 0.0 {
        frame.fill(
            &Path::rounded_rectangle(Point::new(0.0, cy - 1.5), Size::new(fill_w, 3.0), r),
            Studio::ACCENT,
        );
    }
    let thumb_x = (size.width * t).min(size.width - 6.0).max(6.0);
    let thumb = Path::circle(Point::new(thumb_x, cy), 6.0);
    frame.fill(&thumb, Studio::ACCENT);
    frame.stroke(
        &thumb,
        Stroke::default()
            .with_color(Studio::SURFACE)
            .with_width(2.0),
    );
}

/// The interactive EQ, as wide as the panel and [`EQ_HEIGHT`] tall.
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

/// `_GrMeter`: a live gain-reduction meter for the compressor — the
/// reduction as a bar (accent → red as it deepens) with a falling peak tick,
/// and its reading.
fn gr_meter<'a>(gr_db: f64) -> Element<'a, Message> {
    container(
        row![
            container(text("GR").size(10.5).color(Studio::TEXT_FAINT)).width(64),
            canvas_widget(GrBar { gr_db }).width(Length::Fill).height(8),
            Space::new().width(8),
            text(gr_readout(gr_db))
                .size(10.5)
                .color(Studio::TEXT_SECONDARY)
                .width(42)
                .align_x(Horizontal::Right),
        ]
        .align_y(Vertical::Center),
    )
    .padding([4, 0])
    .into()
}

/// The meter's bar, polling the reading the host passed in.
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

/// `_GrPainter`.
fn draw_gr(frame: &mut Frame, size: Size, t: f64, peak: f64) {
    let cy = size.height / 2.0;
    let r = Radius::from(1.5);
    frame.fill(
        &Path::rounded_rectangle(Point::new(0.0, cy - 1.5), Size::new(size.width, 3.0), r),
        Studio::BORDER,
    );
    let tc = t.clamp(0.0, 1.0);
    if tc > 0.0 {
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, cy - 1.5),
                Size::new(size.width * tc as f32, 3.0),
                r,
            ),
            gr_color(tc),
        );
    }
    let pc = peak.clamp(0.0, 1.0);
    if pc > 0.01 {
        let x = (size.width * pc as f32).min(size.width - 1.0).max(1.0);
        frame.fill_rectangle(
            Point::new(x - 1.0, cy - 3.5),
            Size::new(2.0, 7.0),
            Studio::TEXT_SECONDARY,
        );
    }
}
